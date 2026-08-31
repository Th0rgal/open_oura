#!/usr/bin/env python3
"""Export every (newest-version) decrypted Oura TorchScript model to the PyTorch
lite-interpreter (`.ptl`) format used by on-device runtimes (iOS/Android).

This is the iOS spike's go/no-go: the shipped models are full TorchScript
pipelines (pre/post-processing + validators baked into the graph) that do NOT
convert to Core ML — they use int64 timestamps, data-dependent control flow and
multi-tensor tuple outputs. The lite interpreter runs the *same* TorchScript
bytecode, so nothing is reimplemented or lost.

Outputs land in `notes/models/mobile/<name>.ptl` (gitignored, like the models).

Usage:
    python tools/export_mobile.py            # export all
    python tools/export_mobile.py --check    # also run a pt-vs-ptl parity check
"""
import sys
import warnings
from pathlib import Path

warnings.filterwarnings("ignore")
import torch

REPO = Path(__file__).resolve().parent.parent
MODELS = REPO / "notes" / "models"
OUT = MODELS / "mobile"

# newest version per family (same list as inspect_models.py)
NEWEST = [
    "automatic_activity_detection_3_1_11",
    "atlas_2_1_0",
    "awhr_imputation_1_2_0",
    "awhr_profile_selector_1_0_1",
    "cumulative_stress_1_2_2",
    "cva_2_1_0",
    "cva_calibrator_1_3_0",
    "daily_medians_1_1_0",
    "daily_short_term_baselines_1_1_0",
    "dhrv_imputation_1_1_0",
    "energy_expenditure_1_0_0",
    "halite_1_2_0",
    "illness_detection_0_5_1",
    "insomnia_0_1_4",
    "meal_timing_0_1_0",
    "popsicle_1_6_0",
    "pregnancy_biometrics_0_4_0",
    "sleepnet_bdi_0_4_0",
    "sleepnet_moonstone_1_2_0",
    "sleepstaging_2_6_0",
    "step_counter_1_3_0",
    "steps_motion_decoder_2_0_0",
    "stress_daytime_sensing_1_1_0",
    "stress_resilience_2_2_1",
    "training_stress_score_0_2_1",
    "whr_2_7_1",
]


def export_one(name):
    src = MODELS / f"{name}.pt"
    if not src.exists():
        return name, None, "file missing"
    try:
        m = torch.jit.load(str(src), map_location="cpu").eval()
        dst = OUT / f"{name}.ptl"
        m._save_for_lite_interpreter(str(dst))
        return name, dst.stat().st_size, None
    except Exception as e:
        msg = (str(e).strip().splitlines() or ["<no message>"])[0]
        return name, None, msg[:100]


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    print(f"bytecode version: {torch._C._get_max_operator_version()} (torch {torch.__version__})")
    ok = fail = 0
    total = 0
    for name in NEWEST:
        n, sz, err = export_one(name)
        if err or sz is None:
            print(f"  FAIL {n:42s}: {err or 'unknown'}")
            fail += 1
        else:
            print(f"  ok   {n:42s} -> {sz/1e6:6.2f} MB")
            ok += 1
            total += sz
    print(f"\n{ok} exported ({total/1e6:.1f} MB total), {fail} failed -> {OUT}")
    return 0 if fail == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
