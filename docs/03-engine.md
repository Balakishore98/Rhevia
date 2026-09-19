# Engine architecture

Three of your requirements are really one requirement: **never block the frame
clock.** Unlimited inputs, heavy audio DSP, and editing a file while it records
all fail in the same way — something takes a lock the renderer needed.

## Video: "unlimited inputs based on system capacity"

The number of inputs is not the cost. **Decoding and compositing them is.**
vMix and OBS both slow down because every input decodes at full rate whether or
not anyone can see it. Rhevia should not do that.

### Frames never leave the GPU

```
NVDEC decode ──► D3D12/Vulkan texture ──► compositor ──► NVENC encode
                        (no readback to system RAM anywhere)
```

A readback to system memory is ~5-10 ms of stall per surface. Do it 20 times a
frame and you are finished at any resolution. Readback happens **only** for the
preview thumbnails the UI shows, and those go through a separate low-priority
path at 10-15 fps, never at program rate.

### Decode tiers

Each input sits in exactly one tier, re-evaluated when the scene changes:

| Tier | Condition | Decode rate |
|---|---|---|
| `Live` | on Program or Preview | full rate, full res |
| `Visible` | in a multiview or a layer that is on screen | full rate, proxy res |
| `Warm` | in the next scene, or recently used | keyframes only, ~2 fps |
| `Cold` | loaded but unreferenced | paused, first frame held |

Promotion `Cold → Live` must be seamless, so `Warm` exists to keep a decoder
primed for anything that could plausibly cut to air next. This is the single
biggest reason Rhevia can hold input counts that make vMix stutter.

### Threading

- One ingest thread per input, writing into a **lock-free SPSC ring buffer**.
- One render thread, pulling the newest ready frame at vsync. It never waits on
  an input. A late input shows its previous frame; it does not drop the show.
- Encode and file I/O on their own threads with their own queues.

### Known hard limit: NVENC sessions

Consumer NVIDIA cards cap concurrent NVENC sessions (historically 3, raised to 8
on current drivers). This constrains **outputs**, not inputs. Encode once and
remux to multiple destinations rather than encoding per destination.

## Audio

Your list — bass, echo, sharp, high-mid, low-mid — maps onto a standard chain.
Ship both a simple tone UI and the full parametric version underneath.

### Per-input chain

```
input ─► gate ─► EQ ─► compressor ─► de-esser ─► limiter ─┬─► Master bus
                                                           ├─► Aux 1 (mix-minus)
                                                           ├─► Aux 2 (headphones)
                                                           └─► sends ─► reverb / delay
```

- **EQ** — 8-band parametric. Low shelf ("bass"), low-mid, mid, high-mid,
  presence ("sharp"), high shelf, plus HPF and LPF. Implement with TPT state
  variable filters, not naive biquads: SVFs stay stable when you sweep the
  cutoff live, biquads can blow up.
- **Echo** — delay line with feedback and tempo sync, plus a separate algorithmic
  reverb on a send (not an insert — one reverb serving many inputs).
- **Host VST3.** This is the decisive advantage. vMix's audio plugin support is
  thin; hosting VST3 gives users every professional plugin that exists on day one.
  Run plugins in a **separate sandbox process** so a badly written third-party
  plugin cannot crash a live show.

### Audio thread rules

Audio runs at 64-256 sample buffers on a dedicated high-priority thread. On that
thread: no allocation, no locks, no file I/O, no logging. Parameter changes
arrive by lock-free queue and are smoothed over the buffer to avoid zipper noise.

## Recording, live editing, and shorts

You asked for the software to read and edit the recording *while it is still
recording*. That works if the container is chosen correctly.

### Record format

Write **fragmented MP4** (`+frag_keyframe+empty_moov+default_base_moof`) or
segmented MPEG-TS. Both are readable while being written. A standard MP4 is not —
its index lives at the end of the file and does not exist until you stop.

### Record a proxy at the same time

| Stream | Purpose | Format |
|---|---|---|
| Master | archive, final export | 1080p/4K, NVENC, high bitrate |
| Proxy | instant scrubbing | 540p, all-intra, low bitrate |
| Sidecar | seeking | keyframe offsets + timestamps, appended live |

Scrubbing an all-intra proxy is instant at any point. Edit against the proxy,
conform to the master on export. This is how broadcast post works and it is why
the timeline feels immediate instead of stuttering.

### Shorts pipeline

1. **Mark live** — a hotkey during the show drops an in/out marker. The operator
   flags the good moment as it happens, instead of hunting for it later.
2. **Auto-reframe** 16:9 → 9:16 with subject tracking, so the speaker stays
   framed rather than being centre-cropped out of shot.
3. **Auto-caption** from the recorded audio.
4. **Queue and publish** to YouTube / TikTok / Instagram via their APIs.

vMix has instant replay. It does not have this. This is a product, not a feature.

## Graphics: lower thirds and tickers

Build the title system on a **scene graph with keyframed properties**, not a
fixed set of title templates.

- Data binding to live sources: CSV, Google Sheets, HTTP/JSON polling, webhooks.
- A ticker is then just a text layer bound to a data source with a scroll
  behaviour — not a special-cased widget.
- Render titles in the same GPU compositor as video so they composite for free.
- Ship an **HTML/CSS layer** too. Designers already know those tools, and it
  makes the template market self-serving instead of dependent on you.
