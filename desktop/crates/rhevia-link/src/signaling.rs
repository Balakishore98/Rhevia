//! WebSocket client for the RheviaLink signaling server.
//!
//! Handles the pairing handshake and relays WebRTC negotiation. It deliberately
//! does not own the peer connection: signaling is only needed to establish the
//! link, and once media is flowing a signaling outage must not disturb a live
//! camera. Keeping the two separate makes that property structural.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use crate::protocol::{
    ClientInfo, ClientMessage, IceServer, LeaveReason, PeerInfo, Role, ServerMessage, SignalPayload,
    PROTOCOL_VERSION,
};

#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    #[error("could not reach the signaling server: {0}")]
    Connect(#[source] tokio_tungstenite::tungstenite::Error),
    #[error("signaling transport failed: {0}")]
    Transport(#[source] tokio_tungstenite::tungstenite::Error),
    #[error("signaling server sent something we could not parse: {0}")]
    Decode(#[source] serde_json::Error),
    #[error("signaling server rejected us: {code:?}: {message}")]
    Rejected {
        code: crate::protocol::ErrorCode,
        message: String,
    },
    #[error("signaling server closed the connection")]
    Closed,
    #[error("timed out waiting for {0}")]
    Timeout(&'static str),
}

/// Everything the caller needs to react to. Deliberately flat: the desktop UI
/// maps these straight onto what it shows the operator.
#[derive(Debug, Clone)]
pub enum LinkEvent {
    /// Pairing code is ready. Show it, and encode `pair_url` as a QR.
    CodeReady {
        code: String,
        pair_url: String,
        expires_at: i64,
    },
    /// A camera redeemed the code. WebRTC negotiation starts now.
    CameraPaired(PeerInfo),
    /// WebRTC negotiation traffic from the camera.
    Signal(SignalPayload),
    /// The camera dropped but its slot is still reserved until `deadline`.
    /// Show "reconnecting" — do **not** tear down the input yet.
    CameraReconnecting { reason: LeaveReason, deadline: i64 },
    /// The camera is gone for good: either a deliberate stop, or the resume
    /// window lapsed. Tear the input down and offer a fresh pairing code.
    CameraLeft(LeaveReason),
    /// A dropped camera reclaimed its slot. Renegotiate and carry on.
    CameraRejoined(PeerInfo),
    /// We reclaimed our own slot after reconnecting.
    Resumed(PeerInfo),
    /// Non-fatal server complaint, e.g. the code expired before anyone used it.
    ServerError { message: String },
    /// The signaling connection ended. Media may still be flowing.
    Disconnected,
}

pub struct SignalingClient {
    outgoing: mpsc::UnboundedSender<ClientMessage>,
    // Interior mutability so one Arc can be shared between the task pumping
    // ICE candidates out and the task consuming events, which run concurrently.
    events: tokio::sync::Mutex<mpsc::UnboundedReceiver<LinkEvent>>,
    peer_id: String,
    ice_servers: Vec<IceServer>,
    resume_token: Arc<Mutex<Option<String>>>,
}

impl SignalingClient {
    /// Connects as a receiver and completes the hello exchange.
    pub async fn connect_receiver(url: &str, client: ClientInfo) -> Result<Self, LinkError> {
        Self::connect(url, Role::Receiver, client).await
    }

    /// Connects as a sender. Used by the browser-sender harness and by
    /// integration tests standing in for a phone.
    pub async fn connect_sender(url: &str, client: ClientInfo) -> Result<Self, LinkError> {
        Self::connect(url, Role::Sender, client).await
    }

    async fn connect(url: &str, role: Role, client: ClientInfo) -> Result<Self, LinkError> {
        let (stream, _) = tokio_tungstenite::connect_async(url)
            .await
            .map_err(LinkError::Connect)?;
        let (mut sink, mut source) = stream.split();

        let hello = ClientMessage::Hello {
            protocol: PROTOCOL_VERSION,
            role,
            client,
        };
        sink.send(Message::Text(encode(&hello)))
            .await
            .map_err(LinkError::Transport)?;

        // Block only for the hello ack. Everything after this is event-driven,
        // so a slow server delays startup but never stalls the UI thread.
        let (peer_id, ice_servers) =
            match tokio::time::timeout(Duration::from_secs(10), expect_hello_ok(&mut source)).await
            {
                Err(_) => return Err(LinkError::Timeout("hello.ok")),
                Ok(result) => result?,
            };

        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<ClientMessage>();
        let (evt_tx, evt_rx) = mpsc::unbounded_channel::<LinkEvent>();
        let resume_token = Arc::new(Mutex::new(None::<String>));

        // Writer: owns the sink so sends from anywhere are serialised without a lock.
        tokio::spawn(async move {
            while let Some(msg) = out_rx.recv().await {
                if sink.send(Message::Text(encode(&msg))).await.is_err() {
                    break;
                }
            }
            let _ = sink.close().await;
        });

        // Reader: translates the wire protocol into LinkEvents.
        let evt_for_reader = evt_tx.clone();
        let token_for_reader = Arc::clone(&resume_token);
        tokio::spawn(async move {
            while let Some(frame) = source.next().await {
                let text = match frame {
                    Ok(Message::Text(t)) => t,
                    // Pings are answered by tungstenite automatically; binary
                    // frames are not part of this protocol and are ignored
                    // rather than treated as a fault.
                    Ok(Message::Close(_)) | Err(_) => break,
                    Ok(_) => continue,
                };

                let Ok(msg) = serde_json::from_str::<ServerMessage>(&text) else {
                    tracing::warn!(%text, "unparseable signaling message, ignoring");
                    continue;
                };

                let event = match msg {
                    ServerMessage::SessionCreated {
                        code,
                        pair_url,
                        expires_at,
                        resume_token,
                        ..
                    } => {
                        *token_for_reader.lock().unwrap() = Some(resume_token);
                        LinkEvent::CodeReady {
                            code,
                            pair_url,
                            expires_at,
                        }
                    }
                    ServerMessage::SessionJoined {
                        peer, resume_token, ..
                    } => {
                        *token_for_reader.lock().unwrap() = Some(resume_token);
                        LinkEvent::CameraPaired(peer)
                    }
                    ServerMessage::PeerJoined { peer } => LinkEvent::CameraPaired(peer),
                    ServerMessage::SessionResumed { peer, .. } => LinkEvent::Resumed(peer),
                    ServerMessage::PeerRejoined { peer } => LinkEvent::CameraRejoined(peer),
                    ServerMessage::Signal { payload, .. } => LinkEvent::Signal(payload),
                    // The distinction matters: a reserved slot means hold the
                    // input open, a lapsed one means tear it down.
                    ServerMessage::PeerLeft {
                        reason,
                        resumable,
                        resume_deadline,
                        ..
                    } => match (resumable, resume_deadline) {
                        (true, Some(deadline)) => LinkEvent::CameraReconnecting { reason, deadline },
                        _ => LinkEvent::CameraLeft(reason),
                    },
                    ServerMessage::Error { message, .. } => LinkEvent::ServerError { message },
                    // A second hello.ok or a pong needs no caller action.
                    ServerMessage::HelloOk { .. } | ServerMessage::Pong { .. } => continue,
                };

                if evt_for_reader.send(event).is_err() {
                    break; // caller dropped the client
                }
            }
            let _ = evt_tx.send(LinkEvent::Disconnected);
        });

        Ok(Self {
            outgoing: out_tx,
            events: tokio::sync::Mutex::new(evt_rx),
            peer_id,
            ice_servers,
            resume_token,
        })
    }

    /// Our peer id, as assigned by the server.
    pub fn peer_id(&self) -> &str {
        &self.peer_id
    }

    /// ICE servers with freshly minted, time-limited TURN credentials.
    pub fn ice_servers(&self) -> &[IceServer] {
        &self.ice_servers
    }

    /// Token for reclaiming this slot after a dropped connection. Persist it:
    /// it is what lets a phone rejoin the same input instead of re-pairing.
    pub fn resume_token(&self) -> Option<String> {
        self.resume_token.lock().unwrap().clone()
    }

    /// Asks for a pairing code. The code arrives as `LinkEvent::CodeReady`.
    pub fn request_code(&self) -> Result<(), LinkError> {
        self.send(ClientMessage::SessionCreate)
    }

    /// Redeems a pairing code, for a sender.
    pub fn join(&self, code: &str) -> Result<(), LinkError> {
        self.send(ClientMessage::SessionJoin {
            code: code.to_string(),
        })
    }

    /// Reclaims a previously held slot on a fresh connection.
    pub fn resume(&self, token: &str) -> Result<(), LinkError> {
        self.send(ClientMessage::SessionResume {
            token: token.to_string(),
        })
    }

    /// Relays WebRTC negotiation to the paired peer.
    pub fn signal(&self, payload: SignalPayload) -> Result<(), LinkError> {
        self.send(ClientMessage::Signal { payload })
    }

    /// Leaves cleanly, so the peer is told immediately instead of waiting for a
    /// keep-alive timeout. This forfeits the resume reservation by design.
    pub fn bye(&self) -> Result<(), LinkError> {
        self.send(ClientMessage::Bye)
    }

    /// Next event, or `None` once signaling has ended.
    pub async fn next_event(&self) -> Option<LinkEvent> {
        self.events.lock().await.recv().await
    }

    fn send(&self, msg: ClientMessage) -> Result<(), LinkError> {
        self.outgoing.send(msg).map_err(|_| LinkError::Closed)
    }
}

async fn expect_hello_ok<S>(source: &mut S) -> Result<(String, Vec<IceServer>), LinkError>
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    while let Some(frame) = source.next().await {
        let text = match frame.map_err(LinkError::Transport)? {
            Message::Text(t) => t,
            Message::Close(_) => return Err(LinkError::Closed),
            _ => continue,
        };
        match serde_json::from_str::<ServerMessage>(&text).map_err(LinkError::Decode)? {
            ServerMessage::HelloOk {
                peer_id,
                ice_servers,
                ..
            } => return Ok((peer_id, ice_servers)),
            // A rejected hello is fatal and must surface as an error, not as a
            // timeout: "wrong protocol version" is actionable, "timed out" is not.
            ServerMessage::Error { code, message, .. } => {
                return Err(LinkError::Rejected { code, message })
            }
            _ => continue,
        }
    }
    Err(LinkError::Closed)
}

fn encode(msg: &ClientMessage) -> String {
    // These are closed enums built in this crate, so serialisation cannot fail
    // for any input we can construct.
    serde_json::to_string(msg).expect("ClientMessage is always serialisable")
}
