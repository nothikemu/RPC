//! Presence selection policy and activity generation.
//!
//! # Selection precedence
//!
//! 1. **Paused** → no activity is sent (the selection is still computed for the UI).
//! 2. **Exclusions and disabled rules** make an application ineligible
//!    everywhere, including for pinning.
//! 3. **Pinned**: a manually pinned, running, eligible application wins.
//!    The pin is cleared automatically when the application exits or becomes
//!    ineligible.
//! 4. **Foreground mode** (default): the debounced foreground application, if
//!    eligible. If the focused application is ineligible, either the most
//!    recently focused eligible application is kept (`keep_previous`, default)
//!    or the activity is cleared (`clear`). When no application has focus
//!    (desktop, taskbar, DeskPresence itself) the most recently focused
//!    eligible running application is used.
//! 5. **Priority mode**: among running eligible applications that have an
//!    enabled rule, the highest priority wins. Ties are broken by: currently
//!    focused first, then most recently focused, then the normalized
//!    executable path (alphabetical). If none is running, the foreground
//!    policy is used when `priority_fallback_to_foreground` is on; otherwise
//!    the activity is cleared.
//! 6. Nothing eligible → the activity is cleared (never left stale).

use std::cmp::Ordering;
use std::time::Duration;

use serde::Serialize;

use crate::app_detection::{DetectedApp, DetectorSnapshot};
use crate::app_identity::{AppKey, ResolvedName, resolve_display_name};
use crate::config::{Config, ExcludedFocusBehavior, SelectionMode, TimestampMode, validate_asset_value};
use crate::discord_rpc::{Activity, ActivityAssets, ActivityTimestamps};
use crate::presence_templates::{
    DISCORD_TEXT_LIMIT, DISCORD_TEXT_MIN, Template, TemplateContext, Variable, discord_len, fit_discord_text,
};
use crate::rules::{AppFacts, AppRule, CompiledRules, Eligibility};

/// Why the current application controls the presence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum SelectionReason {
    /// Manually pinned from the tray or the UI.
    Pinned,
    /// Owns the foreground window.
    Foreground,
    /// Most recently focused eligible application (focus is elsewhere).
    RecentForeground,
    /// Highest priority in priority mode.
    Priority,
    /// Priority mode found nothing and fell back to the foreground policy.
    PriorityFallback,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    pub key: AppKey,
    pub reason: SelectionReason,
}

/// Evaluates eligibility for a running application.
pub fn eligibility(app_key: &AppKey, config: &Config, compiled: &CompiledRules) -> Eligibility {
    compiled.eligibility(&AppFacts::new(app_key), &config.rules)
}

/// The applicable rule for an application, if any.
pub fn rule_for<'a>(app_key: &AppKey, config: &'a Config, compiled: &CompiledRules) -> Option<&'a AppRule> {
    compiled
        .find_rule(&AppFacts::new(app_key))
        .and_then(|m| config.rules.get(m.index))
}

/// Display name for a detected application under the current rules.
pub fn display_name(app: &DetectedApp, rule: Option<&AppRule>) -> ResolvedName {
    resolve_display_name(
        rule.and_then(|r| r.display_name.as_deref()),
        app.metadata.as_ref(),
        &app.exe_name,
    )
}

/// Applies the documented selection policy.
pub fn select_app(
    snapshot: &DetectorSnapshot,
    config: &Config,
    compiled: &CompiledRules,
    pin: Option<&AppKey>,
) -> Option<Selection> {
    let eligible = |app: &DetectedApp| eligibility(&app.key, config, compiled).is_eligible();

    if let Some(pin) = pin
        && let Some(app) = snapshot.app(pin)
        && eligible(app)
    {
        return Some(Selection {
            key: app.key.clone(),
            reason: SelectionReason::Pinned,
        });
    }

    match config.presence.selection_mode {
        SelectionMode::Foreground => foreground_selection(snapshot, config, &eligible),
        SelectionMode::Priority => {
            let best = snapshot
                .apps
                .iter()
                .filter(|app| eligible(app))
                .filter_map(|app| {
                    rule_for(&app.key, config, compiled)
                        .filter(|rule| rule.enabled)
                        .map(|rule| (app, rule.priority))
                })
                .min_by(|(a, pa), (b, pb)| priority_order(*pa, a, *pb, b, snapshot.foreground.as_ref()));
            if let Some((app, _)) = best {
                return Some(Selection {
                    key: app.key.clone(),
                    reason: SelectionReason::Priority,
                });
            }
            if config.presence.priority_fallback_to_foreground {
                foreground_selection(snapshot, config, &eligible).map(|s| Selection {
                    reason: SelectionReason::PriorityFallback,
                    ..s
                })
            } else {
                None
            }
        }
    }
}

/// Ordering used in priority mode (`Less` = preferred).
fn priority_order(
    pa: i32,
    a: &DetectedApp,
    pb: i32,
    b: &DetectedApp,
    foreground: Option<&AppKey>,
) -> Ordering {
    pb.cmp(&pa)
        .then_with(|| {
            let fa = foreground == Some(&a.key);
            let fb = foreground == Some(&b.key);
            fb.cmp(&fa)
        })
        .then_with(|| match (a.last_focused_ms, b.last_focused_ms) {
            (Some(x), Some(y)) => y.cmp(&x),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        })
        .then_with(|| a.key.cmp(&b.key))
}

fn foreground_selection(
    snapshot: &DetectorSnapshot,
    config: &Config,
    eligible: &dyn Fn(&DetectedApp) -> bool,
) -> Option<Selection> {
    let foreground = snapshot.foreground.as_ref();
    if let Some(app) = foreground.and_then(|key| snapshot.app(key)) {
        if eligible(app) {
            return Some(Selection {
                key: app.key.clone(),
                reason: SelectionReason::Foreground,
            });
        }
        if config.presence.excluded_focus == ExcludedFocusBehavior::Clear {
            return None;
        }
    }
    snapshot
        .apps
        .iter()
        .filter(|app| Some(&app.key) != foreground && app.last_focused_ms.is_some() && eligible(app))
        .max_by(|a, b| {
            a.last_focused_ms
                .cmp(&b.last_focused_ms)
                .then_with(|| b.key.cmp(&a.key))
        })
        .map(|app| Selection {
            key: app.key.clone(),
            reason: SelectionReason::RecentForeground,
        })
}

/// Activity fields that can carry warnings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum PresenceField {
    Details,
    State,
    Name,
    LargeImage,
    LargeText,
    SmallImage,
    SmallText,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct PresenceWarning {
    pub field: PresenceField,
    pub message: String,
}

fn warn(warnings: &mut Vec<PresenceWarning>, field: PresenceField, message: impl Into<String>) {
    warnings.push(PresenceWarning {
        field,
        message: message.into(),
    });
}

/// Inputs for rendering one activity.
pub struct BuildContext<'a> {
    pub config: &'a Config,
    pub rule: Option<&'a AppRule>,
    pub display_name: &'a str,
    pub exe_name: &'a str,
    pub window_title: Option<&'a str>,
    /// Time since the application became the presence source.
    pub elapsed: Option<Duration>,
    /// Timestamp start in Unix seconds, if timestamps are enabled.
    pub timestamp_start_s: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuiltActivity {
    pub activity: Activity,
    pub warnings: Vec<PresenceWarning>,
}

/// The effective templates for an application (rule overrides, else global).
pub fn effective_templates<'a>(config: &'a Config, rule: Option<&'a AppRule>) -> (&'a str, &'a str) {
    let details = rule
        .and_then(|r| r.details_template.as_deref())
        .unwrap_or(&config.presence.details_template);
    let state = rule
        .and_then(|r| r.state_template.as_deref())
        .unwrap_or(&config.presence.state_template);
    (details, state)
}

/// Effective hover-text templates (large, small): the rule's override, else
/// the default.
fn effective_hover_templates<'a>(config: &'a Config, rule: Option<&'a AppRule>) -> [Option<&'a str>; 2] {
    let defaults = &config.presence.default_assets;
    let assets = rule.map(|r| &r.assets);
    [
        assets
            .and_then(|a| a.large_text.as_deref())
            .or(defaults.large_text.as_deref()),
        assets
            .and_then(|a| a.small_text.as_deref())
            .or(defaults.small_text.as_deref()),
    ]
}

fn render_field(
    source: &str,
    field: PresenceField,
    ctx: &TemplateContext<'_>,
    warnings: &mut Vec<PresenceWarning>,
) -> Option<String> {
    let template = match Template::parse(source) {
        Ok(t) => t,
        Err(e) => {
            warn(
                warnings,
                field,
                format!("Template error: {e} The field was left out."),
            );
            return None;
        }
    };
    let rendered = template.render(ctx);
    if rendered.window_title_blocked {
        warn(
            warnings,
            field,
            "{window_title} is empty because window-title tracking is off (Settings → Privacy).",
        );
    }
    fit_text(&rendered.text, field, warnings)
}

fn fit_text(text: &str, field: PresenceField, warnings: &mut Vec<PresenceWarning>) -> Option<String> {
    if text.is_empty() {
        return None;
    }
    // The text is not quoted in warnings: it may contain a window title, and
    // warnings appear in diagnostics.
    if discord_len(text) < DISCORD_TEXT_MIN {
        warn(
            warnings,
            field,
            format!(
                "The text is shorter than {DISCORD_TEXT_MIN} characters, which Discord rejects. The field was left out."
            ),
        );
        return None;
    }
    let fitted = fit_discord_text(text, DISCORD_TEXT_LIMIT);
    // A single grapheme cluster longer than the limit (e.g. a letter with
    // hundreds of combining marks) leaves only the ellipsis.
    if discord_len(&fitted.text) < DISCORD_TEXT_MIN {
        warn(
            warnings,
            field,
            "The text could not be shortened to Discord's limit, so the field was left out.",
        );
        return None;
    }
    if fitted.truncated {
        warn(
            warnings,
            field,
            format!(
                "Shortened from {} to {DISCORD_TEXT_LIMIT} characters (Discord's limit).",
                fitted.original_len
            ),
        );
    }
    Some(fitted.text)
}

fn asset_image(
    value: Option<&String>,
    field: PresenceField,
    warnings: &mut Vec<PresenceWarning>,
) -> Option<String> {
    let value = value?;
    match validate_asset_value(value) {
        Ok(()) => Some(value.clone()),
        Err(message) => {
            warn(
                warnings,
                field,
                format!("Artwork “{value}” {message}; it was left out."),
            );
            None
        }
    }
}

/// Builds the activity payload for the selected application.
pub fn build_activity(ctx: &BuildContext<'_>) -> BuiltActivity {
    let mut warnings = Vec::new();
    let config = ctx.config;
    let template_ctx = TemplateContext {
        app: ctx.display_name,
        exe: ctx.exe_name,
        elapsed: ctx.elapsed,
        window_title: ctx.window_title,
        window_titles_enabled: config.privacy.capture_window_titles,
    };
    let (details_src, state_src) = effective_templates(config, ctx.rule);
    let details = render_field(details_src, PresenceField::Details, &template_ctx, &mut warnings);
    let state = render_field(state_src, PresenceField::State, &template_ctx, &mut warnings);

    let defaults = &config.presence.default_assets;
    let rule_assets = ctx.rule.map(|r| &r.assets);
    let pick = |f: fn(&crate::rules::AssetOverrides) -> &Option<String>| {
        rule_assets
            .and_then(|a| f(a).as_ref())
            .or_else(|| f(defaults).as_ref())
    };
    let large_image = asset_image(pick(|a| &a.large_image), PresenceField::LargeImage, &mut warnings);
    let small_image = asset_image(pick(|a| &a.small_image), PresenceField::SmallImage, &mut warnings);
    let mut hover = |text: Option<&String>, image: &Option<String>, field: PresenceField| {
        let text = text?;
        if image.is_none() {
            warn(
                &mut warnings,
                field,
                "Hover text is only shown with an image, so it was left out.",
            );
            return None;
        }
        let rendered = Template::parse(text)
            .map(|t| t.render(&template_ctx).text)
            .unwrap_or_else(|_| text.clone());
        fit_text(&rendered, field, &mut warnings)
    };
    let large_text = hover(pick(|a| &a.large_text), &large_image, PresenceField::LargeText);
    let small_text = hover(pick(|a| &a.small_text), &small_image, PresenceField::SmallText);
    let assets = ActivityAssets {
        large_image,
        large_text,
        small_image,
        small_text,
    };

    let name = if config.discord.override_activity_name {
        fit_text(ctx.display_name, PresenceField::Name, &mut warnings)
    } else {
        None
    };

    BuiltActivity {
        activity: Activity {
            activity_type: config.discord.activity_type.code(),
            name,
            details,
            state,
            status_display_type: config.discord.status_display.code(),
            timestamps: ctx
                .timestamp_start_s
                .map(|start| ActivityTimestamps { start: Some(start) }),
            assets: (!assets.is_empty()).then_some(assets),
        },
        warnings,
    }
}

/// Why a pin was removed automatically.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PinCleared {
    Exited(AppKey),
    Ineligible(AppKey),
}

/// Result of one engine evaluation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineOutput {
    pub selection: Option<Selection>,
    pub display_name: Option<String>,
    /// `None` clears the activity (paused or nothing eligible).
    pub activity: Option<Activity>,
    pub warnings: Vec<PresenceWarning>,
    pub source_since_ms: Option<u64>,
    /// When `{elapsed}` text will next change (Unix ms), if it is used.
    pub next_refresh_ms: Option<u64>,
    pub pin_cleared: Option<PinCleared>,
}

/// Stateful part of presence management: the pin, the pause switch and the
/// timers behind `{elapsed}` and the activity timestamp.
#[derive(Debug, Default)]
pub struct PresenceEngine {
    pin: Option<AppKey>,
    paused: bool,
    source: Option<(AppKey, u64)>,
    session_since_ms: Option<u64>,
}

impl PresenceEngine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn pin(&self) -> Option<&AppKey> {
        self.pin.as_ref()
    }

    pub fn paused(&self) -> bool {
        self.paused
    }

    pub fn set_pin(&mut self, key: Option<AppKey>) {
        self.pin = key;
    }

    /// Pins `key`, or unpins it if it is already pinned (tray behaviour).
    pub fn toggle_pin(&mut self, key: AppKey) {
        if self.pin.as_ref() == Some(&key) {
            self.pin = None;
        } else {
            self.pin = Some(key);
        }
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    pub fn evaluate(
        &mut self,
        snapshot: &DetectorSnapshot,
        config: &Config,
        compiled: &CompiledRules,
        now_ms: u64,
    ) -> EngineOutput {
        let mut pin_cleared = None;
        if let Some(pin) = self.pin.clone() {
            match snapshot.app(&pin) {
                None => {
                    self.pin = None;
                    pin_cleared = Some(PinCleared::Exited(pin));
                }
                Some(_) if !eligibility(&pin, config, compiled).is_eligible() => {
                    self.pin = None;
                    pin_cleared = Some(PinCleared::Ineligible(pin));
                }
                Some(_) => {}
            }
        }

        let selection = select_app(snapshot, config, compiled, self.pin.as_ref());
        let selected_app = selection.as_ref().and_then(|s| snapshot.app(&s.key));

        let Some(app) = selected_app.filter(|_| !self.paused) else {
            self.source = None;
            self.session_since_ms = None;
            let display_name =
                selected_app.map(|app| display_name(app, rule_for(&app.key, config, compiled)).name);
            return EngineOutput {
                selection,
                display_name,
                activity: None,
                warnings: Vec::new(),
                source_since_ms: None,
                next_refresh_ms: None,
                pin_cleared,
            };
        };

        if self.source.as_ref().is_none_or(|(key, _)| *key != app.key) {
            self.source = Some((app.key.clone(), now_ms));
        }
        let source_since = self.source.as_ref().map_or(now_ms, |(_, since)| *since);
        let session_since = *self.session_since_ms.get_or_insert(now_ms);

        let timestamp_ms = match config.presence.timestamp_mode {
            TimestampMode::SinceSelected => Some(source_since),
            TimestampMode::SinceSession => Some(session_since),
            TimestampMode::SinceLaunch => Some(app.started_at_ms.unwrap_or(source_since)),
            TimestampMode::Off => None,
        };

        let rule = rule_for(&app.key, config, compiled);
        let name = display_name(app, rule);
        let elapsed_ms = now_ms.saturating_sub(source_since);
        let built = build_activity(&BuildContext {
            config,
            rule,
            display_name: &name.name,
            exe_name: &app.exe_name,
            window_title: app.window_title.as_deref(),
            elapsed: Some(Duration::from_millis(elapsed_ms)),
            timestamp_start_s: timestamp_ms.map(|ms| ms / 1000),
        });

        let (details_src, state_src) = effective_templates(config, rule);
        let [large_text_src, small_text_src] = effective_hover_templates(config, rule);
        // Hover texts count only when they are sent (they need an image).
        let assets = built.activity.assets.as_ref();
        let large_text_src = large_text_src.filter(|_| assets.is_some_and(|a| a.large_text.is_some()));
        let small_text_src = small_text_src.filter(|_| assets.is_some_and(|a| a.small_text.is_some()));
        let uses_elapsed = [Some(details_src), Some(state_src), large_text_src, small_text_src]
            .into_iter()
            .flatten()
            .any(|src| Template::parse(src).is_ok_and(|t| t.uses(Variable::Elapsed)));
        let next_refresh_ms = uses_elapsed.then(|| {
            let minutes = elapsed_ms / 60_000;
            source_since + (minutes + 1) * 60_000
        });

        EngineOutput {
            selection,
            display_name: Some(name.name),
            activity: Some(built.activity),
            warnings: built.warnings,
            source_since_ms: Some(source_since),
            next_refresh_ms,
            pin_cleared,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_detection::{DetectorStatus, ScanStats};
    use crate::app_identity::ExeMetadata;
    use crate::config::{ActivityType, StatusDisplay};
    use crate::rules::{AppMatcher, AssetOverrides, ExclusionRule};

    fn app(path: &str, focused: Option<u64>) -> DetectedApp {
        let key = AppKey::from_path(path).unwrap();
        DetectedApp {
            exe_name: crate::app_identity::exe_file_name(path),
            exe_path: path.to_owned(),
            key,
            metadata: None,
            pids: vec![1],
            window_count: 1,
            window_title: Some("Doc.txt".into()),
            started_at_ms: Some(5_000),
            first_seen_ms: 0,
            last_focused_ms: focused,
        }
    }

    fn snapshot(mut apps: Vec<DetectedApp>, foreground: Option<&str>) -> DetectorSnapshot {
        apps.sort_by(|a, b| a.key.cmp(&b.key));
        DetectorSnapshot {
            status: DetectorStatus::Running {
                backend: "test".into(),
            },
            apps,
            exited: Vec::new(),
            foreground: foreground.and_then(AppKey::from_path),
            stats: ScanStats::default(),
        }
    }

    fn key(path: &str) -> AppKey {
        AppKey::from_path(path).unwrap()
    }

    fn select(snap: &DetectorSnapshot, config: &Config, pin: Option<&str>) -> Option<Selection> {
        let compiled = CompiledRules::new(&config.rules, &config.exclusions);
        let pin = pin.map(key);
        select_app(snap, config, &compiled, pin.as_ref())
    }

    const A: &str = r"C:\Apps\a.exe";
    const B: &str = r"C:\Apps\b.exe";
    const C: &str = r"C:\Apps\c.exe";

    #[test]
    fn foreground_mode_selects_focused_app() {
        let snap = snapshot(vec![app(A, Some(10)), app(B, Some(20))], Some(B));
        let s = select(&snap, &Config::default(), None).unwrap();
        assert_eq!(s.key, key(B));
        assert_eq!(s.reason, SelectionReason::Foreground);
    }

    #[test]
    fn not_most_recently_discovered_but_focused() {
        // B was discovered later, but A is focused.
        let mut b = app(B, None);
        b.first_seen_ms = 999;
        let snap = snapshot(vec![app(A, Some(5)), b], Some(A));
        assert_eq!(select(&snap, &Config::default(), None).unwrap().key, key(A));
    }

    #[test]
    fn excluded_foreground_keeps_previous_or_clears() {
        let mut config = Config::default();
        config.exclusions.push(ExclusionRule {
            id: "x".into(),
            matcher: AppMatcher::exe_name("b.exe"),
            note: None,
        });
        let snap = snapshot(vec![app(A, Some(10)), app(B, Some(20))], Some(B));
        let s = select(&snap, &config, None).unwrap();
        assert_eq!(s.key, key(A));
        assert_eq!(s.reason, SelectionReason::RecentForeground);

        config.presence.excluded_focus = ExcludedFocusBehavior::Clear;
        assert_eq!(select(&snap, &config, None), None);
    }

    #[test]
    fn disabled_rule_behaves_like_exclusion_for_selection() {
        let mut config = Config::default();
        let mut rule = AppRule::new("r", AppMatcher::exe_name("b.exe"));
        rule.enabled = false;
        config.rules.push(rule);
        let snap = snapshot(vec![app(A, Some(10)), app(B, Some(20))], Some(B));
        assert_eq!(select(&snap, &config, None).unwrap().key, key(A));
    }

    #[test]
    fn no_focus_uses_most_recent_and_nothing_means_none() {
        let snap = snapshot(vec![app(A, Some(10)), app(B, Some(30)), app(C, None)], None);
        let s = select(&snap, &Config::default(), None).unwrap();
        assert_eq!((s.key, s.reason), (key(B), SelectionReason::RecentForeground));

        let never_focused = snapshot(vec![app(A, None)], None);
        assert_eq!(select(&never_focused, &Config::default(), None), None);
        assert_eq!(select(&snapshot(vec![], None), &Config::default(), None), None);
    }

    fn priority_config(rules: &[(&str, i32)]) -> Config {
        let mut config = Config::default();
        config.presence.selection_mode = SelectionMode::Priority;
        for (i, (exe, priority)) in rules.iter().enumerate() {
            let mut rule = AppRule::new(format!("r{i}"), AppMatcher::exe_name(*exe));
            rule.priority = *priority;
            config.rules.push(rule);
        }
        config
    }

    #[test]
    fn priority_mode_orders_by_priority() {
        let config = priority_config(&[("a.exe", 1), ("b.exe", 5), ("c.exe", 3)]);
        let snap = snapshot(vec![app(A, Some(50)), app(B, None), app(C, Some(60))], Some(A));
        let s = select(&snap, &config, None).unwrap();
        assert_eq!((s.key, s.reason), (key(B), SelectionReason::Priority));
    }

    #[test]
    fn priority_ties_break_by_focus_then_recency_then_path() {
        let config = priority_config(&[("a.exe", 1), ("b.exe", 1), ("c.exe", 1)]);
        // Foreground wins a tie.
        let snap = snapshot(
            vec![app(A, Some(10)), app(B, Some(20)), app(C, Some(30))],
            Some(A),
        );
        assert_eq!(select(&snap, &config, None).unwrap().key, key(A));
        // Without focus: most recently focused.
        let snap = snapshot(vec![app(A, Some(10)), app(B, Some(30)), app(C, Some(20))], None);
        assert_eq!(select(&snap, &config, None).unwrap().key, key(B));
        // Never focused: path order, deterministic regardless of input order.
        let snap = snapshot(vec![app(C, None), app(B, None)], None);
        assert_eq!(select(&snap, &config, None).unwrap().key, key(B));
        let snap = snapshot(vec![app(B, None), app(C, None)], None);
        assert_eq!(select(&snap, &config, None).unwrap().key, key(B));
    }

    #[test]
    fn priority_mode_ignores_apps_without_enabled_rules() {
        let mut config = priority_config(&[("a.exe", 9)]);
        config.rules[0].enabled = false;
        let snap = snapshot(vec![app(A, Some(1)), app(B, Some(2))], Some(B));
        assert_eq!(select(&snap, &config, None), None);

        config.presence.priority_fallback_to_foreground = true;
        let s = select(&snap, &config, None).unwrap();
        assert_eq!((s.key, s.reason), (key(B), SelectionReason::PriorityFallback));
    }

    #[test]
    fn exclusion_beats_priority() {
        let mut config = priority_config(&[("a.exe", 100), ("b.exe", 1)]);
        config.exclusions.push(ExclusionRule {
            id: "x".into(),
            matcher: AppMatcher::path(A),
            note: None,
        });
        let snap = snapshot(vec![app(A, None), app(B, None)], None);
        assert_eq!(select(&snap, &config, None).unwrap().key, key(B));
    }

    #[test]
    fn pin_overrides_modes_but_not_exclusions() {
        let snap = snapshot(vec![app(A, Some(1)), app(B, Some(2))], Some(B));
        let s = select(&snap, &Config::default(), Some(A)).unwrap();
        assert_eq!((s.key, s.reason), (key(A), SelectionReason::Pinned));

        let mut config = Config::default();
        config.exclusions.push(ExclusionRule {
            id: "x".into(),
            matcher: AppMatcher::exe_name("a.exe"),
            note: None,
        });
        assert_eq!(select(&snap, &config, Some(A)).unwrap().key, key(B));
    }

    fn evaluate(
        engine: &mut PresenceEngine,
        snap: &DetectorSnapshot,
        config: &Config,
        now: u64,
    ) -> EngineOutput {
        let compiled = CompiledRules::new(&config.rules, &config.exclusions);
        engine.evaluate(snap, config, &compiled, now)
    }

    #[test]
    fn toggle_pin_and_clear_on_exit() {
        let config = Config::default();
        let mut engine = PresenceEngine::new();
        engine.toggle_pin(key(A));
        let snap = snapshot(vec![app(A, None), app(B, Some(2))], Some(B));
        let out = evaluate(&mut engine, &snap, &config, 1_000);
        assert_eq!(out.selection.unwrap().reason, SelectionReason::Pinned);

        // Toggling again returns to automatic selection.
        engine.toggle_pin(key(A));
        let out = evaluate(&mut engine, &snap, &config, 2_000);
        assert_eq!(out.selection.unwrap().key, key(B));

        // A pinned app that exits clears the pin and reports it.
        engine.toggle_pin(key(A));
        let gone = snapshot(vec![app(B, Some(2))], Some(B));
        let out = evaluate(&mut engine, &gone, &config, 3_000);
        assert_eq!(out.pin_cleared, Some(PinCleared::Exited(key(A))));
        assert_eq!(engine.pin(), None);
        assert_eq!(out.selection.unwrap().key, key(B));
    }

    #[test]
    fn pin_on_newly_excluded_app_is_cleared() {
        let mut engine = PresenceEngine::new();
        engine.set_pin(Some(key(A)));
        let mut config = Config::default();
        config.exclusions.push(ExclusionRule {
            id: "x".into(),
            matcher: AppMatcher::exe_name("a.exe"),
            note: None,
        });
        let snap = snapshot(vec![app(A, None)], None);
        let out = evaluate(&mut engine, &snap, &config, 0);
        assert_eq!(out.pin_cleared, Some(PinCleared::Ineligible(key(A))));
        assert_eq!(out.activity, None);
    }

    #[test]
    fn paused_clears_activity_but_reports_selection() {
        let mut engine = PresenceEngine::new();
        engine.set_paused(true);
        let snap = snapshot(vec![app(A, Some(1))], Some(A));
        let out = evaluate(&mut engine, &snap, &Config::default(), 0);
        assert_eq!(out.activity, None);
        assert_eq!(out.selection.unwrap().key, key(A));
        assert_eq!(out.display_name.as_deref(), Some("A"));
    }

    #[test]
    fn builds_activity_with_templates_rule_overrides_and_assets() {
        let mut config = Config::default();
        config.discord.application_id = Some("123456789012345678".into());
        config.discord.activity_type = ActivityType::Watching;
        config.discord.status_display = StatusDisplay::Details;
        config.presence.state_template = "{exe}".into();
        config.presence.default_assets = AssetOverrides {
            large_image: Some("default_logo".into()),
            large_text: Some("{app}".into()),
            ..AssetOverrides::default()
        };
        let mut rule = AppRule::new("r", AppMatcher::exe_name("a.exe"));
        rule.display_name = Some("Alpha".into());
        rule.details_template = Some("Working in {app}".into());
        rule.assets.small_image = Some("alpha_small".into());
        config.rules.push(rule);

        let mut engine = PresenceEngine::new();
        let snap = snapshot(vec![app(A, Some(1))], Some(A));
        let out = evaluate(&mut engine, &snap, &config, 61_000);
        let activity = out.activity.unwrap();
        assert_eq!(activity.activity_type, 3);
        assert_eq!(activity.status_display_type, Some(2));
        assert_eq!(activity.details.as_deref(), Some("Working in Alpha"));
        assert_eq!(activity.state.as_deref(), Some("a.exe"));
        assert_eq!(activity.timestamps, Some(ActivityTimestamps { start: Some(61) }));
        let assets = activity.assets.unwrap();
        assert_eq!(assets.large_image.as_deref(), Some("default_logo"));
        assert_eq!(assets.large_text.as_deref(), Some("Alpha"));
        assert_eq!(assets.small_image.as_deref(), Some("alpha_small"));
        assert_eq!(assets.small_text, None);
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
        assert_eq!(activity.name, None);
    }

    #[test]
    fn invalid_artwork_and_orphan_hover_text_warn_but_keep_presence() {
        let mut config = Config::default();
        config.presence.default_assets = AssetOverrides {
            large_image: Some("http://insecure/x.png".into()),
            large_text: Some("Hover".into()),
            ..AssetOverrides::default()
        };
        let mut engine = PresenceEngine::new();
        let snap = snapshot(vec![app(A, Some(1))], Some(A));
        let out = evaluate(&mut engine, &snap, &config, 0);
        let activity = out.activity.unwrap();
        assert_eq!(activity.assets, None);
        assert_eq!(activity.details.as_deref(), Some("Using A"));
        let fields: Vec<_> = out.warnings.iter().map(|w| w.field).collect();
        assert_eq!(fields, vec![PresenceField::LargeImage, PresenceField::LargeText]);
    }

    #[test]
    fn long_and_short_fields_are_reported() {
        let mut config = Config::default();
        config.presence.details_template = format!("{} {{app}}", "x".repeat(130));
        config.presence.state_template = "!".into();
        let mut engine = PresenceEngine::new();
        let snap = snapshot(vec![app(A, Some(1))], Some(A));
        let out = evaluate(&mut engine, &snap, &config, 0);
        let activity = out.activity.unwrap();
        assert_eq!(discord_len(activity.details.as_deref().unwrap()), 128);
        assert_eq!(activity.state, None);
        assert!(
            out.warnings
                .iter()
                .any(|w| w.field == PresenceField::Details && w.message.contains("Shortened"))
        );
        assert!(
            out.warnings
                .iter()
                .any(|w| w.field == PresenceField::State && w.message.contains("shorter"))
        );
    }

    #[test]
    fn window_title_requires_opt_in() {
        let mut config = Config::default();
        config.presence.details_template = "{window_title}".into();
        let mut engine = PresenceEngine::new();
        let snap = snapshot(vec![app(A, Some(1))], Some(A));
        let out = evaluate(&mut engine, &snap, &config, 0);
        assert_eq!(out.activity.as_ref().unwrap().details, None);
        assert!(out.warnings[0].message.contains("window-title tracking is off"));

        config.privacy.capture_window_titles = true;
        let out = evaluate(&mut engine, &snap, &config, 0);
        assert_eq!(out.activity.unwrap().details.as_deref(), Some("Doc.txt"));
    }

    #[test]
    fn timestamp_modes() {
        let mut config = Config::default();
        let mut engine = PresenceEngine::new();
        let snap_a = snapshot(vec![app(A, Some(1)), app(B, Some(0))], Some(A));
        let snap_b = snapshot(vec![app(A, Some(1)), app(B, Some(2))], Some(B));
        let start = |out: &EngineOutput| {
            out.activity
                .as_ref()
                .and_then(|a| a.timestamps.as_ref())
                .and_then(|t| t.start)
        };

        // Resets when the source changes (default).
        assert_eq!(start(&evaluate(&mut engine, &snap_a, &config, 10_000)), Some(10));
        assert_eq!(start(&evaluate(&mut engine, &snap_a, &config, 20_000)), Some(10));
        assert_eq!(start(&evaluate(&mut engine, &snap_b, &config, 30_000)), Some(30));

        // Session mode keeps counting across switches.
        config.presence.timestamp_mode = TimestampMode::SinceSession;
        let mut engine = PresenceEngine::new();
        evaluate(&mut engine, &snap_a, &config, 10_000);
        assert_eq!(start(&evaluate(&mut engine, &snap_b, &config, 30_000)), Some(10));

        config.presence.timestamp_mode = TimestampMode::SinceLaunch;
        assert_eq!(start(&evaluate(&mut engine, &snap_b, &config, 30_000)), Some(5));

        config.presence.timestamp_mode = TimestampMode::Off;
        assert_eq!(start(&evaluate(&mut engine, &snap_b, &config, 30_000)), None);
    }

    #[test]
    fn elapsed_text_refreshes_on_minute_boundaries() {
        let mut config = Config::default();
        config.presence.state_template = "for {elapsed}".into();
        let mut engine = PresenceEngine::new();
        let snap = snapshot(vec![app(A, Some(1))], Some(A));
        let out = evaluate(&mut engine, &snap, &config, 100_000);
        assert_eq!(out.activity.unwrap().state.as_deref(), Some("for <1m"));
        assert_eq!(out.next_refresh_ms, Some(160_000));
        let out = evaluate(&mut engine, &snap, &config, 160_000);
        assert_eq!(out.activity.unwrap().state.as_deref(), Some("for 1m"));
        assert_eq!(out.next_refresh_ms, Some(220_000));

        config.presence.state_template = "{app}".into();
        assert_eq!(
            evaluate(&mut engine, &snap, &config, 170_000).next_refresh_ms,
            None
        );
    }

    #[test]
    fn elapsed_in_hover_text_also_refreshes() {
        let mut config = Config::default();
        config.presence.default_assets = AssetOverrides {
            large_text: Some("Session: {elapsed}".into()),
            ..AssetOverrides::default()
        };
        let mut engine = PresenceEngine::new();
        let snap = snapshot(vec![app(A, Some(1))], Some(A));
        // Without an image the hover text is not sent, so no refresh is needed.
        assert_eq!(
            evaluate(&mut engine, &snap, &config, 100_000).next_refresh_ms,
            None
        );

        config.presence.default_assets.large_image = Some("logo".into());
        let out = evaluate(&mut engine, &snap, &config, 100_000);
        assert_eq!(out.next_refresh_ms, Some(160_000));
        let out = evaluate(&mut engine, &snap, &config, 160_000);
        let assets = out.activity.unwrap().assets.unwrap();
        assert_eq!(assets.large_text.as_deref(), Some("Session: 1m"));
    }

    #[test]
    fn text_that_cannot_be_shortened_is_left_out_not_sent_as_an_ellipsis() {
        let mut config = Config::default();
        config.presence.details_template = format!("Z{}", "\u{0301}".repeat(150));
        let mut engine = PresenceEngine::new();
        let snap = snapshot(vec![app(A, Some(1))], Some(A));
        let out = evaluate(&mut engine, &snap, &config, 0);
        assert_eq!(out.activity.unwrap().details, None);
        assert!(
            out.warnings
                .iter()
                .any(|w| w.field == PresenceField::Details && w.message.contains("could not be shortened"))
        );
    }

    #[test]
    fn warnings_do_not_quote_rendered_text() {
        let mut config = Config::default();
        config.privacy.capture_window_titles = true;
        config.presence.state_template = "{window_title}".into();
        let mut a = app(A, Some(1));
        a.window_title = Some("X".into());
        let mut engine = PresenceEngine::new();
        let out = evaluate(&mut engine, &snapshot(vec![a], Some(A)), &config, 0);
        assert!(
            out.warnings.iter().all(|w| !w.message.contains('X')),
            "{:?}",
            out.warnings
        );
    }

    #[test]
    fn identical_inputs_produce_identical_activities() {
        let config = Config::default();
        let mut engine = PresenceEngine::new();
        let snap = snapshot(vec![app(A, Some(1))], Some(A));
        let a = evaluate(&mut engine, &snap, &config, 5_000);
        let b = evaluate(&mut engine, &snap, &config, 9_000);
        assert_eq!(
            a.activity, b.activity,
            "unchanged state must not create a new activity"
        );
    }

    #[test]
    fn metadata_and_rule_names_are_used() {
        let mut a = app(A, Some(1));
        a.metadata = Some(ExeMetadata {
            file_description: Some("Alpha Studio".into()),
            product_name: None,
        });
        assert_eq!(display_name(&a, None).name, "Alpha Studio");
        let mut rule = AppRule::new("r", AppMatcher::path(A));
        rule.display_name = Some("My Alpha".into());
        assert_eq!(display_name(&a, Some(&rule)).name, "My Alpha");
    }

    #[test]
    fn activity_name_override_is_opt_in() {
        let mut config = Config::default();
        config.discord.override_activity_name = true;
        let mut engine = PresenceEngine::new();
        let alpha = r"C:\Apps\alpha.exe";
        let snap = snapshot(vec![app(alpha, Some(1))], Some(alpha));
        let out = evaluate(&mut engine, &snap, &config, 0);
        assert_eq!(out.activity.unwrap().name.as_deref(), Some("Alpha"));
    }
}
