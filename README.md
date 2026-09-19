# Rhevia

A live production switcher aimed at the ground vMix holds — with three things
vMix does not do well.

## The three bets

**1. A phone is a camera, from anywhere.**
Not "install an app and join the same Wi-Fi." Scan a QR code and the phone is a
broadcast camera input, on mobile data, in another city, with tally light,
remote focus/exposure control, and full-quality local recording running
alongside the bitrate-adapted live feed. See [`docs/01-remote-camera.md`](docs/01-remote-camera.md).

**2. Audio that cannot take the show down.**
A deeper built-in chain than vMix's gate/EQ/compressor — de-esser, dynamic EQ,
multiband compression, true-peak limiting — and VST3 hosted in a **sandboxed
process**, so a third-party plugin crash costs you one input instead of the
whole broadcast. See [`docs/03-engine.md`](docs/03-engine.md#audio).

**3. The recording is editable while it is still recording.**
Fragmented MP4 plus a live all-intra proxy means you can mark, cut, auto-reframe
to 9:16 and publish a short *during* the show, not hours after it.
See [`docs/03-engine.md`](docs/03-engine.md#recording-live-editing-and-shorts).

Underneath all three: a GPU-resident pipeline with tiered decoding, so input
count scales with the machine instead of collapsing at a fixed number.

## Documents

| | |
|---|---|
| [`docs/01-remote-camera.md`](docs/01-remote-camera.md) | The flagship feature, and why the transport is WebRTC rather than NDI |
| [`docs/02-licensing.md`](docs/02-licensing.md) | What we may and may not read, link, and ship |
| [`docs/03-engine.md`](docs/03-engine.md) | Video pipeline, audio DSP, recording, graphics |
| [`docs/04-beyond-vmix.md`](docs/04-beyond-vmix.md) | Feature-by-feature against vMix, and where we actually win |
| [`docs/05-product-model.md`](docs/05-product-model.md) | Why there are no licence tiers, enforced in the code |
| [`docs/06-command-bus.md`](docs/06-command-bus.md) | One command bus, many front-ends — how remote control comes for free |

## Status

Pre-alpha. The RheviaLink path works end to end: a camera pairs by six-digit
code through the signaling server, negotiates WebRTC, and streams H.264 that
the desktop receiver counts as real RTP. Proven by `cargo test --test loopback`,
which runs a synthetic camera against the actual server rather than a mock.

Not yet built: the phone app, hardware decode, and the mixer itself.

| Component | State |
|---|---|
| Signaling server (`services/signaling`) | working, hardened, 14 tests |
| Rust link client (`desktop/crates/rhevia-link`) | working, 12 tests incl. cross-language |
| WebRTC ingest | working — negotiates and receives H.264 |
| Relay deployment (`infra/deploy`) | scripted, not yet deployed |
| Phone app | not started |
| NVDEC decode + GPU pipeline | not started |
| Mixer, audio, graphics | not started |

```bash
npm install && npm run build   # signaling server
npm test                       # 14 tests
cd desktop && cargo test       # 12 tests, spawns the real server
```
