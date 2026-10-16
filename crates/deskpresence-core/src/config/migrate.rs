//! Schema migrations operate on raw JSON so that old layouts can be read
//! without keeping old Rust types around. Each step upgrades by one version.
//!
//! Version 0 means "no `schema_version` field": hand-written or pre-release
//! files that used a flat layout (`client_id`, `details`, `state`, string
//! exclusions). Migrating them keeps such files working.

use serde_json::{Map, Value};

use super::ConfigError;
use super::schema::CURRENT_SCHEMA_VERSION;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MigrationReport {
    pub from_version: u32,
    pub to_version: u32,
    /// Human-readable description of each change applied.
    pub steps: Vec<String>,
}

impl MigrationReport {
    pub fn migrated(&self) -> bool {
        self.from_version != self.to_version
    }
}

/// Upgrades `value` to [`CURRENT_SCHEMA_VERSION`]. Newer versions are rejected
/// rather than silently downgraded, so data written by a newer build is never lost.
pub fn migrate_value(mut value: Value) -> Result<(Value, MigrationReport), ConfigError> {
    let Some(object) = value.as_object_mut() else {
        return Err(ConfigError::Json(
            "expected a JSON object at the top level".into(),
        ));
    };
    let from_version = match object.get("schema_version") {
        None | Some(Value::Null) => 0,
        Some(Value::Number(n)) => {
            let v = n.as_u64().ok_or(ConfigError::InvalidVersion)?;
            if v > u64::from(CURRENT_SCHEMA_VERSION) {
                return Err(ConfigError::UnsupportedVersion {
                    found: v,
                    supported: CURRENT_SCHEMA_VERSION,
                });
            }
            u32::try_from(v).map_err(|_| ConfigError::InvalidVersion)?
        }
        Some(_) => return Err(ConfigError::InvalidVersion),
    };

    let mut steps = Vec::new();
    let mut version = from_version;
    while version < CURRENT_SCHEMA_VERSION {
        match version {
            0 => migrate_v0_to_v1(object, &mut steps),
            _ => return Err(ConfigError::InvalidVersion),
        }
        version += 1;
        object.insert("schema_version".into(), Value::from(version));
    }

    Ok((
        value,
        MigrationReport {
            from_version,
            to_version: version,
            steps,
        },
    ))
}

/// Returns the named object section, creating it (or replacing a non-object).
fn section<'a>(object: &'a mut Map<String, Value>, name: &str) -> Option<&'a mut Map<String, Value>> {
    let entry = object
        .entry(name.to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    if !entry.is_object() {
        *entry = Value::Object(Map::new());
    }
    entry.as_object_mut()
}

fn take_string(object: &mut Map<String, Value>, key: &str) -> Option<String> {
    match object.remove(key) {
        Some(Value::String(s)) => Some(s),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

fn migrate_v0_to_v1(object: &mut Map<String, Value>, steps: &mut Vec<String>) {
    for legacy in ["client_id", "application_id"] {
        if let Some(id) = take_string(object, legacy)
            && let Some(discord) = section(object, "discord")
            && !discord.contains_key("application_id")
        {
            discord.insert("application_id".into(), Value::String(id));
            steps.push(format!("Moved `{legacy}` to `discord.application_id`"));
        }
    }
    for (legacy, target) in [("details", "details_template"), ("state", "state_template")] {
        if let Some(template) = take_string(object, legacy)
            && let Some(presence) = section(object, "presence")
            && !presence.contains_key(target)
        {
            presence.insert(target.into(), Value::String(template));
            steps.push(format!("Moved `{legacy}` to `presence.{target}`"));
        }
    }
    if let Some(Value::Number(interval)) = object.remove("poll_interval_ms")
        && let Some(detection) = section(object, "detection")
    {
        detection
            .entry("poll_interval_ms")
            .or_insert(Value::Number(interval));
        steps.push("Moved `poll_interval_ms` to `detection.poll_interval_ms`".into());
    }
    if let Some(Value::Array(items)) = object.get_mut("exclusions") {
        let mut converted = 0;
        for (index, item) in items.iter_mut().enumerate() {
            if let Value::String(raw) = item {
                let raw = raw.trim().to_owned();
                let kind = if raw.contains(['*', '?']) {
                    "pattern"
                } else if raw.contains(['\\', '/']) {
                    "path"
                } else {
                    "exe_name"
                };
                *item = serde_json::json!({
                    "id": format!("x-legacy-{index}"),
                    "matcher": { "kind": kind, "value": raw },
                });
                converted += 1;
            }
        }
        if converted > 0 {
            steps.push(format!(
                "Converted {converted} plain-text exclusion(s) to exclusion rules"
            ));
        }
    }
    steps.push("Upgraded configuration to schema version 1".into());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, parse_config};
    use crate::rules::MatchKind;

    #[test]
    fn current_version_is_untouched() {
        let value = serde_json::to_value(Config::default()).unwrap();
        let (migrated, report) = migrate_value(value.clone()).unwrap();
        assert_eq!(migrated, value);
        assert!(!report.migrated());
        assert!(report.steps.is_empty());
    }

    #[test]
    fn unversioned_flat_layout_is_migrated() {
        let legacy = serde_json::json!({
            "client_id": "123456789012345678",
            "details": "Working in {app}",
            "state": "{exe}",
            "poll_interval_ms": 2000,
            "exclusions": ["secret.exe", "C:\\Private\\tool.exe", "*helper*.exe"],
        });
        let (value, report) = migrate_value(legacy).unwrap();
        assert_eq!(report.from_version, 0);
        assert_eq!(report.to_version, 1);
        assert!(report.migrated());
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["discord"]["application_id"], "123456789012345678");
        assert_eq!(value["presence"]["details_template"], "Working in {app}");
        assert_eq!(value["detection"]["poll_interval_ms"], 2000);
        assert_eq!(value["exclusions"][0]["matcher"]["kind"], "exe_name");
        assert_eq!(value["exclusions"][1]["matcher"]["kind"], "path");
        assert_eq!(value["exclusions"][2]["matcher"]["kind"], "pattern");

        // The migrated value is a fully valid configuration.
        let (config, _) = parse_config(&value.to_string()).unwrap();
        assert_eq!(config.exclusions.len(), 3);
        assert_eq!(config.exclusions[1].matcher.kind, MatchKind::Path);
        assert_eq!(config.presence.state_template, "{exe}");
    }

    #[test]
    fn existing_nested_values_win_over_legacy_ones() {
        let legacy = serde_json::json!({
            "client_id": "111111111111111111",
            "discord": { "application_id": "222222222222222222" },
        });
        let (value, _) = migrate_value(legacy).unwrap();
        assert_eq!(value["discord"]["application_id"], "222222222222222222");
        assert!(value.get("client_id").is_none());
    }

    #[test]
    fn newer_versions_are_rejected() {
        let err = migrate_value(serde_json::json!({ "schema_version": 99 })).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::UnsupportedVersion {
                found: 99,
                supported: 1
            }
        ));
    }

    #[test]
    fn invalid_versions_are_rejected() {
        assert!(matches!(
            migrate_value(serde_json::json!({ "schema_version": "one" })).unwrap_err(),
            ConfigError::InvalidVersion
        ));
        assert!(matches!(
            migrate_value(serde_json::json!({ "schema_version": -1 })).unwrap_err(),
            ConfigError::InvalidVersion
        ));
        assert!(matches!(
            migrate_value(serde_json::json!([1, 2])).unwrap_err(),
            ConfigError::Json(_)
        ));
    }
}
