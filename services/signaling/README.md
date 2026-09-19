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

## Reconnection

A live show must survive a phone crossing a lift, roaming Wi-Fi, or handing over
between cells. So a dropped peer does **not** destroy the session:

```
camera drops ──► slot reserved for 45s ──► peer.left { resumable: true, deadline }
                        │                         desktop UI: "reconnecting"
                        ├── camera returns with its resume token
                        │       └─► peer.rejoined ──► renegotiate, carry on
                        └── deadline passes
                                └─► peer.left { resumable: false } ──► tear down
```

Each peer is issued a 32-byte resume token when it joins. Two rules keep the
token from becoming a weakness:

- A token only works on an **orphaned** slot. A stolen token cannot evict a
  camera that is currently streaming.
- A deliberate `bye` **forfeits** the reservation. Pressing Stop closes the
  input immediately instead of leaving it on "reconnecting" for 45 seconds.

## Abuse resistance

These limits exist because the gaps were demonstrated against the running
server, not because they seemed prudent:

| Limit | Before | After |
|---|---|---|
| Sockets from one address | 400 opened, 0 refused | 20 opened, 380 refused |
| Pairing codes held by one address | 250 | 5 |

Rate limiting on wrong codes is deliberately **not** fatal. Behind carrier-grade
NAT many unrelated users share one address, so closing the socket would punish
everyone for one person's typo; rejecting the attempt is enough to stop a
keyspace walk.

> The per-IP limits assume `X-Forwarded-For` comes from our own reverse proxy.
> Exposing this service directly makes that header attacker-controlled and
> defeats every limit in this table.

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
