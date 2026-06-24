# Native Event Decoder (libringeventparser.so)

The ring's history-event **body** layouts are not in the decompiled Java app - the
binary→struct conversion happens in a native library, `libringeventparser.so`
(JNI `nativeParseEvents`). We decompiled it to port the exact logic into
`oura-protocol` (`oura-protocol::events::decode_body`), instead of guessing byte layouts.

## How it was done

1. Extract the lib from the XAPK: `config.arm64_v8a.apk → lib/arm64-v8a/libringeventparser.so`.
2. Decompile with Ghidra headless (`analyzeHeadless … -postScript`), exporting the
   decompiled C for every `parse_*` / `decode_*` function.
3. The binary keeps **descriptive C++ symbols**, so each event has a named parser:
   `EventParser::parse_api_temp_event`, `parse_api_hrv_event`,
   `parse_api_activity_info_event`, `parse_api_sleep_phase_details_and_data`, etc.
   The byte layout (offsets, int sizes, scales, sample stride, cadence) is read
   directly from each function and translated to Rust.

The `.so`, the Ghidra project, and the decompiled C are RE work products kept local
(not committed), like the rest of `reverse/`.

## Key facts recovered

- **Scales are exact**, e.g. temperature is `int16 / 100` °C (matched our earlier
  empirical decoder byte-for-byte); activity MET is `b<128 → b×0.1`, else
  `12.8 + (b-128)×0.2`.
- **Batched events** carry N samples sharing one event time; the first sample is
  `utc_time_ms − (n-1)×interval`, stepping by `interval` (HRV/ambient = 5 min,
  sleep-temp = 30 s, meas-quality = 3 min, SpO2 = 1 s, AOHR = 1920 ms).
- **The tag→parser map is built at runtime** (a registered table indexed by tag),
  not a static switch - so unknown tags are matched by structure and confirmed
  against real bytes.
- **Tag `0x80` = `green_ibi_quality_event`** (Ring 5): green-LED inter-beat
  intervals, `ibi_ms = (b1 & 7) | (b0 << 3)`, `quality = (b1>>3)&3`. Confirmed by
  decoding real captures to coherent resting heart rate (~52 bpm). This means heart
  rate is recoverable even when the daytime-HR feature is off.

## Workflow for adding a decoder

1. Read the relevant `parse_api_*` in the decompiled C.
2. Port its length check + field reads into `oura-protocol::events::decode_body` with a unit test.
3. `oura redecode` to apply it to events already stored raw - no re-sync needed.

See `crates/README.md` for the current decoding-status table.
