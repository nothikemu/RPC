//! Local IPC endpoint discovery.
//!
//! Windows: named pipes `\\?\pipe\discord-ipc-0` … `discord-ipc-9` (the
//! documented path). The first pipe that accepts a connection is used; busy
//! or missing pipes are skipped. Unix (development only): sockets named
//! `discord-ipc-N` under `XDG_RUNTIME_DIR`, `TMPDIR`, `TMP`, `TEMP`, `/tmp`.

use std::future::Future;

use tokio::io::{AsyncRead, AsyncWrite};

use super::RpcError;

/// Number of IPC endpoints Discord may listen on.
pub const IPC_SLOTS: u8 = 10;

pub struct Connected<S> {
    pub stream: S,
    /// Human-readable endpoint, e.g. `discord-ipc-0`.
    pub endpoint: String,
}

/// Opens a stream to Discord. Abstracted so tests can supply in-memory streams.
pub trait Connector: Send + Sync + 'static {
    type Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static;

    fn connect(&self) -> impl Future<Output = Result<Connected<Self::Stream>, RpcError>> + Send;
}

/// The real transport for the current platform.
#[derive(Clone, Debug)]
pub struct PlatformConnector {
    /// Endpoint name prefix; `discord-ipc-` in production. Tests use a unique
    /// prefix so they never talk to a real Discord client.
    prefix: String,
    /// Unix only: directories to search instead of the documented ones.
    #[cfg_attr(not(unix), allow(dead_code))]
    dirs: Option<Vec<std::path::PathBuf>>,
}

impl Default for PlatformConnector {
    fn default() -> Self {
        Self {
            prefix: "discord-ipc-".to_owned(),
            dirs: None,
        }
    }
}

impl PlatformConnector {
    /// A connector for test endpoints named `<prefix><slot>`.
    pub fn with_prefix(prefix: impl Into<String>, dirs: Option<Vec<std::path::PathBuf>>) -> Self {
        Self {
            prefix: prefix.into(),
            dirs,
        }
    }
}

#[cfg(windows)]
impl Connector for PlatformConnector {
    type Stream = tokio::net::windows::named_pipe::NamedPipeClient;

    async fn connect(&self) -> Result<Connected<Self::Stream>, RpcError> {
        use tokio::net::windows::named_pipe::ClientOptions;

        const ERROR_FILE_NOT_FOUND: i32 = 2;
        const ERROR_PIPE_BUSY: i32 = 231;

        let mut other_error: Option<std::io::Error> = None;
        for slot in 0..IPC_SLOTS {
            let name = format!("{}{slot}", self.prefix);
            let path = format!(r"\\?\pipe\{name}");
            match ClientOptions::new().open(&path) {
                Ok(stream) => {
                    return Ok(Connected {
                        stream,
                        endpoint: name,
                    });
                }
                Err(e)
                    if e.raw_os_error() == Some(ERROR_FILE_NOT_FOUND)
                        || e.kind() == std::io::ErrorKind::NotFound
                        || e.raw_os_error() == Some(ERROR_PIPE_BUSY) => {}
                Err(e) => other_error = Some(e),
            }
        }
        Err(other_error.map_or(RpcError::NotRunning, RpcError::Io))
    }
}

#[cfg(unix)]
impl Connector for PlatformConnector {
    type Stream = tokio::net::UnixStream;

    async fn connect(&self) -> Result<Connected<Self::Stream>, RpcError> {
        let candidates = match &self.dirs {
            Some(dirs) => dirs.clone(),
            None => {
                let mut bases: Vec<std::path::PathBuf> = ["XDG_RUNTIME_DIR", "TMPDIR", "TMP", "TEMP"]
                    .iter()
                    .filter_map(|var| std::env::var_os(var).map(std::path::PathBuf::from))
                    .collect();
                bases.push("/tmp".into());
                // Common sandboxed client locations relative to the runtime dir.
                let mut candidates = Vec::new();
                for base in &bases {
                    candidates.push(base.clone());
                    candidates.push(base.join("app/com.discordapp.Discord"));
                    candidates.push(base.join("snap.discord"));
                }
                candidates
            }
        };
        let mut other_error: Option<std::io::Error> = None;
        for dir in candidates {
            for slot in 0..IPC_SLOTS {
                let path = dir.join(format!("{}{slot}", self.prefix));
                match tokio::net::UnixStream::connect(&path).await {
                    Ok(stream) => {
                        return Ok(Connected {
                            stream,
                            endpoint: path.display().to_string(),
                        });
                    }
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                        ) => {}
                    Err(e) => other_error = Some(e),
                }
            }
        }
        Err(other_error.map_or(RpcError::NotRunning, RpcError::Io))
    }
}

/// Best-effort hint used in the UI when no IPC endpoint exists: whether a
/// Discord desktop installation folder is present for this user.
pub fn discord_install_detected() -> Option<bool> {
    #[cfg(windows)]
    {
        let local = std::env::var_os("LOCALAPPDATA")?;
        let base = std::path::PathBuf::from(local);
        Some(
            ["Discord", "DiscordPTB", "DiscordCanary", "DiscordDevelopment"]
                .iter()
                .any(|name| base.join(name).exists()),
        )
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    //! Exercise the real platform transport (Unix sockets here, named pipes on
    //! Windows) with the mock server, using unique endpoint names so a running
    //! Discord client is never contacted.
    use std::time::Duration;

    use tokio::sync::watch;

    use super::*;
    use crate::discord_rpc::mock::{Behavior, MockDiscord};
    use crate::discord_rpc::{
        Activity, ActivityDelivery, ConnectionPhase, DiscordServiceConfig, spawn_discord_service,
    };

    fn unique_prefix() -> String {
        format!("deskpresence-test-{:08x}-", fastrand::u32(..))
    }

    async fn run_service_against(connector: PlatformConnector, mock: &MockDiscord) {
        let (desired_tx, desired_rx) = watch::channel(None);
        let handle = spawn_discord_service(
            connector,
            DiscordServiceConfig {
                application_id: Some("123456789012345678".into()),
                pid: std::process::id(),
            },
            desired_rx,
        );
        let mut status = handle.status();
        desired_tx.send_replace(Some(Activity {
            activity_type: 0,
            name: None,
            details: Some("Using a real transport".into()),
            state: None,
            status_display_type: None,
            timestamps: None,
            assets: None,
        }));
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                {
                    let s = status.borrow_and_update();
                    if s.phase == ConnectionPhase::Connected && s.delivery == ActivityDelivery::Accepted {
                        break;
                    }
                }
                status.changed().await.unwrap();
            }
        })
        .await
        .expect("connected and accepted over the real transport");
        handle.shutdown(Duration::from_secs(2)).await;
        let activities = mock.activities();
        assert_eq!(activities[0]["details"], "Using a real transport");
        assert_eq!(
            activities.last(),
            Some(&serde_json::Value::Null),
            "cleared on shutdown"
        );
    }

    #[tokio::test]
    async fn reports_not_running_when_no_endpoint_exists() {
        let dir = tempfile::tempdir().unwrap();
        let connector = PlatformConnector::with_prefix(unique_prefix(), Some(vec![dir.path().to_path_buf()]));
        assert!(matches!(connector.connect().await, Err(RpcError::NotRunning)));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn full_session_over_a_unix_socket() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = unique_prefix();
        // Slot 3: the connector must skip the missing slots 0–2.
        let listener = tokio::net::UnixListener::bind(dir.path().join(format!("{prefix}3"))).unwrap();
        let mock = MockDiscord::new(Behavior::Normal);
        let server = mock.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                server.serve_stream(stream);
            }
        });
        let connector = PlatformConnector::with_prefix(prefix, Some(vec![dir.path().to_path_buf()]));
        run_service_against(connector, &mock).await;
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn full_session_over_a_named_pipe() {
        use tokio::net::windows::named_pipe::ServerOptions;

        let prefix = unique_prefix();
        // Servers create pipes with the `\\.\pipe\` form; the client uses the
        // `\\?\pipe\` form from Discord's docs. Both name the same pipe.
        let pipe = format!(r"\\.\pipe\{prefix}2");
        let mock = MockDiscord::new(Behavior::Normal);
        let server = mock.clone();
        let mut instance = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&pipe)
            .unwrap();
        tokio::spawn(async move {
            loop {
                if instance.connect().await.is_err() {
                    break;
                }
                let next = match ServerOptions::new().create(&pipe) {
                    Ok(next) => next,
                    Err(_) => break,
                };
                let connected = std::mem::replace(&mut instance, next);
                server.serve_stream(connected);
            }
        });
        let connector = PlatformConnector::with_prefix(prefix, None);
        run_service_against(connector, &mock).await;
    }
}
