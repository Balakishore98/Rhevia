import { randomBytes, randomInt, randomUUID } from "node:crypto";
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

/**
 * One side of a session.
 *
 * A slot outlives the connection occupying it. That is the whole point: when a
 * phone loses signal mid-show, the slot stays reserved so the same camera can
 * come back to the same input rather than forcing the operator to re-pair.
 */
export interface Slot {
  peer: Peer | null;
  /** Secret issued to the occupant, presented to reclaim the slot. */
  resumeToken: string;
  /** Null while connected; the disconnect timestamp while orphaned. */
  orphanedAt: number | null;
}

export interface Session {
  id: string;
  code: string;
  receiver: Slot;
  /** Created when a sender redeems the code. */
  sender: Slot | null;
  createdAt: number;
  /** Null once paired: the code is single-use and stops being redeemable. */
  expiresAt: number | null;
}

export type JoinResult =
  | { ok: true; session: Session; resumeToken: string }
  | { ok: false; reason: "invalid_code" | "code_expired" | "session_full" };

export type ResumeResult =
  | { ok: true; session: Session; partner: Peer | null }
  | { ok: false; reason: "invalid_resume_token" };

export function peerInfo(p: Peer): PeerInfo {
  return { peerId: p.id, role: p.role, name: p.info.name, platform: p.info.platform };
}

function newSlot(peer: Peer): Slot {
  // 32 bytes: this token is the only thing standing between an eavesdropper
  // and a live camera slot, and unlike the pairing code it is never displayed,
  // so there is no reason to make it short.
  return { peer, resumeToken: randomBytes(32).toString("base64url"), orphanedAt: null };
}

/**
 * In-memory registry of pairing sessions.
 *
 * Single-process and deliberately so: a session is meaningless once both peers
 * are gone, so there is nothing worth persisting beyond the resume grace
 * window. Running more than one instance requires sticky routing by code and
 * token, or a shared store.
 */
export class SessionRegistry {
  readonly #byId = new Map<string, Session>();
  readonly #byCode = new Map<string, Session>();
  readonly #byResumeToken = new Map<string, { sessionId: string; role: Role }>();
  /** Live session count per IP, to stop one client hoarding the keyspace. */
  readonly #countByIp = new Map<string, number>();

  create(receiver: Peer, ip: string, now = Date.now()): Session {
    const slot = newSlot(receiver);
    const session: Session = {
      id: randomUUID(),
      code: this.#allocateCode(),
      receiver: slot,
      sender: null,
      createdAt: now,
      expiresAt: now + LIMITS.sessionTtlMs,
    };
    this.#byId.set(session.id, session);
    this.#byCode.set(session.code, session);
    this.#byResumeToken.set(slot.resumeToken, { sessionId: session.id, role: "receiver" });
    this.#countByIp.set(ip, (this.#countByIp.get(ip) ?? 0) + 1);
    this.#ipOf.set(session.id, ip);
    receiver.sessionId = session.id;
    return session;
  }

  readonly #ipOf = new Map<string, string>();

  sessionsFor(ip: string): number {
    return this.#countByIp.get(ip) ?? 0;
  }

  join(code: string, sender: Peer, now = Date.now()): JoinResult {
    const session = this.#byCode.get(code);
    if (!session) return { ok: false, reason: "invalid_code" };
    if (session.expiresAt !== null && now > session.expiresAt) {
      this.destroy(session.id);
      return { ok: false, reason: "code_expired" };
    }
    if (session.sender) return { ok: false, reason: "session_full" };

    const slot = newSlot(sender);
    session.sender = slot;
    sender.sessionId = session.id;
    this.#byResumeToken.set(slot.resumeToken, { sessionId: session.id, role: "sender" });

    // The code is single-use. Retiring it on pair means a leaked or
    // shoulder-surfed code cannot be redeemed by a second party.
    session.expiresAt = null;
    this.#byCode.delete(session.code);

    return { ok: true, session, resumeToken: slot.resumeToken };
  }

  /**
   * Reclaims an orphaned slot. The returned `partner` is the peer that stayed
   * connected, if any, so the caller can tell it the camera is back.
   */
  resume(token: string, peer: Peer, now = Date.now()): ResumeResult {
    const ref = this.#byResumeToken.get(token);
    if (!ref) return { ok: false, reason: "invalid_resume_token" };

    const session = this.#byId.get(ref.sessionId);
    if (!session) return { ok: false, reason: "invalid_resume_token" };

    const slot = ref.role === "receiver" ? session.receiver : session.sender;
    if (!slot) return { ok: false, reason: "invalid_resume_token" };

    // Only an orphaned slot can be reclaimed. Otherwise a stolen token would
    // let an attacker displace a camera that is happily streaming.
    if (slot.orphanedAt === null) return { ok: false, reason: "invalid_resume_token" };
    if (now - slot.orphanedAt > LIMITS.resumeGraceMs) {
      return { ok: false, reason: "invalid_resume_token" };
    }

    slot.peer = peer;
    slot.orphanedAt = null;
    peer.sessionId = session.id;
    peer.role = ref.role;

    const other = ref.role === "receiver" ? session.sender : session.receiver;
    return { ok: true, session, partner: other?.peer ?? null };
  }

  /**
   * Marks this peer's slot as orphaned, reserving it for the grace window.
   * Returns the partner still connected, plus when the reservation lapses.
   */
  orphan(peer: Peer, now = Date.now()): { session: Session; partner: Peer | null; deadline: number } | null {
    if (!peer.sessionId) return null;
    const session = this.#byId.get(peer.sessionId);
    if (!session) return null;

    const slot = this.#slotOf(session, peer);
    if (!slot) return null;

    slot.peer = null;
    slot.orphanedAt = now;
    peer.sessionId = null;

    const other = session.receiver.peer?.id === peer.id ? session.sender : session.receiver;
    return { session, partner: other?.peer ?? null, deadline: now + LIMITS.resumeGraceMs };
  }

  get(sessionId: string): Session | undefined {
    return this.#byId.get(sessionId);
  }

  /** The other connected peer in this peer's session, if there is one. */
  peerOf(peer: Peer): Peer | null {
    if (!peer.sessionId) return null;
    const session = this.#byId.get(peer.sessionId);
    if (!session) return null;
    if (session.receiver.peer?.id === peer.id) return session.sender?.peer ?? null;
    return session.receiver.peer;
  }

  destroy(sessionId: string): Session | undefined {
    const session = this.#byId.get(sessionId);
    if (!session) return undefined;

    this.#byId.delete(sessionId);
    this.#byCode.delete(session.code);
    this.#byResumeToken.delete(session.receiver.resumeToken);
    if (session.sender) this.#byResumeToken.delete(session.sender.resumeToken);

    if (session.receiver.peer) session.receiver.peer.sessionId = null;
    if (session.sender?.peer) session.sender.peer.sessionId = null;

    const ip = this.#ipOf.get(sessionId);
    if (ip !== undefined) {
      const next = (this.#countByIp.get(ip) ?? 1) - 1;
      if (next <= 0) this.#countByIp.delete(ip);
      else this.#countByIp.set(ip, next);
      this.#ipOf.delete(sessionId);
    }
    return session;
  }

  /**
   * Drops sessions whose code was never redeemed, and those whose resume
   * window has lapsed. Returns each with the peer that is still waiting, so
   * the caller can tell it the camera is not coming back.
   */
  sweep(now = Date.now()): { session: Session; stranded: Peer | null }[] {
    const dead: { session: Session; stranded: Peer | null }[] = [];

    for (const session of this.#byId.values()) {
      // Never paired, and the code has expired.
      if (session.expiresAt !== null && now > session.expiresAt) {
        dead.push({ session, stranded: session.receiver.peer });
        continue;
      }

      const slots = [session.receiver, ...(session.sender ? [session.sender] : [])];
      const lapsed = slots.find(
        (s) => s.orphanedAt !== null && now - s.orphanedAt > LIMITS.resumeGraceMs,
      );
      if (lapsed) {
        const stranded = slots.find((s) => s.peer !== null)?.peer ?? null;
        dead.push({ session, stranded });
      }
    }

    for (const { session } of dead) this.destroy(session.id);
    return dead;
  }

  get size(): number {
    return this.#byId.size;
  }

  #slotOf(session: Session, peer: Peer): Slot | null {
    if (session.receiver.peer?.id === peer.id) return session.receiver;
    if (session.sender?.peer?.id === peer.id) return session.sender;
    return null;
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
