#!/usr/bin/env python3
"""Compute overnight SpO2 % from the ring's spo2_r_pi_event using Oura's own
calibration (the "SpO2 Simple" path).

The ring emits `spo2_r_pi_event` (tag 0x8b) with an `r` ratio-of-ratios and a
perfusion index per sample. Oura converts r -> SpO2 % with a per-hardware
quadratic (coefficients live in the app; the polynomial runs in libecore):

    SpO2 = a*r^2 + b*r + c,  clamped to [85, 100]

Coefficients (com/ouraring/oura/workitem/data/items/d.java):
    gen4 / oreo : a=-13.4 b=-5.1 c=105.2
    cooper      : a=-12.1 b=-6.9 c=106.3

Ring 5's exact mapping isn't confirmed in the decompiled app (the two sets differ
by <1% here); default is gen4/oreo. The production nightly feature instead uses a
firmware-computed % (tags 0x6f/0x70) we don't capture — this is the simple path.

Usage: python tools/run_spo2.py [DB] [--hw gen4|cooper] [--night]
"""
import argparse
import json
import sqlite3
import sys
from pathlib import Path

import numpy as np

from _common import resolve_db

REPO = Path(__file__).resolve().parent.parent
COEFFS = {"gen4": (-13.4, -5.1, 105.2), "cooper": (-12.1, -6.9, 106.3)}


def main():
    p = argparse.ArgumentParser()
    p.add_argument("db", nargs="?", default=None)
    p.add_argument("--hw", default="gen4", choices=list(COEFFS))
    p.add_argument("--night", action="store_true", help="restrict to the most recent bedtime_period window")
    args = p.parse_args()
    con = sqlite3.connect(str(resolve_db(args.db, REPO)))

    where, params = "name='spo2_r_pi_event'", ()
    if args.night:
        bt = con.execute("SELECT decoded_json FROM events WHERE tag=118 ORDER BY ring_timestamp DESC").fetchone()
        if bt is None:
            sys.exit("no bedtime_period — drop --night or sync overnight data")
        v = json.loads(bt[0])
        where += " AND ring_timestamp BETWEEN ? AND ?"
        params = (v["bedtime_start_ds"], v["bedtime_end_ds"])

    R = []
    for (j,) in con.execute(f"SELECT decoded_json FROM events WHERE {where}", params):
        R += [x for x in json.loads(j).get("r", []) if x and x > 0]
    if not R:
        sys.exit("no spo2_r_pi_event samples (is spo2 enabled? oura feature-status)")
    R = np.array(R)
    a, b, c = COEFFS[args.hw]
    spo2 = np.clip(a * R * R + b * R + c, 85, 100)

    print(f"SpO2 (Oura simple calibration, hw={args.hw}{', night' if args.night else ''}) — {len(R)} samples")
    print(f"  mean {spo2.mean():.1f}%   median {np.median(spo2):.0f}%   min {spo2.min():.0f}%   p5 {np.percentile(spo2,5):.0f}%")
    for thr in (90, 88):
        frac = 100 * (spo2 < thr).mean()
        print(f"  time < {thr}%: {frac:.1f}% of samples")
    print(f"  (r: mean {R.mean():.3f}, range [{R.min():.3f}, {R.max():.3f}])")


if __name__ == "__main__":
    main()
