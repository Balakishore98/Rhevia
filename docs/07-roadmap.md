# What is pending for streaming

Written against the repository as it stands, not against intentions.

## The pipeline, and where we actually are

A live production switcher is six stages. We have built part of one.

```
  SOURCE   →   DECODE   →   COMPOSITE   →   ENCODE   →   MUX   →   DELIVER
    ~            ✗             ✗             ✗          ✓          ✓
    │                                                   └──────────┘
    └── RheviaLink transport works.              proven against a real RTMP
        No source produces pixels yet.           server: 60 frames sent,
                                                 60 decoded by ffprobe.
```

**Updated:** the output half is done. `rhevia-stream` publishes H.264 to any
RTMP destination, paced to real time, and the round trip is verified by an
independent implementation rather than by our own assertions. The middle of the
pipeline can now be built against an output that is known to work.

| Stage | State | What exists |
|---|---|---|
| **Source** | partial | RheviaLink pairs, negotiates and receives H.264 RTP. No phone app to *be* a camera. No local webcam, capture card, screen capture, or media file. |
| **Decode** | none | We count RTP packets. They are not depacketised into Annex-B, let alone decoded into frames. |
| **Composite** | none | No GPU pipeline, no scene graph, no Program/Preview, no transitions, no layers. |
| **Encode** | none | NVENC is present on the machine and unused. |
| **Mux** | **done** | FLV tag muxing for H.264 and AAC, including the decoder configuration record and keyframe flagging. |
| **Deliver** | **done** | RTMP publishing: handshake, connect, publish, real-time pacing. Verified end to end against a real server. SRT still to do. |
| **Audio** | none | Nothing at all: no capture, no mixing, no DSP, no A/V sync. |
| **Recording** | none | No fragmented MP4 writer, no proxy, no shorts pipeline. |
| **UI** | none | No Tauri app. Everything so far is a library plus tests. |

So: **the transport for one input type is done. The mixer is not started.**

That is not a discouraging position — transport was the riskiest third, and it
is the part that makes the product worth building. But the remaining work is
most of the work, and planning against "streaming is nearly there" would be
wrong.

## The shortest path to a first real stream

There is a useful shortcut worth taking before building the mixer.

**Passthrough first.** Take the H.264 the phone already sends, depacketise it,
and remux straight to RTMP without decoding or re-encoding:

```
phone → RTP → depacketise → Annex-B → FLV mux → RTMP → YouTube
```

That is a genuinely live stream, reachable in a fraction of the work, and it
proves the entire output half — mux, RTMP handshake, platform ingest quirks,
reconnection, bitrate behaviour — while the compositor does not exist yet.
Decode, composite and encode then get inserted in the middle of a pipeline that
is already known to deliver.

Doing it the other way round means building the compositor with no way to see
whether anything downstream works.

## Ordering, and why

### 1. Depacketise RTP into Annex-B — small
Three packetisation modes have to be handled: single NAL, STAP-A aggregation,
and FU-A fragmentation. Skipping FU-A appears to work on a LAN and then fails
on a phone, because fragmentation only kicks in past the MTU.

### 2. ~~RTMP output~~ — done
Built on `rml_rtmp` (MIT). Handshake, connect, publish, FLV muxing and
real-time pacing, proven against a real RTMP server.

Still missing here: **reconnection with backoff**. Platform ingests drop
connections routinely and a live show must not end because of one. Also
`rtmps://`, which needs TLS and is currently rejected with a clear error rather
than silently downgraded.

### 3. Phone app — large
Nothing is a real source until this exists. CameraX on Android, AVFoundation on
iOS, hardware encode, WebRTC sender, plus the features that justify the product:
tally, remote lens control, and device-side ISO recording.

### 4. Decode — medium
NVDEC through the Video Codec SDK, output straight into a D3D11/Vulkan texture.
The rule from [`03-engine.md`](03-engine.md): never read back to system RAM.

### 5. Clock and A/V sync — medium, and routinely underestimated
Video RTP timestamps run at 90 kHz, audio at the sample rate, and neither maps
to wall clock without RTCP sender reports. A mixer also needs a master clock:
inputs arrive irregularly, output must be regular. Getting this wrong produces
drift that only shows up twenty minutes into a show, which is the worst possible
time to discover it. It is much cheaper to design in than to retrofit.

### 6. Compositor — large
wgpu render graph, texture per input, scene graph, Program/Preview, transitions,
and the [tiered decode scheduler](03-engine.md#decode-tiers) that makes high
input counts work.

### 7. Encode — medium
NVENC, low-latency preset for stream and quality preset for record. Note the
session cap on consumer cards: encode once and remux per destination rather than
encoding per destination.

### 8. Audio — large
Capture (WASAPI via `cpal`), resampling, the per-input DSP chain, bus routing,
sandboxed VST3 hosting, and synchronisation against the video clock. This is a
whole subsystem, not a feature.

### 9. UI — large
Tauri shell, and the [command bus](06-command-bus.md) underneath it so remote
control, scripting and MIDI all come free rather than each being retrofitted.

## Things that will cost more than they look

- **A/V sync.** See above. Design it before the compositor, not after.
- **Colour.** Limited vs full range, BT.709 vs BT.601. Get it wrong and
  everything is subtly washed out in a way users report as "looks wrong" without
  being able to say why.
- **RTMP in practice.** Every platform has its own quirks around keyframe
  interval, audio requirements and reconnect behaviour.
- **NVENC session limits.** Constrains outputs, not inputs. Architect for one
  encode with multiple remuxes.
- **LGPL FFmpeg.** The build installed here is GPL and cannot ship. Producing a
  clean LGPL build is a real task with a real cost, and it blocks the first
  shippable binary — see [`02-licensing.md`](02-licensing.md).

## Honest summary

Streaming needs roughly eight more subsystems, three of them large. What is
finished is the hardest *novel* part — moving a camera across the internet with
pairing, NAT traversal, resume and abuse resistance. What remains is mostly
well-understood work, but there is a great deal of it.

The next decision is only whether to prove the output half cheaply via
passthrough, or to start on the phone app so there is a real camera to point at
it.
