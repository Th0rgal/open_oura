# open_oura — iOS app · Goal

A native iOS client to the same local Rust core as the web dashboard. Same data,
same numbers — a **calm, instrument-grade** reading of your ring, with a quiet
data-science aesthetic. Everything on-device; nothing leaves the phone.

## Design language — "Observatory"

Inspired by Flywheel: deep-black canvas, thin vector traces, dashed concentric
arcs, ringed nodes, sparse monospace micro-labels, lots of negative space. The
night and its baselines read like an **orbital plot**, not a dashboard of cards.

- **Canvas** pure black `#000` → near-black `#0A0A0B`. No panels, no boxes — data
  floats on the void; hierarchy comes from space and weight, not borders.
- **Ink** `#EDEDED` primary · `#8A8A8E` secondary · `#46464A` structural traces.
- **Accent, rare** signal-teal `#4FD0C5` (in-range / good) · signal-yellow
  `#E6C04D` (the one threshold to notice). One accent per screen, max.
- **Type** SF Pro for prose, **SF Mono** for every number, axis and tag. Titles
  set small, lowercase, a touch of italic ("scheduled muon" voice).
- **Structure = polar.** Baselines are dashed reference rings; a metric's night is
  a trace growing outward; events are small ringed nodes on the arc.
- **Motion** slow draw-on of traces (the graph *grows*), spring physics, 120 fps
  ProMotion. No bounce, no chrome. Light haptic on sync + threshold crossings.
- Auto light/dark, but dark is the hero. Respects Reduce Motion / Dynamic Type.

### Signature element — "Night Orbit"
One reusable radial view: concentric **dashed baseline rings**, a **growing trace**
for the selected signal across the night, ringed **event nodes**, a faint sweep
hand. Powers the hero on Today, the hypnogram on Sleep, and the actogram on
Activity — one visual idea, three uses.

## Feature parity with the web dashboard

Renders the **same `build_summary()` JSON** — the shared contract. Screens map to
the existing panels:

1. **Today / Digest** — the one-line digest; Night Orbit hero; vitals as orbiting
   nodes (HRV, resting HR, skin-temp Δ, SpO₂) each against its baseline ring.
2. **Sleep** — hypnogram (Night Orbit or refined stage band), TST, efficiency,
   timing/regularity, stage breakdown, sleep debt.
3. **Cardiovascular** — resting HR & HRV trends with baseline bands; cardiovascular
   age (CVA) as a single quiet gauge.
4. **Blood Oxygen** — nightly SpO₂ % (Oura R→% calibration) + distribution.
5. **Activity** — actogram (polar), automatic sessions, steps / calories, daily.
6. **Device & Data Health** — battery, sync freshness, streams, event counts.

**Actions:** Sync over BLE (pull-to-refresh + header control) · Profile editor
(age/sex/height/weight/ring size) · feature toggles · ring-key entry.

## Architecture (already validated — see [ios-client-spike])

- **SwiftUI** app under `apps/ios/`. Charts hand-drawn in Canvas/Swift Charts —
  no third-party UI deps (keeps the look bespoke and offline).
- **Shared Rust core** via `oura-ffi` → `oura-core` (UniFFI `.xcframework`):
  `oura-protocol` · `oura-analysis` · `oura-store` · `oura-link` reused as-is;
  `build_summary()` is the single source of truth shared with the web client.
- **BLE**: CoreBluetooth implements the `oura-link::Transport` trait (native
  permissions/background); auth + sync logic stays in Rust.
- **ML models**: TorchScript `.ptl` (lite interpreter, bit-exact vs `.pt`) run via
  a Swift torch runner that returns the **same `--json`** the Python runners do —
  the model seam is unchanged, so web and iOS never diverge.

## Done = 
Feature-parity with the web dashboard, the Observatory look applied throughout,
running fully on-device on a real ring's data, sharing the Rust core so a web
change in `build_summary()` flows to iOS with no re-implementation.

## Non-goals (v1)
Cloud sync · accounts · Apple Health export · Android · live realtime (`viz`/
`game`) — all later. v1 is the offline dashboard, done beautifully.

## Status (foundation built & verified on the iOS 26.4 simulator)
- ✅ **`oura-core` UniFFI `.xcframework`** — `crates/oura-core` exposes `summary_json`/
  `quick_summary_json`/`rmssd`; bindings generated, `apps/ios/OuraCore.xcframework` built.
- ✅ **Shared summary** — `build_summary()` extracted to `crates/oura-summary` behind a
  `ModelRunner` trait; web (`oura-cli`, `PythonRunner`) and iOS (`NoModelRunner`) share it.
- ✅ **SwiftUI Observatory UI** (`apps/ios/OuraApp/`) — renders the real shared summary
  (digest, vitals + sparklines, sleep timing, device health) from a bundled `oura.db`.
- ✅ **CoreBluetooth Transport** — `oura-link` btleplug feature-gated so auth/sync compile
  for iOS; `BLETransport.swift` implements the ring link (type-checks).
- ⏳ **Remaining (needs a ring / larger):** UniFFI async `Transport` callback + `sync()`
  entry to drive `OuraClient` on device; on-device torch `.ptl` as a UniFFI `ModelRunner`
  (sleep stages / CVA / activity); multi-screen nav once model data lands.

Build: `tools/export_mobile.py` (models) · `apps/ios/OuraApp/build_run.sh` (app on sim) ·
`apps/ios/spike/build_libtorch_ios.sh` (on-device torch runtime).
