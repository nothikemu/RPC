//! Small time helpers shared across modules.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Milliseconds since the Unix epoch, saturating at zero if the clock is before 1970.
pub fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Formats an elapsed duration with minute granularity for use inside text
/// templates (`{elapsed}`). Minute granularity keeps the rendered text stable
/// so it does not force a Discord update every second.
pub fn format_elapsed(elapsed: Duration) -> String {
    let total_minutes = elapsed.as_secs() / 60;
    if total_minutes == 0 {
        return "<1m".to_owned();
    }
    let days = total_minutes / (60 * 24);
    let hours = (total_minutes / 60) % 24;
    let minutes = total_minutes % 60;
    match (days, hours) {
        (0, 0) => format!("{minutes}m"),
        (0, h) => format!("{h}h {minutes}m"),
        (d, h) => format!("{d}d {h}h"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_formatting_is_minute_granular() {
        assert_eq!(format_elapsed(Duration::from_secs(5)), "<1m");
        assert_eq!(format_elapsed(Duration::from_secs(60)), "1m");
        assert_eq!(format_elapsed(Duration::from_secs(59 * 60 + 59)), "59m");
        assert_eq!(format_elapsed(Duration::from_secs(3600 + 5 * 60)), "1h 5m");
        assert_eq!(format_elapsed(Duration::from_secs(26 * 3600)), "1d 2h");
    }

    #[test]
    fn now_is_after_2020() {
        assert!(now_unix_ms() > 1_577_836_800_000);
    }
}
