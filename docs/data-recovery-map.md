# Oura Data Recovery Map

What can be recovered directly from an Oura ring over BLE, what the ring computes
itself, and what only Oura's cloud produces. Derived from the decompiled Android
app (`com.ouraring.oura`, ourakit SDK) cross-checked against live captures from a
Ring 3 Horizon and a Ring 5.

## Three layers

Oura is a **ring -> app -> cloud** pipeline:

- **Ring**: sensors + on-device summarization (including sleep staging and MET).
- **App**: transport + renderer. Almost no analytics; it uploads ring events and
  downloads finished daily documents.
- **Cloud** (`api.ouraring.com`, `cloud.ouraring.com`, `assa.ouraring.com`,
  `mlops.ouraring.com`): all 0-100 scores and ML classification.

The decisive tell: every daily document the app stores carries an
`*_algorithm_version` field (e.g. `JsonDbDailyReadiness.sleep_algorithm_version`),
proving the value was produced by a versioned server-side algorithm and synced
down as immutable JSON.

## What the ring emits over BLE

Three channels, used at different times (see `sync-orchestration.md`).

### A. History events (the main channel)

Fetched with `GetEvent` (tag `0x10`/`0x11`, legacy) or `ExtGetEvent`
(tag `0x2f`, extended), NORMAL buffer. Each event is `tag | length | payload`,
payload starting with a 4-byte LE timestamp. Tag -> type map is in
`tools/oura_protocol.py` `EVENT_TAGS` and the protobuf schema
`com/ouraring/ringeventparser/Ringeventparser.java`.

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

## What only the cloud produces

| Metric | Where | Evidence |
| --- | --- | --- |
| Readiness score (0-100) + contributors | Cloud | `JsonDbDailyReadiness` DTO, `sleep_algorithm_version` |
| Sleep score | Cloud | `JsonDbDailySleep` (`score`, `sleep_debt`) |
| Activity score, calories, MET-minutes | Cloud | `JsonDbDailyActivity` (`score`, `active_calories`, `met` vs `ring_met`) |
| Daytime / cumulative stress | Cloud | `JsonDbDailyStress`, `DbDailyCumulativeStress` |
| Workout auto-detection ("confirm activity") | Cloud ML | `POST /api/activity-tagging/v2` -> `activity_id` + `confidence` |

The ring supplies raw MET + accelerometer segments; the cloud classifies and
scores. Calories are cloud-derived from ring MET + the user profile.

## Bottom line for an independent client

Recoverable from the ring without Oura's cloud: raw PPG/accel/gyro/temp, live HR
(IBI->BPM), SpO2, IBI/HRV, on-device sleep stages, MET levels + steps, battery,
device info. NOT recoverable: the 0-100 scores and workout classification --
those are Oura-cloud-only and would have to be reimplemented locally.
