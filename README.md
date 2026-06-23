# open_oura

Reverse-engineering notes and tooling for the Oura Ring 5 BLE protocol.

The first goal is to safely map the Ring 5 protocol surface by comparing observed
BLE services, characteristics, notifications, and packet formats against prior
community work on Oura BLE behavior.

## Prior art

- ringverse protocol notes for Oura Ring 4:
  https://github.com/ringverse/protocol/blob/main/oura/BLE.md

Those notes describe a tag/length/payload packet format, little-endian fields,
and known request tags for firmware, battery, time sync, product info, auth,
events, features, and DFU. This repo should verify what still applies to Ring 5
before assuming compatibility.

## Safety rules

- Prefer passive discovery and read-only requests first.
- Do not send reset, factory reset, DFU, or firmware-update packets during basic
  probing.
- Capture raw hex and timestamps for every experiment.
- Keep unknown writes behind explicit flags.

## Rust client

An independent, cloud-free CLI that reads data straight from the ring lives under
`crates/` (`oura-core` library + `oura` binary). It connects, authenticates, and
drains the ring's history events into SQLite, and can stream live heart rate. See
[`crates/README.md`](crates/README.md). Tested live against a Ring 3 Horizon.

```bash
cargo build --release
./target/release/oura scan
```

## Planned layout

- `docs/` - protocol notes, experiments, captures, and packet tables.
- `tools/` - local BLE scanners/probers for Ring 5 experiments.
- `captures/` - ignored by git; local raw captures may contain device-specific
  identifiers.

## Quick start

```bash
python3 -m venv .venv
source .venv/bin/activate
pip install -r requirements.txt
python tools/ble_scan.py
```

On macOS, grant Bluetooth permission to the terminal app running the scanner.
