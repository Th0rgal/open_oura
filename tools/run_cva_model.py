#!/usr/bin/env python3
"""Estimate cardiovascular age + pulse-wave velocity from the ring's raw PPG.

Decodes the `cva_raw_ppg_data` events (BLE tag 0x81) the ring emits when the
`cva_ppg` (CAP_CVA_PPG_SAMPLER, id 13) feature is enabled, reconstructs the PPG
waveform, segments it the way the app does (groups of 1500 samples), and runs
Oura's decrypted `cva_2_1_0.pt` model.

Decode: each 0x81 body is int8 *deltas*; cumulative-sum reconstructs the PPG ADC
samples. A measurement = a run of events <2 s apart (~1503 samples ≈ 10 s @ ~140
Hz on Ring 5). The model wants segments of exactly 1500 samples.

Model I/O (from CardiovascularAgeV2Model in the decompiled app):
  forward(ppg_segments [n,1500] f32, demographics [1,5] f32)
    demographics = [sex(-1 F / +1 M / 0 other), height_m, age_years, ring_size, weight_kg]
  -> (daily_cva, quality, raw_quality, daily_pwv, ppg_segment_metrics[n,11])

NOTE: daily_cva tracks the input age (it is a *vascular* age anchored to your
chronological age and adjusted by PPG morphology), so pass real demographics for a
meaningful number. daily_pwv (m/s) is PPG-only.

Usage: python tools/run_cva_model.py [DB] [--sex M|F|O] [--age Y] [--height M]
                                      [--weight KG] [--ring N] [--since-cursor DS]
"""
import argparse
import sqlite3
import sys
from pathlib import Path

import numpy as np
import torch

REPO = Path(__file__).resolve().parent.parent
MODEL = REPO / "notes" / "models" / "cva_2_1_0.pt"
SEG_LEN = 1500          # samples per segment (hard model constant)
GAP_DS = 20             # >2 s (deciseconds) splits two PPG measurements


def build_segments(db, since_ds):
    con = sqlite3.connect(db)
    rows = con.execute(
        "SELECT ring_timestamp, body FROM events WHERE tag=129 AND ring_timestamp>? "
        "ORDER BY ring_timestamp",
        (since_ds,),
    ).fetchall()
    con.close()
    if not rows:
        sys.exit("no cva_raw_ppg_data (tag 0x81) events — is cva_ppg enabled? (oura feature-status)")
    ts = np.array([r[0] for r in rows])
    # split into contiguous measurement runs, cumsum each, chunk into 1500
    runs, cur = [], [0]
    for i in range(1, len(rows)):
        if ts[i] - ts[i - 1] > GAP_DS:
            runs.append(cur)
            cur = []
        cur.append(i)
    runs.append(cur)
    segs = []
    for run in runs:
        deltas = np.concatenate([np.frombuffer(rows[k][1], dtype=np.int8) for k in run]).astype(np.int32)
        wave = np.cumsum(deltas).astype(np.float32)
        for s in range(0, len(wave) - SEG_LEN + 1, SEG_LEN):
            segs.append(wave[s : s + SEG_LEN])
    return segs, len(runs)


def main():
    p = argparse.ArgumentParser()
    p.add_argument("db", nargs="?", default=str(REPO / "oura.db"))
    p.add_argument("--sex", default="M", choices=["M", "F", "O"])
    p.add_argument("--age", type=float, default=30.0)
    p.add_argument("--height", type=float, default=1.78, help="meters")
    p.add_argument("--weight", type=float, default=75.0, help="kg")
    p.add_argument("--ring", type=float, default=10.0, help="ring size")
    p.add_argument("--since-cursor", type=int, default=0, help="only events with ring_timestamp > this")
    args = p.parse_args()
    if not MODEL.exists():
        sys.exit(f"model not found: {MODEL}")

    segs, n_runs = build_segments(args.db, args.since_cursor)
    if not segs:
        sys.exit(f"no full {SEG_LEN}-sample PPG segment available ({n_runs} measurement runs, all too short)")
    ppg = torch.tensor(np.stack(segs), dtype=torch.float32)
    sex = {"F": -1.0, "M": 1.0, "O": 0.0}[args.sex]
    demo = torch.tensor([[sex, args.height, args.age, args.ring, args.weight]], dtype=torch.float32)

    m = torch.jit.load(str(MODEL), map_location="cpu").eval()
    with torch.no_grad():
        cva, quality, raw_quality, pwv, seg_metrics = m(ppg, demo)

    print(f"CVA (cardiovascular age) — {ppg.shape[0]} PPG segments from {n_runs} measurements")
    print(f"  demographics: sex={args.sex} age={args.age:.0f} height={args.height} weight={args.weight} ring={args.ring}")
    print(f"  vascular age : {cva.item():.1f} years   (anchored to input age; here {cva.item()-args.age:+.1f}y vs chronological)")
    print(f"  pulse-wave velocity: {pwv.item():.2f} m/s   (lower = less arterial stiffness)")
    print(f"  quality: {quality.item():.0f}  raw_quality: {raw_quality.item():.2f}")


if __name__ == "__main__":
    main()
