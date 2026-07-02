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

The non-model math is the **ported ecore ground truth** from `crates/oura-analysis`
(`ported::{spo2, temperature, metabolic, baseline}`): SpO₂ calibration, nightly skin
temperature, Schofield BMR (→ `total_kcal`), Jackson VO₂max, steps→distance, and the
annealing-EMA personal baseline behind each vital's `delta_pct`. Add a new derived
metric there once and both clients receive it in the JSON.

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
| **Unified day (night + activity)** | `renderDay`, `dayCard` | `TodayCard` | `nights[]`, `activity*` | — |
| Sleep detail + **hypnogram** | `openSleepDetail`, `hypnogram()` | `SleepDetail`, `Hypnogram` | `nights[].stages` | SleepNet (web: Python · iOS: `SleepStaging`) |
| Stage breakdown | `sleepDetailBody` breakdown | `StageBreakdown` | `nights[].{deep,light,rem,wake}_pct` | SleepNet |
| **Cardiovascular age** | `renderCardio` | Cardio section | `cardio` | CVA (web: Python · iOS: `CvaModel`) |
| **VO₂max estimate** | `renderCardio` | Fitness section | `fitness.vo2max` | — (Jackson, model-free) |
| Movement ridge | `ridgeSvg` | `MovementRidge` | `activity_profile` | — (MET, model-free) |
| Activity detail | `openActivityDetail`, `activityDetailBody` | `ActivityDetail` | `activity_daily`, `activity` | — |
| **Activity sessions / workouts** | `openActDetail` (session) | workouts section | `activity` | AAD (web: Python · iOS: `ActivityModel`) |
| Steps / active calories / **distance** | `activityDetailBody` stats | activity day stats | `activity_daily` (incl. `distance_m`) | — |
| Previous days browser | `openDaysBrowser` → `openDayDetail` | `AllDaysView` → `DayDetailView` | day keys | — |
| Device & data health | `renderDevice` | device section | `device`, `streams` | — |

## The day is one unit — pair night + activity by *wake date*

Both clients render **one "day" = last night's sleep + that day's activity**, drillable
into either half and browsable back through previous days. The hero on each home screen is
the most recent day; "show all N days" (web: `openDaysBrowser`; iOS: `AllDaysView`) opens
the rest, each as a combined night+activity detail.

The **pairing rule matters and must stay identical across clients**: nights are labelled by
their **onset** date (the evening you went to bed), so an overnight sleep that crosses
midnight belongs to the *next* day's morning. A day `D` pairs with the sleep you *woke from*
on the morning of `D` — the night whose **wake date** is `D`, not whose onset date is `D`.
This lives in `wakeYmd()` (web `app.js`) and `Summary.wakeYmd` (iOS `Models.swift`); keep the
two in lockstep. `nightForDay`/`night(forDay:)` pick the longest in-bed night for a morning so
a nap doesn't shadow the real sleep.

## Where the two clients diverge

- **Home layout**: same day-unit model on both, but the iOS "Observatory" theme floats data on
  a black canvas (no panels) while the web uses bordered cards/dialogs. Match *data/features*,
  not pixel-for-pixel layout. iOS opens details as sheets; the web as stacked `<dialog>`s.
- **BLE sync**: iOS syncs **natively** — `RingSync.swift` (CoreBluetooth `BLETransport`)
  drives the Rust `RingSession` FFI (`oura-core`) to authenticate + drain into a writable
  DB. The web dashboard has **no** BLE; it reads a DB produced by the desktop `oura sync`.
  Both ultimately run the SAME `oura-link` `OuraClient<T: Transport>` over a different
  transport (btleplug on desktop, CoreBluetooth-over-FFI on iOS).

## Known gaps (web-only, not yet on iOS)

- **Advanced & debugging**: on-ring feature toggles (`/api/feature`), the per-type event
  stream, profile editing.

When you close one of these gaps, update this section.
