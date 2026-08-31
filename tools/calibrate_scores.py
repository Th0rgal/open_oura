#!/usr/bin/env python3
"""Calibrate the score model ONCE from a trends export and persist it to
local/score_params.json, so the live scorers never need the CSV at runtime.

Uses **ring-compatible** drivers only (LIVE_DRIVERS) — every input here is one the
live scorers can compute from ring data + accumulated baselines (see build_daily.py),
unlike the analysis-only drivers in fit_scores_all.py. The params file embeds each
contributor's drivers + fitted curve, so the scorers stay driver-agnostic.

Output is per-user calibration (personal physiology) → gitignored local/.
Re-run with a fresh/longer export:  python tools/calibrate_scores.py --csv local/trends.csv
"""
import argparse
import csv
import json
from pathlib import Path

import numpy as np

from fit_scores_all import WEIGHTS, engineer_seq
from fit_sleep_score import engineer, fit_curve, getval

REPO = Path(__file__).resolve().parent.parent

# contributor -> (ring-derivable driver columns, kind). Baseline-relative
# contributors pair today's value with its trailing-14-day baseline so the linear
# fit forms "today vs personal baseline". "mean" = constant fallback where the raw
# driver isn't in the export (Recovery Index sub-score) or is goal/multi-day gated.
LIVE_DRIVERS = {
    # Sleep (all ring-derivable from the SleepNet hypnogram + bedtime)
    "Total Sleep Score": (["Total Sleep Duration"], "mono"),
    "Sleep Efficiency Score": (["Sleep Efficiency"], "mono"),
    "REM Sleep Score": (["REM Sleep Duration"], "mono"),
    "Deep Sleep Score": (["Deep Sleep Duration"], "mono"),
    "Sleep Latency Score": (["Sleep Latency"], "emp"),
    "Restfulness Score": (["awake_frac", "Sleep Efficiency"], "lin"),
    "Sleep Timin Score": (["mid_opt", "mid_reg", "mid_hour"], "lin"),
    # Readiness
    "Previous Night Score": (["Sleep Score"], "mono"),
    "Resting Heart Rate Score": (["Lowest Resting Heart Rate", "Average Resting Heart Rate",
                                  "trail14_Average Resting Heart Rate"], "lin"),
    "HRV Balance Score": (["Average HRV", "trail14_Average HRV"], "lin"),
    "Temperature Score": (["Temperature Deviation (°C)"], "emp"),
    "Sleep Balance Score": (["Total Sleep Duration", "trail14_Total Sleep Duration"], "lin"),
    "Previous Day Activity Score": (["prev_Average MET"], "mono"),
    "Activity Balance Score": (["prev_Average MET", "trail14_Average MET"], "lin"),
    "Recovery Index Score": ([], "mean"),   # raw recovery-index not in export → fallback constant
    # Activity (goal/training-load gated — calibrated best-effort, not live-scored)
    "Move Every Hour Score": (["Long Periods of Inactivity"], "emp"),
    "Stay Active Score": (["Inactive Time"], "mono"),
    "Meet Daily Targets Score": (["Activity Burn", "Steps", "Average MET"], "lin"),
    "Training Volume Score": ([], "mean"),
    "Training Frequency Score": ([], "mean"),
}


def main():
    ap = argparse.ArgumentParser(description="Calibrate score params from a trends CSV")
    ap.add_argument("--csv", default=str(REPO / "local" / "trends.csv"))
    ap.add_argument("--out", default=str(REPO / "local" / "score_params.json"))
    args = ap.parse_args()
    csv_path = Path(args.csv)
    if not csv_path.exists():
        raise SystemExit(f"trends CSV not found: {csv_path} (export from your Oura account)")

    rows = engineer_seq(engineer(list(csv.DictReader(open(csv_path)))))
    params = {"source": csv_path.name, "weights": WEIGHTS, "contributors": {}}
    n_used = 0
    for score, weights in WEIGHTS.items():
        for c in weights:
            drivers, kind = LIVE_DRIVERS[c]
            data = [{k: getval(r, k) for k in drivers + [c]} for r in rows]
            data = [d for d in data if all(v is not None for v in d.values())]
            if len(data) < 20:
                print(f"  skip {c}: only {len(data)} rows")
                continue
            y = np.array([d[c] for d in data])
            if kind == "mean":
                drv = None
            elif kind in ("mono", "emp"):
                drv = np.array([d[drivers[0]] for d in data])
            else:
                drv = np.column_stack([[d[dd] for d in data] for dd in drivers])
            curve = fit_curve(kind, (drv, y))
            params["contributors"][c] = {"drivers": drivers, "kind": kind, "curve": curve.to_dict()}
            n_used = max(n_used, len(data))

    params["n_days"] = n_used
    Path(args.out).write_text(json.dumps(params, indent=1))
    print(f"calibrated {len(params['contributors'])} contributors from {csv_path.name} "
          f"(≤{n_used} days) → {Path(args.out).relative_to(REPO)}")


if __name__ == "__main__":
    main()
