//! DeskPresence desktop shell: tray, configuration window and OS integration
//! around `deskpresence-core`.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod commands;
mod logging;
mod shutdown;
mod startup;
mod state;
mod tray;
mod window;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use deskpresence_core::api::{LoadNotice, LoadNoticeKind};
use deskpresence_core::app_detection::platform_window_system;
use deskpresence_core::config::{ConfigStore, LoadOutcome};
use deskpresence_core::diagnostics::{EventCategory, EventLevel};
use deskpresence_core::discord_rpc::PlatformConnector;
use deskpresence_core::icon_cache::{IconService, platform_extractor};
use deskpresence_core::runtime::{Runtime, RuntimeCommand, RuntimeOptions};
use tauri::{App, AppHandle, Emitter, Manager, RunEvent, WindowEvent};

use crate::startup::LaunchArgs;
use crate::state::{AppPaths, AppState, ConfigState, lock};

/// Optional public Discord application ID baked in at build time:
/// `DESKPRESENCE_DEFAULT_APP_ID=123… cargo tauri build`. Users can override it.
/// It is a public identifier, not a secret.
const DEFAULT_APP_ID: Option<&str> = option_env!("DESKPRESENCE_DEFAULT_APP_ID");

pub fn run() {
    let started = Instant::now();
    let launch = LaunchArgs::parse(std::env::args());

    let app = tauri::Builder::default()
        // Must be registered first: a second launch is forwarded here and exits.
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            let second = LaunchArgs::parse(&args);
            if second.autostart {
                // Windows started us twice (e.g. duplicate startup entries): stay quiet.
                tracing::info!("ignored duplicate launch from startup");
            } else {
                window::show_main(app);
            }
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![startup::AUTOSTART_ARG]),
        ))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            commands::get_app_info,
            commands::get_config,
            commands::save_config,
            commands::get_runtime_view,
            commands::set_pin,
            commands::toggle_pin,
            commands::set_paused,
            commands::reconnect_discord,
            commands::rescan_now,
            commands::preview_presence,
            commands::get_icons,
            commands::get_startup_status,
            commands::export_config,
            commands::import_config_preview,
            commands::apply_import,
            commands::reset_config,
            commands::open_log_folder,
            commands::export_diagnostics,
            commands::open_external,
            commands::quit_app,
        ])
        .setup(move |app| {
            setup(app, launch, started)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event
                && window.label() == window::MAIN_WINDOW
            {
                let app = window.app_handle();
                let state = app.state::<AppState>();
                if state.shutting_down.load(Ordering::SeqCst) {
                    return;
                }
                let close_to_tray = lock(&state.config).config.window.close_to_tray;
                api.prevent_close();
                if close_to_tray {
                    let _ = window.hide();
                } else {
                    shutdown::request_exit(app);
                }
            }
        })
        .build(tauri::generate_context!());

    let app = match app {
        Ok(app) => app,
        Err(error) => {
            eprintln!("DeskPresence failed to start: {error}");
            tracing::error!(%error, "failed to start");
            std::process::exit(1);
        }
    };

    app.run(|handle, event| {
        if let RunEvent::ExitRequested { api, .. } = event {
            let ready = handle
                .try_state::<AppState>()
                .is_none_or(|s| s.exit_ready.load(Ordering::SeqCst));
            if !ready {
                // Finish the clean shutdown first; it calls exit again when done.
                api.prevent_exit();
                shutdown::request_exit(handle);
            }
        }
    });
}

fn resolve_paths(app: &App) -> tauri::Result<AppPaths> {
    let resolver = app.path();
    let config_dir = resolver.app_config_dir()?;
    let log_dir = resolver.app_log_dir()?;
    let cache_dir: PathBuf = resolver.app_cache_dir()?;
    Ok(AppPaths {
        config_file: config_dir.join("config.json"),
        log_dir,
        icon_cache_dir: cache_dir.join("icons"),
    })
}

fn setup(app: &mut App, launch: LaunchArgs, started: Instant) -> Result<(), Box<dyn std::error::Error>> {
    let paths = resolve_paths(app)?;
    let logging = logging::init(&paths.log_dir, Default::default());
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        ?launch,
        "DeskPresence starting"
    );

    // Configuration: never fatal. Problems fall back to defaults with a notice.
    let mut store = ConfigStore::new(paths.config_file.clone());
    let outcome = store.load();
    let notice = match &outcome {
        LoadOutcome::Fresh(_) => None,
        LoadOutcome::Loaded {
            migration: Some(report),
            backup,
            ..
        } => Some(LoadNotice {
            kind: LoadNoticeKind::Migrated,
            message: format!(
                "Your settings were upgraded from schema version {} to {}.",
                report.from_version, report.to_version
            ),
            backup_path: backup.as_ref().map(|p| p.display().to_string()),
        }),
        LoadOutcome::Loaded { .. } => None,
        LoadOutcome::Recovered { error, backup, .. } => {
            tracing::error!(%error, "configuration could not be loaded; using defaults");
            Some(LoadNotice {
                kind: LoadNoticeKind::Recovered,
                message: format!(
                    "Your settings could not be read ({error}). DeskPresence is using default settings."
                ),
                backup_path: backup.as_ref().map(|p| p.display().to_string()),
            })
        }
    };
    let config = Arc::new(outcome.config().clone());
    logging.set_level(config.diagnostics.log_level);

    // Services. Runtime::start needs the Tokio runtime context.
    let runtime = {
        let _guard = tauri::async_runtime::handle().inner().enter();
        Runtime::start(
            RuntimeOptions {
                config: config.clone(),
                default_application_id: DEFAULT_APP_ID.map(str::to_owned),
                pid: std::process::id(),
            },
            platform_window_system(),
            PlatformConnector::default(),
        )?
    };
    let runtime_handle = runtime.handle();
    let icons = IconService::spawn(paths.icon_cache_dir.clone(), platform_extractor)?;

    if let Some(notice) = &notice {
        runtime_handle.try_send(RuntimeCommand::PushEvent {
            level: match notice.kind {
                LoadNoticeKind::Migrated => EventLevel::Info,
                LoadNoticeKind::Recovered => EventLevel::Error,
            },
            category: EventCategory::Config,
            message: notice.message.clone(),
        });
    }

    app.manage(AppState {
        paths,
        config: Mutex::new(ConfigState {
            store,
            config: config.clone(),
            revision: 1,
            notice,
        }),
        runtime: runtime_handle.clone(),
        runtime_owner: Mutex::new(Some(runtime)),
        icons,
        logging,
        imports: Mutex::new(HashMap::new()),
        tray: tray::TrayController::default(),
        shutting_down: AtomicBool::new(false),
        exit_ready: AtomicBool::new(false),
        default_application_id: DEFAULT_APP_ID,
    });

    let handle = app.handle().clone();

    // Refresh the startup registration (keeps the path current after updates).
    if config.startup.launch_at_login
        && let Err(error) = startup::apply(&handle, true)
    {
        tracing::warn!(%error, "could not refresh launch-at-login registration");
    }

    let state = handle.state::<AppState>();
    state.tray.create(&handle, &runtime_handle.view())?;
    spawn_view_forwarder(handle.clone());

    let show_window = !config.ui.onboarding_completed
        || !(launch.autostart || launch.minimized || config.startup.start_minimized);
    if show_window {
        window::show_main(&handle);
    }

    let elapsed = started.elapsed().as_millis();
    tracing::info!(startup_ms = elapsed as u64, "DeskPresence started");
    runtime_handle.try_send(RuntimeCommand::PushEvent {
        level: EventLevel::Info,
        category: EventCategory::App,
        message: format!("DeskPresence started in {elapsed} ms."),
    });
    Ok(())
}

/// Forwards runtime changes to the UI (one event per meaningful change) and
/// to the tray (on the main thread, where native menus live).
fn spawn_view_forwarder(app: AppHandle) {
    let mut views = app.state::<AppState>().runtime.subscribe();
    tauri::async_runtime::spawn(async move {
        loop {
            let view = views.borrow_and_update().clone();
            let _ = app.emit(commands::EVENT_RUNTIME, &*view);
            let tray_app = app.clone();
            let _ = app.run_on_main_thread(move || {
                tray_app.state::<AppState>().tray.update(&tray_app, &view);
            });
            if views.changed().await.is_err() {
                break;
            }
        }
    });
}
