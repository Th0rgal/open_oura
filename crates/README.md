# Rust client (workspace)

An independent, cloud-free client that reads data directly from an Oura ring over
BLE. Designed to work across ring generations (Ring 3/4/5): it shares the common
GATT layout and auth flow, branches on reported *capabilities* rather than model
numbers, and always stores event bodies raw so unknown formats are never lost.

The code is split by concern - **fetch → interpret → apply** (see
[`docs/architecture.md`](../docs/architecture.md)):

- **`oura-protocol`** - *interpret (low level)*: packet framing, app-auth (AES),
  request builders, device parsers, and the event-body decoders (bytes → typed
  samples). Pure, no I/O; unit-tested against real captured packets.
- **`oura-link`** - *fetch*: the `Transport` trait + `btleplug` BLE, the
  connection/auth handshake, the history-event sync drain, live HR/ACM, features,
  RData. The high-level `OuraClient`.
- **`oura-analysis`** - *interpret (high level)*: on-device metric algorithms
  ported from Oura's `ecore` engine (HRV, SpO2, baselines, sleep summary, the
  three scores). See [`docs/algorithms/`](../docs/algorithms/README.md).
- **`oura-store`** - *apply*: SQLite persistence (raw events, readings, sync cursor).
- **`oura-cli`** - the `oura` binary wiring it together (+ the `viz`/`game` web UIs).

## Build

```bash
cargo build --release        # binary at target/release/oura
cargo test                   # protocol/auth/parser tests
```

## Auth key

Auth-gated operations (battery, history events, live HR) need the ring's 16-byte
app-auth key, stored as hex in a file (one line). For a ring you factory-reset and
re-key yourself, that file is written during pairing; for an already-onboarded ring
the key lives in the official app's database. Pass it with `--key-file`.

## Commands

```bash
# Discover nearby rings
oura scan

# Device info (firmware, serial, capabilities; battery needs the key)
oura --key-file key.hex info

# Pair with a factory-reset ring: install + save a new auth key
oura --name "Oura Ring 5" --key-file key.hex pair

# Show / enable measurement features (HR, SpO2 are off after a key-only pairing)
oura --key-file key.hex features --enable-hr --enable-spo2

# Drain history events into SQLite (incremental; app-style setup + flush/ack)
oura --name "Oura Ring Gen3" --key-file key.hex --db oura.db sync

# Latest cached HR / SpO2 values (ring must be worn)
oura --key-file key.hex latest

# Live heart rate stream for 30s (ring must be worn & measuring)
oura --key-file key.hex live-hr --seconds 30 [--raw]

# Live accelerometer stream - wave your hand to see motion (ACM real-time)
oura --key-file key.hex accel --seconds 15

# Real-time 3D motion visualizer (opens a local web UI)
oura --key-file key.hex viz   # then open http://127.0.0.1:8088

# Tilt-controlled asteroid game driven by the ring (local web UI)
oura --key-file key.hex game  # then open http://127.0.0.1:8089

# RData bulk sampler control: state (read) / stop / clear (teardown)
oura --key-file key.hex rdata state

# Offline: event counts; re-decode stored raw bodies with current decoders
oura --db oura.db events
oura --db oura.db redecode
```

> After pairing a ring yourself, its measurement features (daytime HR, SpO2…) are
> **off** - the official app turns them on at onboarding. Run `features --enable-hr
> --enable-spo2` once, then the ring begins measuring and HR/IBI/HRV/SpO2 events
> start accumulating (the ring decides when to measure, so allow a few minutes).

Common flags are global: `--name` (scan name filter, default `Oura`), `--address`,
`--scan-timeout`, `--db`, `--key-file`.

## What it recovers - and what it does not

It reproduces everything obtainable from the ring itself: device info, battery,
live heart rate (IBI → BPM), latest HR/SpO2, and the full history-event stream
(raw PPG/IBI/temperature/motion/SpO2 samples, plus the ring's on-device sleep
stages, activity MET levels and HRV). It does **not** compute the Oura cloud's
0–100 Readiness / Sleep / Activity / Stress scores or workout auto-classification -
those are server-side and out of scope by design (see `docs/data-recovery-map.md`).

## Event decoding status

The history-event **envelope** (tag, timestamp, type name) is fully decoded. Ring
5 may concatenate multiple `tag|length|payload` event frames into one BLE
notification; the protocol layer walks the whole notification before handing
events to the store. The per-event **body** layouts come from the ring's native
`libringeventparser.so`,
which was decompiled with Ghidra - every parser is a named function
(`parse_api_temp_event`, `parse_api_hrv_event`, …), so the decoders below are
**ports of the firmware's own logic**, not guesses (see `docs/native-decoder.md`).
Bodies are still stored **raw and lossless**; `events::decode_body` decodes the
ones below, and `oura redecode` backfills already-stored events when new decoders
land. Each decoder has a unit test.

| Event | Layout (from the native parser) | Decoded as |
| --- | --- | --- |
| `temp_event` / `temp_period` / `sleep_temp` | `i16` LE / 100 | temperature °C (verified worn ~33 °C; 7 probes Ring 3, 3 Ring 5) |
| `hrv_event` | pairs `(u8 hr, u8 rmssd)`, 5 min apart | avg HR + RMSSD (validated overnight: HR 40, RMSSD ~101 ms) |
| **`green_ibi_quality_event` (`0x80`)** | `ibi=(b1&7)|(b0<<3)`, `q=(b1>>3)&3` | inter-beat intervals → HR (daytime; ~50 bpm resting) |
| **`ibi_and_amplitude_event` (`0x60`)** | 14-byte bit-packed | 6× IBI ms + PPG amplitude → HR (validated overnight: 18k beats, median 41 bpm) |
| **`spo2_r_pi_event` (`0x8b`)** | header + 3-byte `(R: u16 BE/16384, PI: u8/255×0.05)` | SpO2 R-ratio + perfusion index (validated overnight: R ~0.72, PI ~4%) |
| `sleep_acm_period` (`0x72`) | 6 fixed-point floats | accelerometer MAD stats during sleep |
| `activity_information` | state + MET bytes (`<128: ×0.1`, else `12.8+(b-128)×0.2`) | state + MET levels |
| `motion_event` | orientation `b0>>5`, axes signed `i8×8`, intensity nibbles | orientation + avg x/y/z + intensity (validated worn) |
| `spo2_event` | header + `u8` per sample | SpO2 % series (decoder ready; not emitted by Ring 5 yet) |
| `sleep_phase_*` | 2-bit codes, 4/byte | hypnogram deep/light/rem/awake (not emitted yet) |
| `ambient` / `ehr_acm_intensity` | `u16` LE samples | raw values |
| `time_sync` / `state_change` / `wear_event` / `alert` / debug | u32 / byte+text | as labelled |
| `rtc_beacon` (`0x85`) | `u32 unix_s`, reserved bytes, trailer | precise 1-second wall-clock anchor |
| `cva_raw_ppg_data` (`0x81`) | `0x80 + i24 absolute`, otherwise signed i8 deltas | raw CVA PPG samples (stateless per event) |

**Ring-5 HR/SpO2 sources (empirical):** daytime HR arrives as `green_ibi_quality`
(`0x80`); overnight HR + amplitude as `ibi_and_amplitude` (`0x60`); SpO2 as the raw
`spo2_r_pi` (`0x8b`) R-ratio/PI stream. The native tag→type table is built at
runtime, so `0x80`/`0x8b` were matched by structure and confirmed against real
captured bytes (coherent HR, stable physiological R-ratio).

**Still to port** (catalogued from the `.so`, lower priority): fully stateful
cross-record CVA PPG accumulation, the bit-packed session-stateful variants
(`green_ibi_and_amp`, `spo2_ibi_and_amplitude`), the
opaque `sleep_summary_1..4` fields, `motion_period`, `real_steps`, and the ~40
`debug_data` statistics subtypes. Adding any of these never needs a re-sync - run
`oura redecode`.

## Live raw-data channels (not history events)

Beyond history events, the ring exposes live/raw signals over two other channels.
Both are app-initiated and **power-hungry, so teardown is part of the operation**:

- **Real-time measurements** (`0x06`): `accel` enables the **ACM** accelerometer
  stream (`bitmask 0x20`, time-boxed in minutes so the ring auto-stops; we also
  send an explicit OFF on exit). Verified live at ~50 Hz. The `ON_DEMAND` PPG
  variant (`0x200`) is defined but not wired into a command yet.
- **RData** bulk sampler (`0x03`): a *persistent flash session* that does **not**
  self-stop - lifecycle is `configure → get_page (drain) → stop → clear`. We
  implement and unit-test the request builders (configure/get-page formats taken
  from the app's `RDataStart`/`RDataGetPage`), and the `rdata` command exposes the
  safe/teardown actions (`state`, `stop`, `clear`). It can sample raw PPG (50–250
  Hz), accelerometer, **gyroscope** (125–2000 dps; never used in normal operation),
  and temperature. **Starting a collection is intentionally not exposed** - see
  limitations.

## Live motion visualizer (`viz`)

`oura viz` serves a self-contained web page (no external scripts - a hand-rolled
canvas renderer, so no CDN/Subresource-Integrity exposure) at
`http://127.0.0.1:8088`. It shows the ring's **orientation** in 3D (from the
gravity vector) and a **motion trajectory**, fed by the live ACM stream over
Server-Sent Events. The page has:

- **Start / Stop** buttons toggle the ring's BLE stream (`/start` arms ACM for
  `--minutes`; `/stop` sends realtime-off). To save battery, streaming also stops
  when you close the tab: the page sends `/stop` on unload, and the server sends
  realtime-off when the last client disconnects (also on Ctrl-C, with the ring's
  own duration timer as a final backstop).
- **Sensitivity settings** (live sliders): smoothing, integration damping,
  zero-velocity threshold, counts-per-g, and path scale.
- Drag to orbit the view; Reset clears the path.

**Limitations (by design):** the **gyroscope is not on the live BLE channel** - the
real-time channel (`0x06`) carries only the accelerometer (gyro is RData-only and
not real-time). So orientation is accel-derived (**pitch/roll** observable, **yaw
is not**), and the trajectory is double-integrated linear acceleration, which
**drifts** - a zero-velocity update and the sensitivity sliders keep it usable, but
it is a demo, not metric positioning. A true gyro-fused trajectory would require an
RData *recording* (non-real-time) replayed offline - kept for later.

## Tilt game (`game`)

`oura game` ("Ring Runner") reuses the same self-contained server and live ACM
stream as a WebGL asteroid-dodging game. It captures a neutral hand pose over 3
seconds, then steers a ship by ring orientation (decoupled pitch/roll, so the axes
do not cross-talk). Same start/stop and battery behavior as `viz`.

## Coverage & limitations

Honest status of what's trustworthy vs. provisional:

**Verified** - matched byte-exact to the native parser and/or validated on real
captures (incl. a full overnight sync):
- device info, battery, auth/pair, event sync, `time_sync`, `state_change`/`wear`,
  debug ASCII, live **ACM** stream, RData `state`.
- `temp_event`/`temp_period`/`sleep_temp` (°C; worn ~33 °C, asleep ~35 °C).
- `green_ibi_quality` (`0x80`) - daytime HR (~50 bpm resting, 1100+ beats).
- `ibi_and_amplitude` (`0x60`) - overnight HR: **18k beats, median 41 bpm**.
- `hrv_event` - overnight HR ~40 bpm, **RMSSD ~101 ms** (81 samples).
- `spo2_r_pi` (`0x8b`) - **25k samples, R ~0.72, PI ~4%** (stable, physiological).
- `motion_event` (orientation + axes + intensity, 250+ worn samples),
  `sleep_acm_period` (accelerometer MAD).

> Ring-5 HR/SpO2 sources: daytime HR = `green_ibi_quality` (`0x80`); overnight HR +
> amplitude = `ibi_and_amplitude` (`0x60`); SpO2 = raw `spo2_r_pi` (`0x8b`).

**Best-effort** - ported from the decompiled logic but **not yet confirmed against
real bytes** (the Ring 5 hasn't emitted these):
- `activity_information` MET scale (`×0.1` / `12.8+(b-128)×0.2`) - from the
  decompile; no per-event ground truth (the trends CSV is daily aggregates).
- `spo2_event` (`0x6f` summarized %), `sleep_phase_*` (hypnogram), `ambient`/`ehr`
  u16 - logic clear, awaiting data.

**Sleep analysis is partial on-device.** After a full overnight sync the ring
emitted only the **raw** streams (IBI, HRV, SpO2 R/PI, sleep temp, sleep ACM), not
the computed architecture. Triggering it explicitly with `oura sleep-analyze
--force` (`CheckSleepAnalysis`) **does** make the ring emit `bedtime_period` - the
detected sleep window (validated: ~7.28 h, decoded as two `u32` ring timestamps).
But the full hypnogram (`sleep_phase_*`) and `sleep_summary_1..4`, and the
summarized `spo2_event` (`0x6f`), still did **not** appear via this path - that
detailed staging is likely finished in the official app's fuller flow or
cloud-side. So sleep *stages/summaries* would, for now, have to be computed locally
from the raw inputs we have (HR/HRV/temp/motion) - the way the cloud does.

**Assumptions** baked in: event-body timestamps are the envelope's ring time
(deciseconds), not the native's resolved wall-clock; batched events' per-sample
times (HRV 5 min, etc.) are documented but not emitted per-sample yet. ACM counts
are raw (~1000/g at rest); no g-unit conversion is applied.

**Deferred** (catalogued, not implemented):
- `green_ibi_and_amp` (`0x71`), `spo2_ibi_and_amplitude` (`0x6e`) - bit-packed +
  session-stateful (need carried state for corrected timestamps).
- `sleep_summary_1..4` - fields are opaque/packed in the decompile; best decoded
  against an overnight capture cross-checked with the trends CSV.
- `motion_period` (2-bit-packed samples), `real_steps`, `on_demand_meas`, `aohr`,
  and the ~40 `debug_data` statistics subtypes. (`motion_event` is now decoded.)
- **RData collection start + page decode**: starting writes a persistent flash
  session, and the page payload format lives in `libecore`/native code we haven't
  decoded - so we expose only read/teardown, not capture, to avoid leaving the ring
  sampling into flash.
- **Sleep/HR/SpO2 validation**: these light up only after a worn-overnight sync;
  the decoders are in place but unproven until then.
