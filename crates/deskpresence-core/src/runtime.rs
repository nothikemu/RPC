//! Wires detection, the presence engine and the Discord service together.
//!
//! Ownership:
//! - the **detector thread** owns the window system and publishes snapshots;
//! - the **Discord task** owns the IPC connection;
//! - the **engine task** owns configuration, the pin/pause state and the
//!   diagnostic event log. It turns snapshots into the desired activity and
//!   a [`RuntimeView`] for the UI.
//!
//! All channels are bounded or latest-value (`watch`), so no queue can grow
//! without limit. No lock is held across an `await` or an OS call.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep_until, timeout};

use crate::api::{
    AppIdSource, AppRuleStatus, AppView, ApplicationIdView, DetectorView, PresenceView, PreviewRequest,
    PreviewResult, RuntimeView, SelectedView,
};
use crate::app_detection::{
    DetectedApp, DetectorController, DetectorHandle, DetectorSettings, DetectorSnapshot, DetectorStatus,
    WindowSystem,
};
use crate::app_identity::{AppKey, exe_file_name, resolve_display_name};
use crate::config::{Config, is_valid_application_id};
use crate::diagnostics::{EventCategory, EventLevel, EventLog};
use crate::discord_rpc::{
    Activity, ActivityDelivery, ConnectionPhase, Connector, DiscordController, DiscordErrorKind,
    DiscordServiceConfig, DiscordServiceHandle, DiscordStatus, spawn_discord_service,
};
use crate::presence_manager::{
    BuildContext, EngineOutput, PinCleared, PresenceEngine, PresenceField, build_activity, display_name,
    rule_for,
};
use crate::presence_templates::Template;
use crate::rules::{AppFacts, CompiledRules, Eligibility, MatchKind};
use crate::time::now_unix_ms;

/// Number of diagnostic events kept in memory and shown in the UI.
pub const EVENT_CAPACITY: usize = 100;

pub struct RuntimeOptions {
    pub config: Arc<Config>,
    /// Build-time default Discord application ID, used when the user has not set one.
    pub default_application_id: Option<String>,
    /// Process ID reported to Discord (DeskPresence's own).
    pub pid: u32,
}

#[derive(Debug)]
pub enum RuntimeCommand {
    ConfigChanged(Arc<Config>),
    SetPin(Option<AppKey>),
    TogglePin(AppKey),
    SetPaused(bool),
    ReconnectDiscord,
    RescanNow,
    PushEvent {
        level: EventLevel,
        category: EventCategory,
        message: String,
    },
    Shutdown(oneshot::Sender<()>),
}

/// Cloneable access for commands, the tray and event forwarding.
#[derive(Clone)]
pub struct RuntimeHandle {
    commands: mpsc::Sender<RuntimeCommand>,
    view: watch::Receiver<Arc<RuntimeView>>,
    snapshot: watch::Receiver<Arc<DetectorSnapshot>>,
}

impl RuntimeHandle {
    pub async fn send(&self, command: RuntimeCommand) -> bool {
        self.commands.send(command).await.is_ok()
    }

    /// Non-blocking send for synchronous callers (tray menu handlers).
    pub fn try_send(&self, command: RuntimeCommand) -> bool {
        self.commands.try_send(command).is_ok()
    }

    pub fn view(&self) -> Arc<RuntimeView> {
        self.view.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<Arc<RuntimeView>> {
        self.view.clone()
    }

    pub fn snapshot(&self) -> Arc<DetectorSnapshot> {
        self.snapshot.borrow().clone()
    }

    /// Renders templates for the live preview using real detected data.
    pub fn preview(&self, config: &Config, request: &PreviewRequest) -> PreviewResult {
        preview(&self.snapshot(), &self.view(), config, request)
    }
}

/// The running system. Dropping it without [`Runtime::shutdown`] aborts tasks.
pub struct Runtime {
    handle: RuntimeHandle,
    engine: Option<JoinHandle<()>>,
    detector: Option<DetectorHandle>,
    discord: Option<DiscordServiceHandle>,
}

pub fn detector_settings(config: &Config) -> DetectorSettings {
    DetectorSettings {
        poll_interval: Duration::from_millis(u64::from(config.detection.poll_interval_ms)),
        foreground_debounce: Duration::from_millis(u64::from(config.detection.foreground_debounce_ms)),
        capture_titles: config.privacy.capture_window_titles,
    }
}

/// The application ID in effect and where it came from.
pub fn effective_application_id(config: &Config, build_default: Option<&str>) -> ApplicationIdView {
    if let Some(id) = config.discord.application_id.as_deref() {
        return ApplicationIdView {
            effective: Some(id.to_owned()),
            source: AppIdSource::Config,
        };
    }
    match build_default.filter(|id| is_valid_application_id(id)) {
        Some(id) => ApplicationIdView {
            effective: Some(id.to_owned()),
            source: AppIdSource::BuildDefault,
        },
        None => ApplicationIdView {
            effective: None,
            source: AppIdSource::None,
        },
    }
}

impl Runtime {
    /// Starts all services. Must be called from within a Tokio runtime.
    pub fn start<C: Connector>(
        options: RuntimeOptions,
        window_system: Box<dyn WindowSystem>,
        connector: C,
    ) -> std::io::Result<Self> {
        let config = options.config;
        let default_id = options
            .default_application_id
            .filter(|id| is_valid_application_id(id));

        let (snapshot_tx, snapshot_rx) = watch::channel(Arc::new(DetectorSnapshot::default()));
        let detector = DetectorHandle::spawn(window_system, detector_settings(&config), snapshot_tx)?;

        let app_id = effective_application_id(&config, default_id.as_deref());
        let discord_config = DiscordServiceConfig {
            application_id: app_id.effective.clone(),
            pid: options.pid,
        };
        let (desired_tx, desired_rx) = watch::channel(None);
        let discord = spawn_discord_service(connector, discord_config.clone(), desired_rx);

        let (commands_tx, commands_rx) = mpsc::channel(64);
        let started_at_ms = now_unix_ms();
        let mut task = EngineTask {
            compiled: CompiledRules::new(&config.rules, &config.exclusions),
            detector_settings: detector_settings(&config),
            config,
            default_id,
            engine: PresenceEngine::new(),
            events: EventLog::new(EVENT_CAPACITY),
            snapshot: snapshot_rx.borrow().clone(),
            discord_status: discord.status().borrow().clone(),
            discord_config,
            desired_tx,
            view_tx: None,
            detector: detector.controller(),
            discord: discord.controller(),
            started_at_ms,
            next_refresh_ms: None,
            path_exists: HashMap::new(),
        };
        task.refresh_path_cache();
        task.note_initial_state();
        let initial = task.compute_view(&task.evaluate_pure(), 0);
        let (view_tx, view_rx) = watch::channel(Arc::new(initial));
        task.view_tx = Some(view_tx);

        let snapshot_for_task = snapshot_rx.clone();
        let discord_status = discord.status();
        let engine = tokio::spawn(task.run(commands_rx, snapshot_for_task, discord_status));

        Ok(Self {
            handle: RuntimeHandle {
                commands: commands_tx,
                view: view_rx,
                snapshot: snapshot_rx,
            },
            engine: Some(engine),
            detector: Some(detector),
            discord: Some(discord),
        })
    }

    pub fn handle(&self) -> RuntimeHandle {
        self.handle.clone()
    }

    /// Graceful shutdown: stops the engine, clears the Discord activity and
    /// closes the connection, then stops the detector thread. Bounded by `limit`.
    pub async fn shutdown(mut self, limit: Duration) {
        let deadline = Instant::now() + limit;
        let (ack_tx, ack_rx) = oneshot::channel();
        if self.handle.send(RuntimeCommand::Shutdown(ack_tx)).await {
            let _ = timeout(limit / 4, ack_rx).await;
        }
        if let Some(engine) = self.engine.take() {
            engine.abort();
            let _ = engine.await;
        }
        if let Some(discord) = self.discord.take() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            discord.shutdown(remaining.max(Duration::from_millis(200))).await;
        }
        if let Some(detector) = self.detector.take() {
            let remaining = deadline
                .saturating_duration_since(Instant::now())
                .max(Duration::from_millis(200));
            let _ = tokio::task::spawn_blocking(move || detector.shutdown(remaining)).await;
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        if let Some(engine) = self.engine.take() {
            engine.abort();
        }
    }
}

struct EngineTask {
    config: Arc<Config>,
    compiled: CompiledRules,
    default_id: Option<String>,
    engine: PresenceEngine,
    events: EventLog,
    snapshot: Arc<DetectorSnapshot>,
    discord_status: DiscordStatus,
    detector_settings: DetectorSettings,
    discord_config: DiscordServiceConfig,
    desired_tx: watch::Sender<Option<Activity>>,
    view_tx: Option<watch::Sender<Arc<RuntimeView>>>,
    detector: DetectorController,
    discord: DiscordController,
    started_at_ms: u64,
    next_refresh_ms: Option<u64>,
    path_exists: HashMap<String, bool>,
}

impl EngineTask {
    async fn run(
        mut self,
        mut commands: mpsc::Receiver<RuntimeCommand>,
        mut snapshots: watch::Receiver<Arc<DetectorSnapshot>>,
        mut discord_status: watch::Receiver<DiscordStatus>,
    ) {
        let mut snapshots_open = true;
        let mut discord_open = true;
        loop {
            let refresh_at = self.next_refresh_ms.map(|at| {
                let delay = at.saturating_sub(now_unix_ms());
                Instant::now() + Duration::from_millis(delay)
            });
            tokio::select! {
                command = commands.recv() => match command {
                    None => break,
                    Some(RuntimeCommand::Shutdown(ack)) => {
                        let _ = ack.send(());
                        break;
                    }
                    Some(command) => self.handle(command).await,
                },
                changed = snapshots.changed(), if snapshots_open => {
                    if changed.is_err() {
                        snapshots_open = false;
                    } else {
                        let next = snapshots.borrow_and_update().clone();
                        self.note_detector_change(&next);
                        self.snapshot = next;
                    }
                }
                changed = discord_status.changed(), if discord_open => {
                    if changed.is_err() {
                        discord_open = false;
                    } else {
                        let next = discord_status.borrow_and_update().clone();
                        self.note_discord_change(&next);
                        self.discord_status = next;
                    }
                }
                _ = sleep_until(refresh_at.unwrap_or_else(|| Instant::now() + Duration::from_secs(3600))), if refresh_at.is_some() => {}
            }
            self.recompute();
        }
    }

    async fn handle(&mut self, command: RuntimeCommand) {
        match command {
            RuntimeCommand::ConfigChanged(config) => self.apply_config(config).await,
            RuntimeCommand::SetPin(key) => {
                let name = key.as_ref().map(|k| self.name_of(k));
                self.engine.set_pin(key);
                if let Some(name) = name {
                    self.event(
                        EventLevel::Info,
                        EventCategory::Presence,
                        format!("Pinned {name}."),
                    );
                }
            }
            RuntimeCommand::TogglePin(key) => {
                if self.snapshot.app(&key).is_none() {
                    return;
                }
                let name = self.name_of(&key);
                let was_pinned = self.engine.pin() == Some(&key);
                self.engine.toggle_pin(key);
                let message = if was_pinned {
                    format!("Unpinned {name}; automatic selection resumed.")
                } else {
                    format!("Pinned {name}.")
                };
                self.event(EventLevel::Info, EventCategory::Presence, message);
            }
            RuntimeCommand::SetPaused(paused) => {
                if self.engine.paused() != paused {
                    self.engine.set_paused(paused);
                    let message = if paused {
                        "Presence paused."
                    } else {
                        "Presence resumed."
                    };
                    self.event(EventLevel::Info, EventCategory::Presence, message);
                }
            }
            RuntimeCommand::ReconnectDiscord => self.discord.reconnect().await,
            RuntimeCommand::RescanNow => self.detector.rescan(),
            RuntimeCommand::PushEvent {
                level,
                category,
                message,
            } => self.event(level, category, message),
            RuntimeCommand::Shutdown(_) => {}
        }
    }

    async fn apply_config(&mut self, config: Arc<Config>) {
        self.compiled = CompiledRules::new(&config.rules, &config.exclusions);
        self.config = config;
        self.refresh_path_cache();
        let settings = detector_settings(&self.config);
        if settings != self.detector_settings {
            self.detector_settings = settings;
            self.detector.configure(settings);
        }
        let app_id = effective_application_id(&self.config, self.default_id.as_deref());
        let discord_config = DiscordServiceConfig {
            application_id: app_id.effective,
            pid: self.discord_config.pid,
        };
        if discord_config != self.discord_config {
            self.discord_config = discord_config.clone();
            self.discord.configure(discord_config).await;
        }
    }

    fn refresh_path_cache(&mut self) {
        self.path_exists = self
            .config
            .rules
            .iter()
            .filter(|r| r.matcher.kind == MatchKind::Path)
            .map(|r| {
                let exists = std::path::Path::new(r.matcher.value.trim()).exists();
                (crate::app_identity::normalize_path(&r.matcher.value), exists)
            })
            .collect();
    }

    fn event(&mut self, level: EventLevel, category: EventCategory, message: impl Into<String>) {
        let message = message.into();
        match level {
            EventLevel::Error => tracing::error!(?category, "{message}"),
            EventLevel::Warning => tracing::warn!(?category, "{message}"),
            EventLevel::Info => tracing::info!(?category, "{message}"),
        }
        self.events.push(level, category, message, now_unix_ms());
    }

    fn name_of(&self, key: &AppKey) -> String {
        match self.snapshot.app(key) {
            Some(app) => display_name(app, rule_for(key, &self.config, &self.compiled)).name,
            None => exe_file_name(key.as_str()),
        }
    }

    fn note_initial_state(&mut self) {
        if self.discord_config.application_id.is_none() {
            self.event(
                EventLevel::Info,
                EventCategory::Discord,
                "No Discord Application ID is configured. Presence is shown as a preview only.",
            );
        }
        if let Some(id) = self.config.discord.application_id.as_deref()
            && !is_valid_application_id(id)
        {
            self.event(
                EventLevel::Error,
                EventCategory::Config,
                "The configured Discord Application ID is not valid.",
            );
        }
    }

    fn note_detector_change(&mut self, next: &DetectorSnapshot) {
        let previous = self.snapshot.status.clone();
        if previous == next.status {
            return;
        }
        match &next.status {
            DetectorStatus::Unsupported { reason } => {
                self.event(EventLevel::Warning, EventCategory::Detector, reason.clone());
            }
            DetectorStatus::Degraded { message } => {
                self.event(EventLevel::Warning, EventCategory::Detector, message.clone());
            }
            DetectorStatus::Mock => self.event(
                EventLevel::Warning,
                EventCategory::Detector,
                "Using mock application data (DESKPRESENCE_MOCK_DETECTOR=1). These applications are not real.",
            ),
            DetectorStatus::Running { .. } if matches!(previous, DetectorStatus::Degraded { .. }) => {
                self.event(EventLevel::Info, EventCategory::Detector, "Application detection recovered.");
            }
            _ => {}
        }
    }

    fn note_discord_change(&mut self, next: &DiscordStatus) {
        let previous = self.discord_status.clone();
        if next.phase != previous.phase {
            match next.phase {
                ConnectionPhase::Connected => {
                    let message = if next.reconnects > 0 {
                        "Reconnected to Discord."
                    } else {
                        "Connected to Discord."
                    };
                    self.event(EventLevel::Info, EventCategory::Discord, message);
                }
                ConnectionPhase::InvalidApplicationId => self.event(
                    EventLevel::Error,
                    EventCategory::Discord,
                    "Discord rejected the Application ID. Check it in Settings → Discord.",
                ),
                ConnectionPhase::NotConfigured if previous.phase != ConnectionPhase::NotConfigured => {
                    self.event(
                        EventLevel::Info,
                        EventCategory::Discord,
                        "No Discord Application ID is configured. Presence is shown as a preview only.",
                    );
                }
                ConnectionPhase::WaitingToRetry if previous.phase == ConnectionPhase::Connected => {
                    let reason = next.last_error.clone().unwrap_or_default();
                    self.event(
                        EventLevel::Warning,
                        EventCategory::Discord,
                        format!("Lost the connection to Discord ({reason}). Reconnecting automatically."),
                    );
                }
                _ => {}
            }
        }
        if next.phase == ConnectionPhase::WaitingToRetry
            && next.last_error_kind != previous.last_error_kind
            && previous.phase != ConnectionPhase::Connected
        {
            match next.last_error_kind {
                Some(DiscordErrorKind::NotRunning) => self.event(
                    EventLevel::Info,
                    EventCategory::Discord,
                    "Discord isn't running. DeskPresence will connect automatically when it starts.",
                ),
                Some(_) => {
                    let reason = next.last_error.clone().unwrap_or_default();
                    self.event(
                        EventLevel::Warning,
                        EventCategory::Discord,
                        format!("Could not connect to Discord: {reason}"),
                    );
                }
                None => {}
            }
        }
        if next.delivery != previous.delivery {
            match next.delivery {
                ActivityDelivery::Rejected | ActivityDelivery::AcceptedWithoutAssets => {
                    let message = next
                        .last_delivery_error
                        .clone()
                        .unwrap_or_else(|| "Discord rejected the activity.".to_owned());
                    self.event(EventLevel::Warning, EventCategory::Discord, message);
                }
                _ => {}
            }
        }
    }

    /// Evaluates without mutating persistent state (used for the initial view).
    fn evaluate_pure(&self) -> EngineOutput {
        let mut engine = PresenceEngine::new();
        engine.evaluate(&self.snapshot, &self.config, &self.compiled, now_unix_ms())
    }

    fn recompute(&mut self) {
        let output = self
            .engine
            .evaluate(&self.snapshot, &self.config, &self.compiled, now_unix_ms());
        if let Some(cleared) = &output.pin_cleared {
            let message = match cleared {
                PinCleared::Exited(key) => format!(
                    "Pinned application {} exited; automatic selection resumed.",
                    exe_file_name(key.as_str())
                ),
                PinCleared::Ineligible(key) => format!(
                    "Pinned application {} is excluded or disabled; automatic selection resumed.",
                    exe_file_name(key.as_str())
                ),
            };
            self.event(EventLevel::Info, EventCategory::Presence, message);
        }
        self.next_refresh_ms = output.next_refresh_ms;
        self.desired_tx.send_if_modified(|desired| {
            if *desired == output.activity {
                false
            } else {
                desired.clone_from(&output.activity);
                true
            }
        });
        let Some(view_tx) = &self.view_tx else {
            return;
        };
        let view = self.compute_view(&output, 0);
        view_tx.send_if_modified(move |current| {
            if current.same_content(&view) {
                return false;
            }
            let revision = current.revision + 1;
            *current = Arc::new(RuntimeView { revision, ..view });
            true
        });
    }

    fn compute_view(&self, output: &EngineOutput, revision: u64) -> RuntimeView {
        let snapshot = &self.snapshot;
        let selected_key = output.selection.as_ref().map(|s| &s.key);
        let pinned = self.engine.pin();
        let mut apps: Vec<AppView> = Vec::with_capacity(snapshot.apps.len() + snapshot.exited.len());

        let status_for = |key: &AppKey| -> (
            AppRuleStatus,
            Option<String>,
            Option<MatchKind>,
            Option<String>,
            i32,
        ) {
            let facts = AppFacts::new(key);
            let rule_match = self.compiled.find_rule(&facts);
            let rule = rule_match.and_then(|m| self.config.rules.get(m.index));
            let eligibility = self.compiled.eligibility(&facts, &self.config.rules);
            let status = match eligibility {
                Eligibility::Excluded(_) => AppRuleStatus::Excluded,
                Eligibility::Disabled(_) => AppRuleStatus::Disabled,
                Eligibility::Eligible if rule.is_some() => AppRuleStatus::Configured,
                Eligibility::Eligible => AppRuleStatus::Automatic,
            };
            let exclusion_id = match eligibility {
                Eligibility::Excluded(i) => self.config.exclusions.get(i).map(|e| e.id.clone()),
                _ => None,
            };
            (
                status,
                rule.map(|r| r.id.clone()),
                rule_match.map(|m| m.kind),
                exclusion_id,
                rule.map_or(0, |r| r.priority),
            )
        };

        for app in &snapshot.apps {
            let rule = rule_for(&app.key, &self.config, &self.compiled);
            let name = display_name(app, rule);
            let (status, rule_id, rule_match, exclusion_id, priority) = status_for(&app.key);
            apps.push(AppView {
                key: app.key.clone(),
                display_name: name.name,
                name_source: name.source,
                exe_name: app.exe_name.clone(),
                exe_path: app.exe_path.clone(),
                running: true,
                is_foreground: snapshot.foreground.as_ref() == Some(&app.key),
                is_selected: selected_key == Some(&app.key),
                is_pinned: pinned == Some(&app.key),
                window_count: app.window_count,
                first_seen_ms: Some(app.first_seen_ms),
                last_seen_ms: None,
                last_focused_ms: app.last_focused_ms,
                status,
                rule_id,
                rule_match,
                exclusion_id,
                priority,
                path_exists: None,
            });
        }
        for exited in &snapshot.exited {
            if snapshot.app(&exited.key).is_some() {
                continue;
            }
            let rule = rule_for(&exited.key, &self.config, &self.compiled);
            let name = resolve_display_name(
                rule.and_then(|r| r.display_name.as_deref()),
                exited.metadata.as_ref(),
                &exited.exe_name,
            );
            let (status, rule_id, rule_match, exclusion_id, priority) = status_for(&exited.key);
            apps.push(AppView {
                key: exited.key.clone(),
                display_name: name.name,
                name_source: name.source,
                exe_name: exited.exe_name.clone(),
                exe_path: exited.exe_path.clone(),
                running: false,
                is_foreground: false,
                is_selected: false,
                is_pinned: false,
                window_count: 0,
                first_seen_ms: None,
                last_seen_ms: Some(exited.last_seen_ms),
                last_focused_ms: None,
                status,
                rule_id,
                rule_match,
                exclusion_id,
                priority,
                path_exists: None,
            });
        }
        // Configured applications (path rules) that have not been seen yet.
        for rule in &self.config.rules {
            if rule.matcher.kind != MatchKind::Path {
                continue;
            }
            let Some(key) = AppKey::from_path(&rule.matcher.value) else {
                continue;
            };
            if apps.iter().any(|a| a.key == key) {
                continue;
            }
            let exe_name = exe_file_name(&rule.matcher.value);
            let name = resolve_display_name(rule.display_name.as_deref(), None, &exe_name);
            let (status, rule_id, rule_match, exclusion_id, priority) = status_for(&key);
            apps.push(AppView {
                path_exists: self.path_exists.get(key.as_str()).copied(),
                key,
                display_name: name.name,
                name_source: name.source,
                exe_name,
                exe_path: rule.matcher.value.clone(),
                running: false,
                is_foreground: false,
                is_selected: false,
                is_pinned: false,
                window_count: 0,
                first_seen_ms: None,
                last_seen_ms: None,
                last_focused_ms: None,
                status,
                rule_id,
                rule_match,
                exclusion_id,
                priority,
            });
        }
        apps.sort_by(|a, b| {
            b.running
                .cmp(&a.running)
                .then_with(|| a.display_name.to_lowercase().cmp(&b.display_name.to_lowercase()))
                .then_with(|| a.key.cmp(&b.key))
        });

        let selected = output.selection.as_ref().map(|s| SelectedView {
            key: s.key.clone(),
            display_name: output
                .display_name
                .clone()
                .unwrap_or_else(|| exe_file_name(s.key.as_str())),
            reason: s.reason,
        });

        RuntimeView {
            revision,
            started_at_ms: self.started_at_ms,
            detector: DetectorView {
                status: snapshot.status.clone(),
                stats: snapshot.stats,
                running_count: u32::try_from(snapshot.apps.len()).unwrap_or(u32::MAX),
            },
            apps,
            presence: PresenceView {
                paused: self.engine.paused(),
                mode: self.config.presence.selection_mode,
                pinned: pinned.cloned(),
                selected,
                activity: output.activity.clone(),
                warnings: output.warnings.clone(),
                source_since_ms: output.source_since_ms,
            },
            discord: self.discord_status.clone(),
            application_id: effective_application_id(&self.config, self.default_id.as_deref()),
            events: self.events.recent(EVENT_CAPACITY),
        }
    }
}

/// Sample values used by the preview when no application is running.
const SAMPLE_APP: &str = "Example Editor";
const SAMPLE_EXE: &str = "example.exe";
const SAMPLE_TITLE: &str = "notes.txt - Example Editor";

fn preview(
    snapshot: &DetectorSnapshot,
    view: &RuntimeView,
    config: &Config,
    request: &PreviewRequest,
) -> PreviewResult {
    let compiled = CompiledRules::new(&config.rules, &config.exclusions);
    let requested = request.app_key.as_deref().and_then(AppKey::parse);
    let app: Option<&DetectedApp> = requested
        .as_ref()
        .and_then(|k| snapshot.app(k))
        .or_else(|| view.presence.selected.as_ref().and_then(|s| snapshot.app(&s.key)))
        .or_else(|| snapshot.apps.first());

    let mut preview_config = config.clone();
    preview_config.presence.details_template = request.details_template.clone();
    preview_config.presence.state_template = request.state_template.clone();

    let (name, exe, title, elapsed) = match app {
        Some(app) => {
            let rule = rule_for(&app.key, config, &compiled);
            let name = display_name(app, rule);
            let elapsed = view
                .presence
                .selected
                .as_ref()
                .filter(|s| s.key == app.key)
                .and(view.presence.source_since_ms)
                .map(|since| Duration::from_millis(now_unix_ms().saturating_sub(since)))
                .unwrap_or_default();
            (name.name, app.exe_name.clone(), app.window_title.clone(), elapsed)
        }
        None => (
            SAMPLE_APP.to_owned(),
            SAMPLE_EXE.to_owned(),
            Some(SAMPLE_TITLE.to_owned()),
            Duration::from_secs(25 * 60),
        ),
    };
    let built = build_activity(&BuildContext {
        config: &preview_config,
        rule: None,
        display_name: &name,
        exe_name: &exe,
        window_title: title.as_deref(),
        elapsed: Some(elapsed),
        timestamp_start_s: None,
    });
    PreviewResult {
        app_display_name: name,
        app_exe_name: exe,
        sample: app.is_none(),
        details: built.activity.details,
        state: built.activity.state,
        details_error: Template::parse(&request.details_template).err(),
        state_error: Template::parse(&request.state_template).err(),
        warnings: built
            .warnings
            .into_iter()
            .filter(|w| matches!(w.field, PresenceField::Details | PresenceField::State))
            .collect(),
    }
}

#[cfg(test)]
mod tests;
