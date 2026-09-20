---
name: Obsidian Broadcast
colors:
  surface: '#0f131c'
  surface-dim: '#0f131c'
  surface-bright: '#353943'
  surface-container-lowest: '#0a0e17'
  surface-container-low: '#181c25'
  surface-container: '#1c2029'
  surface-container-high: '#262a33'
  surface-container-highest: '#31353e'
  on-surface: '#dfe2ef'
  on-surface-variant: '#e4beba'
  inverse-surface: '#dfe2ef'
  inverse-on-surface: '#2c303a'
  outline: '#ab8986'
  outline-variant: '#5b403e'
  surface-tint: '#ffb3ad'
  primary: '#ffb3ad'
  on-primary: '#68000a'
  primary-container: '#ff5451'
  on-primary-container: '#5c0008'
  inverse-primary: '#b91a24'
  secondary: '#4ae176'
  on-secondary: '#003915'
  secondary-container: '#00b954'
  on-secondary-container: '#004119'
  tertiary: '#4cd7f6'
  on-tertiary: '#003640'
  tertiary-container: '#009eb9'
  on-tertiary-container: '#002f38'
  error: '#ffb4ab'
  on-error: '#690005'
  error-container: '#93000a'
  on-error-container: '#ffdad6'
  primary-fixed: '#ffdad7'
  primary-fixed-dim: '#ffb3ad'
  on-primary-fixed: '#410004'
  on-primary-fixed-variant: '#930013'
  secondary-fixed: '#6bff8f'
  secondary-fixed-dim: '#4ae176'
  on-secondary-fixed: '#002109'
  on-secondary-fixed-variant: '#005321'
  tertiary-fixed: '#acedff'
  tertiary-fixed-dim: '#4cd7f6'
  on-tertiary-fixed: '#001f26'
  on-tertiary-fixed-variant: '#004e5c'
  background: '#0f131c'
  on-background: '#dfe2ef'
  surface-variant: '#31353e'
typography:
  headline-lg:
    fontFamily: Chivo
    fontSize: 1.5rem
    fontWeight: '700'
    lineHeight: 1.75rem
    letterSpacing: -0.02em
  headline-md:
    fontFamily: Chivo
    fontSize: 1.125rem
    fontWeight: '600'
    lineHeight: 1.375rem
    letterSpacing: -0.01em
  headline-sm:
    fontFamily: Chivo
    fontSize: 0.875rem
    fontWeight: '600'
    lineHeight: 1.125rem
    letterSpacing: 0.02em
  body-lg:
    fontFamily: Inter
    fontSize: 0.875rem
    fontWeight: '500'
    lineHeight: 1.25rem
  body-md:
    fontFamily: Inter
    fontSize: 0.75rem
    fontWeight: '400'
    lineHeight: 1rem
  body-sm:
    fontFamily: Inter
    fontSize: 0.6875rem
    fontWeight: '400'
    lineHeight: 0.875rem
  label-telemetry-lg:
    fontFamily: JetBrains Mono
    fontSize: 1.25rem
    fontWeight: '700'
    lineHeight: 1.25rem
    letterSpacing: 0.04em
  label-telemetry-md:
    fontFamily: JetBrains Mono
    fontSize: 0.8125rem
    fontWeight: '600'
    lineHeight: 1rem
    letterSpacing: 0.02em
  label-telemetry-sm:
    fontFamily: JetBrains Mono
    fontSize: 0.6875rem
    fontWeight: '500'
    lineHeight: 0.8125rem
    letterSpacing: 0.01em
  label-cap-micro:
    fontFamily: JetBrains Mono
    fontSize: 0.5625rem
    fontWeight: '700'
    lineHeight: 0.6875rem
    letterSpacing: 0.08em
rounded:
  sm: 0.125rem
  DEFAULT: 0.25rem
  md: 0.375rem
  lg: 0.5rem
  xl: 0.75rem
  full: 9999px
spacing:
  gutter: 0.375rem
  margin: 0.5rem
  space-xs: 0.125rem
  space-sm: 0.25rem
  space-md: 0.5rem
  space-lg: 0.75rem
  space-xl: 1rem
---

## Brand & Style

The design system is engineered for mission-critical live broadcast production, vision mixing, and real-time streaming operations. The visual language bridges industrial hardware switchers (physical broadcast decks, illuminated silicon push-buttons, rotary encoders) with modern software control surfaces. Designed specifically for low-light control rooms, master control facilities, and multi-monitor OB (Outside Broadcast) vans, the interface emphasizes zero-latency visual comprehension, spatial memory, and fail-safe operational clarity.

The aesthetic fuses **Precision Industrial Skeuomorphism** with **High-Density Utilitarian Modernism**:
- **Hardware Metaphor**: Tactile, beveled broadcast switcher keys that simulate illuminated LED matrix caps, precision-etched fader tracks, and anodized aluminum control dividers.
- **Fail-Safe Hierarchy**: Critical state signaling relies on unambiguous, standardized broadcast color conventions (Tally Red for Program/Live On-Air, Preview Green for Staged Sources, Electric Cyan for Bus Routing/Selection, and Amber for System Warnings/Buffer Depletion).
- **Zero Eye-Strain Density**: Deep multi-tiered obsidian and slate surfaces isolate interactive regions without bright halos, maintaining contrast integrity during grueling 12-hour live productions.

## Colors

The palette is engineered around standardized international broadcast switching conventions. The default mode is strictly dark; light mode does not exist in this design system to protect operator night vision in studio environments.

### System Surface Tiers
- **Canvas Base (`#0B0D13`)**: Pure void obsidian. Used exclusively for monitor viewport backgrounds, letterbox surrounds, and outer window boundaries.
- **Surface Low (`#12151D`)**: Recessed chassis tier for inactive audio meter wells, timeline beds, and inactive video bins.
- **Surface Mid (`#161A23`)**: Standard panel substrate, rack enclosures, and toolbar backings.
- **Surface High (`#1F2430`)**: Elevated modules, modal dialogue surfaces, channel strips, and inspector drawers.
- **Surface Highlight (`#2A3040`)**: Hover states, active drag zones, and panel headers.

### Signal State Accents
- **Primary / Program (PGM) (`#EF4444`)**: Transmitting Live On-Air. Highest visual priority. Used for tally halos, active cut bus buttons, and recording indicators. Accompanied by a 25% alpha underglow (`rgba(239, 68, 68, 0.25)`).
- **Secondary / Preview (PVW) (`#22C55E`)**: Staged next-source bus. Signals readiness for transition (Cut/Auto/Wipe).
- **Tertiary / Auxiliary Routing (`#06B6D4`)**: Studio electric cyan. Used for M/E bus assignments, upstream/downstream keyers (USK/DSK), macro triggers, and active NDI/SDI routing locks.
- **Telemetry Amber (`#F59E0B`)**: Audio clipping warning (-2 dBFS to 0 dBFS), frame drops, sync slip, and thermal throttling.
- **Signal Neutral (`#8E98B0`)**: Inactive switch caps, unselected labels, and passive track lines. High-contrast white (`#F8FAFC`) is reserved exclusively for live digital readouts and active channel text.

## Typography

Typography is split into three deliberate disciplines:
1. **Structural Headers (Chivo)**: High-density, geometric sans-serif engineered for control surface section names, multiviewer quadrant labeling, and macro bank IDs.
2. **Operational Body (Inter)**: Utilitarian grotesque with micro-scale legibility. Configured with tabular numeric features (`tnum`) enabled globally to prevent spatial jumping during data stream updates.
3. **Telemetry & Readouts (JetBrains Mono)**: Monospaced numeric system with slash-zero formatting for SMPTE timecodes (`HH:MM:SS:FF`), genlock phase variance, audio decibel full scale (`dBFS`), hardware bitrate/FPS counters, and network socket addresses.

### Numeric Display Rules
- All counters (frames, timecodes, bitrates) must use `JetBrains Mono` with fixed-width figures to ensure absolute positional lock during real-time 60fps increments.
- Hardware labels inside physical button matrix caps use `label-cap-micro` in all caps with expanded letter spacing for legibility when illuminated.

## Layout & Spacing

Live broadcast production mandates maximized screen real estate. The layout model utilizes a zero-waste, rigid grid system with micro-gutters (2px to 6px) to maximize preview tile area and channel density.

### Layout Model
- **Multiviewer Master Grid**: Fixed aspect ratio cells (16:9) lock to the top canvas half, utilizing an auto-snapping fluid column matrix (2x2, 4x4, 8x8, or custom 1+7 layouts).
- **Switcher Deck Split**: The lower half of the workspace houses the tactile cross-point (XPT) switcher, audio mixer, and transition engines with fixed-height rows (36px to 48px).
- **Docking Enclosures**: All panels dock into collapsible, nested splitter frames. Panes never overlap or float freely unless dragged onto a discrete physical monitor window.

### Breakpoints & Multi-Screen Matrix
- **Desktop Primary (1920x1080 / 1080p)**: Baseline reference. Minimum supported resolution. 12-channel switcher bus visible simultaneously without scrolling.
- **Broadcast Ultra-Wide / 4K (2560x1440 to 3840x2160)**: Expands multiview quadrant sizes and unhides secondary audio equalization strips and DSK routing racks.
- **Dedicated Aux Display**: Standalone undocked window protocol designed to push pure fullscreen clean-feed multiviewers to auxiliary displays with zero chrome.

## Elevation & Depth

Visual hierarchy is communicated via mechanical depth, tactile recessing, and localized LED backlighting rather than soft drop shadows.

### Mechanical Elevation Tiers
1. **Recessed Wells (`inset 0 1px 3px rgba(0, 0, 0, 0.8)`)**: Applied to audio fader gutters, VU meter slots, and inactive preview thumbnail wells. Creates the perception of cutouts inside a heavy metal faceplate.
2. **Flat Ground Deck (`#161A23`)**: Zero shadow. The structural brushed chassis that unifies all controls.
3. **Elevated Switcher Buttons**: Defined by a razor-thin top highlight (`inset 0 1px 0 rgba(255, 255, 255, 0.12)`) and a bottom drop edge (`0 2px 4px rgba(0, 0, 0, 0.6)`), evoking physical dual-shot plastic push-caps.
4. **Illuminated Active State (The Broadcast Glow)**: Active switches (Cut, Auto, Trans, PGM) discard drop shadows in favor of a tight 3px peripheral perimeter glow and an internal translucent color fill matching their tally state:
   - PGM: `0 0 12px rgba(239, 68, 68, 0.45), inset 0 0 6px rgba(239, 68, 68, 0.6)`
   - PVW: `0 0 12px rgba(34, 197, 94, 0.45), inset 0 0 6px rgba(34, 197, 94, 0.6)`
   - AUX: `0 0 12px rgba(6, 182, 212, 0.45), inset 0 0 6px rgba(6, 182, 212, 0.6)`

### Edge Definition
All panels, modules, and inputs feature a continuous 1px low-contrast border using `#2A3040`. Active focuses shift this border to `#06B6D4` without changing element dimensions.

## Shapes

The design system utilizes an industrial mechanical radius scale:
- **Base Components (`roundedness: 1` / `0.25rem` / `4px`)**: Applied to push buttons, input fields, selector toggles, and multiview video tiles. This micro-curve mirrors the edge profile of machined hardware switch caps while preserving rectilinear grid density.
- **Inner Indicators (`0.125rem` / `2px`)**: Used for VU meter segments, fader track caps, tally pills, and sub-badges.
- **Panel Chassis (`0.25rem` / `4px`)**: Module panels and dock containers maintain strict structural bounds with 4px corner radii.
- **Pill Profiles**: Forbidden across general UI; rounded pills are restricted exclusively to live status tokens (e.g., `REC`, `LIVE`, `STREAM`) to make them immediately distinct from square button matrices.

## Components

### Broadcast Push Buttons (Crosspoint Switchers)
- **Structure**: Rectangular or square tactile keys with dual-line labels (Source Index + Source Name).
- **Idle State**: Background `#1F2430`, border `1px solid #2A3040`, text `#8E98B0`. Subtle top bevel highlight.
- **Program Active (Tally Live)**: Background `#7F1D1D`, border `1px solid #EF4444`, text `#FFFFFF`. High-intensity red tally halo.
- **Preview Active**: Background `#14532D`, border `1px solid #22C55E`, text `#FFFFFF`. Green tally halo.

### Audio dBVU Fader Strips
- **Meter Track**: Vertical recess (`#0B0D13`) containing segmented LED bars:
  - `-60 dB to -18 dB`: Solid Signal Green (`#22C55E`).
  - `-18 dB to -2 dB`: Nominal Amber (`#F59E0B`).
  - `-2 dB to +6 dB`: Peak Clip Red (`#EF4444`) with 1.5-second peak-hold tick.
- **Physical Slider Knob**: Machined brushed metal cap (`#2A3040`) with a center optical reference line (`#F8FAFC`).

### Rotary Encoders (Gain / Pan Knobs)
- 32px radial dials with continuous track indicators.
- Inactive track `#1F2430`; active filled arc `#06B6D4`.
- Center readout reveals absolute value on hover/drag using `label-telemetry-sm`.

### Multiview Video Tiles
- **Frame**: 16:9 ratio with internal 2px inset tally border (red for PGM, green for PVW, transparent for idle).
- **Overlay Bar**: Anchored to the bottom of the video frame. Opaque `#0B0D13` with 80% opacity. Displays input index, source label, audio presence icon, and dropped frame warning.

### T-Bar Transition Fader
- Heavy vertical track with stepped detents at 0%, 50%, and 100%.
- Fader handle features knurled industrial grip texture with dynamic position percentage in `JetBrains Mono`.

### Precision Inputs & Selectors
- **Numeric Fields**: Tabular, dark background (`#12151D`), right-aligned monospace values with scrub-on-drag capability.
- **Segmented Busses (M/E Selection)**: Zero-gap joined button strips where the active bus segment illuminates with a `#06B6D4` top border accent.