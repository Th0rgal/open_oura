#!/usr/bin/env python3
"""Label activities with Oura's OWN model — not a heuristic.

Feeds the met/motion/temperature/heartrate timeseries we already sync (from the
SQLite event log) into Oura's decrypted TorchScript
`automatic_activity_detection_3_1_11.pt` and prints detected activity segments
with the model's own type label and confidence. No raw IMU / RData needed.

This is the engine behind `oura sessions` (the Rust CLI shells out to it).

Usage:
    python tools/run_activity_model.py [DB] [--tz HOURS] [--threshold P]
                                       [--json] [--verbose]

`DB` defaults to ./oura.db (then captures/ring5.db). Requires `torch` in the
venv (CPU is fine). The model lives in `notes/models/`.

Caveat: activity *type* can be weak — the `stepmotion` (gait) input needs raw
IMU (the RData capability we can't enable on a consumer ring), so we stub it
NaN. Detecting *when* an activity happens (and workout-vs-not) is reliable; the
exact sport label is the model's best guess from MET/motion/HR/temp alone.
"""
import argparse
import datetime
import json as jsonlib
import sqlite3
import sys
from pathlib import Path

import warnings

import torch

from _common import resolve_db

# The model triggers a benign non-contiguous torch.searchsorted perf warning.
warnings.filterwarnings("ignore", message=".*searchsorted.*")

REPO = Path(__file__).resolve().parent.parent
MODEL = REPO / "notes" / "models" / "automatic_activity_detection_3_1_11.pt"
MODEL_VERSION = "3.1.11"

# behavior-id -> name, from the model's behavior table (ActivityTypes.json).
BEHAVIOR = {
    -1: "nothing", 0: "<empty>", 1: "badminton", 2: "boxing", 3: "crossCountrySkiing",
    4: "crossTraining", 5: "cycling", 6: "dance", 7: "elliptical", 8: "strengthTraining",
    9: "hockey", 10: "pilates", 11: "rowing", 12: "running", 13: "swimming", 14: "walking",
    15: "yoga", 16: "golf", 17: "tennis", 18: "climbing", 19: "downhillSkiing",
    20: "snowboarding", 21: "hiking", 22: "horsebackRiding", 23: "volleyball", 24: "basketball",
    25: "americanFootball", 26: "soccer", 27: "baseball", 28: "coreExercise", 29: "cricket",
    30: "HIIT", 31: "diving", 32: "fitnessClass", 33: "floorball", 34: "gymnastics",
    35: "handball", 36: "houseWork", 37: "iceSkating", 38: "jumpingRope", 39: "martialArts",
    40: "flexibility", 41: "mountainBiking", 42: "nordicWalking", 48: "stairExercise",
    49: "stretching", 50: "surfing", 51: "waterFitness", 52: "yardwork", 53: "padel",
    69: "skateboarding", 65535: "other", 65536: "nap", 65537: "sleep",
    65538: "pause", 70937: "meditation", 71201: "eating", 71227: "relax", 71239: "transport",
}


def parse_args():
    p = argparse.ArgumentParser(description="Label activities with Oura's automatic_activity_detection model.")
    p.add_argument("db", nargs="?", default=None, help="SQLite DB (default: ./oura.db then captures/ring5.db)")
    p.add_argument("--tz", type=float, default=1.0, help="Timezone offset hours from UTC for display (default 1)")
    p.add_argument("--threshold", type=float, default=0.5, help="is_workout probability marked as a workout (default 0.5)")
    p.add_argument("--min-duration", type=float, default=5.0, help="Minimum segment minutes the model emits (default 5)")
    p.add_argument("--json", action="store_true", help="Emit machine-readable JSON instead of a table")
    p.add_argument("--verbose", action="store_true", help="Print input series ranges (debug)")
    # back-compat: old positional `[DB] [TZ]`
    args, extra = p.parse_known_args()
    if extra:
        try:
            args.tz = float(extra[0])
        except (ValueError, IndexError):
            pass
    # A bare numeric positional (e.g. `2` for UTC+2) is a legacy tz offset, not a
    # DB path — reinterpret it so resolve_db doesn't fail with "database not found".
    if args.db is not None and not Path(args.db).exists():
        try:
            args.tz = float(args.db)
            args.db = None
        except ValueError:
            pass
    return args


def main():
    args = parse_args()
    db = resolve_db(args.db, REPO)
    if not MODEL.exists():
        sys.exit(f"error: model not found: {MODEL}")

    con = sqlite3.connect(str(db))
    rows = con.execute(
        "SELECT ring_timestamp, tag, decoded_json, captured_unix FROM events "
        "WHERE decoded_json IS NOT NULL ORDER BY ring_timestamp"
    ).fetchall()
    if not rows:
        sys.exit(f"error: no decoded events in {db} (run `oura sync` first)")

    # Anchor ring deciseconds to wall-clock via the latest event's capture time.
    max_ds, anchor_unix = max(((r[0], r[3]) for r in rows), key=lambda x: x[0])
    min_ds = min(r[0] for r in rows)

    def _unix_min(ds):
        return (anchor_unix - (max_ds - ds) / 10.0) / 60.0

    # Rebase by whole days: keeps time-of-day (model uses min%1440) but keeps
    # values small enough to be EXACT in float32 (unix-minutes ~29.7M exceed
    # 2^24 integer precision and silently break the model's time alignment).
    OFFSET = int(_unix_min(min_ds) // 1440) * 1440

    def tmin(ds):
        return int(round(_unix_min(ds))) - OFFSET

    met, motion, temp, hr = [], [], [], []
    import os
    acm_scale = float(os.environ.get("ACM_SCALE", "1"))
    for ds, tag, js, _ in rows:
        try:
            v = jsonlib.loads(js)
        except Exception:
            continue
        t = tmin(ds)
        if tag == 0x50 and isinstance(v.get("met"), list):  # activity_information
            for i, m in enumerate(v["met"]):
                met.append((t + i, float(m)))
        elif tag == 0x47:  # motion_event
            motion.append((t,
                float(v.get("orientation", 0)), float(v.get("motion_seconds", 0)),
                v.get("avg_x", 0) * acm_scale, v.get("avg_y", 0) * acm_scale, v.get("avg_z", 0) * acm_scale,
                float("nan"),  # regular_motion: no source
                float(v.get("low_intensity", 0)), float(v.get("high_intensity", 0))))
        elif tag == 0x46 and v.get("temps_c"):  # temp_event
            temp.append((t, float(v["temps_c"][0])))
        elif tag == 0x80 and v.get("hr_bpm"):  # green_ibi_quality_event (PPG HR)
            b = v["hr_bpm"]
            if b:
                hr.append((t, sum(b) / len(b)))

    met = sorted({round(t): (t, m) for t, m in met}.values())
    if not met:
        sys.exit("no MET (activity_information / tag 0x50) events in DB — cannot run the activity model")

    def f32(seq, cols):
        # keep rank 2 ([0, cols]) for empty series, matching the LibTorch mat() path;
        # a bare torch.tensor([]) is 1-D and shape-mismatches the model.
        if seq:
            return torch.tensor(seq, dtype=torch.float32)
        return torch.empty((0, cols), dtype=torch.float32)

    met_t, motion_t = f32(met, 2), f32(sorted(motion), 9)
    temp_t, hr_t = f32(sorted(temp), 2), f32(sorted(hr), 2)

    # ── stepmotion (gait) input ───────────────────────────────────────────────
    # The app feeds AAD a `stepmotion` channel decoded from the ring's real_step
    # events (tags 0x7e/0x7f): the two 14-byte halves are bit-unpacked into a
    # 27-column quantized record → `steps_motion_decoder` → 11 gait features per
    # 10 s sub-window, which is exactly the AAD `stepmotion` input. With real gait,
    # walk/run vs cycle/swim is no longer a blind guess.
    #
    # The exact packing is from libringeventparser.so's native parser
    # (EventParser::parse_api_real_steps_features_1/2): feature_1 fills 14 columns,
    # feature_2 fills 13 more AND patches the 9th (carry) bit of the four `<<1`
    # fields from its last byte p2[13]. Verified physically: this makes a continuous
    # run decode to a stable ~2.7 Hz cadence (vs uniform noise for a naive unpack).
    # Set STEPMOTION=0 to fall back to the NaN stub.
    import os

    def unpack27(p1, p2):
        c = p2[13]  # packed carry byte: bits 7..0 feed the <<1 / 10-bit fields
        return [
            (p2[10] << 2) | (c & 0x3),        (p2[11]),                 (p2[12]),                  # globals
            (p1[0] << 1) | (p1[3] >> 7),      (p1[1] << 1) | ((c >> 7) & 1), (p1[2] << 1) | ((c >> 6) & 1),
            p1[3] & 0x7F, p1[4], p1[5], p1[6], p1[7],                                              # block 1
            (p1[8] << 1) | (p1[11] >> 7),     (p1[9] << 1) | ((c >> 5) & 1), (p1[10] << 1) | ((c >> 4) & 1),
            p1[11] & 0x7F, p1[12], p1[13], p2[0], p2[1],                                           # block 2
            (p2[2] << 1) | (p2[5] >> 7),      (p2[3] << 1) | ((c >> 3) & 1), (p2[4] << 1) | ((c >> 2) & 1),
            p2[5] & 0x7F, p2[6], p2[7], p2[8], p2[9],                                              # block 3
        ]

    def build_stepmotion():
        dec_path = MODEL.parent / "steps_motion_decoder_2_0_0.pt"
        if not dec_path.exists():
            return None
        srows = con.execute(
            "SELECT ring_timestamp, tag, body FROM events WHERE tag IN (126, 127) ORDER BY ring_timestamp"
        ).fetchall()
        f1 = [(ts, b) for ts, tag, b in srows if tag == 0x7E and b and len(b) == 14]
        f2 = {ts: b for ts, tag, b in srows if tag == 0x7F and b and len(b) == 14}
        data, tsms = [], []
        for ts, b1 in f1:
            b2 = f2.get(ts + 1)  # feature_2 is emitted right after feature_1
            if b2 is None:
                continue
            data.append(unpack27(b1, b2))
            tsms.append(int((anchor_unix - (max_ds - ts) / 10.0) * 1000))  # unix ms
        if not data:
            return None
        dec = torch.jit.load(str(dec_path), map_location="cpu").eval()
        with torch.no_grad():
            out_ts, out_data = dec(torch.tensor(tsms, dtype=torch.int64),
                                   torch.tensor(data, dtype=torch.float32))
        out_ts = out_ts.flatten().tolist()
        od = out_data.tolist()
        # the decoder expands each event to 3 sub-windows (t-20s/-10s/0); keep only
        # the t=0 row per event (every 3rd) unless STEP_SUB=0 → keep all 3.
        idx = range(len(od)) if os.environ.get("STEP_SUB") == "0" else range(2, len(od), 3)
        rows = [[out_ts[i] / 60000.0 - OFFSET] + od[i] for i in idx]
        rows.sort(key=lambda r: r[0])
        return rows

    step_rows = None
    # The gait decode is correct (verified: a continuous run → stable ~2.7 Hz cadence),
    # but feeding it alone into this partial pipeline changes segmentation in unhelpful
    # ways (on a high-step day the model stops flagging an embedded run as a discrete
    # workout; the app compensates with GPS + pastActivity + tuned post-processing we
    # don't have). So real gait is opt-in (STEPMOTION=1); default keeps the stub.
    if os.environ.get("STEPMOTION") == "1":
        try:
            step_rows = build_stepmotion()
        except Exception as e:
            if args.verbose:
                print(f"stepmotion build failed ({e}); using stub", file=sys.stderr)
    if step_rows:
        # NaN boundary rows so the model's valid-time still spans the full met range
        lo = min(step_rows[0][0], float(met_t[0, 0]))
        hi = max(step_rows[-1][0], float(met_t[-1, 0]))
        step_t = torch.tensor([[lo] + [float("nan")] * 11] + step_rows + [[hi] + [float("nan")] * 11],
                              dtype=torch.float32)
    else:
        # stub: NaN features spanning the FULL range (its last timestamp caps the
        # model's last_valid_time and would otherwise truncate every series).
        step_t = torch.full((2, 12), float("nan"), dtype=torch.float32)
        step_t[0, 0] = met_t[0, 0]
        step_t[1, 0] = met_t[-1, 0]
    STEP_N = len(step_rows) if step_rows else 0

    if args.verbose:
        print("series time ranges (unix minutes):", file=sys.stderr)
        for t, n in [(met_t, "met"), (motion_t, "motion"), (temp_t, "temp"), (hr_t, "hr")]:
            r = f"{len(t)} rows  [{int(t[0,0])}..{int(t[-1,0])}] min" if len(t) else "EMPTY"
            print(f"  {n}: {r}", file=sys.stderr)

    d = datetime.datetime.utcfromtimestamp(anchor_unix + args.tz * 3600)
    context = torch.tensor([d.year, d.month, d.day, d.weekday()], dtype=torch.float32)
    user = torch.tensor([30, 1, 1.78, 78] + [float("nan")] * 10, dtype=torch.float32)

    m = torch.jit.load(str(MODEL), map_location="cpu").eval()
    with torch.no_grad():
        workouts, _, _segments = m(
            context, user, met_t, step_t, motion_t, temp_t, hr_t,
            None, None, torch.tensor(args.threshold), torch.tensor(args.min_duration), torch.tensor(0.0))

    def to_local(minute):
        return datetime.datetime.utcfromtimestamp((minute + OFFSET) * 60 + args.tz * 3600)

    # workouts[n,9] = [start_min, end_min, is_workout_prob, id1,p1, id2,p2, id3,p3]
    sessions = []
    for w in workouts.tolist():
        start, end, is_wk = w[0], w[1], w[2]
        s0, s1 = to_local(start), to_local(end)
        top = [(BEHAVIOR.get(int(w[3 + 2 * k]), str(int(w[3 + 2 * k]))), round(w[4 + 2 * k], 3))
               for k in range(3)]
        sessions.append({
            "start": s0.strftime("%Y-%m-%d %H:%M"),
            "end": s1.strftime("%H:%M"),
            "duration_min": round(end - start),
            "is_workout": round(is_wk, 3),
            "label": top[0][0],
            "label_confidence": top[0][1],
            "top3": top,
        })

    if args.json:
        print(jsonlib.dumps({"model": MODEL_VERSION, "sessions": sessions}, indent=2))
        return

    print(f"Activity sessions — Oura automatic_activity_detection v{MODEL_VERSION} (the ring's own model)\n")
    if not sessions:
        print("  No activity segments detected.")
        return
    print(f"  {'date':<10} {'time':<13} {'dur':>4}  {'workout':>7}  activity (model confidence)")
    for s in sessions:
        date, hm = s["start"].split(" ")
        span = f"{hm}-{s['end']}"
        wk = f"{s['is_workout']:.2f} {'✓' if s['is_workout'] >= args.threshold else ' '}"
        alt = "   ".join(f"{n} {p:.2f}" for n, p in s["top3"][1:])
        print(f"  {date:<10} {span:<13} {s['duration_min']:>3}m  {wk:>7}  {s['label']} {s['label_confidence']:.2f}   ·   {alt}")
    print("\n  Labels are Oura's model, not a heuristic. ✓ = is_workout ≥ "
          f"{args.threshold:.2f}.")
    note = "with real gait" if os.environ.get("STEPMOTION") == "1" else "stubbed"
    print(f"  Type accuracy is limited: the gait ('stepmotion') channel is {note}. We can")
    print("  decode real gait from real_step events (STEPMOTION=1), but it doesn't improve")
    print("  detection here without the app's GPS/history; default is the MET/motion/HR/temp guess.")


if __name__ == "__main__":
    main()
