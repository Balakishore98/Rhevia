import { CODE_LENGTH, type ClientInfo, type ClientMessage, type SignalPayload } from "@rhevia/proto";

export type Parsed<T> = { ok: true; value: T } | { ok: false; error: string };

/**
 * Validates untrusted JSON from the network into a `ClientMessage`.
 *
 * Every field is checked explicitly rather than cast. This is the only place
 * attacker-controlled data enters the server, so the rule here is that nothing
 * reaches the handlers until its shape is proven.
 */
export function parseClientMessage(raw: string): Parsed<ClientMessage> {
  let json: unknown;
  try {
    json = JSON.parse(raw);
  } catch {
    return { ok: false, error: "not valid JSON" };
  }
  if (!isRecord(json)) return { ok: false, error: "message must be an object" };

  const t = json.t;
  if (typeof t !== "string") return { ok: false, error: "missing message type" };

  switch (t) {
    case "hello": {
      if (typeof json.protocol !== "number") return { ok: false, error: "hello.protocol must be a number" };
      if (json.role !== "receiver" && json.role !== "sender") {
        return { ok: false, error: "hello.role must be 'receiver' or 'sender'" };
      }
      const client = parseClientInfo(json.client);
      if (!client.ok) return client;
      return { ok: true, value: { t: "hello", protocol: json.protocol, role: json.role, client: client.value } };
    }

    case "session.create":
      return { ok: true, value: { t: "session.create" } };

    case "session.join": {
      const code = json.code;
      // Digits only and exactly the expected length, so a malformed code is
      // rejected before it can consume a throttle slot or touch the registry.
      if (typeof code !== "string" || !new RegExp(`^\\d{${CODE_LENGTH}}$`).test(code)) {
        return { ok: false, error: `code must be ${CODE_LENGTH} digits` };
      }
      return { ok: true, value: { t: "session.join", code } };
    }

    case "signal": {
      const payload = parseSignalPayload(json.payload);
      if (!payload.ok) return payload;
      return { ok: true, value: { t: "signal", payload: payload.value } };
    }

    case "bye":
      return { ok: true, value: { t: "bye" } };

    case "ping": {
      if (typeof json.n !== "number") return { ok: false, error: "ping.n must be a number" };
      return { ok: true, value: { t: "ping", n: json.n } };
    }

    default:
      return { ok: false, error: `unknown message type '${t}'` };
  }
}

const MAX_NAME = 64;
const MAX_SDP = 32 * 1024;
const MAX_CANDIDATE = 1024;

function parseClientInfo(value: unknown): Parsed<ClientInfo> {
  if (!isRecord(value)) return { ok: false, error: "hello.client must be an object" };
  const name = str(value.name, MAX_NAME);
  const platform = str(value.platform, MAX_NAME);
  const appVersion = str(value.appVersion, MAX_NAME);
  if (name === null) return { ok: false, error: "client.name must be a string" };
  if (platform === null) return { ok: false, error: "client.platform must be a string" };
  if (appVersion === null) return { ok: false, error: "client.appVersion must be a string" };
  return { ok: true, value: { name, platform, appVersion } };
}

function parseSignalPayload(value: unknown): Parsed<SignalPayload> {
  if (!isRecord(value)) return { ok: false, error: "signal.payload must be an object" };

  switch (value.kind) {
    case "offer":
    case "answer": {
      const sdp = str(value.sdp, MAX_SDP);
      if (sdp === null) return { ok: false, error: `${value.kind}.sdp must be a string under ${MAX_SDP} bytes` };
      return { ok: true, value: { kind: value.kind, sdp } };
    }

    case "candidate": {
      const candidate = str(value.candidate, MAX_CANDIDATE);
      if (candidate === null) return { ok: false, error: "candidate must be a string" };
      const sdpMid = value.sdpMid;
      const sdpMLineIndex = value.sdpMLineIndex;
      if (sdpMid !== null && typeof sdpMid !== "string") {
        return { ok: false, error: "candidate.sdpMid must be a string or null" };
      }
      if (sdpMLineIndex !== null && typeof sdpMLineIndex !== "number") {
        return { ok: false, error: "candidate.sdpMLineIndex must be a number or null" };
      }
      return { ok: true, value: { kind: "candidate", candidate, sdpMid, sdpMLineIndex } };
    }

    case "candidate-end":
      return { ok: true, value: { kind: "candidate-end" } };

    default:
      return { ok: false, error: "unknown signal payload kind" };
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** Returns the string if it is one and within `max` characters, else null. */
function str(value: unknown, max: number): string | null {
  if (typeof value !== "string" || value.length > max) return null;
  return value;
}
