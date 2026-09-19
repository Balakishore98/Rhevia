/**
 * RheviaLink signaling protocol, version 1.
 *
 * Shared by the signaling server, the desktop receiver and the mobile sender.
 * The server is a dumb broker: it pairs two peers by short code and relays
 * opaque WebRTC signaling between them. It never sees media, and once the peer
 * connection is up, tally and camera-control traffic moves to a WebRTC data
 * channel so it does not depend on this server staying reachable.
 */

export const PROTOCOL_VERSION = 1;

/** The desktop receives media; the sender (phone or browser) produces it. */
export type Role = "receiver" | "sender";

export interface ClientInfo {
  /** Display name shown to the other peer, e.g. "Pixel 8 Pro" or "Studio PC". */
  name: string;
  /** Informational only, for diagnostics and support. */
  platform: string;
  appVersion: string;
}

export interface PeerInfo {
  peerId: string;
  role: Role;
  name: string;
  platform: string;
}

export interface IceServer {
  urls: string[];
  username?: string;
  credential?: string;
}

/**
 * WebRTC signaling payloads are relayed verbatim. The server does not parse
 * SDP; keeping it opaque means protocol changes on the endpoints do not require
 * a server deploy.
 */
export type SignalPayload =
  | { kind: "offer"; sdp: string }
  | { kind: "answer"; sdp: string }
  | { kind: "candidate"; candidate: string; sdpMid: string | null; sdpMLineIndex: number | null }
  | { kind: "candidate-end" };

export type ErrorCode =
  | "bad_message"
  | "too_many_connections"
  | "too_many_sessions"
  | "invalid_resume_token"
  | "bad_protocol_version"
  | "unexpected_message"
  | "invalid_code"
  | "code_expired"
  | "session_full"
  | "rate_limited"
  | "no_peer"
  | "internal";

/* ------------------------------------------------------------------ */
/* Client -> Server                                                    */
/* ------------------------------------------------------------------ */

export type ClientMessage =
  /** Must be the first message on every connection. */
  | { t: "hello"; protocol: number; role: Role; client: ClientInfo }
  /** Receiver asks for a pairing code to display as a QR and as digits. */
  | { t: "session.create" }
  /** Sender redeems a pairing code. */
  | { t: "session.join"; code: string }
  /**
   * Reclaims a slot in an existing session after a dropped connection.
   * A live show must survive a phone losing signal for a few seconds without
   * the operator re-pairing, so a disconnected peer keeps its slot reserved
   * for `resumeGraceMs` and comes back with the token it was issued.
   */
  | { t: "session.resume"; token: string }
  /** Relayed verbatim to the paired peer. */
  | { t: "signal"; payload: SignalPayload }
  /** Graceful disconnect, so the peer learns immediately rather than on timeout. */
  | { t: "bye" }
  | { t: "ping"; n: number };

/* ------------------------------------------------------------------ */
/* Server -> Client                                                    */
/* ------------------------------------------------------------------ */

export type ServerMessage =
  | { t: "hello.ok"; peerId: string; iceServers: IceServer[]; keepAliveMs: number }
  | {
      t: "session.created";
      sessionId: string;
      /** Six digits, displayed grouped as "483 920". */
      code: string;
      /** Encoded into the QR the desktop shows. */
      pairUrl: string;
      expiresAt: number;
      /** Secret. Presented to reclaim this slot after a dropped connection. */
      resumeToken: string;
    }
  /** To the sender, once its code is accepted. */
  | { t: "session.joined"; sessionId: string; peer: PeerInfo; resumeToken: string }
  /** To a peer that successfully reclaimed its slot. */
  | { t: "session.resumed"; sessionId: string; peer: PeerInfo }
  /** To the peer that stayed, when its partner reconnects. */
  | { t: "peer.rejoined"; peer: PeerInfo }
  /** To the receiver, when a sender redeems its code. */
  | { t: "peer.joined"; peer: PeerInfo }
  | {
      t: "peer.left";
      peerId: string;
      reason: "bye" | "timeout" | "transport";
      /**
       * True while the slot is still reserved. The UI should show
       * "reconnecting" rather than "disconnected" until the deadline passes,
       * at which point a second peer.left arrives with resumable false.
       */
      resumable: boolean;
      resumeDeadline: number | null;
    }
  | { t: "signal"; from: string; payload: SignalPayload }
  | { t: "error"; code: ErrorCode; message: string; fatal: boolean }
  | { t: "pong"; n: number };

/* ------------------------------------------------------------------ */
/* Limits                                                              */
/* ------------------------------------------------------------------ */

export const LIMITS = {
  /** SDP for a multi-track offer is a few KB; 64 KB is generous and bounded. */
  maxMessageBytes: 64 * 1024,
  /** A pairing code is useless after this and is purged. */
  sessionTtlMs: 5 * 60 * 1000,
  /** Server pings this often; a peer that misses two is dropped. */
  keepAliveMs: 15_000,
  /** A connection that does not say hello promptly is closed. */
  helloTimeoutMs: 10_000,
  /** Per-IP failed `session.join` attempts before a cooldown. */
  maxJoinFailures: 10,
  joinFailureWindowMs: 60_000,
  /**
   * How long a dropped peer keeps its slot. Long enough to cross a lift, a
   * cell handover or a Wi-Fi roam; short enough that abandoned sessions do not
   * accumulate.
   */
  resumeGraceMs: 45_000,
  /**
   * Resource caps. Without these one client can open unbounded sockets and
   * hold a large share of the pairing keyspace — both were verified against
   * the running server before these limits existed.
   */
  maxConnectionsPerIp: 20,
  maxTotalConnections: 5_000,
  maxSessionsPerIp: 5,
} as const;

/** Codes are digits only so they can be typed on a phone keypad. */
export const CODE_LENGTH = 6;
