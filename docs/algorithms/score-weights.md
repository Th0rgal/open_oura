# Daily-score combiner weights (recovered by calibration)

The ecore combiner is `round(Σ wᵢ·contributorᵢ / 100)`, weights summing to 100.
The weight tables don't read back from this APK's `libappecore.so` `.rodata`, so we
recover them by regressing each final score on its contributor sub-scores from an
Oura account **Trends** export (`tools/fit_scores.py`). 336–348 days, no intercept,
unconstrained OLS (the fit's Σ≈1.0 confirms it really is a weighted average).

One genuine app constant *is* extractable as a raw asset:
`reverse/android/.../res/raw/scorepercentiles.csv` — the population percentile→score
table (used for "you're in the Nth percentile"), not the combiner.

## Sleep Score — R²=0.9987, max err 0.8, 100% within ±1  ✅ essentially exact

| weight | contributor |
| --- | --- |
| 35% | Total Sleep |
| 15% | Restfulness |
| 10% | Sleep Efficiency |
| 10% | REM Sleep |
| 10% | Deep Sleep |
| 10% | Sleep Latency |
| 10% | Sleep Timing |

## Readiness Score — R²=0.969, max err 12.6, 89% within ±1  ◐ linear + a nonlinearity

| weight | contributor |
| --- | --- |
| ~17% | Resting Heart Rate |
| ~15% | Previous Night |
| ~15% | HRV Balance |
| ~13% | Temperature |
| ~12% | Sleep Balance |
| ~10% | Previous Day Activity |
| ~10% | Recovery Index |
| ~7% | Activity Balance |

The large-residual days (up to 12 pts low) point to a cap / rest-recovery-mode
override on top of the weighted average (cf. `rest_recovery_* @ 0x20bf38` and the
legacy/modern readiness paths in [`README.md`](README.md)) — not yet modelled.

## Activity Score — R²=0.904, max err 11  ◐ piecewise combiner, linear is approximate

| weight | contributor |
| --- | --- |
| ~33% | Move Every Hour |
| ~24% | Meet Daily Targets |
| ~17% | Stay Active |
| ~15% | Training Volume |
| ~12% | Training Frequency |

Activity uses a per-contributor piecewise interp (Y=[0,25,95,100],
`get_activity_score_raw @ 0x1d5788`) before combining, so a flat linear fit only
approximates it.

## Contributor curves — Sleep Score end-to-end from raw metrics

`tools/fit_sleep_score.py` adds the second layer: each sub-score = fᵢ(raw metric),
fit from the export, then combined with the weights above. Held-out (20%) results:

| weight | contributor | method | held-out R² | driver(s) |
| --- | --- | --- | --- | --- |
| 35% | Total Sleep | isotonic | 0.984 | Total Sleep Duration |
| 10% | Sleep Efficiency | isotonic | 0.999 | Sleep Efficiency |
| 10% | REM Sleep | isotonic | 1.000 | REM Sleep Duration |
| 10% | Deep Sleep | isotonic | 0.998 | Deep Sleep Duration |
| 10% | Sleep Latency | empirical (U-curve) | 0.993 | Sleep Latency |
| 10% | Sleep Timing | linear | 0.805 | midpoint dist-from-03:00, 7-day regularity, midpoint hour |
| 15% | Restfulness | linear | 0.704 | awake fraction, restless fraction, efficiency |

**End-to-end Sleep Score (held-out days):**
- ceiling (weights × Oura's *actual* sub-scores): R²=0.998, max err 0.7
- achieved (weights × *our* sub-scores from raw): **R²=0.964, RMSE 1.38, max err 4.5,
  84% within ±1 pt, 94% within ±3.**

Key finds: Sleep Timing is circadian — a **peaked curve maximised at a ~03:00
midpoint** (corr +0.91) plus **day-to-day regularity** (corr −0.73); Sleep Latency
is a non-monotone U-curve (very short and very long both penalised).

## The exact ceiling — what blocks a bit-exact match

5 of 7 contributors (75% of weight) reconstruct essentially exactly. The residual
is two contributors whose true inputs the export doesn't expose:

- **Restfulness (15%)** caps at R²≈0.70 — Oura drives it from internal
  **restless-period / wake-up counts** computed from movement, not the night
  aggregates (`Awake Time`, `Restless Sleep`) we have.
- **Sleep Timing (10%)** residual ≈0.8 — the personal circadian ideal + regularity
  aren't fully captured by midpoint alone.

Path to exact: compute those two from the **ring's own movement + hypnogram**
(`sleep_acm_period` / `motion` events + SleepNet staging), which are finer-grained
than the export aggregates — a follow-on that uses ring data, not this CSV.

## Live scoring from ring data — `oura sleep-score`

`tools/score_sleep.py` (and the `oura sleep-score` CLI command) compute a night's
Sleep Score live, no cloud:

1. SleepNet (`run_sleep_model.py`) → hypnogram → total-sleep / efficiency / REM /
   deep durations + sleep latency (first sustained-sleep epoch).
2. `bedtime_period` history → real-clock sleep midpoint (from the hypnogram's
   local times) + 7-day regularity (a deciseconds delta, phase-independent).
3. contributor curves + weights (calibrated from the trends export) → sub-scores →
   `round(Σ wᵢ·subᵢ / 100)`.

Restfulness uses only ring-derivable drivers (awake fraction + efficiency); its
movement micro-inputs aren't reproducible from the export's units and add little
once awake-fraction is in. Example (latest night in `oura.db`): 471 min asleep,
91% efficiency → **Sleep Score 82** (Total 83 · Restful 73 · Eff 96 · REM 97 ·
Deep 40 · Latency 92 · Timing 99). Calibration CSV auto-found at
`~/Desktop/oura_*trends.csv` or passed with `--csv`.

## Readiness & Activity end-to-end (`tools/fit_scores_all.py`)

Same approach as Sleep, with lag-1 and trailing-mean features engineered so a linear
fit can form "today vs personal baseline". Held-out results:

| score | ceiling (weights × actual subs) | achieved (subs from raw) |
| --- | --- | --- |
| Sleep | R²=0.998 | **R²=0.969** (85% within ±1) |
| Readiness | R²=0.941 | **R²=0.49** (56% within ±3) |
| Activity | R²=0.838 | **R²=0.06** |

The **ceiling** (≈0.84–0.998) confirms the recovered weights are right for all three.
The **achieved** gap is entirely about which contributors are a function of a *single
day's* exposed metrics vs need history/hidden inputs:

**Readiness** — reconstructs the direct contributors, not the baseline-relative ones:
- ✅ Previous Night R²=0.96 (= that night's Sleep Score), Temperature R²=0.995,
  Sleep Balance R²=0.74.
- ❌ HRV Balance 0.21, Resting HR 0.31, Recovery Index ~0, Activity Balance 0.30 —
  these compare today against a personal ~2-week **baseline trajectory** (an EMA we
  don't carry), so current-value + trailing-mean can't reproduce them. ~50% of weight.

**Activity** — only the within-day inactivity contributors reconstruct:
- ✅ Stay Active R²=0.99 (← inactive time), Move Every Hour R²=0.83 (← long inactive
  periods).
- ❌ Meet Daily Targets (24% weight) tracks a **personalised daily goal** not in the
  export (no single-day driver |corr|>0.2); Training Volume/Frequency (27%) are
  near-constant 100 here and encode **multi-day training load**. So Activity does not
  reconstruct end-to-end from single-day columns.

Takeaway: the weights are recovered for all three scores, and the contributor-curve
method is exact where a contributor is a function of the day's physiology (all of
Sleep bar two; Readiness's direct half; Activity's inactivity half). The rest is
gated on **accumulated personal baselines/goals**, not on missing logic — the same
wall that blocks live Readiness/Activity until weeks of local history accrue.

## Live scoring from the ring

`oura sleep-score` computes Sleep live today. `oura readiness-score` computes
Readiness live from a per-day summary + rolling baselines accrued nightly — with the
baseline-relative contributors flagged "provisional" until ~14 days of history mature.
The substrate (daily summaries, baselines, Recovery Index, persisted calibration) is
documented in [`daily-summaries-and-baselines.md`](daily-summaries-and-baselines.md).
Calibration is fit once by `tools/calibrate_scores.py` into `local/score_params.json`,
so the scorers don't need the trends CSV at runtime.
