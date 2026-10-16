//! In-memory mock of Discord's IPC server for protocol and lifecycle tests.
//!
//! It speaks the real framing over `tokio::io::duplex` streams, validates the
//! handshake, records every frame it receives and can be scripted to reject
//! the application ID, reject activities or artwork, stay silent, ping, or
//! drop all connections (simulating Discord quitting).

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, DuplexStream};
use tokio::sync::broadcast;

use super::RpcError;
use super::protocol::{Opcode, encode_frame, read_frame};
use super::transport::{Connected, Connector};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Behavior {
    /// No IPC endpoint exists.
    NotRunning,
    /// Accepts handshakes and acknowledges every SET_ACTIVITY.
    Normal,
    /// Closes the handshake with code 4000.
    InvalidClientId,
    /// Answers SET_ACTIVITY with an ERROR when the activity has assets.
    RejectAssets,
    /// Answers every SET_ACTIVITY with an ERROR.
    RejectAll,
    /// Completes the handshake, then never answers commands.
    Silent,
    /// Answers the handshake with an ERROR event carrying this RPC error code.
    HandshakeError(i64),
}

#[derive(Clone, Debug)]
pub struct Received {
    pub connection: u32,
    pub opcode: Opcode,
    pub json: Value,
}

#[derive(Clone, Debug)]
enum Inject {
    Ping,
    DropAll,
}

struct Inner {
    behavior: Mutex<Behavior>,
    received: Mutex<Vec<Received>>,
    connects: AtomicU32,
    inject: broadcast::Sender<Inject>,
}

#[derive(Clone)]
pub struct MockDiscord {
    inner: Arc<Inner>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl MockDiscord {
    pub fn new(behavior: Behavior) -> Self {
        let (inject, _) = broadcast::channel(16);
        Self {
            inner: Arc::new(Inner {
                behavior: Mutex::new(behavior),
                received: Mutex::new(Vec::new()),
                connects: AtomicU32::new(0),
                inject,
            }),
        }
    }

    pub fn set_behavior(&self, behavior: Behavior) {
        *lock(&self.inner.behavior) = behavior;
    }

    /// Successful transport connections (handshake attempts).
    pub fn connects(&self) -> u32 {
        self.inner.connects.load(Ordering::SeqCst)
    }

    pub fn received(&self) -> Vec<Received> {
        lock(&self.inner.received).clone()
    }

    /// `activity` argument of every SET_ACTIVITY received (Null when clearing).
    pub fn activities(&self) -> Vec<Value> {
        self.received()
            .into_iter()
            .filter(|r| r.json["cmd"] == "SET_ACTIVITY")
            .map(|r| r.json["args"]["activity"].clone())
            .collect()
    }

    pub fn handshakes(&self) -> Vec<Value> {
        self.received()
            .into_iter()
            .filter(|r| r.opcode == Opcode::Handshake)
            .map(|r| r.json)
            .collect()
    }

    pub fn pongs(&self) -> usize {
        self.received()
            .iter()
            .filter(|r| r.opcode == Opcode::Pong)
            .count()
    }

    pub fn ping_all(&self) {
        let _ = self.inner.inject.send(Inject::Ping);
    }

    /// Simulates Discord exiting: every open connection is closed.
    pub fn drop_all(&self) {
        let _ = self.inner.inject.send(Inject::DropAll);
    }

    /// Serves one connection accepted from a real transport (Unix socket or
    /// Windows named pipe) with this mock's behavior.
    pub fn serve_stream<S>(&self, stream: S) -> tokio::task::JoinHandle<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let id = self.inner.connects.fetch_add(1, Ordering::SeqCst) + 1;
        let inner = Arc::clone(&self.inner);
        let inject = self.inner.inject.subscribe();
        tokio::spawn(serve(inner, stream, id, inject))
    }
}

impl Connector for MockDiscord {
    type Stream = DuplexStream;

    async fn connect(&self) -> Result<Connected<DuplexStream>, RpcError> {
        if *lock(&self.inner.behavior) == Behavior::NotRunning {
            return Err(RpcError::NotRunning);
        }
        let id = self.inner.connects.fetch_add(1, Ordering::SeqCst) + 1;
        let (client, server) = tokio::io::duplex(64 * 1024);
        let inner = Arc::clone(&self.inner);
        let inject = self.inner.inject.subscribe();
        tokio::spawn(serve(inner, server, id, inject));
        Ok(Connected {
            stream: client,
            endpoint: format!("mock-ipc-{id}"),
        })
    }
}

async fn send<S: AsyncWrite + Unpin>(stream: &mut S, opcode: Opcode, value: &Value) -> bool {
    let Ok(bytes) = encode_frame(opcode, value.to_string().as_bytes()) else {
        return false;
    };
    stream.write_all(&bytes).await.is_ok()
}

async fn serve<S: AsyncRead + AsyncWrite + Unpin>(
    inner: Arc<Inner>,
    mut stream: S,
    id: u32,
    mut inject: broadcast::Receiver<Inject>,
) {
    let record = |opcode: Opcode, json: Value| {
        lock(&inner.received).push(Received {
            connection: id,
            opcode,
            json,
        });
    };

    // Handshake.
    let Ok(frame) = read_frame(&mut stream).await else {
        return;
    };
    let json: Value = serde_json::from_slice(&frame.payload).unwrap_or(Value::Null);
    record(frame.opcode, json.clone());
    if frame.opcode != Opcode::Handshake || json["v"] != 1 || !json["client_id"].is_string() {
        let _ = send(
            &mut stream,
            Opcode::Close,
            &json!({"code": 4000, "message": "Invalid payload"}),
        )
        .await;
        return;
    }
    let behavior = *lock(&inner.behavior);
    if let Behavior::HandshakeError(code) = behavior {
        let _ = send(
            &mut stream,
            Opcode::Frame,
            &json!({"cmd": "DISPATCH", "evt": "ERROR", "data": {"code": code, "message": "handshake refused"}}),
        )
        .await;
        return;
    }
    if behavior == Behavior::InvalidClientId {
        let _ = send(
            &mut stream,
            Opcode::Close,
            &json!({"code": 4000, "message": "Invalid Client ID"}),
        )
        .await;
        return;
    }
    let ready = json!({
        "cmd": "DISPATCH",
        "evt": "READY",
        "data": {
            "v": 1,
            "config": {"cdn_host": "cdn.discordapp.com", "api_endpoint": "//discord.com/api", "environment": "production"},
            "user": {"id": "1", "username": "tester", "global_name": "Test User"}
        }
    });
    if !send(&mut stream, Opcode::Frame, &ready).await {
        return;
    }

    loop {
        tokio::select! {
            frame = read_frame(&mut stream) => {
                let Ok(frame) = frame else { return };
                let json: Value = serde_json::from_slice(&frame.payload).unwrap_or(Value::Null);
                record(frame.opcode, json.clone());
                if frame.opcode != Opcode::Frame || json["cmd"] != "SET_ACTIVITY" {
                    continue;
                }
                let behavior = *lock(&inner.behavior);
                let nonce = json["nonce"].clone();
                let activity = json["args"]["activity"].clone();
                let has_assets = activity.get("assets").is_some();
                let reply = match behavior {
                    Behavior::Silent => continue,
                    Behavior::RejectAll => Some(json!({
                        "cmd": "SET_ACTIVITY", "evt": "ERROR", "nonce": nonce,
                        "data": {"code": 4000, "message": "child \"activity\" fails"}
                    })),
                    Behavior::RejectAssets if has_assets => Some(json!({
                        "cmd": "SET_ACTIVITY", "evt": "ERROR", "nonce": nonce,
                        "data": {"code": 4000, "message": "invalid asset"}
                    })),
                    _ => Some(json!({
                        "cmd": "SET_ACTIVITY", "evt": null, "nonce": nonce, "data": activity
                    })),
                };
                if let Some(reply) = reply
                    && !send(&mut stream, Opcode::Frame, &reply).await
                {
                    return;
                }
            }
            injected = inject.recv() => match injected {
                Ok(Inject::Ping) => {
                    if !send(&mut stream, Opcode::Ping, &json!({"probe": id})).await {
                        return;
                    }
                }
                Ok(Inject::DropAll) | Err(_) => return,
            }
        }
    }
}
