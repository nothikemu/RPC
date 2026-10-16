//! Shared application state managed by Tauri.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use deskpresence_core::api::LoadNotice;
use deskpresence_core::config::{Config, ConfigStore};
use deskpresence_core::icon_cache::IconService;
use deskpresence_core::runtime::{Runtime, RuntimeHandle};

use crate::logging::LogControl;
use crate::tray::TrayController;

/// Per-user locations (resolved through Tauri's path API from the bundle identifier).
#[derive(Clone, Debug)]
pub struct AppPaths {
    pub config_file: PathBuf,
    pub log_dir: PathBuf,
    pub icon_cache_dir: PathBuf,
}

pub struct ConfigState {
    pub store: ConfigStore,
    pub config: Arc<Config>,
    /// Incremented on every saved change; used for optimistic concurrency.
    pub revision: u64,
    pub notice: Option<LoadNotice>,
}

/// A validated configuration awaiting the user's confirmation.
pub struct PendingImport {
    pub config: Config,
    pub created: Instant,
}

pub struct AppState {
    pub paths: AppPaths,
    pub config: Mutex<ConfigState>,
    pub runtime: RuntimeHandle,
    /// Taken (once) during shutdown.
    pub runtime_owner: Mutex<Option<Runtime>>,
    pub icons: IconService,
    pub logging: LogControl,
    pub imports: Mutex<HashMap<String, PendingImport>>,
    pub tray: TrayController,
    pub shutting_down: AtomicBool,
    pub exit_ready: AtomicBool,
    pub default_application_id: Option<&'static str>,
}

/// Locks a mutex, recovering from poisoning (a panic elsewhere must not make
/// every later command fail).
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}
