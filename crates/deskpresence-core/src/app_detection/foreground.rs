//! Debounced foreground tracking.
//!
//! A new foreground application only takes over after it has stayed in the
//! foreground for the debounce period. Observations of windows that do not
//! belong to a detected application (the taskbar, the desktop, DeskPresence's
//! own window, transient system popups) are ignored: the previously committed
//! application stays committed. This prevents opening the tray menu from
//! clearing or switching the presence.

use std::time::{Duration, Instant};

use crate::app_identity::AppKey;

#[derive(Debug, Clone)]
pub struct ForegroundTracker {
    debounce: Duration,
    committed: Option<AppKey>,
    candidate: Option<(AppKey, Instant)>,
}

impl ForegroundTracker {
    pub fn new(debounce: Duration) -> Self {
        Self {
            debounce,
            committed: None,
            candidate: None,
        }
    }

    pub fn set_debounce(&mut self, debounce: Duration) {
        self.debounce = debounce;
    }

    pub fn committed(&self) -> Option<&AppKey> {
        self.committed.as_ref()
    }

    /// Feeds one observation. `None` means "not an application window".
    /// Returns `true` when the committed application changed.
    pub fn observe(&mut self, observed: Option<&AppKey>, now: Instant) -> bool {
        let Some(key) = observed else {
            // Transient/shell window: keep the committed app, drop any pending switch.
            self.candidate = None;
            return false;
        };
        if self.committed.as_ref() == Some(key) {
            self.candidate = None;
            return false;
        }
        match &self.candidate {
            Some((pending, since)) if pending == key => {
                if now.saturating_duration_since(*since) >= self.debounce {
                    self.committed = Some(key.clone());
                    self.candidate = None;
                    true
                } else {
                    false
                }
            }
            _ => {
                if self.debounce.is_zero() {
                    self.committed = Some(key.clone());
                    self.candidate = None;
                    true
                } else {
                    self.candidate = Some((key.clone(), now));
                    false
                }
            }
        }
    }

    /// Drops the committed or pending application if it is no longer running.
    /// Returns `true` when the committed application changed.
    pub fn forget_missing(&mut self, is_running: impl Fn(&AppKey) -> bool) -> bool {
        if self.candidate.as_ref().is_some_and(|(key, _)| !is_running(key)) {
            self.candidate = None;
        }
        if self.committed.as_ref().is_some_and(|key| !is_running(key)) {
            self.committed = None;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str) -> AppKey {
        AppKey::from_path(&format!(r"C:\Apps\{name}.exe")).unwrap()
    }

    #[test]
    fn commits_only_after_debounce() {
        let mut t = ForegroundTracker::new(Duration::from_millis(500));
        let start = Instant::now();
        let a = key("a");
        assert!(!t.observe(Some(&a), start));
        assert!(!t.observe(Some(&a), start + Duration::from_millis(250)));
        assert!(t.observe(Some(&a), start + Duration::from_millis(500)));
        assert_eq!(t.committed(), Some(&a));
    }

    #[test]
    fn rapid_switches_do_not_commit_transient_apps() {
        let mut t = ForegroundTracker::new(Duration::from_millis(500));
        let start = Instant::now();
        let (a, b, c) = (key("a"), key("b"), key("c"));
        t.observe(Some(&a), start);
        t.observe(Some(&a), start + Duration::from_millis(600));
        assert_eq!(t.committed(), Some(&a));

        // B flashes for 200 ms, C for 200 ms, then back to A.
        assert!(!t.observe(Some(&b), start + Duration::from_millis(700)));
        assert!(!t.observe(Some(&c), start + Duration::from_millis(900)));
        assert!(!t.observe(Some(&b), start + Duration::from_millis(1100)));
        assert!(!t.observe(Some(&a), start + Duration::from_millis(1300)));
        assert!(!t.observe(Some(&b), start + Duration::from_millis(1500)));
        assert_eq!(t.committed(), Some(&a));
        // B then stays long enough.
        assert!(t.observe(Some(&b), start + Duration::from_millis(2000)));
        assert_eq!(t.committed(), Some(&b));
    }

    #[test]
    fn ignored_windows_keep_the_committed_app() {
        let mut t = ForegroundTracker::new(Duration::from_millis(100));
        let start = Instant::now();
        let a = key("a");
        t.observe(Some(&a), start);
        t.observe(Some(&a), start + Duration::from_millis(100));
        assert!(!t.observe(None, start + Duration::from_millis(200)));
        assert!(!t.observe(None, start + Duration::from_secs(10)));
        assert_eq!(t.committed(), Some(&a));
    }

    #[test]
    fn ignored_window_resets_pending_switch() {
        let mut t = ForegroundTracker::new(Duration::from_millis(500));
        let start = Instant::now();
        let (a, b) = (key("a"), key("b"));
        t.observe(Some(&a), start);
        t.observe(Some(&a), start + Duration::from_millis(500));
        t.observe(Some(&b), start + Duration::from_millis(600));
        t.observe(None, start + Duration::from_millis(800));
        // B must be observed continuously for the full debounce again.
        assert!(!t.observe(Some(&b), start + Duration::from_millis(1100)));
        assert!(t.observe(Some(&b), start + Duration::from_millis(1600)));
    }

    #[test]
    fn zero_debounce_commits_immediately() {
        let mut t = ForegroundTracker::new(Duration::ZERO);
        let a = key("a");
        assert!(t.observe(Some(&a), Instant::now()));
    }

    #[test]
    fn exited_app_is_forgotten() {
        let mut t = ForegroundTracker::new(Duration::ZERO);
        let a = key("a");
        t.observe(Some(&a), Instant::now());
        assert!(!t.forget_missing(|_| true));
        assert!(t.forget_missing(|_| false));
        assert_eq!(t.committed(), None);
    }
}
