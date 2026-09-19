import { afterEach, beforeEach, describe, it } from "node:test";
import assert from "node:assert/strict";
import type { AddressInfo } from "node:net";
import { WebSocket } from "ws";
import { LIMITS, PROTOCOL_VERSION, type ClientMessage, type ServerMessage } from "@rhevia/proto";
import type { Config } from "./config.js";
import { createSignalingServer, type SignalingServer } from "./server.js";

const TEST_CONFIG: Config = {
  port: 0,
  host: "127.0.0.1",
  pairUrlBase: "https://link.test/pair",
  stunUrls: ["stun:stun.test:3478"],
  turnUrls: ["turn:turn.test:3478"],
  turnSecret: "test-secret",
  turnCredentialTtlSec: 3600,
  allowedOrigins: [],
};

/** A test client that queues server messages so tests can await them in order. */
class TestClient {
  readonly #socket: WebSocket;
  readonly #queue: ServerMessage[] = [];
  #waiting: ((msg: ServerMessage) => void) | null = null;

  private constructor(socket: WebSocket) {
    this.#socket = socket;
    socket.on("message", (raw) => {
      const msg = JSON.parse(raw.toString()) as ServerMessage;
      const waiting = this.#waiting;
      if (waiting) {
        this.#waiting = null;
        waiting(msg);
      } else {
        this.#queue.push(msg);
      }
    });
  }

  static async connect(port: number): Promise<TestClient> {
    const socket = new WebSocket(`ws://127.0.0.1:${port}`);
    await new Promise<void>((resolve, reject) => {
      socket.once("open", resolve);
      socket.once("error", reject);
    });
    return new TestClient(socket);
  }

  send(msg: ClientMessage): void {
    this.#socket.send(JSON.stringify(msg));
  }

  /** Sends anything, including shapes the protocol types forbid. */
  sendRaw(raw: string): void {
    this.#socket.send(raw);
  }

  next(timeoutMs = 2000): Promise<ServerMessage> {
    const queued = this.#queue.shift();
    if (queued) return Promise.resolve(queued);
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.#waiting = null;
        reject(new Error("timed out waiting for a server message"));
      }, timeoutMs);
      this.#waiting = (msg) => {
        clearTimeout(timer);
        resolve(msg);
      };
    });
  }

  /** Completes the hello exchange and returns the assigned peer id. */
  async hello(role: "receiver" | "sender", name: string): Promise<string> {
    this.send({
      t: "hello",
      protocol: PROTOCOL_VERSION,
      role,
      client: { name, platform: "test", appVersion: "0.1.0" },
    });
    const ack = await this.next();
    assert.equal(ack.t, "hello.ok");
    return (ack as Extract<ServerMessage, { t: "hello.ok" }>).peerId;
  }

  close(): void {
    this.#socket.close();
  }

  /** Yanks the socket without a close handshake: a phone losing signal. */
  terminate(): void {
    this.#socket.terminate();
  }

  /** Resolves false if the server refused the upgrade. */
  static async tryConnect(port: number): Promise<WebSocket | null> {
    const socket = new WebSocket(`ws://127.0.0.1:${port}`);
    return new Promise((resolve) => {
      socket.once("open", () => resolve(socket));
      socket.once("error", () => resolve(null));
      socket.once("unexpected-response", () => resolve(null));
    });
  }
}

describe("signaling server", () => {
  let server: SignalingServer;
  let port: number;

  // A fresh server per test. The rate limiter and the per-IP session cap are
  // both keyed on the client address, and every test connects from 127.0.0.1,
  // so a shared instance would let one test's throttling break the next.
  beforeEach(async () => {
    server = createSignalingServer(TEST_CONFIG);
    await new Promise<void>((resolve) => server.http.once("listening", resolve));
    port = (server.http.address() as AddressInfo).port;
  });

  afterEach(async () => {
    await server.close();
  });

  /** Drives a receiver and sender through to the paired state. */
  async function pair(): Promise<{ receiver: TestClient; sender: TestClient; code: string }> {
    const receiver = await TestClient.connect(port);
    await receiver.hello("receiver", "Studio PC");
    receiver.send({ t: "session.create" });

    const created = await receiver.next();
    assert.equal(created.t, "session.created");
    const { code } = created as Extract<ServerMessage, { t: "session.created" }>;

    const sender = await TestClient.connect(port);
    await sender.hello("sender", "Pixel 8");
    sender.send({ t: "session.join", code });

    return { receiver, sender, code };
  }

  it("issues a six-digit code and time-limited TURN credentials", async () => {
    const receiver = await TestClient.connect(port);
    receiver.send({
      t: "hello",
      protocol: PROTOCOL_VERSION,
      role: "receiver",
      client: { name: "Studio PC", platform: "win32", appVersion: "0.1.0" },
    });

    const ack = (await receiver.next()) as Extract<ServerMessage, { t: "hello.ok" }>;
    assert.equal(ack.t, "hello.ok");

    const turn = ack.iceServers.find((s) => s.urls.some((u) => u.startsWith("turn:")));
    assert.ok(turn, "a TURN server should be offered");
    // coturn REST scheme: username is "<unix expiry>:<peer id>".
    assert.match(turn.username ?? "", /^\d+:/);
    assert.ok(turn.credential && turn.credential.length > 0, "credential should be minted");

    const expiry = Number((turn.username ?? "").split(":")[0]);
    assert.ok(expiry > Date.now() / 1000, "credential should not already be expired");

    receiver.send({ t: "session.create" });
    const created = (await receiver.next()) as Extract<ServerMessage, { t: "session.created" }>;
    assert.equal(created.t, "session.created");
    assert.match(created.code, /^\d{6}$/);
    assert.equal(created.pairUrl, `https://link.test/pair?c=${created.code}`);
    assert.ok(created.expiresAt > Date.now());

    receiver.close();
  });

  it("pairs a sender to a receiver by code and tells both sides", async () => {
    const { receiver, sender } = await pair();

    const joined = (await sender.next()) as Extract<ServerMessage, { t: "session.joined" }>;
    assert.equal(joined.t, "session.joined");
    assert.equal(joined.peer.name, "Studio PC");
    assert.equal(joined.peer.role, "receiver");

    const peerJoined = (await receiver.next()) as Extract<ServerMessage, { t: "peer.joined" }>;
    assert.equal(peerJoined.t, "peer.joined");
    assert.equal(peerJoined.peer.name, "Pixel 8");
    assert.equal(peerJoined.peer.role, "sender");

    receiver.close();
    sender.close();
  });

  it("relays SDP and ICE between paired peers without inspecting them", async () => {
    const { receiver, sender } = await pair();
    await sender.next(); // session.joined
    await receiver.next(); // peer.joined

    // The phone holds the media, so it offers and the desktop answers.
    sender.send({ t: "signal", payload: { kind: "offer", sdp: "v=0\r\no=- OFFER" } });
    const offer = (await receiver.next()) as Extract<ServerMessage, { t: "signal" }>;
    assert.equal(offer.t, "signal");
    assert.deepEqual(offer.payload, { kind: "offer", sdp: "v=0\r\no=- OFFER" });

    receiver.send({ t: "signal", payload: { kind: "answer", sdp: "v=0\r\no=- ANSWER" } });
    const answer = (await sender.next()) as Extract<ServerMessage, { t: "signal" }>;
    assert.deepEqual(answer.payload, { kind: "answer", sdp: "v=0\r\no=- ANSWER" });

    sender.send({
      t: "signal",
      payload: { kind: "candidate", candidate: "candidate:1 1 udp 2130706431 10.0.0.1 54321 typ host", sdpMid: "0", sdpMLineIndex: 0 },
    });
    const candidate = (await receiver.next()) as Extract<ServerMessage, { t: "signal" }>;
    assert.equal(candidate.payload.kind, "candidate");

    receiver.close();
    sender.close();
  });

  it("retires a code once redeemed, so it cannot be reused", async () => {
    const { receiver, sender, code } = await pair();
    await sender.next();
    await receiver.next();

    const intruder = await TestClient.connect(port);
    await intruder.hello("sender", "Someone else");
    intruder.send({ t: "session.join", code });

    const err = (await intruder.next()) as Extract<ServerMessage, { t: "error" }>;
    assert.equal(err.t, "error");
    // The code was removed from the lookup on pairing, so a replay looks like
    // an unknown code rather than revealing that a session exists.
    assert.equal(err.code, "invalid_code");

    intruder.close();
    receiver.close();
    sender.close();
  });

  it("tells the receiver when the camera disconnects", async () => {
    const { receiver, sender } = await pair();
    await sender.next();
    await receiver.next();

    sender.send({ t: "bye" });
    const left = (await receiver.next()) as Extract<ServerMessage, { t: "peer.left" }>;
    assert.equal(left.t, "peer.left");
    assert.equal(left.reason, "bye");

    receiver.close();
  });

  it("throttles repeated wrong codes so the keyspace cannot be walked", async () => {
    const attacker = await TestClient.connect(port);
    await attacker.hello("sender", "Attacker");

    let sawRateLimit = false;
    for (let i = 0; i < 12; i++) {
      attacker.send({ t: "session.join", code: String(100000 + i) });
      const err = (await attacker.next()) as Extract<ServerMessage, { t: "error" }>;
      assert.equal(err.t, "error");
      if (err.code === "rate_limited") {
        sawRateLimit = true;
        break;
      }
      assert.equal(err.code, "invalid_code");
    }
    assert.ok(sawRateLimit, "should be rate limited within 12 attempts");

    attacker.close();
  });

  it("rejects malformed input rather than trusting it", async () => {
    const client = await TestClient.connect(port);
    await client.hello("sender", "Fuzzer");

    const bad = [
      "not json at all",
      "[]",
      '{"t":"session.join"}',
      '{"t":"session.join","code":"12ab56"}',
      '{"t":"session.join","code":"1234567"}',
      '{"t":"signal","payload":{"kind":"nonsense"}}',
      '{"t":"there-is-no-such-type"}',
    ];

    for (const raw of bad) {
      client.sendRaw(raw);
      const err = (await client.next()) as Extract<ServerMessage, { t: "error" }>;
      assert.equal(err.t, "error", `expected an error for: ${raw}`);
      assert.ok(!err.fatal, "malformed input should not drop the connection");
    }

    client.close();
  });

  it("refuses a client speaking a different protocol version", async () => {
    const client = await TestClient.connect(port);
    client.send({
      t: "hello",
      protocol: PROTOCOL_VERSION + 99,
      role: "sender",
      client: { name: "Old app", platform: "android", appVersion: "0.0.1" },
    });

    const err = (await client.next()) as Extract<ServerMessage, { t: "error" }>;
    assert.equal(err.code, "bad_protocol_version");
    assert.equal(err.fatal, true);

    client.close();
  });

  it("requires hello before anything else", async () => {
    const client = await TestClient.connect(port);
    client.send({ t: "session.create" });

    const err = (await client.next()) as Extract<ServerMessage, { t: "error" }>;
    assert.equal(err.code, "unexpected_message");
    assert.equal(err.fatal, true);

    client.close();
  });

  it("reserves the slot when a camera drops, and lets it resume", async () => {
    // The reliability requirement: a phone that loses signal for a few seconds
    // must come back to the same input, not force a re-pair mid-show.
    const receiver = await TestClient.connect(port);
    await receiver.hello("receiver", "Studio PC");
    receiver.send({ t: "session.create" });
    const created = (await receiver.next()) as Extract<ServerMessage, { t: "session.created" }>;
    assert.ok(created.resumeToken, "the receiver needs a resume token too");

    const sender = await TestClient.connect(port);
    await sender.hello("sender", "Pixel 8");
    sender.send({ t: "session.join", code: created.code });
    const joined = (await sender.next()) as Extract<ServerMessage, { t: "session.joined" }>;
    const token = joined.resumeToken;
    assert.ok(token, "the camera needs a resume token");
    await receiver.next(); // peer.joined

    // Phone loses signal.
    sender.terminate();
    const left = (await receiver.next()) as Extract<ServerMessage, { t: "peer.left" }>;
    assert.equal(left.t, "peer.left");
    assert.equal(left.resumable, true, "a dropped camera should be resumable");
    assert.ok(left.resumeDeadline && left.resumeDeadline > Date.now());

    // Phone comes back on a new socket and reclaims its slot.
    const revived = await TestClient.connect(port);
    await revived.hello("sender", "Pixel 8");
    revived.send({ t: "session.resume", token });

    const resumed = (await revived.next()) as Extract<ServerMessage, { t: "session.resumed" }>;
    assert.equal(resumed.t, "session.resumed");
    assert.equal(resumed.peer.name, "Studio PC");

    const rejoined = (await receiver.next()) as Extract<ServerMessage, { t: "peer.rejoined" }>;
    assert.equal(rejoined.t, "peer.rejoined");
    assert.equal(rejoined.peer.name, "Pixel 8");

    // And signalling works again, so renegotiation can proceed.
    revived.send({ t: "signal", payload: { kind: "offer", sdp: "v=0 RESUMED" } });
    const relayed = (await receiver.next()) as Extract<ServerMessage, { t: "signal" }>;
    assert.equal(relayed.t, "signal");

    receiver.close();
    revived.close();
  });

  it("refuses a bogus resume token, and refuses to displace a live camera", async () => {
    const receiver = await TestClient.connect(port);
    await receiver.hello("receiver", "Studio PC");
    receiver.send({ t: "session.create" });
    const created = (await receiver.next()) as Extract<ServerMessage, { t: "session.created" }>;

    const sender = await TestClient.connect(port);
    await sender.hello("sender", "Pixel 8");
    sender.send({ t: "session.join", code: created.code });
    const joined = (await sender.next()) as Extract<ServerMessage, { t: "session.joined" }>;
    await receiver.next();

    const intruder = await TestClient.connect(port);
    await intruder.hello("sender", "Intruder");

    // Garbage token.
    intruder.send({ t: "session.resume", token: "A".repeat(43) });
    assert.equal(
      ((await intruder.next()) as Extract<ServerMessage, { t: "error" }>).code,
      "invalid_resume_token",
    );

    // A stolen but *valid* token must not evict a camera that is still live —
    // otherwise leaking a token means losing the shot mid-broadcast.
    intruder.send({ t: "session.resume", token: joined.resumeToken });
    assert.equal(
      ((await intruder.next()) as Extract<ServerMessage, { t: "error" }>).code,
      "invalid_resume_token",
    );

    intruder.close();
    receiver.close();
    sender.close();
  });

  it("caps concurrent connections from one address", async () => {
    // Verified exploitable before this limit existed: 400 sockets, 0 rejected.
    const opened: WebSocket[] = [];
    let refused = 0;
    for (let i = 0; i < LIMITS.maxConnectionsPerIp + 5; i++) {
      const socket = await TestClient.tryConnect(port);
      if (socket) opened.push(socket);
      else refused++;
    }
    assert.ok(refused > 0, "the server should refuse connections beyond the per-IP cap");
    assert.ok(
      opened.length <= LIMITS.maxConnectionsPerIp,
      `expected at most ${LIMITS.maxConnectionsPerIp} sockets, got ${opened.length}`,
    );
    for (const s of opened) s.terminate();
  });

  it("caps how many pairing codes one address can hold", async () => {
    // Verified exploitable before this limit existed: one IP held 250 codes.
    const clients: TestClient[] = [];
    let denied = 0;

    for (let i = 0; i < LIMITS.maxSessionsPerIp + 3; i++) {
      const client = await TestClient.connect(port);
      await client.hello("receiver", `Desktop ${i}`);
      client.send({ t: "session.create" });
      const reply = await client.next();
      if (reply.t === "error") {
        assert.equal((reply as Extract<ServerMessage, { t: "error" }>).code, "too_many_sessions");
        denied++;
      }
      clients.push(client);
    }

    assert.equal(denied, 3, "codes beyond the cap should be denied");
    for (const c of clients) c.close();
  });

  it("does not let a sender create a session or a receiver join one", async () => {
    const sender = await TestClient.connect(port);
    await sender.hello("sender", "Phone");
    sender.send({ t: "session.create" });
    assert.equal(((await sender.next()) as Extract<ServerMessage, { t: "error" }>).code, "unexpected_message");

    const receiver = await TestClient.connect(port);
    await receiver.hello("receiver", "Desktop");
    receiver.send({ t: "session.join", code: "123456" });
    assert.equal(((await receiver.next()) as Extract<ServerMessage, { t: "error" }>).code, "unexpected_message");

    sender.close();
    receiver.close();
  });
});
