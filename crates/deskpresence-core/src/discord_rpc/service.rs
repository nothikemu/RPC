//! Connection lifecycle for Discord Rich Presence.
//!
//! One task owns the connection. It receives the *desired* activity through
//! a `watch` channel (latest value only, so bursts coalesce), sends it when it
//! differs from what Discord last acknowledged, and publishes a
//! [`DiscordStatus`] for the UI.
//!
//! Lifecycle: `NotConfigured` (no application ID) → `Connecting` →
//! `Connected` → (`WaitingToRetry` | `InvalidApplicationId`) → `Connecting` …
//! and finally `Stopped`. Retries use exponential backoff with jitter (1 s
//! doubling to 60 s); an application ID rejected by Discord is retried only
//! every 5 minutes or when the configuration changes.
//!
//! An acknowledged `SET_ACTIVITY` means the *local* Discord client accepted
//! the activity. Discord offers no confirmation that it is visible to others.

use std::time::Duration;

use serde::Serialize;
use tokio::io::{ReadHalf, WriteHalf};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep, sleep_until, timeout};

use super::activity::Activity;
use super::backoff::{Backoff, RateLimiter};
use super::protocol::{
    CloseReason, Frame, Opcode, handshake_payload, new_nonce, parse_close, parse_incoming, read_frame,
    set_activity_payload, write_frame,
};
use super::transport::{Connected, Connector, discord_install_detected};
use super::{CLOSE_INVALID_CLIENT_ID, RpcError};
use crate::time::now_unix_ms;

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
pub const RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
pub const CLEAR_TIMEOUT: Duration = Duration::from_millis(1000);
pub const BACKOFF_BASE: Duration = Duration::from_secs(1);
pub const BACKOFF_MAX: Duration = Duration::from_secs(60);
pub const INVALID_ID_RETRY: Duration = Duration::from_secs(300);
/// A connection that lasted at least this long resets the backoff.
pub const STABLE_CONNECTION: Duration = Duration::from_secs(10);
pub const RATE_LIMIT_MAX: usize = 5;
pub const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(20);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscordServiceConfig {
    pub application_id: Option<String>,
    /// Process ID reported to Discord; Discord clears the activity when this
    /// process exits. DeskPresence passes its own PID.
    pub pid: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum ConnectionPhase {
    NotConfigured,
    Connecting,
    Connected,
    WaitingToRetry,
    InvalidApplicationId,
    Stopped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum DiscordErrorKind {
    NotRunning,
    InvalidApplicationId,
    Timeout,
    Io,
    Closed,
    Protocol,
}

/// What happened to the most recent activity update on this connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum ActivityDelivery {
    /// Nothing sent on this connection yet.
    Idle,
    /// Sent; waiting for Discord's response.
    Pending,
    /// The local Discord client accepted the activity.
    Accepted,
    /// Discord rejected the artwork; the activity was accepted without it.
    AcceptedWithoutAssets,
    /// Discord rejected the activity. It will not be resent until it changes.
    Rejected,
    /// The activity was cleared.
    Cleared,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct DiscordStatus {
    pub phase: ConnectionPhase,
    /// IPC endpoint in use, e.g. `discord-ipc-0`.
    pub endpoint: Option<String>,
    /// Display name reported in Discord's READY event. Kept in memory only
    /// and never written to diagnostics exports.
    pub user_name: Option<String>,
    /// Consecutive failed attempts since the last successful connection.
    pub failed_attempts: u32,
    /// Successful reconnections after the first connection.
    pub reconnects: u32,
    pub last_attempt_at_ms: Option<u64>,
    pub next_retry_at_ms: Option<u64>,
    pub connected_since_ms: Option<u64>,
    pub last_error: Option<String>,
    pub last_error_kind: Option<DiscordErrorKind>,
    /// When Discord is not running: whether a Discord installation folder
    /// was found (Windows only; `None` when unknown).
    pub discord_installed: Option<bool>,
    pub delivery: ActivityDelivery,
    pub last_delivery_error: Option<String>,
    pub last_activity_update_at_ms: Option<u64>,
}

impl DiscordStatus {
    fn initial(config: &DiscordServiceConfig) -> Self {
        Self {
            phase: if config.application_id.is_some() {
                ConnectionPhase::Connecting
            } else {
                ConnectionPhase::NotConfigured
            },
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
            delivery: ActivityDelivery::Idle,
            last_delivery_error: None,
            last_activity_update_at_ms: None,
        }
    }
}

enum Control {
    Configure(DiscordServiceConfig),
    Reconnect,
    Shutdown(oneshot::Sender<()>),
}

/// Cloneable control channel for a running service.
#[derive(Clone)]
pub struct DiscordController(mpsc::Sender<Control>);

impl DiscordController {
    pub async fn configure(&self, config: DiscordServiceConfig) {
        let _ = self.0.send(Control::Configure(config)).await;
    }

    /// Reconnects now, skipping any pending backoff.
    pub async fn reconnect(&self) {
        let _ = self.0.send(Control::Reconnect).await;
    }
}

/// Handle to the running service.
pub struct DiscordServiceHandle {
    control: mpsc::Sender<Control>,
    status: watch::Receiver<DiscordStatus>,
    task: Option<JoinHandle<()>>,
}

impl DiscordServiceHandle {
    pub fn status(&self) -> watch::Receiver<DiscordStatus> {
        self.status.clone()
    }

    pub fn controller(&self) -> DiscordController {
        DiscordController(self.control.clone())
    }

    pub async fn configure(&self, config: DiscordServiceConfig) {
        let _ = self.control.send(Control::Configure(config)).await;
    }

    /// Reconnects now, skipping any pending backoff.
    pub async fn reconnect(&self) {
        let _ = self.control.send(Control::Reconnect).await;
    }

    /// Clears the activity (when connected), closes the connection and stops
    /// the task, waiting at most `limit`.
    pub async fn shutdown(mut self, limit: Duration) {
        let (ack_tx, ack_rx) = oneshot::channel();
        if self.control.send(Control::Shutdown(ack_tx)).await.is_ok() {
            let _ = timeout(limit, ack_rx).await;
        }
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
    }
}

/// Starts the service on the current Tokio runtime.
pub fn spawn_discord_service<C: Connector>(
    connector: C,
    config: DiscordServiceConfig,
    desired: watch::Receiver<Option<Activity>>,
) -> DiscordServiceHandle {
    let (control_tx, control_rx) = mpsc::channel(16);
    let (status_tx, status_rx) = watch::channel(DiscordStatus::initial(&config));
    let service = Service {
        connector,
        config,
        desired,
        control: control_rx,
        status: status_tx,
        backoff: Backoff::new(BACKOFF_BASE, BACKOFF_MAX),
        rate: RateLimiter::new(RATE_LIMIT_MAX, RATE_LIMIT_WINDOW),
        last_logged_error: None,
    };
    let task = tokio::spawn(service.run());
    DiscordServiceHandle {
        control: control_tx,
        status: status_rx,
        task: Some(task),
    }
}

struct Session<S> {
    writer: WriteHalf<S>,
    frames: mpsc::Receiver<Result<Frame, RpcError>>,
    reader: JoinHandle<()>,
}

impl<S> Drop for Session<S> {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

struct InFlight {
    nonce: String,
    /// The desired value this request fulfils.
    desired: Option<Activity>,
    /// What was actually sent (may have assets stripped).
    sent: Option<Activity>,
    stripped: bool,
    deadline: Instant,
}

enum SessionEnd {
    Shutdown(Option<oneshot::Sender<()>>),
    Restart,
    Failed(RpcError),
}

enum ControlOutcome {
    Restart,
    Stop(Option<oneshot::Sender<()>>),
}

struct Service<C: Connector> {
    connector: C,
    config: DiscordServiceConfig,
    desired: watch::Receiver<Option<Activity>>,
    control: mpsc::Receiver<Control>,
    status: watch::Sender<DiscordStatus>,
    backoff: Backoff,
    rate: RateLimiter,
    last_logged_error: Option<String>,
}

fn far_future() -> Instant {
    Instant::now() + Duration::from_secs(60 * 60 * 24 * 365)
}

fn close_error(reason: CloseReason) -> RpcError {
    let message = reason.message.unwrap_or_else(|| "no reason given".to_owned());
    match reason.code {
        Some(CLOSE_INVALID_CLIENT_ID) => RpcError::InvalidClientId { message },
        code => RpcError::Closed { code, message },
    }
}

async fn write_timed<S: tokio::io::AsyncWrite>(
    writer: &mut WriteHalf<S>,
    opcode: Opcode,
    payload: &[u8],
) -> Result<(), RpcError> {
    timeout(WRITE_TIMEOUT, write_frame(writer, opcode, payload))
        .await
        .map_err(|_| RpcError::Timeout)?
}

/// Connects, performs the handshake and waits for READY.
async fn open_session<C: Connector>(
    connector: &C,
    client_id: &str,
) -> Result<(Session<C::Stream>, String, Option<String>), RpcError> {
    let Connected { stream, endpoint } = connector.connect().await?;
    let (mut reader, mut writer): (ReadHalf<C::Stream>, WriteHalf<C::Stream>) = tokio::io::split(stream);
    write_frame(&mut writer, Opcode::Handshake, &handshake_payload(client_id)?).await?;
    let user = loop {
        let frame = read_frame(&mut reader).await?;
        match frame.opcode {
            Opcode::Frame => {
                let message = parse_incoming(&frame.payload)?;
                if message.is_ready() {
                    break message.ready_user_name();
                }
                if message.is_error() {
                    let (code, message) = message.error_details();
                    return Err(close_error(CloseReason {
                        code,
                        message: Some(message),
                    }));
                }
            }
            Opcode::Close => return Err(close_error(parse_close(&frame.payload))),
            Opcode::Ping => write_frame(&mut writer, Opcode::Pong, &frame.payload).await?,
            Opcode::Pong => {}
            Opcode::Handshake => {
                return Err(RpcError::Protocol("unexpected HANDSHAKE from Discord".into()));
            }
        }
    };

    // Reading happens on a separate task so that `select!` never cancels a
    // partially read frame.
    let (tx, frames) = mpsc::channel(32);
    let reader_task = tokio::spawn(async move {
        loop {
            let result = read_frame(&mut reader).await;
            let stop = result.is_err();
            if tx.send(result).await.is_err() || stop {
                break;
            }
        }
    });
    Ok((
        Session {
            writer,
            frames,
            reader: reader_task,
        },
        endpoint,
        user,
    ))
}

impl<C: Connector> Service<C> {
    fn update(&self, f: impl FnOnce(&mut DiscordStatus)) {
        self.status.send_if_modified(|status| {
            let before = status.clone();
            f(status);
            *status != before
        });
    }

    fn set_stopped(&self) {
        self.update(|s| {
            s.phase = ConnectionPhase::Stopped;
            s.endpoint = None;
            s.user_name = None;
            s.connected_since_ms = None;
            s.next_retry_at_ms = None;
        });
    }

    fn apply_config(&mut self, config: DiscordServiceConfig) -> bool {
        let changed = config != self.config;
        if changed {
            self.backoff.reset();
            self.rate = RateLimiter::new(RATE_LIMIT_MAX, RATE_LIMIT_WINDOW);
        }
        self.config = config;
        changed
    }

    fn handle_idle_control(&mut self, control: Option<Control>) -> ControlOutcome {
        match control {
            None => ControlOutcome::Stop(None),
            Some(Control::Shutdown(ack)) => ControlOutcome::Stop(Some(ack)),
            Some(Control::Configure(config)) => {
                self.apply_config(config);
                ControlOutcome::Restart
            }
            Some(Control::Reconnect) => {
                self.backoff.reset();
                ControlOutcome::Restart
            }
        }
    }

    async fn run(mut self) {
        let mut connected_before = false;
        loop {
            let Some(app_id) = self.config.application_id.clone() else {
                self.update(|s| {
                    s.phase = ConnectionPhase::NotConfigured;
                    s.next_retry_at_ms = None;
                    s.last_error = None;
                    s.last_error_kind = None;
                    s.failed_attempts = 0;
                    s.delivery = ActivityDelivery::Idle;
                });
                let control = self.control.recv().await;
                match self.handle_idle_control(control) {
                    ControlOutcome::Stop(ack) => return self.finish(ack),
                    ControlOutcome::Restart => continue,
                }
            };

            self.update(|s| {
                s.phase = ConnectionPhase::Connecting;
                s.last_attempt_at_ms = Some(now_unix_ms());
                s.next_retry_at_ms = None;
            });

            let attempt = tokio::select! {
                biased;
                control = self.control.recv() => {
                    match self.handle_idle_control(control) {
                        ControlOutcome::Stop(ack) => return self.finish(ack),
                        ControlOutcome::Restart => continue,
                    }
                }
                result = timeout(CONNECT_TIMEOUT, open_session(&self.connector, &app_id)) => {
                    result.unwrap_or(Err(RpcError::Timeout))
                }
            };

            let error = match attempt {
                Ok((mut session, endpoint, user_name)) => {
                    let reconnect = connected_before;
                    connected_before = true;
                    tracing::info!(endpoint = %endpoint, "connected to Discord");
                    self.last_logged_error = None;
                    self.update(|s| {
                        s.phase = ConnectionPhase::Connected;
                        s.endpoint = Some(endpoint);
                        s.user_name = user_name;
                        s.connected_since_ms = Some(now_unix_ms());
                        s.failed_attempts = 0;
                        s.last_error = None;
                        s.last_error_kind = None;
                        s.discord_installed = None;
                        s.delivery = ActivityDelivery::Idle;
                        s.last_delivery_error = None;
                        if reconnect {
                            s.reconnects += 1;
                        }
                    });
                    let started = Instant::now();
                    let end = self.run_session(&mut session).await;
                    drop(session);
                    match end {
                        SessionEnd::Shutdown(ack) => return self.finish(ack),
                        SessionEnd::Restart => {
                            self.backoff.reset();
                            continue;
                        }
                        SessionEnd::Failed(error) => {
                            if started.elapsed() >= STABLE_CONNECTION {
                                self.backoff.reset();
                            }
                            error
                        }
                    }
                }
                Err(error) => error,
            };

            let invalid_id = matches!(error, RpcError::InvalidClientId { .. });
            let delay = if invalid_id {
                INVALID_ID_RETRY
            } else {
                self.backoff.next_delay()
            };
            self.log_error(&error, delay);
            let installed = matches!(error, RpcError::NotRunning)
                .then(discord_install_detected)
                .flatten();
            let delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX);
            self.update(|s| {
                s.phase = if invalid_id {
                    ConnectionPhase::InvalidApplicationId
                } else {
                    ConnectionPhase::WaitingToRetry
                };
                s.endpoint = None;
                s.user_name = None;
                s.connected_since_ms = None;
                s.failed_attempts = s.failed_attempts.saturating_add(1);
                s.last_error = Some(error.to_string());
                s.last_error_kind = Some(error.kind());
                s.discord_installed = installed;
                s.next_retry_at_ms = Some(now_unix_ms().saturating_add(delay_ms));
                s.delivery = ActivityDelivery::Idle;
            });

            tokio::select! {
                biased;
                control = self.control.recv() => {
                    if let ControlOutcome::Stop(ack) = self.handle_idle_control(control) {
                        return self.finish(ack);
                    }
                }
                _ = sleep(delay) => {}
            }
        }
    }

    fn finish(&self, ack: Option<oneshot::Sender<()>>) {
        self.set_stopped();
        tracing::info!("Discord service stopped");
        if let Some(ack) = ack {
            let _ = ack.send(());
        }
    }

    /// Logs a failure once per distinct message to avoid flooding the log
    /// while Discord stays unavailable.
    fn log_error(&mut self, error: &RpcError, delay: Duration) {
        let message = error.to_string();
        if self.last_logged_error.as_deref() == Some(message.as_str()) {
            tracing::debug!(error = %message, retry_in_ms = delay.as_millis() as u64, "Discord still unavailable");
            return;
        }
        match error {
            RpcError::NotRunning => {
                tracing::info!(
                    retry_in_ms = delay.as_millis() as u64,
                    "Discord is not running; will retry"
                );
            }
            _ => {
                tracing::warn!(error = %message, retry_in_ms = delay.as_millis() as u64, "Discord connection failed")
            }
        }
        self.last_logged_error = Some(message);
    }

    async fn send_activity(
        &mut self,
        session: &mut Session<C::Stream>,
        sent: Option<Activity>,
        desired: Option<Activity>,
        stripped: bool,
    ) -> Result<InFlight, RpcError> {
        let nonce = new_nonce();
        let payload = set_activity_payload(self.config.pid, sent.as_ref(), &nonce)?;
        self.rate.record(Instant::now());
        write_timed(&mut session.writer, Opcode::Frame, &payload).await?;
        self.update(|s| s.delivery = ActivityDelivery::Pending);
        Ok(InFlight {
            nonce,
            desired,
            sent,
            stripped,
            deadline: Instant::now() + RESPONSE_TIMEOUT,
        })
    }

    async fn run_session(&mut self, session: &mut Session<C::Stream>) -> SessionEnd {
        // A fresh connection has no activity from us.
        let mut applied: Option<Activity> = None;
        let mut rejected: Option<Option<Activity>> = None;
        let mut in_flight: Option<InFlight> = None;
        let mut desired_open = true;

        loop {
            let mut wake_at: Option<Instant> = None;
            if in_flight.is_none() {
                let desired = self.desired.borrow_and_update().clone();
                if desired != applied && rejected.as_ref() != Some(&desired) {
                    match self.rate.ready_at(Instant::now()) {
                        None => match self.send_activity(session, desired.clone(), desired, false).await {
                            Ok(request) => in_flight = Some(request),
                            Err(error) => return SessionEnd::Failed(error),
                        },
                        Some(at) => wake_at = Some(at),
                    }
                }
            }
            let deadline = in_flight.as_ref().map(|f| f.deadline);

            tokio::select! {
                biased;
                control = self.control.recv() => match control {
                    None => return SessionEnd::Shutdown(None),
                    Some(Control::Shutdown(ack)) => {
                        self.clear_before_close(session, applied.is_some()).await;
                        return SessionEnd::Shutdown(Some(ack));
                    }
                    Some(Control::Configure(config)) => {
                        let id_changed = config.application_id != self.config.application_id;
                        self.apply_config(config);
                        if id_changed {
                            return SessionEnd::Restart;
                        }
                    }
                    Some(Control::Reconnect) => return SessionEnd::Restart,
                },
                frame = session.frames.recv() => {
                    let frame = match frame {
                        None => return SessionEnd::Failed(RpcError::Eof),
                        Some(Err(error)) => return SessionEnd::Failed(error),
                        Some(Ok(frame)) => frame,
                    };
                    match frame.opcode {
                        Opcode::Ping => {
                            if let Err(error) = write_timed(&mut session.writer, Opcode::Pong, &frame.payload).await {
                                return SessionEnd::Failed(error);
                            }
                        }
                        Opcode::Pong => {}
                        Opcode::Close => return SessionEnd::Failed(close_error(parse_close(&frame.payload))),
                        Opcode::Handshake => {
                            return SessionEnd::Failed(RpcError::Protocol("unexpected HANDSHAKE from Discord".into()));
                        }
                        Opcode::Frame => {
                            let message = match parse_incoming(&frame.payload) {
                                Ok(message) => message,
                                Err(error) => return SessionEnd::Failed(error),
                            };
                            let Some(request) = in_flight.take_if(|f| message.nonce.as_deref() == Some(f.nonce.as_str())) else {
                                continue; // Unrelated dispatch event.
                            };
                            if message.is_error() {
                                let (code, text) = message.error_details();
                                let retry_without_assets = !request.stripped
                                    && request.sent.as_ref().is_some_and(Activity::has_assets);
                                if retry_without_assets {
                                    tracing::warn!(code, error = %text, "Discord rejected the activity; retrying without artwork");
                                    self.update(|s| s.last_delivery_error = Some(format!(
                                        "Discord rejected the artwork ({text}). Showing the activity without images."
                                    )));
                                    let stripped = request.sent.as_ref().map(Activity::without_assets);
                                    match self.send_activity(session, stripped, request.desired, true).await {
                                        Ok(retry) => in_flight = Some(retry),
                                        Err(error) => return SessionEnd::Failed(error),
                                    }
                                } else {
                                    tracing::warn!(code, error = %text, "Discord rejected the activity");
                                    rejected = Some(request.desired);
                                    self.update(|s| {
                                        s.delivery = ActivityDelivery::Rejected;
                                        s.last_delivery_error = Some(match code {
                                            Some(code) => format!("Discord rejected the activity (code {code}): {text}"),
                                            None => format!("Discord rejected the activity: {text}"),
                                        });
                                    });
                                }
                            } else {
                                let delivery = if request.desired.is_none() {
                                    ActivityDelivery::Cleared
                                } else if request.stripped {
                                    ActivityDelivery::AcceptedWithoutAssets
                                } else {
                                    ActivityDelivery::Accepted
                                };
                                applied = request.desired;
                                rejected = None;
                                self.update(|s| {
                                    s.delivery = delivery;
                                    s.last_activity_update_at_ms = Some(now_unix_ms());
                                    if delivery != ActivityDelivery::AcceptedWithoutAssets {
                                        s.last_delivery_error = None;
                                    }
                                });
                            }
                        }
                    }
                }
                changed = self.desired.changed(), if desired_open => {
                    if changed.is_err() {
                        desired_open = false;
                    }
                }
                _ = sleep_until(deadline.unwrap_or_else(far_future)), if deadline.is_some() => {
                    return SessionEnd::Failed(RpcError::Timeout);
                }
                _ = sleep_until(wake_at.unwrap_or_else(far_future)), if wake_at.is_some() => {}
            }
        }
    }

    /// Best effort: clear the activity before closing. Discord also clears it
    /// when the connection closes or the reported process exits.
    async fn clear_before_close(&mut self, session: &mut Session<C::Stream>, has_activity: bool) {
        if !has_activity {
            return;
        }
        let nonce = new_nonce();
        let Ok(payload) = set_activity_payload(self.config.pid, None, &nonce) else {
            return;
        };
        let attempt = timeout(CLEAR_TIMEOUT, async {
            write_frame(&mut session.writer, Opcode::Frame, &payload).await?;
            while let Some(frame) = session.frames.recv().await {
                let frame = frame?;
                if frame.opcode == Opcode::Frame
                    && parse_incoming(&frame.payload)
                        .is_ok_and(|m| m.nonce.as_deref() == Some(nonce.as_str()))
                {
                    return Ok::<(), RpcError>(());
                }
            }
            Ok(())
        })
        .await;
        match attempt {
            Ok(Ok(())) => {
                self.update(|s| s.delivery = ActivityDelivery::Cleared);
                tracing::info!("cleared Discord activity before shutdown");
            }
            Ok(Err(error)) => tracing::debug!(%error, "could not clear activity before shutdown"),
            Err(_) => tracing::debug!("timed out clearing activity before shutdown"),
        }
    }
}

#[cfg(test)]
mod tests;
