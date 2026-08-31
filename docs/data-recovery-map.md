# Oura Data Recovery Map

What can be recovered directly from an Oura ring over BLE, what the ring computes
itself, and what only Oura's cloud produces. Derived from the decompiled Android
app (`com.ouraring.oura`, ourakit SDK) cross-checked against live captures from a
Ring 3 Horizon and a Ring 5.

## Three layers

Oura is a **ring -> app -> cloud** pipeline:

- **Ring**: sensors + on-device summarization (including sleep staging and MET).
- **App**: the real analytics tier. The native `ecore` engine (`libappecore.so`)
  computes the 0-100 Sleep/Readiness/Activity scores + contributors, and the
  encrypted PyTorch models (`*.pt.enc`) compute staging, stress, CVA, etc. — all
  **on the phone**. The app writes the results to its local Realm DB and uploads
  them.
- **Cloud** (`api.ouraring.com`, `cloud.ouraring.com`, `assa.ouraring.com`,
  `mlops.ouraring.com`): storage/sync + model delivery. It round-trips the
  app's locally-produced documents and serves the model asset packs. The one
  genuine cloud-ML step is workout auto-classification (`activity-tagging/v2`).

The `*_algorithm_version` field on every daily document (e.g.
`JsonDbDailyReadiness.sleep_algorithm_version`) names the **local** algorithm
that produced it (`v1/v2/nssa/sleepnet`) — not a server version. We originally
misread this field as proof of cloud computation; see the corrected attribution
below and [`algorithms/README.md`](algorithms/README.md).

## What the ring emits over BLE

Three channels, used at different times (see `sync-orchestration.md`).

### A. History events (the main channel)

Fetched with `GetEvent` (tag `0x10`/`0x11`, legacy) or Ring 5's Android-style
`ExtGetEvent` (`0x2f`, buffer id 0), NORMAL buffer. Stored events are normalized
to `tag | length | payload`, payload starting with a 4-byte LE timestamp. Tag ->
type map is in `tools/oura_protocol.py` `EVENT_TAGS` and the protobuf schema
`com/ouraring/ringeventparser/Ringeventparser.java`.

`ExtGetEvent` returns `0x43` length-prefixed bundled events. The decoder expands
them back into the same normal packets as legacy `GetEvent` before inserting
rows, so downstream storage and body decoding are shared.

**Raw sensor sample events** (genuine measurements):

| Tag | Name | Carries |
| --- | --- | --- |
| `0x44`/`0x60` | ibi / ibi_and_amplitude | IBI (ms) + PPG amplitude |
| `0x71` | green_ibi_and_amplitude | IBI + amplitude, green LED |
| `0x6e` | spo2_ibi_and_amplitude | IBI + amplitude per SpO2 channel |
| `0x46`/`0x69`/`0x75` | temp / temp_period / sleep_temp | skin temperature (float C) |
| `0x47`/`0x6b` | motion / motion_period | accelerometer averages, intensity, orientation |
| `0x72` | sleep_acm_period | accel MAD statistics during sleep |
| `0x64`/`0x68`/`0x81` | raw_ppg / raw_ppg_data / cva_raw_ppg | raw PPG ADC samples |
| `0x6f`/`0x70`/`0x77` | spo2 / spo2_smoothed / spo2_dc | SpO2 % + raw optical DC |
| `0x5d` | hrv | 5-min avg RMSSD + avg HR |
| `0x62` | on_demand_meas | spot HR/HRV/breath/temp |

**Ring-computed summary events** (firmware does analysis on-device):

| Tag | Name | Carries |
| --- | --- | --- |
| `0x49`/`0x4c`/`0x4f`/`0x58` | sleep_summary_1..4 | bedtime, stage durations, lowest HR, contributors |
| `0x4b`/`0x4e`/`0x5a` | sleep_phase_* | hypnogram: enum {DEEP,LIGHT,REM,AWAKE} |
| `0x50`/`0x51`/`0x52` | activity_information/summary | 13 MET-level bins + step counts |
| `0x45`/`0x53` | state_change / wear | finger/wear state machine |

So **sleep staging and activity MET-binning happen on the ring**, not the cloud.

### B. Live / realtime (UI-driven only)

- **Live HR**: `SetFeatureMode(CAP_DAYTIME_HR, CONNECTED_LIVE)` -> ring pushes IBI
  notifications (tag `0x2f`, sub-tag `40`) -> app computes `BPM = 60000 / IBI_ms`,
  shown only when IBI validity == VALID. Stop with mode `AUTOMATIC`.
- **Feature latest values** (poll): `GetFeatureLatestValues` (tag `0x2f` ext `0x24`)
  for `CAP_DAYTIME_HR` (last IBI), `CAP_EXERCISE_HR` (direct bpm), `CAP_SPO2`
  (SpO2% + bpm), `CAP_CHARGING_CONTROL`.
- **Realtime measurements** (tag `0x06`): only ACM (accelerometer, bit `0x20`) and
  ON_DEMAND (`0x200`) actually stream. There is no HR bit here -- which is why
  enabling tag `0x06` modes ACKs but never streams HR.

### C. RData bulk raw download (opt-in research path)

`RDataStart`/`RDataGetPage` (tag `0x03`, RAW_DATA buffer). Streams full-rate raw
sensor data; `RDataRequestDataType` enumerates: PPG 50/125/250 Hz, ACM 2/4/8 G at
10/50 Hz, **gyroscope 125/500/2000 dps at 10/50 Hz** (not in Oura's public spec),
temperature 10 Hz / 10 s / 1 min. Gated behind the `r_data_autosync` pref
(default false) -- a normal user never triggers it.

## Where each derived metric is computed (corrected)

Everything below is computed **on the phone**, not in the cloud. The "Producer"
column names the on-device engine; the cloud only stores and round-trips the
result.

| Metric | Producer (on-device) | Evidence |
| --- | --- | --- |
| Sleep score + contributors | `ecore` (`ecore_sleep_score_calculate @ 0x1f5c20`) | `JsonDbDailySleep` (`score`, `sleep_debt`, `sleep_algorithm_version`) |
| Readiness score + contributors | `ecore` (`readiness_calculate @ 0x20897c`) | `JsonDbDailyReadiness` (`score`, `contributors`, `sleep_algorithm_version`) |
| Activity score, calories, MET-minutes | `ecore` (`get_activity_score_raw @ 0x1d5788`) | `JsonDbDailyActivity` (`score`, `active_calories`, `met` vs `ring_met`) |
| Sleep hypnogram (staging) | SleepNet PyTorch (`sleepnet_*`, `sleepstaging_*`, BDI) | `*.pt.enc` asset packs; ring also stages in firmware |
| Daytime / cumulative stress | PyTorch (`stress_daytime_sensing_1_1_0`, `cumulative_stress_1_2_2`) | `*.pt.enc`, not `isCloudOnly`; `TimeseriesDbDaytimeStress` |
| Resilience level | PyTorch (`stress_resilience_2_2_1`) | `*.pt.enc`; `DbDailyLongTermResilience` |
| Cardiovascular age / PWV | PyTorch (`cva_2_1_0`, `cva_calibrator`) | `*.pt.enc`; see [`cva-cardiovascular-age.md`](cva-cardiovascular-age.md) |
| **Workout auto-detection** ("confirm activity") | **Cloud ML** (the one real exception) | `POST /api/activity-tagging/v2` -> `activity_id` + `confidence` |

So the only genuinely cloud-originated value is **workout auto-classification**.
Note "on-device" still has practical gates for an *independent* client (below).

## Bottom line for an independent client

Recoverable from the ring without Oura's cloud: raw PPG/accel/gyro/temp, live HR
(IBI->BPM), SpO2, IBI/HRV, on-device sleep stages, MET levels + steps, battery,
device info. The 0-100 scores and stress/resilience/CVA are **not cloud-only** —
they are produced by `ecore` + the decrypted PyTorch models, which we have. What
still blocks a bit-exact reproduction is **inputs and constants**, not network:

- **Score constants**: ecore's `.rodata` weight/limit tables don't read back
  cleanly from this APK build, so scores are reproduced by *calibration* against
  an account trends export rather than ported bit-for-bit (Sleep R²=0.999). See
  [`algorithms/README.md`](algorithms/README.md).
- **Stress inputs**: `stress_daytime_sensing` needs **awake, sedentary daytime
  HRV** (validator rejects in-bedtime samples with code 6 "Sleep detected", and
  `ring_met > 1.8` with code 3). Our captures are mostly nocturnal; enable the
  `daytime_hr` feature to collect awake HRV. `tools/run_stress_model.py` runs the
  model on whatever awake samples exist.
- **Resilience inputs**: `stress_resilience` additionally needs a ~14-day history
  (`daily_stress_list` / `daily_restorative_time_list` / `daily_sleep_recovery_list`)
  plus the daily scores (`sleep_score`/`hrv_balance`/`recovery_index`) — so it
  needs weeks of accumulated local data, not a single night.
- **SpO2 OVI/BDI** scoring is delegated/NaN-stubbed; the hypnogram needs the
  SleepNet model (which we have) or the ring's own staging.
