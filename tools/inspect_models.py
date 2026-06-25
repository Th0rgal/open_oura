#!/usr/bin/env python3
"""Introspect every (newest-version) decrypted Oura model: dump its forward()
input/output schema and top-level submodules, so we know each model's input
contract before wiring it to our data.

Usage: python tools/inspect_models.py [--json]
Models live in notes/models/ (gitignored).
"""
import json
import sys
from pathlib import Path

import torch

REPO = Path(__file__).resolve().parent.parent
MODELS_DIR = REPO / "notes" / "models"

# newest version per family (drop superseded duplicates)
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


def describe(name):
    path = MODELS_DIR / f"{name}.pt"
    info = {"model": name, "loaded": False}
    if not path.exists():
        info["error"] = "file missing"
        return info
    try:
        m = torch.jit.load(str(path), map_location="cpu").eval()
    except Exception as e:
        info["error"] = f"load failed: {e}"
        return info
    info["loaded"] = True

    # forward schema (arg names + types + defaults)
    try:
        sch = m.forward.schema
        args = []
        for a in sch.arguments:
            if a.name == "self":
                continue
            d = {"name": a.name, "type": str(a.type)}
            if a.has_default_value():
                d["default"] = str(a.default_value)
            args.append(d)
        info["forward_args"] = args
        info["forward_returns"] = str(sch.returns[0].type) if sch.returns else None
    except Exception as e:
        info["forward_error"] = str(e)
        # fall back to listing callable methods
        try:
            info["methods"] = [n for n in dir(m) if not n.startswith("_")][:40]
        except Exception:
            pass

    # top-level named children (architecture hint)
    try:
        info["submodules"] = [n for n, _ in m.named_children()]
    except Exception:
        pass
    return info


def main():
    as_json = "--json" in sys.argv
    results = [describe(n) for n in NEWEST]
    if as_json:
        print(json.dumps(results, indent=2))
        return
    for r in results:
        print("=" * 78)
        print(r["model"])
        if not r.get("loaded"):
            print(f"  !! {r.get('error')}")
            continue
        if "forward_args" in r:
            print("  forward(")
            for a in r["forward_args"]:
                dv = f" = {a['default']}" if "default" in a else ""
                print(f"      {a['name']}: {a['type']}{dv}")
            print(f"  ) -> {r['forward_returns']}")
        else:
            print(f"  forward schema unavailable: {r.get('forward_error')}")
            if r.get("methods"):
                print(f"  methods: {r['methods']}")
        if r.get("submodules"):
            print(f"  submodules: {r['submodules']}")


if __name__ == "__main__":
    main()
