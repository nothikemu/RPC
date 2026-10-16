//! Deterministic window systems for tests, development previews and
//! platforms without real detection.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use super::{DetectError, FileStamp, ProcessIdentity, RawWindow, WindowSystem};
use crate::app_identity::{ExeMetadata, normalize_path};

/// Used on non-Windows platforms: detection is reported as unsupported.
pub struct UnsupportedWindowSystem;

impl WindowSystem for UnsupportedWindowSystem {
    fn name(&self) -> &'static str {
        "unsupported"
    }

    fn unsupported_reason(&self) -> Option<String> {
        Some(format!(
            "Application detection is only implemented for Windows (this build targets {}).",
            std::env::consts::OS
        ))
    }

    fn top_level_windows(&mut self, _: bool) -> Result<Vec<RawWindow>, DetectError> {
        Ok(Vec::new())
    }

    fn foreground_window(&mut self, _: bool) -> Option<RawWindow> {
        None
    }

    fn process_identity(&mut self, _: u32) -> Result<ProcessIdentity, DetectError> {
        Err(DetectError::Os("unsupported platform".into()))
    }

    fn exe_metadata(&mut self, _: &str) -> Option<ExeMetadata> {
        None
    }
}

/// Builder for a mock application with one or more windows.
#[derive(Clone, Debug)]
pub struct MockApp {
    pid: u32,
    path: String,
    windows: u32,
    title: Option<String>,
    description: Option<String>,
    started_at_ms: Option<u64>,
}

impl MockApp {
    pub fn new(pid: u32, path: &str) -> Self {
        Self {
            pid,
            path: path.to_owned(),
            windows: 1,
            title: None,
            description: None,
            started_at_ms: Some(1_000 + u64::from(pid)),
        }
    }

    pub fn windows(mut self, count: u32) -> Self {
        self.windows = count;
        self
    }

    pub fn title(mut self, title: &str) -> Self {
        self.title = Some(title.to_owned());
        self
    }

    pub fn description(mut self, description: &str) -> Self {
        self.description = Some(description.to_owned());
        self
    }

    pub fn started_at(mut self, ms: u64) -> Self {
        self.started_at_ms = Some(ms);
        self
    }
}

#[derive(Default)]
struct MockState {
    windows: Vec<RawWindow>,
    processes: HashMap<u32, ProcessIdentity>,
    metadata: HashMap<String, ExeMetadata>,
    stamps: HashMap<String, FileStamp>,
    failing: HashMap<u32, DetectError>,
    foreground: Option<RawWindow>,
    fail_enumeration: bool,
    metadata_calls: u32,
    next_handle: u64,
}

/// A scriptable window system. Clones share state, so a test can keep a
/// handle while the detector owns another.
#[derive(Clone, Default)]
pub struct MockWindowSystem {
    state: Arc<Mutex<MockState>>,
    demo_started: Option<Instant>,
}

impl MockWindowSystem {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, MockState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn add(&self, app: MockApp) {
        let mut s = self.lock();
        for _ in 0..app.windows {
            s.next_handle += 1;
            let handle = s.next_handle;
            s.windows.push(RawWindow {
                handle,
                pid: app.pid,
                title: app.title.clone(),
            });
        }
        s.processes.insert(
            app.pid,
            ProcessIdentity {
                pid: app.pid,
                exe_path: app.path.clone(),
                started_at_ms: app.started_at_ms,
            },
        );
        let norm = normalize_path(&app.path);
        if let Some(description) = app.description {
            s.metadata.insert(
                norm.clone(),
                ExeMetadata {
                    file_description: Some(description),
                    product_name: None,
                },
            );
        }
        s.stamps.entry(norm).or_insert(FileStamp {
            size: 1,
            modified_ms: 1,
        });
    }

    pub fn remove_pid(&self, pid: u32) {
        let mut s = self.lock();
        s.windows.retain(|w| w.pid != pid);
        s.processes.remove(&pid);
        if s.foreground.as_ref().is_some_and(|w| w.pid == pid) {
            s.foreground = None;
        }
    }

    pub fn focus_pid(&self, pid: u32) {
        let mut s = self.lock();
        s.foreground = s.windows.iter().find(|w| w.pid == pid).cloned();
    }

    /// Focus a window that is not in the application list (e.g. our own window).
    pub fn focus_raw(&self, pid: u32) {
        self.lock().foreground = Some(RawWindow {
            handle: u64::MAX,
            pid,
            title: None,
        });
    }

    pub fn focus_none(&self) {
        self.lock().foreground = None;
    }

    pub fn fail_identity(&self, pid: u32, error: DetectError) {
        self.lock().failing.insert(pid, error);
    }

    pub fn fail_enumeration(&self, fail: bool) {
        self.lock().fail_enumeration = fail;
    }

    pub fn metadata_calls(&self) -> u32 {
        self.lock().metadata_calls
    }

    pub fn bump_stamp(&self, path: &str) {
        let mut s = self.lock();
        let stamp = s.stamps.entry(normalize_path(path)).or_insert(FileStamp {
            size: 1,
            modified_ms: 1,
        });
        stamp.modified_ms += 1;
    }

    /// Sample applications for UI development. Clearly fictional; the
    /// foreground rotates every eight seconds so the UI shows live changes.
    pub fn demo() -> Self {
        let ws = Self {
            state: Arc::default(),
            demo_started: Some(Instant::now()),
        };
        let samples = [
            (
                101,
                r"C:\Mock\Sample Editor\editor.exe",
                "Sample Editor (mock)",
                "notes.txt - Sample Editor",
            ),
            (
                102,
                r"C:\Mock\Sample Browser\browser.exe",
                "Sample Browser (mock)",
                "Start page - Sample Browser",
            ),
            (
                103,
                r"C:\Mock\Sample Music\music.exe",
                "Sample Music (mock)",
                "Sample Music",
            ),
            (
                104,
                r"C:\Mock\Sample Game\game.exe",
                "Sample Game (mock)",
                "Sample Game",
            ),
        ];
        for (pid, path, description, title) in samples {
            ws.add(MockApp::new(pid, path).description(description).title(title));
        }
        ws
    }
}

impl WindowSystem for MockWindowSystem {
    fn name(&self) -> &'static str {
        "mock"
    }

    fn is_mock(&self) -> bool {
        self.demo_started.is_some()
    }

    fn top_level_windows(&mut self, with_titles: bool) -> Result<Vec<RawWindow>, DetectError> {
        let s = self.lock();
        if s.fail_enumeration {
            return Err(DetectError::Os("mock enumeration failure".into()));
        }
        Ok(s.windows
            .iter()
            .map(|w| RawWindow {
                title: if with_titles { w.title.clone() } else { None },
                ..w.clone()
            })
            .collect())
    }

    fn foreground_window(&mut self, with_title: bool) -> Option<RawWindow> {
        if let Some(started) = self.demo_started {
            let s = self.lock();
            if s.windows.is_empty() {
                return None;
            }
            let index = (started.elapsed().as_secs() / 8) as usize % s.windows.len();
            let mut w = s.windows[index].clone();
            if !with_title {
                w.title = None;
            }
            return Some(w);
        }
        let s = self.lock();
        s.foreground.clone().map(|mut w| {
            if !with_title {
                w.title = None;
            }
            w
        })
    }

    fn process_identity(&mut self, pid: u32) -> Result<ProcessIdentity, DetectError> {
        let s = self.lock();
        if let Some(error) = s.failing.get(&pid) {
            return Err(error.clone());
        }
        s.processes.get(&pid).cloned().ok_or(DetectError::ProcessGone)
    }

    fn exe_metadata(&mut self, exe_path: &str) -> Option<ExeMetadata> {
        let mut s = self.lock();
        s.metadata_calls += 1;
        s.metadata.get(&normalize_path(exe_path)).cloned()
    }

    fn file_stamp(&mut self, exe_path: &str) -> Option<FileStamp> {
        self.lock().stamps.get(&normalize_path(exe_path)).copied()
    }
}
