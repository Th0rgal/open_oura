#!/usr/bin/env python3
"""Run the runnable Oura models on our stored ring data (oura.db).

Harness that maps decoded events -> each model's forward() inputs. Only the
models whose inputs we can supply from synced data are wired here; see
docs/model-usage-map / the feasibility matrix for what's blocked and why.

Usage: python tools/run_models.py <model> [DB] [--tz H]
  model = bdi | daily_medians | all
(sleepnet_moonstone has its own runner: tools/run_sleep_model.py)
"""
import json
import sys
import sqlite3
from pathlib import Path

import torch

from _common import resolve_db

REPO = Path(__file__).resolve().parent.parent
MODELS = REPO / "notes" / "models"


def load(name):
    return torch.jit.load(str(MODELS / f"{name}.pt"), map_location="cpu").eval()


def events(db):
    con = sqlite3.connect(db)
    rows = con.execute(
        "SELECT ring_timestamp, name, decoded_json, captured_unix FROM events "
        "WHERE decoded_json IS NOT NULL ORDER BY ring_timestamp"
    ).fetchall()
    con.close()
    return rows


def anchor(rows):
    max_ds, anchor_unix = max(((r[0], r[3]) for r in rows), key=lambda x: x[0])
    def unix_s(ds):  # ring deciseconds -> unix seconds
        return anchor_unix - (max_ds - ds) / 10.0
    return unix_s


def f32(x):
    return torch.tensor(x, dtype=torch.float32)


def last_bedtime(rows):
    """Most recent bedtime_period dict, or a clear error if none were synced."""
    beds = [json.loads(j) for ds, n, j, _ in rows if n == "bedtime_period"]
    if not beds:
        sys.exit("no bedtime_period (tag 0x76) in DB — sync overnight data first")
    return beds[-1]


# ---- sleepnet_bdi_0_4_0: bedtime_input, ibi_values, ibi_timestamps ----
def run_bdi(db, tz):
    rows = events(db)
    unix_s = anchor(rows)
    bp = last_bedtime(rows)
    bstart = unix_s(bp["bedtime_start_ds"])
    bend = unix_s(bp["bedtime_end_ds"])
    # IBIs within the sleep window (absolute beat timeline by cumulative IBI)
    # ibi_values = [ibi_ms, amplitude, quality(1=valid)]; timestamps passed separately.
    ibi_rows, ibi_t = [], []
    for ds, n, j, _ in rows:
        # both raw (0x60) and green-LED (0x80) IBI streams — on Ring 5 the green
        # stream carries most overnight beats (matches run_sleep_model.py)
        if n not in ("ibi_and_amplitude_event", "green_ibi_quality_event"):
            continue
        t0 = unix_s(ds)
        if not (bstart - 600 <= t0 <= bend + 600):  # ±10 min margin, matches run_sleep_model.py
            continue
        d = json.loads(j)
        ibis = d.get("ibi_ms", [])
        amps = d.get("amplitude", [0] * len(ibis))
        acc = 0.0
        for k, ms in enumerate(ibis):
            if ms and ms > 0:
                amp = amps[k] if k < len(amps) else 0
                ibi_rows.append([float(ms), float(amp), 1.0])
                acc += ms  # a beat occurs at the END of its interval
                ibi_t.append((t0 * 1000.0) + acc)  # ms
    print(f"bedtime {bstart:.0f}..{bend:.0f} ({(bend-bstart)/3600:.2f} h), {len(ibi_rows)} IBIs")
    if not ibi_rows:
        sys.exit("no valid IBI in the sleep window — sync overnight IBI (0x60/0x80) data first")
    m = load("sleepnet_bdi_0_4_0")
    bedtime_input = torch.tensor([int(bstart * 1000), int(bend * 1000)], dtype=torch.long)
    ibi_vals = f32(ibi_rows)  # [N,3]
    ibi_ts = torch.tensor([int(t) for t in ibi_t], dtype=torch.long)
    # outputs (names from app SleepNetBdiPyTorchV04Model.ModelOutput):
    #   timestamps, sleepStages[N,5], apneaEvents[N,2], outputMetrics[6], debugMetrics[10]
    timestamps, sleep_stages, apnea_events, out_metrics, dbg_metrics = m(bedtime_input, ibi_vals, ibi_ts)
    # sleepStages: col0 marker, cols1-4 = 4-class softmax (awake/light/deep/rem;
    # exact deep/rem order not yet confirmed from the app's SleepStage enum)
    stage = sleep_stages[:, 1:5].argmax(dim=1)
    n = stage.shape[0]
    if n == 0:
        sys.exit("SleepNet-BDI returned zero epochs for this window")
    labels = ["awake?", "light?", "stageC?", "stageD?"]
    print(f"\nHypnogram: {n} epochs x 30s = {n*30/60:.0f} min")
    for s in range(4):
        c = int((stage == s).sum())
        print(f"  col{s+1} {labels[s]:8}: {c:4d} epochs ({c*30/60:5.1f} min, {100*c/n:4.1f}%)")
    apnea = apnea_events[:, 0]
    print(f"\napneaEvents: {int((apnea > 0.5).sum())} epochs flagged (>0.5) of {n}")
    print(f"outputMetrics: {[round(x, 3) for x in out_metrics.flatten().tolist()]}")
    print(f"debugMetrics:  {[round(x, 3) for x in dbg_metrics.flatten().tolist()]}")


# ---- daily_medians_1_1_0: HRV/HR/temp/MET medians over a day ----
def run_daily_medians(db, tz):
    import json
    rows = events(db)
    unix_s = anchor(rows)
    hrv, hrv_t, hr_min = [], [], []
    temp, temp_t = [], []
    met, met_t = [], []
    for ds, n, j, _ in rows:
        t = unix_s(ds)
        d = json.loads(j)
        if n == "hrv_event":
            iv = d.get("interval_min", 5) * 60
            rm = d.get("rmssd_ms", []); hb = d.get("hr_bpm", [])
            for k, v in enumerate(rm):
                if v and v > 0:
                    hrv.append(float(v)); hrv_t.append(int((t + k * iv) * 1000))
                    hr_min.append(float(hb[k]) if k < len(hb) and hb[k] else 0.0)
        elif n == "temp_event":
            for v in d.get("temps_c", []):
                temp.append(float(v)); temp_t.append(int(t * 1000))
        elif n == "activity_information":
            for k, v in enumerate(d.get("met", [])):
                met.append(float(v)); met_t.append(int((t + k * 60) * 1000))
    bp = last_bedtime(rows)
    sleep_ts = [int(unix_s(bp["bedtime_start_ds"]) * 1000), int(unix_s(bp["bedtime_end_ds"]) * 1000)]
    print(f"hrv={len(hrv)} temp={len(temp)} met={len(met)} hr_min={len(hr_min)}")
    m = load("daily_medians_1_1_0")
    L = torch.long
    out = m(
        f32(hrv), f32([1.0] * len(hrv)), torch.tensor(hrv_t, dtype=L),
        f32(hr_min),
        f32(temp), torch.tensor(temp_t, dtype=L),
        f32(met), torch.tensor(met_t, dtype=L),
        torch.tensor(sleep_ts, dtype=L),
    )
    print("OUTPUT:", [tuple(o.shape) for o in out])
    for i, o in enumerate(out):
        print(f"  [{i}]", o.flatten()[:8].tolist())


# Note: sleepnet_moonstone (full overnight sleep staging + apnea) already has a
# working, validated runner in tools/run_sleep_model.py — use that. It produces a
# DEEP/LIGHT/REM/WAKE hypnogram + sleep efficiency.

RUNNERS = {"bdi": run_bdi, "daily_medians": run_daily_medians}


def main():
    argv = sys.argv[1:]
    tz = 1
    if "--tz" in argv:  # strip "--tz H" so its value isn't mistaken for the DB path
        i = argv.index("--tz")
        if i + 1 >= len(argv):
            sys.exit("--tz requires a value")
        tz = int(argv[i + 1])
        del argv[i:i + 2]
    model = argv[0] if argv else "bdi"
    db_arg = next((a for a in argv[1:] if not a.startswith("-")), None)
    db = str(resolve_db(db_arg, REPO))
    if model == "all":
        for k, fn in RUNNERS.items():
            print("=" * 70, k)
            try:
                fn(db, tz)
            except Exception as e:
                print(f"  FAILED: {type(e).__name__}: {e}")
        return
    if model not in RUNNERS:
        sys.exit(f"unknown model '{model}' (choose: {', '.join(RUNNERS)} | all)")
    RUNNERS[model](db, tz)


if __name__ == "__main__":
    main()
