//! Latest-value progress delivery between downloader workers and Slint.
//! A worker can emit hundreds of updates while the UI is busy; only the latest
//! one matters for a progress indicator. Keep at most one UI callback pending
//! per worker so a burst cannot starve clicks or completion callbacks.

use std::sync::Mutex;

#[derive(Default)]
pub struct ProgressMailbox {
    pending: Mutex<Pending>,
}

#[derive(Default)]
struct Pending {
    latest: Option<(i32, String)>,
    last_published: Option<(i32, String)>,
    notification_queued: bool,
}

impl ProgressMailbox {
    /// Store the latest reading. True means the caller must queue a single
    /// UI callback; false means an earlier callback will pick this value up.
    pub fn publish(&self, percent: i32, detail: String) -> bool {
        let mut pending = self.pending.lock().unwrap();
        // A responsive event loop can consume each callback before the next
        // yt-dlp reading. Coalescing alone would still notify Slint for every
        // unchanged percentage/speed/ETA. Preserve updates when *either*
        // field changes and discard only exact duplicates.
        if pending
            .last_published
            .as_ref()
            .is_some_and(|(last_percent, last_detail)| {
                *last_percent == percent && *last_detail == detail
            })
        {
            return false;
        }
        pending.last_published = Some((percent, detail.clone()));
        pending.latest = Some((percent, detail));
        if pending.notification_queued {
            return false;
        }
        pending.notification_queued = true;
        true
    }

    /// Consume the newest value and release the notification slot atomically.
    /// Holding one lock across both operations avoids losing a concurrent
    /// update between take() and clearing the notification flag.
    pub fn consume(&self) -> Option<(i32, String)> {
        let mut pending = self.pending.lock().unwrap();
        let latest = pending.latest.take();
        pending.notification_queued = false;
        latest
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    #[ignore = "offline 1/3-worker 10/60/120 Hz progress notification baseline"]
    fn benchmark_stable_progress_notifications() {
        for workers in [1usize, 3] {
            for frequency in [10usize, 60, 120] {
                let mailboxes = (0..workers)
                    .map(|_| ProgressMailbox::default())
                    .collect::<Vec<_>>();
                let mut notifications = 0usize;
                let mut latencies = Vec::new();
                let started = std::time::Instant::now();
                for tick in 0..frequency * 30 {
                    for mailbox in &mailboxes {
                        // Steady source: speed/ETA updates once per second,
                        // percent changes at the source's reported precision.
                        let percent = (tick * 100 / (frequency * 30)) as i32;
                        let detail = format!("3.0 MiB/s · ETA {}", 30 - tick / frequency);
                        let before = std::time::Instant::now();
                        if mailbox.publish(percent, detail) {
                            notifications += 1;
                            assert!(mailbox.consume().is_some());
                        }
                        latencies.push(before.elapsed().as_nanos() as u64);
                    }
                }
                latencies.sort_unstable();
                println!(
                    "steady progress workers={workers} events_per_second={frequency} events={} notifications={notifications} p50_ns={} p95_ns={} elapsed_us={}",
                    latencies.len(), latencies[latencies.len()/2], latencies[latencies.len()*95/100], started.elapsed().as_micros()
                );
            }
        }
    }

    #[test]
    fn latest_update_wins_and_one_notification_serves_a_burst() {
        let mailbox = ProgressMailbox::default();
        assert!(mailbox.publish(1, "initial".into()));
        for percent in 2..=100 {
            assert!(!mailbox.publish(percent, format!("progress {percent}")));
        }
        assert_eq!(mailbox.consume(), Some((100, "progress 100".into())));
        assert_eq!(mailbox.consume(), None);
        assert!(mailbox.publish(10, "new frame".into()));
        assert_eq!(mailbox.consume(), Some((10, "new frame".into())));
    }

    #[test]
    fn duplicates_do_not_requeue_ui_callbacks_but_detail_changes_do() {
        let mailbox = ProgressMailbox::default();
        assert!(mailbox.publish(40, "2 MiB/s · 0:10".into()));
        assert!(!mailbox.publish(40, "2 MiB/s · 0:10".into()));
        assert_eq!(mailbox.consume(), Some((40, "2 MiB/s · 0:10".into())));
        assert!(!mailbox.publish(40, "2 MiB/s · 0:10".into()));
        assert_eq!(mailbox.consume(), None);
        assert!(mailbox.publish(40, "3 MiB/s · 0:09".into()));
        assert!(!mailbox.publish(40, "3 MiB/s · 0:09".into()));
        assert_eq!(mailbox.consume(), Some((40, "3 MiB/s · 0:09".into())));
        assert!(mailbox.publish(41, "3 MiB/s · 0:09".into()));
        assert_eq!(mailbox.consume(), Some((41, "3 MiB/s · 0:09".into())));
    }

    #[test]
    fn concurrent_producers_never_leave_progress_without_a_notification() {
        // Racing the consumer against another producer must either expose
        // its update in the current consume or request another UI callback.
        for _ in 0..500 {
            let mailbox = Arc::new(ProgressMailbox::default());
            assert!(mailbox.publish(1, "first".into()));
            let worker = mailbox.clone();
            let producer = std::thread::spawn(move || worker.publish(2, "second".into()));
            let current = mailbox.consume().unwrap();
            let queued_again = producer.join().unwrap();
            let next = mailbox.consume();
            assert!(
                (current.0 == 2 && !queued_again)
                    || (current.0 == 1 && queued_again && next == Some((2, "second".into()))),
                "current={current:?}, next={next:?}, queued_again={queued_again}"
            );
        }
    }
}
