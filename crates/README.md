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

# Drain history events into SQLite (incremental; resumes from a saved cursor)
oura --name "Oura Ring Gen3" --key-file key.hex --db oura.db sync

# Latest cached HR / SpO2 values (ring must be worn)
oura --key-file key.hex latest

# Live heart rate stream for 30s (ring must be worn)
oura --key-file key.hex live-hr --seconds 30

# Offline: event counts already stored in the database
oura --db oura.db events
```

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
per-event **body** field layouts are produced by the ring's native
`libringeventparser.so` and are not present in the decompiled app, so each body is
stored **raw and lossless** and decoded opportunistically by
`oura-core`'s `events::decode_body`. Bodies recovered so far by correlating real
captured bytes against the protobuf field shapes (each backed by a test):

| Event | Layout | Decoded as |
| --- | --- | --- |
| `temp_event` | 7× `i16` LE, centi-°C | seven probe temperatures (°C) |
| `temp_period`, `sleep_temp_event` | `i16` LE, centi-°C | temperature (°C) |
| `time_sync` | `u32` LE | unix timestamp |
| `state_change`, `wear_event` | state byte + ASCII | state + text |
| `debug_event`, `debug_data` | ASCII | text |

Still raw, pending a **worn-ring** capture (the test ring was on its charger, so
HR/PPG/motion/activity bodies were noise or zero): `ibi_event`, `hrv_event`,
`spo2_event`, sleep summaries/phases, `activity_information`, `motion_event`,
`ppg_amplitude`. Adding a decoder never needs a re-sync — the raw bytes are kept.
