//! Integrity-checked provisioning for the standalone Windows distribution.
//!
//! The packaging script embeds four raw-DEFLATE streams with their uncompressed
//! lengths and SHA-256 digests. Runtime extraction occurs in a unique staging
//! directory under LocalAppData and is published by a same-volume rename only
//! after every executable has been flushed and verified.

use flate2::read::DeflateDecoder;
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

const MAGIC: &[u8; 8] = b"YTDPLD01";
const MANIFEST_NAME: &str = ".complete";
const MAX_TOOL_BYTES: u64 = 400 * 1024 * 1024;
const REQUIRED_TOOLS: [&str; 4] = ["yt-dlp.exe", "ffmpeg.exe", "ffprobe.exe", "deno.exe"];
const EMBEDDED_PAYLOAD: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/runtime-payload.bin"));
const EMBEDDED_PAYLOAD_SHA256: &str = env!("YTD_RUNTIME_PAYLOAD_SHA256");

static EXTRACTION_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static EMBEDDED_RUNTIME: OnceLock<Result<Option<PathBuf>, String>> = OnceLock::new();

#[derive(Debug)]
struct Entry {
    name: String,
    uncompressed_len: u64,
    compressed: Range<usize>,
    sha256: [u8; 32],
}

pub fn has_embedded_payload() -> bool {
    !EMBEDDED_PAYLOAD.is_empty()
}

/// Returns the provisioned runtime directory, or `None` for a developer build
/// that intentionally does not embed third-party executables.
pub fn embedded_runtime_dir() -> Result<Option<PathBuf>, String> {
    if !has_embedded_payload() {
        return Ok(None);
    }
    EMBEDDED_RUNTIME
        .get_or_init(|| {
            if EMBEDDED_PAYLOAD_SHA256.len() != 64 {
                return Err("The embedded runtime has no valid SHA-256 manifest. Re-download the application.".into());
            }
            let actual_hash = digest_hex(EMBEDDED_PAYLOAD);
            if !actual_hash.eq_ignore_ascii_case(EMBEDDED_PAYLOAD_SHA256) {
                return Err("The embedded runtime payload failed its SHA-256 check. Re-download the application.".into());
            }
            let bundle_id = EMBEDDED_PAYLOAD_SHA256[..24].to_ascii_lowercase();
            let base = app_data_dir()?.join("tools");
            ensure_runtime_at(EMBEDDED_PAYLOAD, &base, &bundle_id).map(Some)
        })
        .clone()
}

/// CLI smoke-test entry point. It provisions the tools without opening the UI
/// or contacting YouTube, and returns a non-zero process exit on any failure.
pub fn verify_embedded_runtime() -> Result<(), String> {
    let directory = embedded_runtime_dir()?
        .ok_or_else(|| "This executable does not contain an embedded runtime.".to_string())?;
    for name in REQUIRED_TOOLS {
        if !directory.join(name).is_file() {
            return Err(format!("Embedded runtime is missing {name}."));
        }
    }
    Ok(())
}

pub fn resolve_embedded_dependency(directory: &Path, file: &str) -> Result<PathBuf, String> {
    if !REQUIRED_TOOLS.contains(&file) {
        return Err(format!("Unknown bundled dependency {file}."));
    }
    let candidate = directory.join(file);
    if candidate.is_file() {
        Ok(candidate)
    } else {
        Err(format!(
            "The embedded runtime is missing {file}. Re-download the application."
        ))
    }
}

fn app_data_dir() -> Result<PathBuf, String> {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .map(|value| PathBuf::from(value).join("AppData").join("Local"))
        })
        .ok_or_else(|| {
            "Cannot locate the per-user LocalAppData directory for bundled tools.".to_string()
        })?;
    Ok(base.join("YouTube Downloader Desktop"))
}

fn digest_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    hex(&digest)
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn take<'a>(payload: &'a [u8], offset: &mut usize, length: usize) -> Result<&'a [u8], String> {
    let end = offset
        .checked_add(length)
        .ok_or_else(|| "Embedded runtime payload has an invalid length.".to_string())?;
    let bytes = payload
        .get(*offset..end)
        .ok_or_else(|| "Embedded runtime payload is truncated.".to_string())?;
    *offset = end;
    Ok(bytes)
}

fn read_u16(payload: &[u8], offset: &mut usize) -> Result<u16, String> {
    Ok(u16::from_le_bytes(
        take(payload, offset, 2)?
            .try_into()
            .expect("two bytes were requested"),
    ))
}

fn read_u32(payload: &[u8], offset: &mut usize) -> Result<u32, String> {
    Ok(u32::from_le_bytes(
        take(payload, offset, 4)?
            .try_into()
            .expect("four bytes were requested"),
    ))
}

fn read_u64(payload: &[u8], offset: &mut usize) -> Result<u64, String> {
    Ok(u64::from_le_bytes(
        take(payload, offset, 8)?
            .try_into()
            .expect("eight bytes were requested"),
    ))
}

fn parse_payload(payload: &[u8]) -> Result<Vec<Entry>, String> {
    if payload.len() < 12 || payload.get(..8) != Some(MAGIC.as_slice()) {
        return Err("Embedded runtime payload is missing or has an unsupported format.".into());
    }
    let mut offset = 8;
    let count = read_u32(payload, &mut offset)? as usize;
    if count != REQUIRED_TOOLS.len() {
        return Err("Embedded runtime payload does not contain all four required tools.".into());
    }

    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        let name_len = read_u16(payload, &mut offset)? as usize;
        if name_len == 0 || name_len > 64 {
            return Err("Embedded runtime payload contains an invalid tool name.".into());
        }
        let name = std::str::from_utf8(take(payload, &mut offset, name_len)?)
            .map_err(|_| "Embedded runtime payload contains a non-UTF-8 tool name.")?
            .to_owned();
        if !REQUIRED_TOOLS.contains(&name.as_str())
            || entries.iter().any(|entry: &Entry| entry.name == name)
        {
            return Err(
                "Embedded runtime payload contains an unexpected or duplicate tool.".into(),
            );
        }
        let uncompressed_len = read_u64(payload, &mut offset)?;
        let compressed_len = read_u64(payload, &mut offset)?;
        if uncompressed_len == 0 || uncompressed_len > MAX_TOOL_BYTES {
            return Err(format!("Embedded runtime size is invalid for {name}."));
        }
        let sha256: [u8; 32] = take(payload, &mut offset, 32)?
            .try_into()
            .expect("32 bytes were requested");
        let compressed_len = usize::try_from(compressed_len)
            .map_err(|_| "Embedded runtime compressed length is too large.")?;
        let start = offset;
        take(payload, &mut offset, compressed_len)?;
        entries.push(Entry {
            name,
            uncompressed_len,
            compressed: start..offset,
            sha256,
        });
    }
    if offset != payload.len()
        || REQUIRED_TOOLS
            .iter()
            .any(|name| !entries.iter().any(|entry| &entry.name == name))
    {
        return Err("Embedded runtime payload has trailing bytes or is missing a tool.".into());
    }
    Ok(entries)
}

fn expected_manifest(
    directory: &Path,
    bundle_id: &str,
    entries: &[Entry],
) -> Result<String, String> {
    let mut manifest = format!("YTD-RUNTIME-COMPLETE-1\n{bundle_id}\n");
    for entry in entries {
        let path = directory.join(&entry.name);
        let metadata = fs::metadata(&path)
            .map_err(|_| format!("Embedded runtime is missing {}.", entry.name))?;
        if !metadata.is_file() || metadata.len() != entry.uncompressed_len {
            return Err(format!(
                "Embedded runtime file {} failed its size check.",
                entry.name
            ));
        }
        let modified = metadata
            .modified()
            .map_err(|_| format!("Cannot inspect embedded runtime file {}.", entry.name))?;
        let ticks = modified
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| {
                format!(
                    "Embedded runtime file {} has an invalid timestamp.",
                    entry.name
                )
            })?
            .as_nanos();
        manifest.push_str(&format!(
            "{}\t{}\t{}\t{}\n",
            entry.name,
            entry.uncompressed_len,
            hex(&entry.sha256),
            ticks
        ));
    }
    Ok(manifest)
}

fn validate_runtime_dir(
    directory: &Path,
    bundle_id: &str,
    entries: &[Entry],
) -> Result<(), String> {
    let expected = expected_manifest(directory, bundle_id, entries)?;
    let actual = fs::read_to_string(directory.join(MANIFEST_NAME)).map_err(|_| {
        "Embedded runtime completion manifest is missing or unreadable.".to_string()
    })?;
    if actual != expected {
        return Err("Embedded runtime files no longer match their verified manifest. Close the app and remove its versioned tools cache before retrying.".into());
    }
    Ok(())
}

fn extract_entry(payload: &[u8], entry: &Entry, directory: &Path) -> Result<(), String> {
    let destination = directory.join(&entry.name);
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&destination)
        .map_err(|error| {
            format!(
                "Cannot create embedded runtime file {}: {error}",
                entry.name
            )
        })?;
    let compressed = payload
        .get(entry.compressed.clone())
        .ok_or_else(|| "Embedded runtime payload changed while extracting.".to_string())?;
    let mut decoder = DeflateDecoder::new(compressed);
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        // Permit one probe byte past the declared output size to detect corrupt
        // lengths and decompression bombs without unbounded allocations.
        let remaining = entry.uncompressed_len.saturating_sub(total);
        let limit = buffer.len().min(remaining.saturating_add(1) as usize);
        let count = decoder
            .read(&mut buffer[..limit])
            .map_err(|error| format!("Cannot decompress embedded {}: {error}", entry.name))?;
        if count == 0 {
            break;
        }
        if total.saturating_add(count as u64) > entry.uncompressed_len {
            return Err(format!(
                "Embedded runtime file {} exceeds its declared size.",
                entry.name
            ));
        }
        output
            .write_all(&buffer[..count])
            .map_err(|error| format!("Cannot write embedded {}: {error}", entry.name))?;
        hash.update(&buffer[..count]);
        total += count as u64;
    }
    let actual_hash: [u8; 32] = hash.finalize().into();
    if total != entry.uncompressed_len || actual_hash != entry.sha256 {
        return Err(format!(
            "Embedded runtime file {} failed its SHA-256 or size check.",
            entry.name
        ));
    }
    output
        .flush()
        .and_then(|()| output.sync_all())
        .map_err(|error| format!("Cannot flush embedded {}: {error}", entry.name))?;
    Ok(())
}

fn write_manifest(directory: &Path, bundle_id: &str, entries: &[Entry]) -> Result<(), String> {
    let contents = expected_manifest(directory, bundle_id, entries)?;
    let temporary = directory.join(format!("{MANIFEST_NAME}.tmp"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| format!("Cannot create runtime completion manifest: {error}"))?;
    file.write_all(contents.as_bytes())
        .and_then(|()| file.flush())
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("Cannot persist runtime completion manifest: {error}"))?;
    fs::rename(&temporary, directory.join(MANIFEST_NAME))
        .map_err(|error| format!("Cannot publish runtime completion manifest: {error}"))
}

fn ensure_runtime_at(payload: &[u8], base: &Path, bundle_id: &str) -> Result<PathBuf, String> {
    if bundle_id.is_empty()
        || bundle_id.len() > 64
        || !bundle_id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
    {
        return Err("Embedded runtime version identifier is invalid.".into());
    }
    let entries = parse_payload(payload)?;
    fs::create_dir_all(base)
        .map_err(|error| format!("Cannot create embedded runtime directory: {error}"))?;
    let final_dir = base.join(format!("runtime-{bundle_id}"));
    if final_dir.exists() {
        validate_runtime_dir(&final_dir, bundle_id, &entries)?;
        return Ok(final_dir);
    }

    // Separate staging directories allow concurrent starts without sharing
    // partial files. The final directory is visible only after verification.
    let mut staging = None;
    for _ in 0..32 {
        let sequence = EXTRACTION_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let candidate = base.join(format!(
            ".runtime-{bundle_id}-{}-{sequence}.staging",
            std::process::id()
        ));
        match fs::create_dir(&candidate) {
            Ok(()) => {
                staging = Some(candidate);
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("Cannot create runtime staging directory: {error}")),
        }
    }
    let staging =
        staging.ok_or_else(|| "Cannot allocate a unique runtime staging directory.".to_string())?;
    let extraction = (|| {
        for entry in &entries {
            extract_entry(payload, entry, &staging)?;
        }
        write_manifest(&staging, bundle_id, &entries)?;
        validate_runtime_dir(&staging, bundle_id, &entries)?;
        Ok::<(), String>(())
    })();
    if let Err(error) = extraction {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }

    match fs::rename(&staging, &final_dir) {
        Ok(()) => Ok(final_dir),
        Err(rename_error) if final_dir.exists() => {
            // Another process won publication. Verify its result; never replace
            // a directory that another process may already be using.
            let result = validate_runtime_dir(&final_dir, bundle_id, &entries);
            let _ = fs::remove_dir_all(&staging);
            result.map(|()| final_dir).map_err(|error| {
                format!("Another launch published an invalid runtime ({rename_error}); {error}")
            })
        }
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            Err(format!(
                "Cannot publish embedded runtime atomically: {error}"
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::DeflateEncoder;
    use flate2::Compression;
    use std::thread;

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn test_root(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "ytd-runtime-{label}-{}-{}",
            std::process::id(),
            TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn sample_files() -> Vec<(&'static str, Vec<u8>)> {
        REQUIRED_TOOLS
            .iter()
            .enumerate()
            .map(|(index, name)| (*name, vec![(index + 1) as u8; 1024 + index * 137]))
            .collect()
    }

    fn make_payload(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(MAGIC);
        payload.extend_from_slice(&(files.len() as u32).to_le_bytes());
        for (name, bytes) in files {
            let name_bytes = name.as_bytes();
            let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
            encoder.write_all(bytes).unwrap();
            let compressed = encoder.finish().unwrap();
            payload.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
            payload.extend_from_slice(name_bytes);
            payload.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
            payload.extend_from_slice(&(compressed.len() as u64).to_le_bytes());
            payload.extend_from_slice(&Sha256::digest(bytes));
            payload.extend_from_slice(&compressed);
        }
        payload
    }

    #[test]
    fn extracts_all_tools_and_reuses_verified_cache() {
        let root = test_root("reuse");
        let files = sample_files();
        let payload = make_payload(&files);
        let first = ensure_runtime_at(&payload, &root, "ab12cd34").unwrap();
        for (name, expected) in &files {
            assert_eq!(fs::read(first.join(name)).unwrap(), *expected);
        }
        let second = ensure_runtime_at(&payload, &root, "ab12cd34").unwrap();
        assert_eq!(first, second);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_or_truncated_payload_fails_without_publishing_partial_files() {
        let root = test_root("truncated");
        assert!(ensure_runtime_at(&[], &root, "ab12cd34").is_err());
        let mut payload = make_payload(&sample_files());
        payload.pop();
        assert!(ensure_runtime_at(&payload, &root, "ab12cd34").is_err());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn corrupt_file_digest_fails_and_removes_staging_directory() {
        let root = test_root("digest");
        let mut files = sample_files();
        files[0].1[0] ^= 1;
        let payload = make_payload(&files);
        let mut corrupted = payload.clone();
        // Flip a stored expected digest without changing the compressed bytes.
        let digest_offset = 8 + 4 + 2 + REQUIRED_TOOLS[0].len() + 8 + 8;
        corrupted[digest_offset] ^= 1;
        assert!(ensure_runtime_at(&corrupted, &root, "ab12cd34").is_err());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_extraction_publishes_one_complete_directory() {
        let root = test_root("concurrent");
        let payload = std::sync::Arc::new(make_payload(&sample_files()));
        let base = std::sync::Arc::new(root.clone());
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let payload = payload.clone();
                let base = base.clone();
                thread::spawn(move || ensure_runtime_at(&payload, &base, "ab12cd34").unwrap())
            })
            .collect();
        let paths: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert!(paths.iter().all(|path| path == &paths[0]));
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        for name in REQUIRED_TOOLS {
            assert!(paths[0].join(name).is_file());
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unexpected_names_and_invalid_bundle_ids_are_rejected() {
        let root = test_root("invalid");
        assert!(ensure_runtime_at(&make_payload(&sample_files()), &root, "../escape").is_err());
        let mut files = sample_files();
        files[0].0 = "..\\outside.exe";
        assert!(parse_payload(&make_payload(&files)).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dependency_resolution_rejects_missing_tools_and_untrusted_names() {
        let root = test_root("dependency");
        let expected = root.join("yt-dlp.exe");
        fs::write(&expected, b"tool fixture").unwrap();
        assert_eq!(
            resolve_embedded_dependency(&root, "yt-dlp.exe").unwrap(),
            expected
        );
        assert!(resolve_embedded_dependency(&root, "ffmpeg.exe").is_err());
        assert!(resolve_embedded_dependency(&root, "..\\outside.exe").is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
