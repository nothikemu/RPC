// Declares the application's own commands so each one gets an explicit
// permission (`allow-<command>`) that capabilities must grant. Commands not
// listed in `capabilities/` cannot be invoked from the webview.
const COMMANDS: &[&str] = &[
    "get_app_info",
    "get_config",
    "save_config",
    "get_runtime_view",
    "set_pin",
    "toggle_pin",
    "set_paused",
    "reconnect_discord",
    "rescan_now",
    "preview_presence",
    "get_icons",
    "get_startup_status",
    "export_config",
    "import_config_preview",
    "apply_import",
    "reset_config",
    "open_log_folder",
    "export_diagnostics",
    "open_external",
    "quit_app",
];

fn main() {
    tauri_build::try_build(
        tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .unwrap_or_else(|error| panic!("tauri build script failed: {error:#}"));
}
