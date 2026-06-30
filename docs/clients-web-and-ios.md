# Two clients, one core — keep them in sync

open_oura has **two user-facing clients that render the same health data**. When you
add or change a feature, you almost always have to touch **both**. This is the map.

| | Web dashboard | Native iOS app |
| --- | --- | --- |
| Where | `dashboard/web/` (vanilla HTML/CSS/JS) served by `crates/oura-cli/src/dashboard.rs` | `apps/ios/OuraApp/` (SwiftUI) on `crates/oura-core` (UniFFI) |
| Entry | `oura dashboard` → `http://127.0.0.1:8090` | `apps/ios/OuraApp/build_run.sh` (model-free) / `build_run_torch.sh` (on-device models) |
| Render code | `app.js`, `styles.css`, `index.html` | `OuraApp.swift`, `Theme.swift` |
| Models run via | Python torch runners (`tools/run_*_model.py`) | on-device `.ptl` (`TorchBridge.{h,mm}` + `SleepStaging`/`CvaModel`/`ActivityModel.swift`) |

## The one shared brain: `crates/oura-summary`

`oura_summary::build_summary()` computes **the summary JSON both clients render** — vitals,
per-night stats, the digest, the MET activity profile, steps/kcal, device health. The web
calls it in `dashboard.rs`; iOS calls it through `oura-core`'s `summary_json()` FFI. The
models are injected via the `ModelRunner` trait (web: `PythonRunner`; iOS: `NoModelRunner`
+ the on-device torch code).

**So the rule of thumb:**

- **A new computed metric / field** → add it once in `oura-summary` (`build_summary`). Both
  clients receive it in the JSON. Then render it in **both** `app.js` and `OuraApp.swift`.
- **A new visualization / UI** (no new data) → do it in **both** `app.js` and `OuraApp.swift`.
- **A new model** → wire **both** runners: a `tools/run_*_model.py` (used by `PythonRunner`)
  **and** an `oura_*` function in `TorchBridge.mm` + a Swift `*Model.swift` that builds the
  same input tensors and folds the result into the summary.

## Feature ↔ feature correspondence

| Feature | Web (`app.js`) | iOS (`OuraApp.swift`) | Data (JSON key) | Model |
| --- | --- | --- | --- | --- |
| Digest headline | `load()` digest | `RootView` digest | `digest` | — |
| Vitals (HRV/RHR/temp/SpO₂) | `renderTiles` / `VitalCell`-like | `VitalCell` | `vitals`, `nights[]` | — |
| Sleep nights + **hypnogram** | `renderNights`, `hypnogram()` | `SleepRow`, `Hypnogram`, `SleepDetail` | `nights[].stages` | SleepNet (web: Python · iOS: `SleepStaging`) |
| Stage breakdown | `renderNights` breakdown | `StageBreakdown` | `nights[].{deep,light,rem,wake}_pct` | SleepNet |
| **Cardiovascular age** | `renderCardio` | Cardio section | `cardio` | CVA (web: Python · iOS: `CvaModel`) |
| Movement ridge / actogram | `renderActivity` ridge | `MovementRidge` | `activity_profile` | — (MET, model-free) |
| **Activity sessions / workouts** | `renderActivity` bars | workouts section | `activity` | AAD (web: Python · iOS: `ActivityModel`) |
| Steps / active calories | `renderActivity` stats | activity day stats | `activity_daily` | — |
| Device & data health | `renderDevice` | device section | `device`, `streams` | — |
| Long-list capping + "show all" | `collapsibleList()` | `MoreButton` | — | — |

## Where the two clients diverge

- **Home layout**: the iOS app is intentionally more condensed — "last night" (one night),
  a merged activity+workouts section, and a "show all days" page (`AllDaysView` →
  `DayDetailView`) that combines a day's sleep + activity. The web keeps the actogram +
  per-section lists. Match *data/features*, not pixel-for-pixel layout.
- **BLE sync**: iOS syncs **natively** — `RingSync.swift` (CoreBluetooth `BLETransport`)
  drives the Rust `RingSession` FFI (`oura-core`) to authenticate + drain into a writable
  DB. The web dashboard has **no** BLE; it reads a DB produced by the desktop `oura sync`.
  Both ultimately run the SAME `oura-link` `OuraClient<T: Transport>` over a different
  transport (btleplug on desktop, CoreBluetooth-over-FFI on iOS).

## Known gaps (web-only, not yet on iOS)

- **Advanced & debugging**: on-ring feature toggles (`/api/feature`), the per-type event
  stream, profile editing.

When you close one of these gaps, update this section.
