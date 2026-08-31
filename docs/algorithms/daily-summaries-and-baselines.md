# Daily summaries, baselines & live Readiness

To compute the **baseline-relative** score contributors (HRV Balance, Resting-HR,
Sleep/Activity Balance), an independent client must carry the same per-day state
`ecore` accumulates on-device: a daily summary plus rolling personal baselines.
This is the substrate that turns Readiness/Activity from "weights-solved" (see
[`score-weights.md`](score-weights.md)) into *live from the ring*.

## Where the missing inputs come from

Almost nothing extra is needed from the ring — we already capture the raw signals.
The gap was **accumulated local state**, plus one app/account setting:

| Contributor input | Source | How it's produced now |
| --- | --- | --- |
| HRV (nocturnal) | Ring `hrv_event` 0x5d | mean nocturnal RMSSD |
| Resting HR | Ring IBI 0x60 | overnight HR → low/avg |
| **Recovery Index** | Ring IBI 0x60 | hours between RHR minimum and wake (single night) |
| Skin temperature | Ring `temp_event` 0x46 | mean nocturnal temp − trailing baseline |
| Activity MET | Ring `activity_information` | mean MET over the day |
| HRV/RHR/temp/sleep/MET **baselines** | **local state** | trailing-14-day mean, accrued nightly |
| Activity goal (Meet Daily Targets) | **app/account** (`DbDailyActivity.target_calories`, adaptive) | not reproduced — Activity stays goal-gated |

## Pipeline

```
oura sync                      # raw events → oura.db (nightly)
tools/calibrate_scores.py      # trends export → local/score_params.json   (one-time / per export)
tools/build_daily.py           # per night: SleepNet + HRV/RHR/recovery/temp/MET → daily_summary + baselines
tools/score_readiness.py       # daily_summary + baselines + params → Readiness Score
```

`oura sleep-score` and `oura readiness-score` wrap this; `readiness-score` rebuilds
`daily_summary` first, then scores.

### Calibration is persisted (no CSV at runtime)

`tools/calibrate_scores.py` fits the combiner weights + every contributor curve from
a trends export **once** and writes `local/score_params.json` (gitignored — personal
calibration). All live scorers load that file, so they never need the CSV again. Put
the export at `local/trends.csv` (or pass `--csv`) and re-run after a longer export.
Drivers are **ring-compatible** (`LIVE_DRIVERS`): e.g. Restfulness uses awake-fraction
+ efficiency, not the movement micro-inputs the analysis-only `fit_scores_all.py` uses.

### `daily_summary` table (in oura.db)

One row per bedtime night: sleep metrics + `sleep_score`, `hrv_avg`, `rhr_low/avg`,
`recovery_index_h`, `temp_mean`/`temp_dev`, `met_avg`, the five trailing-14-day
baselines (`hrv/rhr/temp/sleep/met_baseline`), and `n_history` (days of prior data).
Re-runnable; baselines are causal (trailing only).

### Recovery Index (new, single-night)

From the overnight HR series (IBI → bpm, rolling-median smoothed) we find when resting
HR bottoms out and report **hours between that minimum and wake** — Oura's Recovery
Index (the earlier RHR settles, the more recovered). No history needed. We compute the
raw hours today; mapping hours → 0-100 sub-score isn't calibratable from the export
(it has no raw recovery column), so that one sub-score uses a constant fallback (flagged).

## Maturity: baselines need ~14 days

The baseline-relative contributors compare today to a personal ~14-day baseline. With
fewer days the baseline is **cold** (falls back to the current value → neutral
deviation) and the Readiness number is **provisional** — the scorer flags each cold
contributor and prints how many days of history exist. After ~2 weeks of nightly sync
they mature and Readiness becomes as live as Sleep. Example on 6 days of history:

```
Readiness Score … history 6 day(s)  ⚠ baselines still maturing (<14d) — provisional
  inputs: HRV 111/92ms  RHR 34/35.6bpm  recovery 3.17h  tempΔ +0.23°C
  Resting Heart Rate Score   17%  100  17.0 ~baseline-cold
  HRV Balance Score          15%   82  12.4 ~baseline-cold
  …
  READINESS SCORE                          76
```

## What's still gated

- **Activity Score** — `Meet Daily Targets` tracks an adaptive personal goal in
  `DbDailyActivity.target_calories` (app/account, not on the ring); `Training
  Volume/Frequency` are multi-day training load. So Activity stays weights-solved but
  not live-scored. To unblock: read/replicate the goal (age-based default in
  `DbDailyActivityReference`) or expose it as a config value.
- **Recovery Index sub-score curve** — needs a raw-recovery↔sub-score pairing the
  trends export doesn't provide; we surface the raw hours and use a constant sub-score.
