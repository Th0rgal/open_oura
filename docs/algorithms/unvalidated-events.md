# Event decoders ported from the native parser — validation status

These decoders were ported from `libringeventparser.so` (the byte layouts are the
parser's ground truth) but most haven't appeared in our captures, so their field
*mapping* is confirmed by code, not by real data. Each unvalidated decoder emits
`"_status":"unvalidated"` in its JSON; drop it once a real sample confirms the
fields. The handler `@ address` is cited in `crates/oura-protocol/src/events.rs`.

## Validated (confirmed against captured bytes)

| Tag / subtype | Event | How confirmed |
| --- | --- | --- |
| `0x61`/`0x24` | `battery_level_changed` | captured: `battery_pct` 92–100 %, `voltage_mv` 4280–4391 mV (Li-ion). |
| `0x6b` | `motion_period` | decodes the captured packed 2-bit streams. |
| `0x6c` | `feature_session` | captured balanced start/stop pairs. |

## Unvalidated (layout from parser, awaiting a real sample)

| Tag | Event | Field confidence | How to trigger / validate |
| --- | --- | --- | --- |
| `0x49` | sleep_summary_1 | offsets only (abs time needs header) | after a processed sleep period |
| `0x4c` | sleep_summary_2 | structure only (u64/u16/u32, names TBD) | after a processed sleep period |
| `0x4f` | sleep_summary_3 | structure (3 fields are ÷8 fixed-point) | after a processed sleep period |
| `0x58` | sleep_summary_4 | structure only | after a processed sleep period |
| `0x7e`/`0x7f` | real_steps_features | bit-unpacked fields, names TBD | walk with the step feature on |
| `0x86` | aohr_event | fields (bpm, quality, 1920 ms interval) | no toggle — rides on daytime HR (enabled); appears when worn |
| `0x84` | ambient_event | i16 @ 5 min, units TBD | appears with ambient sensing |
| `0x87` | atlas_metadata | start-stream control msg | **backend-gated** (`sensing_discovery/atlas` FeatureDefinition, cloud-delivered) — not enableable from an independent client |
| `0x88` | atlas_raw_bioz_data | delta-coded i32 stream | same backend gate as `0x87` |
| `0x61`/`0x11` | charging_time | u32 (units TBD) | **not emitted in normal use** — charge-end comes via `charging_ended_statistics` (0x20/0x27) instead; likely needs a full low→full cycle or is Ring-3-only |

## Not decoded (low value / diagnostic only)

`0x61` debug subtypes other than charging/battery (sleep_statistics `0x09`,
afe_stats `0x28`, ppg_signal_quality `0x35`, fuel_gauge `0x14`, …) are device
diagnostics; they're tagged `{kind:"debug_data", subtype, raw}` and left raw.
Raw-PPG streams (`0x67/0x68/0x81`) and on-demand measurements (`0x62/0x65/0x66`)
are decoded elsewhere via the RData path or not yet needed.
