//! Visible-application detection.
//!
//! The [`Detector`] is platform independent: it groups windows by executable,
//! tracks the debounced foreground application and keeps a short history of
//! applications that exited. Operating-system access goes through the
//! [`WindowSystem`] trait, implemented with Win32 APIs on Windows
//! (`windows.rs`), by a deterministic mock for tests, and by an "unsupported"
//! stub elsewhere.

mod detector;
mod foreground;
pub mod mock;
mod thread;
#[cfg(windows)]
mod windows;

use serde::{Deserialize, Serialize};

use crate::app_identity::{AppKey, ExeMetadata};

pub use detector::{Detector, MAX_EXITED_HISTORY, is_ignored_system_executable};
pub use foreground::ForegroundTracker;
pub use thread::{DetectorController, DetectorHandle};

/// A visible, user-facing top-level window as reported by the platform.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawWindow {
    /// Opaque window handle value, used only for identity within one scan.
    pub handle: u64,
    /// Owning process. For UWP apps this is the hosted app, not the frame host.
    pub pid: u32,
    /// Window title, only present when title capture is enabled.
    pub title: Option<String>,
}

/// Process facts needed to identify an application.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub exe_path: String,
    /// Process creation time (Unix ms), used to detect PID reuse and for the
    /// "since launch" timestamp mode.
    pub started_at_ms: Option<u64>,
}

/// Size and modification time of an executable, used to invalidate caches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FileStamp {
    pub size: u64,
    pub modified_ms: u64,
}

impl FileStamp {
    pub fn of(path: &str) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        let modified_ms = meta
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_millis();
        Some(Self {
            size: meta.len(),
            modified_ms: u64::try_from(modified_ms).unwrap_or(u64::MAX),
        })
    }
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum DetectError {
    #[error("the process has exited")]
    ProcessGone,
    #[error("access to the process was denied")]
    AccessDenied,
    #[error("{0}")]
    Os(String),
}

/// Operating-system access needed by the detector.
pub trait WindowSystem: Send {
    /// Short name shown in diagnostics.
    fn name(&self) -> &'static str;

    /// `Some(reason)` when detection is not available on this platform.
    fn unsupported_reason(&self) -> Option<String> {
        None
    }

    /// `true` for development mocks; surfaced in the UI so mock data is never
    /// mistaken for real detection.
    fn is_mock(&self) -> bool {
        false
    }

    /// Visible, user-facing top-level windows in z-order (topmost first).
    /// Shell surfaces (taskbar, desktop, Start), cloaked and tool windows are excluded.
    fn top_level_windows(&mut self, with_titles: bool) -> Result<Vec<RawWindow>, DetectError>;

    /// The current foreground window, resolved to its root owner, or `None`.
    fn foreground_window(&mut self, with_title: bool) -> Option<RawWindow>;

    /// Executable path and start time for a process.
    fn process_identity(&mut self, pid: u32) -> Result<ProcessIdentity, DetectError>;

    /// Version-resource metadata. Called once per executable version.
    fn exe_metadata(&mut self, exe_path: &str) -> Option<ExeMetadata>;

    fn file_stamp(&mut self, exe_path: &str) -> Option<FileStamp> {
        FileStamp::of(exe_path)
    }
}

/// Builds the window system for the current platform.
///
/// Setting the environment variable `DESKPRESENCE_MOCK_DETECTOR=1` selects a
/// clearly labelled mock with sample applications, intended only for UI
/// development on machines where real detection is unavailable.
pub fn platform_window_system() -> Box<dyn WindowSystem> {
    if std::env::var_os("DESKPRESENCE_MOCK_DETECTOR").is_some_and(|v| v == "1") {
        return Box::new(mock::MockWindowSystem::demo());
    }
    #[cfg(windows)]
    {
        Box::new(windows::Win32WindowSystem::new())
    }
    #[cfg(not(windows))]
    {
        Box::new(mock::UnsupportedWindowSystem)
    }
}

/// Runtime knobs for the detector, derived from configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DetectorSettings {
    pub poll_interval: std::time::Duration,
    pub foreground_debounce: std::time::Duration,
    pub capture_titles: bool,
}

impl Default for DetectorSettings {
    fn default() -> Self {
        Self {
            poll_interval: std::time::Duration::from_millis(1000),
            foreground_debounce: std::time::Duration::from_millis(500),
            capture_titles: false,
        }
    }
}

/// Detector health, shown in Overview and Diagnostics.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DetectorStatus {
    Starting,
    Running {
        backend: String,
    },
    /// Running on mock data (development only).
    Mock,
    Unsupported {
        reason: String,
    },
    /// Detection is running but the last scan failed.
    Degraded {
        message: String,
    },
    Stopped,
}

/// A running application, aggregated over all of its visible windows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetectedApp {
    pub key: AppKey,
    pub exe_path: String,
    pub exe_name: String,
    pub metadata: Option<ExeMetadata>,
    /// Processes that own visible windows, sorted.
    pub pids: Vec<u32>,
    pub window_count: u32,
    /// Title of the most relevant window (foreground or topmost). Only
    /// populated when title capture is enabled.
    pub window_title: Option<String>,
    /// Earliest start time among the owning processes.
    pub started_at_ms: Option<u64>,
    pub first_seen_ms: u64,
    /// Last time the application stopped being the foreground application,
    /// or became it. `None` if never focused this session.
    pub last_focused_ms: Option<u64>,
}

/// An application that was visible earlier in this session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExitedApp {
    pub key: AppKey,
    pub exe_path: String,
    pub exe_name: String,
    pub metadata: Option<ExeMetadata>,
    pub last_seen_ms: u64,
}

/// Timing statistics (not part of change detection).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ScanStats {
    pub scans: u64,
    pub last_scan_at_ms: u64,
    pub last_scan_duration_us: u64,
    pub max_scan_duration_us: u64,
}

/// What the detector publishes after each meaningful change.
#[derive(Clone, Debug)]
pub struct DetectorSnapshot {
    pub status: DetectorStatus,
    /// Running applications sorted by key.
    pub apps: Vec<DetectedApp>,
    /// Recently exited applications, most recent first.
    pub exited: Vec<ExitedApp>,
    /// Debounced foreground application.
    pub foreground: Option<AppKey>,
    pub stats: ScanStats,
}

impl Default for DetectorSnapshot {
    fn default() -> Self {
        Self {
            status: DetectorStatus::Starting,
            apps: Vec::new(),
            exited: Vec::new(),
            foreground: None,
            stats: ScanStats::default(),
        }
    }
}

impl DetectorSnapshot {
    /// Equality that ignores timing statistics, used to suppress duplicate events.
    pub fn meaningfully_equals(&self, other: &DetectorSnapshot) -> bool {
        self.status == other.status
            && self.apps == other.apps
            && self.exited == other.exited
            && self.foreground == other.foreground
    }

    pub fn app(&self, key: &AppKey) -> Option<&DetectedApp> {
        self.apps
            .binary_search_by(|a| a.key.cmp(key))
            .ok()
            .map(|i| &self.apps[i])
    }
}
