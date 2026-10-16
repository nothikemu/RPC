//! In-app diagnostic events, log throttling and sanitized exports.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use serde::Serialize;

/// Severity of a diagnostic event shown in the UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum EventLevel {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum EventCategory {
    App,
    Config,
    Detector,
    Discord,
    Presence,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct DiagnosticEvent {
    pub id: u64,
    pub at_ms: u64,
    pub level: EventLevel,
    pub category: EventCategory,
    pub message: String,
    /// How many consecutive times this event occurred (≥ 1).
    pub count: u32,
}

/// Bounded ring buffer of recent events. Consecutive duplicates are folded
/// into one entry with a count, so a recurring failure cannot flood the list.
#[derive(Debug)]
pub struct EventLog {
    events: VecDeque<DiagnosticEvent>,
    capacity: usize,
    next_id: u64,
}

impl EventLog {
    pub fn new(capacity: usize) -> Self {
        Self {
            events: VecDeque::new(),
            capacity: capacity.max(1),
            next_id: 1,
        }
    }

    pub fn push(
        &mut self,
        level: EventLevel,
        category: EventCategory,
        message: impl Into<String>,
        at_ms: u64,
    ) {
        let message = message.into();
        if let Some(last) = self.events.front_mut()
            && last.level == level
            && last.category == category
            && last.message == message
        {
            last.count = last.count.saturating_add(1);
            last.at_ms = at_ms;
            return;
        }
        self.events.push_front(DiagnosticEvent {
            id: self.next_id,
            at_ms,
            level,
            category,
            message,
            count: 1,
        });
        self.next_id += 1;
        self.events.truncate(self.capacity);
    }

    /// Newest first.
    pub fn recent(&self, limit: usize) -> Vec<DiagnosticEvent> {
        self.events.iter().take(limit).cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

/// Decides whether a repeated log message should be written: the first
/// occurrence always is, repeats at most once per `interval`, with a count
/// of what was suppressed in between.
#[derive(Debug)]
pub struct LogThrottle {
    interval: Duration,
    seen: HashMap<String, (Instant, u32)>,
}

impl LogThrottle {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            seen: HashMap::new(),
        }
    }

    /// `Some(suppressed)` when the message should be logged now.
    pub fn check(&mut self, key: &str, now: Instant) -> Option<u32> {
        if self.seen.len() > 256 {
            self.seen.clear();
        }
        match self.seen.get_mut(key) {
            Some((last, suppressed)) if now.saturating_duration_since(*last) < self.interval => {
                *suppressed += 1;
                None
            }
            Some((last, suppressed)) => {
                let count = *suppressed;
                *last = now;
                *suppressed = 0;
                Some(count)
            }
            None => {
                self.seen.insert(key.to_owned(), (now, 0));
                Some(0)
            }
        }
    }
}

/// Removes user-specific information from text before it leaves the machine.
#[derive(Clone, Debug, Default)]
pub struct Redactor {
    home: Option<String>,
}

impl Redactor {
    pub fn from_env() -> Self {
        let home = std::env::var("USERPROFILE")
            .ok()
            .or_else(|| std::env::var("HOME").ok())
            .filter(|h| h.len() > 3);
        Self { home }
    }

    pub fn with_home(home: Option<&str>) -> Self {
        Self {
            home: home.map(str::to_owned),
        }
    }

    /// Replaces the profile directory and any `\Users\<name>` or
    /// `/home/<name>` segment with a placeholder.
    pub fn redact(&self, text: &str) -> String {
        let mut out = text.to_owned();
        if let Some(home) = &self.home {
            out = replace_case_insensitive(&out, home, "%USERPROFILE%");
            let forward = home.replace('\\', "/");
            if forward != *home {
                out = replace_case_insensitive(&out, &forward, "%USERPROFILE%");
            }
        }
        for marker in ["\\users\\", "/users/", "/home/"] {
            out = redact_segment_after(&out, marker);
        }
        out
    }
}

fn replace_case_insensitive(haystack: &str, needle: &str, replacement: &str) -> String {
    if needle.is_empty() {
        return haystack.to_owned();
    }
    let lower_hay = haystack.to_lowercase();
    let lower_needle = needle.to_lowercase();
    // Lower-casing can change byte lengths for some scripts; fall back to an
    // exact match in that case rather than risk slicing mid-character.
    if lower_hay.len() != haystack.len() || lower_needle.len() != needle.len() {
        return haystack.replace(needle, replacement);
    }
    let mut out = String::with_capacity(haystack.len());
    let mut i = 0;
    while let Some(pos) = lower_hay[i..].find(&lower_needle) {
        let start = i + pos;
        out.push_str(&haystack[i..start]);
        out.push_str(replacement);
        i = start + needle.len();
    }
    out.push_str(&haystack[i..]);
    out
}

/// Replaces the path segment following `marker` (case-insensitive) with `<user>`.
fn redact_segment_after(text: &str, marker: &str) -> String {
    let lower = text.to_lowercase();
    if lower.len() != text.len() {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while let Some(pos) = lower[i..].find(marker) {
        let seg_start = i + pos + marker.len();
        out.push_str(&text[i..seg_start]);
        let rest = &text[seg_start..];
        let seg_len = rest
            .find(['\\', '/', '"', '\'', ' ', '\n', ';', ','])
            .unwrap_or(rest.len());
        let segment = &rest[..seg_len];
        if segment.is_empty() || segment.eq_ignore_ascii_case("public") || segment == "<user>" {
            out.push_str(segment);
        } else {
            out.push_str("<user>");
        }
        i = seg_start + seg_len;
    }
    out.push_str(&text[i..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_events_are_folded() {
        let mut log = EventLog::new(3);
        log.push(
            EventLevel::Warning,
            EventCategory::Discord,
            "Discord is not running",
            1,
        );
        log.push(
            EventLevel::Warning,
            EventCategory::Discord,
            "Discord is not running",
            2,
        );
        log.push(
            EventLevel::Warning,
            EventCategory::Discord,
            "Discord is not running",
            3,
        );
        assert_eq!(log.len(), 1);
        let e = &log.recent(10)[0];
        assert_eq!((e.count, e.at_ms), (3, 3));
    }

    #[test]
    fn event_log_is_bounded_and_newest_first() {
        let mut log = EventLog::new(3);
        for i in 0..10 {
            log.push(EventLevel::Info, EventCategory::App, format!("event {i}"), i);
        }
        let recent = log.recent(10);
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].message, "event 9");
        assert!(recent[0].id > recent[1].id);
    }

    #[test]
    fn throttle_limits_repeats() {
        let mut t = LogThrottle::new(Duration::from_secs(60));
        let now = Instant::now();
        assert_eq!(t.check("enum failed", now), Some(0));
        for i in 1..100 {
            assert_eq!(t.check("enum failed", now + Duration::from_millis(i * 100)), None);
        }
        assert_eq!(t.check("other", now), Some(0));
        assert_eq!(t.check("enum failed", now + Duration::from_secs(61)), Some(99));
    }

    #[test]
    fn redacts_profile_and_user_segments() {
        let r = Redactor::with_home(Some(r"C:\Users\Alice"));
        assert_eq!(
            r.redact(r"failed to open c:\users\alice\AppData\x.exe"),
            r"failed to open %USERPROFILE%\AppData\x.exe"
        );
        assert_eq!(
            r.redact(r"D:\Users\Bob\Games\game.exe and C:/Users/Carol/x"),
            r"D:\Users\<user>\Games\game.exe and C:/Users/<user>/x"
        );
        assert_eq!(r.redact(r"C:\Users\Public\x.exe"), r"C:\Users\Public\x.exe");
        assert_eq!(r.redact("/home/dave/.config"), "/home/<user>/.config");
        assert_eq!(
            r.redact(r"C:\Program Files\App\app.exe"),
            r"C:\Program Files\App\app.exe"
        );
    }
}
