//! Platform-independent detection logic: grouping, deduplication, metadata
//! caching, exit handling and foreground tracking.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::{Duration, Instant};

use super::foreground::ForegroundTracker;
use super::{
    DetectError, DetectedApp, DetectorSettings, DetectorSnapshot, DetectorStatus, ExitedApp, FileStamp,
    ScanStats, WindowSystem,
};
use crate::app_identity::{AppKey, ExeMetadata, exe_file_name};

/// Number of exited applications remembered for the "last seen" list.
pub const MAX_EXITED_HISTORY: usize = 50;

/// Windows shell and system processes that may own visible windows but are not
/// user applications. Compared against the lower-cased executable name.
/// `explorer.exe` is intentionally absent: File Explorer windows are real
/// applications, while the taskbar and desktop are filtered by window class.
const IGNORED_SYSTEM_EXECUTABLES: &[&str] = &[
    "applicationframehost.exe",
    "ctfmon.exe",
    "dwm.exe",
    "csrss.exe",
    "winlogon.exe",
    "lockapp.exe",
    "logonui.exe",
    "searchapp.exe",
    "searchhost.exe",
    "searchui.exe",
    "shellexperiencehost.exe",
    "shellhost.exe",
    "startmenuexperiencehost.exe",
    "textinputhost.exe",
    "sihost.exe",
    "taskhostw.exe",
    "runtimebroker.exe",
    "systemsettingsbroker.exe",
    "credentialuibroker.exe",
    "consent.exe",
    "smartscreen.exe",
    "msedgewebview2.exe",
    "widgets.exe",
    "widgetservice.exe",
    "gamebar.exe",
    "gamebarftserver.exe",
    "screenclippinghost.exe",
];

pub fn is_ignored_system_executable(exe_name_lower: &str) -> bool {
    IGNORED_SYSTEM_EXECUTABLES.contains(&exe_name_lower)
}

#[derive(Default)]
struct Accumulator {
    exe_path: String,
    pids: Vec<u32>,
    window_count: u32,
    title: Option<String>,
    started_at_ms: Option<u64>,
}

/// Owns the window system and all detection state. Not thread-safe by design:
/// it lives on the detector thread and publishes immutable snapshots.
pub struct Detector {
    ws: Box<dyn WindowSystem>,
    settings: DetectorSettings,
    tracker: ForegroundTracker,
    apps: BTreeMap<AppKey, DetectedApp>,
    pid_map: HashMap<u32, AppKey>,
    metadata_cache: HashMap<AppKey, (Option<FileStamp>, Option<ExeMetadata>)>,
    exited: VecDeque<ExitedApp>,
    status: DetectorStatus,
    stats: ScanStats,
    own_pid: u32,
    foreground_handle: Option<u64>,
    last_unknown_rescan: Option<(u32, Instant)>,
    identity_failures: u64,
}

impl Detector {
    pub fn new(ws: Box<dyn WindowSystem>, settings: DetectorSettings) -> Self {
        Self::with_own_pid(ws, settings, std::process::id())
    }

    pub fn with_own_pid(ws: Box<dyn WindowSystem>, settings: DetectorSettings, own_pid: u32) -> Self {
        let status = match ws.unsupported_reason() {
            Some(reason) => DetectorStatus::Unsupported { reason },
            None => DetectorStatus::Starting,
        };
        Self {
            tracker: ForegroundTracker::new(settings.foreground_debounce),
            ws,
            settings,
            apps: BTreeMap::new(),
            pid_map: HashMap::new(),
            metadata_cache: HashMap::new(),
            exited: VecDeque::new(),
            status,
            stats: ScanStats::default(),
            own_pid,
            foreground_handle: None,
            last_unknown_rescan: None,
            identity_failures: 0,
        }
    }

    pub fn is_supported(&self) -> bool {
        !matches!(self.status, DetectorStatus::Unsupported { .. })
    }

    pub fn settings(&self) -> DetectorSettings {
        self.settings
    }

    pub fn configure(&mut self, settings: DetectorSettings) {
        if settings.capture_titles != self.settings.capture_titles && !settings.capture_titles {
            // Privacy: drop any titles captured before the opt-out immediately.
            for app in self.apps.values_mut() {
                app.window_title = None;
            }
        }
        self.tracker.set_debounce(settings.foreground_debounce);
        self.settings = settings;
    }

    /// Full enumeration of visible windows and their processes.
    pub fn full_scan(&mut self, now_ms: u64) {
        if !self.is_supported() {
            return;
        }
        let started = Instant::now();
        let windows = match self.ws.top_level_windows(self.settings.capture_titles) {
            Ok(windows) => windows,
            Err(error) => {
                self.status = DetectorStatus::Degraded {
                    message: format!("Window enumeration failed: {error}"),
                };
                tracing::warn!(%error, "window enumeration failed");
                return;
            }
        };

        let mut seen: BTreeMap<AppKey, Accumulator> = BTreeMap::new();
        let mut pid_map = HashMap::new();
        let mut identities: HashMap<u32, Option<(AppKey, String, Option<u64>)>> = HashMap::new();

        for window in windows {
            if window.pid == 0 || window.pid == self.own_pid {
                continue;
            }
            let identity = identities.entry(window.pid).or_insert_with(|| {
                match self.ws.process_identity(window.pid) {
                    Ok(identity) => AppKey::from_path(&identity.exe_path)
                        .map(|key| (key, identity.exe_path, identity.started_at_ms)),
                    Err(error) => {
                        // The process may have exited between enumeration and
                        // lookup, or be protected. Skip it; never fail the scan.
                        self.identity_failures += 1;
                        if !matches!(error, DetectError::ProcessGone) {
                            tracing::debug!(pid = window.pid, %error, "process lookup failed");
                        }
                        None
                    }
                }
            });
            let Some((key, exe_path, started_at_ms)) = identity.clone() else {
                continue;
            };
            if is_ignored_system_executable(key.exe_name()) {
                continue;
            }
            pid_map.insert(window.pid, key.clone());
            let acc = seen.entry(key).or_default();
            if acc.exe_path.is_empty() {
                acc.exe_path = exe_path;
            }
            if !acc.pids.contains(&window.pid) {
                acc.pids.push(window.pid);
            }
            acc.window_count += 1;
            acc.started_at_ms = match (acc.started_at_ms, started_at_ms) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            // Windows arrive topmost-first; prefer the foreground window's title.
            let is_foreground = self.foreground_handle == Some(window.handle);
            if (acc.title.is_none() || is_foreground)
                && let Some(title) = window.title.filter(|t| !t.trim().is_empty())
            {
                acc.title = Some(title);
            }
        }

        // Applications that disappeared move to the exited history.
        let gone: Vec<AppKey> = self
            .apps
            .keys()
            .filter(|key| !seen.contains_key(*key))
            .cloned()
            .collect();
        for key in gone {
            if let Some(app) = self.apps.remove(&key) {
                self.remember_exited(app, now_ms);
            }
        }

        for (key, mut acc) in seen {
            acc.pids.sort_unstable();
            let title = if self.settings.capture_titles {
                acc.title
            } else {
                None
            };
            if let Some(app) = self.apps.get_mut(&key) {
                app.pids = acc.pids;
                app.window_count = acc.window_count;
                app.window_title = title;
                app.started_at_ms = acc.started_at_ms;
                continue;
            }
            self.exited.retain(|e| e.key != key);
            let metadata = self.metadata_for(&key, &acc.exe_path);
            let app = DetectedApp {
                exe_name: exe_file_name(&acc.exe_path),
                exe_path: acc.exe_path,
                key: key.clone(),
                metadata,
                pids: acc.pids,
                window_count: acc.window_count,
                window_title: title,
                started_at_ms: acc.started_at_ms,
                first_seen_ms: now_ms,
                last_focused_ms: None,
            };
            self.apps.insert(key, app);
        }

        self.pid_map = pid_map;
        let apps = &self.apps;
        self.tracker.forget_missing(|key| apps.contains_key(key));
        // Drop cached metadata for executables not seen for a while.
        if self.metadata_cache.len() > 512 {
            let apps = &self.apps;
            self.metadata_cache.retain(|key, _| apps.contains_key(key));
        }

        if !matches!(self.status, DetectorStatus::Mock) {
            self.status = if self.ws.is_mock() {
                DetectorStatus::Mock
            } else {
                DetectorStatus::Running {
                    backend: self.ws.name().to_owned(),
                }
            };
        }

        let elapsed = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        self.stats.scans += 1;
        self.stats.last_scan_at_ms = now_ms;
        self.stats.last_scan_duration_us = elapsed;
        self.stats.max_scan_duration_us = self.stats.max_scan_duration_us.max(elapsed);
    }

    /// Cheap foreground check, run several times per second. Triggers a full
    /// scan when an unknown process takes the foreground (a new application).
    pub fn foreground_tick(&mut self, now: Instant, now_ms: u64) {
        if !self.is_supported() {
            return;
        }
        let window = self.ws.foreground_window(self.settings.capture_titles);
        self.foreground_handle = window.as_ref().map(|w| w.handle);

        let mut observed = window.as_ref().and_then(|w| self.pid_map.get(&w.pid).cloned());
        if observed.is_none()
            && let Some(w) = &window
            && w.pid != 0
            && w.pid != self.own_pid
        {
            let recently = self.last_unknown_rescan.is_some_and(|(pid, at)| {
                pid == w.pid && now.saturating_duration_since(at) < self.settings.poll_interval
            });
            if !recently {
                self.last_unknown_rescan = Some((w.pid, now));
                self.full_scan(now_ms);
                observed = self.pid_map.get(&w.pid).cloned();
            }
        }

        let previous = self.tracker.committed().cloned();
        if self.tracker.observe(observed.as_ref(), now) {
            if let Some(prev) = previous.and_then(|k| self.apps.get_mut(&k)) {
                prev.last_focused_ms = Some(now_ms);
            }
            if let Some(current) = self.tracker.committed().cloned()
                && let Some(app) = self.apps.get_mut(&current)
            {
                app.last_focused_ms = Some(now_ms);
            }
        }

        // Keep the foreground app's title current (only with the opt-in).
        if self.settings.capture_titles
            && let (Some(w), Some(committed)) = (window, self.tracker.committed())
            && observed.as_ref() == Some(committed)
            && let Some(app) = self.apps.get_mut(committed)
        {
            let title = w.title.filter(|t| !t.trim().is_empty());
            if title.is_some() && app.window_title != title {
                app.window_title = title;
            }
        }
    }

    pub fn snapshot(&self) -> DetectorSnapshot {
        DetectorSnapshot {
            status: self.status.clone(),
            apps: self.apps.values().cloned().collect(),
            exited: self.exited.iter().cloned().collect(),
            foreground: self.tracker.committed().cloned(),
            stats: self.stats,
        }
    }

    pub fn mark_degraded(&mut self, message: String) {
        self.status = DetectorStatus::Degraded { message };
    }

    pub fn mark_stopped(&mut self) {
        self.status = DetectorStatus::Stopped;
    }

    pub fn identity_failures(&self) -> u64 {
        self.identity_failures
    }

    fn remember_exited(&mut self, app: DetectedApp, now_ms: u64) {
        self.exited.retain(|e| e.key != app.key);
        self.exited.push_front(ExitedApp {
            key: app.key,
            exe_path: app.exe_path,
            exe_name: app.exe_name,
            metadata: app.metadata,
            last_seen_ms: now_ms,
        });
        self.exited.truncate(MAX_EXITED_HISTORY);
    }

    fn metadata_for(&mut self, key: &AppKey, exe_path: &str) -> Option<ExeMetadata> {
        let stamp = self.ws.file_stamp(exe_path);
        if let Some((cached_stamp, metadata)) = self.metadata_cache.get(key)
            && *cached_stamp == stamp
        {
            return metadata.clone();
        }
        let metadata = self.ws.exe_metadata(exe_path);
        self.metadata_cache.insert(key.clone(), (stamp, metadata.clone()));
        metadata
    }
}

/// Interval between foreground checks.
pub(super) fn foreground_tick_interval(settings: &DetectorSettings) -> Duration {
    Duration::from_millis(250).min(settings.poll_interval)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_detection::mock::{MockApp, MockWindowSystem};

    const OWN_PID: u32 = 4242;

    fn settings(debounce_ms: u64) -> DetectorSettings {
        DetectorSettings {
            poll_interval: Duration::from_millis(1000),
            foreground_debounce: Duration::from_millis(debounce_ms),
            capture_titles: false,
        }
    }

    fn detector(ws: &MockWindowSystem, debounce_ms: u64) -> Detector {
        Detector::with_own_pid(Box::new(ws.clone()), settings(debounce_ms), OWN_PID)
    }

    fn key(path: &str) -> AppKey {
        AppKey::from_path(path).unwrap()
    }

    #[test]
    fn groups_windows_and_processes_by_executable() {
        let ws = MockWindowSystem::new();
        ws.add(
            MockApp::new(10, r"C:\Apps\Browser\browser.exe")
                .windows(3)
                .description("Web Browser"),
        );
        ws.add(MockApp::new(11, r"C:\Apps\Browser\browser.exe").windows(1));
        ws.add(MockApp::new(20, r"C:\Apps\Editor\editor.exe"));
        let mut d = detector(&ws, 0);
        d.full_scan(1_000);
        let snap = d.snapshot();
        assert_eq!(snap.apps.len(), 2);
        let browser = snap.app(&key(r"c:\apps\browser\browser.exe")).unwrap();
        assert_eq!(browser.pids, vec![10, 11]);
        assert_eq!(browser.window_count, 4);
        assert_eq!(browser.exe_name, "browser.exe");
        assert_eq!(
            browser
                .metadata
                .as_ref()
                .and_then(|m| m.file_description.as_deref()),
            Some("Web Browser")
        );
        assert!(matches!(snap.status, DetectorStatus::Running { .. }));
    }

    #[test]
    fn excludes_own_process_system_processes_and_unidentifiable_ones() {
        let ws = MockWindowSystem::new();
        ws.add(MockApp::new(
            OWN_PID,
            r"C:\Program Files\DeskPresence\deskpresence.exe",
        ));
        ws.add(MockApp::new(30, r"C:\Windows\System32\TextInputHost.exe"));
        ws.add(MockApp::new(31, r"C:\Apps\real.exe"));
        ws.add(MockApp::new(32, r"C:\Apps\protected.exe"));
        ws.fail_identity(32, DetectError::AccessDenied);
        let mut d = detector(&ws, 0);
        d.full_scan(0);
        let snap = d.snapshot();
        let keys: Vec<&str> = snap.apps.iter().map(|a| a.key.as_str()).collect();
        assert_eq!(keys, vec![r"c:\apps\real.exe"]);
        assert_eq!(d.identity_failures(), 1);
    }

    #[test]
    fn process_exiting_mid_scan_is_skipped() {
        let ws = MockWindowSystem::new();
        ws.add(MockApp::new(40, r"C:\Apps\quick.exe"));
        ws.add(MockApp::new(41, r"C:\Apps\stays.exe"));
        // Window enumerated, but the process is gone when queried.
        ws.fail_identity(40, DetectError::ProcessGone);
        let mut d = detector(&ws, 0);
        d.full_scan(0);
        assert_eq!(d.snapshot().apps.len(), 1);
    }

    #[test]
    fn exited_apps_move_to_history_and_return_when_relaunched() {
        let ws = MockWindowSystem::new();
        ws.add(MockApp::new(50, r"C:\Apps\game.exe"));
        let mut d = detector(&ws, 0);
        d.full_scan(1_000);
        ws.remove_pid(50);
        d.full_scan(2_000);
        let snap = d.snapshot();
        assert!(snap.apps.is_empty());
        assert_eq!(snap.exited.len(), 1);
        assert_eq!(snap.exited[0].last_seen_ms, 2_000);

        ws.add(MockApp::new(51, r"C:\Apps\game.exe"));
        d.full_scan(3_000);
        let snap = d.snapshot();
        assert_eq!(snap.apps.len(), 1);
        assert!(snap.exited.is_empty());
    }

    #[test]
    fn foreground_is_debounced_and_cleared_when_app_exits() {
        let ws = MockWindowSystem::new();
        ws.add(MockApp::new(60, r"C:\Apps\a.exe"));
        ws.add(MockApp::new(61, r"C:\Apps\b.exe"));
        let mut d = detector(&ws, 500);
        let t0 = Instant::now();
        d.full_scan(0);
        ws.focus_pid(60);
        d.foreground_tick(t0, 0);
        assert_eq!(d.snapshot().foreground, None);
        d.foreground_tick(t0 + Duration::from_millis(500), 500);
        assert_eq!(d.snapshot().foreground, Some(key(r"C:\Apps\a.exe")));

        // Focus moves to the taskbar (no application): A stays committed.
        ws.focus_none();
        d.foreground_tick(t0 + Duration::from_millis(1000), 1000);
        assert_eq!(d.snapshot().foreground, Some(key(r"C:\Apps\a.exe")));

        // Focus moves to our own window: still A.
        ws.focus_raw(OWN_PID);
        d.foreground_tick(t0 + Duration::from_millis(1500), 1500);
        assert_eq!(d.snapshot().foreground, Some(key(r"C:\Apps\a.exe")));

        ws.remove_pid(60);
        d.full_scan(2000);
        assert_eq!(d.snapshot().foreground, None);
    }

    #[test]
    fn focus_history_is_recorded() {
        let ws = MockWindowSystem::new();
        ws.add(MockApp::new(70, r"C:\Apps\a.exe"));
        ws.add(MockApp::new(71, r"C:\Apps\b.exe"));
        let mut d = detector(&ws, 0);
        let t0 = Instant::now();
        d.full_scan(0);
        ws.focus_pid(70);
        d.foreground_tick(t0, 100);
        ws.focus_pid(71);
        d.foreground_tick(t0, 200);
        let snap = d.snapshot();
        assert_eq!(
            snap.app(&key(r"C:\Apps\a.exe")).unwrap().last_focused_ms,
            Some(200)
        );
        assert_eq!(
            snap.app(&key(r"C:\Apps\b.exe")).unwrap().last_focused_ms,
            Some(200)
        );
        assert_eq!(snap.foreground, Some(key(r"C:\Apps\b.exe")));
    }

    #[test]
    fn new_app_taking_focus_triggers_a_rescan() {
        let ws = MockWindowSystem::new();
        ws.add(MockApp::new(80, r"C:\Apps\a.exe"));
        let mut d = detector(&ws, 0);
        let t0 = Instant::now();
        d.full_scan(0);
        ws.add(MockApp::new(81, r"C:\Apps\new.exe"));
        ws.focus_pid(81);
        d.foreground_tick(t0 + Duration::from_millis(100), 100);
        let snap = d.snapshot();
        assert_eq!(snap.apps.len(), 2);
        assert_eq!(snap.foreground, Some(key(r"C:\Apps\new.exe")));
    }

    #[test]
    fn metadata_is_cached_per_executable() {
        let ws = MockWindowSystem::new();
        ws.add(MockApp::new(90, r"C:\Apps\a.exe").description("A"));
        let mut d = detector(&ws, 0);
        for i in 0..5 {
            d.full_scan(i);
        }
        assert_eq!(ws.metadata_calls(), 1);
        // Relaunch after exit reuses the cache when the file is unchanged.
        ws.remove_pid(90);
        d.full_scan(10);
        ws.add(MockApp::new(91, r"C:\Apps\a.exe").description("A"));
        d.full_scan(11);
        assert_eq!(ws.metadata_calls(), 1);
        // A changed file stamp (app updated) re-reads metadata.
        ws.remove_pid(91);
        d.full_scan(12);
        ws.bump_stamp(r"C:\Apps\a.exe");
        ws.add(MockApp::new(92, r"C:\Apps\a.exe").description("A v2"));
        d.full_scan(13);
        assert_eq!(ws.metadata_calls(), 2);
    }

    #[test]
    fn titles_are_only_kept_with_opt_in() {
        let ws = MockWindowSystem::new();
        ws.add(MockApp::new(100, r"C:\Apps\a.exe").title("Secret document"));
        let mut d = detector(&ws, 0);
        d.full_scan(0);
        assert_eq!(d.snapshot().apps[0].window_title, None);

        let mut s = d.settings();
        s.capture_titles = true;
        d.configure(s);
        d.full_scan(1);
        assert_eq!(
            d.snapshot().apps[0].window_title.as_deref(),
            Some("Secret document")
        );

        s.capture_titles = false;
        d.configure(s);
        assert_eq!(d.snapshot().apps[0].window_title, None);
    }

    #[test]
    fn unchanged_scans_produce_equal_snapshots() {
        let ws = MockWindowSystem::new();
        ws.add(MockApp::new(110, r"C:\Apps\a.exe"));
        let mut d = detector(&ws, 0);
        d.full_scan(0);
        let a = d.snapshot();
        d.full_scan(1000);
        let b = d.snapshot();
        assert!(a.meaningfully_equals(&b));
        assert_ne!(a.stats.scans, b.stats.scans);
    }

    #[test]
    fn enumeration_failure_degrades_without_losing_state() {
        let ws = MockWindowSystem::new();
        ws.add(MockApp::new(120, r"C:\Apps\a.exe"));
        let mut d = detector(&ws, 0);
        d.full_scan(0);
        ws.fail_enumeration(true);
        d.full_scan(1);
        let snap = d.snapshot();
        assert!(matches!(snap.status, DetectorStatus::Degraded { .. }));
        assert_eq!(snap.apps.len(), 1);
        ws.fail_enumeration(false);
        d.full_scan(2);
        assert!(matches!(d.snapshot().status, DetectorStatus::Running { .. }));
    }
}
