//! Versioned, typed configuration with migrations and atomic persistence.

mod diff;
mod migrate;
mod schema;
mod store;
mod validate;

pub use diff::{ConfigChange, diff_configs};
pub use migrate::{MigrationReport, migrate_value};
pub use schema::*;
pub use store::{ConfigStore, LoadOutcome, read_config_file, write_atomic, write_config_file};
pub use validate::{ValidationIssue, is_valid_application_id, validate_asset_value};

/// Errors raised while reading, migrating or writing configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not read or write {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("the configuration is not valid JSON: {0}")]
    Json(String),
    #[error(
        "the configuration was written by a newer version of DeskPresence (schema {found}; this version supports up to {supported})"
    )]
    UnsupportedVersion { found: u64, supported: u32 },
    #[error("the configuration has an invalid schema_version field")]
    InvalidVersion,
    #[error("the configuration file is too large ({size} bytes; limit {limit})")]
    TooLarge { size: u64, limit: u64 },
    #[error("the configuration is invalid: {}", summarize(.0))]
    Invalid(Vec<ValidationIssue>),
    #[error(
        "the previous settings file could not be backed up, so it was not overwritten ({0}). Close programs that may be using it and try again"
    )]
    BackupFailed(String),
}

fn summarize(issues: &[ValidationIssue]) -> String {
    let mut parts: Vec<String> = issues
        .iter()
        .take(3)
        .map(|i| format!("{}: {}", i.path, i.message))
        .collect();
    if issues.len() > 3 {
        parts.push(format!("and {} more", issues.len() - 3));
    }
    parts.join("; ")
}

/// Maximum accepted size for configuration and import files.
pub const MAX_CONFIG_BYTES: u64 = 2 * 1024 * 1024;

/// Parses configuration JSON text: migrates older schemas, rejects newer ones,
/// fills missing fields with defaults, normalizes and validates.
pub fn parse_config(text: &str) -> Result<(Config, MigrationReport), ConfigError> {
    if text.len() as u64 > MAX_CONFIG_BYTES {
        return Err(ConfigError::TooLarge {
            size: text.len() as u64,
            limit: MAX_CONFIG_BYTES,
        });
    }
    // Notepad and PowerShell 5.1 can save UTF-8 with a byte order mark.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| ConfigError::Json(e.to_string()))?;
    let (value, report) = migrate_value(value)?;
    let mut config: Config = serde_json::from_value(value).map_err(|e| ConfigError::Json(e.to_string()))?;
    config.normalize();
    let issues = config.validate();
    if issues.is_empty() {
        Ok((config, report))
    } else {
        Err(ConfigError::Invalid(issues))
    }
}

/// Serializes configuration as pretty JSON (the on-disk and export format).
pub fn to_json(config: &Config) -> Result<String, ConfigError> {
    serde_json::to_string_pretty(config).map_err(|e| ConfigError::Json(e.to_string()))
}
