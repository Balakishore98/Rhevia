import { randomInt, randomUUID } from "node:crypto";
import { CODE_LENGTH, LIMITS, type ClientInfo, type PeerInfo, type Role } from "@rhevia/proto";

export interface Peer {
  id: string;
  role: Role;
  info: ClientInfo;
  sessionId: string | null;
  /** Cleared on every pong; a peer that misses two keep-alives is dropped. */
  missedKeepAlives: number;
  send(data: string): void;
  close(code: number, reason: string): void;
}

export interface Session {
  id: string;
  code: string;
  receiver: Peer;
  sender: Peer | null;
  createdAt: number;
  /** Null once paired: the code is single-use and stops being redeemable. */
  expiresAt: number | null;
}

export type JoinResult =
  | { ok: true; session: Session }
  | { ok: false; reason: "invalid_code" | "code_expired" | "session_full" };

export function peerInfo(p: Peer): PeerInfo {
  return { peerId: p.id, role: p.role, name: p.info.name, platform: p.info.platform };
}

/**
 * In-memory registry of pairing sessions.
 *
 * Single-process and deliberately so: pairing is short-lived and a session is
 * meaningless once either peer's socket is gone, so there is nothing worth
 * persisting. Running more than one instance requires sticky routing by code,
 * or a shared store — see docs/04-rhevialink-protocol.md.
 */
export class SessionRegistry {
  readonly #byId = new Map<string, Session>();
  readonly #byCode = new Map<string, Session>();

  create(receiver: Peer, now = Date.now()): Session {
    const session: Session = {
      id: randomUUID(),
      code: this.#allocateCode(),
      receiver,
      sender: null,
      createdAt: now,
      expiresAt: now + LIMITS.sessionTtlMs,
    };
    this.#byId.set(session.id, session);
    this.#byCode.set(session.code, session);
    receiver.sessionId = session.id;
    return session;
  }

  join(code: string, sender: Peer, now = Date.now()): JoinResult {
    const session = this.#byCode.get(code);
    if (!session) return { ok: false, reason: "invalid_code" };
    if (session.expiresAt !== null && now > session.expiresAt) {
      this.destroy(session.id);
      return { ok: false, reason: "code_expired" };
    }
    if (session.sender) return { ok: false, reason: "session_full" };

    session.sender = sender;
    sender.sessionId = session.id;

    // The code is single-use. Retiring it on pair means a leaked or
    // shoulder-surfed code cannot be redeemed by a second party.
    session.expiresAt = null;
    this.#byCode.delete(session.code);

    return { ok: true, session };
  }

  get(sessionId: string): Session | undefined {
    return this.#byId.get(sessionId);
  }

  /** The other peer in this peer's session, if there is one. */
  peerOf(peer: Peer): Peer | null {
    if (!peer.sessionId) return null;
    const session = this.#byId.get(peer.sessionId);
    if (!session) return null;
    return session.receiver.id === peer.id ? session.sender : session.receiver;
  }

  destroy(sessionId: string): Session | undefined {
    const session = this.#byId.get(sessionId);
    if (!session) return undefined;
    this.#byId.delete(sessionId);
    this.#byCode.delete(session.code);
    session.receiver.sessionId = null;
    if (session.sender) session.sender.sessionId = null;
    return session;
  }

  /** Drops sessions whose code was never redeemed. Paired sessions never expire. */
  sweepExpired(now = Date.now()): Session[] {
    const dropped: Session[] = [];
    for (const session of this.#byId.values()) {
      if (session.expiresAt !== null && now > session.expiresAt) dropped.push(session);
    }
    for (const session of dropped) this.destroy(session.id);
    return dropped;
  }

  get size(): number {
    return this.#byId.size;
  }

  #allocateCode(): string {
    // 10^6 codes against a handful of live sessions, so collisions are rare;
    // bounded retries keep this honest rather than relying on that.
    for (let attempt = 0; attempt < 32; attempt++) {
      const code = String(randomInt(0, 10 ** CODE_LENGTH)).padStart(CODE_LENGTH, "0");
      if (!this.#byCode.has(code)) return code;
    }
    throw new Error("could not allocate a free pairing code");
  }
}

/**
 * Sliding-window failure counter, keyed by client IP.
 *
 * A six-digit code is only 10^6 possibilities, so without this an attacker
 * could walk the keyspace and hijack a pairing. Combined with the five-minute
 * TTL this makes a successful guess impractical.
 */
export class JoinThrottle {
  readonly #failures = new Map<string, number[]>();

  isBlocked(key: string, now = Date.now()): boolean {
    return this.#recent(key, now).length >= LIMITS.maxJoinFailures;
  }

  recordFailure(key: string, now = Date.now()): void {
    const recent = this.#recent(key, now);
    recent.push(now);
    this.#failures.set(key, recent);
  }

  clear(key: string): void {
    this.#failures.delete(key);
  }

  sweep(now = Date.now()): void {
    for (const key of [...this.#failures.keys()]) {
      if (this.#recent(key, now).length === 0) this.#failures.delete(key);
    }
  }

  #recent(key: string, now: number): number[] {
    const cutoff = now - LIMITS.joinFailureWindowMs;
    const recent = (this.#failures.get(key) ?? []).filter((t) => t > cutoff);
    if (recent.length > 0) this.#failures.set(key, recent);
    return recent;
  }
}
