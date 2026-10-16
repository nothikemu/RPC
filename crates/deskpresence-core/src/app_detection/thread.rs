//! The detector runs on its own OS thread so Win32 calls never block the UI
//! or the async runtime. It sleeps on a control channel between ticks (no
//! busy waiting) and publishes snapshots through a `watch` channel, which only
//! ever holds the latest value (no unbounded queue).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tokio::sync::watch;

use super::detector::{Detector, foreground_tick_interval};
use super::{DetectorSettings, DetectorSnapshot, WindowSystem};
use crate::time::now_unix_ms;

enum Control {
    Configure(DetectorSettings),
    Rescan,
    Shutdown,
}

/// Cloneable control channel for a running detector.
#[derive(Clone)]
pub struct DetectorController(mpsc::Sender<Control>);

impl DetectorController {
    pub fn configure(&self, settings: DetectorSettings) {
        let _ = self.0.send(Control::Configure(settings));
    }

    pub fn rescan(&self) {
        let _ = self.0.send(Control::Rescan);
    }
}

/// Owner of the detector thread.
pub struct DetectorHandle {
    control: mpsc::Sender<Control>,
    done: mpsc::Receiver<()>,
    thread: Option<JoinHandle<()>>,
}

impl DetectorHandle {
    pub fn spawn(
        ws: Box<dyn WindowSystem>,
        settings: DetectorSettings,
        snapshots: watch::Sender<Arc<DetectorSnapshot>>,
    ) -> std::io::Result<Self> {
        let (control, rx) = mpsc::channel();
        let (done_tx, done) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("deskpresence-detector".into())
            .spawn(move || {
                run(Detector::new(ws, settings), rx, snapshots);
                let _ = done_tx.send(());
            })?;
        Ok(Self {
            control,
            done,
            thread: Some(thread),
        })
    }

    pub fn controller(&self) -> DetectorController {
        DetectorController(self.control.clone())
    }

    /// Stops the thread, waiting at most `timeout`. A thread stuck in an OS
    /// call is detached rather than blocking application exit.
    pub fn shutdown(mut self, timeout: Duration) {
        let _ = self.control.send(Control::Shutdown);
        match self.done.recv_timeout(timeout) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {
                if let Some(thread) = self.thread.take() {
                    let _ = thread.join();
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                tracing::warn!("detector thread did not stop in time; detaching");
            }
        }
    }
}

impl Drop for DetectorHandle {
    fn drop(&mut self) {
        let _ = self.control.send(Control::Shutdown);
    }
}

fn run(
    mut detector: Detector,
    control: mpsc::Receiver<Control>,
    snapshots: watch::Sender<Arc<DetectorSnapshot>>,
) {
    tracing::info!("detector thread started");
    let mut published = detector.snapshot();
    snapshots.send_replace(Arc::new(published.clone()));

    if !detector.is_supported() {
        // Nothing to poll: just wait for shutdown.
        loop {
            match control.recv() {
                Ok(Control::Shutdown) | Err(_) => break,
                Ok(_) => {}
            }
        }
        return;
    }

    let mut next_full_scan = Instant::now();
    let mut consecutive_panics = 0u32;
    loop {
        let settings = detector.settings();
        let wait = if consecutive_panics > 0 {
            Duration::from_secs(u64::from(consecutive_panics.min(30)))
        } else {
            foreground_tick_interval(&settings)
        };
        let mut force_scan = false;
        match control.recv_timeout(wait) {
            Ok(Control::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(Control::Configure(s)) => {
                detector.configure(s);
                force_scan = true;
            }
            Ok(Control::Rescan) => force_scan = true,
            Err(RecvTimeoutError::Timeout) => {}
        }

        let result = catch_unwind(AssertUnwindSafe(|| {
            let now = Instant::now();
            let now_ms = now_unix_ms();
            if force_scan || now >= next_full_scan {
                detector.full_scan(now_ms);
                next_full_scan = now + detector.settings().poll_interval;
            }
            detector.foreground_tick(now, now_ms);
        }));
        match result {
            Ok(()) => consecutive_panics = 0,
            Err(_) => {
                consecutive_panics += 1;
                tracing::error!(consecutive_panics, "detector iteration panicked; continuing");
                detector.mark_degraded("An internal detection error occurred; retrying.".into());
            }
        }

        let snapshot = detector.snapshot();
        if !snapshot.meaningfully_equals(&published) {
            published = snapshot.clone();
            snapshots.send_replace(Arc::new(snapshot));
        }
    }
    detector.mark_stopped();
    snapshots.send_replace(Arc::new(detector.snapshot()));
    tracing::info!("detector thread stopped");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_detection::DetectorStatus;
    use crate::app_detection::mock::{MockApp, MockWindowSystem};

    #[test]
    fn publishes_changes_and_stops_cleanly() {
        let ws = MockWindowSystem::new();
        ws.add(MockApp::new(10, r"C:\Apps\a.exe"));
        let (tx, mut rx) = watch::channel(Arc::new(DetectorSnapshot::default()));
        let settings = DetectorSettings {
            poll_interval: Duration::from_millis(250),
            foreground_debounce: Duration::ZERO,
            capture_titles: false,
        };
        let handle = DetectorHandle::spawn(Box::new(ws.clone()), settings, tx).unwrap();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        rt.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    rx.changed().await.unwrap();
                    if rx.borrow().apps.len() == 1 {
                        break;
                    }
                }
            })
            .await
            .expect("first snapshot");

            ws.add(MockApp::new(11, r"C:\Apps\b.exe"));
            ws.focus_pid(11);
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    rx.changed().await.unwrap();
                    let snap = rx.borrow().clone();
                    if snap.apps.len() == 2 && snap.foreground.is_some() {
                        break;
                    }
                }
            })
            .await
            .expect("second snapshot");
        });

        let started = Instant::now();
        handle.shutdown(Duration::from_secs(2));
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(rx.borrow().status, DetectorStatus::Stopped);
    }
}
