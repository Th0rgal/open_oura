# Ring features: capabilities, modes, and what's on by default

Two different things are easy to confuse:

- **Capability** (`GetCapabilities`, `FeatureCapabilityId`): does the firmware
  *support* a feature, and at what *version*. Read-only; the version is **not**
  an on/off flag. Provisioned firmware/factory-side (see
  `docs/rdata-capacity-probe.md` for the research-gated ones).
- **Feature mode** (`SetFeatureMode` / `GetFeatureStatus`): the *runtime* on/off
  state. This is what determines whether the ring actually produces the data.

## FeatureMode values

| mode | meaning |
| --- | --- |
| `OFF` (0) | disabled; ring computes nothing |
| `AUTOMATIC` (1) | ring runs it in the background when worn, logs results for sync — the "always-on" mode |
| `REQUESTED` (2) | produced on demand / in a specific context (e.g. an active workout) |
| `CONNECTED_LIVE` (3) | streamed live while BLE-connected (e.g. live HR readout) |

## What's on by default (read from a consumer Ring 5, fw 2.1.3)

`oura feature-status` reports the real runtime mode:

| feature | id | mode | default? | produces |
| --- | --- | --- | --- | --- |
| DAYTIME_HR | 0x02 | **AUTOMATIC** | ✅ on for everyone | background daytime HR |
| SPO2 | 0x04 | **AUTOMATIC** | ✅ on for everyone | `spo2_r_pi` events |
| RESTING_HR | 0x08 | **AUTOMATIC** | ✅ on for everyone | resting HR |
| REAL_STEPS | 0x0b | **AUTOMATIC** | ❌ off by default (server-flag gated) — we enabled it | `real_step_event_feature_1/2` (0x7e/0x7f) → stepmotion |
| EXERCISE_HR | 0x03 | **AUTOMATIC** | ❌ not in default set — we enabled it | `ehr_trace_event` 0x73 + `ehr_acm_intensity_event` 0x74 |
| CVA_PPG_SAMPLER | 0x0d | **AUTOMATIC** | ❌ not in default set — we enabled it | **`cva_raw_ppg_data` 0x81** (raw PPG → CVA model; see [cva-cardiovascular-age.md](cva-cardiovascular-age.md)) |
| EXPERIMENTAL | 0x0c | **AUTOMATIC** | ❌ not in default set — we enabled it | **— (firmware-only gate; no app-visible event type)** |

A *stock* ring has **DAYTIME_HR + SPO2 + RESTING_HR** running automatically (plus
the always-on base streams: motion 0x47, temperature 0x46, MET/activity 0x50,
IBI/HR 0x60/0x80). On **our** ring we additionally enabled REAL_STEPS, EXERCISE_HR,
CVA_PPG_SAMPLER and EXPERIMENTAL — all now AUTOMATIC (verified live with
`oura feature-status`).

**What each enabled feature actually produced** (cross-checked: enum
`FeatureCapabilityId` → event tag `RingEventType`, plus the `feature_session`
volumes seen in our sync):

| feature (id) | event(s) it produces |
| --- | --- |
| DAYTIME_HR (0x02) | `ibi_and_amplitude` 0x60, `green_ibi_quality` 0x80 |
| EXERCISE_HR (0x03) | `ehr_trace` 0x73, `ehr_acm_intensity` 0x74 |
| SPO2 (0x04) | `spo2_r_pi` 0x8b (R-ratio + PI; → [spo2-calibration.md](spo2-calibration.md)) |
| REAL_STEPS (0x0b) | `real_step_event_feature_1/2` 0x7e/0x7f |
| CVA_PPG_SAMPLER (0x0d) | `cva_raw_ppg_data` 0x81 (raw PPG) |
| **EXPERIMENTAL (0x0c)** | **nothing decodable — see below** |

**EXPERIMENTAL adds no app-visible data.** Empirically it emits **zero**
`feature_session` events and **no** event type of its own; in the decompiled app
`CAP_EXPERIMENTAL` is referenced by no parser, no `RingEventType` tag, and no
`RdataFeatureId`. It only flips a firmware switch (`ENABLE_EXPERIMENTAL_FEATURES`,
gated server-side by `firmware/experimental`). Whatever it changes is internal to
the firmware and opaque to us — enabling it did **not** add any extra stream we can
read.

## Enable conditions (from the Android app)

- `SetFeatureMode` only works for capabilities the ring *advertises*; the ring is
  the gatekeeper (rejects with `NOT_SUPPORTED` / `NOT_AVAILABLE`).
- **REAL_STEPS** is gated by a **server feature flag**
  (`FeatureDefinitions.ActivityRealSteps`) in the downloaded client config; the
  app enables it via `setFeatureMode(CAP_REAL_STEPS, AUTOMATIC)` as a prerequisite
  for AWHR. Off by default on our ring until that flag turns it on — which is why
  we enable it directly. (Gate is server-controlled — staged rollout / account /
  possibly membership; not a hard paid-membership check we can confirm.)
- The research/raw capabilities (`RAW_DATA_SAMPLER` 0x12, `RESEARCH_DATA` 0x01)
  are entitlement-locked firmware-side and cannot be enabled over the wire — see
  `docs/rdata-capacity-probe.md`.
### Per-feature default / gate / mode (from the decompiled app)

Master gate: `f2.setFeatureMode()` first requires ring generation > 2
(`ringconfiguration.s.j`); on older rings every call returns `NOT_SUPPORTED`.
The enable sequence lives in `b0.smali` method `r()`: ENABLE_DAYTIME_HR →
ENABLE_SPO2 → ENABLE_BUNDLING → ENABLE_REAL_STEPS → ENABLE_EXPERIMENTAL →
ENABLE_PPG_SAMPLER → ENABLE_AWHR → UPDATE_CHARGING_PROFILE → ENABLE_AMBIENT_LIGHT.

| feature | default ON for everyone? | gate / condition |
| --- | --- | --- |
| DAYTIME_HR (0x02) | **yes** | Gen3+ only; no flag, no toggle |
| EXERCISE_HR / AWHR (0x03) | **yes (opt-out)** | Gen3+; cap ver ≥ 2; user `awhrEnabled` (default TRUE); **enables REAL_STEPS first as prerequisite** |
| SPO2 (0x04) | **yes (opt-out)** | Gen3+; server flag `health/spo2`; user SpO2 toggle (default TRUE) |
| RESTING_HR (0x08) | n/a | app never toggles it; firmware-computed during sleep |
| REAL_STEPS (0x0b) | **no** | Gen3+; server flag `activity/real_steps` (default false) |
| CVA_PPG_SAMPLER (0x0d) | **no** | Gen3+; server flag `heart_health/ppg_sampler` (default false) |
| EXPERIMENTAL (0x0c) | **no** | Gen3+; server flag `firmware/experimental` (default false) |
| AMBIENT_LIGHT (0x10) | **no** | Gen3+; server flag `health/ambient_light`; cap must be present |

All `FeatureDefinitions.*` server flags default **false** client-side — the
effective value comes from the per-user `ClientConfiguration` Oura delivers.

**Why EXERCISE_HR read OFF despite being "default-on":** AWHR enables `REAL_STEPS`
first, but `REAL_STEPS` is server-flag-gated and was off → the chain never ran, so
EXERCISE_HR stayed off too. Forcing REAL_STEPS + EXERCISE_HR by hand reconstructs
the AWHR setup.

**EXPERIMENTAL (0x0c)** = server-controlled "experimental firmware features"
switch (`firmware/experimental`): flips a firmware-internal behavior. Off for
normal consumers; only on for opted-in cohorts. **It produces no app-visible event
type** (no parser / tag / RdataFeatureId references it; zero `feature_session`
events in our data) — whatever it changes stays inside the firmware and is opaque
to us. See the feature→event table above.

### What we enabled by hand, and what stayed locked

`SetFeatureMode(…, AUTOMATIC)` results on our consumer Ring 5:

| feature | result |
| --- | --- |
| real_steps, exercise_hr, cva_ppg, experimental | **SUCCESS** (now AUTOMATIC) |
| research_data (0x01) | rejected `0x02` NOT_AVAILABLE |
| raw_data (0x12) | SetFeatureMode says SUCCESS, **but RData CONFIGURE still `INVALID_SUBTAG`** → sampler stays entitlement-locked; the mode change is a no-op. Reset to OFF. |
| atlas (0x15), ambient (0x10) | rejected `0x01` NOT_SUPPORTED |

So the full consumer + experimental set is now on; the research/raw entitlements
remain unreachable (confirms `docs/rdata-capacity-probe.md`).

Key files: `…/data/device/ring/b0.smali` (`r()` enable sequence), `f2.java`
(`setFeatureMode`, gen gate, `enableRealSteps`), `k2.java`/`j2.java` (AWHR),
`core/features/ringconfiguration/s.java` (gen/version gates),
`core/model/backend/FeatureDefinitions.java` (server flags),
`ourakit/domain/FeatureMode.java` (mode enum).

## Commands

- `oura feature-status` — read the real on-ring mode of the data features.
- `oura feature-mode <feature> --mode <off|automatic|requested|connected_live>` —
  set a feature's mode (consumer-feature enable path; needs `--key-file`).
  Accepts names (`real_steps`, `exercise_hr`, `daytime_hr`, `resting_hr`,
  `cva_ppg`, …) or a raw `0xNN` id.
- `oura subscribe <feature> --mode <…>` — SetFeatureSubscription (subscribe to a
  feature's data events; distinct from enabling its mode).

After enabling a feature: wear the ring, let it run, then `oura sync` to pull the
new events.
