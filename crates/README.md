# Rust client (`oura-core` + `oura-cli`)

An independent, cloud-free client that reads data directly from an Oura ring over
BLE. Designed to work across ring generations (Ring 3/4/5): it shares the common
GATT layout and auth flow, branches on reported *capabilities* rather than model
numbers, and always stores event bodies raw so unknown formats are never lost.

- **`oura-core`** — the reusable library: packet framing, app-auth (AES), a
  `Transport` trait with a `btleplug` BLE implementation, device-info parsers, the
  history-event drain loop, and optional SQLite storage. Pure logic is unit-tested
  against real captured packets, with no ring required.
- **`oura-cli`** — a thin `oura` binary over the library.

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

# Drain history events into SQLite (incremental; resumes from a saved cursor)
oura --name "Oura Ring Gen3" --key-file key.hex --db oura.db sync

# Latest cached HR / SpO2 values (ring must be worn)
oura --key-file key.hex latest

# Live heart rate stream for 30s (ring must be worn & measuring)
oura --key-file key.hex live-hr --seconds 30 [--raw]

# Live accelerometer stream — wave your hand to see motion (ACM real-time)
oura --key-file key.hex accel --seconds 15

# RData bulk sampler control: state (read) / stop / clear (teardown)
oura --key-file key.hex rdata state

# Offline: event counts; re-decode stored raw bodies with current decoders
oura --db oura.db events
oura --db oura.db redecode
```

> After pairing a ring yourself, its measurement features (daytime HR, SpO2…) are
> **off** — the official app turns them on at onboarding. Run `features --enable-hr
> --enable-spo2` once, then the ring begins measuring and HR/IBI/HRV/SpO2 events
> start accumulating (the ring decides when to measure, so allow a few minutes).

Common flags are global: `--name` (scan name filter, default `Oura`), `--address`,
`--scan-timeout`, `--db`, `--key-file`.

## What it recovers — and what it does not

It reproduces everything obtainable from the ring itself: device info, battery,
live heart rate (IBI → BPM), latest HR/SpO2, and the full history-event stream
(raw PPG/IBI/temperature/motion/SpO2 samples, plus the ring's on-device sleep
stages, activity MET levels and HRV). It does **not** compute the Oura cloud's
0–100 Readiness / Sleep / Activity / Stress scores or workout auto-classification —
those are server-side and out of scope by design (see `docs/data-recovery-map.md`).

## Event decoding status

The history-event **envelope** (tag, timestamp, type name) is fully decoded. The
per-event **body** layouts come from the ring's native `libringeventparser.so`,
which was decompiled with Ghidra — every parser is a named function
(`parse_api_temp_event`, `parse_api_hrv_event`, …), so the decoders below are
**ports of the firmware's own logic**, not guesses (see `docs/native-decoder.md`).
Bodies are still stored **raw and lossless**; `events::decode_body` decodes the
ones below, and `oura redecode` backfills already-stored events when new decoders
land. Each decoder has a unit test.

| Event | Layout (from the native parser) | Decoded as |
| --- | --- | --- |
| `temp_event` / `temp_period` / `sleep_temp` | `i16` LE / 100 | temperature °C (verified worn ~33 °C; 7 probes Ring 3, 3 Ring 5) |
| `hrv_event` | pairs `(u8 hr, u8 rmssd)`, 5 min apart | avg HR bpm + RMSSD ms |
| **`green_ibi_quality_event` (`0x80`)** | `ibi=(b1&7)|(b0<<3)`, `q=(b1>>3)&3` | **inter-beat intervals → heart rate** (validated: ~52 bpm resting) |
| `ibi_and_amplitude_event` | 14-byte bit-packed | 6× IBI ms + PPG amplitude (pending real-data check) |
| `activity_information` | state + MET bytes (`<128: ×0.1`, else `12.8+(b-128)×0.2`) | state + MET levels |
| `spo2_event` | header + `u8` per sample | SpO2 % series |
| `sleep_phase_*` | 2-bit codes, 4/byte | hypnogram deep/light/rem/awake |
| `ambient` / `ehr_acm_intensity` | `u16` LE samples | raw values |
| `time_sync` / `state_change` / `wear_event` / `alert` / debug | u32 / byte+text | as labelled |

**Identified `0x80`:** it's a Ring-5 green-LED IBI stream (the native tag→type
table is built at runtime, so it was matched by structure and confirmed against
real bytes — coherent resting HR). So heart rate is recoverable even with the
daytime-HR feature off.

**Still to port** (catalogued from the `.so`, lower priority): the bit-packed
session-stateful variants (`green_ibi_and_amp`, `spo2_ibi_and_amplitude`), the
opaque `sleep_summary_1..4` fields, full `motion_event/period`, `real_steps`, and
the ~40 `debug_data` statistics subtypes. Adding any of these never needs a
re-sync — run `oura redecode`.

## Live raw-data channels (not history events)

Beyond history events, the ring exposes live/raw signals over two other channels.
Both are app-initiated and **power-hungry, so teardown is part of the operation**:

- **Real-time measurements** (`0x06`): `accel` enables the **ACM** accelerometer
  stream (`bitmask 0x20`, time-boxed in minutes so the ring auto-stops; we also
  send an explicit OFF on exit). Verified live at ~50 Hz. The `ON_DEMAND` PPG
  variant (`0x200`) is defined but not wired into a command yet.
- **RData** bulk sampler (`0x03`): a *persistent flash session* that does **not**
  self-stop — lifecycle is `configure → get_page (drain) → stop → clear`. We
  implement and unit-test the request builders (configure/get-page formats taken
  from the app's `RDataStart`/`RDataGetPage`), and the `rdata` command exposes the
  safe/teardown actions (`state`, `stop`, `clear`). It can sample raw PPG (50–250
  Hz), accelerometer, **gyroscope** (125–2000 dps; never used in normal operation),
  and temperature. **Starting a collection is intentionally not exposed** — see
  limitations.

## Coverage & limitations

Honest status of what's trustworthy vs. provisional:

**Verified** — matched byte-exact to the native parser and/or validated on real
captures/live: device info, battery, auth/pair, event sync, `temp_event`/
`temp_period`, `green_ibi_quality` (`0x80` → heart rate, ~52 bpm resting),
`time_sync`, `state_change`/`wear`, debug ASCII, live **ACM** stream, RData `state`.

**Best-effort** — ported from the decompiled logic but **not yet confirmed against
real bytes** (no such data captured yet, or no ground truth):
- `hrv_event` — layout is clear, but no HRV event captured yet (needs the ring to
  measure / sleep).
- `ibi_and_amplitude_event` (`0x60`) — intricate bit-packing; **no real `0x60`
  capture yet** to confirm.
- `activity_information` MET scale (`×0.1` / `12.8+(b-128)×0.2`) — taken from the
  decompile; no per-event ground truth (the trends CSV is daily aggregates).
- `spo2_event`, `sleep_phase_*`, `ambient`/`ehr` u16 — logic clear, awaiting real
  worn/asleep data.

**Assumptions** baked in: event-body timestamps are the envelope's ring time
(deciseconds), not the native's resolved wall-clock; batched events' per-sample
times (HRV 5 min, etc.) are documented but not emitted per-sample yet. ACM counts
are raw (~1000/g at rest); no g-unit conversion is applied.

**Deferred** (catalogued, not implemented):
- `green_ibi_and_amp` (`0x71`), `spo2_ibi_and_amplitude` (`0x6e`) — bit-packed +
  session-stateful (need carried state for corrected timestamps).
- `sleep_summary_1..4` — fields are opaque/packed in the decompile; best decoded
  against an overnight capture cross-checked with the trends CSV.
- `motion_event`/`motion_period` full fields, `real_steps`, `on_demand_meas`,
  `aohr`, and the ~40 `debug_data` statistics subtypes.
- **RData collection start + page decode**: starting writes a persistent flash
  session, and the page payload format lives in `libecore`/native code we haven't
  decoded — so we expose only read/teardown, not capture, to avoid leaving the ring
  sampling into flash.
- **Sleep/HR/SpO2 validation**: these light up only after a worn-overnight sync;
  the decoders are in place but unproven until then.
