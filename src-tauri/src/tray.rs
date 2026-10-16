//! Native system-tray icon and context menu.
//!
//! The menu lists running, eligible applications from the live detector,
//! followed by Pause, Configure and Exit. Clicking an application pins it
//! (a check mark shows the pin); clicking it again returns to automatic
//! selection.
//!
//! To avoid flicker or resetting a menu the user is looking at, the tray is
//! updated only when its *model* changes. If only check states or the status
//! line change, items are updated in place; the menu is rebuilt only when the
//! list of applications changes.

use std::sync::Mutex;

use deskpresence_core::PRODUCT_NAME;
use deskpresence_core::api::{AppRuleStatus, RuntimeView};
use deskpresence_core::app_identity::AppKey;
use deskpresence_core::discord_rpc::ConnectionPhase;
use deskpresence_core::presence_manager::SelectionReason;
use deskpresence_core::runtime::RuntimeCommand;
use tauri::image::Image;
use tauri::menu::{CheckMenuItem, IsMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Wry};
use unicode_segmentation::UnicodeSegmentation;

use crate::state::AppState;

/// Maximum applications listed directly in the menu.
pub const MAX_TRAY_APPS: usize = 15;
/// Maximum visible characters for an application label.
pub const MAX_LABEL_GRAPHEMES: usize = 40;
/// Windows limits notification-area tooltips to 127 characters.
const MAX_TOOLTIP_CHARS: usize = 127;

const ID_STATUS: &str = "status";
const ID_PAUSE: &str = "pause";
const ID_CONFIGURE: &str = "configure";
const ID_EXIT: &str = "exit";
const ID_MORE: &str = "more";
const APP_PREFIX: &str = "app:";

const ICON_ACTIVE: &[u8] = include_bytes!("../icons/tray/active.png");
const ICON_IDLE: &[u8] = include_bytes!("../icons/tray/idle.png");

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayAppEntry {
    pub key: String,
    pub label: String,
    pub pinned: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayModel {
    pub status: String,
    pub tooltip: String,
    pub apps: Vec<TrayAppEntry>,
    pub overflow: usize,
    pub paused: bool,
    /// Grey icon: paused, disconnected or nothing shown.
    pub idle: bool,
}

impl TrayModel {
    /// Whether the menu structure (and therefore the native menu) must be rebuilt.
    fn same_structure(&self, other: &TrayModel) -> bool {
        self.overflow == other.overflow
            && self.apps.len() == other.apps.len()
            && self
                .apps
                .iter()
                .zip(&other.apps)
                .all(|(a, b)| a.key == b.key && a.label == b.label)
    }
}

/// Truncates on grapheme boundaries and escapes `&` (a mnemonic marker in
/// Windows menus) so names display literally.
pub fn menu_label(name: &str, escape_ampersand: bool) -> String {
    let trimmed = name.trim();
    let graphemes: Vec<&str> = trimmed.graphemes(true).collect();
    let mut label = if graphemes.len() > MAX_LABEL_GRAPHEMES {
        let mut s: String = graphemes[..MAX_LABEL_GRAPHEMES - 1].concat();
        s = s.trim_end().to_owned();
        s.push('…');
        s
    } else {
        trimmed.to_owned()
    };
    if escape_ampersand {
        label = label.replace('&', "&&");
    }
    label
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        let mut s: String = text.chars().take(max.saturating_sub(1)).collect();
        s.push('…');
        s
    }
}

fn status_line(view: &RuntimeView) -> String {
    let selected = view.presence.selected.as_ref();
    let name = selected.map(|s| s.display_name.as_str());
    if view.presence.paused {
        return "Paused — presence hidden".to_owned();
    }
    match view.discord.phase {
        ConnectionPhase::NotConfigured => "Set up Discord in Configure…".to_owned(),
        ConnectionPhase::InvalidApplicationId => "Discord rejected the Application ID".to_owned(),
        ConnectionPhase::Connecting => "Connecting to Discord…".to_owned(),
        ConnectionPhase::WaitingToRetry => {
            if view.discord.last_error_kind
                == Some(deskpresence_core::discord_rpc::DiscordErrorKind::NotRunning)
            {
                "Discord is not running".to_owned()
            } else {
                "Discord unavailable — retrying".to_owned()
            }
        }
        ConnectionPhase::Stopped => "Stopped".to_owned(),
        ConnectionPhase::Connected => match (name, selected.map(|s| s.reason)) {
            (Some(name), Some(SelectionReason::Pinned)) => format!("Showing {name} (pinned)"),
            (Some(name), _) => format!("Showing {name}"),
            (None, _) => "Nothing to show".to_owned(),
        },
    }
}

/// Builds the tray model from the runtime view. Pure, so it is unit tested.
pub fn model_from_view(view: &RuntimeView, escape_ampersand: bool) -> TrayModel {
    let mut eligible: Vec<_> = view
        .apps
        .iter()
        .filter(|a| a.running && matches!(a.status, AppRuleStatus::Automatic | AppRuleStatus::Configured))
        .collect();
    eligible.sort_by(|a, b| {
        a.display_name
            .to_lowercase()
            .cmp(&b.display_name.to_lowercase())
            .then_with(|| a.key.cmp(&b.key))
    });
    let overflow = eligible.len().saturating_sub(MAX_TRAY_APPS);
    let apps = eligible
        .into_iter()
        .take(MAX_TRAY_APPS)
        .map(|a| TrayAppEntry {
            key: a.key.as_str().to_owned(),
            label: menu_label(&a.display_name, escape_ampersand),
            pinned: a.is_pinned,
        })
        .collect();
    let status = status_line(view);
    let showing = view.discord.phase == ConnectionPhase::Connected
        && view.presence.activity.is_some()
        && !view.presence.paused;
    TrayModel {
        tooltip: truncate_chars(&format!("{PRODUCT_NAME} — {status}"), MAX_TOOLTIP_CHARS),
        status: truncate_chars(&status, 60),
        apps,
        overflow,
        paused: view.presence.paused,
        idle: !showing,
    }
}

struct TrayItems {
    status: MenuItem<Wry>,
    apps: Vec<CheckMenuItem<Wry>>,
    pause: CheckMenuItem<Wry>,
}

struct TrayInner {
    icon: TrayIcon<Wry>,
    model: TrayModel,
    items: TrayItems,
}

/// Owns the tray icon. All methods must run on the main thread.
#[derive(Default)]
pub struct TrayController {
    inner: Mutex<Option<TrayInner>>,
}

fn build_menu(app: &AppHandle, model: &TrayModel) -> tauri::Result<(Menu<Wry>, TrayItems)> {
    let status = MenuItem::with_id(app, ID_STATUS, &model.status, false, None::<&str>)?;
    let mut app_items = Vec::with_capacity(model.apps.len());
    for entry in &model.apps {
        app_items.push(CheckMenuItem::with_id(
            app,
            format!("{APP_PREFIX}{}", entry.key),
            &entry.label,
            true,
            entry.pinned,
            None::<&str>,
        )?);
    }
    let more = (model.overflow > 0)
        .then(|| {
            MenuItem::with_id(
                app,
                ID_MORE,
                format!("{} more in Configure…", model.overflow),
                false,
                None::<&str>,
            )
        })
        .transpose()?;
    let empty = model
        .apps
        .is_empty()
        .then(|| MenuItem::with_id(app, ID_MORE, "No applications detected", false, None::<&str>))
        .transpose()?;
    let pause = CheckMenuItem::with_id(app, ID_PAUSE, "Pause presence", true, model.paused, None::<&str>)?;
    let configure = MenuItem::with_id(app, ID_CONFIGURE, "Configure…", true, None::<&str>)?;
    let exit = MenuItem::with_id(app, ID_EXIT, "Exit", true, None::<&str>)?;
    let separator_a = PredefinedMenuItem::separator(app)?;
    let separator_b = PredefinedMenuItem::separator(app)?;
    let separator_c = PredefinedMenuItem::separator(app)?;

    let mut items: Vec<&dyn IsMenuItem<Wry>> = vec![&status, &separator_a];
    for item in &app_items {
        items.push(item);
    }
    if let Some(more) = &more {
        items.push(more);
    }
    if let Some(empty) = &empty {
        items.push(empty);
    }
    items.extend([
        &separator_b as &dyn IsMenuItem<Wry>,
        &pause,
        &separator_c,
        &configure,
        &exit,
    ]);
    let menu = Menu::with_items(app, &items)?;
    Ok((
        menu,
        TrayItems {
            status,
            apps: app_items,
            pause,
        },
    ))
}

fn icon_for(idle: bool) -> Option<Image<'static>> {
    Image::from_bytes(if idle { ICON_IDLE } else { ICON_ACTIVE }).ok()
}

impl TrayController {
    pub fn create(&self, app: &AppHandle, view: &RuntimeView) -> tauri::Result<()> {
        let model = model_from_view(view, cfg!(windows));
        let (menu, items) = build_menu(app, &model)?;
        let mut builder = TrayIconBuilder::with_id("main")
            .tooltip(&model.tooltip)
            .menu(&menu)
            .show_menu_on_left_click(false)
            .on_menu_event(handle_menu_event)
            .on_tray_icon_event(|tray, event| {
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } = event
                {
                    crate::window::show_main(tray.app_handle());
                }
            });
        if let Some(icon) = icon_for(model.idle).or_else(|| app.default_window_icon().cloned()) {
            builder = builder.icon(icon);
        }
        let icon = builder.build(app)?;
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        *inner = Some(TrayInner { icon, model, items });
        Ok(())
    }

    /// Applies a new runtime view. Cheap when nothing visible changed.
    pub fn update(&self, app: &AppHandle, view: &RuntimeView) {
        let model = model_from_view(view, cfg!(windows));
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(inner) = guard.as_mut() else {
            return;
        };
        if inner.model == model {
            return;
        }
        let result: tauri::Result<()> = (|| {
            if inner.model.same_structure(&model) {
                if inner.model.status != model.status {
                    inner.items.status.set_text(&model.status)?;
                }
                for (item, entry) in inner.items.apps.iter().zip(&model.apps) {
                    item.set_checked(entry.pinned)?;
                }
                inner.items.pause.set_checked(model.paused)?;
            } else {
                let (menu, items) = build_menu(app, &model)?;
                inner.icon.set_menu(Some(menu))?;
                inner.items = items;
            }
            if inner.model.tooltip != model.tooltip {
                inner.icon.set_tooltip(Some(&model.tooltip))?;
            }
            if inner.model.idle != model.idle {
                inner.icon.set_icon(icon_for(model.idle))?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => inner.model = model,
            Err(error) => tracing::warn!(%error, "failed to update the tray menu"),
        }
    }

    /// Removes the tray icon (on exit).
    pub fn remove(&self) {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(inner) = guard.take() {
            let _ = inner.icon.set_visible(false);
        }
    }
}

fn handle_menu_event(app: &AppHandle, event: MenuEvent) {
    let id = event.id().as_ref();
    let state = app.state::<AppState>();
    match id {
        ID_CONFIGURE => crate::window::show_main(app),
        ID_EXIT => crate::shutdown::request_exit(app),
        ID_PAUSE => {
            let paused = state.runtime.view().presence.paused;
            if !state.runtime.try_send(RuntimeCommand::SetPaused(!paused)) {
                tracing::warn!("could not toggle pause: runtime busy");
            }
        }
        _ => {
            if let Some(key) = id.strip_prefix(APP_PREFIX).and_then(AppKey::parse)
                && !state.runtime.try_send(RuntimeCommand::TogglePin(key))
            {
                tracing::warn!("could not toggle pin: runtime busy");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deskpresence_core::api::{
        AppIdSource, AppView, ApplicationIdView, DetectorView, PresenceView, SelectedView,
    };
    use deskpresence_core::app_detection::{DetectorStatus, ScanStats};
    use deskpresence_core::app_identity::NameSource;
    use deskpresence_core::config::SelectionMode;
    use deskpresence_core::discord_rpc::{Activity, ActivityDelivery, DiscordStatus};

    fn app(name: &str, path: &str, status: AppRuleStatus, running: bool, pinned: bool) -> AppView {
        AppView {
            key: AppKey::from_path(path).unwrap(),
            display_name: name.into(),
            name_source: NameSource::Metadata,
            exe_name: "x.exe".into(),
            exe_path: path.into(),
            running,
            is_foreground: false,
            is_selected: false,
            is_pinned: pinned,
            window_count: 1,
            first_seen_ms: None,
            last_seen_ms: None,
            last_focused_ms: None,
            status,
            rule_id: None,
            rule_match: None,
            exclusion_id: None,
            priority: 0,
            path_exists: None,
        }
    }

    fn view(apps: Vec<AppView>, phase: ConnectionPhase) -> RuntimeView {
        RuntimeView {
            revision: 1,
            started_at_ms: 0,
            detector: DetectorView {
                status: DetectorStatus::Running {
                    backend: "test".into(),
                },
                stats: ScanStats::default(),
                running_count: 0,
            },
            apps,
            presence: PresenceView {
                paused: false,
                mode: SelectionMode::Foreground,
                pinned: None,
                selected: Some(SelectedView {
                    key: AppKey::from_path(r"C:\b.exe").unwrap(),
                    display_name: "Brave".into(),
                    reason: SelectionReason::Foreground,
                }),
                activity: Some(Activity {
                    activity_type: 0,
                    name: None,
                    details: Some("Using Brave".into()),
                    state: None,
                    status_display_type: None,
                    timestamps: None,
                    assets: None,
                }),
                warnings: vec![],
                source_since_ms: None,
            },
            discord: DiscordStatus {
                phase,
                endpoint: None,
                user_name: None,
                failed_attempts: 0,
                reconnects: 0,
                last_attempt_at_ms: None,
                next_retry_at_ms: None,
                connected_since_ms: None,
                last_error: None,
                last_error_kind: None,
                discord_installed: None,
                delivery: ActivityDelivery::Accepted,
                last_delivery_error: None,
                last_activity_update_at_ms: None,
            },
            application_id: ApplicationIdView {
                effective: Some("123456789012345678".into()),
                source: AppIdSource::Config,
            },
            events: vec![],
        }
    }

    #[test]
    fn labels_are_truncated_and_escaped() {
        assert_eq!(menu_label("  Zed  ", true), "Zed");
        assert_eq!(menu_label("Tom & Jerry", true), "Tom && Jerry");
        assert_eq!(menu_label("Tom & Jerry", false), "Tom & Jerry");
        let long = "A very long application name that keeps going and going";
        let label = menu_label(long, false);
        assert_eq!(label.graphemes(true).count(), MAX_LABEL_GRAPHEMES);
        assert!(label.ends_with('…'));
        let emoji = "👨‍👩‍👧‍👦".repeat(50);
        assert_eq!(
            menu_label(&emoji, false).graphemes(true).count(),
            MAX_LABEL_GRAPHEMES
        );
    }

    #[test]
    fn lists_running_eligible_apps_sorted_with_pin_state() {
        let v = view(
            vec![
                app("Zed", r"C:\z.exe", AppRuleStatus::Automatic, true, false),
                app("Brave", r"C:\b.exe", AppRuleStatus::Configured, true, true),
                app("Hidden", r"C:\h.exe", AppRuleStatus::Excluded, true, false),
                app("Off", r"C:\o.exe", AppRuleStatus::Disabled, true, false),
                app("Closed", r"C:\c.exe", AppRuleStatus::Automatic, false, false),
            ],
            ConnectionPhase::Connected,
        );
        let model = model_from_view(&v, true);
        let labels: Vec<_> = model.apps.iter().map(|a| a.label.as_str()).collect();
        assert_eq!(labels, vec!["Brave", "Zed"]);
        assert!(model.apps[0].pinned);
        assert_eq!(model.status, "Showing Brave");
        assert!(!model.idle);
        assert!(model.tooltip.starts_with("DeskPresence — "));
    }

    #[test]
    fn overflow_is_counted() {
        let apps = (0..20)
            .map(|i| {
                app(
                    &format!("App {i:02}"),
                    &format!(r"C:\a{i}.exe"),
                    AppRuleStatus::Automatic,
                    true,
                    false,
                )
            })
            .collect();
        let model = model_from_view(&view(apps, ConnectionPhase::Connected), true);
        assert_eq!(model.apps.len(), MAX_TRAY_APPS);
        assert_eq!(model.overflow, 5);
    }

    #[test]
    fn check_state_changes_do_not_require_a_rebuild() {
        let a = model_from_view(
            &view(
                vec![app("Zed", r"C:\z.exe", AppRuleStatus::Automatic, true, false)],
                ConnectionPhase::Connected,
            ),
            true,
        );
        let b = model_from_view(
            &view(
                vec![app("Zed", r"C:\z.exe", AppRuleStatus::Automatic, true, true)],
                ConnectionPhase::Connected,
            ),
            true,
        );
        assert_ne!(a, b);
        assert!(a.same_structure(&b));
        let c = model_from_view(
            &view(
                vec![
                    app("Zed", r"C:\z.exe", AppRuleStatus::Automatic, true, false),
                    app("New", r"C:\n.exe", AppRuleStatus::Automatic, true, false),
                ],
                ConnectionPhase::Connected,
            ),
            true,
        );
        assert!(!a.same_structure(&c));
    }

    #[test]
    fn status_reflects_connection_and_pause() {
        let mut v = view(vec![], ConnectionPhase::NotConfigured);
        assert_eq!(model_from_view(&v, true).status, "Set up Discord in Configure…");
        assert!(model_from_view(&v, true).idle);
        v.presence.paused = true;
        assert_eq!(model_from_view(&v, true).status, "Paused — presence hidden");
        v.presence.paused = false;
        v.discord.phase = ConnectionPhase::WaitingToRetry;
        v.discord.last_error_kind = Some(deskpresence_core::discord_rpc::DiscordErrorKind::NotRunning);
        assert_eq!(model_from_view(&v, true).status, "Discord is not running");
    }

    #[test]
    fn tooltip_respects_windows_limit() {
        let mut v = view(vec![], ConnectionPhase::Connected);
        if let Some(s) = v.presence.selected.as_mut() {
            s.display_name = "x".repeat(300);
        }
        assert!(model_from_view(&v, true).tooltip.chars().count() <= MAX_TOOLTIP_CHARS);
    }
}
