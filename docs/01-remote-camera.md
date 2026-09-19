# Rhevia Remote Camera (working name: **RheviaLink**)

The flagship feature: a phone anywhere on the internet becomes a broadcast camera
input on a desktop anywhere else. No shared network, no IP configuration, no port
forwarding.

## Why not NDI as the transport

NDI is a LAN protocol and cannot do this job:

| Property | NDI (full) | What we need |
|---|---|---|
| Bitrate, 1080p60 | ~100-160 Mbps (SpeedHQ, intra-frame) | 2-12 Mbps over 4G/5G |
| Discovery | mDNS / Bonjour, same L2 subnet | works across any two networks |
| NAT traversal | none | mandatory (mobile carriers are CGNAT) |
| Congestion control | none, assumes gigabit LAN | mandatory, link quality varies |
| Loss recovery | none | mandatory |

`NDI|HX` is the low-bitrate H.264/HEVC variant, but encoding to it requires the
**NDI Advanced SDK**, which is a separate paid licence from Vizrt. It also still
relies on mDNS discovery, so it does not solve the cross-network problem.

**Decision: WebRTC is the transport. NDI is an output we offer, not an input we depend on.**

## Pipeline

```
┌───────────────────────────┐
│ Phone (Rhevia Camera app) │
│  CameraX / AVFoundation   │
│  → HW H.264/HEVC encode   │
│  → Opus audio             │
└─────────────┬─────────────┘
              │ DTLS-SRTP
              │
    ┌─────────▼──────────┐      ┌──────────────────┐
    │  Signaling (WSS)   │◄────►│  STUN  +  TURN   │   cloud, small VPS
    │  pairing + SDP/ICE │      │  relay fallback  │
    └─────────┬──────────┘      └──────────────────┘
              │
              │  P2P direct when ICE succeeds (~70-80% of the time)
              │  TURN relay when it does not
              ▼
┌──────────────────────────────────────────────┐
│ Rhevia Desktop                               │
│  WebRTC receive → NVDEC → GPU texture        │
│  → mixer input (never touches system RAM)    │
│                                              │
│  optional: re-publish as a local NDI sender  │
│  so OBS / vMix / any NDI app can use it too  │
└──────────────────────────────────────────────┘
```

### Why WebRTC and not SRT or RIST

Both SRT and RIST are excellent contribution protocols and are already compiled
into our FFmpeg. They are the right tool for **site-to-site** links with known
endpoints. They are the wrong tool here because:

- NAT traversal is manual. A phone on mobile data sits behind carrier-grade NAT
  and cannot accept an inbound connection. You would need a relay anyway.
- No built-in adaptive bitrate. On a phone walking around a venue, the link
  quality changes constantly.

WebRTC gives ICE/STUN/TURN, congestion control (GCC), NACK retransmission and
FEC in one package. Latency lands around 100-300 ms glass-to-glass.

Offer SRT/RIST as *additional* ingest types for fixed encoders and remote studios.

## Pairing flow

No IP addresses are ever shown to the user.

1. Desktop displays a QR code and a 6-digit code.
2. Phone scans the QR, or types the code.
3. Signaling server matches them, brokers SDP + ICE candidates.
4. Input appears in Rhevia, named after the phone.

## What makes it better than vMix, not merely equal

These are the features that justify the product existing:

- **Phone-side ISO recording.** The phone records full quality locally *while*
  streaming a bitrate-adapted feed. After the show, pull the pristine file over
  and relink it for the edit. The live feed can be 3 Mbps and the archive 50 Mbps.
- **Tally back-channel.** Phone screen border goes red on Program, green on
  Preview. The operator behind the phone becomes a real camera operator.
- **Remote camera control** from the desktop: focus, exposure lock, ISO, shutter,
  white balance, zoom, torch, stabilisation, lens selection.
- **Browser sender.** Because the transport is WebRTC, a guest can join from a
  link with no app install at all. This is vMix Call, without the friction.
- **Store-and-forward gap repair.** If the link drops for 4 seconds, the phone
  has those 4 seconds on local storage. Backfill them into the recording (not
  the live feed) so the archive is clean even when the stream was not.

## Cloud infrastructure required

This is the part that costs money and cannot be avoided:

- **Signaling server** — WebSocket, stateless, tiny. Any small VPS.
- **STUN** — trivial, can self-host (`coturn`) or use public.
- **TURN relay** — the real cost. Relays media when P2P fails. Budget by
  transferred GB. `coturn` on a VPS with good egress pricing.

Rough figure: a 5 Mbps feed relayed for one hour is ~2.25 GB each way.
Plan tiering around relayed minutes, since direct P2P connections cost us nothing.
