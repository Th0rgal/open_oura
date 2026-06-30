#!/usr/bin/env python3
"""Compute a Sleep Score for a night **live from ring data** — no cloud.

Pipeline:
  1. SleepNet (run_sleep_model.py) → hypnogram → durations / efficiency / latency.
  2. bedtime_period history → sleep-midpoint + 7-day regularity (circadian timing).
  3. contributor curves + combiner weights (fit from the trends export, see
     tools/fit_sleep_score.py / docs/algorithms/score-weights.md) map each raw
     metric → 0-100 sub-score, then `round(Σ wᵢ·subᵢ / 100)`.

Restfulness here uses only ring-derivable drivers (awake fraction + efficiency);
its movement-based micro-inputs aren't reproducible from the export's units, and
they carry little marginal signal once awake-fraction is in.

Usage: python tools/score_sleep.py [DB] [--csv TRENDS.csv] [--start DS --end DS] [--json]
"""
import argparse
import json
import subprocess
import sys
from pathlib import Path

import numpy as np

from _common import resolve_db
from fit_sleep_score import load_params, score_from_params

REPO = Path(__file__).resolve().parent.parent
PARAMS = REPO / "local" / "score_params.json"


def clock_midpoint(start_local, end_local):
    """Real-clock sleep-midpoint hour from 'HH:MM' bedtime strings, wrapped to the
    24-30 range the calibration uses (so 03:00 -> 27.0)."""
    def hm(s):
        h, m = s.split(":")
        return int(h) + int(m) / 60.0
    a, b = hm(start_local), hm(end_local)
    if b < a:
        b += 24
    mid = (a + b) / 2.0
    return mid + 24 if mid < 12 else mid


def timing_features(db, start_ds, end_ds, mid_hour):
    """mid_hour/mid_opt from the real clock; 7-day regularity from bedtime_period
    deciseconds (a day-to-day delta, so the unknown ds→clock phase offset cancels)."""
    import sqlite3
    con = sqlite3.connect(str(db))
    bts = sorted({(v["bedtime_start_ds"], v["bedtime_end_ds"])
                  for (j,) in con.execute(
                      "SELECT decoded_json FROM events WHERE tag=118 AND decoded_json IS NOT NULL")
                  for v in [json.loads(j)]})
    con.close()
    cur_mid_ds = None
    prev_mid_ds = []
    for s, e in bts:
        m = (s + e) / 2.0
        if s < start_ds:
            prev_mid_ds.append(m)
        elif abs(s - start_ds) < 1:
            cur_mid_ds = m
    if cur_mid_ds is None:
        # fall back to the scored night's own midpoint — independent of whether any
        # bedtime_period rows exist (with --start/--end, `bts` can be empty).
        cur_mid_ds = (start_ds + end_ds) / 2.0
    prev_h = [(p % 864000) / 36000.0 for p in prev_mid_ds[-7:]]
    cur_h = (cur_mid_ds % 864000) / 36000.0
    if prev_h:
        # circular mean of the prior midpoints on a 24h clock, so regularity is a true
        # circular delta: frame-invariant (independent of --tz / the ds→clock phase)
        # and correct across the midnight wrap, where a naive arithmetic mean breaks.
        ang = np.array(prev_h) * (np.pi / 12.0)
        mean_h = (np.arctan2(np.sin(ang).mean(), np.cos(ang).mean()) % (2 * np.pi)) * (12.0 / np.pi)
        reg = abs(cur_h - mean_h)
        reg = min(reg, 24 - reg)                                   # circular distance
    else:
        reg = 0.0
    return {"mid_hour": mid_hour, "mid_opt": -((mid_hour - 27.0) ** 2), "mid_reg": reg}


def metrics_from_night(db, start_ds, end_ds, tz=1):
    """Run SleepNet for the window and derive the raw metrics the curves expect
    (durations in seconds to match the export; efficiency in %; awake fraction)."""
    cmd = [sys.executable, str(REPO / "tools" / "run_sleep_model.py"), "--json",
           str(start_ds), str(end_ds), str(db), str(tz)]
    r = subprocess.run(cmd, capture_output=True, text=True)
    if r.returncode != 0 or not r.stdout.strip():
        sys.exit(f"SleepNet failed: {r.stderr.strip() or r.stdout.strip()}")
    o = json.loads(r.stdout)
    stages = o["stages"]                       # 1=DEEP 2=LIGHT 3=REM 4=WAKE
    # sleep latency: time to first sustained sleep (>=10 min / 20 epochs non-WAKE)
    onset = next((i for i in range(len(stages))
                  if all(s != 4 for s in stages[i:i + 20]) and i + 20 <= len(stages)), 0)
    m = {
        "Total Sleep Duration": o["asleep_min"] * 60.0,
        "Sleep Efficiency": o["efficiency_pct"],
        "REM Sleep Duration": o["rem_min"] * 60.0,
        "Deep Sleep Duration": o["deep_min"] * 60.0,
        "Sleep Latency": onset * 30.0,
        "awake_frac": o["wake_min"] / o["in_bed_min"] if o["in_bed_min"] else 0.0,
    }
    m.update(timing_features(db, start_ds, end_ds, clock_midpoint(o["start_local"], o["end_local"])))
    return m, o


def main():
    ap = argparse.ArgumentParser(description="Live Sleep Score from ring data")
    ap.add_argument("db", nargs="?")
    ap.add_argument("--params", default=str(PARAMS),
                    help="calibrated score params (tools/calibrate_scores.py output)")
    ap.add_argument("--start", type=int, help="bedtime start (deciseconds)")
    ap.add_argument("--end", type=int, help="bedtime end (deciseconds)")
    ap.add_argument("--tz", type=int, default=1, help="hours offset for local bedtime clock")
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args()

    db = resolve_db(args.db, REPO)
    params = load_params(args.params)

    if args.start and args.end:
        start_ds, end_ds = args.start, args.end
    else:
        import sqlite3
        con = sqlite3.connect(str(db))
        bt = con.execute("SELECT decoded_json FROM events WHERE tag=118 "
                         "ORDER BY ring_timestamp DESC").fetchone()
        con.close()
        if not bt:
            sys.exit("no bedtime_period in DB — sync an overnight first")
        v = json.loads(bt[0])
        start_ds, end_ds = v["bedtime_start_ds"], v["bedtime_end_ds"]

    metrics, hyp = metrics_from_night(db, start_ds, end_ds, args.tz)

    score, subs, contributions = score_from_params(params, "Sleep Score", metrics)

    if args.json:
        print(json.dumps({"sleep_score": score, "sub_scores": subs, "metrics": metrics,
                          "hypnogram": {k: hyp[k] for k in
                                        ("asleep_min", "efficiency_pct", "deep_min",
                                         "rem_min", "light_min", "wake_min")}}))
        return

    print(f"Sleep Score (live from ring, calibrated on {params.get('n_days', '?')} export days)")
    print(f"  window ds [{start_ds}..{end_ds}]  {hyp['start_local']}–{hyp['end_local']}  "
          f"asleep {hyp['asleep_min']:.0f}m  eff {hyp['efficiency_pct']}%\n")
    weights = params["weights"]["Sleep Score"]
    print(f"  {'contributor':24s}{'weight':>7}{'sub':>6}{'pts':>7}")
    for sub, w in weights.items():
        print(f"  {sub:24s}{w:6d}%{subs[sub]:6.0f}{contributions[sub]:7.1f}")
    print(f"  {'':24s}{'':7}{'':6}{'':7}")
    print(f"  {'SLEEP SCORE':24s}{'':7}{'':6}{score:7d}")


if __name__ == "__main__":
    main()
