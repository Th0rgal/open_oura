#!/usr/bin/env python3
"""Reconstruct ALL three Oura daily scores (Sleep, Readiness, Activity) end-to-end
from raw metrics in a trends export — the same two-layer approach as Sleep:

  weights (round(Σ wᵢ·subᵢ/100), tools/fit_scores.py)  ×  contributor curves.

Readiness/Activity differ from Sleep: several contributors are **baseline-relative**
(HRV Balance, Resting HR, Sleep/Activity Balance) or **sequential** (Previous Night,
Previous Day Activity, Training Frequency/Volume). We engineer lag-1 and trailing-
mean features so a linear fit can form "today vs personal baseline"; what stays low
is what genuinely needs accumulated history the export can't encode statically.

Reports per-contributor and end-to-end held-out R² for each score:
  ceiling  = weights × Oura's actual sub-scores
  achieved = weights × our sub-scores predicted from raw metrics

Usage: python tools/fit_scores_all.py [CSV]
"""
import csv
import sys
from pathlib import Path

import numpy as np

from fit_sleep_score import EmpCurve, LinFit, MonoCurve, engineer, getval, num, r2

REPO = Path(__file__).resolve().parent.parent

WEIGHTS = {
    "Sleep Score": {
        "Total Sleep Score": 35, "Restfulness Score": 15, "Sleep Efficiency Score": 10,
        "REM Sleep Score": 10, "Deep Sleep Score": 10, "Sleep Latency Score": 10,
        "Sleep Timin Score": 10,
    },
    "Readiness Score": {
        "Resting Heart Rate Score": 17, "Previous Night Score": 15, "HRV Balance Score": 15,
        "Temperature Score": 13, "Sleep Balance Score": 12, "Previous Day Activity Score": 10,
        "Recovery Index Score": 10, "Activity Balance Score": 8,
    },
    "Activity Score": {
        "Move Every Hour Score": 33, "Meet Daily Targets Score": 24, "Stay Active Score": 17,
        "Training Volume Score": 15, "Training Frequency Score": 12,
    },
}
DRIVERS = {
    # Sleep (see fit_sleep_score.py)
    "Total Sleep Score": (["Total Sleep Duration"], "mono"),
    "Sleep Efficiency Score": (["Sleep Efficiency"], "mono"),
    "REM Sleep Score": (["REM Sleep Duration"], "mono"),
    "Deep Sleep Score": (["Deep Sleep Duration"], "mono"),
    "Sleep Latency Score": (["Sleep Latency"], "emp"),
    "Restfulness Score": (["awake_frac", "restless_frac", "Sleep Efficiency"], "lin"),
    "Sleep Timin Score": (["mid_opt", "mid_reg", "mid_hour"], "lin"),
    # Readiness — baseline-relative contributors get (current, trailing-mean) pairs
    "Previous Night Score": (["Sleep Score"], "mono"),
    "Resting Heart Rate Score": (["Lowest Resting Heart Rate", "Average Resting Heart Rate",
                                  "trail14_Average Resting Heart Rate"], "lin"),
    "HRV Balance Score": (["Average HRV", "trail14_Average HRV"], "lin"),
    "Temperature Score": (["Temperature Deviation (°C)"], "emp"),
    "Sleep Balance Score": (["Total Sleep Duration", "trail14_Total Sleep Duration"], "lin"),
    "Previous Day Activity Score": (["prev_Average MET", "prev_Steps"], "lin"),
    "Activity Balance Score": (["prev_Average MET", "trail14_Average MET"], "lin"),
    "Recovery Index Score": (["Average Resting Heart Rate", "Lowest Resting Heart Rate"], "lin"),
    # Activity
    "Move Every Hour Score": (["Long Periods of Inactivity"], "emp"),
    "Stay Active Score": (["Inactive Time"], "mono"),
    # Meet Daily Targets tracks a personalised daily activity goal (not in the export);
    # no single-day driver predicts it well (|corr|≤0.2) — this is the cap on Activity.
    "Meet Daily Targets Score": (["Activity Burn", "Steps", "Average MET"], "lin"),
    # Training Volume/Frequency sit at 100 on ~92% of days for this user (rarely the
    # binding constraint) — a constant ≈ their mean predicts the final score well;
    # the true multi-day training-load logic isn't recoverable from single days.
    "Training Volume Score": (["Average MET"], "mean"),
    "Training Frequency Score": (["Average MET"], "mean"),
}
LAG = ["Sleep Score", "Activity Score", "Average MET", "Steps"]
TRAIL = {7: ["Average MET", "High Activity Time", "Medium Activity Time"],
         14: ["Average HRV", "Average Resting Heart Rate", "Total Sleep Duration", "Average MET"]}


class ConstCurve:
    """Constant predictor (training mean) for near-saturated contributors."""
    def __init__(self, y):
        self.v = float(np.mean(y))

    def predict(self, x):
        return np.full(len(np.atleast_1d(x)), self.v)


def engineer_seq(rows):
    """Add lag-1 and causal trailing-mean features (rows already date-sorted by
    fit_sleep_score.engineer)."""
    for base in LAG:
        prev = None
        for r in rows:
            r[f"prev_{base}"] = prev
            prev = num(r.get(base))
    for win, bases in TRAIL.items():
        for base in bases:
            hist = []
            for r in rows:
                r[f"trail{win}_{base}"] = float(np.mean(hist[-win:])) if hist else None
                v = num(r.get(base))
                if v is not None:
                    hist.append(v)
    return rows


def main():
    cands = ([Path(sys.argv[1])] if len(sys.argv) > 1 else
             list(Path.home().glob("Desktop/oura_*trends.csv")) + list(REPO.glob("*trends*.csv")))
    if not cands:
        sys.exit("no trends CSV — pass the path")
    rows = engineer_seq(engineer(list(csv.DictReader(open(cands[0])))))
    print(f"trends: {cands[0].name}  ({len(rows)} days)\n")

    rng = np.random.default_rng(0)

    for score, weights in WEIGHTS.items():
        contribs = list(weights)
        need = {score, *contribs}
        for c in contribs:
            need.update(DRIVERS[c][0])
        data = [{k: getval(r, k) for k in need} for r in rows]
        data = [d for d in data if all(v is not None for v in d.values())]
        if len(data) < 40:
            print(f"{score}: only {len(data)} complete rows — skipping\n")
            continue
        idx = rng.permutation(len(data))
        cut = int(len(data) * 0.8)
        tr, te = idx[:cut], idx[cut:]
        col = lambda rows_idx, name: np.array([data[i][name] for i in rows_idx])

        print(f"=== {score} === ({len(data)} complete days)")
        curves = {}
        for c in contribs:
            drivers, kind = DRIVERS[c]
            ytr, yte = col(tr, c), col(te, c)
            if kind == "mean":
                cv = ConstCurve(ytr); pte = cv.predict(col(te, drivers[0]))
            elif kind == "mono":
                cv = MonoCurve(col(tr, drivers[0]), ytr); pte = cv.predict(col(te, drivers[0]))
            elif kind == "emp":
                cv = EmpCurve(col(tr, drivers[0]), ytr); pte = cv.predict(col(te, drivers[0]))
            else:
                cv = LinFit(np.column_stack([col(tr, d) for d in drivers]), ytr)
                pte = cv.predict(np.column_stack([col(te, d) for d in drivers]))
            curves[c] = (cv, drivers, kind)
            print(f"  {weights[c]:2d}%  {kind:4s}  {c:28s} R²={r2(pte, yte):+.3f}")

        def predict_sub(rows_idx, c):
            cv, drivers, kind = curves[c]
            if kind in ("mono", "emp", "mean"):
                return cv.predict(col(rows_idx, drivers[0]))
            return cv.predict(np.column_stack([col(rows_idx, d) for d in drivers]))

        w = np.array([weights[c] for c in contribs])
        actual = np.column_stack([col(te, c) for c in contribs]) @ w / 100.0
        pred = np.column_stack([np.clip(predict_sub(te, c), 1, 100) for c in contribs]) @ w / 100.0
        y = col(te, score)
        for label, p in [("ceiling ", actual), ("achieved", pred)]:
            print(f"  {label}: R²={r2(p, y):.4f}  RMSE={np.sqrt(((p-y)**2).mean()):.2f}  "
                  f"max|err|={np.abs(p-y).max():.1f}  within±1: {100*(np.abs(np.round(p)-y)<=1).mean():.0f}%"
                  f"  ±3: {100*(np.abs(np.round(p)-y)<=3).mean():.0f}%")
        print()


if __name__ == "__main__":
    main()
