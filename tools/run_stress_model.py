#!/usr/bin/env python3
"""Compute Oura's *daytime stress* timeseries locally and run the decrypted
`stress_daytime_sensing_1_1_0` model on our stored ring data — no cloud.

The model is a small deterministic transform (recovered from its TorchScript):

    intensity            = dhrv_value - dhrv_baseline          # HRV vs daytime baseline
    neutral_zone_half    = f(night_hrv_baseline)               # 2 / 3 / 4 ms dead-zone
    stress_threshold     = -neutral_zone_half                  # below -> "stressed"
    recovery_threshold   = +neutral_zone_half                  # above -> "restored"
    stress_saturation    = lut(night_hrv_baseline)             # negative clamp
    recovery_saturation  = lut(night_hrv_baseline)             # positive clamp
    scaled_*             = scale_output(intensity, thresholds, saturations)

The 8 outputs map 1:1 onto `TimeseriesDbDaytimeStress` (intensity / intensity_scaled
/ stress_limit{,_scaled} / recovery_limit{,_scaled} / saturation_stress_deviation /
saturation_recovery_deviation), i.e. running this per sample reconstructs the exact
daytime-stress timeseries Oura would have synced.

INPUT GATE (enforced by the model's own `validator`): each sample needs a single
*awake, sedentary* daytime HRV reading —
  * timestamp must fall OUTSIDE every bedtime window (else code 6 "Sleep detected"),
  * ring_met <= 1.8 (else code 3 "exceeded limit for activity"),
  * dhrv / both baselines must be one positive observation.
Our captures are mostly nocturnal, so few samples qualify; enable the `daytime_hr`
feature (`oura feature daytime_hr ...`) to collect awake HRV and this fills in.

dhrv source: the ring's own 5-min RMSSD (`hrv_event`, tag 0x5d). Daytime samples
(outside bedtime) are the dhrv values; nocturnal samples form `night_hrv_baseline`;
`dhrv_baseline` is the daytime median (falls back to the night baseline when too
few daytime samples exist — the app uses `dhrv_imputation` here; we approximate).

Usage: python tools/run_stress_model.py [DB] [--tz N] [--met M] [--json]
"""
import argparse
import json
import sqlite3
import sys
from pathlib import Path
from statistics import median

import torch

from _common import resolve_db

REPO = Path(__file__).resolve().parent.parent
MODEL = REPO / "notes" / "models" / "stress_daytime_sensing_1_1_0.pt"
DS_PER_5MIN = 3000          # 5 min in ring deciseconds (1 s = 10 ds)
MIN_DAYTIME_FOR_BASELINE = 3  # below this, dhrv_baseline falls back to the night baseline
OUT_LABELS = ["intensity", "stress_limit", "recovery_limit",
              "saturation_stress_deviation", "saturation_recovery_deviation",
              "intensity_scaled", "stress_limit_scaled", "recovery_limit_scaled"]


def load(db):
    con = sqlite3.connect(str(db))
    bedtimes = sorted({
        (v["bedtime_start_ds"], v["bedtime_end_ds"])
        for (j,) in con.execute(
            "SELECT decoded_json FROM events WHERE tag=118 AND decoded_json IS NOT NULL")
        for v in [json.loads(j)]
    })
    # expand each hrv_event into (timestamp_ds, rmssd_ms) 5-min samples
    samples = []
    for ts, j in con.execute(
            "SELECT ring_timestamp, decoded_json FROM events "
            "WHERE tag=93 AND decoded_json IS NOT NULL ORDER BY ring_timestamp"):
        for i, r in enumerate(json.loads(j).get("rmssd_ms", [])):
            if r and r > 0:
                samples.append((ts + i * DS_PER_5MIN, float(r)))
    con.close()
    return bedtimes, samples


def asleep(ts, bedtimes):
    return any(s <= ts <= e for s, e in bedtimes)


def run(model, dhrv, ts, bedtimes, dhrv_baseline, night_baseline, ring_met):
    """Call the model for one sample; return (ok, result_or_errcode)."""
    bs = torch.tensor([s for s, _ in bedtimes], dtype=torch.float64)
    be = torch.tensor([e for _, e in bedtimes], dtype=torch.float64)
    try:
        out = model(
            torch.tensor([dhrv], dtype=torch.float32),
            torch.tensor([float(ts)], dtype=torch.float64),
            bs, be,
            torch.tensor([dhrv_baseline], dtype=torch.float32),
            torch.tensor([night_baseline], dtype=torch.float32),
            torch.tensor([ring_met], dtype=torch.float32),
        )
        return True, {l: float(o.flatten()[0]) for l, o in zip(OUT_LABELS, out)}
    except Exception as e:  # validator raised "<code>, <message>"
        msg = str(e).strip().splitlines()[-1]
        return False, msg


def zone(intensity, stress_lim, recovery_lim):
    if intensity <= stress_lim:
        return "stressed"
    if intensity >= recovery_lim:
        return "restored"
    return "neutral"


def main():
    ap = argparse.ArgumentParser(description="Local daytime-stress timeseries from ring HRV")
    ap.add_argument("db", nargs="?", help="events DB (default: ./oura.db)")
    ap.add_argument("--tz", type=int, default=1, help="hours offset for printed clock times")
    ap.add_argument("--met", type=float, default=1.0,
                    help="ring MET to assume for awake samples (must be <=1.8; default sedentary 1.0)")
    ap.add_argument("--json", action="store_true", help="emit the timeseries as JSON")
    args = ap.parse_args()

    if not MODEL.exists():
        sys.exit(f"error: model not found: {MODEL} (decrypt notes/models first)")
    model = torch.jit.load(str(MODEL), map_location="cpu").eval()

    bedtimes, samples = load(resolve_db(args.db, REPO))
    if not bedtimes:
        sys.exit("no bedtime_period events — can't separate awake vs sleep HRV")
    if not samples:
        sys.exit("no hrv_event (tag 0x5d) samples — is HRV being captured?")

    night = [r for ts, r in samples if asleep(ts, bedtimes)]
    daytime = [(ts, r) for ts, r in samples if not asleep(ts, bedtimes)]
    if not night:
        sys.exit("no nocturnal HRV — can't form night_hrv_baseline")
    night_baseline = median(night)
    dhrv_baseline = (median([r for _, r in daytime])
                     if len(daytime) >= MIN_DAYTIME_FOR_BASELINE else night_baseline)
    baseline_src = "daytime median" if len(daytime) >= MIN_DAYTIME_FOR_BASELINE else "night fallback"

    series, rejected = [], {}
    for ts, dhrv in daytime:
        ok, res = run(model, dhrv, ts, bedtimes, dhrv_baseline, night_baseline, args.met)
        if ok:
            res["timestamp_ds"] = ts
            res["dhrv"] = dhrv
            res["zone"] = zone(res["intensity"], res["stress_limit"], res["recovery_limit"])
            series.append(res)
        else:
            rejected[res] = rejected.get(res, 0) + 1

    if args.json:
        print(json.dumps({
            "night_hrv_baseline": night_baseline,
            "dhrv_baseline": dhrv_baseline,
            "samples": series,
        }, indent=2))
        return

    print(f"Daytime stress (local, stress_daytime_sensing_1_1_0) — {len(samples)} HRV samples "
          f"({len(night)} nocturnal, {len(daytime)} awake)")
    print(f"  night_hrv_baseline = {night_baseline:.0f} ms   "
          f"dhrv_baseline = {dhrv_baseline:.0f} ms ({baseline_src})   "
          f"neutral zone = ±{series[0]['recovery_limit']:.0f} ms" if series else
          f"  night_hrv_baseline = {night_baseline:.0f} ms   dhrv_baseline = {dhrv_baseline:.0f} ms ({baseline_src})")
    if series:
        intens = [s["intensity"] for s in series]
        zones = [s["zone"] for s in series]
        print(f"  scored {len(series)} awake samples | mean intensity {sum(intens)/len(intens):+.1f} ms"
              f" | restored {zones.count('restored')}  neutral {zones.count('neutral')}  stressed {zones.count('stressed')}")
        for s in series:
            print(f"    ts={s['timestamp_ds']:>9}  dhrv={s['dhrv']:5.0f}  "
                  f"intensity={s['intensity']:+5.0f}  scaled={s['intensity_scaled']:+.2f}  [{s['zone']}]")
    else:
        print("  no awake samples scored.")
    if rejected:
        print("  rejected samples (model validator):")
        for msg, n in sorted(rejected.items(), key=lambda x: -x[1]):
            print(f"    {n:4d} x  {msg}")

    # Resilience: report readiness honestly rather than fabricating missing inputs.
    print("\nResilience (stress_resilience_2_2_1) — not run. Still needs, per its forward():")
    print("  * ~14-day history: daily_stress_list / daily_restorative_time_list / daily_sleep_recovery_list")
    print(f"    (have ~{len(bedtimes)} day(s) of data)")
    print("  * daily scores sleep_score / hrv_balance / recovery_index "
          "(from the ecore score-calibration work — see docs/algorithms/README.md)")
    print("  The daytime-stress fields above (stress/limits/saturations) are the ready inputs.")


if __name__ == "__main__":
    main()
