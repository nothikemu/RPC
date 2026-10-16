//! Core logic for DeskPresence.
//!
//! This crate owns everything that does not need a window: application
//! detection (with a Win32 backend on Windows), canonical application identity,
//! rule evaluation, presence templates, the presence selection engine, the
//! Discord IPC client, versioned configuration and diagnostics.
//!
//! The Tauri shell (`src-tauri`) wires these pieces to the tray, the
//! configuration window and the operating system's startup registration.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod api;
pub mod app_detection;
pub mod app_identity;
pub mod config;
pub mod diagnostics;
pub mod discord_rpc;
pub mod icon_cache;
pub mod presence_manager;
pub mod presence_templates;
pub mod rules;
pub mod runtime;
pub mod time;

/// Human readable product name. Change it here (and in `tauri.conf.json`) to rebrand.
pub const PRODUCT_NAME: &str = "DeskPresence";
