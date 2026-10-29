//! Ignored, offline benchmark of the real Slint event queue and TaskRow model.
//! Run in isolation: cargo test --locked benchmark_slint_progress_event_delivery -- --ignored --nocapture
use super::*;
use std::sync::atomic::AtomicUsize;
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Default)]
struct EventStats {
    applied_ages_us: Mutex<Vec<u64>>,
    pending: AtomicUsize,
    peak_pending: AtomicUsize,
}

impl EventStats {
    fn queued(&self) {
        let count = self.pending.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak_pending.fetch_max(count, Ordering::SeqCst);
    }

    fn applied(&self, created: Instant) {
        self.applied_ages_us
            .lock()
            .unwrap()
            .push(created.elapsed().as_micros() as u64);
        self.pending.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(windows)]
fn process_cpu_and_memory() -> (u64, usize) {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    // SAFETY: pseudo-handle refers to our process; output pointers are fully
    // initialized native POD structures with the correct sizes.
    unsafe {
        let process = GetCurrentProcess();
        let mut created = std::mem::zeroed::<FILETIME>();
        let mut exited = std::mem::zeroed::<FILETIME>();
        let mut kernel = std::mem::zeroed::<FILETIME>();
        let mut user = std::mem::zeroed::<FILETIME>();
        let mut counters = std::mem::zeroed::<PROCESS_MEMORY_COUNTERS>();
        counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        assert_ne!(
            GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user),
            0
        );
        assert_ne!(
            K32GetProcessMemoryInfo(process, &mut counters, counters.cb),
            0
        );
        let ticks =
            |time: FILETIME| ((time.dwHighDateTime as u64) << 32) | (time.dwLowDateTime as u64);
        (
            (ticks(kernel) + ticks(user)) / 10_000,
            counters.WorkingSetSize,
        )
    }
}

#[cfg(not(windows))]
fn process_cpu_and_memory() -> (u64, usize) {
    (0, 0)
}

fn sample_row(index: i32) -> TaskRow {
    TaskRow {
        job_index: index,
        title: format!("Upload {index}").into(),
        title_long: false,
        detail: "Waiting  ·  1:30".into(),
        metadata: "Video  ·  Best quality".into(),
        thumbnail: slint::Image::default(),
        state: "Downloading".into(),
        progress: 0,
    }
}

fn run_case(
    weak: &slint::Weak<MainWindow>,
    rows: usize,
    events_per_second_per_job: usize,
    use_mailbox: bool,
    burst: bool,
) {
    // All model creation and mutation occur on the real Slint event-loop
    // thread; this worker only sends events, exactly as the downloader does.
    let (model_tx, model_rx) = mpsc::sync_channel(0);
    let model_weak = weak.clone();
    slint::invoke_from_event_loop(move || {
        if let Some(ui) = model_weak.upgrade() {
            ui.set_tasks(ModelRc::from(Rc::new(VecModel::from(
                (0..rows)
                    .rev()
                    .map(|index| sample_row(index as i32))
                    .collect::<Vec<_>>(),
            ))));
        }
        model_tx.send(()).unwrap();
    })
    .unwrap();
    model_rx.recv().unwrap();

    let stats = Arc::new(EventStats::default());
    let mailboxes = (0..3)
        .map(|_| Arc::new(progress::ProgressMailbox::default()))
        .collect::<Vec<_>>();
    let last_published = (0..3)
        .map(|_| Arc::new(Mutex::new(Instant::now())))
        .collect::<Vec<_>>();
    let job_ids = [0, rows / 2, rows - 1];
    let duration = Duration::from_millis(if burst { 0 } else { 350 });
    let events_per_job = if burst {
        1000
    } else {
        events_per_second_per_job * duration.as_millis() as usize / 1000
    };
    // Mimic a 75ms main-thread stall caused by layout, disk activity, or an
    // expensive callback before a sudden progress burst reaches the UI.
    if burst {
        slint::invoke_from_event_loop(|| std::thread::sleep(Duration::from_millis(75))).unwrap();
    }
    let before = process_cpu_and_memory();
    let start = Instant::now();
    for sequence in 0..events_per_job {
        if !burst {
            let due =
                start + Duration::from_secs_f64(sequence as f64 / events_per_second_per_job as f64);
            if let Some(wait) = due.checked_duration_since(Instant::now()) {
                std::thread::sleep(wait);
            }
        }
        for job in 0..3 {
            let id = job_ids[job];
            let percent = ((sequence + job) % 101) as i32;
            let detail = format!("3.2 MiB/s #{sequence:04}");
            let emitted_at = Instant::now();
            let stats_for_event = stats.clone();
            let weak_for_event = weak.clone();
            if use_mailbox {
                *last_published[job].lock().unwrap() = emitted_at;
                if !mailboxes[job].publish(percent, detail) {
                    continue;
                }
                let mailbox = mailboxes[job].clone();
                let timestamp = last_published[job].clone();
                stats.queued();
                slint::invoke_from_event_loop(move || {
                    if let Some((percent, detail)) = mailbox.consume() {
                        let created = *timestamp.lock().unwrap();
                        if let Some(ui) = weak_for_event.upgrade() {
                            set_task_row_progress(&ui.get_tasks(), id, percent, &detail, "1:30");
                        }
                        stats_for_event.applied(created);
                    }
                })
                .unwrap();
            } else {
                stats.queued();
                slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak_for_event.upgrade() {
                        set_task_row_progress(&ui.get_tasks(), id, percent, &detail, "1:30");
                    }
                    stats_for_event.applied(emitted_at);
                })
                .unwrap();
            }
        }
    }
    let (done_tx, done_rx) = mpsc::sync_channel(0);
    slint::invoke_from_event_loop(move || done_tx.send(()).unwrap()).unwrap();
    done_rx.recv().unwrap();
    let elapsed = start.elapsed();
    let after = process_cpu_and_memory();
    let mut latencies = stats.applied_ages_us.lock().unwrap().clone();
    latencies.sort_unstable();
    let p50 = latencies[latencies.len() / 2];
    let p95 = latencies[latencies.len() * 95 / 100];
    let max = latencies[latencies.len() - 1];
    let mode = if use_mailbox { "coalesced" } else { "per-line" };
    println!(
        "Slint-event rows={rows:4}, per-job-rate={events_per_second_per_job:3}/s, burst={burst:5}, mode={mode:9}: produced={} applied={}, wall={:.1}ms, process-CPU={}ms, working-set={}KiB, peak-pending={}, p50/p95/max UI-model-latency={p50}/{p95}/{max}us",
        events_per_job * 3,
        latencies.len(),
        elapsed.as_secs_f64() * 1000.0,
        after.0.saturating_sub(before.0),
        after.1 / 1024,
        stats.peak_pending.load(Ordering::SeqCst),
    );
    assert_eq!(stats.pending.load(Ordering::SeqCst), 0);
    assert!(latencies.len() <= events_per_job * 3);
    assert!(use_mailbox || latencies.len() == events_per_job * 3);
}

#[test]
#[ignore = "offline headless Slint event loop benchmark; run alone with --ignored --nocapture"]
fn benchmark_slint_progress_event_delivery() {
    // The backend runs Slint's genuine event queue with no native window,
    // rendering, network, downloaded media, or user session data.
    i_slint_backend_testing::init_integration_test_with_system_time();
    let ui = MainWindow::new().unwrap();
    let weak = ui.as_weak();
    let producer = std::thread::spawn(move || {
        for rows in [10, 100, 1000] {
            for rate in [10, 60, 120] {
                for coalesced in [false, true] {
                    run_case(&weak, rows, rate, coalesced, false);
                }
            }
        }
        for coalesced in [false, true] {
            run_case(&weak, 1000, 1000, coalesced, true);
        }
        slint::invoke_from_event_loop(|| slint::quit_event_loop().unwrap()).unwrap();
    });
    slint::run_event_loop().unwrap();
    producer.join().unwrap();
}
