# Algorithms (ecore port)

Oura computes its daily metrics **on the phone**, in the native `ecore` engine
(`libappecore.so`) - not in the cloud (the cloud is storage/sync). The one
exception is the **sleep hypnogram**: ecore *consumes* a pre-computed 30-second
stage array and only produces the staging *features*; the stager itself is the
on-device **SleepNet** PyTorch model (encrypted `*.pt.enc` in `oura_models.apk`)
and/or the ring firmware. Everything *downstream of staging* is deterministic and
portable.

This directory documents each metric we port into `oura-analysis`. Source
addresses refer to functions in the decompiled `libappecore.so` (see
`native-decoder.md` for the Ghidra method). Per-metric detail files are added as
each is ported; this index is the status table.

> **Models are not included in this repo.** Where the table says a metric runs via
> a decrypted PyTorch model (`tools/run_*.py`), those `.pt` files are Oura's
> proprietary IP and are **not committed** (gitignored under `notes/models/`) —
> you supply your own locally-decrypted copies. See
> [`docs/model-runners.md`](../model-runners.md).

## Status

| Metric | ecore source | Rust impl | Status |
| --- | --- | --- | --- |
| HRV (RMSSD) | `hrv @ 0x1e7984` (`sqrt(mean(diff(ibi)^2))`) | `oura-analysis::hrv` | ✅ ported + tested; validated on overnight IBI (RMSSD ~101 ms) |
| SpO2 (simple) | `spo2_simple_calculate @ 0x22ad50` (`a+b·R+c·R²`, clamp 0–120) | `oura-analysis::spo2` | ✅ ported + tested; needs per-device {a,b,c} |
| Personal baseline | `baseline_update_lt_mean_and_dev @ 0x1dad04` (asymmetric EMA, anneals by age; int16 ×8) | `oura-analysis::baseline` | ✅ ported + tested (EMA; per-metric clamp tables unresolved) |
| Nightly temperature + baseline | `nightly_temperature_calculate @ 0x203520`, `baseline_calculate_temperature_baseline @ 0x1db4d0` (7-sample median → 30-min window) | `oura-analysis::temperature` | ✅ ported + tested |
| Breathing rate | `breathing_rate_calculate_averages @ 0x27342c` (IBI→RR @4 Hz→12-tap IIR) | - | ◐ IIR coefficients recovered; 4 Hz resample kernel unresolved - deferred |
| Sleep durations / efficiency / latency | `calculate_sleep_score_numerical @ 0x1f4444` (decodes 30 s nibble stages) | `oura-analysis::sleep` | ✅ decode+summary ported (aggregation reconstructed) |
| Sleep score + contributors | `ecore_sleep_score_calculate @ 0x1f5c20`; limits `…_init_limits_v2 @ 0x1f5b20` (from age byte) | - | ◐ limits + sleep_timing recovered; contributor mapper `FUN_001f5e64` not in export - needs re-decompile |
| Sleep debt | `sleep_debt_calculate @ 0x215658` | `oura-analysis::sleep_debt` | ✅ ported + tested |
| Readiness score + contributors | `readiness_calculate @ 0x20897c`, `recovery_legacy_run @ 0x20b258` | - | ◐ legacy contributor formulas recovered; weight tables `0x17bff8`/`0x17bfdc` + modern `FUN_002094e0` need .rodata dump + re-decompile |
| Rest/recovery mode | `rest_recovery_* @ 0x20bf38…` | - | ⏳ to port |
| Activity score + contributors | `get_activity_score_raw @ 0x1d5788` (per-contributor pw-interp, Y=[0,25,95,100]); combiner `@ 0x1d781c` | - | ◐ contributor curves + X-tables recovered; final-combiner divisor ambiguous - needs careful re-read |
| Activity targets / cals / MET | `actinfo_target_to_cal @ 0x1cd2c8`, `actinfo_update_5_min_classification @ 0x1cd640` | `oura-analysis::metabolic` | ◐ VO2max/BMR/steps→m ported+tested; MET-class ordering + calorie/step regression best-effort |
| Cycle prediction / tracking | `cycle_prediction_calculate @ 0x1e2864`, `cycle_tracking_calculate @ 0x1e4244` | - | ◐ day-type thresholds + 0.18–0.30 sine band recovered; fit_sin/sine_from_range unresolved - deferred |
| **Sleep hypnogram (staging)** | SleepNet PyTorch model - not in ecore | `tools/run_sleep_model.py`, `tools/run_models.py bdi` | ✅ **decrypted + running**: we produce a DEEP/LIGHT/REM/WAKE hypnogram offline with the decrypted `sleepnet_moonstone_1_2_0` (and `sleepnet_bdi_0_4_0` for apnea). The model key was extracted, so decryption is no longer blocked. `sleepstaging_2_6_0` itself won't load (needs a custom op `oura_ops::oura_create_windows` our torch runtime lacks) — moonstone covers staging. See [model-runners.md](../model-runners.md) |

## Device vs cloud (corrected)

Earlier we read the `score` + `*_algorithm_version` JSON fields as cloud-computed.
They are actually computed **on-device** by ecore (versions `v1/v2/nssa/sleepnet`
identify *local* algorithms), written to the local Realm `DbSleep`/`DbDaily*`, then
**uploaded**; the cloud serves the same locally-produced doc back on sync. There is
**no network code in ecore** - all tuning is embedded `get_default_*` tables or
host-supplied via `set_*` setters.

**Reproducible offline by porting ecore:** sleep/readiness/activity scores, sleep
debt, durations/efficiency, nightly temperature + baseline, HRV/RMSSD, resting-HR
percentiles, breathing rate, simple SpO2, activity targets/calories/MET, cycle.

**Not from ecore alone:** the **hypnogram** (needs the SleepNet model or the ring's
staging), **SpO2 OVI** (NaN stub) and **BDI/apnea** scoring (delegated), and the
exact **algorithm-version + baseline-state history** required to match Oura
bit-for-bit.

## Persisted state an independent client must carry across days

ecore is stateless-per-call; the host re-injects state each night via typed
objects: recovery state (+prev), temperature baseline, previous sleep periods,
cycle-tracking state, and the SpO2 main storage (the only native binary serializer,
versioned TLV). Mirror these in `oura-store` so baselines accumulate.

## Score combiners: structure recovered, exact tables blocked → use calibration

The three 0–100 scores were chased to the combiner level: the sleep mapper
(`FUN_001f5e64`) is a per-contributor piecewise interp (X = age-derived limits
from `init_limits_v2`, shared Y-curve `DAT_0017bcb7`, weights `DAT_0017bcbf`
summing to 100, contributor 8 = `sleep_timing_score`, final `round(Σ wᵢ·cᵢ/100)`);
the activity combiner (`get_activity_score_from_raw_100`) and the legacy/modern
readiness paths are likewise resolved structurally. **But the constant tables they
read (`DAT_0017bcb7/bcbf`, readiness weights `0x17bff8/bfdc`, activity X-tables)
do not read back cleanly from this APK's `libappecore.so` build** - direct ELF
extraction yields non-monotonic/garbage values and several live in `.data`
populated at runtime. So an exact bit-match of Oura's scores isn't feasible from
this artifact alone.

**Practical path (validated): calibration.** We have all the raw inputs (sleep
durations, HRV, resting HR, temperature deviation, MET) and the user's trends
export pairs those with Oura's contributor sub-scores and final scores. A linear
fit reproduces the **Sleep Score at R²=0.999** (and Readiness/Activity close) - see
`data-recovery-map.md`. So `oura-analysis::score` will fit weights from the trends
CSV rather than porting the unreadable `.rodata`; the decompiled structure
(contributor set, the piecewise shape, the weighted-sum-/100 combination) guides
the model. Bit-exact reproduction would need a cleaner build's `.rodata` or
device-side calibration data.

**Done (combiner weights recovered):** `tools/fit_scores.py` regresses each final
score on its contributor sub-scores from a 336-day trends export — Sleep at
R²=0.9987 (weights 35/15/10/10/10/10/10, essentially exact), Readiness R²=0.969,
Activity R²=0.904. `tools/fit_sleep_score.py` then adds the contributor curves
(sub-score = f(raw metric)) to reconstruct the **Sleep Score end-to-end from raw
metrics at R²=0.964** (84% within ±1 pt; 5/7 contributors near-exact, residual in
Restfulness + Timing). See [`score-weights.md`](score-weights.md).
`tools/fit_scores_all.py` extends it to all three: weights recovered for each
(ceiling R²=0.84–0.998), end-to-end Sleep R²=0.97, Readiness R²=0.49, Activity
R²=0.06 — the gap is the baseline-relative / personalised-goal / multi-day-load
contributors that need accumulated history, not missing logic.

**Live from the ring:** `oura sleep-score` (today) and `oura readiness-score` —
the latter accrues a per-day summary + rolling baselines nightly (`tools/build_daily.py`),
including a from-scratch **Recovery Index** (overnight RHR-minimum→wake), and flags
baseline-relative contributors provisional until ~14 days mature. Calibration is
persisted once to `local/score_params.json`. See
[`daily-summaries-and-baselines.md`](daily-summaries-and-baselines.md).
