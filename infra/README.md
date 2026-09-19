# Infrastructure

RheviaLink needs two small internet-facing services. Everything else runs on the
user's own machine.

| Service | What it does | Cost driver |
|---|---|---|
| Signaling (`services/signaling`) | Pairs a phone to a desktop by six-digit code, relays WebRTC setup | negligible — a few KB per pairing |
| TURN (`infra/coturn`) | Relays media when peers cannot connect directly | **bandwidth** — this is the real cost |

## Cost model

A direct peer-to-peer connection costs nothing to run. Only relayed sessions
consume bandwidth, at roughly:

```
5 Mbps feed x 3600 s = 2.25 GB per hour, each direction
```

So price any hosted tier on **relayed minutes**, not total minutes, and report
the two separately in the desktop UI so users can see when they are on the
expensive path.

## Deploying

1. Point a domain at the VM, e.g. `link.rhevia.app`, and obtain a certificate.
2. Generate a shared secret and put the same value in both places:
   - `static-auth-secret` in `coturn/turnserver.conf`
   - `RHEVIA_TURN_SECRET` in the signaling environment
3. Run the signaling server behind a reverse proxy terminating TLS (`wss://`).
   It trusts `X-Forwarded-For` for its join throttle, so it must **not** be
   exposed directly — that header would otherwise be attacker-controlled.
4. Open UDP/TCP 3478 and 5349, plus coturn's relay port range.

### Signaling environment

| Variable | Default | Notes |
|---|---|---|
| `RHEVIA_PORT` | `8080` | behind the proxy |
| `RHEVIA_HOST` | `0.0.0.0` | |
| `RHEVIA_PAIR_URL_BASE` | `https://link.rhevia.app/pair` | encoded into the QR code |
| `RHEVIA_STUN_URLS` | Google public STUN | comma-separated; self-host before launch |
| `RHEVIA_TURN_URLS` | none | e.g. `turn:link.rhevia.app:3478,turns:link.rhevia.app:5349` |
| `RHEVIA_TURN_SECRET` | none | must match coturn |
| `RHEVIA_TURN_TTL_SEC` | `43200` | credential lifetime |
| `RHEVIA_ALLOWED_ORIGINS` | none (all allowed) | set once the browser sender ships |

Setting `RHEVIA_TURN_URLS` without `RHEVIA_TURN_SECRET` fails at startup rather
than silently handing out a relay nobody can authenticate against.
