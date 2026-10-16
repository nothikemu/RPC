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
use tokio::time::{Instant, sleep_until, timeout};

use super::activity::Activity;
use super::backoff::{Backoff, RateLimiter};
use super::protocol::{
    CloseReason, Frame, Opcode, handshake_payload, new_nonce, parse_close, parse_incoming, read_frame,
    set_activity_payload, write_frame,
};
use super::transport::{Connected, Connector, discord_install_detected};
use super::{CLOSE_INVALID_CLIENT_ID, ERROR_INVALID_CLIENT_ID, RpcError};
use crate::time::now_unix_ms;

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
pub const RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
pub const CLEAR_TIMEOUT: Duration = Duration::from_millis(1000);
pub const BACKOFF_BASE: Duration = Duration::from_secs(1);
pub const BACKOFF_MAX: Duration = Duration::from_secs(60);
pub const INVALID_ID_RETRY: Duration = Duration::from_secs(300);
/// A connection with nothing to send that lasted at least this long counts as
/// healthy (resets the backoff). Otherwise Discord must have answered a command.
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
        connector: std::sync::Arc::new(connector),
        config,
        desired,
        control: control_rx,
        status: status_tx,
        backoff: Backoff::new(BACKOFF_BASE, BACKOFF_MAX),
        rate: RateLimiter::new(RATE_LIMIT_MAX, RATE_LIMIT_WINDOW),
        last_logged_error: None,
        last_attempt: None,
        unresponsive_sessions: 0,
        session_responsive: false,
        session_sent: false,
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

/// Why a `SET_ACTIVITY` request was sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Purpose {
    /// Show the desired activity (or clear it when the desired value is `None`).
    Desired,
    /// Re-send the desired activity without artwork after Discord rejected it.
    WithoutArtwork,
    /// Clear an older activity because the current one was rejected, so
    /// Discord does not keep showing an application the user has left.
    ClearStale,
}

struct InFlight {
    nonce: String,
    /// The desired value this request fulfils (`None` for clears).
    desired: Option<Activity>,
    purpose: Purpose,
    /// Whether the request carried artwork.
    had_assets: bool,
    deadline: Instant,
}

/// Per-connection bookkeeping of what Discord shows and what it refused.
#[derive(Default)]
struct SessionState {
    /// What Discord currently shows from us, per its acknowledgements.
    applied: Option<Activity>,
    /// Delivery status that matches `applied`.
    applied_delivery: Option<ActivityDelivery>,
    /// A desired value Discord rejected; not re-sent until the desired value changes.
    rejected: Option<Option<Activity>>,
    /// A desired value whose artwork was rejected and should be re-sent without it.
    retry_without_artwork: Option<Option<Activity>>,
    /// The rejected value for which a stale-activity clear was already attempted.
    stale_clear_attempted: Option<Option<Activity>>,
    in_flight: Option<InFlight>,
}

enum SessionEnd {
    Shutdown(Option<oneshot::Sender<()>>),
    Restart,
    Failed(RpcError),
}

enum ControlOutcome {
    Restart,
    Ignore,
    Stop(Option<oneshot::Sender<()>>),
}

/// Reconnect requests closer together than this are ignored, so a stuck UI
/// (or anything else) cannot churn connections to Discord.
pub const MIN_RECONNECT_INTERVAL: Duration = Duration::from_secs(3);

struct Service<C: Connector> {
    connector: std::sync::Arc<C>,
    config: DiscordServiceConfig,
    desired: watch::Receiver<Option<Activity>>,
    control: mpsc::Receiver<Control>,
    status: watch::Sender<DiscordStatus>,
    backoff: Backoff,
    rate: RateLimiter,
    last_logged_error: Option<String>,
    last_attempt: Option<Instant>,
    /// Consecutive sessions in which Discord never answered a command.
    unresponsive_sessions: u32,
    /// Set during a session once Discord answers any command.
    session_responsive: bool,
    /// Set during a session once any command was sent.
    session_sent: bool,
}

fn far_future() -> Instant {
    Instant::now() + Duration::from_secs(60 * 60 * 24 * 365)
}

/// Maps a CLOSE frame (RPC close codes) to an error.
fn close_error(reason: CloseReason) -> RpcError {
    let message = reason.message.unwrap_or_else(|| "no reason given".to_owned());
    match reason.code {
        Some(CLOSE_INVALID_CLIENT_ID) => RpcError::InvalidClientId { message },
        code => RpcError::Closed { code, message },
    }
}

/// Maps an ERROR event received during the handshake. ERROR events use the
/// RPC *error* code table, where 4007 means "Invalid client ID" (and 4000
/// means "Invalid payload", unlike the close-code table).
fn handshake_error(code: Option<i64>, message: String) -> RpcError {
    match code {
        Some(ERROR_INVALID_CLIENT_ID) => RpcError::InvalidClientId { message },
        Some(code) => RpcError::Protocol(format!(
            "Discord reported error {code} during the handshake: {message}"
        )),
        None => RpcError::Protocol(format!(
            "Discord reported an error during the handshake: {message}"
        )),
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
                    let (code, text) = message.error_details();
                    return Err(handshake_error(code, text));
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

    /// Whether a reconnect request should be honoured now.
    fn reconnect_allowed(&self) -> bool {
        self.last_attempt
            .is_none_or(|at| at.elapsed() >= MIN_RECONNECT_INTERVAL)
    }

    fn handle_idle_control(&mut self, control: Option<Control>) -> ControlOutcome {
        match control {
            None => ControlOutcome::Stop(None),
            Some(Control::Shutdown(ack)) => ControlOutcome::Stop(Some(ack)),
            Some(Control::Configure(config)) => {
                if self.apply_config(config) {
                    ControlOutcome::Restart
                } else {
                    ControlOutcome::Ignore
                }
            }
            Some(Control::Reconnect) if self.reconnect_allowed() => {
                self.backoff.reset();
                ControlOutcome::Restart
            }
            Some(Control::Reconnect) => {
                tracing::debug!("ignored reconnect request: the last attempt was moments ago");
                ControlOutcome::Ignore
            }
        }
    }

    async fn run(mut self) {
        let mut connected_before = false;
        loop {
            let Some(app_id) = self.config.application_id.clone() else {
                self.update(|s| {
                    s.phase = ConnectionPhase::NotConfigured;
                    s.endpoint = None;
                    s.user_name = None;
                    s.connected_since_ms = None;
                    s.next_retry_at_ms = None;
                    s.last_error = None;
                    s.last_error_kind = None;
                    s.failed_attempts = 0;
                    s.discord_installed = None;
                    s.delivery = ActivityDelivery::Idle;
                    s.last_delivery_error = None;
                });
                loop {
                    let control = self.control.recv().await;
                    match self.handle_idle_control(control) {
                        ControlOutcome::Stop(ack) => return self.finish(ack),
                        ControlOutcome::Restart => break,
                        ControlOutcome::Ignore => {}
                    }
                }
                continue;
            };

            self.last_attempt = Some(Instant::now());
            self.update(|s| {
                s.phase = ConnectionPhase::Connecting;
                s.endpoint = None;
                s.user_name = None;
                s.connected_since_ms = None;
                s.last_attempt_at_ms = Some(now_unix_ms());
                s.next_retry_at_ms = None;
            });

            let connector = std::sync::Arc::clone(&self.connector);
            let connect = timeout(CONNECT_TIMEOUT, async move {
                open_session(&*connector, &app_id).await
            });
            tokio::pin!(connect);
            let attempt = loop {
                tokio::select! {
                    biased;
                    control = self.control.recv() => {
                        match self.handle_idle_control(control) {
                            ControlOutcome::Stop(ack) => return self.finish(ack),
                            ControlOutcome::Restart => break None,
                            ControlOutcome::Ignore => {}
                        }
                    }
                    result = &mut connect => break Some(result.unwrap_or(Err(RpcError::Timeout))),
                }
            };
            let Some(attempt) = attempt else {
                continue;
            };

            let error = match attempt {
                Ok((mut session, endpoint, user_name)) => {
                    let reconnect = connected_before;
                    connected_before = true;
                    if self.unresponsive_sessions == 0 {
                        tracing::info!(endpoint = %endpoint, "connected to Discord");
                    } else {
                        tracing::debug!(endpoint = %endpoint, "connected to Discord again");
                    }
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
                    self.session_responsive = false;
                    self.session_sent = false;
                    let started = Instant::now();
                    let end = self.run_session(&mut session).await;
                    drop(session);
                    // A session is healthy if Discord answered a command, or if
                    // there was nothing to send and the connection simply lasted.
                    // A connection that accepts the handshake but never answers
                    // must not reset the backoff, or it would reconnect forever
                    // at the minimum delay.
                    let healthy = self.session_responsive
                        || (!self.session_sent && started.elapsed() >= STABLE_CONNECTION);
                    if healthy {
                        self.unresponsive_sessions = 0;
                        self.last_logged_error = None;
                    } else {
                        self.unresponsive_sessions = self.unresponsive_sessions.saturating_add(1);
                    }
                    match end {
                        SessionEnd::Shutdown(ack) => return self.finish(ack),
                        SessionEnd::Restart => {
                            self.backoff.reset();
                            continue;
                        }
                        SessionEnd::Failed(error) => {
                            if healthy {
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

            let retry_at = Instant::now() + delay;
            loop {
                tokio::select! {
                    biased;
                    control = self.control.recv() => {
                        match self.handle_idle_control(control) {
                            ControlOutcome::Stop(ack) => return self.finish(ack),
                            ControlOutcome::Restart => break,
                            ControlOutcome::Ignore => {}
                        }
                    }
                    _ = sleep_until(retry_at) => break,
                }
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
        purpose: Purpose,
    ) -> Result<InFlight, RpcError> {
        let nonce = new_nonce();
        let payload = set_activity_payload(self.config.pid, sent.as_ref(), &nonce)?;
        self.rate.record(Instant::now());
        self.session_sent = true;
        write_timed(&mut session.writer, Opcode::Frame, &payload).await?;
        if purpose != Purpose::ClearStale {
            self.update(|s| s.delivery = ActivityDelivery::Pending);
        }
        Ok(InFlight {
            nonce,
            had_assets: sent.as_ref().is_some_and(Activity::has_assets),
            desired,
            purpose,
            deadline: Instant::now() + RESPONSE_TIMEOUT,
        })
    }

    /// Decides what, if anything, to send next for the current desired value.
    /// Returns `(sent, desired, purpose)`.
    fn next_request(
        &self,
        state: &mut SessionState,
        desired: &Option<Activity>,
    ) -> Option<(Option<Activity>, Option<Activity>, Purpose)> {
        // A pending artwork-less retry only applies while that value is still wanted.
        if state
            .retry_without_artwork
            .as_ref()
            .is_some_and(|value| value != desired)
        {
            state.retry_without_artwork = None;
        }
        if let Some(original) = &state.retry_without_artwork {
            let stripped = original.as_ref().map(Activity::without_assets);
            return Some((stripped, original.clone(), Purpose::WithoutArtwork));
        }
        if state.rejected.as_ref() == Some(desired) {
            // Discord refused the current value. Don't leave an older activity
            // (from an application the user has left) on the profile.
            if state.applied.is_some() && state.stale_clear_attempted.as_ref() != Some(desired) {
                return Some((None, None, Purpose::ClearStale));
            }
            return None;
        }
        if *desired != state.applied {
            return Some((desired.clone(), desired.clone(), Purpose::Desired));
        }
        // Discord already shows the desired value. If an earlier, different
        // value was rejected, the status must describe what is shown now.
        if state.rejected.take().is_some() {
            let delivery = state.applied_delivery.unwrap_or(ActivityDelivery::Idle);
            self.update(|s| {
                s.delivery = delivery;
                if delivery != ActivityDelivery::AcceptedWithoutAssets {
                    s.last_delivery_error = None;
                }
            });
        }
        None
    }

    fn handle_response(
        &mut self,
        state: &mut SessionState,
        request: InFlight,
        message: &super::protocol::Incoming,
    ) {
        self.session_responsive = true;
        if message.is_error() {
            let (code, text) = message.error_details();
            match request.purpose {
                Purpose::Desired if request.had_assets => {
                    tracing::warn!(code, error = %text, "Discord rejected the activity; retrying without artwork");
                    state.retry_without_artwork = Some(request.desired);
                    self.update(|s| {
                        s.last_delivery_error = Some(format!(
                            "Discord rejected the artwork ({text}). Showing the activity without images."
                        ))
                    });
                }
                Purpose::Desired | Purpose::WithoutArtwork => {
                    tracing::warn!(code, error = %text, "Discord rejected the activity");
                    state.retry_without_artwork = None;
                    state.rejected = Some(request.desired);
                    self.update(|s| {
                        s.delivery = ActivityDelivery::Rejected;
                        s.last_delivery_error = Some(match code {
                            Some(code) => format!("Discord rejected the activity (code {code}): {text}"),
                            None => format!("Discord rejected the activity: {text}"),
                        });
                    });
                }
                Purpose::ClearStale => {
                    tracing::warn!(code, error = %text, "Discord rejected clearing the previous activity");
                    state.stale_clear_attempted = state.rejected.clone();
                }
            }
            return;
        }

        match request.purpose {
            Purpose::ClearStale => {
                // Keep reporting the rejection; Discord now shows nothing from us.
                state.stale_clear_attempted = state.rejected.clone();
                state.applied = None;
                state.applied_delivery = Some(ActivityDelivery::Cleared);
                self.update(|s| s.last_activity_update_at_ms = Some(now_unix_ms()));
            }
            Purpose::Desired | Purpose::WithoutArtwork => {
                let delivery = if request.desired.is_none() {
                    ActivityDelivery::Cleared
                } else if request.purpose == Purpose::WithoutArtwork {
                    ActivityDelivery::AcceptedWithoutAssets
                } else {
                    ActivityDelivery::Accepted
                };
                state.applied = request.desired;
                state.applied_delivery = Some(delivery);
                state.rejected = None;
                state.retry_without_artwork = None;
                state.stale_clear_attempted = None;
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

    async fn run_session(&mut self, session: &mut Session<C::Stream>) -> SessionEnd {
        // A fresh connection has no activity from us.
        let mut state = SessionState::default();
        let mut desired_open = true;

        loop {
            let mut wake_at: Option<Instant> = None;
            if state.in_flight.is_none() {
                let desired = self.desired.borrow_and_update().clone();
                if let Some((sent, value, purpose)) = self.next_request(&mut state, &desired) {
                    match self.rate.ready_at(Instant::now()) {
                        None => match self.send_activity(session, sent, value, purpose).await {
                            Ok(request) => state.in_flight = Some(request),
                            Err(error) => return SessionEnd::Failed(error),
                        },
                        Some(at) => wake_at = Some(at),
                    }
                }
            }
            let deadline = state.in_flight.as_ref().map(|f| f.deadline);

            tokio::select! {
                biased;
                control = self.control.recv() => match control {
                    None => return SessionEnd::Shutdown(None),
                    Some(Control::Shutdown(ack)) => {
                        self.clear_before_close(session, state.applied.is_some()).await;
                        return SessionEnd::Shutdown(Some(ack));
                    }
                    Some(Control::Configure(config)) => {
                        let id_changed = config.application_id != self.config.application_id;
                        self.apply_config(config);
                        if id_changed {
                            return SessionEnd::Restart;
                        }
                    }
                    Some(Control::Reconnect) => {
                        if self.reconnect_allowed() {
                            return SessionEnd::Restart;
                        }
                        tracing::debug!("ignored reconnect request: connected moments ago");
                    }
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
                            let matches = state
                                .in_flight
                                .as_ref()
                                .is_some_and(|f| message.nonce.as_deref() == Some(f.nonce.as_str()));
                            if !matches {
                                continue; // Unrelated dispatch event.
                            }
                            if let Some(request) = state.in_flight.take() {
                                self.handle_response(&mut state, request, &message);
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
                if frame.opcode != Opcode::Frame {
                    continue;
                }
                let Ok(message) = parse_incoming(&frame.payload) else {
                    continue;
                };
                if message.nonce.as_deref() == Some(nonce.as_str()) {
                    if message.is_error() {
                        let (code, text) = message.error_details();
                        return Err(RpcError::Closed { code, message: text });
                    }
                    return Ok::<(), RpcError>(());
                }
            }
            Err(RpcError::Eof)
        })
        .await;
        match attempt {
            Ok(Ok(())) => {
                self.update(|s| s.delivery = ActivityDelivery::Cleared);
                tracing::info!("cleared Discord activity before shutdown");
            }
            Ok(Err(error)) => tracing::warn!(%error, "could not clear the activity before shutdown"),
            Err(_) => tracing::warn!("timed out clearing the activity before shutdown"),
        }
    }
}

#[cfg(test)]
mod tests;
