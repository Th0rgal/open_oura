# Running Oura's on-device models on your synced data

The ring ships Oura's analysis as encrypted on-device PyTorch models. Once
decrypted locally (into the gitignored `notes/models/`), several of them run on
the signals we already sync into `oura.db`. This page indexes the runners and what
each can and can't do.

## Tools

| tool | model(s) | output | status |
| --- | --- | --- | --- |
| `oura sessions` / `tools/run_activity_model.py` | `automatic_activity_detection_3_1_11` | activity/workout segments + type label | ✅ runs |
| `tools/run_sleep_model.py` | `sleepnet_moonstone_1_2_0` | DEEP/LIGHT/REM/WAKE hypnogram + efficiency | ✅ runs |
| `tools/run_models.py bdi` | `sleepnet_bdi_0_4_0` | hypnogram + apnea/breathing-disturbance | ✅ runs |
| `tools/run_cva_model.py` | `cva_2_1_0` | cardiovascular (vascular) age + pulse-wave velocity | ✅ runs (needs `cva_ppg` on — see [cva-cardiovascular-age.md](cva-cardiovascular-age.md)) |
| `tools/run_spo2.py` | (no model — Oura's calibration) | overnight SpO2 % | ✅ runs (see [spo2-calibration.md](spo2-calibration.md)) |
| `tools/run_models.py daily_medians` | `daily_medians_1_1_0` | HRV/HR/temp/MET daily medians | ⚠️ runs but needs **awake** HRV (ours is nocturnal) |
| `tools/inspect_models.py` | all | dump each model's `forward()` schema | helper |

The two sleep models cross-validate (close DEEP/LIGHT/REM/WAKE %); that agreement
also fixes the BDI stage-column order to `[AWAKE, LIGHT, REM, DEEP]`.

## What's blocked, and why (data availability, not wiring)

Of the ~26 newest-version models, most are blocked on a signal we can't get:

- **raw PPG waveform** — `cva_2_1_0` was here until `cva_ppg` was enabled (now
  `cva_raw_ppg_data` 0x81 supplies it). Still blocked for `halite` (needs the same
  raw PPG wired) and `whr` (needs raw PPG + raw accel).
- **raw ACM / stepmotion gait** — `step_counter`, `steps_motion_decoder`,
  `awhr_imputation`, `awhr_profile_selector` (RData-gated; entitlement-locked).
- **bioZ + EDA sensors** — `atlas` (not in our event stream).
- **cloud-computed scores** — `stress_resilience`, `training_stress_score`.
- **cycle / pregnancy inputs** — `popsicle`, `pregnancy_biometrics`.
- **a custom C++ op** missing in the runtime — `sleepstaging_2_6_0`
  (`oura_ops::oura_create_windows`).
- **pipeline deps** (need another model's / cloud output first) — `cumulative_stress`,
  `cva_calibrator`, `stress_daytime_sensing`.

(The full per-model matrix lives in the local-only `notes/model-usage-map.md`.)

## Notes
- All runners read `oura.db` by default and reference `notes/models/` (both
  gitignored — no model weights or personal data are committed).
- The models are Oura's proprietary, decrypted artifacts; they are **not** included
  in the repo. You supply your own locally.
