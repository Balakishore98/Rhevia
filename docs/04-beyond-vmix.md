# Beyond vMix

Grounded in the vMix 29 User Guide (236 pages, 182 documented features), not in
impressions. Page references are to that document.

The goal is not "vMix with a nicer skin." It is to match the table stakes and
then win decisively in places vMix structurally cannot follow.

---

## Part 1 — Table stakes

None of this is a differentiator. All of it is required before anyone will
switch, because a switcher that cannot do these is not a switcher.

| Area | What vMix has | Notes for us |
|---|---|---|
| Inputs | Video, DVD, List, Camera, NDI, Desktop Capture, Stream/SRT, Instant Replay, Image Sequence/Stinger, Video Delay, Image, Photos, PowerPoint, Colour, Audio, Title/XAML, Virtual Set, Web Browser, Video Call, Mix | Match the list. Drop DVD. |
| Switching | Cut, Fade, Merge, Stinger, FTB, Quick Play, fade bar, overlays 1-4 | Table stakes |
| Compositing | Layers, MultiView, position, crop, zoom | Table stakes |
| Keying | Colour key, colour correct, colour adjust | Must beat — see Part 2 |
| Audio | Full mixer, per-input EQ / compressor / noise gate, buses, ASIO, **VST3 on inputs and buses** (p91) | vMix is strong here |
| Recording | MP4 SD/HD/4K, FFmpeg, fault-tolerant, second recorder, WAV | Table stakes |
| Streaming | Multi-bitrate, multiple destinations, independent control | Table stakes |
| Control | Keyboard, MIDI, controllers, Activators, Web Controller, vMix Control Surface | Table stakes |
| PTZ | Virtual inputs, mouse, shortcuts, joysticks | Table stakes |
| Titles | GT Title Designer, Data Sources (CSV, Sheets, XML, HTTP) | Must beat — see Part 2 |
| API | HTTP Web API, TCP API | Table stakes |
| Replay | Instant Replay, MultiCorder | **4K/Pro editions only** |
| Monitoring | Waveform, vectorscope, safe areas, statistics | Table stakes |

**Be honest about this list.** It is roughly two years of work before Rhevia is
merely *comparable*. The differentiators below are what make that work worth
doing — but they do not substitute for it.

---

## Part 2 — Where we actually win

Ranked by how hard it would be for vMix to respond.

### 1. RheviaLink — remote cameras (very hard for vMix to match)

vMix Call: guests join **from a web browser with a webcam**, capped at **HD
video**, and licence-gated to **1 guest (HD), 4 (4K), 8 (Pro)** (p177).

| | vMix Call | Rhevia |
|---|---|---|
| Client | browser + webcam | **native phone app** with full camera control |
| Resolution | HD maximum | 4K, limited by the phone |
| Guest count | 1 / 4 / 8 by licence tier | limited by bandwidth, not by a price list |
| Tally to the remote operator | ✗ | **✓ red/green on the phone screen** |
| Remote lens control | ✗ | **✓ focus, exposure, ISO, WB, zoom, torch** |
| Full-quality local recording on the remote device | ✗ | **✓ phone records clean while streaming compressed** |
| Gap repair after a dropout | ✗ | **✓ backfilled into the archive from phone storage** |

The last two are the ones that change what is possible. A remote camera whose
archive is pristine regardless of link quality is not a "video call" — it is a
second camera, and it lets one person shoot a multi-camera show from a phone in
their pocket. vMix Call is architecturally a conferencing feature; retrofitting
device-side ISO recording and lens control into it is close to a rewrite.

See [`01-remote-camera.md`](01-remote-camera.md).

### 2. Live-to-shorts pipeline (no vMix equivalent)

vMix has Instant Replay and MultiCorder, both **4K/Pro only** (p8), and both aimed
at sports replay — getting a moment back on air seconds later.

Neither produces a publishable vertical clip. Rhevia's pipeline does:

```
mark live (hotkey)  →  cut from the all-intra proxy  →  auto-reframe 16:9 → 9:16
                    →  auto-caption  →  publish to YouTube / TikTok / Instagram
```

Editing a file *while it is still recording* is the enabling trick, and it is a
container-format decision (fragmented MP4 plus a live proxy and keyframe
sidecar), not a feature that can be bolted on later. See
[`03-engine.md`](03-engine.md#recording-live-editing-and-shorts).

For most streamers this is worth more than the switcher itself — the show is
one artifact, the clips are what actually reach new viewers.

### 3. Performance at high input counts

vMix decodes every input at full rate regardless of visibility. Rhevia's
[tiered decode scheduler](03-engine.md#decode-tiers) drops unseen inputs to
keyframes or pauses them, so input count scales with the machine.

This is the difference between "unlimited inputs" as a marketing line and as a
thing that is actually true at input 40.

### 4. Cross-platform

vMix is Windows-only, and deeply so — the manual references Windows display
settings and DirectShow codecs throughout. Choosing Rust + wgpu means Mac and
Linux are a port, not a rewrite. Every Mac-based studio is unavailable to vMix
today.

### 5. Scripting that a working developer would accept

vMix scripting is **a subset of VB.NET 2.0** — no custom classes, no structures,
single sub or function only (p201). That is not an automation platform; it is a
macro language from 2005.

Rhevia: TypeScript or Python, real modules, npm/pip packages, a debugger,
and the same API surface the UI uses so anything clickable is scriptable.

### 6. Titles designers already know how to make

vMix has GT Title Designer, a proprietary editor, plus Data Sources bound to
CSV / Google Sheets / XML / HTTP. The data binding is good and we should match
it. The editor is the weak point.

Rhevia renders an **HTML/CSS layer** in the same GPU compositor. A motion
designer uses the tools they already have, templates become a market we do not
have to supply ourselves, and a ticker is a bound text layer rather than a
special-cased widget.

### 7. No feature tiers on core function

Instant Replay, MultiCorder and most vMix Call guests are gated behind the 4K
and Pro editions. Every time a user hits one of those walls, that is an opening.

### 8. Audio reliability (incremental, not decisive)

**Correction to an earlier assumption: vMix already hosts all 64-bit VST3
plugins on inputs and output buses (p91), and ships EQ, compressor and noise
gate built in.** Audio is a genuine vMix strength, not a weakness.

What remains for us is narrower but real:

- **Sandboxed plugin hosting.** vMix loads VST3 in-process; one bad plugin takes
  the show with it. We isolate them.
- **Deeper built-in chain** — de-esser, dynamic EQ, multiband compression,
  true-peak limiting.

Worth doing. Not worth leading the marketing with.

---

## What this means for sequencing

The table-stakes list is large and unavoidable, so the differentiators have to
carry the product early — there is no window where Rhevia wins on completeness.

That is the reasoning behind making RheviaLink milestone one: it is the feature
someone would switch for *before* the switcher is finished, and it is the one
that is hardest to copy once shipped.
