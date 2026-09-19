import { createServer, type IncomingMessage, type Server } from "node:http";
import { randomUUID } from "node:crypto";
import { WebSocketServer, type WebSocket } from "ws";
import {
  LIMITS,
  PROTOCOL_VERSION,
  type ClientMessage,
  type ErrorCode,
  type ServerMessage,
} from "@rhevia/proto";
import { loadConfig, mintIceServers, type Config } from "./config.js";
import { JoinThrottle, SessionRegistry, peerInfo, type Peer } from "./sessions.js";
import { parseClientMessage } from "./validate.js";

interface Conn extends Peer {
  socket: WebSocket;
  ip: string;
  greeted: boolean;
}

export interface SignalingServer {
  http: Server;
  close(): Promise<void>;
  readonly sessionCount: number;
  readonly connectionCount: number;
}

export function createSignalingServer(config: Config = loadConfig()): SignalingServer {
  const sessions = new SessionRegistry();
  const throttle = new JoinThrottle();
  const conns = new Set<Conn>();
  const connsByIp = new Map<string, number>();

  const http = createServer((req, res) => {
    if (req.url === "/healthz") {
      res.writeHead(200, { "content-type": "application/json" });
      res.end(JSON.stringify({ ok: true, sessions: sessions.size, connections: conns.size }));
      return;
    }
    res.writeHead(404).end();
  });

  const wss = new WebSocketServer({
    server: http,
    maxPayload: LIMITS.maxMessageBytes,
    verifyClient: (info, done) => {
      // A browser sender is a first-class client, so Origin has to be checked
      // here rather than relying on the same-origin policy.
      if (config.allowedOrigins.length > 0 && info.origin) {
        if (!config.allowedOrigins.includes(info.origin)) {
          return done(false, 403, "origin not allowed");
        }
      }
      // Reject before the upgrade completes: an unbounded socket count from a
      // single source was verified to be exploitable before this existed.
      if (conns.size >= LIMITS.maxTotalConnections) {
        return done(false, 503, "server at capacity");
      }
      const ip = clientIp(info.req);
      if ((connsByIp.get(ip) ?? 0) >= LIMITS.maxConnectionsPerIp) {
        return done(false, 429, "too many connections from this address");
      }
      done(true);
    },
  });

  function send(conn: Conn, msg: ServerMessage): void {
    if (conn.socket.readyState === conn.socket.OPEN) {
      conn.socket.send(JSON.stringify(msg));
    }
  }

  function tell(peer: Peer, msg: ServerMessage): void {
    peer.send(JSON.stringify(msg));
  }

  function fail(conn: Conn, code: ErrorCode, message: string, fatal = false): void {
    send(conn, { t: "error", code, message, fatal });
    if (fatal) conn.socket.close(1008, code);
  }

  /**
   * Handles a peer going away.
   *
   * A paired peer keeps its slot reserved rather than destroying the session:
   * a phone that loses signal for a few seconds must be able to come back to
   * the same input instead of forcing the operator to re-pair mid-show.
   * A deliberate `bye` gets no reservation, because nobody is coming back.
   */
  function leave(conn: Conn, reason: "bye" | "timeout" | "transport"): void {
    const sessionId = conn.sessionId;
    if (!sessionId) return;
    const session = sessions.get(sessionId);
    if (!session) return;

    const paired = session.sender !== null;
    if (reason === "bye" || !paired) {
      const partner = sessions.peerOf(conn);
      sessions.destroy(sessionId);
      if (partner) {
        tell(partner, {
          t: "peer.left",
          peerId: conn.id,
          reason,
          resumable: false,
          resumeDeadline: null,
        });
      }
      return;
    }

    const orphaned = sessions.orphan(conn);
    if (orphaned?.partner) {
      tell(orphaned.partner, {
        t: "peer.left",
        peerId: conn.id,
        reason,
        resumable: true,
        resumeDeadline: orphaned.deadline,
      });
    }
  }

  wss.on("connection", (socket: WebSocket, req: IncomingMessage) => {
    const ip = clientIp(req);

    // Re-check authoritatively: several upgrades can clear verifyClient before
    // any of them registers here.
    if ((connsByIp.get(ip) ?? 0) >= LIMITS.maxConnectionsPerIp) {
      socket.close(1013, "too many connections from this address");
      return;
    }

    const conn: Conn = {
      id: randomUUID(),
      role: "sender", // provisional; the hello sets the real role
      info: { name: "", platform: "", appVersion: "" },
      sessionId: null,
      missedKeepAlives: 0,
      greeted: false,
      socket,
      ip,
      send: (data) => {
        if (socket.readyState === socket.OPEN) socket.send(data);
      },
      close: (code, reason) => socket.close(code, reason),
    };
    conns.add(conn);
    connsByIp.set(ip, (connsByIp.get(ip) ?? 0) + 1);

    // A socket that connects and says nothing is either a scanner or a broken
    // client. Either way it should not hold a slot.
    const helloTimer = setTimeout(() => {
      if (!conn.greeted) fail(conn, "unexpected_message", "no hello", true);
    }, LIMITS.helloTimeoutMs);

    socket.on("message", (raw) => {
      const parsed = parseClientMessage(raw.toString());
      if (!parsed.ok) {
        fail(conn, "bad_message", parsed.error);
        return;
      }
      handle(conn, parsed.value);
    });

    socket.on("pong", () => {
      conn.missedKeepAlives = 0;
    });

    socket.on("close", () => {
      clearTimeout(helloTimer);
      conns.delete(conn);
      const remaining = (connsByIp.get(ip) ?? 1) - 1;
      if (remaining <= 0) connsByIp.delete(ip);
      else connsByIp.set(ip, remaining);
      leave(conn, "transport");
    });

    socket.on("error", () => socket.close());
  });

  function handle(conn: Conn, msg: ClientMessage): void {
    if (msg.t === "hello") {
      if (conn.greeted) return fail(conn, "unexpected_message", "already greeted");
      if (msg.protocol !== PROTOCOL_VERSION) {
        return fail(conn, "bad_protocol_version", `server speaks protocol ${PROTOCOL_VERSION}`, true);
      }
      conn.greeted = true;
      conn.role = msg.role;
      conn.info = msg.client;
      return send(conn, {
        t: "hello.ok",
        peerId: conn.id,
        iceServers: mintIceServers(config, conn.id),
        keepAliveMs: LIMITS.keepAliveMs,
      });
    }

    if (!conn.greeted) return fail(conn, "unexpected_message", "hello must come first", true);

    switch (msg.t) {
      case "ping":
        return send(conn, { t: "pong", n: msg.n });

      case "session.create": {
        if (conn.role !== "receiver") {
          return fail(conn, "unexpected_message", "only a receiver may create a session");
        }
        if (conn.sessionId) return fail(conn, "unexpected_message", "already in a session");
        // Codes come from a 10^6 keyspace; one client hoarding them shrinks it
        // for everyone and is a memory vector besides.
        if (sessions.sessionsFor(conn.ip) >= LIMITS.maxSessionsPerIp) {
          return fail(conn, "too_many_sessions", "too many open pairing codes from this address");
        }

        const session = sessions.create(conn, conn.ip);
        return send(conn, {
          t: "session.created",
          sessionId: session.id,
          code: session.code,
          pairUrl: `${config.pairUrlBase}?c=${session.code}`,
          expiresAt: session.expiresAt as number,
          resumeToken: session.receiver.resumeToken,
        });
      }

      case "session.join": {
        if (conn.role !== "sender") {
          return fail(conn, "unexpected_message", "only a sender may join a session");
        }
        if (conn.sessionId) return fail(conn, "unexpected_message", "already in a session");
        // Not fatal: behind carrier-grade NAT many unrelated users share one
        // address, so closing the socket would punish everyone for one
        // person's typo. Rejecting the attempt is enough to stop the walk.
        if (throttle.isBlocked(conn.ip)) {
          return fail(conn, "rate_limited", "too many failed attempts; wait a minute");
        }

        const result = sessions.join(msg.code, conn);
        if (!result.ok) {
          throttle.recordFailure(conn.ip);
          return fail(conn, result.reason, joinErrorMessage(result.reason));
        }
        throttle.clear(conn.ip);

        const { session, resumeToken } = result;
        const receiver = session.receiver.peer;
        send(conn, {
          t: "session.joined",
          sessionId: session.id,
          peer: receiver ? peerInfo(receiver) : { peerId: "", role: "receiver", name: "", platform: "" },
          resumeToken,
        });
        if (receiver) tell(receiver, { t: "peer.joined", peer: peerInfo(conn) });
        return;
      }

      case "session.resume": {
        if (conn.sessionId) return fail(conn, "unexpected_message", "already in a session");
        // Tokens are 32 random bytes, so unlike pairing codes they are not
        // guessable; the throttle still applies to stop blind hammering.
        if (throttle.isBlocked(conn.ip)) {
          return fail(conn, "rate_limited", "too many failed attempts; wait a minute");
        }

        const result = sessions.resume(msg.token, conn);
        if (!result.ok) {
          throttle.recordFailure(conn.ip);
          return fail(conn, "invalid_resume_token", "that session is no longer available");
        }
        throttle.clear(conn.ip);

        const { session, partner } = result;
        send(conn, {
          t: "session.resumed",
          sessionId: session.id,
          peer: partner ? peerInfo(partner) : { peerId: "", role: conn.role, name: "", platform: "" },
        });
        if (partner) tell(partner, { t: "peer.rejoined", peer: peerInfo(conn) });
        return;
      }

      case "signal": {
        const other = sessions.peerOf(conn);
        // Not an error worth closing over: during a resume window the partner
        // is briefly absent while the caller is still negotiating.
        if (!other) return fail(conn, "no_peer", "not paired");
        tell(other, { t: "signal", from: conn.id, payload: msg.payload });
        return;
      }

      case "bye":
        leave(conn, "bye");
        conn.socket.close(1000, "bye");
        return;
    }
  }

  // Keep-alive and expiry sweep share one timer: both are cheap and neither
  // needs finer resolution than the keep-alive interval.
  const ticker = setInterval(() => {
    const now = Date.now();

    for (const { session, stranded } of sessions.sweep(now)) {
      if (!stranded) continue;
      // The reservation lapsed. Tell whoever is still waiting that the camera
      // is genuinely gone, so the UI stops showing "reconnecting" and the
      // operator can generate a fresh code.
      const wasPaired = session.sender !== null;
      tell(stranded, wasPaired
        ? { t: "peer.left", peerId: "", reason: "timeout", resumable: false, resumeDeadline: null }
        : { t: "error", code: "code_expired", message: "pairing code expired", fatal: false });
    }
    throttle.sweep(now);

    for (const conn of conns) {
      if (conn.missedKeepAlives >= 2) {
        conn.socket.terminate();
        continue; // the close handler does the cleanup
      }
      conn.missedKeepAlives++;
      if (conn.socket.readyState === conn.socket.OPEN) conn.socket.ping();
    }
  }, LIMITS.keepAliveMs);
  ticker.unref();

  http.listen(config.port, config.host);

  return {
    http,
    get sessionCount() {
      return sessions.size;
    },
    get connectionCount() {
      return conns.size;
    },
    close: () =>
      new Promise<void>((resolve) => {
        clearInterval(ticker);
        for (const conn of conns) conn.socket.terminate();
        wss.close(() => http.close(() => resolve()));
      }),
  };
}

function joinErrorMessage(reason: "invalid_code" | "code_expired" | "session_full"): string {
  switch (reason) {
    case "invalid_code":
      return "no session with that code";
    case "code_expired":
      return "that code has expired; generate a new one";
    case "session_full":
      return "that session already has a camera connected";
  }
}

function clientIp(req: IncomingMessage): string {
  // Trusted only because this sits behind our own reverse proxy. Exposing the
  // service directly would make this header attacker-controlled and defeat the
  // join throttle and the connection caps.
  const forwarded = req.headers["x-forwarded-for"];
  if (typeof forwarded === "string" && forwarded.length > 0) {
    return forwarded.split(",")[0]!.trim();
  }
  return req.socket.remoteAddress ?? "unknown";
}
