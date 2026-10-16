//! Lifecycle tests against the in-memory mock Discord server. Tokio's clock
//! is paused, so backoff and timeouts run in virtual time deterministically.

use super::*;
use crate::discord_rpc::activity::ActivityAssets;
use crate::discord_rpc::mock::{Behavior, MockDiscord};
use crate::discord_rpc::protocol::Opcode;

const APP_ID: &str = "123456789012345678";
const OTHER_ID: &str = "876543210987654321";

fn config(id: Option<&str>) -> DiscordServiceConfig {
    DiscordServiceConfig {
        application_id: id.map(Into::into),
        pid: 4242,
    }
}

fn activity(details: &str) -> Activity {
    Activity {
        activity_type: 0,
        name: None,
        details: Some(details.into()),
        state: None,
        status_display_type: None,
        timestamps: None,
        assets: None,
    }
}

async fn wait_for(
    rx: &mut watch::Receiver<DiscordStatus>,
    what: &str,
    pred: impl Fn(&DiscordStatus) -> bool,
) -> DiscordStatus {
    let result = timeout(Duration::from_secs(900), async {
        loop {
            {
                let status = rx.borrow_and_update();
                if pred(&status) {
                    return status.clone();
                }
            }
            if rx.changed().await.is_err() {
                panic!("status channel closed while waiting for {what}");
            }
        }
    })
    .await;
    result.unwrap_or_else(|_| panic!("timed out waiting for {what}; last status {:?}", *rx.borrow()))
}

async fn settle(duration: Duration) {
    tokio::time::sleep(duration).await;
}

fn start(
    mock: &MockDiscord,
    id: Option<&str>,
) -> (
    DiscordServiceHandle,
    watch::Sender<Option<Activity>>,
    watch::Receiver<DiscordStatus>,
) {
    let (desired_tx, desired_rx) = watch::channel(None);
    let handle = spawn_discord_service(mock.clone(), config(id), desired_rx);
    let status = handle.status();
    (handle, desired_tx, status)
}

#[tokio::test(start_paused = true)]
async fn waits_for_an_application_id_before_connecting() {
    let mock = MockDiscord::new(Behavior::Normal);
    let (handle, _desired, mut status) = start(&mock, None);
    wait_for(&mut status, "not configured", |s| {
        s.phase == ConnectionPhase::NotConfigured
    })
    .await;
    settle(Duration::from_secs(30)).await;
    assert_eq!(mock.connects(), 0, "must not connect without an application ID");

    handle.configure(config(Some(APP_ID))).await;
    let s = wait_for(&mut status, "connected", |s| {
        s.phase == ConnectionPhase::Connected
    })
    .await;
    assert_eq!(s.user_name.as_deref(), Some("Test User"));
    assert_eq!(s.endpoint.as_deref(), Some("mock-ipc-1"));
    assert_eq!(
        mock.handshakes()[0],
        serde_json::json!({"v": 1, "client_id": APP_ID})
    );
    handle.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test(start_paused = true)]
async fn sends_activity_and_suppresses_duplicates() {
    let mock = MockDiscord::new(Behavior::Normal);
    let (handle, desired, mut status) = start(&mock, Some(APP_ID));
    desired.send_replace(Some(activity("A")));
    wait_for(&mut status, "accepted", |s| {
        s.delivery == ActivityDelivery::Accepted
    })
    .await;
    let sent = mock.received();
    let set = sent.iter().find(|r| r.json["cmd"] == "SET_ACTIVITY").unwrap();
    assert_eq!(set.json["args"]["pid"], 4242);
    assert_eq!(set.json["args"]["activity"]["details"], "A");

    // Re-publishing an identical activity must not produce another update.
    desired.send_replace(Some(activity("A")));
    settle(Duration::from_secs(30)).await;
    assert_eq!(mock.activities().len(), 1);

    desired.send_replace(Some(activity("B")));
    wait_for(&mut status, "second update", |_| mock.activities().len() == 2).await;
    wait_for(&mut status, "accepted B", |s| {
        s.delivery == ActivityDelivery::Accepted
    })
    .await;

    // No eligible application: the activity is cleared, not left stale.
    desired.send_replace(None);
    wait_for(&mut status, "cleared", |s| {
        s.delivery == ActivityDelivery::Cleared
    })
    .await;
    assert_eq!(mock.activities().last(), Some(&serde_json::Value::Null));
    handle.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test(start_paused = true)]
async fn invalid_application_id_is_reported_and_not_hammered() {
    let mock = MockDiscord::new(Behavior::InvalidClientId);
    let (handle, _desired, mut status) = start(&mock, Some(APP_ID));
    let s = wait_for(&mut status, "invalid id", |s| {
        s.phase == ConnectionPhase::InvalidApplicationId
    })
    .await;
    assert_eq!(s.last_error_kind, Some(DiscordErrorKind::InvalidApplicationId));
    assert!(s.next_retry_at_ms.is_some());
    settle(Duration::from_secs(200)).await;
    assert_eq!(mock.connects(), 1, "an invalid ID must not be retried rapidly");

    // Fixing the ID in Discord's portal + asking to reconnect connects.
    mock.set_behavior(Behavior::Normal);
    handle.reconnect().await;
    wait_for(&mut status, "connected", |s| {
        s.phase == ConnectionPhase::Connected
    })
    .await;
    assert_eq!(mock.connects(), 2);
    handle.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test(start_paused = true)]
async fn backs_off_while_discord_is_not_running_then_connects() {
    let mock = MockDiscord::new(Behavior::NotRunning);
    let (handle, desired, mut status) = start(&mock, Some(APP_ID));
    desired.send_replace(Some(activity("queued")));
    let s = wait_for(&mut status, "waiting", |s| {
        s.phase == ConnectionPhase::WaitingToRetry
    })
    .await;
    assert_eq!(s.last_error_kind, Some(DiscordErrorKind::NotRunning));

    let before = Instant::now();
    settle(Duration::from_secs(60)).await;
    let attempts = status.borrow().failed_attempts;
    // Delays are at least 0.5, 1, 2, 4, 8, 16, 32 s: no more than 7 attempts per minute.
    assert!(
        (3..=8).contains(&attempts),
        "attempts in first minute: {attempts}"
    );
    assert!(before.elapsed() >= Duration::from_secs(60));

    // Discord starts later: the queued activity is delivered once connected.
    mock.set_behavior(Behavior::Normal);
    wait_for(&mut status, "accepted", |s| {
        s.delivery == ActivityDelivery::Accepted
    })
    .await;
    assert_eq!(mock.activities()[0]["details"], "queued");
    let s = status.borrow().clone();
    assert_eq!(s.failed_attempts, 0);
    assert_eq!(s.last_error, None);
    handle.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test(start_paused = true)]
async fn reconnects_after_discord_restarts_and_resends_activity() {
    let mock = MockDiscord::new(Behavior::Normal);
    let (handle, desired, mut status) = start(&mock, Some(APP_ID));
    desired.send_replace(Some(activity("A")));
    wait_for(&mut status, "accepted", |s| {
        s.delivery == ActivityDelivery::Accepted
    })
    .await;

    mock.drop_all();
    wait_for(&mut status, "disconnected", |s| {
        s.phase != ConnectionPhase::Connected
    })
    .await;
    let s = wait_for(&mut status, "reconnected", |s| {
        s.phase == ConnectionPhase::Connected && s.reconnects == 1
    })
    .await;
    assert_eq!(s.endpoint.as_deref(), Some("mock-ipc-2"));
    wait_for(&mut status, "re-sent", |s| {
        s.delivery == ActivityDelivery::Accepted
    })
    .await;
    let resent: Vec<_> = mock
        .received()
        .into_iter()
        .filter(|r| r.connection == 2 && r.json["cmd"] == "SET_ACTIVITY")
        .collect();
    assert_eq!(resent.len(), 1);
    assert_eq!(resent[0].json["args"]["activity"]["details"], "A");
    handle.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test(start_paused = true)]
async fn rejected_artwork_falls_back_to_text_only() {
    let mock = MockDiscord::new(Behavior::RejectAssets);
    let (handle, desired, mut status) = start(&mock, Some(APP_ID));
    let mut with_art = activity("A");
    with_art.assets = Some(ActivityAssets {
        large_image: Some("missing_key".into()),
        ..ActivityAssets::default()
    });
    desired.send_replace(Some(with_art));
    let s = wait_for(&mut status, "accepted without assets", |s| {
        s.delivery == ActivityDelivery::AcceptedWithoutAssets
    })
    .await;
    assert!(s.last_delivery_error.unwrap().contains("artwork"));
    let activities = mock.activities();
    assert_eq!(activities.len(), 2);
    assert!(activities[0].get("assets").is_some());
    assert!(activities[1].get("assets").is_none());
    assert_eq!(activities[1]["details"], "A");

    // The same desired activity is not retried in a loop.
    settle(Duration::from_secs(60)).await;
    assert_eq!(mock.activities().len(), 2);
    handle.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test(start_paused = true)]
async fn rejected_activity_is_not_resent_until_it_changes() {
    let mock = MockDiscord::new(Behavior::RejectAll);
    let (handle, desired, mut status) = start(&mock, Some(APP_ID));
    desired.send_replace(Some(activity("A")));
    let s = wait_for(&mut status, "rejected", |s| {
        s.delivery == ActivityDelivery::Rejected
    })
    .await;
    assert!(s.last_delivery_error.unwrap().contains("code 4000"));
    settle(Duration::from_secs(60)).await;
    assert_eq!(mock.activities().len(), 1);
    assert_eq!(
        status.borrow().phase,
        ConnectionPhase::Connected,
        "a rejection is not a disconnect"
    );

    desired.send_replace(Some(activity("B")));
    wait_for(&mut status, "second attempt", |_| mock.activities().len() == 2).await;
    handle.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test(start_paused = true)]
async fn unanswered_requests_time_out_and_reconnect() {
    let mock = MockDiscord::new(Behavior::Silent);
    let (handle, desired, mut status) = start(&mock, Some(APP_ID));
    desired.send_replace(Some(activity("A")));
    let s = wait_for(&mut status, "timeout", |s| {
        s.last_error_kind == Some(DiscordErrorKind::Timeout)
    })
    .await;
    assert_eq!(s.phase, ConnectionPhase::WaitingToRetry);
    mock.set_behavior(Behavior::Normal);
    wait_for(&mut status, "recovered", |s| {
        s.delivery == ActivityDelivery::Accepted
    })
    .await;
    assert!(mock.connects() >= 2);
    handle.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test(start_paused = true)]
async fn answers_pings_with_pongs() {
    let mock = MockDiscord::new(Behavior::Normal);
    let (handle, _desired, mut status) = start(&mock, Some(APP_ID));
    wait_for(&mut status, "connected", |s| {
        s.phase == ConnectionPhase::Connected
    })
    .await;
    mock.ping_all();
    settle(Duration::from_millis(100)).await;
    assert_eq!(mock.pongs(), 1);
    let pong = mock
        .received()
        .into_iter()
        .find(|r| r.opcode == Opcode::Pong)
        .unwrap();
    assert_eq!(
        pong.json,
        serde_json::json!({"probe": 1}),
        "PONG echoes the PING payload"
    );
    assert_eq!(status.borrow().phase, ConnectionPhase::Connected);
    handle.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test(start_paused = true)]
async fn shutdown_clears_activity_and_stops() {
    let mock = MockDiscord::new(Behavior::Normal);
    let (handle, desired, mut status) = start(&mock, Some(APP_ID));
    desired.send_replace(Some(activity("A")));
    wait_for(&mut status, "accepted", |s| {
        s.delivery == ActivityDelivery::Accepted
    })
    .await;
    handle.shutdown(Duration::from_secs(2)).await;
    assert_eq!(mock.activities().last(), Some(&serde_json::Value::Null));
    assert_eq!(status.borrow().phase, ConnectionPhase::Stopped);
}

#[tokio::test(start_paused = true)]
async fn shutdown_while_disconnected_is_immediate() {
    let mock = MockDiscord::new(Behavior::NotRunning);
    let (handle, _desired, mut status) = start(&mock, Some(APP_ID));
    wait_for(&mut status, "waiting", |s| {
        s.phase == ConnectionPhase::WaitingToRetry
    })
    .await;
    let started = Instant::now();
    handle.shutdown(Duration::from_secs(5)).await;
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(status.borrow().phase, ConnectionPhase::Stopped);
}

#[tokio::test(start_paused = true)]
async fn bursts_are_rate_limited_and_coalesced() {
    let mock = MockDiscord::new(Behavior::Normal);
    let (handle, desired, mut status) = start(&mock, Some(APP_ID));
    wait_for(&mut status, "connected", |s| {
        s.phase == ConnectionPhase::Connected
    })
    .await;
    let start = Instant::now();
    for i in 0..20 {
        desired.send_replace(Some(activity(&format!("update {i}"))));
        settle(Duration::from_millis(100)).await;
    }
    assert!(start.elapsed() < Duration::from_secs(19));
    assert!(
        mock.activities().len() <= RATE_LIMIT_MAX,
        "sent {}",
        mock.activities().len()
    );

    settle(Duration::from_secs(25)).await;
    let activities = mock.activities();
    assert_eq!(activities.last().unwrap()["details"], "update 19");
    assert!(activities.len() <= RATE_LIMIT_MAX + 1);
    handle.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test(start_paused = true)]
async fn changing_the_application_id_reconnects() {
    let mock = MockDiscord::new(Behavior::Normal);
    let (handle, _desired, mut status) = start(&mock, Some(APP_ID));
    wait_for(&mut status, "connected", |s| {
        s.phase == ConnectionPhase::Connected
    })
    .await;
    handle.configure(config(Some(OTHER_ID))).await;
    wait_for(&mut status, "second connection", |s| {
        s.phase == ConnectionPhase::Connected && mock.connects() == 2
    })
    .await;
    let ids: Vec<_> = mock.handshakes().iter().map(|h| h["client_id"].clone()).collect();
    assert_eq!(ids, vec![APP_ID, OTHER_ID]);

    handle.configure(config(None)).await;
    wait_for(&mut status, "not configured", |s| {
        s.phase == ConnectionPhase::NotConfigured
    })
    .await;
    handle.shutdown(Duration::from_secs(1)).await;
}
