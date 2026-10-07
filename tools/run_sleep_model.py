#!/usr/bin/env python3
"""Run Oura's decrypted SleepNet (moonstone) model on our stored ring data to
extract a per-30s hypnogram (DEEP/LIGHT/REM/WAKE).

Inputs from the SQLite event log: IBI (0x60), motion_seconds (0x47), temp (0x46),
bedtime (0x76). SpO2 passed empty (we only have R-ratio, not %). Time axis uses per-boot clock anchors and the bedtime capture epoch.

Usage: python tools/run_sleep_model.py START_DS END_DS [DB] [TZ=1]
       (no args → uses the bedtime_period in the DB)
       --batch reads a JSON array of [start_ds, end_ds] pairs from stdin and emits
       a JSON array of results, loading the model + scanning the DB only once (the
       dashboard uses this to score every night in a single process).
"""
import sys, json, sqlite3, datetime
from pathlib import Path
import torch

from _common import resolve_db, resolve_models_dir
from sleep_inputs import aligned_stages, collect_inputs, refine_deep_stages

REPO = Path(__file__).resolve().parent.parent
TZ = 1.0
MODEL_NAME = "sleepnet_moonstone_1_2_0.pt"
_model_path = resolve_models_dir(REPO, MODEL_NAME) / MODEL_NAME
if not _model_path.is_file():
    _ios_ptl = REPO / "apps" / "ios" / "OuraApp" / "Resources" / "models" / "sleepnet_moonstone_1_2_0.ptl"
    if _ios_ptl.is_file():
        _model_path = _ios_ptl
MODEL = str(_model_path)
STAGE = {1: "DEEP", 2: "LIGHT", 3: "REM", 4: "WAKE"}

JSON = "--json" in sys.argv
BATCH = "--batch" in sys.argv
args = [a for a in sys.argv[1:] if a not in ("--json", "--batch")]
start_ds = end_ds = None
if not BATCH and len(args) >= 2 and args[0].isdigit():
    start_ds, end_ds = int(args[0]), int(args[1])
    rest = args[2:]
else:
    rest = args
db_arg = rest[0] if rest else None
if len(rest) > 1:
    TZ = float(rest[1])
DB = resolve_db(db_arg, REPO)

con = sqlite3.connect(str(DB))
rows = con.execute("SELECT ring_timestamp, tag, COALESCE(decoded_json, '{}'), captured_unix FROM events "
                   "WHERE decoded_json IS NOT NULL OR (tag = 65 AND LENGTH(body) >= 14) "
                   "ORDER BY captured_unix, id").fetchall()
# Anchor ring deciseconds to wall-clock per boot epoch (ds resets on reboot; a single
# global anchor mis-dates older epochs — see epoch_time / crates/oura-summary).
from epoch_time import build_epochs, is_dated, make_unix_s
_epochs = build_epochs(rows)
_unix_s = make_unix_s(_epochs)
def ms(ds, cu=None):  # device deciseconds -> absolute epoch ms (int64), consistent across signals
    u = _unix_s(ds, cu)
    return int(u * 1000) if u is not None else int(ds * 100)
def hm(ms_):
    return datetime.datetime.utcfromtimestamp(ms_/1000 + TZ*3600).strftime("%H:%M")

# load the SleepNet (moonstone) model once; batch mode reuses it across nights.
MODEL_M = torch.jit.load(MODEL, map_location="cpu").eval()


def _collect_window_vitals(start_ds, end_ds, bed_cu):
    start_ms, end_ms = ms(start_ds, bed_cu), ms(end_ds, bed_cu)
    hr_t, hrv_t, motion_t = [], [], []
    for ds, tag, js, cu in rows:
        if not start_ds - 6000 <= ds <= end_ds + 6000:
            continue
        t = ms(ds, cu)
        if not start_ms - 600000 <= t <= end_ms + 600000:
            continue
        if abs(t - (start_ms + (ds - start_ds) * 100)) > 300000:
            continue
        if tag == 0x5D:
            v = json.loads(js)
            step_ds = max(1, int(v.get("interval_min", 5) or 5)) * 600
            for i, x in enumerate(v.get("hr_bpm", []) or []):
                if x and float(x) > 0:
                    hr_t.append((ds + i * step_ds, float(x)))
            for i, x in enumerate(v.get("rmssd_ms", []) or []):
                if x and float(x) > 0:
                    hrv_t.append((ds + i * step_ds, float(x)))
        elif tag == 0x47:
            v = json.loads(js)
            if v.get("motion_seconds") is not None:
                motion_t.append((ds, float(v["motion_seconds"])))
    return hr_t, hrv_t, motion_t


def score_window(start_ds, end_ds, captured_unix=None):
    """Score one bedtime window. Returns (out_dict, ts, stages) or (err_str, None, None)."""
    bed_cu = captured_unix if captured_unix is not None else next((cu for ds, tag, js, cu in reversed(rows) if tag in (0x76, 0x4E) and
                   (json.loads(js).get("bedtime_start_ds") == start_ds or json.loads(js).get("bedtime_start") == start_ds)), None)
    if bed_cu is not None and (not is_dated(_epochs, start_ds, bed_cu) or not is_dated(_epochs, end_ds, bed_cu)):
        return "sleep window clock is undated or ambiguous", None, None
    decoded_rows = ((ds, tag, json.loads(js), cu) for ds, tag, js, cu in rows)
    beats, acm, temp = collect_inputs(decoded_rows, start_ds, end_ds, bed_cu, ms)
    if not beats or not any(b[3] == 1 for b in beats):
        return "not enough valid IBI in this window", None, None

    def col(seq, i):
        return [r[i] for r in seq]
    ibi_ts = torch.tensor(col(beats, 0), dtype=torch.int64)
    ibi_val = torch.tensor([[b[1], b[2], b[3]] for b in beats], dtype=torch.float32)
    acm_ts = torch.tensor(col(acm, 0), dtype=torch.int64)
    acm_val = torch.tensor([[a[1]] for a in acm], dtype=torch.float32)
    temp_ts = torch.tensor(col(temp, 0), dtype=torch.int64)
    temp_val = torch.tensor([[t[1]] for t in temp], dtype=torch.float32)
    bedtime = torch.tensor([ms(start_ds, bed_cu), ms(end_ds, bed_cu)], dtype=torch.int64)
    spo2_val = torch.empty(0, 1, dtype=torch.float32)
    spo2_ts = torch.empty(0, dtype=torch.int64)
    scalars = torch.tensor([35, 25, 0, 0, 0], dtype=torch.float32)
    tst = torch.tensor([300.0], dtype=torch.float32)

    with torch.no_grad():
        ts, staging, apnea, spo2_out, metrics, debug = MODEL_M(
            bedtime, ibi_val, ibi_ts, acm_val, acm_ts, temp_val, temp_ts,
            spo2_val, spo2_ts, scalars, tst)

    stages = [int(s) for s in staging[:, 0].tolist()]
    if not stages:
        return "SleepNet-moonstone returned zero epochs for this window", None, None
    stages = aligned_stages(ts.reshape(-1).tolist(), stages, ms(start_ds, bed_cu), ms(end_ds, bed_cu))
    n = len(stages)
    if n == 0:
        return "SleepNet-moonstone returned zero epochs for this window", None, None
    if 1 not in stages:
        hr_t, hrv_t, motion_t = _collect_window_vitals(start_ds, end_ds, bed_cu)
        stages = refine_deep_stages(stages, hr_t, hrv_t, motion_t, start_ds, end_ds)
    mins = {k: stages.count(c) * 0.5 for c, k in STAGE.items()}
    asleep = sum(mins[k] for k in ("DEEP", "LIGHT", "REM"))
    in_bed = n * 0.5
    out = {
        "start_ds": start_ds, "end_ds": end_ds, "captured_unix": bed_cu,
        "start_local": hm(int(ts[0])), "end_local": hm(int(ts[-1])),
        "epochs": n, "in_bed_min": in_bed,
        "asleep_min": asleep, "efficiency_pct": round(100 * asleep / in_bed) if all(stages) else None,
        "source": "sleepnet",
        "stages": stages,  # per-30s ints: 1=DEEP 2=LIGHT 3=REM 4=WAKE
    }
    for c, k in STAGE.items():
        out[k.lower() + "_min"] = mins[k]
        out[k.lower() + "_pct"] = round(100 * mins[k] / in_bed)
    return out, ts, stages


if BATCH:
    # one process, every night: read [[start,end], ...] from stdin, emit a JSON
    # array (null for any window that couldn't be scored).
    pairs = json.load(sys.stdin)
    results = []
    for p in pairs:
        out, _, _ = score_window(int(p[0]), int(p[1]), int(p[2]) if len(p) > 2 else None)
        results.append(out if isinstance(out, dict) else None)
    print(json.dumps(results))
    sys.exit(0)

if start_ds is None:  # default: most recent bedtime_period in the DB (matches run_models.last_bedtime)
    bt = con.execute("SELECT decoded_json FROM events WHERE tag=118 ORDER BY captured_unix DESC, id DESC").fetchone()
    if bt is None:
        raise SystemExit("no bedtime_period (tag 0x76) in DB — pass start/end deciseconds or sync overnight data first")
    v = json.loads(bt[0])
    start_ds, end_ds = v["bedtime_start_ds"], v["bedtime_end_ds"]

out, ts, stages = score_window(start_ds, end_ds)
if not isinstance(out, dict):
    sys.exit(out)  # error string

if JSON:
    print(json.dumps(out))
    sys.exit(0)

n = out["epochs"]
in_bed = out["in_bed_min"]
print(f"window ds [{start_ds}..{end_ds}] ({(end_ds-start_ds)/10/3600:.1f}h)")
print(f"\nHypnogram: {n} epochs = {n*0.5:.0f} min in bed")
for k in ["DEEP", "LIGHT", "REM", "WAKE"]:
    print(f"  {k:<6} {out[k.lower()+'_min']:>6.0f} min  ({out[k.lower()+'_pct']:4.0f}%)")
print(f"  asleep {out['asleep_min']:.0f} min,  sleep efficiency {out['efficiency_pct']:.0f}%")

# compact timeline: one glyph per ~10 min (20 epochs), majority stage
g = {1: "D", 2: "L", 3: "R", 4: "W"}
print(f"\n  {hm(int(ts[0]))} ", end="")
for i in range(0, n, 20):
    blk = stages[i:i+20]
    maj = max(set(blk), key=blk.count)
    print(g.get(maj, "?"), end="")
print(f" {hm(int(ts[-1]))}   (D=deep L=light R=rem W=wake, ~10min/char)")
