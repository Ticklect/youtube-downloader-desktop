use serde::de::{IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use url::Url;

pub const CHANNEL_PAGE_SIZE: usize = 200;
pub const MAX_CHANNEL_VIDEOS: usize = 1_000;
pub const MAX_SELECTED: usize = 1_000;
pub const MAX_PARALLEL: usize = 3;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Video {
    pub id: String,
    pub title: String,
    pub duration: String,
    #[serde(default)]
    pub uploaded: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Video,
    Audio,
    Captions,
}

impl Mode {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "video" => Ok(Self::Video),
            "audio" => Ok(Self::Audio),
            "captions" => Ok(Self::Captions),
            _ => Err("Choose a supported download mode.".into()),
        }
    }
}

pub fn validate_id(value: &str) -> bool {
    value.len() == 11
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub fn channel_url(input: &str) -> Result<String, String> {
    let input = input.trim();
    if input.is_empty() || input.len() > 2048 {
        return Err("Enter a YouTube channel URL.".into());
    }
    let expanded = if input.starts_with('@') {
        format!("https://www.youtube.com/{input}")
    } else if input.starts_with("youtube.com/") || input.starts_with("www.youtube.com/") {
        format!("https://{input}")
    } else {
        input.to_string()
    };
    let url = Url::parse(&expanded).map_err(|_| "Enter a valid YouTube channel URL.".to_owned())?;
    if url.scheme() != "https"
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || !matches!(
            url.host_str(),
            Some("www.youtube.com" | "youtube.com" | "m.youtube.com")
        )
    {
        return Err("Only secure youtube.com channel addresses are supported.".into());
    }
    let mut parts: Vec<&str> = url
        .path_segments()
        .ok_or("Invalid channel address.")?
        .filter(|s| !s.is_empty())
        .collect();
    if parts.last() == Some(&"videos") {
        parts.pop();
    }
    let valid = match parts.as_slice() {
        [handle] if handle.starts_with('@') => {
            handle.len() >= 4
                && handle.len() <= 61
                && handle[1..]
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        }
        ["channel", id] => {
            id.len() == 24
                && id.starts_with("UC")
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
        }
        ["c" | "user", label] => {
            !label.is_empty()
                && label.len() <= 100
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        }
        _ => false,
    };
    if !valid {
        return Err("Use a channel URL such as https://www.youtube.com/@creator.".into());
    }
    Ok(format!(
        "https://www.youtube.com/{}/videos",
        parts.join("/")
    ))
}

pub fn parse_channel_json(text: &str) -> Result<(String, Vec<Video>), String> {
    parse_channel_page_json(text, false)
}

#[derive(Deserialize)]
struct ChannelMetadata {
    #[serde(default)]
    channel: Value,
    #[serde(default)]
    title: Value,
    #[serde(default)]
    entries: Option<Vec<ChannelMetadataEntry>>,
}

#[derive(Default, Deserialize)]
struct ChannelMetadataFields {
    #[serde(default)]
    id: Value,
    #[serde(default)]
    title: Value,
    #[serde(default)]
    duration: Value,
    #[serde(default)]
    upload_date: Value,
}

#[derive(Default)]
struct ChannelMetadataEntry(ChannelMetadataFields);

impl<'de> Deserialize<'de> for ChannelMetadataEntry {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct EntryVisitor;
        impl<'de> Visitor<'de> for EntryVisitor {
            type Value = ChannelMetadataEntry;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a yt-dlp playlist entry or an unusable JSON value")
            }
            fn visit_map<M: MapAccess<'de>>(self, map: M) -> Result<Self::Value, M::Error> {
                ChannelMetadataFields::deserialize(serde::de::value::MapAccessDeserializer::new(
                    map,
                ))
                .map(ChannelMetadataEntry)
            }
            fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<Self::Value, S::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(ChannelMetadataEntry::default())
            }
            fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(ChannelMetadataEntry::default())
            }
            fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(ChannelMetadataEntry::default())
            }
            fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(ChannelMetadataEntry::default())
            }
            fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(ChannelMetadataEntry::default())
            }
            fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(ChannelMetadataEntry::default())
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(ChannelMetadataEntry::default())
            }
        }
        deserializer.deserialize_any(EntryVisitor)
    }
}

fn parse_channel_page_json(text: &str, allow_empty: bool) -> Result<(String, Vec<Video>), String> {
    if text.len() > 12_000_000 {
        return Err("Channel metadata is unexpectedly large.".into());
    }
    let document: ChannelMetadata = serde_json::from_str(text)
        .map_err(|_| "yt-dlp returned unreadable channel metadata.".to_owned())?;
    let name = document
        .channel
        .as_str()
        .or(document.title.as_str())
        .unwrap_or("YouTube channel");
    let entries = document
        .entries
        .as_deref()
        .ok_or("yt-dlp did not return a channel video list.")?;
    let mut found = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for entry in entries.iter().take(CHANNEL_PAGE_SIZE * 2) {
        let entry = &entry.0;
        let id = entry.id.as_str().unwrap_or("");
        if !validate_id(id) || !seen.insert(id.to_owned()) {
            continue;
        }
        let title = entry.title.as_str().unwrap_or("Untitled video");
        let title: String = title
            .chars()
            .filter(|ch| !ch.is_control())
            .take(200)
            .collect();
        let seconds = entry.duration.as_u64();
        let duration = seconds
            .map(|s| format!("{}:{:02}", s / 60, s % 60))
            .unwrap_or_else(|| "Duration unknown".into());
        let uploaded = entry
            .upload_date
            .as_str()
            .filter(|date| date.len() == 8 && date.bytes().all(|c| c.is_ascii_digit()))
            .map(|date| format!("{}-{}-{}", &date[..4], &date[4..6], &date[6..]))
            .unwrap_or_default();
        found.push(Video {
            id: id.to_owned(),
            title,
            duration,
            uploaded,
        });
        if found.len() >= CHANNEL_PAGE_SIZE {
            break;
        }
    }
    if found.is_empty() && !allow_empty {
        return Err("No public videos were returned for this channel.".into());
    }
    Ok((name.chars().take(100).collect(), found))
}

pub fn download_arguments(
    video: &Video,
    mode: Mode,
    quality: &str,
    root: &Path,
) -> Result<Vec<String>, String> {
    if !validate_id(&video.id) {
        return Err("Invalid video ID.".into());
    }
    if !matches!(quality, "best" | "1080" | "720" | "360") {
        return Err("Unsupported quality.".into());
    }
    if !root.is_absolute() {
        return Err("Choose an absolute download folder.".into());
    }
    let mut args = vec!["--ignore-config", "--no-playlist", "--windows-filenames", "--continue",
        "--no-overwrites", "--no-warnings", "--newline", "--progress", "--concurrent-fragments", "4",
        "--progress-template", "download:__YCD_PROGRESS__%(progress._percent_str)s|%(progress._speed_str)s|%(progress._eta_str)s",
        "--paths", "home:", "-o", "%(uploader|Channel).90B/%(title).150B [%(id)s].%(ext)s"]
        .into_iter().map(str::to_string).collect::<Vec<_>>();
    let paths_index = args.iter().position(|x| x == "home:").unwrap();
    args[paths_index] = format!("home:{}", root.display());
    if mode == Mode::Video {
        // A Best and a 1080p (or 720p) request can both resolve to the same
        // actual height. Include the requested quality so --no-overwrites
        // never treats two different selections as the same completed file.
        let quality_label = if quality == "best" {
            "best".to_owned()
        } else {
            format!("up-to-{quality}p")
        };
        if let Some(template) = args.iter_mut().find(|arg| arg.contains("[%(id)s]")) {
            *template = format!(
                "%(uploader|Channel).90B/%(title).150B [%(id)s] [{quality_label}] [%(height)sp].%(ext)s"
            );
        }
    }
    match mode {
        Mode::Video => {
            let format = if quality == "best" {
                "bv*[vcodec^=avc1]+ba[acodec^=mp4a]/b[ext=mp4]/bv*+ba/b".to_string()
            } else {
                format!("bv*[vcodec^=avc1][height<={quality}]+ba[acodec^=mp4a]/b[ext=mp4][height<={quality}]/bv*[height<={quality}]+ba/b[height<={quality}]")
            };
            args.extend([
                "-f".into(),
                format,
                "--merge-output-format".into(),
                "mp4/mkv".into(),
            ]);
        }
        Mode::Audio => args.extend([
            "-f".into(),
            "bestaudio/best".into(),
            "--extract-audio".into(),
            "--audio-format".into(),
            "mp3".into(),
            "--audio-quality".into(),
            "0".into(),
        ]),
        Mode::Captions => args.extend([
            "--skip-download".into(),
            "--write-subs".into(),
            "--write-auto-subs".into(),
            "--sub-langs".into(),
            "en.*,en".into(),
            "--sub-format".into(),
            "vtt/best".into(),
        ]),
    }
    // yt-dlp can exit successfully without writing anything (notably when
    // English captions are unavailable). Print the actual final output paths
    // so download_video can distinguish saved files from an empty success.
    args.push("--no-simulate".into());
    args.push("--print".into());
    args.push(
        match mode {
            Mode::Captions => "after_video:__YCD_CAPTIONS__%(requested_subtitles)j",
            Mode::Video | Mode::Audio => "after_move:__YCD_FILE__%(filepath)j",
        }
        .into(),
    );
    args.push(format!("https://www.youtube.com/watch?v={}", video.id));
    Ok(args)
}

fn is_saved_file_in_folder(path: &str, folder: &Path) -> bool {
    let Ok(root) = folder.canonicalize() else {
        return false;
    };
    let Ok(file) = Path::new(path).canonicalize() else {
        return false;
    };
    file.starts_with(root)
        && std::fs::metadata(file).is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
}

fn validate_download_output(mode: Mode, folder: &Path, output: &str) -> Result<(), String> {
    match mode {
        Mode::Captions => {
            let saved = output
                .lines()
                .filter_map(|line| line.strip_prefix("__YCD_CAPTIONS__"))
                .filter_map(|json| serde_json::from_str::<Value>(json).ok())
                .filter_map(|value| value.as_object().cloned())
                .flat_map(|subtitles| subtitles.into_values())
                .filter_map(|subtitle| subtitle["filepath"].as_str().map(str::to_owned))
                .any(|path| is_saved_file_in_folder(&path, folder));
            if !saved {
                return Err("No English captions were saved for this video.".into());
            }
        }
        Mode::Video | Mode::Audio => {
            let saved = output
                .lines()
                .filter_map(|line| line.strip_prefix("__YCD_FILE__"))
                .filter_map(|json| serde_json::from_str::<String>(json).ok())
                .any(|path| is_saved_file_in_folder(&path, folder));
            if !saved {
                return Err("yt-dlp finished without saving the requested file.".into());
            }
        }
    }
    Ok(())
}

pub fn parse_percent(line: &str) -> Option<(i32, String)> {
    let value = line.strip_prefix("__YCD_PROGRESS__")?;
    let mut fields = value.split('|');
    let numeric = fields
        .next()?
        .trim()
        .trim_end_matches('%')
        .trim()
        .parse::<f64>()
        .ok()?;
    if !numeric.is_finite() {
        return None;
    }
    let speed = fields.next().unwrap_or("").trim();
    let eta = fields.next().unwrap_or("").trim();
    Some((
        (numeric.round() as i32).clamp(0, 100),
        format!(
            "{} · ETA {}",
            speed.chars().take(30).collect::<String>(),
            eta.chars().take(20).collect::<String>()
        ),
    ))
}

pub fn find_dependency(file: &str) -> Result<PathBuf, String> {
    // A standalone executable must always use the tools embedded in that exact
    // build. Do not silently substitute a stale sibling executable or PATH copy.
    if crate::runtime::has_embedded_payload() {
        let directory = crate::runtime::embedded_runtime_dir()?
            .ok_or_else(|| "The embedded runtime is unavailable.".to_string())?;
        return crate::runtime::resolve_embedded_dependency(&directory, file);
    }
    let here = std::env::current_exe()
        .map_err(|_| "Cannot locate the application executable.".to_string())?;
    let parent = here
        .parent()
        .ok_or("Cannot locate the application folder.")?;
    for candidate in [parent.join("bin").join(file), parent.join(file)] {
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    // Developer convenience only. Release ZIP bundles both dependencies.
    if cfg!(debug_assertions) {
        if let Some(paths) = std::env::var_os("PATH") {
            for path in std::env::split_paths(&paths) {
                let candidate = path.join(file);
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
        }
    }
    Err(format!(
        "Missing {file}. Install the complete desktop package containing the bin folder."
    ))
}

pub fn load_channel(executable: &Path, url: &str) -> Result<(String, Vec<Video>), String> {
    load_channel_page(executable, url, 1)
}

pub fn load_channel_page(
    executable: &Path,
    url: &str,
    start: usize,
) -> Result<(String, Vec<Video>), String> {
    let (page_start, page_end) = channel_page_args(start)?;
    let url = channel_url(url)?;
    let runtime = executable
        .parent()
        .ok_or("yt-dlp has no parent directory.")?
        .join("deno.exe");
    if !runtime.is_file() {
        return Err("The bundled Deno runtime is missing from the bin folder.".into());
    }
    let runtime_arg = format!("deno:{}", runtime.display());
    let mut command = Command::new(executable);
    command
        .args([
            "--ignore-config",
            "--js-runtimes",
            &runtime_arg,
            "--flat-playlist",
            "--dump-single-json",
            "--playlist-start",
            &page_start,
            "--playlist-end",
            &page_end,
            "--ignore-errors",
            "--no-warnings",
            "--socket-timeout",
            "12",
            &url,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hide_console(&mut command);
    let child = command
        .spawn()
        .map_err(|e| format!("Unable to launch yt-dlp: {e}"))?;
    let output = run_bounded(child, Duration::from_secs(100), None, |_, _| {})?;
    if !output.0 {
        return Err(
            "yt-dlp could not load the channel. Check the address and your network connection."
                .into(),
        );
    }
    if start == 1 {
        parse_channel_json(&output.1)
    } else {
        parse_channel_page_json(&output.1, true)
    }
}

fn channel_page_args(start: usize) -> Result<(String, String), String> {
    if start == 0 || start > MAX_CHANNEL_VIDEOS || !(start - 1).is_multiple_of(CHANNEL_PAGE_SIZE) {
        return Err("Invalid channel page offset.".into());
    }
    Ok((
        start.to_string(),
        (start + CHANNEL_PAGE_SIZE - 1)
            .min(MAX_CHANNEL_VIDEOS)
            .to_string(),
    ))
}

#[cfg(windows)]
fn hide_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x0800_0000);
}

#[cfg(not(windows))]
fn hide_console(_command: &mut Command) {}

// Windows job objects guarantee that FFmpeg and other grandchildren die when
// our desktop app exits, even if its UI is closed unexpectedly.
#[cfg(windows)]
struct KillOnCloseJob(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl KillOnCloseJob {
    fn attach(child: &std::process::Child) -> Result<Self, String> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };
        // SAFETY: all pointers are either null or point to valid initialized
        // structs. The returned HANDLE is owned by Self and closed in Drop.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return Err(format!(
                    "Could not create download process guard: {}",
                    std::io::Error::last_os_error()
                ));
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let info_size = std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32;
            if SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &raw const info as *const std::ffi::c_void,
                info_size,
            ) == 0
            {
                let error = std::io::Error::last_os_error();
                CloseHandle(job);
                return Err(format!(
                    "Could not configure download process guard: {error}"
                ));
            }
            if AssignProcessToJobObject(job, child.as_raw_handle()) == 0 {
                let error = std::io::Error::last_os_error();
                CloseHandle(job);
                return Err(format!("Could not attach download process guard: {error}"));
            }
            Ok(Self(job))
        }
    }
}

#[cfg(windows)]
impl Drop for KillOnCloseJob {
    fn drop(&mut self) {
        // SAFETY: this owns the HANDLE from CreateJobObjectW. Closing the
        // handle terminates any remaining child processes in the job.
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

fn stop_process_tree(child: &mut std::process::Child) {
    // yt-dlp may have launched FFmpeg. Stop its descendants as well as yt-dlp
    // instead of leaving a transcoder running after cancellation on Windows.
    #[cfg(windows)]
    {
        let mut command = Command::new(r"C:\Windows\System32\taskkill.exe");
        hide_console(&mut command);
        let _ = command
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Read through a complete line, but retain at most `limit` bytes. An
/// unterminated diagnostic must not cause read_until() to allocate gigabytes
/// before the subprocess deadline/cancellation is checked.
///
/// Delimiters are still consumed after truncation, preserving the following
/// yt-dlp progress and ERROR lines. This only discards text that exceeded the
/// existing stdout cap or stderr diagnostic tail budget.
fn read_bounded_line(
    reader: &mut impl BufRead,
    line: &mut Vec<u8>,
    limit: usize,
) -> io::Result<usize> {
    line.clear();
    if limit == 0 {
        return reader.skip_until(b'\n');
    }
    // read_until and skip_until use the standard library's optimized newline
    // search. Limit the first read so it cannot grow an unbounded Vec, then
    // consume any overflow without allocating it.
    let prefix_bytes = io::Read::take(&mut *reader, limit as u64).read_until(b'\n', line)?;
    if prefix_bytes == 0 || line.last() == Some(&b'\n') {
        return Ok(prefix_bytes);
    }
    let overflow_bytes = reader.skip_until(b'\n')?;
    if overflow_bytes > 0 {
        // Preserve a boundary even when the stored prefix was truncated.
        // Without this, the next ERROR line can merge with the prior text.
        if let Some(last) = line.last_mut() {
            *last = b'\n';
        }
    }
    Ok(prefix_bytes.saturating_add(overflow_bytes))
}

// Pipes are drained on dedicated reader threads so the child cannot block on
// a full pipe. Individual lines and aggregate output are bounded.
pub fn run_bounded(
    mut child: std::process::Child,
    deadline: Duration,
    cancel: Option<Arc<AtomicBool>>,
    on_progress: impl Fn(i32, String) + Send + 'static,
) -> Result<(bool, String), String> {
    #[cfg(windows)]
    let _job_guard = match KillOnCloseJob::attach(&child) {
        Ok(job) => job,
        Err(error) => {
            stop_process_tree(&mut child);
            return Err(error);
        }
    };
    let stdout = match child.stdout.take() {
        Some(output) => output,
        None => {
            stop_process_tree(&mut child);
            return Err("Unable to read yt-dlp output.".into());
        }
    };
    let stderr = match child.stderr.take() {
        Some(output) => output,
        None => {
            stop_process_tree(&mut child);
            return Err("Unable to read yt-dlp diagnostics.".into());
        }
    };
    let on_progress = Arc::new(Mutex::new(on_progress));
    let stdout_progress = Arc::clone(&on_progress);
    let reader = std::thread::spawn(move || {
        let mut output = String::new();
        let mut reader = BufReader::new(stdout);
        let mut line = Vec::new();
        while read_bounded_line(&mut reader, &mut line, 12_000_000).unwrap_or(0) != 0 {
            if output.len() < 12_000_000 {
                let text = String::from_utf8_lossy(&line);
                if let Some((percent, detail)) = parse_percent(text.trim()) {
                    if let Ok(callback) = stdout_progress.lock() {
                        callback(percent, detail);
                    }
                } else {
                    let room = 12_000_000 - output.len();
                    let mut end = text.len().min(room);
                    while !text.is_char_boundary(end) {
                        end -= 1;
                    }
                    output.push_str(&text[..end]);
                }
            }
            line.clear();
        }
        output
    });
    let stderr_progress = on_progress;
    let err_reader = std::thread::spawn(move || {
        let mut result = Vec::new();
        let mut input = BufReader::new(stderr);
        let mut line = Vec::new();
        while read_bounded_line(&mut input, &mut line, 16_384).unwrap_or(0) != 0 {
            let text = String::from_utf8_lossy(&line);
            if let Some((percent, detail)) = parse_percent(text.trim()) {
                if let Ok(callback) = stderr_progress.lock() {
                    callback(percent, detail);
                }
            } else {
                // yt-dlp's actionable ERROR is usually its last diagnostic.
                // Keep the tail without allowing unbounded stderr growth.
                let skip = line.len().saturating_sub(16_384);
                let chunk = &line[skip..];
                let excess = result
                    .len()
                    .saturating_add(chunk.len())
                    .saturating_sub(16_384);
                result.drain(..excess);
                result.extend_from_slice(chunk);
            }
            line.clear();
        }
        String::from_utf8_lossy(&result).to_string()
    });
    let began = Instant::now();
    let (status, reason) = loop {
        if cancel
            .as_ref()
            .is_some_and(|value| value.load(Ordering::Relaxed))
        {
            stop_process_tree(&mut child);
            break (false, "Download cancelled.".to_owned());
        }
        if began.elapsed() > deadline {
            stop_process_tree(&mut child);
            break (false, "yt-dlp timed out.".to_owned());
        }
        match child.try_wait() {
            Ok(Some(status)) => break (status.success(), String::new()),
            Ok(None) => {
                #[cfg(windows)]
                {
                    use std::os::windows::io::AsRawHandle;
                    // A timed kernel wait wakes immediately when yt-dlp exits.
                    // The 20 ms bound keeps cancellation and deadlines responsive
                    // without waking the CPU continuously during long downloads.
                    let waited = unsafe {
                        windows_sys::Win32::System::Threading::WaitForSingleObject(
                            child.as_raw_handle(),
                            20,
                        )
                    };
                    if waited == windows_sys::Win32::Foundation::WAIT_FAILED {
                        let error = std::io::Error::last_os_error();
                        stop_process_tree(&mut child);
                        break (false, format!("Unable to wait for yt-dlp: {error}"));
                    }
                }
                #[cfg(not(windows))]
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => {
                stop_process_tree(&mut child);
                break (false, e.to_string());
            }
        }
    };
    // Descendants can inherit yt-dlp's pipe handles. Once yt-dlp has exited,
    // terminate any surviving descendants so joining the readers cannot hang.
    #[cfg(windows)]
    drop(_job_guard);
    let stdout = reader
        .join()
        .map_err(|_| "Download output reader crashed.")?;
    let errors = err_reader.join().unwrap_or_default();
    if !status {
        let message = if reason.is_empty() {
            errors
                .lines()
                .rfind(|line| line.starts_with("ERROR:"))
                .unwrap_or("yt-dlp could not finish this download.")
                .to_owned()
        } else {
            reason
        };
        return Err(message.chars().take(280).collect());
    }
    Ok((true, stdout))
}

pub struct DownloadSpec<'a> {
    pub executable: &'a Path,
    pub ffmpeg: &'a Path,
    pub video: &'a Video,
    pub mode: Mode,
    pub quality: &'a str,
    pub folder: &'a Path,
}

pub fn download_video(
    spec: DownloadSpec<'_>,
    cancel: Arc<AtomicBool>,
    progress: impl Fn(i32, String) + Send + 'static,
) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        return Err("Download cancelled.".into());
    }
    let mut args = download_arguments(spec.video, spec.mode, spec.quality, spec.folder)?;
    let bin_dir = spec
        .ffmpeg
        .parent()
        .ok_or("Cannot locate FFmpeg directory.")?;
    let runtime = bin_dir.join("deno.exe");
    if !runtime.is_file() {
        return Err("The bundled Deno runtime is missing from the bin folder.".into());
    }
    args.insert(0, format!("{}", bin_dir.display()));
    args.insert(0, "--ffmpeg-location".to_string());
    args.insert(0, format!("deno:{}", runtime.display()));
    args.insert(0, "--js-runtimes".to_string());
    let mut command = Command::new(spec.executable);
    command
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hide_console(&mut command);
    let child = command
        .spawn()
        .map_err(|e| format!("Unable to launch download: {e}"))?;
    let (_, output) = run_bounded(child, Duration::from_secs(4 * 3600), Some(cancel), progress)?;
    validate_download_output(spec.mode, spec.folder, &output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "offline channel metadata CPU benchmark, no yt-dlp or network"]
    fn benchmark_channel_metadata_parsing_realistic_sizes() {
        for count in [10usize, 100, 200, 1000] {
            let mut entries = Vec::with_capacity(count);
            for i in 0..count {
                entries.push(serde_json::json!({
                    "id": format!("{i:011}"),
                    "title": format!("Episode {i} - a useful title for a realistic creator upload"),
                    "duration": 3872,
                    "upload_date": "20261025",
                    "url": format!("https://www.youtube.com/watch?v={i:011}"),
                    "webpage_url": format!("https://www.youtube.com/watch?v={i:011}"),
                    "thumbnails": [
                        {"url":"https://i.ytimg.com/vi/example/hqdefault.jpg", "width": 480, "height": 360},
                        {"url":"https://i.ytimg.com/vi/example/mqdefault.jpg", "width": 320, "height": 180},
                        {"url":"https://i.ytimg.com/vi/example/sddefault.jpg", "width": 640, "height": 480}
                    ],
                    "tags": ["tutorial", "content", "2026", "video"],
                    "channel_id": "UCaaaaaaaaaaaaaaaaaaaaaa",
                    "channel_follower_count": 345000,
                    "extra": {"format_id": "some", "duration_string": "64:32", "formats": ["mp4","webm"]}
                }));
            }
            let json = serde_json::to_string(
                &serde_json::json!({"channel": "Example channel", "entries": entries}),
            )
            .unwrap();
            let iterations = if count >= 1000 { 50 } else { 150 };
            let mut elapsed = Vec::with_capacity(iterations);
            for _ in 0..iterations {
                let started = Instant::now();
                let (_, videos) = parse_channel_page_json(&json, true).unwrap();
                assert_eq!(videos.len(), count.min(200));
                elapsed.push(started.elapsed().as_micros());
            }
            elapsed.sort_unstable();
            println!(
                "metadata entries={count} json_kib={} iterations={iterations}: p50={}us p95={}us",
                json.len() / 1024,
                elapsed[iterations / 2],
                elapsed[iterations * 95 / 100]
            );
        }
    }

    #[test]
    fn long_unterminated_lines_are_bounded_and_following_progress_is_preserved() {
        let mut stream = vec![b'x'; 20_000_000];
        stream.extend_from_slice(b"\n__YCD_PROGRESS__87%|3MiB/s|00:01\nERROR: LAST_DIAGNOSTIC\n");
        let mut reader = BufReader::new(stream.as_slice());
        let mut line = Vec::new();
        assert_eq!(
            read_bounded_line(&mut reader, &mut line, 16_384).unwrap(),
            20_000_001
        );
        assert_eq!(line.len(), 16_384);
        assert!(line.capacity() <= 16_384);
        assert_eq!(line.last(), Some(&b'\n'));
        let next_size = read_bounded_line(&mut reader, &mut line, 16_384).unwrap();
        assert!(next_size > 0);
        assert_eq!(
            parse_percent(String::from_utf8_lossy(&line).trim())
                .unwrap()
                .0,
            87
        );
        assert!(read_bounded_line(&mut reader, &mut line, 16_384).unwrap() > 0);
        assert_eq!(
            String::from_utf8_lossy(&line).trim(),
            "ERROR: LAST_DIAGNOSTIC"
        );
        assert_eq!(
            read_bounded_line(&mut reader, &mut line, 16_384).unwrap(),
            0
        );
    }

    #[test]
    fn bounded_reader_preserves_crlf_exact_limit_and_unterminated_eof() {
        let input = b"ERROR: first\r\n__YCD_PROGRESS__42%|3MiB/s|00:01\r\nERROR: last";
        let mut reader = BufReader::with_capacity(3, &input[..]);
        let mut line = Vec::new();
        assert_eq!(
            read_bounded_line(&mut reader, &mut line, 16_384).unwrap(),
            14
        );
        assert_eq!(&line, b"ERROR: first\r\n");
        assert!(read_bounded_line(&mut reader, &mut line, 16_384).unwrap() > 0);
        assert_eq!(
            parse_percent(String::from_utf8_lossy(&line).trim())
                .unwrap()
                .0,
            42
        );
        assert_eq!(
            read_bounded_line(&mut reader, &mut line, 16_384).unwrap(),
            11
        );
        assert_eq!(&line, b"ERROR: last");
        assert_eq!(
            read_bounded_line(&mut reader, &mut line, 16_384).unwrap(),
            0
        );

        let bytes = b"1234567\nNEXT\r\n";
        let mut exact_reader = BufReader::new(&bytes[..]);
        assert_eq!(
            read_bounded_line(&mut exact_reader, &mut line, 8).unwrap(),
            8
        );
        assert_eq!(&line, b"1234567\n");
        assert_eq!(
            read_bounded_line(&mut exact_reader, &mut line, 8).unwrap(),
            6
        );
        assert_eq!(&line, b"NEXT\r\n");

        let mut overflow_crlf = BufReader::new(&b"xxxxxxxxxxxxxxxx\r\nERROR: preserved\r\n"[..]);
        assert_eq!(
            read_bounded_line(&mut overflow_crlf, &mut line, 8).unwrap(),
            18
        );
        assert_eq!(line.len(), 8);
        assert_eq!(line.last(), Some(&b'\n'));
        assert_eq!(
            read_bounded_line(&mut overflow_crlf, &mut line, 32).unwrap(),
            18
        );
        assert_eq!(&line, b"ERROR: preserved\r\n");
    }

    #[test]
    #[ignore = "offline bounded-line parser benchmark, 10-1000 progress events and long lines"]
    fn benchmark_bounded_line_transport() {
        for lines in [10usize, 100, 1000, 10000] {
            let input = b"__YCD_PROGRESS__42.0%|3MiB/s|00:01\n".repeat(lines);
            let old_started = Instant::now();
            let mut old = BufReader::new(input.as_slice());
            let mut line = Vec::new();
            let mut count = 0;
            while old.read_until(b'\n', &mut line).unwrap() > 0 {
                count += 1;
                line.clear();
            }
            let old_time = old_started.elapsed();
            assert_eq!(count, lines);
            let new_started = Instant::now();
            let mut new = BufReader::new(input.as_slice());
            let mut bounded = Vec::new();
            let mut count = 0;
            while read_bounded_line(&mut new, &mut bounded, 16_384).unwrap() > 0 {
                count += 1;
            }
            let new_time = new_started.elapsed();
            assert_eq!(count, lines);
            println!(
                "Progress lines={lines}: read_until={old_time:?} bounded={new_time:?} ratio={:.2}x",
                old_time.as_secs_f64() / new_time.as_secs_f64()
            );
        }
        let line_bytes = 20_000_000;
        let bytes = vec![b'x'; line_bytes];
        let old_started = Instant::now();
        let mut old = BufReader::new(bytes.as_slice());
        let mut old_line = Vec::new();
        assert_eq!(old.read_until(b'\n', &mut old_line).unwrap(), line_bytes);
        let old_time = old_started.elapsed();
        let new_started = Instant::now();
        let mut new = BufReader::new(bytes.as_slice());
        let mut new_line = Vec::new();
        assert_eq!(
            read_bounded_line(&mut new, &mut new_line, 16_384).unwrap(),
            line_bytes
        );
        let new_time = new_started.elapsed();
        println!(
            "20 MB unterminated stderr: read_until={old_time:?} allocated={} bytes; bounded={new_time:?} allocated={} bytes",
            old_line.capacity(),
            new_line.capacity()
        );
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "offline 1- and 3-process yt-dlp progress transport proxy; no media/network"]
    fn benchmark_mock_parallel_progress_transport() {
        use std::sync::atomic::AtomicUsize;
        for (parallel, events) in [(1usize, 100usize), (3, 100), (3, 1000)] {
            let started = Instant::now();
            let mut handles = Vec::new();
            for _ in 0..parallel {
                handles.push(std::thread::spawn(move || {
                    let script = format!(
                        "for($i=0;$i -lt {events};$i++){{[Console]::Error.WriteLine('__YCD_PROGRESS__42%|3MiB/s|00:01')}};[Console]::Out.WriteLine('MOCK_OK')"
                    );
                    let child = Command::new("powershell.exe")
                        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped())
                        .spawn()
                        .unwrap();
                    let count = Arc::new(AtomicUsize::new(0));
                    let received = Arc::clone(&count);
                    let result = run_bounded(child, Duration::from_secs(15), None, move |_, _| {
                        received.fetch_add(1, Ordering::Relaxed);
                    })
                    .unwrap();
                    assert!(result.1.contains("MOCK_OK"));
                    assert_eq!(count.load(Ordering::Relaxed), events);
                }));
            }
            for handle in handles {
                handle.join().unwrap();
            }
            println!(
                "Mock yt-dlp transport parallel={parallel} events/child={events}: elapsed={:?} total_events={}",
                started.elapsed(),
                parallel * events
            );
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "manual, offline subprocess latency measurement"]
    fn bench_short_lived_mock_process() {
        let samples = 24;
        let mut elapsed = Vec::new();
        for _ in 0..samples {
            let child = Command::new("cmd.exe")
                .args(["/C", "echo MOCK_METADATA_OK"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let start = Instant::now();
            let output = run_bounded(child, Duration::from_secs(5), None, |_, _| {}).unwrap();
            assert!(output.1.contains("MOCK_METADATA_OK"));
            elapsed.push(start.elapsed().as_micros());
        }
        elapsed.sort_unstable();
        println!(
            "mock process (n={}): min={}us median={}us p90={}us max={}us",
            samples,
            elapsed[0],
            elapsed[samples / 2],
            elapsed[samples * 9 / 10],
            elapsed[samples - 1]
        );
    }

    #[test]
    fn accepts_only_real_channel_addresses() {
        assert_eq!(
            channel_url("@ExampleCreator").unwrap(),
            "https://www.youtube.com/@ExampleCreator/videos"
        );
        assert_eq!(
            channel_url("https://m.youtube.com/channel/UCaaaaaaaaaaaaaaaaaaaaaa/videos").unwrap(),
            "https://www.youtube.com/channel/UCaaaaaaaaaaaaaaaaaaaaaa/videos"
        );
        for bad in [
            "",
            "https://evil.test/@creator",
            "https://youtube.com.evil.test/@creator",
            "http://youtube.com/@creator",
            "https://youtube.com/watch?v=abcdefghijk",
            "https://youtube.com@evil.test/@creator",
            "https://youtube.com:444/@creator",
            "https://youtube.com/@hello/../watch",
        ] {
            assert!(channel_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn parser_handles_missing_and_duplicate_entries() {
        let input = r#"{"channel":"My channel","entries":[{"id":"abcdefghijk","title":"First","duration":65},{"id":"abcdefghijk","title":"duplicate"},{"id":"bad","title":"invalid"},{"id":"___________","title":"Second"}]}"#;
        let (name, videos) = parse_channel_json(input).unwrap();
        assert_eq!(name, "My channel");
        assert_eq!(videos.len(), 2);
        assert_eq!(videos[0].duration, "1:05");
        assert!(parse_channel_json("not JSON").is_err());
        assert!(parse_channel_json(r#"{"channel":"Empty","entries":[]}"#).is_err());
        assert_eq!(
            parse_channel_page_json(r#"{"channel":"Empty","entries":[]}"#, true)
                .unwrap()
                .1
                .len(),
            0
        );
        for input in [
            r#"{"channel":"Missing"}"#,
            r#"{"channel":"Null","entries":null}"#,
        ] {
            assert_eq!(
                parse_channel_page_json(input, true).unwrap_err(),
                "yt-dlp did not return a channel video list."
            );
        }
    }

    #[test]
    fn parser_skips_non_object_entries_without_discarding_valid_metadata() {
        let source = r#"{"channel":null,"title":"Fallback title","entries":[null,false,17,2.5,"bad",[],{},
        {"id":"abcdefghijk","title":23,"duration":"bad","upload_date":false,"unused":{"large":["nested","discarded"]}},
        {"id":"pqrstuvwxyz","title":"Preserved","duration":60,"upload_date":"20261029","unused":{"metadata":[1,2,3]}},
        {"id":"abcdefghijk","title":"Duplicate"}]}"#;
        let (name, videos) = parse_channel_page_json(source, false).unwrap();
        assert_eq!(name, "Fallback title");
        assert_eq!(videos.len(), 2);
        assert_eq!(videos[0].title, "Untitled video");
        assert_eq!(videos[0].duration, "Duration unknown");
        assert_eq!(videos[1].title, "Preserved");
        assert_eq!(videos[1].duration, "1:00");
        assert_eq!(videos[1].uploaded, "2026-10-29");
    }

    #[test]
    fn later_pages_use_distinct_bounded_playlist_ranges() {
        assert_eq!(channel_page_args(1).unwrap(), ("1".into(), "200".into()));
        assert_eq!(
            channel_page_args(201).unwrap(),
            ("201".into(), "400".into())
        );
        assert_eq!(
            channel_page_args(801).unwrap(),
            ("801".into(), "1000".into())
        );
        for invalid in [0, 2, 200, 1_001, 1_201, usize::MAX] {
            assert!(channel_page_args(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn download_args_do_not_accept_urls_or_shell_commands_as_id() {
        let folder = Path::new("C:\\Downloads");
        let video = Video {
            id: "abcdefghijk".into(),
            title: "Safe".into(),
            duration: "1:00".into(),
            uploaded: String::new(),
        };
        let arguments = download_arguments(&video, Mode::Video, "720", folder).unwrap();
        assert!(arguments.join(" ").contains("%(height)sp"));
        assert_eq!(
            arguments.last().unwrap(),
            "https://www.youtube.com/watch?v=abcdefghijk"
        );
        assert!(arguments.join(" ").contains("height<=720"));
        assert!(download_arguments(&video, Mode::Audio, "best", folder)
            .unwrap()
            .contains(&"--extract-audio".to_string()));
        assert!(download_arguments(&video, Mode::Captions, "best", folder)
            .unwrap()
            .contains(&"--skip-download".to_string()));
        assert!(download_arguments(&video, Mode::Video, "--exec", folder).is_err());
        let mut invalid = video;
        invalid.id = "; del *".into();
        assert!(download_arguments(&invalid, Mode::Video, "best", folder).is_err());
    }

    #[test]
    fn requested_quality_gets_a_distinct_output_name_even_at_the_same_height() {
        let video = Video {
            id: "abcdefghijk".into(),
            title: "Test".into(),
            duration: "1:00".into(),
            uploaded: String::new(),
        };
        let mut templates = std::collections::HashSet::new();
        for (quality, label) in [
            ("best", "[best]"),
            ("1080", "[up-to-1080p]"),
            ("720", "[up-to-720p]"),
            ("360", "[up-to-360p]"),
        ] {
            let args = download_arguments(&video, Mode::Video, quality, Path::new("C:\\Downloads"))
                .unwrap();
            let template = args.windows(2).find(|items| items[0] == "-o").unwrap()[1].clone();
            assert!(template.contains(label));
            assert!(template.contains("[%(height)sp]"));
            assert!(templates.insert(template));
            assert!(args.contains(&"--no-overwrites".to_owned()));
            assert!(args.contains(&"after_move:__YCD_FILE__%(filepath)j".to_owned()));
            assert!(args.contains(&"--no-simulate".to_owned()));
        }
        let caption_args =
            download_arguments(&video, Mode::Captions, "best", Path::new("C:\\Downloads")).unwrap();
        assert!(caption_args
            .contains(&"after_video:__YCD_CAPTIONS__%(requested_subtitles)j".to_owned()));
    }

    #[test]
    fn a_successful_exit_requires_a_nonempty_output_in_the_chosen_folder() {
        let executable = std::env::current_exe().unwrap();
        let folder = executable.parent().unwrap();
        let quoted_path = serde_json::to_string(&executable.to_string_lossy()).unwrap();
        let file_result = format!("__YCD_FILE__{quoted_path}\n");
        assert!(validate_download_output(Mode::Video, folder, &file_result).is_ok());
        assert!(validate_download_output(Mode::Audio, folder, &file_result).is_ok());

        let missing_path = executable.with_extension("definitely-not-a-file");
        let missing = format!(
            "__YCD_FILE__{}",
            serde_json::to_string(&missing_path.to_string_lossy()).unwrap()
        );
        assert!(validate_download_output(Mode::Video, folder, &missing).is_err());
        assert!(validate_download_output(Mode::Video, folder, "").is_err());

        let captions = format!("__YCD_CAPTIONS__{{\"en\":{{\"filepath\":{quoted_path}}}}}");
        assert!(validate_download_output(Mode::Captions, folder, &captions).is_ok());
        assert!(validate_download_output(Mode::Captions, folder, "__YCD_CAPTIONS__NA").is_err());
        assert!(validate_download_output(Mode::Captions, folder, "__YCD_CAPTIONS__{}").is_err());
    }

    #[test]
    fn cancellation_before_launch_does_not_start_download() {
        let video = Video {
            id: "abcdefghijk".into(),
            title: "Cancelled".into(),
            duration: "1:00".into(),
            uploaded: String::new(),
        };
        let cancel = Arc::new(AtomicBool::new(true));
        let result = download_video(
            DownloadSpec {
                executable: Path::new("missing-yt-dlp.exe"),
                ffmpeg: Path::new("missing-ffmpeg.exe"),
                video: &video,
                mode: Mode::Video,
                quality: "best",
                folder: &std::env::temp_dir(),
            },
            cancel,
            |_, _| {},
        );
        assert_eq!(result.unwrap_err(), "Download cancelled.");
    }

    #[test]
    fn progress_reader_rejects_malformed_and_nonfinite_values() {
        assert_eq!(
            parse_percent("__YCD_PROGRESS__ 45.8%|2.5MiB/s|00:14")
                .unwrap()
                .0,
            46
        );
        assert!(parse_percent("__YCD_PROGRESS__NaN|n/a|n/a").is_none());
        assert!(parse_percent("__YCD_PROGRESS__not-a-number").is_none());
        assert!(parse_percent("other text").is_none());
    }

    #[cfg(windows)]
    #[test]
    fn subprocess_reader_collects_output_without_shell_interpolation() {
        let child = Command::new("cmd.exe")
            .args(["/C", "echo PROBE_OK"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let result = run_bounded(child, Duration::from_secs(5), None, |_, _| {}).unwrap();
        assert!(result.0);
        assert!(result.1.contains("PROBE_OK"));
    }

    #[cfg(windows)]
    #[test]
    fn subprocess_reader_reports_progress_from_stderr_and_stdout() {
        let child = Command::new("powershell.exe")
            .args([
                "-NoProfile", "-NonInteractive", "-Command",
                "[Console]::Error.WriteLine('__YCD_PROGRESS__41.6%|3MiB/s|00:09'); [Console]::Out.WriteLine('__YCD_PROGRESS__92%|4MiB/s|00:02'); [Console]::Out.WriteLine('PROBE_OK')",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let received = Arc::clone(&events);
        let (_, output) = run_bounded(child, Duration::from_secs(8), None, move |percent, _| {
            received.lock().unwrap().push(percent);
        })
        .unwrap();
        let recorded = events.lock().unwrap();
        assert!(recorded.contains(&42), "{recorded:?}");
        assert!(recorded.contains(&92), "{recorded:?}");
        assert_eq!(output.trim(), "PROBE_OK");
    }

    #[cfg(windows)]
    #[test]
    fn subprocess_reader_retains_the_last_error_after_long_stderr() {
        let child = Command::new("powershell.exe")
            .args([
                "-NoProfile", "-NonInteractive", "-Command",
                "[Console]::Error.WriteLine('noise' * 4000); [Console]::Error.WriteLine('ERROR: FINAL_FAILURE'); exit 1",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let error = run_bounded(child, Duration::from_secs(8), None, |_, _| {}).unwrap_err();
        assert_eq!(error.trim(), "ERROR: FINAL_FAILURE");
    }

    #[cfg(windows)]
    #[test]
    fn process_timeout_terminates_owned_child() {
        let child = Command::new("powershell.exe")
            .args(["-NoProfile", "-Command", "Start-Sleep -Seconds 5"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let began = Instant::now();
        let result = run_bounded(child, Duration::from_millis(200), None, |_, _| {});
        assert!(result.unwrap_err().contains("timed out"));
        assert!(began.elapsed() < Duration::from_secs(4));
    }

    #[cfg(windows)]
    #[test]
    fn process_cancellation_terminates_owned_child() {
        let child = Command::new("powershell.exe")
            .args(["-NoProfile", "-Command", "Start-Sleep -Seconds 5"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let began = Instant::now();
        let cancel = Arc::new(AtomicBool::new(true));
        let result = run_bounded(child, Duration::from_secs(8), Some(cancel), |_, _| {});
        assert_eq!(result.unwrap_err(), "Download cancelled.");
        assert!(began.elapsed() < Duration::from_secs(4));
    }

    #[cfg(windows)]
    #[test]
    fn process_cancellation_after_launch_interrupts_wait() {
        let child = Command::new("powershell.exe")
            .args(["-NoProfile", "-Command", "Start-Sleep -Seconds 5"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let signal = Arc::clone(&cancel);
        let setter = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(70));
            signal.store(true, Ordering::Relaxed);
        });
        let began = Instant::now();
        let result = run_bounded(child, Duration::from_secs(8), Some(cancel), |_, _| {});
        setter.join().unwrap();
        assert_eq!(result.unwrap_err(), "Download cancelled.");
        assert!(began.elapsed() < Duration::from_secs(4));
    }
}
