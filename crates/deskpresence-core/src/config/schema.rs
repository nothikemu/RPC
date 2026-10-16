//! Configuration types (schema version 1).
//!
//! Every struct uses `#[serde(default)]` so that a file missing optional
//! fields still loads; required structure changes go through migrations.

use serde::{Deserialize, Serialize};

use crate::rules::{AppRule, AssetOverrides, ExclusionRule};

/// The schema version this build reads and writes.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

pub const POLL_INTERVAL_MIN_MS: u32 = 250;
pub const POLL_INTERVAL_MAX_MS: u32 = 10_000;
pub const POLL_INTERVAL_DEFAULT_MS: u32 = 1_000;
pub const DEBOUNCE_MIN_MS: u32 = 0;
pub const DEBOUNCE_MAX_MS: u32 = 5_000;
pub const DEBOUNCE_DEFAULT_MS: u32 = 500;
pub const MAX_RULES: usize = 500;
pub const MAX_EXCLUSIONS: usize = 500;

pub const DEFAULT_DETAILS_TEMPLATE: &str = "Using {app}";
pub const DEFAULT_STATE_TEMPLATE: &str = "";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(default)]
pub struct Config {
    pub schema_version: u32,
    pub discord: DiscordConfig,
    pub presence: PresenceConfig,
    pub rules: Vec<AppRule>,
    pub exclusions: Vec<ExclusionRule>,
    pub detection: DetectionConfig,
    pub privacy: PrivacyConfig,
    pub startup: StartupConfig,
    pub window: WindowConfig,
    pub ui: UiConfig,
    pub diagnostics: DiagnosticsConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            discord: DiscordConfig::default(),
            presence: PresenceConfig::default(),
            rules: Vec::new(),
            exclusions: Vec::new(),
            detection: DetectionConfig::default(),
            privacy: PrivacyConfig::default(),
            startup: StartupConfig::default(),
            window: WindowConfig::default(),
            ui: UiConfig::default(),
            diagnostics: DiagnosticsConfig::default(),
        }
    }
}

/// Activity types permitted for `SET_ACTIVITY` by Discord's RPC docs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum ActivityType {
    #[default]
    Playing,
    Listening,
    Watching,
    Competing,
}

impl ActivityType {
    /// Discord's numeric activity type.
    pub fn code(self) -> u8 {
        match self {
            ActivityType::Playing => 0,
            ActivityType::Listening => 2,
            ActivityType::Watching => 3,
            ActivityType::Competing => 5,
        }
    }
}

/// Which field Discord shows in the member-list status line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum StatusDisplay {
    /// The Discord application's name (Discord's default).
    #[default]
    Name,
    State,
    Details,
}

impl StatusDisplay {
    pub fn code(self) -> Option<u8> {
        match self {
            StatusDisplay::Name => None,
            StatusDisplay::State => Some(1),
            StatusDisplay::Details => Some(2),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(default)]
pub struct DiscordConfig {
    /// User override of the Discord Application (client) ID. When unset, a
    /// build-time default is used if one was compiled in.
    pub application_id: Option<String>,
    pub activity_type: ActivityType,
    pub status_display: StatusDisplay,
    /// Experimental: send the detected application's name as the activity
    /// `name`. Documented on the activity object; Discord may ignore it for
    /// IPC clients, in which case the Discord application's name is shown.
    pub override_activity_name: bool,
}

/// How the elapsed-time timestamp is computed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum TimestampMode {
    /// Resets whenever a different application becomes the presence source.
    #[default]
    SinceSelected,
    /// Keeps counting across application switches until presence is cleared.
    SinceSession,
    /// Uses the selected application's process start time.
    SinceLaunch,
    /// No timestamp is sent.
    Off,
}

/// Automatic selection policy (manual pinning overrides either).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum SelectionMode {
    /// The application that owns the foreground window.
    #[default]
    Foreground,
    /// The highest-priority running application with an enabled rule.
    Priority,
}

/// What happens when the focused application is excluded or disabled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum ExcludedFocusBehavior {
    /// Keep showing the most recently focused eligible application.
    #[default]
    KeepPrevious,
    /// Clear the activity while an excluded application is focused.
    Clear,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(default)]
pub struct PresenceConfig {
    pub details_template: String,
    pub state_template: String,
    pub timestamp_mode: TimestampMode,
    pub selection_mode: SelectionMode,
    pub excluded_focus: ExcludedFocusBehavior,
    /// In priority mode, use the foreground application when no
    /// prioritized application is running instead of clearing.
    pub priority_fallback_to_foreground: bool,
    /// Artwork used when an application rule does not set its own.
    pub default_assets: AssetOverrides,
}

impl Default for PresenceConfig {
    fn default() -> Self {
        Self {
            details_template: DEFAULT_DETAILS_TEMPLATE.to_owned(),
            state_template: DEFAULT_STATE_TEMPLATE.to_owned(),
            timestamp_mode: TimestampMode::default(),
            selection_mode: SelectionMode::default(),
            excluded_focus: ExcludedFocusBehavior::default(),
            priority_fallback_to_foreground: false,
            default_assets: AssetOverrides::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(default)]
pub struct DetectionConfig {
    /// Full window/process scan interval.
    pub poll_interval_ms: u32,
    /// How long a new foreground application must stay focused before it
    /// takes over the presence.
    pub foreground_debounce_ms: u32,
}

impl Default for DetectionConfig {
    fn default() -> Self {
        Self {
            poll_interval_ms: POLL_INTERVAL_DEFAULT_MS,
            foreground_debounce_ms: DEBOUNCE_DEFAULT_MS,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(default)]
pub struct PrivacyConfig {
    /// Allow reading window titles for `{window_title}`. Off by default.
    pub capture_window_titles: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(default)]
pub struct StartupConfig {
    /// The user's preference; the operating-system registration is the
    /// source of truth for whether startup launch is currently active.
    pub launch_at_login: bool,
    /// Start hidden in the tray when launched manually.
    pub start_minimized: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(default)]
pub struct WindowConfig {
    /// Closing the window hides it to the tray instead of exiting.
    pub close_to_tray: bool,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self { close_to_tray: true }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(default)]
pub struct UiConfig {
    pub theme: Theme,
    pub onboarding_completed: bool,
    /// The "closing hides to tray" explanation was acknowledged.
    pub tray_hint_dismissed: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
    Trace,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(default)]
pub struct DiagnosticsConfig {
    pub log_level: LogLevel,
}

impl Config {
    /// Lenient cleanup applied to every loaded or saved configuration:
    /// trims text, converts empty optional strings to `None`, clamps numeric
    /// ranges and sets the schema version. Validation runs afterwards.
    pub fn normalize(&mut self) {
        self.schema_version = CURRENT_SCHEMA_VERSION;
        normalize_opt(&mut self.discord.application_id);
        self.detection.poll_interval_ms = self
            .detection
            .poll_interval_ms
            .clamp(POLL_INTERVAL_MIN_MS, POLL_INTERVAL_MAX_MS);
        self.detection.foreground_debounce_ms = self
            .detection
            .foreground_debounce_ms
            .clamp(DEBOUNCE_MIN_MS, DEBOUNCE_MAX_MS);
        normalize_assets(&mut self.presence.default_assets);
        for rule in &mut self.rules {
            rule.id = rule.id.trim().to_owned();
            rule.matcher.value = rule.matcher.value.trim().to_owned();
            normalize_opt(&mut rule.display_name);
            normalize_opt_template(&mut rule.details_template);
            normalize_opt_template(&mut rule.state_template);
            normalize_assets(&mut rule.assets);
            rule.priority = rule
                .priority
                .clamp(crate::rules::PRIORITY_MIN, crate::rules::PRIORITY_MAX);
        }
        for exclusion in &mut self.exclusions {
            exclusion.id = exclusion.id.trim().to_owned();
            exclusion.matcher.value = exclusion.matcher.value.trim().to_owned();
            normalize_opt(&mut exclusion.note);
        }
    }
}

fn normalize_opt(value: &mut Option<String>) {
    if let Some(v) = value {
        let trimmed = v.trim();
        if trimmed.is_empty() {
            *value = None;
        } else if trimmed.len() != v.len() {
            *value = Some(trimmed.to_owned());
        }
    }
}

/// Templates keep inner whitespace but an all-whitespace override means "inherit".
fn normalize_opt_template(value: &mut Option<String>) {
    if value.as_deref().is_some_and(|v| v.trim().is_empty()) {
        *value = None;
    }
}

fn normalize_assets(assets: &mut AssetOverrides) {
    normalize_opt(&mut assets.large_image);
    normalize_opt(&mut assets.large_text);
    normalize_opt(&mut assets.small_image);
    normalize_opt(&mut assets.small_text);
}
