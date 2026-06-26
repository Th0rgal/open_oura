# RData Capacity & Battery Probe (Phase-2 spike)

**Goal.** Determine whether the ring's RData bulk sampler (tag `0x03`) can capture
high-rate raw accelerometer/gyro to flash *while disconnected* (e.g. during a
swim, when BLE can't reach the phone underwater), and if so, measure:

- how long it can record before saturating flash, and what happens at the limit;
- the data rate / volume; and
- the battery impact of sampling.

This matters because the synced history only contains **30-second windowed
motion summaries** (`motion_event` 0x47) — far too coarse to resolve swim
strokes, laps, or turns. RData is the only path to sample-rate raw data, and
unlike the realtime ACM stream (`0x06`, BLE-only, dies on disconnect) it logs to
the ring's flash, so it *could* survive an underwater swim.

## Status: BLOCKED — raw sampler is gated off at the firmware level

With everything we can drive over the wire, **the ring refuses to start
recording.** `configure` (subtag 2) always returns status `3`, which the
decompiled app decodes as **`INVALID_SUBTAG`** (NOT "idle" — see status table
below): the firmware rejects the configure op because the **raw-data sampler
capability is not enabled on this (consumer) ring**. Capacity and battery numbers
below therefore remain **unmeasured** — the blocker is upstream of them, and is
the research/enterprise provisioning gate, not a bug in our packets.

### Capability read (decisive)

`oura info` on the Ring 5 (`50380B…`, fw 2.1.3) reports (`feature:version` pairs):

```
Caps: 0:5 1:3 2:5 3:3 4:6 5:1 8:3 9:0 10:7 11:0 12:0 13:1 14:2 18:1 20:0 22:1
```

- `1:3`  → `RESEARCH_DATA` (id 0x01) = version 3 — advertised.
- `18:1` → **`RAW_DATA_SAMPLER` (id 0x12) = version 1 — advertised/present.**

> **Correction:** `RAW_DATA_SAMPLER`'s wire id is **`0x12` (18)**, not `0x14` — the
> app's `FeatureCapabilityId` sets it to `DFUStart.REQUEST_LENGTH = 18`. Our
> `protocol.rs` constant was mislabeled `0x14` (a different, version-0
> capability); now fixed. So the sampler is **not** absent — it is advertised at
> version 1 but **not entitled** (see below).

The capability is present yet every enable/start path is refused — so "advertised"
≠ "usable"; the entitlement to actually run it is gated firmware-side.

### RData status codes (from the decompiled app)

`0 SUCCESS · 3 INVALID_SUBTAG · 5 NOT_INITIALIZED · 7 RECORDING_ON ·
11 SYNC_NOT_IDLE · 12 MEMORY_FULL`. These apply to the op replies (subtag 2/3/4).
The STATE query (subtag 5) reply uses a *different* enum
(`0 IDLE · 1 SCHEDULED · 2 RECORDING · 3 STOPPED · 4 BUSY`).

## The safe harness we built

Two diagnostic CLI actions on `oura rdata` (auth-gated; both **always** tear down
with `stop` + `clear`, even on error, and re-read state afterward):

- `oura rdata probe` — sync time → arm ACM @ 50 Hz → record N s → read battery
  delta → drain pages (before stop, counting bytes/page size) → stop → clear.
- `oura rdata sweep` — try several `configure` argument variants, checking after
  each whether the ring left the idle state; tears down between attempts.

Starting RData remains intentionally absent from normal operation; these are
spike tools only. Wire helpers: `OuraClient::rdata_configure` / `rdata_get_page`
(added alongside the existing `rdata_state` / `rdata_stop` / `rdata_clear`).

## Experiments run (Ring 5, `50380B...`, firmware 2.1.3, on finger, battery 98%)

| Attempt | configure request (hex) | response | ring state after | result |
| --- | --- | --- | --- | --- |
| ACM 2g 50Hz, start=current=now, **no time sync** | `030a02<now><now>04` | `0203` | status 3 | idle, 0 pages, battery Δ0 over 10 min |
| ACM 2g 50Hz, start=current=now, **after `sync_time`** | `030a026bf63b6a6bf63b6a04` | `0203` | status 3 | idle, 0 pages, battery Δ0 over 30 s; page0 = `NO_DATA` (`00…`) |
| ACM 2g 50Hz, start=0, current=now | `030a0200000000<now>04` | `0203` | status 3 | still idle |
| Metadata + ACM, start=now | `030b02<now><now>0804` | `0203` | status 3 | still idle |
| Metadata + ACM, start=0 | `030b0200000000<now>0804` | `0203` | status 3 | still idle |
| ACM 8g 50Hz, start=now | `030a02<now><now>03` | `0203` | status 3 | still idle |

`<now>` = host UTC unix seconds, u32 LE. Request layout matches the documented
app `RDataStart` (`03 <len> 02 <start u32> <current u32> <type bytes…>`,
len = typecount + 9), so this is **not** a framing error.

## Analysis

- **Status `3` on CONFIGURE = `INVALID_SUBTAG`** (per the app's `RDataStatusCode`
  enum), i.e. the firmware will not accept the configure op — the expected
  symptom when the `RAW_DATA_SAMPLER` capability is disabled. (`stop` returns
  status `8`; an empty page read returns `6` = `NO_DATA`.) We have **never**
  observed a `RECORDING_ON` (7) state.
- **Our packet was correct all along.** The decompiled app sends the identical
  frame; the only difference is it uses `startTime=0` (we tried that too).
- **Time sync was not the blocker.** `sync_time` sets the ring clock to UTC unix
  seconds — the same domain `configure` uses — and changed nothing.
- **Battery impact is consistent with "nothing recorded"**: 0-point change over
  both a 10-minute and a 30-second window. (At ~1% resolution even real 50 Hz
  sampling might not move the gauge in 10 min, so this alone isn't proof — but
  combined with 0 pages it is.)
- **`Metadata` channel and `start=0` did not help**, ruling out the cheap
  "missing argument" hypotheses.

### Enable attempts on the correct id (all refused)

With `RAW_DATA_SAMPLER` advertised at version 1, we tried every enable path in one
BLE session (`oura rdata unlock`):

| Op | Result |
| --- | --- |
| `SetFeatureSubscription(0x12, FEATURE_DATA)` | `2` = `NOT_AVAILABLE` (precondition not met) |
| `SetFeatureMode(0x12, REQUESTED)` | `0x01` = `NOT_SUPPORTED` |
| `RDataClear` | `0` = SUCCESS |
| `RDataConfigure(start=0)` (same session) | `3` = `INVALID_SUBTAG` |

So the capability is *known* to the firmware (version 1) but **not entitled**: the
consumer app + app-auth key cannot turn it on.

### How it actually gets enabled (from the decompiled app)

The capability table originates **firmware/factory-side**; nothing the consumer
app or a reverse-engineered client can send grants it:

- Capability values are **read** from the ring via `GetCapabilities` (tag 0x2F,
  ext 0x01/0x02) into `(id, version)` pairs. **There is no set-capability op.**
- `SetFeatureMode` / `SetFeatureSubscription` only configure *already-entitled*
  capabilities; the ring is the gatekeeper and rejects ours (`NOT_SUPPORTED` /
  `NOT_AVAILABLE`).
- **No privileged/research auth tier** — one 16-byte app-auth key; `AuthResponse`
  has no escalation level.
- **The server never pushes capabilities to the ring.** `/client/config` *uploads*
  the ring's advertised caps as telemetry; the response carries app feature flags
  (`ConnectivityRdataSampling`, `research_data_collection_enabled` consent) that
  gate *app* behavior, not ring capability bits.
- App-side `r_data_autosync` / `r_data_autoschedule` prefs (which gate whether the
  app initiates RData) are **never written** by the shipping consumer app.

Realistically, enabling `RAW_DATA_SAMPLER` requires either **(1) a research/
enterprise firmware image** advertising it as entitled (via DFU ops), or **(2) a
manufacturing/factory provisioning step** writing the capability/entitlement table
(cf. `SetManufacturingInfo` 0x37 and the `PRODUCTION_TESTS_MISSING` auth response).

Conclusion: RData raw capture is a **research/enterprise-provisioned capability**.
On a consumer Ring 5 it is advertised but not entitled, and **cannot be unlocked
over the wire** with anything a normal ring + app-auth key exposes.

### Where the entitlement lives (artifact evidence)

- **The read/parse path ships in the consumer app.** `libnexusengine.so` defines a
  `RingDebugDataRawDataDump` table (`gyroscope_odr`, `gyroscope_range`,
  `raw_data_bytes`), and `libringeventparser.so` exports `parse_raw_data_dump`,
  `serialize_debug_data_raw_data_dump`, and a `RawDataSamplerSession_v1` protobuf.
  So a normal ring/app can *decode* raw dumps — only *starting* capture is gated.
- **Firmware is per-hardware-platform, not per-tier.** `ring_firmware.apk` carries
  builds keyed by generation (`cooper`, `gen2`, `gen2x`, `nomad`, `nomad2`,
  `oreo`) — no "research" variant. Researchers don't get a different firmware via
  the consumer channel.
- **No entitlement logic in the app's native libs** (grepped) — the check is in
  the ring firmware (signed/encrypted, unreadable here).
- The protocol exposes a **factory/manufacturing stage** (`SetManufacturingInfo`
  0x37; auth code `RESPONSE_ERROR_PRODUCTION_TESTS_MISSING`).

Best-supported model: a **per-device entitlement provisioned at manufacturing**
(or via a non-public research firmware/config) for rings Oura ships into research/
enterprise studies. Effectively the sampler works **only on specific Oura-
provisioned rings**; a retail ring cannot be converted from the outside. (We
cannot prove the exact internal gate — per-device flag vs. non-public build —
because it lives in encrypted firmware; but every path a client could reach
[commands, server, auth, the public firmware images] is ruled out.)

## Capacity estimate (still theoretical — could not be measured)

Until recording starts, the best we have is the bound from data rate × flash:

| Mode | Effective rate (incl. ~30% framing) | per minute |
| --- | --- | --- |
| ACM 50 Hz (xyz i16) | ~400 B/s | ~24 KB |
| ACM + Gyro 50 Hz | ~800 B/s | ~48 KB |

The page index is a `u16` (≤ 65536 pages), so addressing alone allows up to
~16 MB at 256 B/page; the real ceiling is whatever flash is allotted to the
RAW_DATA buffer, which the `state` response does **not** report. Plausible
buffer sizes (512 KB–4 MB) put ACM-only runtime somewhere between **~20 min and
~3 hr**. A 27-minute swim is therefore *possibly* in range ACM-only, tight-to-
impossible with gyro added — but this stays a guess until a real session fills.

What happens at saturation (halt vs. circular overwrite) is likewise unknown and
only a fill-to-limit run can answer it.

## Next steps

All cheap wire tests are now exhausted and conclusive (see tables above): strict
`Clear`→`Configure` recipe (`oura rdata recipe`), capability re-read with the
corrected id, and in-session enable + start (`oura rdata unlock`) — every path is
refused. The capability is firmware-entitlement-gated.

What remains is **out of scope for a consumer ring**:
- A **research/enterprise-provisioned ring** (firmware/factory) that advertises
  `RAW_DATA_SAMPLER` as *entitled* — then `oura rdata probe` would measure rate,
  page size, and saturation directly, and the swim ML pipeline becomes reachable.
- Absent that, there is **no high-rate raw path on this ring**: realtime ACM is
  BLE-only (dies underwater), and RData is entitlement-locked. Swim analytics is
  therefore capped at the 30 s windowed envelope (Phase 1).

## Cross-references

- `docs/sync-orchestration.md` — RData is `r_data_autosync=false`, a research/
  diagnostics opt-in; never used in normal wear.
- `docs/horizon-ring3-protocol-cheatsheet.md` — the original `0x03` probes
  (status 3 on start-none-zero) that this spike extends and confirms.
- `crates/oura-protocol/src/protocol.rs` — `rdata` module (subtags, `DataType`).
- `crates/oura-cli/src/main.rs` — `rdata_probe` / `rdata_sweep` harness.
