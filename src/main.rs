#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod engine;
mod progress;
mod recommendations;
mod runtime;
#[cfg(windows)]
mod single_instance;
mod store;
mod thumbnails;
#[cfg(test)]
mod ui_event_benchmark;

use engine::{Mode, Video};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};

slint::include_modules!();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JobState {
    Queued,
    Active,
    Completed,
    Failed,
    Cancelled,
}

struct Job {
    video: Video,
    mode: Mode,
    quality: String,
    folder: PathBuf,
    status: JobState,
    percent: i32,
    detail: String,
    cancel: Arc<AtomicBool>,
}

// A request for the same video in a different destination is a distinct job.
// Only identical in-flight destinations are duplicates.
fn is_pending_duplicate(
    job: &Job,
    video: &Video,
    mode: Mode,
    quality: &str,
    folder: &PathBuf,
) -> bool {
    job.video.id == video.id
        && job.mode == mode
        && job.quality == quality
        && job.folder == *folder
        && matches!(job.status, JobState::Queued | JobState::Active)
}

fn unqueued_videos(
    jobs: &[Job],
    selected: Vec<Video>,
    mode: Mode,
    quality: &str,
    folder: &PathBuf,
) -> Vec<Video> {
    // For a handful of selected videos the allocation/hash setup would cost
    // more than a short linear scan. Keep the fast path for small queues.
    if jobs.len().saturating_mul(selected.len()) <= 25_000 {
        return selected
            .into_iter()
            .filter(|video| {
                !jobs
                    .iter()
                    .any(|job| is_pending_duplicate(job, video, mode, quality, folder))
            })
            .collect();
    }
    // Only in-flight jobs with the same destination/format can be duplicates.
    // Index them once: selecting 1,000 videos beside 1,000 history entries
    // previously performed up to a million linear comparisons on the UI thread.
    let pending_ids: std::collections::HashSet<&str> = jobs
        .iter()
        .filter(|job| {
            job.mode == mode
                && job.quality == quality
                && job.folder == *folder
                && matches!(job.status, JobState::Queued | JobState::Active)
        })
        .map(|job| job.video.id.as_str())
        .collect();
    selected
        .into_iter()
        .filter(|video| !pending_ids.contains(video.id.as_str()))
        .collect()
}

fn begin_channel_request(generation: &AtomicU64) -> u64 {
    generation.fetch_add(1, Ordering::SeqCst) + 1
}

#[derive(Default)]
struct State {
    theme: String,
    videos: Vec<(Video, bool)>,
    jobs: Vec<Job>,
    active: usize,
    channel_name: String,
    channel_url: String,
    channel_history: Vec<store::SavedChannelHistory>,
    saved_channels: Vec<store::SavedChannel>,
    folder: PathBuf,
}

impl State {
    fn snapshot(&self) -> store::SavedState {
        store::SavedState {
            schema: store::SCHEMA_VERSION,
            theme: self.theme.clone(),
            channel_name: self.channel_name.clone(),
            channel_url: self.channel_url.clone(),
            channel_history: self.channel_history.clone(),
            saved_channels: self.saved_channels.clone(),
            folder: self.folder.clone(),
            videos: self
                .videos
                .iter()
                .map(|(video, selected)| store::SavedVideo {
                    video: video.clone(),
                    selected: *selected,
                })
                .collect(),
            jobs: self
                .jobs
                .iter()
                .map(|job| {
                    // The worker remains Active until yt-dlp actually stops,
                    // but a requested cancellation must survive an app exit.
                    // Otherwise load() resumes it as a queued download.
                    let cancelling =
                        job.status == JobState::Active && job.cancel.load(Ordering::Relaxed);
                    store::SavedJob {
                        video: job.video.clone(),
                        mode: job.mode,
                        quality: job.quality.clone(),
                        folder: job.folder.clone(),
                        state: match job.status {
                            JobState::Queued => store::SavedJobStatus::Queued,
                            JobState::Active if cancelling => store::SavedJobStatus::Cancelled,
                            JobState::Active => store::SavedJobStatus::Active,
                            JobState::Completed => store::SavedJobStatus::Completed,
                            JobState::Failed => store::SavedJobStatus::Failed,
                            JobState::Cancelled => store::SavedJobStatus::Cancelled,
                        },
                        detail: if cancelling {
                            "Cancelled by user.".to_owned()
                        } else {
                            job.detail.chars().take(300).collect()
                        },
                    }
                })
                .collect(),
        }
    }

    fn persist_checked(&self) -> Result<(), String> {
        store::save(&store::session_path(), &self.snapshot())
    }

    fn persist(&self) {
        if let Err(error) = self.persist_checked() {
            eprintln!("Unable to persist queue: {error}");
        }
    }
}

// Centralize all successful-load identity changes. yt-dlp controls its raw
// metadata; the same validated display name must reach the UI, pinned bar,
// current channel and persisted history.
fn apply_loaded_channel_identity(state: &mut State, canonical: &str, raw_name: &str) -> String {
    let name = recommendations::clean_name(raw_name);
    recommendations::record_success(&mut state.channel_history, &name, canonical);
    for saved in &mut state.saved_channels {
        if saved.url == canonical {
            saved.name = name.clone();
        }
    }
    state.channel_name = name.clone();
    state.channel_url = canonical.to_owned();
    name
}

fn restore_state() -> (State, Option<String>) {
    match store::load(&store::session_path()) {
        Ok(Some(mut saved)) => {
            // Upgrade old session data from the last successfully loaded
            // library. Never infer channels from a free-text search or failed
            // load: this migration needs both a saved name and real videos.
            if saved.channel_history.is_empty()
                && !saved.channel_name.is_empty()
                && !saved.channel_url.is_empty()
                && !saved.videos.is_empty()
            {
                recommendations::record_success(
                    &mut saved.channel_history,
                    &saved.channel_name,
                    &saved.channel_url,
                );
                if let Err(error) = store::save(&store::session_path(), &saved) {
                    eprintln!("Unable to migrate previous channel visit: {error}");
                }
            }
            let had_previous_content = !saved.videos.is_empty() || !saved.jobs.is_empty();
            let jobs = saved
                .jobs
                .into_iter()
                .map(|job| {
                    let status = match job.state {
                        store::SavedJobStatus::Queued | store::SavedJobStatus::Active => {
                            JobState::Queued
                        }
                        store::SavedJobStatus::Completed => JobState::Completed,
                        store::SavedJobStatus::Failed => JobState::Failed,
                        store::SavedJobStatus::Cancelled => JobState::Cancelled,
                    };
                    Job {
                        video: job.video,
                        mode: job.mode,
                        quality: job.quality,
                        folder: job.folder,
                        status,
                        percent: if status == JobState::Completed {
                            100
                        } else {
                            0
                        },
                        detail: job.detail,
                        cancel: Arc::new(AtomicBool::new(false)),
                    }
                })
                .collect::<Vec<_>>();
            let pending = jobs
                .iter()
                .filter(|job| job.status == JobState::Queued)
                .count();
            (
                State {
                    theme: saved.theme,
                    videos: saved
                        .videos
                        .into_iter()
                        .map(|item| (item.video, item.selected))
                        .collect(),
                    jobs,
                    active: 0,
                    channel_name: saved.channel_name,
                    channel_url: saved.channel_url,
                    channel_history: saved.channel_history,
                    saved_channels: saved.saved_channels,
                    folder: saved.folder,
                },
                if pending > 0 {
                    Some(format!(
                        "Resuming {pending} queued downloads from the saved session."
                    ))
                } else if had_previous_content {
                    Some("Saved library and download history restored.".into())
                } else {
                    None
                },
            )
        }
        Ok(None) => (
            State {
                theme: "red".into(),
                folder: default_folder(),
                ..State::default()
            },
            None,
        ),
        Err(error) => (
            State {
                theme: "red".into(),
                folder: default_folder(),
                ..State::default()
            },
            Some(format!("Saved queue could not be recovered: {error}")),
        ),
    }
}

fn reconcile_videos(
    previous: &[(Video, bool)],
    incoming: Vec<Video>,
    same_channel: bool,
) -> Vec<(Video, bool)> {
    if !same_channel {
        return incoming.into_iter().map(|video| (video, false)).collect();
    }
    // The first 200 refreshed videos may need to preserve selections from a
    // 1,000-video saved library. Avoid rescanning the library for every ID.
    // Keep the linear path for tiny lists where building a set costs more.
    if previous.len().saturating_mul(incoming.len()) > 2_000 {
        let selected_ids: std::collections::HashSet<&str> = previous
            .iter()
            .filter(|(_, selected)| *selected)
            .map(|(video, _)| video.id.as_str())
            .collect();
        return incoming
            .into_iter()
            .map(|video| {
                let retained = selected_ids.contains(video.id.as_str());
                (video, retained)
            })
            .collect();
    }
    incoming
        .into_iter()
        .map(|video| {
            let retained = previous
                .iter()
                .any(|(old, selected)| old.id == video.id && *selected);
            (video, retained)
        })
        .collect()
}

/// Append a later playlist page without losing a selection made while it was
/// loading, and without duplicating entries returned by yt-dlp.
fn append_channel_page(existing: &mut Vec<(Video, bool)>, page: Vec<Video>) -> usize {
    let mut seen: std::collections::HashSet<String> =
        existing.iter().map(|(video, _)| video.id.clone()).collect();
    let original_len = existing.len();
    for video in page {
        if existing.len() >= engine::MAX_CHANNEL_VIDEOS {
            break;
        }
        if seen.insert(video.id.clone()) {
            existing.push((video, false));
        }
    }
    existing.len() - original_len
}

#[derive(Default)]
struct ChannelPages {
    next_start: usize,
    available: bool,
    fetching: bool,
}

impl ChannelPages {
    fn reset(&mut self, loaded: usize) {
        // Duplicated IDs can leave a page with fewer than 200 saved rows.
        // Continue from the next playlist page, never repeat earlier items.
        self.next_start = if loaded == 0 {
            1
        } else {
            ((loaded - 1) / engine::CHANNEL_PAGE_SIZE + 1) * engine::CHANNEL_PAGE_SIZE + 1
        };
        self.available =
            loaded >= engine::CHANNEL_PAGE_SIZE && self.next_start <= engine::MAX_CHANNEL_VIDEOS;
        self.fetching = false;
    }

    fn after_page(&mut self, page_len: usize, total: usize) {
        self.next_start += engine::CHANNEL_PAGE_SIZE;
        self.fetching = false;
        self.available = page_len == engine::CHANNEL_PAGE_SIZE
            && total < engine::MAX_CHANNEL_VIDEOS
            && self.next_start <= engine::MAX_CHANNEL_VIDEOS;
    }
}

fn set_visible_selection(items: &mut [(Video, bool)], search: &str, checked: bool) {
    let term = search.trim().to_lowercase();
    for (video, selected) in items {
        if video.title.to_lowercase().contains(&term) {
            *selected = checked;
        }
    }
}

fn visible_selected_count(items: &[(Video, bool)], search: &str) -> usize {
    let term = search.trim().to_lowercase();
    items
        .iter()
        .filter(|(video, selected)| *selected && video.title.to_lowercase().contains(&term))
        .count()
}

fn publish_channels(ui: &MainWindow, state: &State) {
    let saved = state
        .saved_channels
        .iter()
        .map(|entry| ChannelRow {
            name: entry.name.clone().into(),
            url: entry.url.clone().into(),
        })
        .collect::<Vec<_>>();
    let suggestions = recommendations::suggested(&state.channel_history, &state.saved_channels)
        .into_iter()
        .map(|entry| ChannelRow {
            name: entry.name.into(),
            url: entry.url.into(),
        })
        .collect::<Vec<_>>();
    ui.set_saved_channels(ModelRc::from(Rc::new(VecModel::from(saved))));
    ui.set_recommended_channels(ModelRc::from(Rc::new(VecModel::from(suggestions))));
}

fn publish_video_selection_counts(ui: &MainWindow, state: &State) {
    let filter = ui.get_video_search().to_string().trim().to_lowercase();
    let visible_selected = visible_selected_count(&state.videos, &filter);
    ui.set_visible_selected_items(visible_selected as i32);
    let selected = state
        .videos
        .iter()
        .filter(|(_, selected)| *selected)
        .count();
    ui.set_selected_items(selected as i32);
    ui.set_selected_count(
        format!(
            "{selected} video{} selected",
            if selected == 1 { "" } else { "s" }
        )
        .into(),
    );
}

fn publish_videos(ui: &MainWindow, state: &State) {
    publish_filtered_videos(ui, state);
    publish_selected_videos(ui, state);
}

fn existing_video_thumbnails(
    model: &ModelRc<VideoRow>,
) -> std::collections::HashMap<String, slint::Image> {
    (0..model.row_count())
        .filter_map(|index| model.row_data(index))
        .map(|row| (row.id.to_string(), row.thumbnail))
        .collect()
}

fn thumbnail_for_video(
    id: &str,
    previous: &std::collections::HashMap<String, slint::Image>,
) -> slint::Image {
    previous.get(id).cloned().unwrap_or_else(|| {
        thumbnails::cached(id)
            .and_then(|path| slint::Image::load_from_path(&path).ok())
            .unwrap_or_default()
    })
}

fn video_row_unchanged(existing: &VideoRow, refreshed: &VideoRow) -> bool {
    existing.id == refreshed.id
        && existing.title == refreshed.title
        && existing.title_long == refreshed.title_long
        && existing.detail == refreshed.detail
        && existing.secondary == refreshed.secondary
        && existing.selected == refreshed.selected
    // The thumbnail comes from the existing row or the background changed-ID
    // callback. Re-publishing identical metadata must not discard an image
    // or notify the repeater about a redundant row mutation.
}

fn publish_filtered_videos(ui: &MainWindow, state: &State) {
    publish_video_selection_counts(ui, state);
    let filter = ui.get_video_search().to_string().trim().to_lowercase();
    // Slint has a small decoded-image cache. Reopening the same image paths on
    // every keystroke is unnecessary when the image already lives in a row.
    // The asynchronous fetch callback updates these live rows when new IDs
    // become available; images for newly revealed rows load from disk once.
    let previous = ui.get_videos();
    let previous_images = existing_video_thumbnails(&previous);
    let visible_rows = state
        .videos
        .iter()
        .filter(|(video, _)| video.title.to_lowercase().contains(&filter))
        .map(|(video, selected)| VideoRow {
            id: video.id.clone().into(),
            title: video.title.clone().into(),
            title_long: video.title.chars().count() > 55,
            detail: video.duration.clone().into(),
            secondary: if video.uploaded.is_empty() {
                state.channel_name.clone()
            } else {
                format!("Published {}", video.uploaded)
            }
            .into(),
            thumbnail: thumbnail_for_video(&video.id, &previous_images),
            selected: *selected,
        })
        .collect::<Vec<_>>();
    // Preserve the live Slint repeater and focused row during selection
    // changes. Replacing the entire model on each toggle dropped keyboard
    // focus, so Space could not toggle the same video twice in succession.
    let previous_len = previous.row_count();
    let prefix_identity = previous_len > 0
        && previous_len <= visible_rows.len()
        && visible_rows
            .iter()
            .take(previous_len)
            .enumerate()
            .all(|(index, row)| {
                previous
                    .row_data(index)
                    .is_some_and(|existing| existing.id == row.id)
            });
    if prefix_identity {
        for (index, row) in visible_rows.iter().take(previous_len).cloned().enumerate() {
            if !previous
                .row_data(index)
                .is_some_and(|existing| video_row_unchanged(&existing, &row))
            {
                previous.set_row_data(index, row);
            }
        }
        for row in visible_rows.into_iter().skip(previous_len) {
            // The view is a VecModel. Appending rows retains the scroll offset,
            // unlike replacing the repeater model at every loaded page.
            previous
                .push_row(row)
                .expect("video model supports appending rows");
        }
    } else {
        ui.set_videos(ModelRc::from(Rc::new(VecModel::from(visible_rows))));
    }
}

fn publish_selected_videos(ui: &MainWindow, state: &State) {
    // The review screen must show the complete selection, even when Library
    // search filters the visible list. Otherwise saved selections look lost.
    let mut previous_images = existing_video_thumbnails(&ui.get_selected_videos());
    previous_images.extend(existing_video_thumbnails(&ui.get_videos()));
    ui.set_selected_videos(ModelRc::from(Rc::new(VecModel::from(
        state
            .videos
            .iter()
            .filter(|(_, selected)| *selected)
            .map(|(video, selected)| VideoRow {
                id: video.id.clone().into(),
                title: video.title.clone().into(),
                title_long: video.title.chars().count() > 55,
                detail: video.duration.clone().into(),
                secondary: if video.uploaded.is_empty() {
                    state.channel_name.clone()
                } else {
                    format!("Published {}", video.uploaded)
                }
                .into(),
                thumbnail: thumbnail_for_video(&video.id, &previous_images),
                selected: *selected,
            })
            .collect::<Vec<_>>(),
    ))));
}

/// Update a checkbox in the existing Slint model without replacing its
/// ModelRc or recalculating thumbnail images. That keeps the focused repeater
/// row alive during Space/Return interaction and preserves the current filter.
fn set_video_row_selection(model: &ModelRc<VideoRow>, id: &str, selected: bool) -> bool {
    for index in 0..model.row_count() {
        let Some(mut row) = model.row_data(index) else {
            continue;
        };
        if row.id.as_str() != id {
            continue;
        }
        if row.selected != selected {
            row.selected = selected;
            model.set_row_data(index, row);
        }
        return true;
    }
    false
}

fn publish_video_selection_change(ui: &MainWindow, state: &State, id: &str, selected: bool) {
    // A checkbox changes its selection only: rebuilding the entire Library
    // model here can invalidate the Slint accessibility focus target.
    set_video_row_selection(&ui.get_videos(), id, selected);
    publish_video_selection_counts(ui, state);
    publish_selected_videos(ui, state);
}

fn update_thumbnails(ui: &MainWindow, state: &Arc<Mutex<State>>) {
    let videos = {
        let value = state.lock().unwrap();
        value
            .videos
            .iter()
            .map(|(v, _)| v.clone())
            .collect::<Vec<_>>()
    };
    update_page_thumbnails(ui, videos);
}

/// A batch of fetched thumbnails changes only the corresponding image cells.
/// Rebuilding every video row on each batch repeatedly decoded all cached
/// images and recreated the selected-videos model, disturbing keyboard focus.
fn apply_thumbnail_images(
    models: &[ModelRc<VideoRow>; 2],
    images: &std::collections::HashMap<String, slint::Image>,
) {
    if images.is_empty() {
        return;
    }
    for model in models {
        for row_index in 0..model.row_count() {
            let Some(mut row) = model.row_data(row_index) else {
                continue;
            };
            if let Some(image) = images.get(row.id.as_str()) {
                row.thumbnail = image.clone();
                model.set_row_data(row_index, row);
            }
        }
    }
}

fn refresh_video_thumbnails(ui: &MainWindow, changed_ids: Vec<String>) {
    let images = changed_ids
        .into_iter()
        .filter_map(|id| {
            let path = thumbnails::cached(&id)?;
            let image = slint::Image::load_from_path(&path).ok()?;
            Some((id, image))
        })
        .collect::<std::collections::HashMap<_, _>>();
    let videos = ui.get_videos();
    let selected = ui.get_selected_videos();
    apply_thumbnail_images(&[videos, selected], &images);
}

fn update_page_thumbnails(ui: &MainWindow, videos: Vec<Video>) {
    let weak_ui = ui.as_weak();
    thumbnails::update_in_background(videos, move |changed_ids| {
        let ui = weak_ui.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = ui.upgrade() {
                refresh_video_thumbnails(&ui, changed_ids);
            }
        });
    });
}

fn task_row_for_job(index: usize, job: &Job) -> TaskRow {
    TaskRow {
        job_index: index as i32,
        title: job.video.title.clone().into(),
        title_long: job.video.title.chars().count() > 40,
        thumbnail: thumbnails::cached(&job.video.id)
            .and_then(|path| slint::Image::load_from_path(&path).ok())
            .unwrap_or_default(),
        detail: format!("{}  ·  {}", job.detail, job.video.duration).into(),
        metadata: format!(
            "{}  ·  {}",
            match job.mode {
                Mode::Video => "Video",
                Mode::Audio => "MP3",
                Mode::Captions => "Captions",
            },
            if job.mode == Mode::Video {
                if job.quality == "best" {
                    "Best quality"
                } else {
                    &job.quality
                }
            } else {
                "Original"
            },
        )
        .into(),
        progress: job.percent,
        state: match job.status {
            JobState::Queued => "Queued",
            JobState::Active => "Downloading",
            JobState::Completed => "Completed",
            JobState::Failed => "Failed",
            JobState::Cancelled => "Cancelled",
        }
        .into(),
    }
}

fn publish_tasks(ui: &MainWindow, state: &State) {
    let filter = ui.get_queue_filter().to_string();
    ui.set_tasks(ModelRc::from(Rc::new(VecModel::from(
        state
            .jobs
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, job)| match filter.as_str() {
                "active" => matches!(job.status, JobState::Active | JobState::Queued),
                "completed" => job.status == JobState::Completed,
                "failed" => job.status == JobState::Failed,
                _ => true,
            })
            .map(|(index, job)| task_row_for_job(index, job))
            .collect::<Vec<_>>(),
    ))));
    publish_task_counters(ui, state);
}

fn publish_task_counters(ui: &MainWindow, state: &State) {
    let done = state
        .jobs
        .iter()
        .filter(|j| j.status == JobState::Completed)
        .count();
    let failed = state
        .jobs
        .iter()
        .filter(|j| j.status == JobState::Failed)
        .count();
    ui.set_failed_items(failed as i32);
    ui.set_pending_items(
        state
            .jobs
            .iter()
            .filter(|j| matches!(j.status, JobState::Active | JobState::Queued))
            .count() as i32,
    );
    ui.set_finished_items(
        state
            .jobs
            .iter()
            .filter(|j| {
                matches!(
                    j.status,
                    JobState::Completed | JobState::Failed | JobState::Cancelled
                )
            })
            .count() as i32,
    );
    ui.set_queue_count(
        format!(
            "{} total  •  {} active  •  {} completed  •  {} failed",
            state.jobs.len(),
            state.active,
            done,
            failed
        )
        .into(),
    );
}

/// Update the lifecycle of a stable job in the existing newest-first model.
/// For filtered queues a status transition may add/remove a row, so callers
/// must rebuild the filtered model instead.
fn set_task_row_lifecycle(model: &ModelRc<TaskRow>, index: usize, job: &Job) -> bool {
    let mut start = 0;
    let mut end = model.row_count();
    while start < end {
        let middle = start + (end - start) / 2;
        let Some(mut row) = model.row_data(middle) else {
            return false;
        };
        match row.job_index.cmp(&(index as i32)) {
            std::cmp::Ordering::Greater => start = middle + 1,
            std::cmp::Ordering::Less => end = middle,
            std::cmp::Ordering::Equal => {
                row.progress = job.percent;
                row.detail = format!("{}  ·  {}", job.detail, job.video.duration).into();
                row.state = match job.status {
                    JobState::Queued => "Queued",
                    JobState::Active => "Downloading",
                    JobState::Completed => "Completed",
                    JobState::Failed => "Failed",
                    JobState::Cancelled => "Cancelled",
                }
                .into();
                model.set_row_data(middle, row);
                return true;
            }
        }
    }
    false
}

/// Locate an existing row or its insertion point in the newest-first model.
fn task_row_position(model: &ModelRc<TaskRow>, index: usize) -> Option<(usize, bool)> {
    let mut start = 0;
    let mut end = model.row_count();
    while start < end {
        let middle = start + (end - start) / 2;
        let row = model.row_data(middle)?;
        match row.job_index.cmp(&(index as i32)) {
            std::cmp::Ordering::Greater => start = middle + 1,
            std::cmp::Ordering::Less => end = middle,
            std::cmp::Ordering::Equal => return Some((middle, true)),
        }
    }
    Some((start, false))
}

fn task_status_matches_filter(filter: &str, status: JobState) -> bool {
    match filter {
        "active" => matches!(status, JobState::Active | JobState::Queued),
        "completed" => status == JobState::Completed,
        "failed" => status == JobState::Failed,
        _ => true,
    }
}

/// A completion changes at most one row's membership, and starting a queued
/// replacement changes only its state. Preserve the filtered VecModel and all
/// unaffected rows, falling back to a full rebuild on unexpected model state.
fn update_task_membership(
    model: &ModelRc<TaskRow>,
    index: usize,
    job: &Job,
    was_visible: bool,
    now_visible: bool,
) -> bool {
    let Some((position, exists)) = task_row_position(model, index) else {
        return false;
    };
    if exists != was_visible {
        return false;
    }
    match (exists, now_visible) {
        (true, true) => set_task_row_lifecycle(model, index, job),
        (true, false) => model.remove_row(position).is_ok(),
        (false, true) => model
            .insert_row(position, task_row_for_job(index, job))
            .is_ok(),
        (false, false) => true,
    }
}

/// Keep queue rows stable on routine completed-job transitions. Once filters
/// change membership, or a new job changes the queue length, rebuild once.
fn publish_scheduled_changes(
    ui: &MainWindow,
    state: &State,
    finished: usize,
    newly_active: &[usize],
) {
    let tasks = ui.get_tasks();
    let filter = ui.get_queue_filter();
    if !matches!(filter.as_str(), "all" | "active" | "completed" | "failed")
        || (filter.as_str() == "all" && tasks.row_count() != state.jobs.len())
    {
        publish_tasks(ui, state);
        return;
    }
    let was_active_visible = task_status_matches_filter(&filter, JobState::Active);
    let was_queued_visible = task_status_matches_filter(&filter, JobState::Queued);
    if !update_task_membership(
        &tasks,
        finished,
        &state.jobs[finished],
        was_active_visible,
        task_status_matches_filter(&filter, state.jobs[finished].status),
    ) || newly_active.iter().any(|&index| {
        !update_task_membership(
            &tasks,
            index,
            &state.jobs[index],
            was_queued_visible,
            task_status_matches_filter(&filter, state.jobs[index].status),
        )
    }) {
        publish_tasks(ui, state);
        return;
    }
    publish_task_counters(ui, state);
}

/// Progress changes only one task. Queue rows are always newest-first (see
/// publish_tasks), including after status filters. Binary search avoids
/// cloning hundreds of Slint row values for each progress update while
/// retaining the existing model, scroll position and accessibility focus.
fn set_task_row_progress(
    model: &ModelRc<TaskRow>,
    job_index: usize,
    percent: i32,
    detail: &str,
    duration: &str,
) -> bool {
    let mut start = 0;
    let mut end = model.row_count();
    while start < end {
        let middle = start + (end - start) / 2;
        let Some(mut row) = model.row_data(middle) else {
            return false;
        };
        match row.job_index.cmp(&(job_index as i32)) {
            std::cmp::Ordering::Greater => start = middle + 1,
            std::cmp::Ordering::Less => end = middle,
            std::cmp::Ordering::Equal => {
                row.progress = percent;
                row.detail = format!("{detail}  ·  {duration}").into();
                model.set_row_data(middle, row);
                return true;
            }
        }
    }
    false
}

fn cancel_job(job: &mut Job) -> bool {
    match job.status {
        JobState::Queued => {
            job.status = JobState::Cancelled;
            job.detail = "Cancelled before starting.".into();
            true
        }
        JobState::Active => {
            job.cancel.store(true, Ordering::Relaxed);
            job.detail = "Cancelling download...".into();
            true
        }
        _ => false,
    }
}

fn retry_job(job: &mut Job) -> bool {
    if job.status != JobState::Failed {
        return false;
    }
    job.status = JobState::Queued;
    job.detail = "Retry queued...".into();
    job.percent = 0;
    job.cancel = Arc::new(AtomicBool::new(false));
    true
}

/// Do not discard download history in memory unless its removal was committed
/// to the session file. Otherwise a denied write leaves the UI claiming the
/// history was cleared while the next startup restores all of it.
fn clear_finished_and_commit(state: &mut State, session: &Path) -> Result<usize, String> {
    if state.active > 0 || state.jobs.iter().any(|job| job.status == JobState::Queued) {
        return Err("Wait until the download queue is idle before clearing history.".into());
    }
    let mut snapshot = state.snapshot();
    snapshot.jobs.retain(|job| {
        matches!(
            job.state,
            store::SavedJobStatus::Queued | store::SavedJobStatus::Active
        )
    });
    store::save(session, &snapshot)?;
    let previous = state.jobs.len();
    state
        .jobs
        .retain(|job| matches!(job.status, JobState::Queued | JobState::Active));
    Ok(previous - state.jobs.len())
}

fn publish(ui: &MainWindow, state: &State) {
    publish_videos(ui, state);
    publish_tasks(ui, state);
}

fn schedule(ui: &MainWindow, state: &Arc<Mutex<State>>, yt_dlp: PathBuf, ffmpeg: PathBuf) {
    schedule_after_completion(ui, state, yt_dlp, ffmpeg, None);
}

type ScheduledJob = (usize, Video, Mode, String, PathBuf, Arc<AtomicBool>);

/// Reserve the next slots and durably record their Active states before any
/// subprocess launches. A failed save must not launch downloads that a restart
/// would not know about. Restore the in-memory reservations on failure.
fn reserve_and_commit_jobs(
    state: &mut State,
    session: &Path,
    max_parallel: usize,
) -> Result<Vec<ScheduledJob>, String> {
    let previous_active = state.active;
    let mut to_start = Vec::new();
    let mut previous_details = Vec::new();
    for index in 0..state.jobs.len() {
        if state.active >= max_parallel {
            break;
        }
        if state.jobs[index].status != JobState::Queued {
            continue;
        }
        previous_details.push((index, state.jobs[index].detail.clone()));
        state.jobs[index].status = JobState::Active;
        state.jobs[index].detail = "Preparing download…".into();
        state.active += 1;
        let job = &state.jobs[index];
        to_start.push((
            index,
            job.video.clone(),
            job.mode,
            job.quality.clone(),
            job.folder.clone(),
            job.cancel.clone(),
        ));
    }
    if let Err(error) = store::save(session, &state.snapshot()) {
        for (index, detail) in previous_details {
            state.jobs[index].status = JobState::Queued;
            state.jobs[index].detail = detail;
        }
        state.active = previous_active;
        return Err(error);
    }
    Ok(to_start)
}

fn schedule_after_completion(
    ui: &MainWindow,
    state: &Arc<Mutex<State>>,
    yt_dlp: PathBuf,
    ffmpeg: PathBuf,
    finished_index: Option<usize>,
) {
    let to_start = {
        let mut state = state.lock().unwrap();
        match reserve_and_commit_jobs(&mut state, &store::session_path(), engine::MAX_PARALLEL) {
            Ok(to_start) => {
                if let Some(finished) = finished_index {
                    let newly_active = to_start.iter().map(|item| item.0).collect::<Vec<_>>();
                    publish_scheduled_changes(ui, &state, finished, &newly_active);
                } else {
                    publish_tasks(ui, &state);
                }
                to_start
            }
            Err(error) => {
                publish_tasks(ui, &state);
                show_queue_error(
                    ui,
                    format!(
                        "Could not save download queue; no new downloads were started. {error}"
                    ),
                );
                return;
            }
        }
    };
    for (index, video, mode, quality, folder, cancel) in to_start {
        let weak = ui.as_weak();
        let state_for_progress = Arc::downgrade(state);
        let progress_mailbox = Arc::new(progress::ProgressMailbox::default());
        let yt_dlp = yt_dlp.clone();
        let ffmpeg = ffmpeg.clone();
        std::thread::spawn(move || {
            let progress_ui = weak.clone();
            let progress_state = state_for_progress.clone();
            let progress_mailbox_for_worker = progress_mailbox.clone();
            let result = engine::download_video(
                engine::DownloadSpec {
                    executable: &yt_dlp,
                    ffmpeg: &ffmpeg,
                    video: &video,
                    mode,
                    quality: &quality,
                    folder: &folder,
                },
                cancel.clone(),
                move |percent, detail| {
                    if !progress_mailbox_for_worker.publish(percent, detail) {
                        return;
                    }
                    let weak = progress_ui.clone();
                    let state = progress_state.clone();
                    let mailbox = progress_mailbox_for_worker.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        let Some((percent, detail)) = mailbox.consume() else {
                            return;
                        };
                        if let (Some(ui), Some(state)) = (weak.upgrade(), state.upgrade()) {
                            let mut value = state.lock().unwrap();
                            if let Some(job) = value.jobs.get_mut(index) {
                                if job.status != JobState::Active
                                    || job.cancel.load(Ordering::Relaxed)
                                {
                                    return;
                                }
                                job.percent = percent;
                                job.detail = detail;
                                // Queue filter can exclude this active job.
                                // On visible rows, update only the changed
                                // task; the counters and statuses are static.
                                if matches!(ui.get_queue_filter().as_str(), "all" | "active") {
                                    set_task_row_progress(
                                        &ui.get_tasks(),
                                        index,
                                        job.percent,
                                        &job.detail,
                                        &job.video.duration,
                                    );
                                }
                            }
                        }
                    });
                },
            );
            let _ = slint::invoke_from_event_loop(move || {
                let (Some(ui), Some(state)) = (weak.upgrade(), state_for_progress.upgrade()) else {
                    return;
                };
                {
                    let mut value = state.lock().unwrap();
                    if let Some(job) = value.jobs.get_mut(index) {
                        job.status = match &result {
                            Ok(()) => JobState::Completed,
                            Err(_) if cancel.load(Ordering::Relaxed) => JobState::Cancelled,
                            Err(_) => JobState::Failed,
                        };
                        job.percent = if job.status == JobState::Completed {
                            100
                        } else {
                            job.percent
                        };
                        job.detail = match result {
                            Ok(()) => "Saved successfully".to_owned(),
                            Err(message) => message,
                        };
                    }
                    value.active = value.active.saturating_sub(1);
                }
                schedule_after_completion(&ui, &state, yt_dlp, ffmpeg, Some(index));
            });
        });
    }
}

fn default_folder() -> PathBuf {
    let home = std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join("Downloads").join("YouTube Downloader")
}

fn show_error(ui: &MainWindow, text: impl Into<String>) {
    ui.set_status_text(text.into().into());
}

fn show_channel_error(ui: &MainWindow, text: impl Into<String>) {
    let message = text.into();
    ui.set_channel_error(message.clone().into());
    show_error(ui, message);
}

fn show_download_error(ui: &MainWindow, text: impl Into<String>) {
    let message = text.into();
    ui.set_download_error(message.clone().into());
    show_error(ui, message);
}

fn show_queue_error(ui: &MainWindow, text: impl Into<String>) {
    let message = text.into();
    ui.set_queue_error(message.clone().into());
    show_error(ui, message);
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().nth(1).as_deref() == Some("--verify-embedded-runtime") {
        runtime::verify_embedded_runtime().map_err(std::io::Error::other)?;
        return Ok(());
    }
    #[cfg(windows)]
    let _instance = if std::env::var("YTD_UI_SMOKE_PREVIEW").as_deref() == Ok("1") {
        // Synthetic screenshot sessions use a separate app-data directory and
        // must not interrupt a running user's desktop downloader.
        None
    } else {
        Some(single_instance::InstanceGuard::acquire()?)
    };
    let ui = MainWindow::new()?;
    // Provision and verify the embedded tools on first launch, while leaving
    // the established session/history location completely unchanged.
    let runtime_error = runtime::embedded_runtime_dir().err();
    let (loaded_state, restore_notice) = restore_state();
    let folder = loaded_state.folder.clone();
    ui.set_channel_url(loaded_state.channel_url.clone().into());
    if !loaded_state.channel_name.is_empty() {
        ui.set_channel_name(loaded_state.channel_name.clone().into());
    }
    let state = Arc::new(Mutex::new(loaded_state));
    publish_channels(&ui, &state.lock().unwrap());
    let pages = Arc::new(Mutex::new(ChannelPages::default()));
    {
        let mut paging = pages.lock().unwrap();
        paging.reset(state.lock().unwrap().videos.len());
        ui.set_more_available(paging.available);
    }
    ui.set_download_folder(folder.display().to_string().into());
    ui.set_theme(state.lock().unwrap().theme.clone().into());
    // Visual smoke tests override only the displayed palette, without
    // modifying the user's persisted preference.
    if let Ok(theme) = std::env::var("YTD_UI_SMOKE_THEME") {
        if matches!(theme.as_str(), "red" | "mono") {
            ui.set_theme(theme.into());
        }
    }
    if let Ok(page) = std::env::var("YTD_UI_SMOKE_PAGE") {
        if matches!(page.as_str(), "discover" | "setup" | "queue" | "legal") {
            ui.set_page(page.into());
        }
    }
    // Deterministic UI snapshots for error layouts. Only the explicitly
    // isolated smoke-preview process accepts these test-only values.
    if std::env::var("YTD_UI_SMOKE_PREVIEW").as_deref() == Ok("1") {
        match std::env::var("YTD_UI_SMOKE_ERROR").as_deref() {
            Ok("channel") => ui.set_channel_error(
                "Enter a valid YouTube channel URL, then try loading again.".into(),
            ),
            Ok("download") => ui.set_download_error(
                "Choose an accessible destination folder and try again.".into(),
            ),
            Ok("queue") => ui.set_queue_error("The download folder is no longer available.".into()),
            _ => {}
        }
    }
    ui.on_minimize_window({
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                ui.window().set_minimized(true);
            }
        }
    });
    ui.on_close_window({
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                let _ = ui.hide();
                let _ = slint::quit_event_loop();
            }
        }
    });
    ui.on_toggle_maximize({
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                let window = ui.window();
                let maximized = !window.is_maximized();
                window.set_fullscreen(false);
                window.set_maximized(maximized);
                ui.set_fullscreen_active(false);
                ui.set_maximized_active(maximized);
            }
        }
    });
    ui.on_toggle_fullscreen({
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                let window = ui.window();
                let fullscreen = !window.is_fullscreen();
                window.set_fullscreen(fullscreen);
                ui.set_fullscreen_active(fullscreen);
                ui.set_maximized_active(window.is_maximized());
            }
        }
    });
    ui.on_change_theme({
        let state = state.clone();
        let weak = ui.as_weak();
        move |theme| {
            let theme = theme.to_string();
            if !matches!(theme.as_str(), "red" | "mono") {
                return;
            }
            if let Some(ui) = weak.upgrade() {
                ui.set_theme(theme.clone().into());
                let mut value = state.lock().unwrap();
                value.theme = theme;
                value.persist();
            }
        }
    });

    match (
        engine::find_dependency("yt-dlp.exe"),
        engine::find_dependency("ffmpeg.exe"),
        engine::find_dependency("deno.exe"),
    ) {
        (Ok(_), Ok(_), Ok(_)) => {
            show_error(&ui, "Ready — bundled yt-dlp, FFmpeg and Deno detected.")
        }
        _ => show_error(
            &ui,
            "Dependencies not found. Install the complete package with its bin folder.",
        ),
    }
    if let Some(notice) = restore_notice {
        show_error(&ui, notice);
    }
    if let Some(error) = runtime_error {
        ui.set_status_text(format!("Bundled tools could not be prepared: {error}").into());
    }

    ui.on_add_recommended_channel({
        let weak = ui.as_weak();
        let state = state.clone();
        move |url| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let mut value = state.lock().unwrap();
            let history = value.channel_history.clone();
            if recommendations::add_saved(&history, &mut value.saved_channels, &url) {
                value.persist();
                publish_channels(&ui, &value);
            }
        }
    });
    ui.on_remove_saved_channel({
        let weak = ui.as_weak();
        let state = state.clone();
        move |url| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let mut value = state.lock().unwrap();
            if recommendations::remove_saved(&mut value.saved_channels, &url) {
                value.persist();
                publish_channels(&ui, &value);
            }
        }
    });
    ui.on_open_saved_channel({
        let weak = ui.as_weak();
        let state = state.clone();
        move |url| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let url = url.to_string();
            if ui.get_loading()
                || !state
                    .lock()
                    .unwrap()
                    .saved_channels
                    .iter()
                    .any(|saved| saved.url == url)
            {
                return;
            }
            ui.set_page("discover".into());
            ui.set_channel_url(url.into());
            ui.invoke_load_channel();
        }
    });

    let channel_request_generation = Arc::new(AtomicU64::new(0));
    ui.on_load_channel({
        let weak = ui.as_weak();
        let state = state.clone();
        let pages = pages.clone();
        let channel_request_generation = channel_request_generation.clone();
        move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            // Even an invalid newer attempt supersedes an earlier in-flight
            // request. Otherwise the older response can replace its error.
            let generation = begin_channel_request(&channel_request_generation);
            {
                let mut paging = pages.lock().unwrap();
                paging.reset(state.lock().unwrap().videos.len());
                ui.set_more_available(false);
                ui.set_loading_more(false);
                ui.set_more_error("".into());
            }
            let input = ui.get_channel_url().to_string();
            let canonical = match engine::channel_url(&input) {
                Ok(url) => url,
                Err(error) => {
                    ui.set_loading(false);
                    ui.set_more_available(pages.lock().unwrap().available);
                    let previous = state.lock().unwrap();
                    if !previous.videos.is_empty() {
                        ui.set_channel_name(
                            format!("Previously loaded: {}", previous.channel_name).into(),
                        );
                        show_channel_error(
                            &ui,
                            format!("{error} Previous channel videos are still shown below."),
                        );
                    } else {
                        show_channel_error(&ui, error);
                    }
                    return;
                }
            };
            let exe = match engine::find_dependency("yt-dlp.exe") {
                Ok(exe) => exe,
                Err(error) => {
                    ui.set_loading(false);
                    ui.set_more_available(pages.lock().unwrap().available);
                    show_channel_error(&ui, error);
                    return;
                }
            };
            ui.set_channel_error("".into());
            let same_channel = {
                let mut value = state.lock().unwrap();
                let same = value.channel_url == canonical;
                if !same {
                    value.channel_url = canonical.clone();
                    value.channel_name = String::new();
                    value.videos.clear();
                    value.persist();
                }
                same
            };
            if !same_channel {
                ui.set_channel_name("Loading channel...".into());
                ui.set_video_search("".into());
                publish(&ui, &state.lock().unwrap());
            }
            ui.set_loading(true);
            show_error(&ui, "Scanning channel with yt-dlp…");
            let state = Arc::downgrade(&state);
            let weak = weak.clone();
            let pages = pages.clone();
            let channel_request_generation = channel_request_generation.clone();
            std::thread::spawn(move || {
                let result = engine::load_channel(&exe, &input);
                let _ = slint::invoke_from_event_loop(move || {
                    if channel_request_generation.load(Ordering::SeqCst) != generation {
                        return;
                    }
                    let (Some(ui), Some(state)) = (weak.upgrade(), state.upgrade()) else {
                        return;
                    };
                    ui.set_loading(false);
                    match result {
                        Ok((name, videos)) => {
                            let page_len = videos.len();
                            show_error(
                                &ui,
                                format!(
                                    "Loaded {} videos. Select which to download.",
                                    videos.len()
                                ),
                            );
                            {
                                let mut value = state.lock().unwrap();
                                let clean_name =
                                    apply_loaded_channel_identity(&mut value, &canonical, &name);
                                ui.set_channel_name(clean_name.into());
                                let previous = value.videos.clone();
                                value.videos = reconcile_videos(&previous, videos, same_channel);
                                // Reloading the same channel must not erase
                                // older pages or their selected videos.
                                if same_channel {
                                    let mut seen: std::collections::HashSet<String> = value
                                        .videos
                                        .iter()
                                        .map(|(video, _)| video.id.clone())
                                        .collect();
                                    for (video, selected) in
                                        previous.into_iter().skip(engine::CHANNEL_PAGE_SIZE)
                                    {
                                        if value.videos.len() >= engine::MAX_CHANNEL_VIDEOS {
                                            break;
                                        }
                                        if seen.insert(video.id.clone()) {
                                            value.videos.push((video, selected));
                                        }
                                    }
                                }
                                value.persist();
                                publish_channels(&ui, &value);
                                let mut paging = pages.lock().unwrap();
                                paging.reset(value.videos.len());
                                if page_len < engine::CHANNEL_PAGE_SIZE {
                                    paging.available = false;
                                }
                                ui.set_more_available(paging.available);
                            }
                            publish(&ui, &state.lock().unwrap());
                            update_thumbnails(&ui, &state);
                        }
                        Err(error) => {
                            ui.set_more_available(pages.lock().unwrap().available);
                            if !same_channel {
                                ui.set_channel_name("Channel could not be loaded".into());
                            }
                            show_channel_error(&ui, error);
                        }
                    }
                });
            });
        }
    });

    ui.on_load_more({
        let weak = ui.as_weak();
        let state = state.clone();
        let pages = pages.clone();
        let channel_request_generation = channel_request_generation.clone();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            if ui.get_loading() || !ui.get_video_search().is_empty() {
                return;
            }
            let start = {
                let mut paging = pages.lock().unwrap();
                if paging.fetching || (!paging.available && ui.get_more_error().is_empty()) {
                    return;
                }
                paging.fetching = true;
                paging.next_start
            };
            let generation = channel_request_generation.load(Ordering::SeqCst);
            let (url, executable) = {
                let value = state.lock().unwrap();
                (
                    value.channel_url.clone(),
                    engine::find_dependency("yt-dlp.exe"),
                )
            };
            let executable = match executable {
                Ok(exe) => exe,
                Err(error) => {
                    pages.lock().unwrap().fetching = false;
                    ui.set_more_available(false);
                    ui.set_more_error(error.into());
                    return;
                }
            };
            ui.set_more_available(false);
            ui.set_loading_more(true);
            ui.set_more_error("".into());
            let weak = weak.clone();
            let pages = pages.clone();
            let state = Arc::downgrade(&state);
            let channel_request_generation = channel_request_generation.clone();
            std::thread::spawn(move || {
                let result = engine::load_channel_page(&executable, &url, start);
                let _ = slint::invoke_from_event_loop(move || {
                    if channel_request_generation.load(Ordering::SeqCst) != generation {
                        return;
                    }
                    let (Some(ui), Some(state)) = (weak.upgrade(), state.upgrade()) else {
                        return;
                    };
                    ui.set_loading_more(false);
                    match result {
                        Ok((_name, incoming)) => {
                            let count = incoming.len();
                            let appended = {
                                let mut current = state.lock().unwrap();
                                if current.channel_url != url {
                                    return;
                                }
                                let added =
                                    append_channel_page(&mut current.videos, incoming.clone());
                                current.persist();
                                let mut paging = pages.lock().unwrap();
                                paging.after_page(count, current.videos.len());
                                ui.set_more_available(paging.available);
                                added
                            };
                            if appended > 0 {
                                publish_videos(&ui, &state.lock().unwrap());
                                update_page_thumbnails(&ui, incoming);
                            }
                            show_error(
                                &ui,
                                format!(
                                    "{} channel videos loaded.",
                                    state.lock().unwrap().videos.len()
                                ),
                            );
                        }
                        Err(error) => {
                            pages.lock().unwrap().fetching = false;
                            ui.set_more_error(
                                format!("Could not load more videos: {error}").into(),
                            );
                            ui.set_more_available(false);
                        }
                    }
                });
            });
        }
    });

    ui.on_toggle_video({
        let weak = ui.as_weak();
        let state = state.clone();
        move |id| {
            let Some(ui) = weak.upgrade() else { return };
            let mut value = state.lock().unwrap();
            let Some((_, selected)) = value
                .videos
                .iter_mut()
                .find(|(video, _)| video.id == id.as_str())
            else {
                return;
            };
            *selected = !*selected;
            let selected = *selected;
            value.persist();
            publish_video_selection_change(&ui, &value, id.as_str(), selected);
        }
    });
    ui.on_search_videos({
        let weak = ui.as_weak();
        let state = state.clone();
        move || {
            if let Some(ui) = weak.upgrade() {
                // The review selection is unchanged by searching. Rebuilding
                // that second list on every keystroke can decode hundreds
                // more thumbnails and invalidate its accessibility model.
                publish_filtered_videos(&ui, &state.lock().unwrap());
            }
        }
    });
    ui.on_filter_tasks({
        let weak = ui.as_weak();
        let state = state.clone();
        move || {
            if let Some(ui) = weak.upgrade() {
                publish_tasks(&ui, &state.lock().unwrap());
            }
        }
    });
    ui.on_select_all({
        let weak = ui.as_weak();
        let state = state.clone();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let search = ui.get_video_search().to_string();
            set_visible_selection(&mut state.lock().unwrap().videos, &search, true);
            state.lock().unwrap().persist();
            publish_videos(&ui, &state.lock().unwrap());
        }
    });
    ui.on_clear_selection({
        let weak = ui.as_weak();
        let state = state.clone();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let search = ui.get_video_search().to_string();
            set_visible_selection(&mut state.lock().unwrap().videos, &search, false);
            state.lock().unwrap().persist();
            publish_videos(&ui, &state.lock().unwrap());
        }
    });
    ui.on_choose_folder({
        let weak = ui.as_weak();
        let state = state.clone();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            if let Some(path) = rfd::FileDialog::new()
                .set_title("Choose download folder")
                .pick_folder()
            {
                ui.set_download_folder(path.display().to_string().into());
                {
                    let mut value = state.lock().unwrap();
                    value.folder = path;
                    value.persist();
                }
                ui.set_download_error("".into());
                show_error(&ui, "Download location updated.");
            }
        }
    });
    ui.on_start_download({
        let weak = ui.as_weak();
        let state = state.clone();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let folder = PathBuf::from(ui.get_download_folder().to_string());
            if !folder.is_absolute() {
                show_download_error(&ui, "Choose a valid absolute download directory.");
                return;
            }
            if let Err(error) = std::fs::create_dir_all(&folder) {
                show_download_error(&ui, format!("Cannot create download directory: {error}"));
                return;
            }
            let yt_dlp = match engine::find_dependency("yt-dlp.exe") {
                Ok(path) => path,
                Err(e) => {
                    show_download_error(&ui, e);
                    return;
                }
            };
            let ffmpeg = match engine::find_dependency("ffmpeg.exe") {
                Ok(path) => path,
                Err(e) => {
                    show_download_error(&ui, e);
                    return;
                }
            };
            if let Err(error) = engine::find_dependency("deno.exe") {
                show_download_error(&ui, error);
                return;
            }
            let mode = match Mode::parse(&ui.get_mode()) {
                Ok(mode) => mode,
                Err(e) => {
                    show_download_error(&ui, e);
                    return;
                }
            };
            let quality = ui.get_quality().to_string();
            let selected = state
                .lock()
                .unwrap()
                .videos
                .iter()
                .filter(|(_, selected)| *selected)
                .map(|(v, _)| v.clone())
                .collect::<Vec<_>>();
            if selected.is_empty() {
                show_download_error(&ui, "Select at least one real video before downloading.");
                return;
            }
            if selected.len() > engine::MAX_SELECTED {
                show_download_error(&ui, "Too many downloads selected.");
                return;
            }
            let added = {
                let mut value = state.lock().unwrap();
                let candidates = unqueued_videos(&value.jobs, selected, mode, &quality, &folder);
                if value.jobs.len() + candidates.len() > store::MAX_JOBS {
                    None
                } else {
                    let added = candidates.len();
                    if added > 0 {
                        value.folder = folder.clone();
                        for video in candidates {
                            value.jobs.push(Job {
                                video,
                                mode,
                                quality: quality.clone(),
                                folder: folder.clone(),
                                status: JobState::Queued,
                                percent: 0,
                                detail: "Waiting for download slot…".into(),
                                cancel: Arc::new(AtomicBool::new(false)),
                            });
                        }
                    }
                    // schedule() reserves all available slots and persists a
                    // single complete snapshot *before* any worker starts.
                    // An earlier save here duplicated synchronous disk IO.
                    Some(added)
                }
            };
            let Some(added) = added else {
                show_download_error(
                    &ui,
                    "Queue history is full. Finish current downloads and clear history first.",
                );
                return;
            };
            if added == 0 {
                show_download_error(&ui, "These videos are already queued or downloading.");
                return;
            }
            ui.set_download_error("".into());
            ui.set_page("queue".into());
            show_error(
                &ui,
                format!(
                    "Queued up to {added} items. Running {} simultaneously.",
                    engine::MAX_PARALLEL
                ),
            );
            schedule(&ui, &state, yt_dlp, ffmpeg);
        }
    });
    ui.on_cancel_downloads({
        let weak = ui.as_weak();
        let state = state.clone();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let mut value = state.lock().unwrap();
            let changed = value
                .jobs
                .iter_mut()
                .map(cancel_job)
                .filter(|changed| *changed)
                .count();
            if changed == 0 {
                return;
            }
            let result = value.persist_checked();
            publish_tasks(&ui, &value);
            match result {
                Ok(()) => ui.set_queue_error("".into()),
                Err(error) => show_queue_error(
                    &ui,
                    format!("Cancellation requested, but could not save it for restart: {error}"),
                ),
            }
        }
    });
    ui.on_retry_failed({
        let weak = ui.as_weak();
        let state = state.clone();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let yt_dlp = match engine::find_dependency("yt-dlp.exe") {
                Ok(path) => path,
                Err(e) => {
                    show_queue_error(&ui, e);
                    return;
                }
            };
            let ffmpeg = match engine::find_dependency("ffmpeg.exe") {
                Ok(path) => path,
                Err(e) => {
                    show_queue_error(&ui, e);
                    return;
                }
            };
            let mut count = 0;
            for job in &mut state.lock().unwrap().jobs {
                if retry_job(job) {
                    count += 1;
                }
            }
            if count == 0 {
                show_error(&ui, "No failed downloads to retry.");
                return;
            }
            ui.set_queue_error("".into());
            show_error(&ui, format!("Retrying {count} failed download(s)."));
            // schedule() commits the queued/active recovery states in one
            // atomic session file before launching replacement workers.
            schedule(&ui, &state, yt_dlp, ffmpeg);
        }
    });
    ui.on_clear_finished({
        let weak = ui.as_weak();
        let state = state.clone();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let mut value = state.lock().unwrap();
            let removed = match clear_finished_and_commit(&mut value, &store::session_path()) {
                Ok(removed) => removed,
                Err(error) => {
                    show_queue_error(&ui, error);
                    return;
                }
            };
            publish_tasks(&ui, &value);
            ui.set_queue_error("".into());
            show_error(&ui, format!("Cleared {removed} finished download records."));
        }
    });
    ui.on_cancel_job({
        let weak = ui.as_weak();
        let state = state.clone();
        move |index| {
            let Some(ui) = weak.upgrade() else { return };
            let mut value = state.lock().unwrap();
            if let Some(job) = usize::try_from(index)
                .ok()
                .and_then(|i| value.jobs.get_mut(i))
            {
                if cancel_job(job) {
                    let result = value.persist_checked();
                    publish_tasks(&ui, &value);
                    match result {
                        Ok(()) => ui.set_queue_error("".into()),
                        Err(error) => show_queue_error(
                            &ui,
                            format!(
                                "Cancellation requested, but could not save it for restart: {error}"
                            ),
                        ),
                    }
                }
            }
        }
    });
    ui.on_retry_job({
        let weak = ui.as_weak();
        let state = state.clone();
        move |index| {
            let Some(ui) = weak.upgrade() else { return };
            let yt = match engine::find_dependency("yt-dlp.exe") {
                Ok(path) => path,
                Err(error) => {
                    show_queue_error(&ui, error);
                    return;
                }
            };
            let ffmpeg = match engine::find_dependency("ffmpeg.exe") {
                Ok(path) => path,
                Err(error) => {
                    show_queue_error(&ui, error);
                    return;
                }
            };
            {
                let mut value = state.lock().unwrap();
                if !usize::try_from(index)
                    .ok()
                    .and_then(|i| value.jobs.get_mut(i))
                    .is_some_and(retry_job)
                {
                    return;
                }
                // The scheduler persists this retry before starting it.
            }
            ui.set_queue_error("".into());
            schedule(&ui, &state, yt, ffmpeg);
        }
    });
    ui.on_open_job_folder({
        let weak = ui.as_weak();
        let state = state.clone();
        move |index| {
            let Some(ui) = weak.upgrade() else { return };
            let folder = {
                let value = state.lock().unwrap();
                usize::try_from(index)
                    .ok()
                    .and_then(|i| value.jobs.get(i))
                    .filter(|job| job.status == JobState::Completed)
                    .map(|job| job.folder.clone())
            };
            let Some(folder) = folder else { return };
            if !folder.is_dir() {
                show_queue_error(&ui, "The download folder is no longer available.");
                return;
            }
            if let Err(error) = std::process::Command::new("explorer.exe")
                .arg(folder)
                .spawn()
            {
                show_queue_error(&ui, format!("Could not open folder: {error}"));
            } else {
                ui.set_queue_error("".into());
            }
        }
    });

    publish(&ui, &state.lock().unwrap());
    update_thumbnails(&ui, &state);
    // A deterministic active-transfer UI fixture has no network or filesystem
    // effects. It exists only for screenshot review in isolated smoke sessions.
    if std::env::var("YTD_UI_SMOKE_PREVIEW").as_deref() == Ok("1")
        && std::env::var("YTD_UI_SMOKE_ACTIVE").as_deref() == Ok("1")
    {
        let tasks = ui.get_tasks();
        if let Some(mut row) = tasks.row_data(0) {
            row.state = "Downloading".into();
            row.progress = 47;
            row.detail = "2.8 MiB/s · 00:22 remaining".into();
            tasks.set_row_data(0, row);
            ui.set_pending_items(1);
            ui.set_queue_count("3 total · 1 active · 1 completed · 1 failed".into());
        }
    }
    if std::env::var("YTD_UI_SMOKE_PREVIEW").as_deref() != Ok("1")
        && state
            .lock()
            .unwrap()
            .jobs
            .iter()
            .any(|job| job.status == JobState::Queued)
    {
        if let (Ok(yt_dlp), Ok(ffmpeg), Ok(_deno)) = (
            engine::find_dependency("yt-dlp.exe"),
            engine::find_dependency("ffmpeg.exe"),
            engine::find_dependency("deno.exe"),
        ) {
            schedule(&ui, &state, yt_dlp, ffmpeg);
        }
    }
    // On Windows, requesting a native size before show() can be overwritten
    // during the first winit layout pass. Apply the size after creating the
    // OS window, before entering the Slint event loop.
    ui.show()?;
    // Fit the initial window to smaller laptop displays; resizing, maximize
    // and true fullscreen remain available afterwards.
    #[cfg(windows)]
    let start_size = {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN,
        };
        // SAFETY: GetSystemMetrics is read-only with valid predefined indices.
        let (screen_w, screen_h) =
            unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
        slint::PhysicalSize::new(
            1180u32.min((screen_w - 40).max(880) as u32),
            800u32.min((screen_h - 100).max(625) as u32),
        )
    };
    #[cfg(not(windows))]
    let start_size = slint::PhysicalSize::new(1180, 800);
    ui.window().set_size(start_size);
    match std::env::var("YTD_UI_SMOKE_WINDOW_MODE").as_deref() {
        Ok("maximized") => {
            ui.window().set_maximized(true);
            ui.set_maximized_active(true);
        }
        Ok("fullscreen") => {
            ui.window().set_fullscreen(true);
            ui.set_fullscreen_active(true);
        }
        Ok("compact") => {
            ui.window().set_size(slint::PhysicalSize::new(930, 660));
        }
        Ok("minimum") => {
            ui.window().set_size(slint::PhysicalSize::new(880, 625));
        }
        _ => {}
    }
    ui.run()?;
    Ok(())
}

#[cfg(test)]
mod ui_state_tests {
    use super::*;
    fn init_ui_testing() {
        static INIT: std::sync::Once = std::sync::Once::new();
        INIT.call_once(i_slint_backend_testing::init_integration_test_with_system_time);
    }
    fn video(id: &str, title: &str) -> Video {
        Video {
            id: id.to_string(),
            title: title.to_string(),
            duration: "1:30".to_string(),
            uploaded: String::new(),
        }
    }

    fn display_row(id: &str, title: &str, selected: bool) -> VideoRow {
        VideoRow {
            id: id.into(),
            title: title.into(),
            title_long: false,
            detail: "1:30".into(),
            secondary: "Creator".into(),
            thumbnail: slint::Image::default(),
            selected,
        }
    }

    #[test]
    fn thumbnail_from_visible_row_is_reused_without_reopening_a_path() {
        let mut row = display_row("abcdefghijk", "Cached", true);
        row.thumbnail = slint::Image::from_rgba8(slint::SharedPixelBuffer::new(7, 3));
        let model: ModelRc<VideoRow> = ModelRc::from(Rc::new(VecModel::from(vec![row])));
        let images = existing_video_thumbnails(&model);
        let reused = thumbnail_for_video("abcdefghijk", &images);
        assert_eq!(reused.size().width, 7);
        assert_eq!(reused.size().height, 3);
        assert_eq!(thumbnail_for_video("invalid", &images).size().width, 0);
    }

    #[test]
    fn identical_video_rows_skip_update_but_selection_and_metadata_changes_do_not() {
        let mut original = display_row("abcdefghijk", "Original", false);
        original.thumbnail = slint::Image::from_rgba8(slint::SharedPixelBuffer::new(7, 3));
        let mut refreshed = original.clone();
        assert!(video_row_unchanged(&original, &refreshed));
        refreshed.selected = true;
        assert!(!video_row_unchanged(&original, &refreshed));
        refreshed = original.clone();
        refreshed.title = "Updated".into();
        assert!(!video_row_unchanged(&original, &refreshed));
        refreshed = original.clone();
        refreshed.detail = "2:31".into();
        assert!(!video_row_unchanged(&original, &refreshed));
        refreshed = original.clone();
        refreshed.secondary = "Published now".into();
        assert!(!video_row_unchanged(&original, &refreshed));
        refreshed = original.clone();
        refreshed.id = "pqrstuvwxyz".into();
        assert!(!video_row_unchanged(&original, &refreshed));
        // A changed-ID thumbnail callback can replace the existing image
        // independently of search, and the refresh path must reuse it.
        let mut refreshed = original.clone();
        refreshed.thumbnail = slint::Image::from_rgba8(slint::SharedPixelBuffer::new(9, 2));
        assert!(video_row_unchanged(&original, &refreshed));
    }

    fn task_row(job_index: i32) -> TaskRow {
        TaskRow {
            job_index,
            title: format!("Video {job_index}").into(),
            title_long: false,
            thumbnail: slint::Image::default(),
            detail: "Preparing download  ·  1:30".into(),
            metadata: "Video  ·  Best quality".into(),
            progress: 0,
            state: "Downloading".into(),
        }
    }

    fn queue_fixture(size: usize) -> State {
        State {
            jobs: (0..size)
                .map(|i| Job {
                    video: video(&format!("{i:011}"), "Queued download"),
                    mode: Mode::Video,
                    quality: "720".into(),
                    folder: PathBuf::from(r"D:\Downloads"),
                    status: if i < 3 {
                        JobState::Active
                    } else {
                        JobState::Queued
                    },
                    percent: 0,
                    detail: "Waiting".into(),
                    cancel: Arc::new(AtomicBool::new(false)),
                })
                .collect(),
            active: size.min(3),
            ..State::default()
        }
    }

    #[test]
    fn scheduler_never_launches_unpersisted_jobs_and_rolls_back_failed_reservations() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("schedule-failed-save-{}", std::process::id()));
        let session = root.join("session.json");
        std::fs::create_dir_all(&session).unwrap();
        for (size, existing_active, slots) in [(10usize, 0usize, 1usize), (100, 1, 3), (1000, 2, 3)]
        {
            let mut state = queue_fixture(size);
            state.theme = "red".into();
            state.folder = PathBuf::from(r"D:\Downloads");
            state.active = existing_active;
            for (index, job) in state.jobs.iter_mut().enumerate() {
                job.status = if index < existing_active {
                    JobState::Active
                } else {
                    JobState::Queued
                };
                job.detail = format!("Original detail {index}");
            }
            let before = state
                .jobs
                .iter()
                .map(|job| (job.status, job.detail.clone()))
                .collect::<Vec<_>>();
            // Previously schedule() ignored precisely this save error, then
            // spawned up to three download processes despite no recovery file.
            assert!(reserve_and_commit_jobs(&mut state, &session, slots).is_err());
            assert_eq!(state.active, existing_active);
            assert_eq!(
                state
                    .jobs
                    .iter()
                    .map(|job| (job.status, job.detail.clone()))
                    .collect::<Vec<_>>(),
                before
            );
            assert!(std::fs::read_dir(&root)
                .unwrap()
                .all(|entry| entry.unwrap().path() == session));
        }
        std::fs::remove_dir(&session).unwrap();
        std::fs::remove_dir(&root).unwrap();
    }

    #[test]
    fn scheduler_commits_exactly_the_reserved_slots_before_launch() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("schedule-committed-{}", std::process::id()));
        let session = root.join("session.json");
        for (size, parallel) in [(10usize, 1usize), (100, 3), (1000, 3)] {
            let mut state = queue_fixture(size);
            state.theme = "red".into();
            state.folder = PathBuf::from(r"D:\Downloads");
            state.active = 0;
            for (index, job) in state.jobs.iter_mut().enumerate() {
                job.status = if index % 17 == 0 {
                    JobState::Failed
                } else {
                    JobState::Queued
                };
                job.detail = "Waiting for slot".into();
            }
            let started = reserve_and_commit_jobs(&mut state, &session, parallel).unwrap();
            assert_eq!(started.len(), parallel);
            assert_eq!(state.active, parallel);
            let saved = store::load(&session).unwrap().unwrap();
            assert_eq!(saved.jobs.len(), size);
            for (index, job) in saved.jobs.iter().enumerate() {
                let expected = if started.iter().any(|item| item.0 == index) {
                    store::SavedJobStatus::Queued // load() safely requeues Active
                } else if index % 17 == 0 {
                    store::SavedJobStatus::Failed
                } else {
                    store::SavedJobStatus::Queued
                };
                assert_eq!(job.state, expected);
                if started.iter().any(|item| item.0 == index) {
                    assert!(job.detail.contains("Resuming after application restart"));
                }
            }
        }
        std::fs::remove_file(&session).unwrap();
        std::fs::remove_dir(&root).unwrap();
    }

    #[test]
    fn clear_history_is_atomic_with_save_and_never_loses_memory_on_failure() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("clear-transaction-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let session = root.join("session.json");
        std::fs::create_dir(&session).unwrap();
        for size in [10usize, 100, 1000] {
            let mut state = queue_fixture(size);
            state.theme = "red".into();
            state.folder = PathBuf::from(r"D:\Downloads");
            state.active = 0;
            for job in &mut state.jobs {
                job.status = JobState::Completed;
            }
            state.jobs[1].status = JobState::Failed;
            state.jobs[2].status = JobState::Cancelled;
            let original = state.jobs.iter().map(|job| job.status).collect::<Vec<_>>();
            assert!(clear_finished_and_commit(&mut state, &session).is_err());
            assert_eq!(
                state.jobs.iter().map(|job| job.status).collect::<Vec<_>>(),
                original
            );
        }
        std::fs::remove_dir(&session).unwrap();
        for size in [10usize, 100, 1000] {
            let mut state = queue_fixture(size);
            state.theme = "red".into();
            state.folder = PathBuf::from(r"D:\Downloads");
            state.active = 0;
            for job in &mut state.jobs {
                job.status = JobState::Completed;
            }
            state.jobs[1].status = JobState::Failed;
            assert_eq!(
                clear_finished_and_commit(&mut state, &session).unwrap(),
                size
            );
            assert!(state.jobs.is_empty());
            assert!(store::load(&session).unwrap().unwrap().jobs.is_empty());
        }
        std::fs::remove_file(&session).unwrap();
        std::fs::remove_dir(&root).unwrap();
    }

    #[test]
    fn clear_history_refuses_active_and_queued_downloads_without_mutating() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("clear-busy-{}", std::process::id()));
        let session = root.join("session.json");
        let mut state = queue_fixture(100);
        state.theme = "red".into();
        state.folder = PathBuf::from(r"D:\Downloads");
        let original = state.jobs.len();
        assert!(clear_finished_and_commit(&mut state, &session).is_err());
        assert_eq!(state.jobs.len(), original);
        assert!(!session.exists());
        state.active = 0;
        for job in &mut state.jobs {
            job.status = JobState::Completed;
        }
        state.jobs[7].status = JobState::Queued;
        assert!(clear_finished_and_commit(&mut state, &session).is_err());
        assert!(!session.exists());
    }

    #[test]
    fn completion_updates_only_changed_rows_and_handles_filtered_membership() {
        init_ui_testing();
        let ui = MainWindow::new().unwrap();
        let mut state = queue_fixture(1000);
        ui.set_queue_filter("all".into());
        publish_tasks(&ui, &state);
        let before = ui.get_tasks();
        state.jobs[0].status = JobState::Completed;
        state.jobs[0].percent = 100;
        state.jobs[0].detail = "Saved successfully".into();
        state.jobs[3].status = JobState::Active;
        publish_scheduled_changes(&ui, &state, 0, &[3]);
        let after = ui.get_tasks();
        assert_eq!(after.row_count(), 1000);
        assert_eq!(after.row_data(999).unwrap().state.as_str(), "Completed");
        assert_eq!(after.row_data(996).unwrap().state.as_str(), "Downloading");
        assert_eq!(after.row_data(500).unwrap().state.as_str(), "Queued");
        assert_eq!(after.row_data(999).unwrap().progress, 100);
        assert_eq!(before.row_data(999).unwrap().state.as_str(), "Completed");
        ui.set_queue_filter("active".into());
        publish_tasks(&ui, &state);
        assert_eq!(ui.get_tasks().row_count(), 999);
        let active_model = ui.get_tasks();
        state.jobs[1].status = JobState::Completed;
        publish_scheduled_changes(&ui, &state, 1, &[]);
        assert_eq!(ui.get_tasks().row_count(), 998);
        assert!(ui.get_tasks().row_data(997).is_some());
        assert_eq!(ui.get_tasks(), active_model);
        ui.set_queue_filter("completed".into());
        publish_tasks(&ui, &state);
        let completed_model = ui.get_tasks();
        state.jobs[2].status = JobState::Completed;
        publish_scheduled_changes(&ui, &state, 2, &[]);
        assert_eq!(ui.get_tasks(), completed_model);
        assert_eq!(ui.get_tasks().row_count(), 3);
        assert_eq!(ui.get_tasks().row_data(0).unwrap().job_index, 2);
        assert_eq!(ui.get_tasks().row_data(2).unwrap().job_index, 0);
        ui.set_queue_filter("failed".into());
        publish_tasks(&ui, &state);
        let failed_model = ui.get_tasks();
        state.jobs[3].status = JobState::Failed;
        publish_scheduled_changes(&ui, &state, 3, &[]);
        assert_eq!(ui.get_tasks(), failed_model);
        assert_eq!(ui.get_tasks().row_count(), 1);
        assert_eq!(ui.get_tasks().row_data(0).unwrap().job_index, 3);
        ui.set_queue_filter("active".into());
        publish_tasks(&ui, &state);
        let before_cancel = ui.get_tasks().row_count();
        state.jobs[4].status = JobState::Cancelled;
        publish_scheduled_changes(&ui, &state, 4, &[]);
        assert_eq!(ui.get_tasks().row_count(), before_cancel - 1);
        // Slint binds the testing platform to its first test thread. Exercise
        // search model identity in this same test instead of initializing a
        // second platform from another concurrently scheduled Rust test.
        verify_search_model_identity();
    }

    #[test]
    #[ignore = "offline 10/100/1000-row completion UI benchmark"]
    fn benchmark_queue_completion_ui_refresh() {
        init_ui_testing();
        let ui = MainWindow::new().unwrap();
        ui.set_queue_filter("all".into());
        for size in [10usize, 100, 1000] {
            let mut state = queue_fixture(size);
            publish_tasks(&ui, &state);
            state.jobs[0].status = JobState::Completed;
            state.jobs[0].percent = 100;
            state.jobs[3].status = JobState::Active;
            let iterations = if size == 1000 { 50 } else { 200 };
            let started = std::time::Instant::now();
            for _ in 0..iterations {
                publish_tasks(&ui, &state);
            }
            let old = started.elapsed();
            let started = std::time::Instant::now();
            for _ in 0..iterations {
                publish_scheduled_changes(&ui, &state, 0, &[3]);
            }
            let new = started.elapsed();
            println!(
                "Queue-completion UI size={size} iterations={iterations}: rebuild={old:?}, incremental={new:?}, ratio={:.2}x",
                old.as_secs_f64() / new.as_secs_f64()
            );
            assert_eq!(ui.get_tasks().row_count(), size);
        }
    }

    #[test]
    #[ignore = "offline before/after benchmark of filtered queue lifecycle changes"]
    fn benchmark_filtered_queue_lifecycle() {
        init_ui_testing();
        let old_ui = MainWindow::new().unwrap();
        let new_ui = MainWindow::new().unwrap();
        for size in [100usize, 1000] {
            for filter in ["active", "completed", "failed"] {
                let mut state = queue_fixture(size);
                old_ui.set_queue_filter(filter.into());
                new_ui.set_queue_filter(filter.into());
                publish_tasks(&old_ui, &state);
                publish_tasks(&new_ui, &state);
                let transitions = 30;
                let mut old_samples = Vec::with_capacity(transitions);
                let mut new_samples = Vec::with_capacity(transitions);
                for i in 0..transitions {
                    state.jobs[i].status = if filter == "failed" {
                        JobState::Failed
                    } else {
                        JobState::Completed
                    };
                    state.jobs[i].detail = "Finished".into();
                    state.jobs[i].percent = 100;
                    state.jobs[i + 3].status = JobState::Active;
                    let started = std::time::Instant::now();
                    publish_tasks(&old_ui, &state);
                    old_samples.push(started.elapsed().as_micros());
                    let started = std::time::Instant::now();
                    publish_scheduled_changes(&new_ui, &state, i, &[i + 3]);
                    new_samples.push(started.elapsed().as_micros());
                    let old_rows = old_ui.get_tasks();
                    let new_rows = new_ui.get_tasks();
                    assert_eq!(old_rows.row_count(), new_rows.row_count());
                    for (previous, current) in old_rows.iter().zip(new_rows.iter()) {
                        assert_eq!(previous.job_index, current.job_index);
                        assert_eq!(previous.state, current.state);
                        assert_eq!(previous.progress, current.progress);
                        assert_eq!(previous.detail, current.detail);
                    }
                }
                old_samples.sort_unstable();
                new_samples.sort_unstable();
                println!(
                    "filtered completion rows={size} filter={filter} events={transitions}: baseline_p50={}us baseline_p95={}us targeted_p50={}us targeted_p95={}us ratio={:.1}x",
                    old_samples[transitions/2], old_samples[transitions*95/100],
                    new_samples[transitions/2], new_samples[transitions*95/100],
                    old_samples[transitions/2] as f64 / new_samples[transitions/2].max(1) as f64
                );
            }
        }
    }

    #[test]
    #[ignore = "offline queue completion: two saves plus rebuild versus one save and targeted UI"]
    fn benchmark_queue_completion_with_persistence() {
        init_ui_testing();
        let ui = MainWindow::new().unwrap();
        ui.set_queue_filter("all".into());
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("offline-bench-queue");
        std::fs::create_dir_all(&root).unwrap();
        for (size, iterations) in [(10usize, 15usize), (100, 12), (1000, 8)] {
            let mut state = queue_fixture(size);
            state.theme = "red".into();
            state.folder = PathBuf::from(r"D:\Downloads");
            state.jobs[0].status = JobState::Completed;
            state.jobs[0].percent = 100;
            state.jobs[3].status = JobState::Active;
            let snapshot = state.snapshot();
            let path = root.join(format!("completion-{size}-{}.json", std::process::id()));
            // Warm filesystem and the Slint model before timed comparisons.
            store::save(&path, &snapshot).unwrap();
            publish_tasks(&ui, &state);
            let started = std::time::Instant::now();
            for _ in 0..iterations {
                store::save(&path, &snapshot).unwrap();
                store::save(&path, &snapshot).unwrap();
                publish_tasks(&ui, &state);
            }
            let previous = started.elapsed();
            let started = std::time::Instant::now();
            for _ in 0..iterations {
                store::save(&path, &snapshot).unwrap();
                publish_scheduled_changes(&ui, &state, 0, &[3]);
            }
            let optimized = started.elapsed();
            println!(
                "Completion persistence+UI queue={size} iterations={iterations}: old={previous:?}, new={optimized:?}, ratio={:.2}x",
                previous.as_secs_f64() / optimized.as_secs_f64()
            );
            assert_eq!(ui.get_tasks().row_count(), size);
            std::fs::remove_file(path).unwrap();
        }
    }

    #[test]
    #[ignore = "offline new queued/start and retry commits including real atomic session writes"]
    fn benchmark_queue_start_retry_single_commit() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("offline-bench-start-commit");
        std::fs::create_dir_all(&root).unwrap();
        for size in [10usize, 100, 1000] {
            for parallel in [1usize, 3] {
                let mut state = queue_fixture(size);
                state.theme = "red".into();
                state.folder = PathBuf::from(r"D:\Downloads");
                state.active = parallel;
                for (index, job) in state.jobs.iter_mut().enumerate() {
                    job.status = if index < parallel {
                        JobState::Active
                    } else if index % 11 == 0 {
                        JobState::Failed
                    } else {
                        JobState::Queued
                    };
                }
                let snapshot = state.snapshot();
                let path = root.join(format!("launch-{size}-{parallel}.json"));
                store::save(&path, &snapshot).unwrap();
                let iterations = if size == 1000 { 20 } else { 30 };
                let mut old = Vec::with_capacity(iterations);
                let mut revised = Vec::with_capacity(iterations);
                for _ in 0..iterations {
                    let start = std::time::Instant::now();
                    store::save(&path, &snapshot).unwrap();
                    store::save(&path, &snapshot).unwrap();
                    old.push(start.elapsed().as_micros());
                    let start = std::time::Instant::now();
                    store::save(&path, &snapshot).unwrap();
                    revised.push(start.elapsed().as_micros());
                }
                old.sort_unstable();
                revised.sort_unstable();
                let restored = store::load(&path).unwrap().unwrap();
                assert_eq!(restored.jobs.len(), size);
                assert_eq!(restored.jobs[0].state, store::SavedJobStatus::Queued);
                assert_eq!(restored.jobs[parallel].state, snapshot.jobs[parallel].state);
                println!(
                    "start/retry commit queue={size} active={parallel} serialized_bytes={} p50_old={}us p95_old={}us p50_single={}us p95_single={}us ratio={:.2}x",
                    std::fs::metadata(&path).unwrap().len(),
                    old[iterations / 2], old[iterations * 95 / 100],
                    revised[iterations / 2], revised[iterations * 95 / 100],
                    old[iterations/2] as f64 / revised[iterations/2].max(1) as f64
                );
                std::fs::remove_file(&path).unwrap();
            }
        }
    }

    #[test]
    fn progress_updates_one_filtered_task_without_rebuilding_queue_model() {
        // Rows are displayed newest-first and filters can remove intervening
        // job indices. The lookup must use the stable original job index.
        let backing = Rc::new(VecModel::from(vec![task_row(9), task_row(3)]));
        let model = ModelRc::from(backing.clone());
        assert!(set_task_row_progress(&model, 3, 52, "3.2 MiB/s", "1:30"));
        assert_eq!(backing.row_count(), 2);
        assert_eq!(backing.row_data(0).unwrap().progress, 0);
        let updated = backing.row_data(1).unwrap();
        assert_eq!(updated.progress, 52);
        assert_eq!(updated.detail.as_str(), "3.2 MiB/s  ·  1:30");
        assert_eq!(updated.metadata.as_str(), "Video  ·  Best quality");
        assert_eq!(updated.state.as_str(), "Downloading");
        assert!(!set_task_row_progress(&model, 5, 90, "missing", "2:00"));
        assert_eq!(backing.row_data(0).unwrap().progress, 0);
        assert_eq!(backing.row_data(1).unwrap().progress, 52);
    }

    #[test]
    fn progress_binary_search_handles_sparse_filtered_queue_and_missing_jobs() {
        let backing = Rc::new(VecModel::from(vec![
            task_row(999),
            task_row(721),
            task_row(333),
            task_row(1),
        ]));
        let model = ModelRc::from(backing.clone());
        for (index, position) in [(999, 0), (721, 1), (333, 2), (1, 3)] {
            assert!(set_task_row_progress(&model, index, 64, "fast", "1:00"));
            assert_eq!(backing.row_data(position).unwrap().progress, 64);
        }
        for absent in [0, 2, 334, 720, 722, 1000] {
            assert!(!set_task_row_progress(&model, absent, 90, "absent", "1:00"));
        }
        for position in 0..backing.row_count() {
            let row = backing.row_data(position).unwrap();
            assert_eq!(row.progress, 64);
            assert_eq!(row.detail.as_str(), "fast  ·  1:00");
        }
    }

    #[test]
    fn thumbnail_refresh_preserves_existing_models_selection_and_metadata() {
        let original_rows = Rc::new(VecModel::from(vec![
            display_row("abcdefghijk", "First title", false),
            display_row("pqrstuvwxyz", "Second title", true),
        ]));
        let selected_rows = Rc::new(VecModel::from(vec![display_row(
            "pqrstuvwxyz",
            "Second title",
            true,
        )]));
        let model = ModelRc::from(original_rows.clone());
        let selected = ModelRc::from(selected_rows.clone());
        let updated_image = slint::Image::from_rgba8(slint::SharedPixelBuffer::new(2, 2));
        let changed = std::collections::HashMap::from([("pqrstuvwxyz".into(), updated_image)]);
        apply_thumbnail_images(&[model.clone(), selected.clone()], &changed);
        assert_eq!(original_rows.row_count(), 2);
        let unchanged = original_rows.row_data(0).unwrap();
        assert_eq!(unchanged.thumbnail.size().width, 0);
        assert!(!unchanged.selected);
        let changed_main = original_rows.row_data(1).unwrap();
        assert_eq!(changed_main.thumbnail.size().width, 2);
        assert_eq!(changed_main.title.as_str(), "Second title");
        assert!(changed_main.selected);
        let changed_selected = selected_rows.row_data(0).unwrap();
        assert_eq!(changed_selected.thumbnail.size().width, 2);
        assert_eq!(changed_selected.title.as_str(), "Second title");
    }

    #[test]
    #[ignore = "offline benchmark of full visible list refresh versus six thumbnail rows"]
    fn benchmark_visible_thumbnail_refresh() {
        init_ui_testing();
        let ui = MainWindow::new().unwrap();
        let state = State {
            videos: (0..1000)
                .map(|index| {
                    (
                        video(&format!("{index:011}"), "A selected channel video"),
                        index % 50 == 0,
                    )
                })
                .collect(),
            ..State::default()
        };
        publish_videos(&ui, &state);
        let changed = (0..6)
            .map(|index| {
                (
                    format!("{index:011}"),
                    slint::Image::from_rgba8(slint::SharedPixelBuffer::new(2, 2)),
                )
            })
            .collect::<std::collections::HashMap<_, _>>();
        let iterations = 30;
        let started = std::time::Instant::now();
        for _ in 0..iterations {
            publish_videos(&ui, &state);
        }
        let full_refresh = started.elapsed();
        let videos = ui.get_videos();
        let selected = ui.get_selected_videos();
        let started = std::time::Instant::now();
        for _ in 0..iterations {
            apply_thumbnail_images(&[videos.clone(), selected.clone()], &changed);
        }
        let targeted = started.elapsed();
        println!(
            "Thumbnail UI update: {iterations} batches of 6 new thumbnails / 1000 video rows: full refresh {full_refresh:?}, targeted update {targeted:?}, ratio {:.1}x",
            full_refresh.as_secs_f64() / targeted.as_secs_f64()
        );
        assert!(targeted < full_refresh);
        assert_eq!(videos.row_count(), 1000);
        assert_eq!(selected.row_count(), 20);
    }

    #[test]
    #[ignore = "offline UI search benchmark: unnecessary queue model rebuild"]
    fn benchmark_video_search_queue_rebuild() {
        init_ui_testing();
        let ui = MainWindow::new().unwrap();
        for size in [10usize, 100, 1000] {
            let state = State {
                videos: (0..size)
                    .map(|i| {
                        (
                            video(&format!("{i:011}"), "Example channel video"),
                            i % 20 == 0,
                        )
                    })
                    .collect(),
                jobs: (0..size)
                    .map(|i| Job {
                        video: video(&format!("{i:011}"), "Queued download"),
                        mode: Mode::Video,
                        quality: "720".into(),
                        folder: PathBuf::from(r"D:\Downloads"),
                        status: if i % 3 == 0 {
                            JobState::Completed
                        } else {
                            JobState::Queued
                        },
                        percent: 0,
                        detail: "Waiting".into(),
                        cancel: Arc::new(AtomicBool::new(false)),
                    })
                    .collect(),
                ..State::default()
            };
            let iterations = if size == 1000 { 15 } else { 40 };
            let started = std::time::Instant::now();
            for _ in 0..iterations {
                publish(&ui, &state);
            }
            let full = started.elapsed();
            let started = std::time::Instant::now();
            for _ in 0..iterations {
                publish_videos(&ui, &state);
            }
            let video_only = started.elapsed();
            println!(
                "Video-search UI refresh rows={size} iterations={iterations}: old full={full:?}, videos-only={video_only:?}, ratio={:.2}x",
                full.as_secs_f64() / video_only.as_secs_f64()
            );
            assert_eq!(ui.get_tasks().row_count(), size);
            assert_eq!(ui.get_videos().row_count(), size);
        }
    }

    fn verify_search_model_identity() {
        let ui = MainWindow::new().unwrap();
        let state = State {
            videos: vec![
                (video("abcdefghijk", "Rust channel video"), true),
                (video("pqrstuvwxyz", "Music channel video"), false),
            ],
            jobs: queue_fixture(2).jobs,
            ..State::default()
        };
        publish(&ui, &state);
        let selected_model = ui.get_selected_videos();
        let queue_model = ui.get_tasks();
        let mut review_row = selected_model.row_data(0).unwrap();
        review_row.detail = "Unchanged review row".into();
        selected_model.set_row_data(0, review_row);
        ui.set_video_search("Music".into());
        publish_filtered_videos(&ui, &state);
        assert_eq!(ui.get_videos().row_count(), 1);
        assert_eq!(
            ui.get_videos().row_data(0).unwrap().id.as_str(),
            "pqrstuvwxyz"
        );
        assert_eq!(ui.get_selected_videos().row_count(), 1);
        assert_eq!(
            ui.get_selected_videos()
                .row_data(0)
                .unwrap()
                .detail
                .as_str(),
            "Unchanged review row"
        );
        assert_eq!(ui.get_tasks().row_count(), 2);
        assert_eq!(
            queue_model.row_data(0).unwrap().job_index,
            ui.get_tasks().row_data(0).unwrap().job_index
        );
    }

    #[test]
    #[ignore = "offline UI search benchmark with 100/1000 selected thumbnails"]
    fn benchmark_search_without_selected_review_rebuild() {
        init_ui_testing();
        let ui = MainWindow::new().unwrap();
        for (size, selected_step) in [(100usize, 1), (1000, 10), (1000, 1)] {
            let state = State {
                videos: (0..size)
                    .map(|i| {
                        (
                            video(&format!("{i:011}"), "Example channel video"),
                            i % selected_step == 0,
                        )
                    })
                    .collect(),
                ..State::default()
            };
            publish_videos(&ui, &state);
            let iterations = if size == 1000 { 15 } else { 40 };
            let started = std::time::Instant::now();
            for _ in 0..iterations {
                publish_videos(&ui, &state);
            }
            let old = started.elapsed();
            let started = std::time::Instant::now();
            for _ in 0..iterations {
                publish_filtered_videos(&ui, &state);
            }
            let new = started.elapsed();
            println!(
                "Search selected-review rows={size}, selected={}: full={old:?}, filtered-only={new:?}, ratio={:.2}x",
                size / selected_step,
                old.as_secs_f64() / new.as_secs_f64()
            );
        }
    }

    #[test]
    #[ignore = "offline 100/1000 JPEG thumbnail benchmark; requires isolated LOCALAPPDATA cache fixture"]
    fn benchmark_existing_cached_thumbnail_reuse() {
        init_ui_testing();
        let ui = MainWindow::new().unwrap();
        ui.set_video_search("".into());
        assert!(
            thumbnails::cached("00000000000").is_some(),
            "create 00000000000.jpg in an isolated LOCALAPPDATA thumbnail directory"
        );
        for (size, iterations) in [(100usize, 6usize), (1000, 3)] {
            let state = State {
                videos: (0..size)
                    .map(|i| (video(&format!("{i:011}"), "Cached thumbnail video"), false))
                    .collect(),
                ..State::default()
            };
            publish_filtered_videos(&ui, &state);
            let started = std::time::Instant::now();
            for _ in 0..iterations {
                // Exact previous steady-state path: same row IDs and count,
                // load cached images anew and set individual VecModel rows.
                // In production the prefix-identity branch is also used for
                // repeated searches that retain the visible row membership.
                let rows = state
                    .videos
                    .iter()
                    .map(|(video, selected)| VideoRow {
                        id: video.id.clone().into(),
                        title: video.title.clone().into(),
                        title_long: false,
                        detail: video.duration.clone().into(),
                        secondary: state.channel_name.clone().into(),
                        thumbnail: thumbnails::cached(&video.id)
                            .and_then(|path| slint::Image::load_from_path(&path).ok())
                            .unwrap_or_default(),
                        selected: *selected,
                    })
                    .collect::<Vec<_>>();
                let model = ui.get_videos();
                for (index, row) in rows.into_iter().enumerate() {
                    model.set_row_data(index, row);
                }
            }
            let old = started.elapsed();
            let started = std::time::Instant::now();
            for _ in 0..iterations {
                publish_filtered_videos(&ui, &state);
            }
            let new = started.elapsed();
            println!(
                "Cached JPEG video search rows={size} iterations={iterations}: previous={old:?}, reused-row-images={new:?}, ratio={:.2}x",
                old.as_secs_f64() / new.as_secs_f64()
            );
            assert_eq!(ui.get_videos().row_count(), size);
        }
    }

    #[test]
    #[ignore = "manual, repeatable 1,000-task queue update performance proxy"]
    fn benchmark_queue_progress_model_rebuild_vs_single_row() {
        // Conservative local proxy: model rebuilding and per-row clones only.
        // Production's old path also decoded cached images and counted jobs.
        const ROWS: i32 = 1_000;
        const UPDATES: usize = 500;
        let rows = (0..ROWS).rev().map(task_row).collect::<Vec<_>>();
        let started = std::time::Instant::now();
        for _ in 0..UPDATES {
            let model = ModelRc::from(Rc::new(VecModel::from(rows.clone())));
            std::hint::black_box(model.row_count());
        }
        let rebuild = started.elapsed();
        let model = ModelRc::from(Rc::new(VecModel::from(rows)));
        let started = std::time::Instant::now();
        for percent in 0..UPDATES {
            assert!(set_task_row_progress(
                &model,
                (ROWS - 1) as usize,
                (percent % 101) as i32,
                "3.2 MiB/s",
                "1:30",
            ));
        }
        let targeted = started.elapsed();
        println!(
            "Queue proxy: {UPDATES} progress events, {ROWS} rows: full model rebuild {rebuild:?}, single-row updates {targeted:?}, ratio {:.1}x",
            rebuild.as_secs_f64() / targeted.as_secs_f64()
        );
    }

    #[test]
    fn selection_toggle_mutates_existing_rows_without_replacing_model_or_other_metadata() {
        let backing = Rc::new(VecModel::from(vec![
            display_row("abcdefghijk", "Gameplay upload", false),
            display_row("pqrstuvwxyz", "Different upload", true),
        ]));
        let model = ModelRc::from(backing.clone());
        assert!(set_video_row_selection(&model, "abcdefghijk", true));
        assert!(backing.row_data(0).unwrap().selected);
        assert!(backing.row_data(1).unwrap().selected);
        assert!(set_video_row_selection(&model, "abcdefghijk", false));
        assert!(!backing.row_data(0).unwrap().selected);
        assert_eq!(
            backing.row_data(0).unwrap().title.as_str(),
            "Gameplay upload"
        );
        assert_eq!(backing.row_data(0).unwrap().secondary.as_str(), "Creator");
        assert!(backing.row_data(1).unwrap().selected);
        assert_eq!(model.row_count(), 2);
    }

    #[test]
    fn filtered_model_ignores_non_visible_ids_without_replacing_visible_rows() {
        // The search callback constructs filtered rows. A change to a video
        // not displayed in the current filter must leave these rows untouched.
        let backing = Rc::new(VecModel::from(vec![display_row(
            "abcdefghijk",
            "Gameplay upload",
            false,
        )]));
        let model = ModelRc::from(backing.clone());
        assert!(!set_video_row_selection(&model, "pqrstuvwxyz", true));
        assert_eq!(backing.row_count(), 1);
        assert_eq!(backing.row_data(0).unwrap().id.as_str(), "abcdefghijk");
        assert!(!backing.row_data(0).unwrap().selected);
        assert!(set_video_row_selection(&model, "abcdefghijk", true));
        assert!(backing.row_data(0).unwrap().selected);
    }

    #[test]
    fn youtube_metadata_controls_and_long_unicode_name_do_not_break_queue_persistence() {
        let url = "https://www.youtube.com/@testedcreator/videos";
        // 4-byte characters exceed the storage's 400-byte name limit unless
        // truncated by Unicode characters; the control characters must go.
        let raw_name = format!("\0\n\t{}\u{007f}", "🦊".repeat(150));
        let video = video("abcdefghijk", "Keep this queued video");
        let mut state = State {
            theme: "red".into(),
            channel_name: "Previous creator".into(),
            channel_url: url.into(),
            folder: PathBuf::from(r"C:\Downloads"),
            videos: vec![(video.clone(), true)],
            jobs: vec![Job {
                video,
                mode: Mode::Video,
                quality: "720".into(),
                folder: PathBuf::from(r"C:\Downloads"),
                status: JobState::Queued,
                percent: 0,
                detail: "Queued".into(),
                cancel: Arc::new(AtomicBool::new(false)),
            }],
            saved_channels: vec![store::SavedChannel {
                name: "Old pinned name".into(),
                url: url.into(),
            }],
            ..State::default()
        };
        // This is the same helper used by the production yt-dlp success path.
        let display_name = apply_loaded_channel_identity(&mut state, url, &raw_name);
        assert_eq!(display_name.chars().count(), 100);
        assert_eq!(display_name.len(), 400);
        assert!(!display_name.chars().any(char::is_control));
        assert_eq!(state.channel_name, display_name);
        assert_eq!(state.saved_channels[0].name, display_name);
        assert_eq!(state.channel_history[0].name, display_name);
        let dir = std::env::temp_dir().join(format!(
            "ycd-sanitized-channel-queue-{}",
            std::process::id()
        ));
        let path = dir.join("session.json");
        store::save(&path, &state.snapshot()).unwrap();
        let restored = store::load(&path).unwrap().unwrap();
        assert_eq!(restored.channel_name, display_name);
        assert_eq!(restored.saved_channels[0].name, display_name);
        assert_eq!(restored.channel_history[0].name, display_name);
        assert_eq!(restored.jobs.len(), 1);
        assert_eq!(restored.jobs[0].state, store::SavedJobStatus::Queued);
        assert!(restored.videos[0].selected);
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn reloading_same_channel_retains_only_matching_selected_ids() {
        let old = vec![
            (video("abcdefghijk", "One"), true),
            (video("defghijklmn", "Two"), false),
        ];
        let fresh = vec![
            video("defghijklmn", "Two changed"),
            video("abcdefghijk", "One changed"),
        ];
        let restored = reconcile_videos(&old, fresh.clone(), true);
        assert!(!restored[0].1);
        assert!(restored[1].1);
        assert!(reconcile_videos(&old, fresh, false)
            .iter()
            .all(|(_, selected)| !selected));
    }

    #[test]
    fn large_channel_refresh_retains_selection_across_page_boundaries() {
        let old = (0..1000)
            .map(|index| (video(&format!("{index:011}"), "Old"), index % 7 == 0))
            .collect::<Vec<_>>();
        let incoming = (790..1000)
            .map(|index| video(&format!("{index:011}"), "Updated"))
            .collect::<Vec<_>>();
        let result = reconcile_videos(&old, incoming, true);
        assert_eq!(result.len(), 210);
        for (index, (video, selected)) in (790..1000).zip(result.iter()) {
            assert_eq!(video.id, format!("{index:011}"));
            assert_eq!(*selected, index % 7 == 0);
        }
    }

    #[test]
    #[ignore = "offline selection restoration benchmark at multiple library sizes"]
    fn benchmark_channel_selection_reconciliation() {
        for size in [10usize, 100, 1000] {
            let old = (0..size)
                .map(|index| (video(&format!("{index:011}"), "Existing"), index % 7 == 0))
                .collect::<Vec<_>>();
            let incoming = (size.saturating_sub(200)..size)
                .map(|index| video(&format!("{index:011}"), "Updated"))
                .collect::<Vec<_>>();
            let iterations = if size == 1000 { 200 } else { 1000 };
            let started = std::time::Instant::now();
            for _ in 0..iterations {
                std::hint::black_box(
                    incoming
                        .iter()
                        .cloned()
                        .map(|video| {
                            let selected = old
                                .iter()
                                .any(|(saved, selected)| saved.id == video.id && *selected);
                            (video, selected)
                        })
                        .collect::<Vec<_>>(),
                );
            }
            let linear = started.elapsed();
            let started = std::time::Instant::now();
            for _ in 0..iterations {
                std::hint::black_box(reconcile_videos(&old, incoming.clone(), true));
            }
            let indexed = started.elapsed();
            println!(
                "Selection reconciliation: saved={size}, incoming={}, iterations={iterations}, linear={linear:?}, indexed={indexed:?}, ratio={:.2}x",
                incoming.len(),
                linear.as_secs_f64() / indexed.as_secs_f64(),
            );
            let expected = incoming
                .iter()
                .map(|video| {
                    old.iter()
                        .any(|(saved, selected)| saved.id == video.id && *selected)
                })
                .collect::<Vec<_>>();
            assert_eq!(
                reconcile_videos(&old, incoming, true)
                    .iter()
                    .map(|(_, selected)| *selected)
                    .collect::<Vec<_>>(),
                expected
            );
        }
    }
    #[test]
    fn bulk_selection_only_affects_visible_filtered_titles() {
        let mut rows = vec![
            (video("abcdefghijk", "Rust gameplay"), false),
            (video("defghijklmn", "Unrelated music"), true),
            (video("pqrstuvwxyz", "Rust highlights"), false),
        ];
        set_visible_selection(&mut rows, "RUST", true);
        assert!(rows.iter().all(|(_, selected)| *selected));
        set_visible_selection(&mut rows, "rust", false);
        assert!(!rows[0].1);
        assert!(rows[1].1);
        assert!(!rows[2].1);
        assert_eq!(visible_selected_count(&rows, "rust"), 0);
        assert_eq!(visible_selected_count(&rows, "music"), 1);
        assert_eq!(visible_selected_count(&rows, ""), 1);
        assert_eq!(visible_selected_count(&rows, "does not exist"), 0);
    }
    #[test]
    fn cancel_and_retry_operate_on_only_the_selected_job() {
        let mut a = Job {
            video: video("abcdefghijk", "One"),
            mode: Mode::Video,
            quality: "best".into(),
            folder: PathBuf::from(r"C:\Downloads"),
            status: JobState::Active,
            percent: 47,
            detail: "Downloading".into(),
            cancel: Arc::new(AtomicBool::new(false)),
        };
        let mut b = Job {
            video: video("defghijklmn", "Two"),
            mode: Mode::Video,
            quality: "720".into(),
            folder: PathBuf::from(r"C:\Downloads"),
            status: JobState::Failed,
            percent: 40,
            detail: "Failure".into(),
            cancel: Arc::new(AtomicBool::new(false)),
        };
        assert!(cancel_job(&mut a));
        assert!(a.cancel.load(Ordering::Relaxed));
        assert_eq!(a.status, JobState::Active);
        assert!(!retry_job(&mut a));
        assert!(retry_job(&mut b));
        assert_eq!(b.status, JobState::Queued);
        assert_eq!(b.percent, 0);
        assert!(!b.cancel.load(Ordering::Relaxed));
        assert!(!retry_job(&mut b));
    }

    #[test]
    fn cancelling_active_download_stays_cancelled_after_restart() {
        let make_job = |id: &str| Job {
            video: video(id, "Download"),
            mode: Mode::Video,
            quality: "720".into(),
            folder: PathBuf::from(r"C:\Downloads"),
            status: JobState::Active,
            percent: 32,
            detail: "Downloading".into(),
            cancel: Arc::new(AtomicBool::new(false)),
        };
        let mut state = State {
            theme: "red".into(),
            folder: PathBuf::from(r"C:\Downloads"),
            jobs: vec![make_job("abcdefghijk"), make_job("defghijklmn")],
            ..State::default()
        };
        assert!(cancel_job(&mut state.jobs[0]));
        assert_eq!(state.jobs[0].status, JobState::Active);
        let snapshot = state.snapshot();
        assert_eq!(snapshot.jobs[0].state, store::SavedJobStatus::Cancelled);
        assert_eq!(snapshot.jobs[0].detail, "Cancelled by user.");
        assert_eq!(snapshot.jobs[1].state, store::SavedJobStatus::Active);

        let dir = std::env::temp_dir().join(format!("ycd-cancel-restart-{}", std::process::id()));
        let path = dir.join("session.json");
        store::save(&path, &snapshot).unwrap();
        let restored = store::load(&path).unwrap().unwrap();
        assert_eq!(restored.jobs[0].state, store::SavedJobStatus::Cancelled);
        assert_eq!(restored.jobs[1].state, store::SavedJobStatus::Queued);
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn queued_duplicate_detection_includes_destination_folder() {
        let video = video("abcdefghijk", "One");
        let source = PathBuf::from(r"C:\Downloads\One");
        let other = PathBuf::from(r"C:\Downloads\Two");
        let mut job = Job {
            video: video.clone(),
            mode: Mode::Video,
            quality: "720".into(),
            folder: source.clone(),
            status: JobState::Queued,
            percent: 0,
            detail: String::new(),
            cancel: Arc::new(AtomicBool::new(false)),
        };
        assert!(is_pending_duplicate(
            &job,
            &video,
            Mode::Video,
            "720",
            &source
        ));
        assert!(!is_pending_duplicate(
            &job,
            &video,
            Mode::Video,
            "720",
            &other
        ));
        assert!(!is_pending_duplicate(
            &job,
            &video,
            Mode::Video,
            "1080",
            &source
        ));
        job.status = JobState::Completed;
        assert!(!is_pending_duplicate(
            &job,
            &video,
            Mode::Video,
            "720",
            &source
        ));
    }

    #[test]
    fn queue_capacity_counts_only_new_jobs_after_duplicate_filtering() {
        let folder = PathBuf::from(r"C:\Downloads");
        let jobs = (0..store::MAX_JOBS - 1)
            .map(|index| Job {
                video: video(&format!("{index:011}"), "Previous"),
                mode: Mode::Video,
                quality: "best".into(),
                folder: folder.clone(),
                status: if index == 0 {
                    JobState::Active
                } else {
                    JobState::Completed
                },
                percent: 0,
                detail: String::new(),
                cancel: Arc::new(AtomicBool::new(false)),
            })
            .collect::<Vec<_>>();
        let selected = vec![
            video("00000000000", "Already active"),
            video("abcdefghijk", "New"),
        ];
        assert!(jobs.len() + selected.len() > store::MAX_JOBS);
        let candidates = unqueued_videos(&jobs, selected, Mode::Video, "best", &folder);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].id, "abcdefghijk");
        assert_eq!(jobs.len() + candidates.len(), store::MAX_JOBS);
    }

    #[test]
    fn indexed_queue_matching_preserves_format_destination_and_status_rules() {
        let folder = PathBuf::from(r"C:\Downloads");
        let other_folder = PathBuf::from(r"C:\Other");
        let mut active = Job {
            video: video("abcdefghijk", "Existing"),
            mode: Mode::Video,
            quality: "720".into(),
            folder: folder.clone(),
            status: JobState::Active,
            percent: 30,
            detail: String::new(),
            cancel: Arc::new(AtomicBool::new(false)),
        };
        let selected = vec![
            video("abcdefghijk", "Existing"),
            video("pqrstuvwxyz", "New"),
        ];
        let selected_ids = |jobs: &[Job], mode, quality, path: &PathBuf| {
            unqueued_videos(jobs, selected.clone(), mode, quality, path)
                .into_iter()
                .map(|item| item.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            selected_ids(&[active], Mode::Video, "720", &folder),
            vec!["pqrstuvwxyz"]
        );
        active = Job {
            video: video("abcdefghijk", "Existing"),
            mode: Mode::Video,
            quality: "720".into(),
            folder: folder.clone(),
            status: JobState::Active,
            percent: 30,
            detail: String::new(),
            cancel: Arc::new(AtomicBool::new(false)),
        };
        for (mode, quality, destination, status) in [
            (Mode::Video, "1080", &folder, JobState::Active),
            (Mode::Audio, "720", &folder, JobState::Active),
            (Mode::Video, "720", &other_folder, JobState::Active),
            (Mode::Video, "720", &folder, JobState::Completed),
            (Mode::Video, "720", &folder, JobState::Cancelled),
        ] {
            active.status = status;
            assert_eq!(
                selected_ids(std::slice::from_ref(&active), mode, quality, destination),
                vec!["abcdefghijk", "pqrstuvwxyz"]
            );
        }
    }

    #[test]
    #[ignore = "manual offline 10/100/1000 job duplicate-matching comparison"]
    fn benchmark_indexed_queue_duplicate_matching() {
        let folder = PathBuf::from(r"C:\Downloads");
        for (size, iterations) in [(10usize, 1000usize), (100, 200), (1000, 20)] {
            let jobs = (0..size)
                .map(|index| Job {
                    video: video(&format!("{index:011}"), "Existing"),
                    mode: Mode::Video,
                    quality: "720".into(),
                    folder: folder.clone(),
                    status: JobState::Queued,
                    percent: 0,
                    detail: String::new(),
                    cancel: Arc::new(AtomicBool::new(false)),
                })
                .collect::<Vec<_>>();
            let selected = jobs.iter().map(|job| job.video.clone()).collect::<Vec<_>>();
            // The exact pre-fix matching operation, kept only as a benchmark.
            let old = |selection: Vec<Video>| {
                selection
                    .into_iter()
                    .filter(|video| {
                        !jobs.iter().any(|job| {
                            is_pending_duplicate(job, video, Mode::Video, "720", &folder)
                        })
                    })
                    .collect::<Vec<_>>()
            };
            let start = std::time::Instant::now();
            for _ in 0..iterations {
                std::hint::black_box(old(selected.clone()));
            }
            let linear = start.elapsed();
            let start = std::time::Instant::now();
            for _ in 0..iterations {
                std::hint::black_box(unqueued_videos(
                    &jobs,
                    selected.clone(),
                    Mode::Video,
                    "720",
                    &folder,
                ));
            }
            let indexed = start.elapsed();
            println!(
                "Duplicate matching {size} history + {size} selections x{iterations}: linear {linear:?}, indexed {indexed:?}, ratio {:.1}x",
                linear.as_secs_f64() / indexed.as_secs_f64()
            );
            assert_eq!(
                old(selected.clone()),
                unqueued_videos(&jobs, selected, Mode::Video, "720", &folder)
            );
        }
    }

    #[test]
    fn newer_channel_attempt_invalidates_older_generation() {
        let current = AtomicU64::new(0);
        let first = begin_channel_request(&current);
        assert_eq!(first, 1);
        // A validation failure still consumes a generation, so the prior
        // worker cannot publish an outdated result after this second click.
        let invalid_second = begin_channel_request(&current);
        assert_eq!(invalid_second, 2);
        assert_ne!(current.load(Ordering::SeqCst), first);
        assert_eq!(current.load(Ordering::SeqCst), invalid_second);
    }

    #[test]
    fn scrolling_pagination_advances_in_batches_and_stops_at_end() {
        let mut pages = ChannelPages::default();
        pages.reset(200);
        assert_eq!(pages.next_start, 201);
        assert!(pages.available);
        pages.fetching = true;
        pages.after_page(200, 400);
        assert_eq!(pages.next_start, 401);
        assert!(pages.available);
        assert!(!pages.fetching);
        pages.after_page(58, 458);
        assert!(!pages.available);
        pages.reset(1_000);
        assert!(!pages.available);
        pages.reset(398);
        assert_eq!(pages.next_start, 401);
    }

    #[test]
    fn next_page_preserves_old_selections_and_skips_duplicate_ids() {
        let mut rows = vec![(video("abcdefghijk", "Selected"), true)];
        let appended = append_channel_page(
            &mut rows,
            vec![
                video("abcdefghijk", "Duplicate"),
                video("pqrstuvwxyz", "New entry"),
                video("pqrstuvwxyz", "Repeated"),
            ],
        );
        assert_eq!(appended, 1);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0.title, "Selected");
        assert!(rows[0].1);
        assert!(!rows[1].1);
    }
}
