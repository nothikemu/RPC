//! Discord Rich Presence over local IPC.
//!
//! This is a deliberately small client for the documented "RPC over IPC"
//! transport: handshake with the application (client) ID, then
//! `SET_ACTIVITY` commands. Rich Presence via `SET_ACTIVITY` needs no OAuth
//! authorization, so no secret or token exists anywhere in DeskPresence.
//!
//! The [`service`] module owns the connection lifecycle (discovery,
//! handshake, keep-alive, reconnect with backoff, rate limiting, duplicate
//! suppression and clean shutdown) behind a small handle.

pub mod activity;
pub mod backoff;
#[cfg(test)]
pub(crate) mod mock;
pub mod protocol;
pub mod service;
pub mod transport;

pub use activity::{Activity, ActivityAssets, ActivityTimestamps};
pub use service::{
    ActivityDelivery, ConnectionPhase, DiscordController, DiscordErrorKind, DiscordServiceConfig,
    DiscordServiceHandle, DiscordStatus, spawn_discord_service,
};
pub use transport::{Connector, PlatformConnector};

/// Discord's close code for an unknown application (client) ID.
pub const CLOSE_INVALID_CLIENT_ID: i64 = 4000;
/// Discord's close code for rate limiting.
pub const CLOSE_RATE_LIMITED: i64 = 4002;
/// Discord's RPC *error* code (ERROR events) for an invalid client ID.
pub const ERROR_INVALID_CLIENT_ID: i64 = 4007;

#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    #[error("Discord is not running, or its local RPC endpoint is unavailable")]
    NotRunning,
    #[error("Discord's local connection is busy (another program may be connecting)")]
    Busy,
    #[error("I/O error talking to Discord: {0}")]
    Io(#[from] std::io::Error),
    #[error("Discord closed the connection: the application ID was rejected ({message})")]
    InvalidClientId { message: String },
    #[error("Discord closed the connection (code {code:?}): {message}")]
    Closed { code: Option<i64>, message: String },
    #[error("Discord closed the connection")]
    Eof,
    #[error("timed out waiting for Discord")]
    Timeout,
    #[error("unexpected data from Discord: {0}")]
    Protocol(String),
    #[error("frame from Discord is too large ({0} bytes)")]
    FrameTooLarge(u32),
    #[error("unknown opcode {0} from Discord")]
    UnknownOpcode(u32),
}

impl RpcError {
    pub fn kind(&self) -> DiscordErrorKind {
        match self {
            RpcError::NotRunning => DiscordErrorKind::NotRunning,
            RpcError::InvalidClientId { .. } => DiscordErrorKind::InvalidApplicationId,
            RpcError::Timeout => DiscordErrorKind::Timeout,
            RpcError::Io(_) | RpcError::Busy => DiscordErrorKind::Io,
            RpcError::Closed { .. } | RpcError::Eof => DiscordErrorKind::Closed,
            RpcError::Protocol(_) | RpcError::FrameTooLarge(_) | RpcError::UnknownOpcode(_) => {
                DiscordErrorKind::Protocol
            }
        }
    }
}
