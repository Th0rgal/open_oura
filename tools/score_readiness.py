#!/usr/bin/env python3
"""Compute a Readiness Score live from ring data — using the daily_summary +
rolling baselines built by tools/build_daily.py and the calibrated contributor
curves in local/score_params.json.

Readiness is mostly **baseline-relative**: HRV Balance, Resting-HR, Sleep/Activity
Balance compare today against your personal ~2-week baseline. Those baselines need
history to mature — with < ~14 days the score is provisional (flagged below). Two
contributors are honest fallbacks: Recovery Index uses a constant (we compute the
raw recovery hours, but the export has no raw→sub mapping to calibrate); MET-based
contributors need the day's activity, defaulted when absent.

Run build_daily first:  python tools/build_daily.py
Usage: python tools/score_readiness.py [DB] [--params P] [--json]
"""
import argparse
import json
import sqlite3
import sys
from pathlib import Path

from _common import resolve_db
from fit_sleep_score import load_params, score_from_params

REPO = Path(__file__).resolve().parent.parent
PARAMS = REPO / "local" / "score_params.json"
MET_DEFAULT = 1.4   # light-sedentary fallback when a day's activity is missing


def metrics_for(day, prev):
    """Map a daily_summary row (+ previous day) to the calibrated driver names.
    Cold baselines fall back to the current value (→ zero deviation, i.e. neutral)."""
    g = lambda k, alt=None: day[k] if day[k] is not None else alt
    prev_met = (prev["met_avg"] if prev and prev["met_avg"] is not None else g("met_avg", MET_DEFAULT))
    return {
        "Sleep Score": g("sleep_score", 70),
        "Average HRV": g("hrv_avg", 60),
        "trail14_Average HRV": g("hrv_baseline", g("hrv_avg", 60)),
        "Lowest Resting Heart Rate": g("rhr_low", 55),
        "Average Resting Heart Rate": g("rhr_avg", 58),
        "trail14_Average Resting Heart Rate": g("rhr_baseline", g("rhr_avg", 58)),
        "Temperature Deviation (°C)": g("temp_dev", 0.0),
        "Total Sleep Duration": g("total_sleep_sec", 27000),
        "trail14_Total Sleep Duration": g("sleep_baseline", g("total_sleep_sec", 27000)),
        "prev_Average MET": prev_met,
        "trail14_Average MET": g("met_baseline", prev_met),
    }


def main():
    ap = argparse.ArgumentParser(description="Live Readiness Score from ring data")
    ap.add_argument("db", nargs="?")
    ap.add_argument("--params", default=str(PARAMS))
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args()

    db = resolve_db(args.db, REPO)
    params = load_params(args.params)
    con = sqlite3.connect(str(db))
    con.row_factory = sqlite3.Row
    rows = con.execute("SELECT * FROM daily_summary ORDER BY start_ds").fetchall()
    con.close()
    if not rows:
        sys.exit("no daily_summary — run: python tools/build_daily.py")

    day, prev = rows[-1], (rows[-2] if len(rows) > 1 else None)
    metrics = metrics_for(day, prev)
    score, subs, contrib = score_from_params(params, "Readiness Score", metrics)

    n_hist = day["n_history"]
    cold = n_hist < 14
    baseline_relative = {"HRV Balance Score", "Resting Heart Rate Score",
                         "Sleep Balance Score", "Activity Balance Score"}

    if args.json:
        print(json.dumps({"readiness_score": score, "sub_scores": subs,
                          "n_history_days": n_hist, "baselines_mature": not cold,
                          "recovery_index_h": day["recovery_index_h"]}))
        return

    print(f"Readiness Score (live from ring, calibrated on {params.get('n_days', '?')} export days)")
    print(f"  night ds {day['start_ds']}   history {n_hist} day(s)"
          f"{'  ⚠ baselines still maturing (<14d) — provisional' if cold else ''}")
    print(f"  inputs: HRV {day['hrv_avg']:.0f}/{day['hrv_baseline'] or float('nan'):.0f}ms  "
          f"RHR {day['rhr_low']}/{day['rhr_avg']}bpm  recovery {day['recovery_index_h']}h  "
          f"tempΔ {day['temp_dev']:+.2f}°C\n")
    print(f"  {'contributor':28s}{'weight':>7}{'sub':>6}{'pts':>7}")
    for c, w in params["weights"]["Readiness Score"].items():
        flag = " ~baseline-cold" if (cold and c in baseline_relative) else \
               (" ~constant fallback" if c == "Recovery Index Score" else "")
        print(f"  {c:28s}{w:6d}%{subs[c]:6.0f}{contrib[c]:7.1f}{flag}")
    print(f"  {'':28s}{'':7}{'':6}{'':7}")
    print(f"  {'READINESS SCORE':28s}{'':7}{'':6}{score:7d}")
    if cold:
        print(f"\n  Note: baseline-relative contributors use a cold baseline until ~14 days of"
              f"\n  history accrue ({n_hist} so far). Keep syncing nightly + re-run build_daily.")


if __name__ == "__main__":
    main()
