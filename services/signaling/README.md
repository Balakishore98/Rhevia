# RheviaLink signaling server

Pairs a phone to a desktop by six-digit code and relays WebRTC setup between
them. It never touches media: once the peer connection is established, video,
audio, tally and camera control all flow directly between the two endpoints.

## Run it

```bash
npm install          # from the repository root
npm run build
npm start -w @rhevia/signaling
```

Configuration is environment-driven; see [`infra/README.md`](../../infra/README.md).
With no TURN configured it runs STUN-only, which is fine for two machines on the
same network but will fail across carrier-grade NAT.

## Test

```bash
npm test -w @rhevia/signaling
```

## Protocol

Message types live in [`proto/signaling.ts`](../../proto/signaling.ts), shared
with the desktop and mobile clients.

```
receiver                server                 sender
   │                      │                      │
   ├── hello ────────────►│                      │
   │◄──── hello.ok ───────┤  (+ ICE servers,     │
   │                      │     TURN creds)      │
   ├── session.create ───►│                      │
   │◄─ session.created ───┤  code: 483920        │
   │   show QR + digits   │                      │
   │                      │◄──────── hello ──────┤
   │                      ├──── hello.ok ───────►│
   │                      │◄─ session.join ──────┤  code: 483920
   │◄──── peer.joined ────┼── session.joined ───►│
   │                      │                      │
   │                      │◄──── signal offer ───┤   phone has the media,
   │◄──── signal offer ───┤                      │   so the phone offers
   ├── signal answer ────►│                      │
   │                      ├─── signal answer ───►│
   │◄─── signal ice ──────┼──── signal ice ─────►│
   │                      │                      │
   └──────── direct WebRTC, or TURN relay ───────┘
```

## Design notes

- **Codes are single-use.** Redeeming one removes it from the lookup table, so a
  shoulder-surfed or leaked code cannot be redeemed twice. A replay is reported
  as `invalid_code` rather than `session_full`, so the server does not confirm
  that a session exists.
- **Join attempts are throttled per IP.** Six digits is only 10^6 combinations;
  without a throttle the keyspace is walkable. Combined with the five-minute
  TTL, guessing is impractical.
- **Signaling payloads are opaque.** The server does not parse SDP. Endpoint
  changes — new codecs, simulcast, data channels — need no server deploy.
- **Single process by design.** A pairing session is meaningless once either
  socket closes, so there is nothing worth persisting. Running multiple
  instances needs sticky routing by code or a shared store; until concurrency
  justifies it, one process is simpler and has fewer failure modes.
