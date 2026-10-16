//! Human-readable summary of what an import would replace.

use std::collections::HashMap;

use serde::Serialize;

use super::schema::Config;
use crate::rules::{AppMatcher, MatchKind};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ConfigChange {
    pub section: String,
    pub description: String,
}

fn change(section: &str, description: String) -> ConfigChange {
    ConfigChange {
        section: section.to_owned(),
        description,
    }
}

fn quoted(value: &str) -> String {
    if value.is_empty() {
        "(empty)".to_owned()
    } else {
        format!("“{value}”")
    }
}

fn opt(value: &Option<String>) -> String {
    value.as_deref().map_or_else(|| "(not set)".to_owned(), quoted)
}

fn debug_name<T: std::fmt::Debug>(value: &T) -> String {
    let raw = format!("{value:?}");
    let mut out = String::new();
    for (i, c) in raw.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push(' ');
        }
        out.extend(c.to_lowercase());
    }
    out
}

fn matcher_label(m: &AppMatcher) -> String {
    let kind = match m.kind {
        MatchKind::Path => "path",
        MatchKind::ExeName => "executable",
        MatchKind::Pattern => "pattern",
    };
    format!("{kind} {}", m.value)
}

macro_rules! compare {
    ($out:ident, $section:expr, $label:expr, $a:expr, $b:expr, $fmt:expr) => {
        if $a != $b {
            $out.push(change(
                $section,
                format!("{}: {} → {}", $label, $fmt(&$a), $fmt(&$b)),
            ));
        }
    };
}

/// Lists the differences between the current configuration and an incoming one.
pub fn diff_configs(current: &Config, incoming: &Config) -> Vec<ConfigChange> {
    let mut out = Vec::new();
    let (a, b) = (current, incoming);

    compare!(
        out,
        "Discord",
        "Application ID",
        a.discord.application_id,
        b.discord.application_id,
        opt
    );
    compare!(
        out,
        "Discord",
        "Activity type",
        a.discord.activity_type,
        b.discord.activity_type,
        debug_name
    );
    compare!(
        out,
        "Discord",
        "Status line shows",
        a.discord.status_display,
        b.discord.status_display,
        debug_name
    );
    compare!(
        out,
        "Discord",
        "Use app name as activity name",
        a.discord.override_activity_name,
        b.discord.override_activity_name,
        debug_name
    );

    compare!(
        out,
        "Presence",
        "Details template",
        a.presence.details_template,
        b.presence.details_template,
        |v: &String| quoted(v)
    );
    compare!(
        out,
        "Presence",
        "State template",
        a.presence.state_template,
        b.presence.state_template,
        |v: &String| quoted(v)
    );
    compare!(
        out,
        "Presence",
        "Timestamp",
        a.presence.timestamp_mode,
        b.presence.timestamp_mode,
        debug_name
    );
    compare!(
        out,
        "Presence",
        "Selection mode",
        a.presence.selection_mode,
        b.presence.selection_mode,
        debug_name
    );
    compare!(
        out,
        "Presence",
        "When an excluded app is focused",
        a.presence.excluded_focus,
        b.presence.excluded_focus,
        debug_name
    );
    compare!(
        out,
        "Presence",
        "Fall back to foreground in priority mode",
        a.presence.priority_fallback_to_foreground,
        b.presence.priority_fallback_to_foreground,
        debug_name
    );
    if a.presence.default_assets != b.presence.default_assets {
        out.push(change("Presence", "Default artwork changes".to_owned()));
    }

    let old_rules: HashMap<&str, _> = a.rules.iter().map(|r| (r.id.as_str(), r)).collect();
    let new_rules: HashMap<&str, _> = b.rules.iter().map(|r| (r.id.as_str(), r)).collect();
    for rule in &b.rules {
        match old_rules.get(rule.id.as_str()) {
            None => out.push(change(
                "Applications",
                format!("Adds rule for {}", matcher_label(&rule.matcher)),
            )),
            Some(old) if *old != rule => out.push(change(
                "Applications",
                format!("Changes rule for {}", matcher_label(&rule.matcher)),
            )),
            Some(_) => {}
        }
    }
    for rule in &a.rules {
        if !new_rules.contains_key(rule.id.as_str()) {
            out.push(change(
                "Applications",
                format!("Removes rule for {}", matcher_label(&rule.matcher)),
            ));
        }
    }

    for exclusion in &b.exclusions {
        if !a
            .exclusions
            .iter()
            .any(|e| e.matcher.same_target(&exclusion.matcher))
        {
            out.push(change(
                "Exclusions",
                format!("Adds {}", matcher_label(&exclusion.matcher)),
            ));
        }
    }
    for exclusion in &a.exclusions {
        if !b
            .exclusions
            .iter()
            .any(|e| e.matcher.same_target(&exclusion.matcher))
        {
            out.push(change(
                "Exclusions",
                format!("Removes {}", matcher_label(&exclusion.matcher)),
            ));
        }
    }

    let ms = |v: &u32| format!("{v} ms");
    compare!(
        out,
        "Detection",
        "Polling interval",
        a.detection.poll_interval_ms,
        b.detection.poll_interval_ms,
        ms
    );
    compare!(
        out,
        "Detection",
        "Foreground debounce",
        a.detection.foreground_debounce_ms,
        b.detection.foreground_debounce_ms,
        ms
    );
    compare!(
        out,
        "Privacy",
        "Window-title tracking",
        a.privacy.capture_window_titles,
        b.privacy.capture_window_titles,
        debug_name
    );
    compare!(
        out,
        "Startup",
        "Launch at Windows startup",
        a.startup.launch_at_login,
        b.startup.launch_at_login,
        debug_name
    );
    compare!(
        out,
        "Startup",
        "Start minimized",
        a.startup.start_minimized,
        b.startup.start_minimized,
        debug_name
    );
    compare!(
        out,
        "Window",
        "Close to tray",
        a.window.close_to_tray,
        b.window.close_to_tray,
        debug_name
    );
    compare!(out, "Appearance", "Theme", a.ui.theme, b.ui.theme, debug_name);
    compare!(
        out,
        "Diagnostics",
        "Log level",
        a.diagnostics.log_level,
        b.diagnostics.log_level,
        debug_name
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{SelectionMode, Theme};
    use crate::rules::{AppRule, ExclusionRule};

    #[test]
    fn identical_configs_have_no_changes() {
        assert!(diff_configs(&Config::default(), &Config::default()).is_empty());
    }

    #[test]
    fn lists_changed_values_rules_and_exclusions() {
        let mut current = Config::default();
        current
            .rules
            .push(AppRule::new("keep", AppMatcher::exe_name("a.exe")));
        current
            .rules
            .push(AppRule::new("gone", AppMatcher::exe_name("b.exe")));

        let mut incoming = current.clone();
        incoming.presence.details_template = "Coding in {app}".into();
        incoming.presence.selection_mode = SelectionMode::Priority;
        incoming.ui.theme = Theme::Dark;
        incoming.rules.retain(|r| r.id != "gone");
        incoming.rules[0].priority = 5;
        incoming
            .rules
            .push(AppRule::new("new", AppMatcher::path(r"C:\c.exe")));
        incoming.exclusions.push(ExclusionRule {
            id: "x".into(),
            matcher: AppMatcher::exe_name("secret.exe"),
            note: None,
        });

        let changes = diff_configs(&current, &incoming);
        let text: Vec<String> = changes
            .iter()
            .map(|c| format!("{}: {}", c.section, c.description))
            .collect();
        assert!(text.contains(&"Presence: Details template: “Using {app}” → “Coding in {app}”".to_owned()));
        assert!(text.contains(&"Presence: Selection mode: foreground → priority".to_owned()));
        assert!(text.contains(&"Appearance: Theme: system → dark".to_owned()));
        assert!(text.contains(&"Applications: Changes rule for executable a.exe".to_owned()));
        assert!(text.contains(&"Applications: Removes rule for executable b.exe".to_owned()));
        assert!(text.contains(&r"Applications: Adds rule for path C:\c.exe".to_owned()));
        assert!(text.contains(&"Exclusions: Adds executable secret.exe".to_owned()));
    }
}
