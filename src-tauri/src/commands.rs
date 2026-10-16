//! Typed Tauri commands. The webview is treated as untrusted input: every
//! value is validated here even when the UI already validated it, keys are
//! re-normalized, sizes are bounded, and file dialogs, URLs and paths are
//! chosen in Rust rather than accepted from the frontend.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use deskpresence_core::PRODUCT_NAME;
use deskpresence_core::api::{
    AppInfo, BundleInput, CommandError, CommandErrorCode, ConfigEnvelope, ImportPreview, PreviewRequest,
    PreviewResult, RuntimeView, StartupStatus, build_diagnostics_bundle,
};
use deskpresence_core::app_identity::AppKey;
use deskpresence_core::config::{Config, diff_configs, read_config_file, write_atomic, write_config_file};
use deskpresence_core::diagnostics::Redactor;
use deskpresence_core::presence_templates::MAX_TEMPLATE_LEN;
use deskpresence_core::runtime::RuntimeCommand;
use deskpresence_core::time::now_unix_ms;
use serde::Deserialize;
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

use crate::state::{AppState, PendingImport, lock};

pub const EVENT_RUNTIME: &str = "deskpresence://runtime";
pub const EVENT_CONFIG: &str = "deskpresence://config";

type CommandResult<T> = Result<T, CommandError>;

fn internal(message: impl Into<String>) -> CommandError {
    CommandError::new(CommandErrorCode::Internal, message)
}

fn io_error(message: impl Into<String>) -> CommandError {
    CommandError::new(CommandErrorCode::Io, message)
}

fn envelope(state: &AppState) -> ConfigEnvelope {
    let guard = lock(&state.config);
    ConfigEnvelope {
        config: (*guard.config).clone(),
        revision: guard.revision,
        notice: guard.notice.clone(),
    }
}

/// Validates, persists and applies a new configuration. `base_revision`
/// guards against overwriting changes made elsewhere since the UI loaded.
async fn commit(
    app: &AppHandle,
    state: &AppState,
    mut config: Config,
    base_revision: Option<u64>,
) -> CommandResult<ConfigEnvelope> {
    config.normalize();
    let issues = config.validate();
    if !issues.is_empty() {
        return Err(CommandError::invalid(issues));
    }

    let (shared, previous, envelope) = {
        let mut guard = lock(&state.config);
        if let Some(base) = base_revision
            && base != guard.revision
        {
            return Err(CommandError::new(
                CommandErrorCode::Conflict,
                "The configuration changed in the meantime. Reload and try again.",
            ));
        }
        let previous = guard.config.clone();
        if previous.startup.launch_at_login != config.startup.launch_at_login {
            crate::startup::apply(app, config.startup.launch_at_login).map_err(|e| {
                CommandError::new(
                    CommandErrorCode::Unavailable,
                    format!("Could not change the Windows startup setting: {e}"),
                )
            })?;
        }
        guard
            .store
            .save(&config)
            .map_err(|e| io_error(format!("Could not save the configuration: {e}")))?;
        let shared = Arc::new(config);
        guard.config = shared.clone();
        guard.revision += 1;
        guard.notice = None;
        let envelope = ConfigEnvelope {
            config: (*shared).clone(),
            revision: guard.revision,
            notice: None,
        };
        (shared, previous, envelope)
    };

    if previous.diagnostics.log_level != shared.diagnostics.log_level {
        state.logging.set_level(shared.diagnostics.log_level);
    }
    state.runtime.send(RuntimeCommand::ConfigChanged(shared)).await;
    let _ = app.emit(EVENT_CONFIG, &envelope);
    Ok(envelope)
}

#[tauri::command]
pub fn get_app_info(state: State<'_, AppState>) -> AppInfo {
    let view = state.runtime.view();
    AppInfo {
        product_name: PRODUCT_NAME.to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        platform: std::env::consts::OS.to_owned(),
        detection_supported: !matches!(
            view.detector.status,
            deskpresence_core::app_detection::DetectorStatus::Unsupported { .. }
        ),
        has_build_default_app_id: state.default_application_id.is_some(),
        config_path: state.paths.config_file.display().to_string(),
        log_dir: state.paths.log_dir.display().to_string(),
    }
}

#[tauri::command]
pub fn get_config(state: State<'_, AppState>) -> ConfigEnvelope {
    envelope(&state)
}

#[tauri::command]
pub async fn save_config(
    app: AppHandle,
    state: State<'_, AppState>,
    config: Config,
    base_revision: u64,
) -> CommandResult<ConfigEnvelope> {
    commit(&app, &state, config, Some(base_revision)).await
}

#[tauri::command]
pub fn get_runtime_view(state: State<'_, AppState>) -> RuntimeView {
    (*state.runtime.view()).clone()
}

fn parse_key(key: &str) -> CommandResult<AppKey> {
    if key.len() > 4096 {
        return Err(CommandError::new(
            CommandErrorCode::Invalid,
            "Application key is too long.",
        ));
    }
    AppKey::parse(key).ok_or_else(|| CommandError::new(CommandErrorCode::Invalid, "Invalid application key."))
}

#[tauri::command]
pub async fn set_pin(state: State<'_, AppState>, key: Option<String>) -> CommandResult<()> {
    let key = key.as_deref().map(parse_key).transpose()?;
    state.runtime.send(RuntimeCommand::SetPin(key)).await;
    Ok(())
}

#[tauri::command]
pub async fn toggle_pin(state: State<'_, AppState>, key: String) -> CommandResult<()> {
    let key = parse_key(&key)?;
    state.runtime.send(RuntimeCommand::TogglePin(key)).await;
    Ok(())
}

#[tauri::command]
pub async fn set_paused(state: State<'_, AppState>, paused: bool) -> CommandResult<()> {
    state.runtime.send(RuntimeCommand::SetPaused(paused)).await;
    Ok(())
}

#[tauri::command]
pub async fn reconnect_discord(state: State<'_, AppState>) -> CommandResult<()> {
    state.runtime.send(RuntimeCommand::ReconnectDiscord).await;
    Ok(())
}

#[tauri::command]
pub async fn rescan_now(state: State<'_, AppState>) -> CommandResult<()> {
    state.runtime.send(RuntimeCommand::RescanNow).await;
    Ok(())
}

#[tauri::command]
pub fn preview_presence(state: State<'_, AppState>, request: PreviewRequest) -> CommandResult<PreviewResult> {
    let limit = MAX_TEMPLATE_LEN * 4;
    if request.details_template.len() > limit || request.state_template.len() > limit {
        return Err(CommandError::new(
            CommandErrorCode::Invalid,
            "Template is too long.",
        ));
    }
    let config = lock(&state.config).config.clone();
    Ok(state.runtime.preview(&config, &request))
}

/// Maximum icons resolved per request.
const MAX_ICON_KEYS: usize = 100;

#[tauri::command]
pub async fn get_icons(
    state: State<'_, AppState>,
    keys: Vec<String>,
) -> CommandResult<HashMap<String, Option<String>>> {
    let view = state.runtime.view();
    // Paths come from the detector or configuration, never from the webview.
    let items: Vec<(AppKey, String)> = keys
        .iter()
        .take(MAX_ICON_KEYS)
        .filter_map(|k| AppKey::parse(k))
        .filter_map(|key| {
            view.apps
                .iter()
                .find(|a| a.key == key)
                .map(|a| (key, a.exe_path.clone()))
        })
        .collect();
    let results = state.icons.get(items, Duration::from_secs(10)).await;
    Ok(results
        .into_iter()
        .map(|(key, url)| (key.as_str().to_owned(), url.map(|u| u.to_string())))
        .collect())
}

#[tauri::command]
pub fn get_startup_status(app: AppHandle, state: State<'_, AppState>) -> StartupStatus {
    let preference = lock(&state.config).config.startup.launch_at_login;
    crate::startup::status(&app, preference)
}

fn timestamp_slug() -> String {
    now_unix_ms().to_string()
}

async fn pick_save_path(app: &AppHandle, title: &str, file_name: String) -> CommandResult<Option<PathBuf>> {
    let dialog = app
        .dialog()
        .file()
        .set_title(title)
        .add_filter("JSON", &["json"])
        .set_file_name(file_name);
    let picked = tauri::async_runtime::spawn_blocking(move || dialog.blocking_save_file())
        .await
        .map_err(|e| internal(e.to_string()))?;
    picked
        .map(|p| p.into_path().map_err(|e| io_error(e.to_string())))
        .transpose()
}

#[tauri::command]
pub async fn export_config(app: AppHandle, state: State<'_, AppState>) -> CommandResult<Option<String>> {
    let Some(path) = pick_save_path(
        &app,
        "Export DeskPresence configuration",
        "deskpresence-config.json".into(),
    )
    .await?
    else {
        return Ok(None);
    };
    let config = lock(&state.config).config.clone();
    write_config_file(&path, &config).map_err(|e| io_error(e.to_string()))?;
    tracing::info!("configuration exported");
    Ok(Some(path.display().to_string()))
}

#[tauri::command]
pub async fn import_config_preview(
    app: AppHandle,
    state: State<'_, AppState>,
) -> CommandResult<Option<ImportPreview>> {
    let dialog = app
        .dialog()
        .file()
        .set_title("Import DeskPresence configuration")
        .add_filter("JSON", &["json"]);
    let picked = tauri::async_runtime::spawn_blocking(move || dialog.blocking_pick_file())
        .await
        .map_err(|e| internal(e.to_string()))?;
    let Some(path) = picked
        .map(|p| p.into_path().map_err(|e| io_error(e.to_string())))
        .transpose()?
    else {
        return Ok(None);
    };
    let (incoming, report) = read_config_file(&path).map_err(|e| {
        CommandError::new(
            CommandErrorCode::Invalid,
            format!("This file can't be imported: {e}"),
        )
    })?;
    let current = lock(&state.config).config.clone();
    let changes = diff_configs(&current, &incoming);
    let token = format!("{:016x}", fastrand::u64(..));
    {
        let mut imports = lock(&state.imports);
        imports.retain(|_, p| p.created.elapsed() < Duration::from_secs(600));
        if imports.len() >= 4 {
            imports.clear();
        }
        imports.insert(
            token.clone(),
            PendingImport {
                config: incoming,
                created: Instant::now(),
            },
        );
    }
    Ok(Some(ImportPreview {
        token,
        file_name: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        from_version: report.from_version,
        migrated: report.migrated(),
        changes,
    }))
}

#[tauri::command]
pub async fn apply_import(
    app: AppHandle,
    state: State<'_, AppState>,
    token: String,
    base_revision: u64,
) -> CommandResult<ConfigEnvelope> {
    let pending = lock(&state.imports).remove(&token).ok_or_else(|| {
        CommandError::new(
            CommandErrorCode::NotFound,
            "This import has expired. Choose the file again.",
        )
    })?;
    let mut incoming = pending.config;
    {
        let guard = lock(&state.config);
        // Keep first-run state so importing never re-triggers onboarding.
        incoming.ui.onboarding_completed = guard.config.ui.onboarding_completed;
        incoming.ui.tray_hint_dismissed = guard.config.ui.tray_hint_dismissed;
        if let Err(error) = guard.store.backup_current("before-import") {
            tracing::warn!(%error, "could not back up configuration before import");
        }
    }
    let envelope = commit(&app, &state, incoming, Some(base_revision)).await?;
    tracing::info!("configuration imported");
    Ok(envelope)
}

#[tauri::command]
pub async fn reset_config(
    app: AppHandle,
    state: State<'_, AppState>,
    base_revision: u64,
) -> CommandResult<ConfigEnvelope> {
    let mut fresh = Config::default();
    {
        let guard = lock(&state.config);
        fresh.ui.onboarding_completed = guard.config.ui.onboarding_completed;
        fresh.ui.tray_hint_dismissed = guard.config.ui.tray_hint_dismissed;
        if let Err(error) = guard.store.backup_current("before-reset") {
            tracing::warn!(%error, "could not back up configuration before reset");
        }
    }
    let envelope = commit(&app, &state, fresh, Some(base_revision)).await?;
    tracing::info!("configuration reset to defaults");
    Ok(envelope)
}

#[tauri::command]
pub fn open_log_folder(app: AppHandle, state: State<'_, AppState>) -> CommandResult<()> {
    std::fs::create_dir_all(&state.paths.log_dir).map_err(|e| io_error(e.to_string()))?;
    app.opener()
        .open_path(state.paths.log_dir.display().to_string(), None::<&str>)
        .map_err(|e| io_error(format!("Could not open the log folder: {e}")))
}

#[tauri::command]
pub async fn export_diagnostics(
    app: AppHandle,
    state: State<'_, AppState>,
    include_sensitive: bool,
) -> CommandResult<Option<String>> {
    let file_name = format!("deskpresence-diagnostics-{}.json", timestamp_slug());
    let Some(path) = pick_save_path(&app, "Export diagnostics", file_name).await? else {
        return Ok(None);
    };
    state.logging.flush();
    let log_tail = crate::logging::read_tail(state.logging.log_path(), 256 * 1024);
    let config = lock(&state.config).config.clone();
    let view = state.runtime.view();
    let redactor = Redactor::from_env();
    let os = format!("{} {}", std::env::consts::OS, std::env::consts::ARCH);
    let bundle = build_diagnostics_bundle(&BundleInput {
        app_version: env!("CARGO_PKG_VERSION"),
        os: &os,
        generated_at_ms: now_unix_ms(),
        config: &config,
        view: &view,
        log_tail: &log_tail,
        include_sensitive,
        redactor: &redactor,
    });
    let json = serde_json::to_vec_pretty(&bundle).map_err(|e| internal(e.to_string()))?;
    write_atomic(&path, &json).map_err(|e| io_error(e.to_string()))?;
    tracing::info!(include_sensitive, "diagnostics exported");
    Ok(Some(path.display().to_string()))
}

/// Links the UI may open. URLs are fixed here, never supplied by the webview.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalLink {
    DeveloperPortal,
    RichPresenceDocs,
    RpcDocs,
}

impl ExternalLink {
    fn url(self) -> &'static str {
        match self {
            ExternalLink::DeveloperPortal => "https://discord.com/developers/applications",
            ExternalLink::RichPresenceDocs => "https://discord.com/developers/docs/rich-presence/overview",
            ExternalLink::RpcDocs => "https://discord.com/developers/docs/topics/rpc",
        }
    }
}

#[tauri::command]
pub fn open_external(app: AppHandle, link: ExternalLink) -> CommandResult<()> {
    app.opener()
        .open_url(link.url(), None::<&str>)
        .map_err(|e| io_error(format!("Could not open the link: {e}")))
}

#[tauri::command]
pub fn quit_app(app: AppHandle) {
    crate::shutdown::request_exit(&app);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_links_are_fixed_https_urls() {
        for link in [
            ExternalLink::DeveloperPortal,
            ExternalLink::RichPresenceDocs,
            ExternalLink::RpcDocs,
        ] {
            assert!(link.url().starts_with("https://discord.com/developers/"));
        }
        let parsed: ExternalLink = serde_json::from_str("\"developer_portal\"").unwrap();
        assert!(matches!(parsed, ExternalLink::DeveloperPortal));
        assert!(serde_json::from_str::<ExternalLink>("\"https://evil.example\"").is_err());
    }

    #[test]
    fn keys_are_validated() {
        assert!(parse_key(r"C:\Apps\a.exe").is_ok());
        assert!(parse_key("   ").is_err());
        assert!(parse_key(&"x".repeat(5000)).is_err());
    }
}
