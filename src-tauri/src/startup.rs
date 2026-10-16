//! Launch at Windows sign-in, via the official Tauri autostart plugin. On
//! Windows it writes a value under
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` (no administrator
//! rights). DeskPresence is launched with `--autostart`, which starts it in
//! the tray. The registry is the source of truth; users can also disable the
//! entry from Task Manager → Startup apps.

use deskpresence_core::api::StartupStatus;
use tauri::AppHandle;
use tauri_plugin_autostart::ManagerExt;

pub const AUTOSTART_ARG: &str = "--autostart";
pub const MINIMIZED_ARG: &str = "--minimized";

pub fn status(app: &AppHandle, preference: bool) -> StartupStatus {
    match app.autolaunch().is_enabled() {
        Ok(registered) => StartupStatus {
            registered,
            preference,
            error: None,
        },
        Err(error) => StartupStatus {
            registered: false,
            preference,
            error: Some(error.to_string()),
        },
    }
}

pub fn apply(app: &AppHandle, enabled: bool) -> Result<(), String> {
    let manager = app.autolaunch();
    let result = if enabled {
        manager.enable()
    } else {
        manager.disable()
    };
    match result {
        Ok(()) => {
            tracing::info!(enabled, "updated launch-at-login registration");
            Ok(())
        }
        // Disabling an entry that does not exist is not an error.
        Err(_) if !enabled && !manager.is_enabled().unwrap_or(true) => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

/// How the process was launched.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LaunchArgs {
    pub autostart: bool,
    pub minimized: bool,
}

impl LaunchArgs {
    pub fn parse<I: IntoIterator<Item = S>, S: AsRef<str>>(args: I) -> Self {
        let mut parsed = LaunchArgs::default();
        for arg in args {
            match arg.as_ref() {
                AUTOSTART_ARG => parsed.autostart = true,
                MINIMIZED_ARG => parsed.minimized = true,
                _ => {}
            }
        }
        parsed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_launch_arguments() {
        assert_eq!(LaunchArgs::parse(["app.exe"]), LaunchArgs::default());
        let args = LaunchArgs::parse(["app.exe", "--autostart"]);
        assert!(args.autostart && !args.minimized);
        let args = LaunchArgs::parse(["app.exe", "--minimized", "--unknown"]);
        assert!(!args.autostart && args.minimized);
    }
}
