//! Rust mirror of `proto/signaling.ts`.
//!
//! These two definitions must agree byte for byte, and nothing in the type
//! system enforces that across languages. `tests/interop.rs` runs the real Node
//! server against this client to catch drift, because a silent field-name
//! mismatch here surfaces as "the camera never connects" with no error anywhere.

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;
pub const CODE_LENGTH: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Receiver,
    Sender,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientInfo {
    pub name: String,
    pub platform: String,
    pub app_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerInfo {
    pub peer_id: String,
    pub role: Role,
    pub name: String,
    pub platform: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IceServer {
    pub urls: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SignalPayload {
    #[serde(rename = "offer")]
    Offer { sdp: String },
    #[serde(rename = "answer")]
    Answer { sdp: String },
    #[serde(rename = "candidate")]
    Candidate {
        candidate: String,
        #[serde(rename = "sdpMid")]
        sdp_mid: Option<String>,
        #[serde(rename = "sdpMLineIndex")]
        sdp_mline_index: Option<u16>,
    },
    #[serde(rename = "candidate-end")]
    CandidateEnd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    BadMessage,
    BadProtocolVersion,
    UnexpectedMessage,
    TooManyConnections,
    TooManySessions,
    InvalidResumeToken,
    InvalidCode,
    CodeExpired,
    SessionFull,
    RateLimited,
    NoPeer,
    Internal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LeaveReason {
    Bye,
    Timeout,
    Transport,
}

/* ------------------------------------------------------------------ */
/* Client -> Server                                                    */
/* ------------------------------------------------------------------ */

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t")]
pub enum ClientMessage {
    #[serde(rename = "hello")]
    Hello {
        protocol: u32,
        role: Role,
        client: ClientInfo,
    },
    #[serde(rename = "session.create")]
    SessionCreate,
    #[serde(rename = "session.join")]
    SessionJoin { code: String },
    /// Reclaims a slot after a dropped connection, so a phone that loses
    /// signal returns to the same input instead of forcing a re-pair.
    #[serde(rename = "session.resume")]
    SessionResume { token: String },
    #[serde(rename = "signal")]
    Signal { payload: SignalPayload },
    #[serde(rename = "bye")]
    Bye,
    #[serde(rename = "ping")]
    Ping { n: u64 },
}

/* ------------------------------------------------------------------ */
/* Server -> Client                                                    */
/* ------------------------------------------------------------------ */

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t")]
pub enum ServerMessage {
    #[serde(rename = "hello.ok")]
    #[serde(rename_all = "camelCase")]
    HelloOk {
        peer_id: String,
        ice_servers: Vec<IceServer>,
        keep_alive_ms: u64,
    },
    #[serde(rename = "session.created")]
    #[serde(rename_all = "camelCase")]
    SessionCreated {
        session_id: String,
        code: String,
        pair_url: String,
        expires_at: i64,
        resume_token: String,
    },
    #[serde(rename = "session.joined")]
    #[serde(rename_all = "camelCase")]
    SessionJoined {
        session_id: String,
        peer: PeerInfo,
        resume_token: String,
    },
    #[serde(rename = "session.resumed")]
    #[serde(rename_all = "camelCase")]
    SessionResumed { session_id: String, peer: PeerInfo },
    #[serde(rename = "peer.rejoined")]
    PeerRejoined { peer: PeerInfo },
    #[serde(rename = "peer.joined")]
    PeerJoined { peer: PeerInfo },
    #[serde(rename = "peer.left")]
    #[serde(rename_all = "camelCase")]
    PeerLeft {
        peer_id: String,
        reason: LeaveReason,
        /// While true the slot is still reserved: show "reconnecting", not
        /// "disconnected". A second PeerLeft with `resumable: false` arrives
        /// if the deadline passes without the camera returning.
        resumable: bool,
        resume_deadline: Option<i64>,
    },
    #[serde(rename = "signal")]
    Signal { from: String, payload: SignalPayload },
    #[serde(rename = "error")]
    Error {
        code: ErrorCode,
        message: String,
        fatal: bool,
    },
    #[serde(rename = "pong")]
    Pong { n: u64 },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Each assertion here is a field name the Node server reads or writes.
    /// If serde's casing ever disagrees with the TypeScript, this fails loudly
    /// at `cargo test` instead of quietly at 3am during a live show.
    #[test]
    fn client_messages_match_the_typescript_wire_format() {
        let hello = ClientMessage::Hello {
            protocol: PROTOCOL_VERSION,
            role: Role::Receiver,
            client: ClientInfo {
                name: "Studio PC".into(),
                platform: "win32".into(),
                app_version: "0.1.0".into(),
            },
        };
        assert_eq!(
            serde_json::to_value(&hello).unwrap(),
            json!({
                "t": "hello",
                "protocol": 1,
                "role": "receiver",
                "client": { "name": "Studio PC", "platform": "win32", "appVersion": "0.1.0" }
            })
        );

        // A unit variant must serialise to the bare tag, not to `{"t":..,"..":null}`.
        assert_eq!(
            serde_json::to_value(ClientMessage::SessionCreate).unwrap(),
            json!({ "t": "session.create" })
        );
        assert_eq!(
            serde_json::to_value(ClientMessage::Bye).unwrap(),
            json!({ "t": "bye" })
        );

        assert_eq!(
            serde_json::to_value(ClientMessage::SessionJoin { code: "483920".into() }).unwrap(),
            json!({ "t": "session.join", "code": "483920" })
        );

        assert_eq!(
            serde_json::to_value(ClientMessage::SessionResume { token: "tok-123".into() }).unwrap(),
            json!({ "t": "session.resume", "token": "tok-123" })
        );
    }

    #[test]
    fn candidate_keeps_the_webrtc_field_names_exactly() {
        // sdpMid and sdpMLineIndex are WebRTC's spelling. Anything else is
        // silently dropped by the browser sender.
        let value = serde_json::to_value(ClientMessage::Signal {
            payload: SignalPayload::Candidate {
                candidate: "candidate:1 1 udp 2130706431 10.0.0.1 54321 typ host".into(),
                sdp_mid: Some("0".into()),
                sdp_mline_index: Some(0),
            },
        })
        .unwrap();

        assert_eq!(
            value,
            json!({
                "t": "signal",
                "payload": {
                    "kind": "candidate",
                    "candidate": "candidate:1 1 udp 2130706431 10.0.0.1 54321 typ host",
                    "sdpMid": "0",
                    "sdpMLineIndex": 0
                }
            })
        );
    }

    #[test]
    fn null_candidate_fields_survive_the_round_trip() {
        // The browser genuinely sends null for these on some candidates, so
        // they must deserialise rather than error.
        let raw = json!({
            "t": "signal",
            "from": "peer-1",
            "payload": { "kind": "candidate", "candidate": "x", "sdpMid": null, "sdpMLineIndex": null }
        });
        let parsed: ServerMessage = serde_json::from_value(raw).unwrap();
        match parsed {
            ServerMessage::Signal { payload: SignalPayload::Candidate { sdp_mid, sdp_mline_index, .. }, .. } => {
                assert!(sdp_mid.is_none());
                assert!(sdp_mline_index.is_none());
            }
            other => panic!("expected a candidate, got {other:?}"),
        }
    }

    #[test]
    fn server_messages_parse_from_the_typescript_wire_format() {
        let hello_ok: ServerMessage = serde_json::from_value(json!({
            "t": "hello.ok",
            "peerId": "abc",
            "iceServers": [{ "urls": ["turn:t:3478"], "username": "1:abc", "credential": "sig" }],
            "keepAliveMs": 15000
        }))
        .unwrap();
        match hello_ok {
            ServerMessage::HelloOk { peer_id, ice_servers, keep_alive_ms } => {
                assert_eq!(peer_id, "abc");
                assert_eq!(keep_alive_ms, 15000);
                assert_eq!(ice_servers[0].username.as_deref(), Some("1:abc"));
            }
            other => panic!("expected hello.ok, got {other:?}"),
        }

        let created: ServerMessage = serde_json::from_value(json!({
            "t": "session.created",
            "sessionId": "s1", "code": "483920",
            "pairUrl": "https://link.test/pair?c=483920",
            "expiresAt": 1_760_000_000_000i64,
            "resumeToken": "Zm9vYmFy"
        }))
        .unwrap();
        match created {
            ServerMessage::SessionCreated { resume_token, .. } => {
                assert_eq!(resume_token, "Zm9vYmFy");
            }
            other => panic!("expected session.created, got {other:?}"),
        }

        // Error codes are snake_case on the wire.
        let err: ServerMessage = serde_json::from_value(json!({
            "t": "error", "code": "bad_protocol_version", "message": "nope", "fatal": true
        }))
        .unwrap();
        match err {
            ServerMessage::Error { code, fatal, .. } => {
                assert_eq!(code, ErrorCode::BadProtocolVersion);
                assert!(fatal);
            }
            other => panic!("expected error, got {other:?}"),
        }

        let left: ServerMessage = serde_json::from_value(json!({
            "t": "peer.left", "peerId": "p1", "reason": "transport",
            "resumable": true, "resumeDeadline": 1_760_000_000_000i64
        }))
        .unwrap();
        match left {
            ServerMessage::PeerLeft { reason, resumable, resume_deadline, .. } => {
                assert_eq!(reason, LeaveReason::Transport);
                assert!(resumable, "a dropped camera is resumable");
                assert!(resume_deadline.is_some());
            }
            other => panic!("expected peer.left, got {other:?}"),
        }

        // The server emits a null deadline once the reservation has lapsed.
        let gone: ServerMessage = serde_json::from_value(json!({
            "t": "peer.left", "peerId": "", "reason": "timeout",
            "resumable": false, "resumeDeadline": null
        }))
        .unwrap();
        assert!(matches!(gone, ServerMessage::PeerLeft { resumable: false, .. }));
    }
}
