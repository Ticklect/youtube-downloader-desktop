//! Bounded, cached thumbnail fetches for *validated* public YouTube video IDs.
//! The downloader engine remains yt-dlp; thumbnails are purely presentation.
use crate::engine::{validate_id, Video};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

const MIN_IMAGE_BYTES: u64 = 201;
const MAX_IMAGE_BYTES: u64 = 350_000;

fn valid_image(path: &Path) -> bool {
    std::fs::metadata(path)
        .is_ok_and(|m| m.is_file() && (MIN_IMAGE_BYTES..=MAX_IMAGE_BYTES).contains(&m.len()))
}

// URLMon may successfully save an HTML error response. Check the fixed JPEG
// endpoint's signature only when publishing a new download, keeping frequent
// cached() lookups limited to one metadata operation.
fn jpeg_header(path: &Path) -> bool {
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut magic = [0; 3];
    file.read_exact(&mut magic).is_ok() && magic == [0xff, 0xd8, 0xff]
}

// A failed or partial download must not leave the cache permanently broken.
// URLMon writes a temporary file; only a verified result is published.
fn publish_image(tmp: &Path, path: &Path) -> bool {
    if !valid_image(tmp) || !jpeg_header(tmp) {
        let _ = std::fs::remove_file(tmp);
        return false;
    }
    if valid_image(path) && jpeg_header(path) {
        // Another process may have populated the cache in the meantime.
        let _ = std::fs::remove_file(tmp);
        return true;
    }
    if path.exists() && std::fs::remove_file(path).is_err() {
        let _ = std::fs::remove_file(tmp);
        return false;
    }
    if std::fs::rename(tmp, path).is_err() {
        let _ = std::fs::remove_file(tmp);
        return false;
    }
    true
}

struct FetchSlot {
    completed: Mutex<bool>,
    wake: Condvar,
}

fn active_fetches() -> &'static Mutex<HashMap<String, Arc<FetchSlot>>> {
    static ACTIVE: OnceLock<Mutex<HashMap<String, Arc<FetchSlot>>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(HashMap::new()))
}

// One owner fetches an ID; concurrent callers wait for that attempt's result.
// The guard also wakes waiters if the downloader panics.
struct FetchOwner<'a> {
    id: &'a str,
    slot: Arc<FetchSlot>,
}

impl Drop for FetchOwner<'_> {
    fn drop(&mut self) {
        *self
            .slot
            .completed
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = true;
        self.slot.wake.notify_all();
        active_fetches()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(self.id);
    }
}

pub fn cache_dir() -> PathBuf {
    crate::store::session_path()
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("thumbnails")
}

pub fn cache_path(video_id: &str) -> Option<PathBuf> {
    validate_id(video_id).then(|| cache_dir().join(format!("{video_id}.jpg")))
}

pub fn cached(video_id: &str) -> Option<PathBuf> {
    let path = cache_path(video_id)?;
    valid_image(&path).then_some(path)
}

#[cfg(windows)]
fn download(video_id: &str, path: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "urlmon")]
    extern "system" {
        fn URLDownloadToFileW(
            caller: *mut std::ffi::c_void,
            url: *const u16,
            file_name: *const u16,
            reserved: u32,
            callback: *mut std::ffi::c_void,
        ) -> i32;
    }
    // Only the fixed HTTPS image host and a previously validated 11-byte ID.
    let remote = format!("https://i.ytimg.com/vi/{video_id}/mqdefault.jpg");
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    let url = std::ffi::OsStr::new(&remote)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<u16>>();
    let destination = tmp
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<u16>>();
    // SAFETY: parameters are valid null-terminated UTF-16 strings and URLMon
    // writes only the explicitly chosen cache file. No callback is supplied.
    let result = unsafe {
        URLDownloadToFileW(
            std::ptr::null_mut(),
            url.as_ptr(),
            destination.as_ptr(),
            0,
            std::ptr::null_mut(),
        )
    };
    if result != 0 {
        let _ = std::fs::remove_file(&tmp);
        return false;
    }
    publish_image(&tmp, path)
}

#[cfg(not(windows))]
fn download(_video_id: &str, _path: &Path) -> bool {
    false
}

fn fetch_with_status(
    video_id: &str,
    downloader: impl FnOnce(&str, &Path) -> bool,
) -> Option<(PathBuf, bool)> {
    let path = cache_path(video_id)?;
    fetch_at_path(video_id, path, downloader)
}

fn fetch_at_path(
    video_id: &str,
    path: PathBuf,
    downloader: impl FnOnce(&str, &Path) -> bool,
) -> Option<(PathBuf, bool)> {
    if valid_image(&path) {
        return Some((path, false));
    }
    let (slot, owner) = {
        let mut active = active_fetches().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(slot) = active.get(video_id) {
            (Arc::clone(slot), false)
        } else {
            let slot = Arc::new(FetchSlot {
                completed: Mutex::new(false),
                wake: Condvar::new(),
            });
            active.insert(video_id.to_owned(), Arc::clone(&slot));
            (slot, true)
        }
    };

    if !owner {
        let mut completed = slot.completed.lock().unwrap_or_else(|e| e.into_inner());
        while !*completed {
            completed = slot.wake.wait(completed).unwrap_or_else(|e| e.into_inner());
        }
        return valid_image(&path).then_some((path, false));
    }
    let _owner = FetchOwner { id: video_id, slot };
    // A different caller could have completed while we registered our slot.
    if valid_image(&path) {
        return Some((path, false));
    }
    std::fs::create_dir_all(path.parent()?).ok()?;
    (downloader(video_id, &path) && valid_image(&path)).then_some((path, true))
}

/// Small groups provide progressive thumbnails without flooding the network.
pub fn update_in_background(
    videos: Vec<Video>,
    update: impl Fn(Vec<String>) + Send + Sync + 'static,
) {
    std::thread::spawn(move || {
        update_batches(
            &videos,
            |id| fetch_with_status(id, download).is_some_and(|(_, new)| new),
            update,
        );
    });
}

fn update_batches(
    videos: &[Video],
    mut fetch_new: impl FnMut(&str) -> bool,
    mut update: impl FnMut(Vec<String>),
) {
    let mut changed = Vec::with_capacity(6);
    for video in videos.iter().take(200) {
        if fetch_new(&video.id) {
            changed.push(video.id.clone());
            if changed.len() == 6 {
                update(std::mem::take(&mut changed));
            }
        }
    }
    if !changed.is_empty() {
        update(changed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Barrier;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn fixture_dir() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "thumbnail-unit-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn thumbnail_paths_are_bound_to_validated_ids() {
        let path = cache_path("abcdefghijk").unwrap();
        assert_eq!(path.extension().and_then(|x| x.to_str()), Some("jpg"));
        assert!(cache_path("../escape").is_none());
        assert!(cache_path("url?bad.png").is_none());
    }

    #[test]
    fn corrupt_or_incomplete_cache_is_replaced_by_a_verified_download() {
        let root = fixture_dir();
        let target = root.join("thumbnail.jpg");
        let tmp = root.join("thumbnail.tmp");
        std::fs::write(&target, vec![2; 100]).unwrap();
        let mut jpeg = vec![3; 1_024];
        jpeg[..3].copy_from_slice(&[0xff, 0xd8, 0xff]);
        std::fs::write(&tmp, &jpeg).unwrap();
        assert!(publish_image(&tmp, &target));
        assert_eq!(std::fs::read(&target).unwrap(), jpeg);
        assert!(!tmp.exists());

        // A failed download must not replace a healthy cached image.
        std::fs::write(&tmp, vec![9; 200]).unwrap();
        assert!(!publish_image(&tmp, &target));
        assert_eq!(std::fs::read(&target).unwrap(), jpeg);
        assert!(!tmp.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn html_response_cannot_poison_thumbnail_cache() {
        let root = fixture_dir();
        let target = root.join("thumbnail.jpg");
        let tmp = root.join("thumbnail.tmp");
        let html = format!("<html>{}</html>", "x".repeat(1_024));
        std::fs::write(&tmp, &html).unwrap();
        assert!(!publish_image(&tmp, &target));
        assert!(!tmp.exists());
        assert!(!target.exists());

        // A later legitimate fetch should replace an existing poisoned file.
        std::fs::write(&target, html).unwrap();
        let mut jpeg = vec![7; 2_048];
        jpeg[..3].copy_from_slice(&[0xff, 0xd8, 0xff]);
        std::fs::write(&tmp, &jpeg).unwrap();
        assert!(publish_image(&tmp, &target));
        assert_eq!(std::fs::read(&target).unwrap(), jpeg);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn image_bounds_reject_directories_and_invalid_sizes() {
        let root = fixture_dir();
        let path = root.join("thumbnail.jpg");
        assert!(!valid_image(&root));
        for (size, valid) in [(200, false), (201, true), (350_000, true), (350_001, false)] {
            std::fs::write(&path, vec![0; size]).unwrap();
            assert_eq!(valid_image(&path), valid, "size={size}");
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_requests_for_same_id_share_one_download() {
        let root = fixture_dir();
        let target = root.join("racedupe001.jpg");
        let attempts = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(Barrier::new(9));
        let mut threads = Vec::new();
        for _ in 0..8 {
            let path = target.clone();
            let attempts = Arc::clone(&attempts);
            let start = Arc::clone(&start);
            threads.push(std::thread::spawn(move || {
                start.wait();
                fetch_at_path("racedupe001", path, |_, path| {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(30));
                    std::fs::write(path, vec![7; 2_048]).is_ok()
                })
            }));
        }
        start.wait();
        let outcomes: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert!(outcomes.iter().all(Option::is_some));
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| outcome.as_ref().unwrap().1)
                .count(),
            1
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert!(valid_image(&target));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn background_updates_only_contain_new_ids_in_batches_of_six() {
        let videos: Vec<_> = (0..15)
            .map(|i| Video {
                id: format!("{i:011}"),
                title: String::new(),
                duration: String::new(),
                uploaded: String::new(),
            })
            .collect();
        let mut batches = Vec::new();
        update_batches(
            &videos,
            |id| id != "00000000002" && id != "00000000008",
            |ids| batches.push(ids),
        );
        assert_eq!(
            batches.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![6, 6, 1]
        );
        let expected: Vec<_> = videos
            .iter()
            .filter(|video| video.id != "00000000002" && video.id != "00000000008")
            .map(|video| video.id.clone())
            .collect();
        assert_eq!(batches.into_iter().flatten().collect::<Vec<_>>(), expected);
    }
}
