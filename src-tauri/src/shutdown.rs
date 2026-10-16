//! Clean shutdown, shared by the tray's Exit, the window close button (when
//! close-to-tray is off), the UI and operating-system exit requests.
//!
//! Order: stop the presence engine, clear the Discord activity and close the
//! connection, stop the detector thread, flush logs, remove the tray icon,
//! then exit the process. The whole sequence is bounded to a few seconds.

use std::sync::atomic::Ordering;
use std::time::Duration;

use tauri::{AppHandle, Manager};

use crate::state::{AppState, lock};

pub const SHUTDOWN_LIMIT: Duration = Duration::from_secs(4);

pub fn request_exit(app: &AppHandle) {
    let state = app.state::<AppState>();
    if state.shutting_down.swap(true, Ordering::SeqCst) {
        return;
    }
    tracing::info!("shutting down");
    crate::window::hide_main(app);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        let runtime = lock(&state.runtime_owner).take();
        if let Some(runtime) = runtime {
            runtime.shutdown(SHUTDOWN_LIMIT).await;
        }
        tracing::info!("shutdown complete");
        state.logging.flush();
        state.exit_ready.store(true, Ordering::SeqCst);
        let handle = app.clone();
        let _ = app.run_on_main_thread(move || {
            handle.state::<AppState>().tray.remove();
            handle.exit(0);
        });
    });
}
