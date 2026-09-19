# Licensing constraints

> Not legal advice. Before shipping commercially, have a lawyer review this.
> The point of this document is that these decisions are **cheap now and
> extremely expensive later** — they determine what code you are allowed to read.

## The OBS question

**OBS Studio is GPLv2.** If Rhevia is a derivative work of OBS, Rhevia must be
released under GPLv2, with source available to every user.

That is incompatible with the vMix business model (sell perpetual licences for a
closed binary). So:

- ✅ Run OBS, study its *behaviour*, read its docs, learn which problems it solves.
- ✅ Read the GPL source for education — understanding concepts is not infringement.
- ❌ Copy code, translate it line by line, or structure Rhevia as an OBS fork.

If you would rather go open source, that is a legitimate strategy too — but pick
deliberately, because it cannot be reversed once GPL code is in the tree.

## Dependency licence map

| Component | Licence | Commercial closed-source? |
|---|---|---|
| **FFmpeg** (LGPL build) | LGPL-2.1+ | ✅ if dynamically linked, unmodified |
| **FFmpeg** (GPL build) | GPL-2.0+ | ❌ — this is what is installed locally |
| **x264** | GPL-2.0+ | ❌ unless commercial licence from x264 LLC |
| **x265** | GPL-2.0+ | ❌ unless commercial licence from MulticoreWare |
| **NVENC / NVDEC** | NVIDIA SDK terms | ✅ — the encoder is in the driver |
| **libwebrtc** | BSD-3-Clause | ✅ |
| **webrtc-rs** | MIT / Apache-2.0 | ✅ |
| **SRT** | MPL-2.0 | ✅ file-level copyleft only |
| **Opus** | BSD-3-Clause | ✅ |
| **NDI SDK** | proprietary, free tier | ✅ under Vizrt's SDK agreement |
| **NDI Advanced SDK** (HX encode) | proprietary, paid | ✅ once licensed |
| **Qt** | GPL / LGPL-3 / commercial | ✅ LGPL if dynamically linked |
| **VST3 SDK** | GPL-3 / Steinberg proprietary | ✅ under Steinberg's agreement (free, requires registration) |

### Codec patents are separate from software licences

This trips people up constantly. A permissive software licence does not grant
patent rights to the codec itself.

- **H.264 / H.265** — patent pools (Via LA, Access Advance). Distributing an
  encoder or decoder commercially above volume thresholds incurs royalties.
  Using NVENC does not automatically clear the distributor's obligation.
- **AAC** — patent licensing via Via LA. `libfdk-aac` additionally has a licence
  that is *not* GPL-compatible.
- **AV1 / Opus / VP9** — royalty-free by design. Prefer these where the delivery
  platform accepts them.

## Decision (2026-09-19)

**Personal use now, with the option to sell later.** That option is only worth
something if the tree stays clean from the first commit. There is no way to
"clean up the licensing later" — if GPL code is in the tree, the only path to a
sellable product is rewriting everything it touched, by someone who never read it.

So the rules below apply **now**, not at the point of sale:

- No OBS source in the tree, ever. Behaviour and docs only.
- Every dependency must be permissive (MIT / Apache-2.0 / BSD / MPL) or LGPL
  dynamically linked. Recorded in `docs/DEPENDENCIES.md` with its licence, at
  the moment it is added.
- Patent-bearing codecs (H.264, AAC) come from hardware encoders or OS
  frameworks, never bundled software encoders.

## Practical recommendation for a commercial Rhevia

1. Build or obtain an **LGPL FFmpeg** (no `--enable-gpl`, no x264/x265), link it
   dynamically, and ship the licence text plus a written offer for the source of
   the FFmpeg you used.
2. Encode with **NVENC / QuickSync / AMF** as the primary path. Hardware encoders
   sidestep the x264 problem entirely and are faster anyway.
3. Keep a software-encode fallback behind an **optional, user-installed** plugin
   so the GPL component is never distributed by you.
4. Register for the **NDI SDK** and the **VST3 SDK** early. Both are free but
   require accepting terms, and the NDI Advanced SDK negotiation takes time.
5. Budget for H.264/AAC patent licensing before revenue, not after.
