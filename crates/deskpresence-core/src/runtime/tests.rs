//! End-to-end tests: mock window system → detector thread → engine →
//! Discord service → mock Discord server, using real time.

use super::*;
use crate::app_detection::mock::{MockApp, MockWindowSystem};
use crate::discord_rpc::mock::{Behavior, MockDiscord};
use crate::presence_manager::SelectionReason;
use crate::rules::{AppMatcher, ExclusionRule};

const APP_ID: &str = "123456789012345678";
const ALPHA: &str = r"C:\Apps\Alpha\alpha.exe";
const BETA: &str = r"C:\Apps\Beta\beta.exe";

fn fast_config(app_id: Option<&str>) -> Config {
    let mut config = Config::default();
    config.discord.application_id = app_id.map(Into::into);
    config.detection.poll_interval_ms = 250;
    config.detection.foreground_debounce_ms = 0;
    config
}

async fn eventually<T>(what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(value) = check() {
            return value;
        }
        if Instant::now() > deadline {
            panic!("timed out waiting for {what}");
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn last_details(mock: &MockDiscord) -> Option<String> {
    mock.activities()
        .last()
        .and_then(|a| a["details"].as_str().map(str::to_owned))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn detection_to_presence_to_shutdown() {
    let ws = MockWindowSystem::new();
    ws.add(MockApp::new(10, ALPHA).description("Alpha Studio"));
    ws.add(MockApp::new(11, BETA).description("Beta Player"));
    ws.focus_pid(10);
    let mock = MockDiscord::new(Behavior::Normal);

    let runtime = Runtime::start(
        RuntimeOptions {
            config: Arc::new(fast_config(Some(APP_ID))),
            default_application_id: None,
            pid: 999,
        },
        Box::new(ws.clone()),
        mock.clone(),
    )
    .unwrap();
    let handle = runtime.handle();

    // Foreground application becomes the presence.
    eventually("alpha presence", || {
        (last_details(&mock).as_deref() == Some("Using Alpha Studio")).then_some(())
    })
    .await;
    let view = eventually("connected view", || {
        let v = handle.view();
        (v.discord.phase == ConnectionPhase::Connected && v.apps.len() == 2).then_some(v)
    })
    .await;
    let selected = view.presence.selected.clone().unwrap();
    assert_eq!(selected.display_name, "Alpha Studio");
    assert_eq!(selected.reason, SelectionReason::Foreground);
    assert!(view.events.iter().any(|e| e.message == "Connected to Discord."));

    // Focus switch follows.
    ws.focus_pid(11);
    eventually("beta presence", || {
        (last_details(&mock).as_deref() == Some("Using Beta Player")).then_some(())
    })
    .await;

    // Pinning overrides the foreground.
    assert!(
        handle
            .send(RuntimeCommand::TogglePin(AppKey::from_path(ALPHA).unwrap()))
            .await
    );
    eventually("pinned alpha", || {
        (last_details(&mock).as_deref() == Some("Using Alpha Studio")).then_some(())
    })
    .await;
    assert_eq!(handle.view().presence.pinned, AppKey::from_path(ALPHA));

    // The pinned application exits: the pin clears and Beta (focused) returns.
    ws.remove_pid(10);
    eventually("pin cleared", || {
        (last_details(&mock).as_deref() == Some("Using Beta Player")).then_some(())
    })
    .await;
    let view = handle.view();
    assert_eq!(view.presence.pinned, None);
    assert!(
        view.events
            .iter()
            .any(|e| e.message.contains("exited; automatic selection resumed"))
    );
    assert!(
        view.apps
            .iter()
            .any(|a| a.exe_name == "alpha.exe" && !a.running && a.last_seen_ms.is_some())
    );

    // Excluding the only remaining application clears the activity.
    let mut config = fast_config(Some(APP_ID));
    config.exclusions.push(ExclusionRule {
        id: "x-beta".into(),
        matcher: AppMatcher::exe_name("beta.exe"),
        note: None,
    });
    assert!(handle.send(RuntimeCommand::ConfigChanged(Arc::new(config))).await);
    eventually("cleared", || {
        (mock.activities().last() == Some(&serde_json::Value::Null)).then_some(())
    })
    .await;
    let beta = handle
        .view()
        .apps
        .iter()
        .find(|a| a.exe_name == "beta.exe")
        .cloned()
        .unwrap();
    assert_eq!(beta.status, AppRuleStatus::Excluded);
    assert_eq!(beta.exclusion_id.as_deref(), Some("x-beta"));

    runtime.shutdown(Duration::from_secs(3)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pause_clears_and_resume_restores() {
    let ws = MockWindowSystem::new();
    ws.add(MockApp::new(10, ALPHA).description("Alpha Studio"));
    ws.focus_pid(10);
    let mock = MockDiscord::new(Behavior::Normal);
    let runtime = Runtime::start(
        RuntimeOptions {
            config: Arc::new(fast_config(Some(APP_ID))),
            default_application_id: None,
            pid: 1,
        },
        Box::new(ws.clone()),
        mock.clone(),
    )
    .unwrap();
    let handle = runtime.handle();
    eventually("presence", || last_details(&mock)).await;

    handle.send(RuntimeCommand::SetPaused(true)).await;
    eventually("cleared on pause", || {
        (mock.activities().last() == Some(&serde_json::Value::Null)).then_some(())
    })
    .await;
    assert!(handle.view().presence.paused);

    handle.send(RuntimeCommand::SetPaused(false)).await;
    eventually("restored", || {
        (last_details(&mock).as_deref() == Some("Using Alpha Studio")).then_some(())
    })
    .await;

    // Shutdown clears the activity that is currently shown.
    runtime.shutdown(Duration::from_secs(3)).await;
    assert_eq!(mock.activities().last(), Some(&serde_json::Value::Null));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_application_id_presence_is_preview_only() {
    let ws = MockWindowSystem::new();
    ws.add(MockApp::new(10, ALPHA).description("Alpha Studio"));
    ws.focus_pid(10);
    let mock = MockDiscord::new(Behavior::Normal);
    let runtime = Runtime::start(
        RuntimeOptions {
            config: Arc::new(fast_config(None)),
            default_application_id: Some("not-a-valid-id".into()),
            pid: 1,
        },
        Box::new(ws.clone()),
        mock.clone(),
    )
    .unwrap();
    let handle = runtime.handle();
    let view = eventually("preview activity", || {
        let v = handle.view();
        v.presence.activity.is_some().then_some(v)
    })
    .await;
    assert_eq!(view.discord.phase, ConnectionPhase::NotConfigured);
    assert_eq!(view.application_id.source, AppIdSource::None);
    assert_eq!(
        view.presence.activity.as_ref().and_then(|a| a.details.as_deref()),
        Some("Using Alpha Studio")
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(mock.connects(), 0);

    let preview = handle.preview(
        &fast_config(None),
        &PreviewRequest {
            details_template: "In {app}".into(),
            state_template: "{bogus}".into(),
            app_key: None,
        },
    );
    assert_eq!(preview.details.as_deref(), Some("In Alpha Studio"));
    assert!(!preview.sample);
    assert!(preview.state_error.is_some());
    runtime.shutdown(Duration::from_secs(3)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn build_default_application_id_is_used_when_valid() {
    let ws = MockWindowSystem::new();
    let mock = MockDiscord::new(Behavior::Normal);
    let runtime = Runtime::start(
        RuntimeOptions {
            config: Arc::new(fast_config(None)),
            default_application_id: Some(APP_ID.into()),
            pid: 1,
        },
        Box::new(ws),
        mock.clone(),
    )
    .unwrap();
    let handle = runtime.handle();
    let view = eventually("connected", || {
        let v = handle.view();
        (v.discord.phase == ConnectionPhase::Connected).then_some(v)
    })
    .await;
    assert_eq!(view.application_id.source, AppIdSource::BuildDefault);
    assert_eq!(mock.handshakes()[0]["client_id"], APP_ID);

    // With nothing running, the preview falls back to clearly-marked sample data.
    let preview = handle.preview(
        &Config::default(),
        &PreviewRequest {
            details_template: "{app}".into(),
            state_template: String::new(),
            app_key: None,
        },
    );
    assert!(preview.sample);
    assert_eq!(preview.details.as_deref(), Some(SAMPLE_APP));
    runtime.shutdown(Duration::from_secs(3)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diagnostics_export_hides_window_titles_and_user_names_by_default() {
    use crate::api::{BundleInput, build_diagnostics_bundle};
    use crate::diagnostics::Redactor;
    use crate::rules::AssetOverrides;

    const SECRET: &str = "Quarterly plan draft";
    let path = r"C:\Users\Alice\Apps\alpha.exe";
    let ws = MockWindowSystem::new();
    ws.add(MockApp::new(10, path).description("Alpha Studio").title(SECRET));
    ws.focus_pid(10);
    let mut config = fast_config(None);
    config.privacy.capture_window_titles = true;
    config.presence.details_template = "{window_title}".into();
    config.presence.default_assets = AssetOverrides {
        large_image: Some("logo".into()),
        large_text: Some("Editing {window_title}".into()),
        ..AssetOverrides::default()
    };
    let runtime = Runtime::start(
        RuntimeOptions {
            config: Arc::new(config.clone()),
            default_application_id: None,
            pid: 1,
        },
        Box::new(ws.clone()),
        MockDiscord::new(Behavior::Normal),
    )
    .unwrap();
    let handle = runtime.handle();
    let view = eventually("activity with the window title", || {
        let v = handle.view();
        let has_title = v
            .presence
            .activity
            .as_ref()
            .and_then(|a| a.assets.as_ref())
            .and_then(|a| a.large_text.as_deref())
            .is_some_and(|t| t.contains(SECRET));
        has_title.then_some(v)
    })
    .await;

    let redactor = Redactor::with_home(Some(r"C:\Users\Alice"));
    let export = |include_sensitive| {
        build_diagnostics_bundle(&BundleInput {
            app_version: "test",
            os: "test",
            generated_at_ms: 0,
            config: &config,
            view: &view,
            log_tail: r"opened C:\Users\Alice\Apps\alpha.exe",
            include_sensitive,
            redactor: &redactor,
        })
        .to_string()
    };
    let redacted = export(false);
    assert!(!redacted.contains(SECRET), "{redacted}");
    assert!(!redacted.contains("Alice"), "{redacted}");
    assert!(redacted.contains("\"large_image\":\"logo\""), "{redacted}");
    let full = export(true);
    assert!(full.contains(SECRET) && full.contains("Alice"));
    runtime.shutdown(Duration::from_secs(3)).await;
}
