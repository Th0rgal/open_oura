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
from fit_sleep_score import EmpCurve, LinFit, MonoCurve, engineer, getval

REPO = Path(__file__).resolve().parent.parent

WEIGHTS = {
    "Total Sleep Score": 35, "Restfulness Score": 15, "Sleep Efficiency Score": 10,
    "REM Sleep Score": 10, "Deep Sleep Score": 10, "Sleep Latency Score": 10,
    "Sleep Timin Score": 10,
}
# contributor -> (driver column(s) as named in the trends CSV / engineered, kind).
# All drivers here are reproducible from ring data (see metrics_from_night()).
DRIVERS = {
    "Total Sleep Score": (["Total Sleep Duration"], "mono"),
    "Sleep Efficiency Score": (["Sleep Efficiency"], "mono"),
    "REM Sleep Score": (["REM Sleep Duration"], "mono"),
    "Deep Sleep Score": (["Deep Sleep Duration"], "mono"),
    "Sleep Latency Score": (["Sleep Latency"], "emp"),
    "Restfulness Score": (["awake_frac", "Sleep Efficiency"], "lin"),
    "Sleep Timin Score": (["mid_opt", "mid_reg", "mid_hour"], "lin"),
}


def find_csv(arg):
    if arg:
        return Path(arg)
    cands = list(Path.home().glob("Desktop/oura_*trends.csv")) + list(REPO.glob("*trends*.csv"))
    if not cands:
        sys.exit("no trends CSV for calibration — pass --csv (export from your Oura account)")
    return cands[0]


def fit_curves(csv_path):
    """Fit every contributor curve from the trends export."""
    import csv as _csv
    rows = engineer(list(_csv.DictReader(open(csv_path))))
    need = {"Sleep Score", *WEIGHTS}
    for drv, _ in DRIVERS.values():
        need.update(drv)
    data = [{c: getval(r, c) for c in need} for r in rows]
    data = [d for d in data if all(v is not None for v in d.values())]
    col = lambda name: np.array([d[name] for d in data])
    curves = {}
    for sub, (drivers, kind) in DRIVERS.items():
        y = col(sub)
        if kind == "mono":
            curves[sub] = (MonoCurve(col(drivers[0]), y), drivers, kind)
        elif kind == "emp":
            curves[sub] = (EmpCurve(col(drivers[0]), y), drivers, kind)
        else:
            curves[sub] = (LinFit(np.column_stack([col(d) for d in drivers]), y), drivers, kind)
    return curves, len(data)


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
    ap.add_argument("--csv", help="trends export for calibration (default: ~/Desktop/oura_*trends.csv)")
    ap.add_argument("--start", type=int, help="bedtime start (deciseconds)")
    ap.add_argument("--end", type=int, help="bedtime end (deciseconds)")
    ap.add_argument("--tz", type=int, default=1, help="hours offset for local bedtime clock")
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args()

    db = resolve_db(args.db, REPO)
    curves, n_cal = fit_curves(find_csv(args.csv))

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

    subs, contributions = {}, {}
    for sub, (curve, drivers, kind) in curves.items():
        x = (metrics[drivers[0]] if kind in ("mono", "emp")
             else np.array([[metrics[d] for d in drivers]]))
        s = float(np.clip(np.ravel(curve.predict(x))[0], 1, 100))
        subs[sub] = s
        contributions[sub] = WEIGHTS[sub] * s / 100.0
    score = round(sum(contributions.values()))

    if args.json:
        print(json.dumps({"sleep_score": score, "sub_scores": subs, "metrics": metrics,
                          "hypnogram": {k: hyp[k] for k in
                                        ("asleep_min", "efficiency_pct", "deep_min",
                                         "rem_min", "light_min", "wake_min")}}))
        return

    print(f"Sleep Score (live from ring, calibrated on {n_cal} export days)")
    print(f"  window ds [{start_ds}..{end_ds}]  {hyp['start_local']}–{hyp['end_local']}  "
          f"asleep {hyp['asleep_min']:.0f}m  eff {hyp['efficiency_pct']}%\n")
    print(f"  {'contributor':24s}{'weight':>7}{'sub':>6}{'pts':>7}")
    for sub in WEIGHTS:
        print(f"  {sub:24s}{WEIGHTS[sub]:6d}%{subs[sub]:6.0f}{contributions[sub]:7.1f}")
    print(f"  {'':24s}{'':7}{'':6}{'':7}")
    print(f"  {'SLEEP SCORE':24s}{'':7}{'':6}{score:7d}")


if __name__ == "__main__":
    main()
