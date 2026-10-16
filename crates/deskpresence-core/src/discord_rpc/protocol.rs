//! Discord RPC-over-IPC wire format.
//!
//! Reference: <https://discord.com/developers/docs/topics/rpc> ("RPC over IPC").
//! Every message is a frame: a little-endian `u32` opcode, a little-endian
//! `u32` payload length, then a UTF-8 JSON payload of that length.
//!
//! | Opcode | Name      | Use                                   |
//! |--------|-----------|---------------------------------------|
//! | 0      | HANDSHAKE | client → Discord, `{"v":1,"client_id"}` |
//! | 1      | FRAME     | commands, responses and events        |
//! | 2      | CLOSE     | either side closes the connection     |
//! | 3      | PING      | keep-alive probe                      |
//! | 4      | PONG      | reply to PING (echoes the payload)    |

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::RpcError;
use super::activity::Activity;

/// RPC version sent in the handshake.
pub const RPC_VERSION: u32 = 1;
/// Upper bound for a single frame payload. Discord's messages are a few KiB;
/// anything larger indicates a corrupt stream and is rejected without allocating.
pub const MAX_FRAME_LEN: u32 = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Opcode {
    Handshake = 0,
    Frame = 1,
    Close = 2,
    Ping = 3,
    Pong = 4,
}

impl Opcode {
    pub fn from_u32(value: u32) -> Option<Self> {
        match value {
            0 => Some(Opcode::Handshake),
            1 => Some(Opcode::Frame),
            2 => Some(Opcode::Close),
            3 => Some(Opcode::Ping),
            4 => Some(Opcode::Pong),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub opcode: Opcode,
    pub payload: Vec<u8>,
}

/// Encodes a frame into bytes.
pub fn encode_frame(opcode: Opcode, payload: &[u8]) -> Result<Vec<u8>, RpcError> {
    let len = u32::try_from(payload.len())
        .ok()
        .filter(|&len| len <= MAX_FRAME_LEN)
        .ok_or(RpcError::FrameTooLarge(u32::MAX))?;
    let mut out = Vec::with_capacity(8 + payload.len());
    out.extend_from_slice(&(opcode as u32).to_le_bytes());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

/// Parses an 8-byte frame header.
pub fn decode_header(header: [u8; 8]) -> Result<(Opcode, u32), RpcError> {
    let raw_opcode = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
    let len = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
    let opcode = Opcode::from_u32(raw_opcode).ok_or(RpcError::UnknownOpcode(raw_opcode))?;
    if len > MAX_FRAME_LEN {
        return Err(RpcError::FrameTooLarge(len));
    }
    Ok((opcode, len))
}

/// Reads one complete frame. An orderly end-of-stream before a header
/// yields [`RpcError::Eof`].
pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Frame, RpcError> {
    let mut header = [0u8; 8];
    match reader.read_exact(&mut header).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Err(RpcError::Eof),
        Err(e) => return Err(RpcError::Io(e)),
    }
    let (opcode, len) = decode_header(header)?;
    let mut payload = vec![0u8; len as usize];
    reader.read_exact(&mut payload).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            RpcError::Protocol("stream ended in the middle of a frame".into())
        } else {
            RpcError::Io(e)
        }
    })?;
    Ok(Frame { opcode, payload })
}

pub async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    opcode: Opcode,
    payload: &[u8],
) -> Result<(), RpcError> {
    let bytes = encode_frame(opcode, payload)?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

#[derive(Serialize)]
pub struct Handshake<'a> {
    pub v: u32,
    pub client_id: &'a str,
}

#[derive(Serialize)]
pub struct SetActivityArgs<'a> {
    pub pid: u32,
    /// Omitted to clear the activity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity: Option<&'a Activity>,
}

#[derive(Serialize)]
pub struct Command<'a, A: Serialize> {
    pub cmd: &'a str,
    pub args: A,
    pub nonce: &'a str,
}

/// Builds the JSON for `SET_ACTIVITY`. `None` clears the activity.
pub fn set_activity_payload(pid: u32, activity: Option<&Activity>, nonce: &str) -> Result<Vec<u8>, RpcError> {
    serde_json::to_vec(&Command {
        cmd: "SET_ACTIVITY",
        args: SetActivityArgs { pid, activity },
        nonce,
    })
    .map_err(|e| RpcError::Protocol(e.to_string()))
}

pub fn handshake_payload(client_id: &str) -> Result<Vec<u8>, RpcError> {
    serde_json::to_vec(&Handshake {
        v: RPC_VERSION,
        client_id,
    })
    .map_err(|e| RpcError::Protocol(e.to_string()))
}

/// Any message received in a FRAME.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Incoming {
    #[serde(default)]
    pub cmd: Option<String>,
    #[serde(default)]
    pub evt: Option<String>,
    #[serde(default)]
    pub nonce: Option<String>,
    #[serde(default)]
    pub data: Option<serde_json::Value>,
}

impl Incoming {
    pub fn is_error(&self) -> bool {
        self.evt.as_deref() == Some("ERROR")
    }

    pub fn is_ready(&self) -> bool {
        self.cmd.as_deref() == Some("DISPATCH") && self.evt.as_deref() == Some("READY")
    }

    /// `(code, message)` from an ERROR payload.
    pub fn error_details(&self) -> (Option<i64>, String) {
        let data = self.data.as_ref();
        let code = data
            .and_then(|d| d.get("code"))
            .and_then(serde_json::Value::as_i64);
        let message = data
            .and_then(|d| d.get("message"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown error")
            .to_owned();
        (code, message)
    }

    /// Display name of the connected user from a READY dispatch.
    pub fn ready_user_name(&self) -> Option<String> {
        let user = self.data.as_ref()?.get("user")?;
        user.get("global_name")
            .and_then(serde_json::Value::as_str)
            .or_else(|| user.get("username").and_then(serde_json::Value::as_str))
            .map(str::to_owned)
    }
}

/// Payload of a CLOSE frame.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CloseReason {
    #[serde(default)]
    pub code: Option<i64>,
    #[serde(default)]
    pub message: Option<String>,
}

pub fn parse_incoming(payload: &[u8]) -> Result<Incoming, RpcError> {
    serde_json::from_slice(payload).map_err(|e| RpcError::Protocol(format!("invalid JSON from Discord: {e}")))
}

/// CLOSE payloads are parsed leniently: a malformed reason still closes.
pub fn parse_close(payload: &[u8]) -> CloseReason {
    serde_json::from_slice(payload).unwrap_or_default()
}

/// Random 128-bit hex nonce for matching responses to commands.
pub fn new_nonce() -> String {
    format!("{:016x}{:016x}", fastrand::u64(..), fastrand::u64(..))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discord_rpc::activity::{ActivityAssets, ActivityTimestamps};

    #[tokio::test]
    async fn frame_round_trip() {
        let bytes = encode_frame(Opcode::Frame, br#"{"cmd":"X"}"#).unwrap();
        assert_eq!(&bytes[..4], &[1, 0, 0, 0]);
        assert_eq!(&bytes[4..8], &[11, 0, 0, 0]);
        let mut reader = bytes.as_slice();
        let frame = read_frame(&mut reader).await.unwrap();
        assert_eq!(frame.opcode, Opcode::Frame);
        assert_eq!(frame.payload, br#"{"cmd":"X"}"#);
    }

    #[test]
    fn handshake_matches_documented_example() {
        let payload = handshake_payload("123456789012345678").unwrap();
        assert_eq!(payload, br#"{"v":1,"client_id":"123456789012345678"}"#);
        let frame = encode_frame(Opcode::Handshake, &payload).unwrap();
        // Opcode 0, then the payload length. (The docs' example annotates this
        // payload as length 45, but it is 40 bytes; the header must carry the
        // real byte length.)
        assert_eq!(&frame[..8], &[0, 0, 0, 0, 40, 0, 0, 0]);
    }

    #[tokio::test]
    async fn multiple_frames_in_one_stream() {
        let mut bytes = encode_frame(Opcode::Ping, b"{}").unwrap();
        bytes.extend(encode_frame(Opcode::Close, br#"{"code":4000}"#).unwrap());
        let mut reader = bytes.as_slice();
        assert_eq!(read_frame(&mut reader).await.unwrap().opcode, Opcode::Ping);
        let close = read_frame(&mut reader).await.unwrap();
        assert_eq!(close.opcode, Opcode::Close);
        assert_eq!(parse_close(&close.payload).code, Some(4000));
        assert!(matches!(read_frame(&mut reader).await, Err(RpcError::Eof)));
    }

    #[tokio::test]
    async fn rejects_unknown_opcode() {
        let bytes = [9u8, 0, 0, 0, 0, 0, 0, 0];
        let mut reader = bytes.as_slice();
        assert!(matches!(
            read_frame(&mut reader).await,
            Err(RpcError::UnknownOpcode(9))
        ));
    }

    #[tokio::test]
    async fn rejects_oversized_length_without_allocating() {
        let mut bytes = vec![1, 0, 0, 0];
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        let mut reader = bytes.as_slice();
        assert!(matches!(
            read_frame(&mut reader).await,
            Err(RpcError::FrameTooLarge(u32::MAX))
        ));
    }

    #[tokio::test]
    async fn truncated_frames_are_protocol_errors() {
        let bytes = encode_frame(Opcode::Frame, b"{\"a\":1}").unwrap();
        let mut reader = &bytes[..bytes.len() - 2];
        assert!(matches!(
            read_frame(&mut reader).await,
            Err(RpcError::Protocol(_))
        ));
        let mut header_only = &bytes[..5];
        assert!(matches!(read_frame(&mut header_only).await, Err(RpcError::Eof)));
    }

    #[test]
    fn malformed_json_is_a_protocol_error() {
        assert!(matches!(parse_incoming(b"{not json"), Err(RpcError::Protocol(_))));
        assert!(matches!(
            parse_incoming(&[0xff, 0xfe]),
            Err(RpcError::Protocol(_))
        ));
        // Valid JSON of the wrong shape is tolerated as an empty message.
        let msg = parse_incoming(b"{\"unexpected\":true}").unwrap();
        assert!(msg.cmd.is_none() && !msg.is_ready());
        assert!(parse_incoming(b"[1,2]").is_err());
        assert_eq!(parse_close(b"garbage").code, None);
    }

    #[test]
    fn parses_ready_and_errors() {
        let ready = parse_incoming(
            br#"{"cmd":"DISPATCH","evt":"READY","data":{"v":1,"user":{"id":"1","username":"mason","global_name":"Mason"}}}"#,
        )
        .unwrap();
        assert!(ready.is_ready());
        assert_eq!(ready.ready_user_name().as_deref(), Some("Mason"));

        let error = parse_incoming(
            br#"{"cmd":"SET_ACTIVITY","evt":"ERROR","nonce":"n","data":{"code":4000,"message":"bad"}}"#,
        )
        .unwrap();
        assert!(error.is_error());
        assert_eq!(error.error_details(), (Some(4000), "bad".to_owned()));
    }

    #[test]
    fn set_activity_payload_shape() {
        let activity = Activity {
            activity_type: 0,
            name: None,
            details: Some("Using Zed".into()),
            state: None,
            status_display_type: None,
            timestamps: Some(ActivityTimestamps {
                start: Some(1_700_000_000),
            }),
            assets: Some(ActivityAssets {
                large_image: Some("zed".into()),
                ..ActivityAssets::default()
            }),
        };
        let json: serde_json::Value =
            serde_json::from_slice(&set_activity_payload(42, Some(&activity), "abc").unwrap()).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "cmd": "SET_ACTIVITY",
                "args": {
                    "pid": 42,
                    "activity": {
                        "type": 0,
                        "details": "Using Zed",
                        "timestamps": { "start": 1_700_000_000u64 },
                        "assets": { "large_image": "zed" }
                    }
                },
                "nonce": "abc"
            })
        );

        let clear: serde_json::Value =
            serde_json::from_slice(&set_activity_payload(42, None, "n").unwrap()).unwrap();
        assert_eq!(clear["args"], serde_json::json!({ "pid": 42 }));
    }

    #[test]
    fn nonces_are_unique() {
        assert_ne!(new_nonce(), new_nonce());
        assert_eq!(new_nonce().len(), 32);
    }
}
