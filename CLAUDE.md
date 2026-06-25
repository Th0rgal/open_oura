# CLAUDE.md — open_oura

Guidance for Claude Code (and humans) working in this repo. Read this first, then
the docs it points to. The deep protocol reference lives in `docs/`; this file is
the orientation map, the **feature inventory** (what we can actually read from a
ring), and the **smart-alarm / iPhone-app** plan that motivates current work.

## What this project is

A **cloud-free** Oura ring client. We reverse-engineered the ring's BLE protocol
and talk to it directly — pair, authenticate, sync history events, read live-ish
signals — with no Oura account. See `README.md` for the pitch and `docs/` for the
protocol internals.

## Build / run

```bash
cargo build --release
./target/release/oura scan                          # find rings
./target/release/oura --name "Oura ..." --key-file key.hex pair   # factory-reset ring only
./target/release/oura --key-file key.hex info       # firmware/serial/battery/caps
./target/release/oura --key-file key.hex sync        # drain history events -> oura.db
./target/release/oura --key-file key.hex live        # NEW: real-time health dashboard
./target/release/oura --key-file key.hex viz         # 3D motion visualizer (accel)
```

Auth key is a 16-byte hex file (`key.hex`, gitignored). It is installed once on a
**factory-reset** ring (`pair`) and re-sent each connection. macOS: grant the
terminal Bluetooth permission.

## Crate layout

`crates/` is split by concern (see `docs/architecture.md`):
- `oura-protocol` — packet framing, request builders, event-body decoders (`events.rs`), auth.
- `oura-link` — BLE transport (`ble.rs`), the high-level `OuraClient` (`client.rs`).
- `oura-analysis` — ported on-device metric algorithms (HRV, temperature, sleep, …).
- `oura-store` — SQLite persistence (events + scalar `readings`).
- `oura-cli` — the `oura` binary; web UIs (`live.rs`, `viz.rs`, `game.rs`) share `motion_server.rs` plumbing.

## The device on hand (important for live behavior)

`info` reports `Hardware: ORE_06`, `Firmware: 2.12.0`. Per `docs/firmware-update.md`,
the **`oreo` codename = Gen 4 / Ring 4**, so `ORE_*` is a Ring 4. The repo's other
test devices were a Ring 3 Horizon (`BLB_03`, fw 3.x) and a Ring 5. **Pair, auth,
battery, history-event sync, and the accelerometer stream all work on this Ring 4**
— the GATT layout, framing, and auth flow are shared across generations. Live HR is
where firmware behavior diverges (below). A factory reset wipes the auth key and
user data but does **not** change firmware; new firmware only comes via Oura's
signed OTA (needs the official app/cloud — `docs/firmware-update.md`).

## Feature inventory — what a ring actually gives us

Three layers (full detail: `docs/data-recovery-map.md`):

### 1. Live / real-time over BLE (only while connected)

| Signal | Mechanism | Status on this Ring 4 |
| --- | --- | --- |
| **Accelerometer** | `SetRealtime(ACM)` → ~50 Hz x/y/z indications (tag `0x33`) | ✅ **Works** — true stream (powers `viz`, `game`, `live`) |
| **Heart rate + HRV** | force-measure → ring records `0x80` events → drain them | ✅ **Works** — see below |
| **SpO2** | enable SpO2 + drain `0x6f`-family events | ◐ needs long stillness; rarely fires |
| **Skin temp** | recorded as `0x46`/`0x69`/`0x75` during measurement | ◐ occasional |
| **Battery / charging** | poll `0x0c` | ✅ reliable (works on charger) |

**Key empirical finding — how live HR actually works (the "no beats" answer).**
The decompiled app suggests `SetFeatureMode(DAYTIME_HR, CONNECTED_LIVE)` makes the
ring *push* IBI notifications (`0x2f`/sub-tag `0x28`). **It does not.** Confirmed
empirically on this Ring 4 (fw 2.12.0):

- `CONNECTED_LIVE` is ACK'd (`2f03230200`) and forces `state=idle → measuring`, but
  **no** `0x2f`/`0x28` push frames ever arrive — even with notifications enabled.
- `GetFeatureLatestValues(DAYTIME_HR)` returns `result=0 state=2` but the IBI field
  stays `00 00` on this firmware (the Ring 3 Horizon populated it; 2.12.0 doesn't).

The path that **works** (and is what the app's "Measure pulse" does):

1. `SetNotification(0x3f)` — the load-bearing handshake step our CLI was skipping;
   without it the ring won't emit async notifications (`sync` works without it only
   because event-drain is a pull, and ACM works because it's explicitly armed).
2. `SetFeatureMode(DAYTIME_HR, CONNECTED_LIVE)` — forces the green-LED measurement.
3. The ring **records** `0x80 green_ibi_quality_event`s into the history stream
   (~one every 3–8 s), each carrying `{hr_bpm:[…], ibi_ms:[7 beats], quality:[…]}`.
4. **Incrementally drain** those events (`OuraClient::drain_events_live`) and decode
   them → live bpm + per-beat IBI (→ **HRV/RMSSD**). "Live" = rapid event-sync, not
   a raw push stream.

Confirmed reading (resting): `0x80 {"hr_bpm":[73,71,78,74,71,73],
"ibi_ms":[828,820,834,763,802,845,818],"quality":[0,1,1,1,1,1,1]}` → ~73 bpm. Note
the `quality` byte is unreliable for gating (clean resting beats showed `0/1`,
noisy moving beats `2/3`); filter IBIs by physiological range instead (400–1300 ms).

`oura live-hr` and `oura live` both use this path. The throwaway `oura measure`
command is the diagnostic that nailed it (before/after event-diff).

### 2. History events (synced after the fact — the real data) — `oura sync`

This is where the rich data lives. The ring records and summarizes on-device; we
drain it incrementally (`docs/sync-orchestration.md`). Decoders: `events.rs`.

**Raw sample events:** `ibi`/`ibi_and_amplitude` (`0x44`/`0x60`/`0x71`, per-beat
IBI → HR + HRV), `temp` (`0x46`/`0x69`/`0x75`, skin °C), `motion` (`0x47`/`0x6b`),
`spo2` (`0x6f`/`0x70`/`0x77`), `raw_ppg`, `on_demand_meas` (`0x62`, spot
HR/HRV/breath/temp). **HRV** as 5-min avg RMSSD + HR (`0x5d`, `hrv_event`).

**Ring-computed summaries:** `sleep_summary_1..4` (`0x49`/`0x4c`/`0x4f`/`0x58` —
bedtime, stage durations, lowest HR), **`sleep_phase_*`** (`0x4b`/`0x4e`/`0x5a` —
the **hypnogram**: 2-bit DEEP/LIGHT/REM/AWAKE per epoch), `activity_information`
(`0x50`, 13 MET-level bins + steps), `wear`/`state_change`.

So **sleep staging and MET binning happen on the ring** and sync down as events.

### 3. RData bulk raw (`0x03`) — research opt-in

Full-rate raw PPG/ACM/gyro/temp via a flash session. Heavy, gated, mandatory
teardown. Not used in normal pulls. `oura rdata state|stop|clear`.

### What we *cannot* reproduce cloud-free

The 0–100 **scores** (Readiness/Sleep/Activity/Stress) and workout
auto-classification. Correction from later RE (`docs/algorithms/README.md`): the
scores are actually computed **on-phone** by the native `ecore` engine, not the
cloud — and are being ported into `oura-analysis` (calibration-fit where the
`.rodata` tables won't read back). The genuine blocker is the **sleep hypnogram
model** (`SleepNet`, an encrypted PyTorch `.pt.enc` whose AES-GCM key is
**server-delivered** — `docs/algorithms/sleepnet.md`). But note: the **ring already
emits its own hypnogram** as `sleep_phase_*` events, so we get stages without
SleepNet — just not Oura's exact phone-side restaging.

## The `oura live` dashboard (current work)

`crates/oura-cli/src/live.rs` + `live.html`. A self-contained web health dashboard
(no CDN). One BLE connection; **Start** enters live mode, **Stop** (or closing the
tab) returns the ring to normal (`AUTOMATIC` modes, realtime off).

Architecture worth knowing before editing:
- **One writer task** (`spawn_poll_loop`) owns *all* protocol writes so they never
  race. HTTP `/start`/`/stop` only flip an `AtomicBool`; the loop acts on the edge.
- On Start it does `SetNotification(0x3f)` + force daytime-HR `CONNECTED_LIVE`,
  baseline-drains to position an event cursor at "now", then arms the ACM stream.
- A **parser task** turns raw accel notifications into `accel` JSON over SSE.
- HR/temp/SpO2 come from `OuraClient::drain_events_live` (new) — a **stream-safe**
  incremental event drain (bounded per-batch wait, ignores accel frames) that pulls
  freshly-recorded `0x80`/temp/SpO2 events while the ~50 Hz accel stream is live.
  (The quiet-window `transact`/`request` would hang forever during that stream;
  `request_until` is the bounded single-response variant used for battery.)
- The page draws hand-rolled canvas charts for HR / motion / temp / SpO2 / battery,
  computes **HRV (RMSSD)** from the streamed IBIs, and shows live **restlessness %**.

## Long-term goal: a Sleep-Cycle-style smart alarm, in our own app

The user's target: a "wake in your optimal window" alarm (e.g. wake me between
08:00–08:30 at the lightest point of my sleep). Feasibility from the feature map:

**We have the right signals — and richer than a phone-only app.** Sleep-Cycle-class
apps stage sleep almost entirely from **accelerometer movement** (phone on the
mattress). We get, from the ring, all of: per-beat **HR** + **HRV** (autonomic tone
— HR drops and HRV rises in deep sleep, both shift before/at wake), **motion/MAD**
(restlessness, the classic wake signal), **skin temperature**, and the ring's own
**hypnogram** (`sleep_phase_*`). That's a strong multi-signal basis for detecting
light-sleep / near-wake windows.

**The architecture question is *when* the signals are available:**
- During sleep the ring is **not connected** to the phone — it records to flash and
  syncs on the next connection. So a naive "read live HR at 08:00 and decide" won't
  work: live HR is unreliable anyway (above), and the ring isn't streaming overnight.
- **Two viable designs:**
  1. **Wake-window polling:** a few minutes before the earliest alarm time, the app
     connects to the ring and **syncs the recent history events** (they're already
     recorded), reconstructs the last ~30–60 min of HR/HRV/motion/stages, and picks
     the lightest moment to fire the alarm. This is the realistic path — it uses the
     reliable history channel, not the flaky live channel.
  2. **Continuous-ish motion during the window:** in the final window, also arm the
     **accelerometer stream** (which *does* work live) and watch movement in real
     time as the immediate trigger, blended with the synced stage history.
- We do **not** need Oura's cloud or the SleepNet model: the ring emits its own
  stages, and we have the raw signals to run our own light/deep classifier if we
  want independence from the firmware's staging.

**On-demand fresh HR/HRV is proven** (the live-HR work above): in the wake window
the app can connect, force a green-LED measurement, and within ~10–20 s pull fresh
`0x80` events → current HR + HRV — *plus* the live accelerometer for movement. So at
decision time we have real-time HR, HRV and motion, not just stale history. That's a
strong trigger basis.

**Open research questions (probe these with `oura live` + `oura sync`):**
- How fresh is the synced history near "now"? What's the event latency/granularity
  for HR/HRV/motion in the last few minutes before a connect?
- Does arming the accelerometer stream while worn-and-asleep stay stable for the
  ~30-min window without waking the user / draining battery?
- How long does a green-LED measurement take to lock a stable beat overnight (skin
  contact differs from daytime)? Tune the force-measure → first-`0x80` latency.
- Validate our own light-vs-deep classifier against the ring's `sleep_phase_*`.

## iPhone app — `ios/` (built)

A native SwiftUI app exists under `ios/` (see `ios/README.md`). Architecture:

- **`crates/oura-ffi`** — a Rust `staticlib` exposing a tiny C ABI over the tested
  core: `oura_encrypt_nonce` (AES auth) + `oura_decode_event`/`oura_event_name`
  (the `events.rs` decoders). Built into `OuraFFI.xcframework` by `ios/build-rust.sh`
  (device `aarch64-apple-ios` + sim `aarch64-apple-ios-sim`). Nothing async crosses
  the boundary.
- **Swift** (`ios/OpenOura`) does the rest natively: `OuraProtocol.swift` (GATT
  UUIDs, `tag|len|payload` framing, request builders), `OuraRing.swift`
  (CoreBluetooth + the request/response primitives — `transact`, `requestUntil`,
  `drainEventsLive` — auth, pair, live mode, history sync), SwiftUI tabs
  (Today/Live/Sleep/Settings), Keychain key store.
- **Pairing**: `OuraRing.pairNewRing()` mirrors `oura pair` — generate a random
  16-byte key, `SetAuthKey` (factory-reset ring only), store in Keychain. The paste
  path is a secondary "import existing key" for a ring already paired via the CLI.
- **Project**: generated by `xcodegen` from `ios/project.yml` (set your own
  `DEVELOPMENT_TEAM` — a free Personal Team works, a paid team avoids the 7-day
  expiry). Compile-check without signing: `xcodebuild -project
  ios/OpenOura.xcodeproj -scheme OpenOura -sdk iphoneos26.5 build
  CODE_SIGNING_ALLOWED=NO`. Device deploy needs the iOS platform component installed
  (Xcode → Settings → Components) + `-allowProvisioningUpdates`.
- **Auto-reconnect/sync**: `OuraRing.autoReconnect()` uses a no-timeout CoreBluetooth
  pending connect on the saved peripheral id, so the app silently reconnects when the
  ring wakes (and auto-syncs); toggles in Settings.
- The Simulator has **no Bluetooth** — ring features only work on a real device.
- **Connecting gotchas** (verified on device): the ring must be **awake** to accept a
  connection — off-charger-and-not-worn it deep-sleeps and `connect()` hangs with no
  `didConnect` (iOS sees a stray advert but can't grab a connectable window). On the
  **charging pad** (or worn) it connects immediately. Also BLE is one-central: a Mac
  with the ring bonded auto-reconnects and steals the slot — `blueutil -p 0` on the
  Mac (or "Forget" the ring) while testing the phone. The app surfaces these via a
  `ConnectGuideView` onboarding sheet (place ring on pad → Connect, with live state)
  and a connect timeout that fails loudly instead of hanging.
- iOS BLE background modes (`bluetooth-central`, already in Info.plist) allow a
  timed wake-window connect for the smart alarm.

## Conventions / gotchas

- Prefer passive, read-only requests. reset / DFU / factory-reset / flight-mode are
  danger-gated — never send during normal use.
- Multi-byte ints are little-endian; extended ops ride outer tag `0x2f` with the
  first payload byte as the ext op.
- During a live accel stream, **don't** use the quiet-window `request`/`transact` —
  use `request_until` (bounded, stream-safe).
- Never commit keys or captures (`key.hex`, `captures/`, `reverse/` are gitignored).
- The store retains raw event bodies, so new decoders apply retroactively via
  `oura redecode` / `Store::redecode` (no re-sync needed).
