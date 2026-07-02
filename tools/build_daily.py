#!/usr/bin/env python3
"""Build the per-day summary + rolling baselines an independent client must carry
to compute baseline-relative score contributors (HRV Balance, Resting-HR, Sleep
Balance, Recovery Index). Mirrors the on-device state ecore accumulates nightly.

For every bedtime night in oura.db it derives, from ring data only:
  * sleep metrics + Sleep Score   (via tools/score_sleep.py)
  * hrv_avg        — mean nocturnal RMSSD (hrv_event 0x5d)
  * rhr_low/avg    — overnight resting HR from IBI (0x60)
  * recovery_index — hours between the RHR minimum and wake (single-night)
  * temp_mean      — mean nocturnal skin temperature (temp_event 0x46)
  * met_avg        — mean MET that day (activity_information)
…then writes them to a `daily_summary` table and fills causal **trailing-14-day**
baselines (hrv/rhr/temp/sleep/met) used as each metric's personal reference.

Re-runnable: rebuilds every night each time (cheap). Persists into oura.db.

Usage: python tools/build_daily.py [DB] [--csv TRENDS.csv] [--tz N]
"""
import argparse
import json
import sqlite3
import subprocess
import sys
from pathlib import Path

import numpy as np

from _common import resolve_db

REPO = Path(__file__).resolve().parent.parent
TRAIL = 14   # days of history for a personal baseline (matches the score calibration)

SCHEMA = """
CREATE TABLE IF NOT EXISTS daily_summary (
    start_ds INTEGER PRIMARY KEY, end_ds INTEGER,
    sleep_score REAL, total_sleep_sec REAL, efficiency REAL, rem_sec REAL,
    deep_sec REAL, latency_sec REAL, awake_frac REAL, mid_hour REAL,
    hrv_avg REAL, rhr_low REAL, rhr_avg REAL, recovery_index_h REAL,
    temp_mean REAL, met_avg REAL,
    hrv_baseline REAL, rhr_baseline REAL, temp_baseline REAL,
    sleep_baseline REAL, met_baseline REAL, temp_dev REAL, n_history INTEGER
)
"""


def overnight_hr(rows):
    """(ds, bpm) series from IBI hr_bpm within the window, physiologic only."""
    series = []
    for ds, j in rows:
        for hr in json.loads(j).get("hr_bpm", []):
            if 25 <= hr <= 150:
                series.append((ds, float(hr)))
    return sorted(series)


def recovery_index_hours(hr_series, end_ds):
    """Hours between the (smoothed) resting-HR minimum and wake — Oura's Recovery
    Index: the earlier RHR bottoms out overnight, the more recovered. Returns the
    gap in hours plus the low/avg RHR."""
    if len(hr_series) < 20:
        return None, None, None
    ds = np.array([s[0] for s in hr_series], float)
    bpm = np.array([s[1] for s in hr_series], float)
    # rolling median over ~30-sample window to suppress beat-to-beat noise
    w = max(5, len(bpm) // 40)
    sm = np.array([np.median(bpm[max(0, i - w):i + w + 1]) for i in range(len(bpm))])
    i_min = int(np.argmin(sm))
    rhr_low = float(sm[i_min])
    rhr_avg = float(np.mean(sm))
    gap_h = max(0.0, (end_ds - ds[i_min]) / 36000.0)   # 36000 ds = 1 h
    return round(gap_h, 2), round(rhr_low, 1), round(rhr_avg, 1)


def nocturnal(rows_for_tag, lo, hi):
    return [(ds, j) for ds, j in rows_for_tag if lo <= ds <= hi]


def sleep_metrics(db, start_ds, end_ds, csv, tz):
    cmd = [sys.executable, str(REPO / "tools" / "score_sleep.py"), str(db),
           "--start", str(start_ds), "--end", str(end_ds), "--tz", str(tz), "--json"]
    if csv:
        cmd += ["--csv", csv]
    r = subprocess.run(cmd, capture_output=True, text=True)
    if r.returncode != 0:
        return None
    return json.loads(r.stdout)


def main():
    ap = argparse.ArgumentParser(description="Build daily_summary + baselines from ring data")
    ap.add_argument("db", nargs="?")
    ap.add_argument("--csv", help="trends export for Sleep-Score calibration")
    ap.add_argument("--tz", type=int, default=1)
    args = ap.parse_args()
    db = resolve_db(args.db, REPO)

    con = sqlite3.connect(str(db))
    con.executescript(SCHEMA)
    bts = sorted({(v["bedtime_start_ds"], v["bedtime_end_ds"])
                  for (j,) in con.execute("SELECT decoded_json FROM events "
                                          "WHERE tag=118 AND decoded_json IS NOT NULL")
                  for v in [json.loads(j)]})
    ibi = con.execute("SELECT ring_timestamp, decoded_json FROM events "
                      "WHERE tag=96 AND decoded_json IS NOT NULL ORDER BY ring_timestamp").fetchall()
    hrv = con.execute("SELECT ring_timestamp, decoded_json FROM events "
                      "WHERE tag=93 AND decoded_json IS NOT NULL ORDER BY ring_timestamp").fetchall()
    tmp = con.execute("SELECT ring_timestamp, decoded_json FROM events "
                      "WHERE name='temp_event' AND decoded_json IS NOT NULL ORDER BY ring_timestamp").fetchall()
    act = con.execute("SELECT ring_timestamp, decoded_json FROM events "
                      "WHERE name='activity_information' AND decoded_json IS NOT NULL ORDER BY ring_timestamp").fetchall()

    days = []
    for start_ds, end_ds in bts:
        sm = sleep_metrics(db, start_ds, end_ds, args.csv, args.tz)
        if not sm:
            continue
        m = sm["metrics"]
        hr_series = overnight_hr(nocturnal(ibi, start_ds, end_ds))
        rec_h, rhr_low, rhr_avg = recovery_index_hours(hr_series, end_ds)
        rmssd = [r for _, j in nocturnal(hrv, start_ds, end_ds)
                 for r in json.loads(j).get("rmssd_ms", []) if r and r > 0]
        temps = [t for _, j in nocturnal(tmp, start_ds, end_ds)
                 for t in json.loads(j).get("temps_c", []) if 30 <= t <= 40]
        # activity in the ~18h leading up to bedtime start (that day's exposure)
        mets = [me for ds, j in act if start_ds - 648000 <= ds <= start_ds
                for me in json.loads(j).get("met", []) if me]
        days.append({
            "start_ds": start_ds, "end_ds": end_ds,
            "sleep_score": sm["sleep_score"],
            "total_sleep_sec": m["Total Sleep Duration"], "efficiency": m["Sleep Efficiency"],
            "rem_sec": m["REM Sleep Duration"], "deep_sec": m["Deep Sleep Duration"],
            "latency_sec": m["Sleep Latency"], "awake_frac": m["awake_frac"],
            "mid_hour": m["mid_hour"],
            "hrv_avg": float(np.mean(rmssd)) if rmssd else None,
            "rhr_low": rhr_low, "rhr_avg": rhr_avg, "recovery_index_h": rec_h,
            "temp_mean": float(np.mean(temps)) if temps else None,
            "met_avg": float(np.mean(mets)) if mets else None,
        })

    # causal trailing-14-day baselines + temperature deviation
    def trail(i, key):
        prev = [days[k][key] for k in range(i) if days[k].get(key) is not None]
        return float(np.mean(prev[-TRAIL:])) if prev else None

    rows = []
    for i, d in enumerate(days):
        hb, rb, tb = trail(i, "hrv_avg"), trail(i, "rhr_low"), trail(i, "temp_mean")
        d["hrv_baseline"], d["rhr_baseline"], d["temp_baseline"] = hb, rb, tb
        d["sleep_baseline"], d["met_baseline"] = trail(i, "total_sleep_sec"), trail(i, "met_avg")
        d["temp_dev"] = (d["temp_mean"] - tb) if (d["temp_mean"] is not None and tb is not None) else 0.0
        d["n_history"] = i
        rows.append(d)

    cols = [c.strip().split()[0] for c in SCHEMA.split("(", 1)[1].split(")")[0].split(",")]
    con.execute("DELETE FROM daily_summary")
    con.executemany(
        f"INSERT OR REPLACE INTO daily_summary ({','.join(cols)}) "
        f"VALUES ({','.join('?' for _ in cols)})",
        [tuple(d.get(c) for c in cols) for d in rows])
    con.commit()
    con.close()

    print(f"daily_summary: {len(rows)} night(s) written to {db.name}")
    for d in rows:
        rb = f"hrvσ={d['hrv_avg']:.0f}/{d['hrv_baseline']:.0f}" if d['hrv_baseline'] else f"hrv={d['hrv_avg']:.0f}/—"
        print(f"  ds {d['start_ds']:>9}  sleep={d['sleep_score']:>3}  {rb}  "
              f"rhr={d['rhr_low']}/{d['rhr_avg']}  recov={d['recovery_index_h']}h  "
              f"tempΔ={d['temp_dev']:+.2f}  hist={d['n_history']}d")


if __name__ == "__main__":
    main()
