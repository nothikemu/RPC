//! Types exchanged with the configuration UI. With the `ts` feature these are
//! exported as TypeScript (`src/lib/bindings`), so the frontend and backend
//! cannot drift apart silently.

use serde::{Deserialize, Serialize};

use crate::app_detection::{DetectorStatus, ScanStats};
use crate::app_identity::{AppKey, NameSource};
use crate::config::{Config, ConfigChange, SelectionMode, ValidationIssue};
use crate::diagnostics::{DiagnosticEvent, Redactor};
use crate::discord_rpc::{Activity, DiscordStatus};
use crate::presence_manager::{PresenceWarning, SelectionReason};
use crate::presence_templates::TemplateError;
use crate::rules::MatchKind;

/// How rules currently treat an application.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum AppRuleStatus {
    /// No rule applies; global settings are used.
    Automatic,
    /// An enabled rule applies.
    Configured,
    /// A rule disables presence for this application.
    Disabled,
    /// An exclusion matches.
    Excluded,
}

/// One row in the Applications list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct AppView {
    pub key: AppKey,
    pub display_name: String,
    pub name_source: NameSource,
    pub exe_name: String,
    pub exe_path: String,
    pub running: bool,
    pub is_foreground: bool,
    pub is_selected: bool,
    pub is_pinned: bool,
    pub window_count: u32,
    pub first_seen_ms: Option<u64>,
    /// For applications that are no longer running.
    pub last_seen_ms: Option<u64>,
    pub last_focused_ms: Option<u64>,
    pub status: AppRuleStatus,
    pub rule_id: Option<String>,
    pub rule_match: Option<MatchKind>,
    pub exclusion_id: Option<String>,
    pub priority: i32,
    /// For configured paths: whether the executable exists on disk.
    pub path_exists: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct SelectedView {
    pub key: AppKey,
    pub display_name: String,
    pub reason: SelectionReason,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct PresenceView {
    pub paused: bool,
    pub mode: SelectionMode,
    pub pinned: Option<AppKey>,
    pub selected: Option<SelectedView>,
    /// The activity DeskPresence wants Discord to show (`null` = cleared).
    pub activity: Option<Activity>,
    pub warnings: Vec<PresenceWarning>,
    pub source_since_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct DetectorView {
    pub status: DetectorStatus,
    pub stats: ScanStats,
    pub running_count: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum AppIdSource {
    /// Set by the user in Settings.
    Config,
    /// Compiled into this build (`DESKPRESENCE_DEFAULT_APP_ID`).
    BuildDefault,
    /// Not configured: Discord integration is in setup/preview mode.
    None,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ApplicationIdView {
    pub effective: Option<String>,
    pub source: AppIdSource,
}

/// Everything the UI shows that changes at runtime.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RuntimeView {
    pub revision: u64,
    pub started_at_ms: u64,
    pub detector: DetectorView,
    pub apps: Vec<AppView>,
    pub presence: PresenceView,
    pub discord: DiscordStatus,
    pub application_id: ApplicationIdView,
    /// Newest first.
    pub events: Vec<DiagnosticEvent>,
}

impl RuntimeView {
    /// Equality ignoring the revision counter.
    pub fn same_content(&self, other: &RuntimeView) -> bool {
        self.started_at_ms == other.started_at_ms
            && self.detector == other.detector
            && self.apps == other.apps
            && self.presence == other.presence
            && self.discord == other.discord
            && self.application_id == other.application_id
            && self.events == other.events
    }
}

/// The current configuration plus its revision for optimistic concurrency.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ConfigEnvelope {
    pub config: Config,
    pub revision: u64,
    pub notice: Option<LoadNotice>,
}

/// Shown once after startup when the configuration needed attention.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct LoadNotice {
    pub kind: LoadNoticeKind,
    pub message: String,
    pub backup_path: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum LoadNoticeKind {
    Migrated,
    Recovered,
}

/// Request to render templates for the live preview.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct PreviewRequest {
    pub details_template: String,
    pub state_template: String,
    /// Application to preview with; defaults to the selected one, then a sample.
    pub app_key: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct PreviewResult {
    pub app_display_name: String,
    pub app_exe_name: String,
    /// `true` when no real application was available and sample values were used.
    pub sample: bool,
    pub details: Option<String>,
    pub state: Option<String>,
    pub details_error: Option<TemplateError>,
    pub state_error: Option<TemplateError>,
    pub warnings: Vec<PresenceWarning>,
}

/// Summary of a configuration file chosen for import, shown before applying.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ImportPreview {
    pub token: String,
    pub file_name: String,
    pub from_version: u32,
    pub migrated: bool,
    pub changes: Vec<ConfigChange>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct StartupStatus {
    /// Whether Windows will launch DeskPresence at sign-in (registry state).
    pub registered: bool,
    pub preference: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct AppInfo {
    pub product_name: String,
    pub version: String,
    pub platform: String,
    pub detection_supported: bool,
    pub has_build_default_app_id: bool,
    pub config_path: String,
    pub log_dir: String,
}

/// Error returned by commands, safe to display.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct CommandError {
    pub code: CommandErrorCode,
    pub message: String,
    pub issues: Vec<ValidationIssue>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum CommandErrorCode {
    Invalid,
    Conflict,
    NotFound,
    Io,
    Unavailable,
    Cancelled,
    Internal,
}

impl CommandError {
    pub fn new(code: CommandErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            issues: Vec::new(),
        }
    }

    pub fn invalid(issues: Vec<ValidationIssue>) -> Self {
        let message = match issues.first() {
            Some(first) if issues.len() == 1 => format!("{}: {}", first.path, first.message),
            Some(first) => format!(
                "{}: {} (and {} more)",
                first.path,
                first.message,
                issues.len() - 1
            ),
            None => "The configuration is invalid.".to_owned(),
        };
        Self {
            code: CommandErrorCode::Invalid,
            message,
            issues,
        }
    }
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CommandError {}

/// Inputs for a diagnostics export.
pub struct BundleInput<'a> {
    pub app_version: &'a str,
    pub os: &'a str,
    pub generated_at_ms: u64,
    pub config: &'a Config,
    pub view: &'a RuntimeView,
    pub log_tail: &'a str,
    /// Include paths, rendered activity text and the Discord user name.
    pub include_sensitive: bool,
    pub redactor: &'a Redactor,
}

/// Builds the diagnostics bundle (JSON). By default it redacts user-specific
/// paths, rendered activity text (which may contain window titles) and the
/// Discord user name. Window titles are never stored outside the activity.
pub fn build_diagnostics_bundle(input: &BundleInput<'_>) -> serde_json::Value {
    let redact = |text: &str| {
        if input.include_sensitive {
            text.to_owned()
        } else {
            input.redactor.redact(text)
        }
    };
    let hide = |text: &Option<String>| -> serde_json::Value {
        match text {
            None => serde_json::Value::Null,
            Some(t) if input.include_sensitive => serde_json::Value::String(t.clone()),
            Some(t) => serde_json::Value::String(format!("<redacted, {} characters>", t.chars().count())),
        }
    };

    let mut config = input.config.clone();
    for rule in &mut config.rules {
        rule.matcher.value = redact(&rule.matcher.value);
    }
    for exclusion in &mut config.exclusions {
        exclusion.matcher.value = redact(&exclusion.matcher.value);
    }

    let view = input.view;
    let apps: Vec<serde_json::Value> = view
        .apps
        .iter()
        .map(|a| {
            serde_json::json!({
                "exe_name": a.exe_name,
                "exe_path": redact(&a.exe_path),
                "display_name": a.display_name,
                "name_source": a.name_source,
                "running": a.running,
                "is_foreground": a.is_foreground,
                "is_selected": a.is_selected,
                "status": a.status,
                "rule_match": a.rule_match,
                "window_count": a.window_count,
            })
        })
        .collect();

    let activity = view.presence.activity.as_ref().map(|a| {
        serde_json::json!({
            "type": a.activity_type,
            "details": hide(&a.details),
            "state": hide(&a.state),
            "name": hide(&a.name),
            "has_timestamps": a.timestamps.is_some(),
            // Hover texts are rendered templates and may contain window titles.
            "assets": a.assets.as_ref().map(|assets| serde_json::json!({
                "large_image": assets.large_image,
                "large_text": hide(&assets.large_text),
                "small_image": assets.small_image,
                "small_text": hide(&assets.small_text),
            })),
        })
    });

    let mut discord = serde_json::to_value(&view.discord).unwrap_or(serde_json::Value::Null);
    if let Some(obj) = discord.as_object_mut() {
        if !input.include_sensitive {
            obj.insert("user_name".into(), serde_json::Value::Null);
        }
        for key in ["last_error", "last_delivery_error"] {
            if let Some(serde_json::Value::String(text)) = obj.get(key).cloned() {
                obj.insert(key.into(), serde_json::Value::String(redact(&text)));
            }
        }
    }

    let events: Vec<serde_json::Value> = view
        .events
        .iter()
        .map(|e| {
            serde_json::json!({
                "at_ms": e.at_ms,
                "level": e.level,
                "category": e.category,
                "message": redact(&e.message),
                "count": e.count,
            })
        })
        .collect();

    let log_tail: Vec<String> = input.log_tail.lines().map(redact).collect();

    serde_json::json!({
        "format": "deskpresence-diagnostics/1",
        "generated_at_ms": input.generated_at_ms,
        "app_version": input.app_version,
        "os": input.os,
        "sensitive_data_included": input.include_sensitive,
        "config": config,
        "detector": view.detector,
        "apps": apps,
        "presence": {
            "paused": view.presence.paused,
            "mode": view.presence.mode,
            "selection_reason": view.presence.selected.as_ref().map(|s| s.reason),
            "pinned": view.presence.pinned.is_some(),
            "activity": activity,
            "warnings": view.presence.warnings.iter().map(|w| serde_json::json!({"field": w.field, "message": redact(&w.message)})).collect::<Vec<_>>(),
        },
        "discord": discord,
        "application_id": view.application_id,
        "events": events,
        "log_tail": log_tail,
    })
}
