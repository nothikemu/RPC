//! Validation of configuration values. The frontend validates too, but Rust is
//! the authority: every command that changes configuration runs these checks.

use std::collections::HashSet;

use serde::Serialize;

use super::schema::{Config, MAX_EXCLUSIONS, MAX_RULES};
use crate::presence_templates::Template;
use crate::rules::AssetOverrides;

/// One problem found in a configuration, addressed by a dotted path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ValidationIssue {
    pub path: String,
    pub message: String,
}

impl ValidationIssue {
    fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

/// Maximum length for asset keys and image URLs.
pub const MAX_ASSET_KEY_LEN: usize = 256;
/// Maximum stored length for hover text (rendering truncates to Discord's limit).
pub const MAX_STORED_TEXT_LEN: usize = 512;
pub const MAX_DISPLAY_NAME_LEN: usize = 128;
pub const MAX_RULE_ID_LEN: usize = 64;

/// Checks the *syntax* of a Discord application ID (a snowflake): 17–20
/// decimal digits that fit in an unsigned 64-bit integer. Syntactic validity
/// does not prove the application exists.
pub fn is_valid_application_id(id: &str) -> bool {
    (17..=20).contains(&id.len())
        && id.bytes().all(|b| b.is_ascii_digit())
        && id.parse::<u64>().is_ok_and(|v| v > 0)
}

/// Validates an asset value: either an asset key or an `https://` URL.
pub fn validate_asset_value(value: &str) -> Result<(), String> {
    if value.chars().count() > MAX_ASSET_KEY_LEN {
        return Err(format!("must be at most {MAX_ASSET_KEY_LEN} characters"));
    }
    if value.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("must not contain spaces or control characters".to_owned());
    }
    let lower = value.to_ascii_lowercase();
    if lower.starts_with("http://") {
        return Err("image URLs must use https://".to_owned());
    }
    if lower.contains("://") && !lower.starts_with("https://") {
        return Err("only asset keys and https:// image URLs are supported".to_owned());
    }
    Ok(())
}

fn validate_text(path: &str, value: &str, max: usize, issues: &mut Vec<ValidationIssue>) {
    if value.chars().count() > max {
        issues.push(ValidationIssue::new(
            path,
            format!("must be at most {max} characters"),
        ));
    }
    if value.chars().any(|c| c.is_control()) {
        issues.push(ValidationIssue::new(path, "must not contain control characters"));
    }
}

fn validate_assets(prefix: &str, assets: &AssetOverrides, issues: &mut Vec<ValidationIssue>) {
    for (name, value) in [
        ("large_image", &assets.large_image),
        ("small_image", &assets.small_image),
    ] {
        if let Some(v) = value
            && let Err(message) = validate_asset_value(v)
        {
            issues.push(ValidationIssue::new(format!("{prefix}.{name}"), message));
        }
    }
    for (name, value) in [
        ("large_text", &assets.large_text),
        ("small_text", &assets.small_text),
    ] {
        if let Some(v) = value {
            validate_text(&format!("{prefix}.{name}"), v, MAX_STORED_TEXT_LEN, issues);
        }
    }
}

fn validate_template(path: &str, source: &str, issues: &mut Vec<ValidationIssue>) {
    if let Err(e) = Template::parse(source) {
        issues.push(ValidationIssue::new(path, e.to_string()));
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_RULE_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl Config {
    /// Returns every problem found. An empty list means the configuration is valid.
    pub fn validate(&self) -> Vec<ValidationIssue> {
        let mut issues = Vec::new();

        if let Some(id) = &self.discord.application_id
            && !is_valid_application_id(id)
        {
            issues.push(ValidationIssue::new(
                "discord.application_id",
                "must be a Discord application ID: 17 to 20 digits",
            ));
        }

        validate_template(
            "presence.details_template",
            &self.presence.details_template,
            &mut issues,
        );
        validate_template(
            "presence.state_template",
            &self.presence.state_template,
            &mut issues,
        );
        validate_assets(
            "presence.default_assets",
            &self.presence.default_assets,
            &mut issues,
        );

        if self.rules.len() > MAX_RULES {
            issues.push(ValidationIssue::new(
                "rules",
                format!("at most {MAX_RULES} application rules are supported"),
            ));
        }
        let mut ids = HashSet::new();
        for (i, rule) in self.rules.iter().enumerate() {
            let prefix = format!("rules[{i}]");
            if !valid_id(&rule.id) {
                issues.push(ValidationIssue::new(
                    format!("{prefix}.id"),
                    "must be 1–64 letters, digits, '-' or '_'",
                ));
            } else if !ids.insert(rule.id.as_str()) {
                issues.push(ValidationIssue::new(
                    format!("{prefix}.id"),
                    "is used by another rule",
                ));
            }
            if let Err(e) = rule.matcher.validate() {
                issues.push(ValidationIssue::new(format!("{prefix}.matcher"), e.to_string()));
            }
            if self.rules[..i]
                .iter()
                .any(|other| other.matcher.same_target(&rule.matcher))
            {
                issues.push(ValidationIssue::new(
                    format!("{prefix}.matcher"),
                    "another rule already targets the same application",
                ));
            }
            if let Some(name) = &rule.display_name {
                validate_text(
                    &format!("{prefix}.display_name"),
                    name,
                    MAX_DISPLAY_NAME_LEN,
                    &mut issues,
                );
            }
            if let Some(t) = &rule.details_template {
                validate_template(&format!("{prefix}.details_template"), t, &mut issues);
            }
            if let Some(t) = &rule.state_template {
                validate_template(&format!("{prefix}.state_template"), t, &mut issues);
            }
            validate_assets(&format!("{prefix}.assets"), &rule.assets, &mut issues);
        }

        if self.exclusions.len() > MAX_EXCLUSIONS {
            issues.push(ValidationIssue::new(
                "exclusions",
                format!("at most {MAX_EXCLUSIONS} exclusions are supported"),
            ));
        }
        let mut exclusion_ids = HashSet::new();
        for (i, exclusion) in self.exclusions.iter().enumerate() {
            let prefix = format!("exclusions[{i}]");
            if !valid_id(&exclusion.id) {
                issues.push(ValidationIssue::new(
                    format!("{prefix}.id"),
                    "must be 1–64 letters, digits, '-' or '_'",
                ));
            } else if !exclusion_ids.insert(exclusion.id.as_str()) {
                issues.push(ValidationIssue::new(
                    format!("{prefix}.id"),
                    "is used by another exclusion",
                ));
            }
            if let Err(e) = exclusion.matcher.validate() {
                issues.push(ValidationIssue::new(format!("{prefix}.matcher"), e.to_string()));
            }
            if let Some(note) = &exclusion.note {
                validate_text(&format!("{prefix}.note"), note, MAX_DISPLAY_NAME_LEN, &mut issues);
            }
        }

        issues
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{AppMatcher, AppRule, ExclusionRule};

    #[test]
    fn application_id_syntax() {
        assert!(is_valid_application_id("123456789012345678"));
        assert!(is_valid_application_id("12345678901234567"));
        assert!(!is_valid_application_id("1234567890123456"));
        assert!(!is_valid_application_id("123456789012345678901"));
        assert!(!is_valid_application_id("12345678901234567a"));
        assert!(!is_valid_application_id(" 123456789012345678"));
        assert!(!is_valid_application_id("99999999999999999999")); // > u64::MAX
        assert!(!is_valid_application_id("00000000000000000"));
    }

    #[test]
    fn default_config_is_valid() {
        assert!(Config::default().validate().is_empty());
    }

    #[test]
    fn reports_template_and_id_problems() {
        let mut c = Config::default();
        c.discord.application_id = Some("abc".into());
        c.presence.details_template = "Using {nope}".into();
        let issues = c.validate();
        assert!(issues.iter().any(|i| i.path == "discord.application_id"));
        assert!(
            issues
                .iter()
                .any(|i| i.path == "presence.details_template" && i.message.contains("{nope}"))
        );
    }

    #[test]
    fn rules_must_have_unique_ids_and_targets() {
        let mut c = Config::default();
        c.rules.push(AppRule::new("a", AppMatcher::exe_name("x.exe")));
        c.rules.push(AppRule::new("a", AppMatcher::exe_name("y.exe")));
        c.rules.push(AppRule::new("b", AppMatcher::exe_name("X.EXE")));
        c.rules
            .push(AppRule::new("bad id!", AppMatcher::exe_name("z.exe")));
        let issues = c.validate();
        assert!(issues.iter().any(|i| i.path == "rules[1].id"));
        assert!(
            issues
                .iter()
                .any(|i| i.path == "rules[2].matcher" && i.message.contains("same application"))
        );
        assert!(issues.iter().any(|i| i.path == "rules[3].id"));
    }

    #[test]
    fn asset_values() {
        assert!(validate_asset_value("my_large_icon").is_ok());
        assert!(validate_asset_value("https://example.com/a.png").is_ok());
        assert!(validate_asset_value("http://example.com/a.png").is_err());
        assert!(validate_asset_value("ftp://example.com/a.png").is_err());
        assert!(validate_asset_value("has space").is_err());
        assert!(validate_asset_value(&"k".repeat(257)).is_err());
    }

    #[test]
    fn exclusions_are_validated() {
        let mut c = Config::default();
        c.exclusions.push(ExclusionRule {
            id: "x".into(),
            matcher: AppMatcher::pattern("*"),
            note: None,
        });
        let issues = c.validate();
        assert!(issues.iter().any(|i| i.path == "exclusions[0].matcher"));
    }

    #[test]
    fn normalize_clamps_and_trims() {
        let mut c = Config::default();
        c.detection.poll_interval_ms = 1;
        c.detection.foreground_debounce_ms = 99_999;
        c.discord.application_id = Some("   ".into());
        let mut rule = AppRule::new(" r1 ", AppMatcher::exe_name(" a.exe "));
        rule.display_name = Some("  ".into());
        rule.details_template = Some("   ".into());
        rule.priority = 1_000_000;
        c.rules.push(rule);
        c.normalize();
        assert_eq!(c.detection.poll_interval_ms, 250);
        assert_eq!(c.detection.foreground_debounce_ms, 5_000);
        assert_eq!(c.discord.application_id, None);
        assert_eq!(c.rules[0].id, "r1");
        assert_eq!(c.rules[0].matcher.value, "a.exe");
        assert_eq!(c.rules[0].display_name, None);
        assert_eq!(c.rules[0].details_template, None);
        assert_eq!(c.rules[0].priority, 1000);
        assert!(c.validate().is_empty());
    }
}
