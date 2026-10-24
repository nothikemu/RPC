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

fn chars_eq_ignore_case(a: char, b: char) -> bool {
    a == b || a.to_lowercase().eq(b.to_lowercase())
}

/// Finds `needle` in `haystack[from..]`, comparing character by character
/// without regard to case. Returns byte offsets into `haystack`, always on
/// character boundaries (lower-casing whole strings would shift offsets for
/// characters whose lower-case form has a different length, such as `İ`).
fn find_ignore_case(haystack: &str, needle: &[char], from: usize) -> Option<(usize, usize)> {
    if needle.is_empty() {
        return None;
    }
    let rest = haystack.get(from..)?;
    rest.char_indices().find_map(|(offset, _)| {
        let start = from + offset;
        let mut candidate = haystack[start..].char_indices();
        let mut end = start;
        for &expected in needle {
            match candidate.next() {
                Some((i, c)) if chars_eq_ignore_case(c, expected) => end = start + i + c.len_utf8(),
                _ => return None,
            }
        }
        Some((start, end))
    })
}

fn replace_case_insensitive(haystack: &str, needle: &str, replacement: &str) -> String {
    let needle: Vec<char> = needle.chars().collect();
    let mut out = String::with_capacity(haystack.len());
    let mut i = 0;
    while let Some((start, end)) = find_ignore_case(haystack, &needle, i) {
        out.push_str(&haystack[i..start]);
        out.push_str(replacement);
        i = end;
    }
    out.push_str(&haystack[i..]);
    out
}

/// Replaces the path segment following `marker` (case-insensitive) with `<user>`.
fn redact_segment_after(text: &str, marker: &str) -> String {
    let marker: Vec<char> = marker.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while let Some((_, seg_start)) = find_ignore_case(text, &marker, i) {
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

    #[test]
    fn characters_whose_case_changes_length_neither_panic_nor_leak() {
        // `İ` lower-cases to two characters (3 bytes) and `Ω` (OHM SIGN, 3
        // bytes) to `ω` (2 bytes).
        let r = Redactor::with_home(Some(r"C:\Users\İsmail"));
        assert_eq!(
            r.redact(r"c:\Users\İsmail\Games\x.exe"),
            r"%USERPROFILE%\Games\x.exe"
        );
        assert_eq!(r.redact("İC:\\Users\\Alice\u{2126}"), "İC:\\Users\\<user>");
        assert_eq!(
            r.redact("İ C:\\Users\\émile\\app.exe \u{2126}"),
            "İ C:\\Users\\<user>\\app.exe \u{2126}"
        );
        assert_eq!(r.redact("D:\\USERS\\Bob\\x"), "D:\\USERS\\<user>\\x");
        assert_eq!(r.redact(""), "");
        assert_eq!(r.redact("\\users\\"), "\\users\\");
    }
}
