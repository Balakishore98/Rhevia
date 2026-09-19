import { createHmac } from "node:crypto";
import type { IceServer } from "@rhevia/proto";

export interface Config {
  port: number;
  host: string;
  /** Base for the QR payload, e.g. "https://link.rhevia.app/pair". */
  pairUrlBase: string;
  stunUrls: string[];
  turnUrls: string[];
  /** Shared secret matching coturn's `static-auth-secret`. */
  turnSecret: string | null;
  turnCredentialTtlSec: number;
  /** Allowed Origin values for browser senders. Empty disables the check. */
  allowedOrigins: string[];
}

function list(name: string, fallback: string[]): string[] {
  const raw = process.env[name];
  if (!raw) return fallback;
  return raw.split(",").map((s) => s.trim()).filter(Boolean);
}

export function loadConfig(env = process.env): Config {
  const turnSecret = env.RHEVIA_TURN_SECRET ?? null;
  const turnUrls = list("RHEVIA_TURN_URLS", []);

  if (turnUrls.length > 0 && !turnSecret) {
    throw new Error("RHEVIA_TURN_URLS is set but RHEVIA_TURN_SECRET is not; TURN would be unusable");
  }

  return {
    port: Number(env.RHEVIA_PORT ?? 8080),
    host: env.RHEVIA_HOST ?? "0.0.0.0",
    pairUrlBase: env.RHEVIA_PAIR_URL_BASE ?? "https://link.rhevia.app/pair",
    stunUrls: list("RHEVIA_STUN_URLS", ["stun:stun.l.google.com:19302"]),
    turnUrls,
    turnSecret,
    turnCredentialTtlSec: Number(env.RHEVIA_TURN_TTL_SEC ?? 12 * 60 * 60),
    allowedOrigins: list("RHEVIA_ALLOWED_ORIGINS", []),
  };
}

/**
 * Mints short-lived TURN credentials using coturn's REST scheme
 * (`use-auth-secret` / `static-auth-secret`).
 *
 * The alternative is a static TURN username and password compiled into the
 * mobile app, which is equivalent to publishing them: anyone who extracts them
 * gets free bandwidth on our relay. These expire instead, and the secret itself
 * never leaves the server.
 */
export function mintIceServers(config: Config, peerId: string, now = Date.now()): IceServer[] {
  const servers: IceServer[] = [];

  if (config.stunUrls.length > 0) {
    servers.push({ urls: config.stunUrls });
  }

  if (config.turnUrls.length > 0 && config.turnSecret) {
    const expiry = Math.floor(now / 1000) + config.turnCredentialTtlSec;
    const username = `${expiry}:${peerId}`;
    const credential = createHmac("sha1", config.turnSecret).update(username).digest("base64");
    servers.push({ urls: config.turnUrls, username, credential });
  }

  return servers;
}
