# Dependency ledger

Every third-party dependency, with its licence, recorded when it is added.

The rule from [`02-licensing.md`](02-licensing.md): permissive (MIT / Apache-2.0
/ BSD / MPL) or LGPL dynamically linked. **No GPL in the tree.** If a dependency
does not fit, it does not go in — find another, or write it.

## Shipped in the product

| Dependency | Version | Licence | OK | Notes |
|---|---|---|---|---|
| `ws` | ^8.18 | MIT | ✅ | WebSocket server, signaling |
| Node.js runtime | >=22 | MIT | ✅ | |
| coturn | 4.x | BSD-3-Clause | ✅ | TURN relay, deployed not shipped |
| `webrtc` (webrtc-rs) | 0.20 | MIT / Apache-2.0 | ✅ | desktop WebRTC ingest |
| `rtc` | 0.20 | MIT / Apache-2.0 | ✅ | Sans-I/O core under webrtc-rs |
| `tokio` | 1.x | MIT | ✅ | async runtime |
| `tokio-tungstenite` | 0.24 | MIT / Apache-2.0 | ✅ | signaling client |
| `serde` / `serde_json` | 1.x | MIT / Apache-2.0 | ✅ | protocol encoding |
| `async-trait` | 0.1 | MIT / Apache-2.0 | ✅ | event handler traits |
| `futures-util` | 0.3 | MIT / Apache-2.0 | ✅ | stream combinators |
| `thiserror` / `anyhow` | 2.x / 1.x | MIT / Apache-2.0 | ✅ | error types |
| `tracing` | 0.1 | MIT | ✅ | structured logging |
| `rml_rtmp` | 0.8 | MIT | ✅ | RTMP protocol, publishing |
| `bytes` | 1.x | MIT | ✅ | zero-copy buffers |

## Build-time only (not distributed)

| Dependency | Version | Licence | OK |
|---|---|---|---|
| `typescript` | ^5.7 | Apache-2.0 | ✅ |
| `@types/node` | ^22 | MIT | ✅ |
| `@types/ws` | ^8.5 | MIT | ✅ |

## Planned, and their licence position

| Dependency | Licence | OK | Notes |
|---|---|---|---|
| `wgpu` | MIT / Apache-2.0 | ✅ | GPU compositor |
| Tauri v2 | MIT / Apache-2.0 | ✅ | desktop shell |
| FFmpeg (LGPL build) | LGPL-2.1+ | ⚠️ | must be **our own build**, no `--enable-gpl`, dynamically linked |
| NVIDIA Video Codec SDK | NVIDIA SDK terms | ✅ | NVENC / NVDEC |
| NDI SDK | proprietary, free tier | ⚠️ | register with Vizrt before shipping an NDI output |
| VST3 SDK | GPL-3 **or** Steinberg proprietary | ⚠️ | take the **proprietary** option; free, requires registration |
| CameraX (Android) | Apache-2.0 | ✅ | |
| AVFoundation (iOS) | Apple SDK terms | ✅ | |

⚠️ = usable, but requires a deliberate action (our own build, or registering for
a licence) before it can ship. Do not let one of these reach a release branch
without that action completed.

## Explicitly rejected

| Dependency | Why |
|---|---|
| OBS Studio (any code) | GPL-2.0 — would force Rhevia to be GPL and unsellable |
| x264 | GPL-2.0 — use NVENC / QuickSync / AMF instead |
| x265 | GPL-2.0 — same |
| Stock FFmpeg Windows builds (gyan, BtbN) | built with `--enable-gpl`; fine for local development, must never be redistributed |
| `libfdk-aac` | licence is not GPL-compatible and carries separate patent obligations |
