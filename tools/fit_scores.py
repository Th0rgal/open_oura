#!/usr/bin/env python3
"""Recover Oura's daily-score combiner weights from a trends CSV export.

The decompiled ecore combiner is `round(Σ wᵢ·contributorᵢ / 100)` with fixed
per-contributor weights summing to 100. Those weight tables don't read back from
`libappecore.so`, but the account "Trends" export pairs each day's final score
with its contributor sub-scores — so we recover the weights by regressing the
final score on the contributors (non-negative, sum-to-100 constrained).

Export from your Oura account (web) → this CSV. Pass its path or drop it on the
Desktop / repo root.

Usage: python tools/fit_scores.py [CSV]
"""
import csv
import sys
from pathlib import Path

import numpy as np

REPO = Path(__file__).resolve().parent.parent

# final score -> its contributor columns (exact trends-CSV header names, incl. the
# "Sleep Timin Score" typo Oura ships).
SCORES = {
    "Sleep Score": [
        "Total Sleep Score", "Sleep Efficiency Score", "Restfulness Score",
        "REM Sleep Score", "Deep Sleep Score", "Sleep Latency Score", "Sleep Timin Score",
    ],
    "Readiness Score": [
        "Previous Night Score", "Sleep Balance Score", "Previous Day Activity Score",
        "Activity Balance Score", "Temperature Score", "Resting Heart Rate Score",
        "HRV Balance Score", "Recovery Index Score",
    ],
    "Activity Score": [
        "Stay Active Score", "Move Every Hour Score", "Meet Daily Targets Score",
        "Training Frequency Score", "Training Volume Score",
    ],
}


def find_csv(arg):
    if arg:
        return Path(arg)
    cands = list(Path.home().glob("Desktop/oura_*trends.csv")) + \
        list(REPO.glob("oura_*trends.csv")) + list(REPO.glob("*trends*.csv"))
    if not cands:
        sys.exit("no trends CSV found — pass the path (export from your Oura account)")
    return cands[0]


def num(v):
    v = (v or "").strip()
    return float(v) if v and v != "None" else None


def fit_weights(C, y):
    """Recover combiner weights. The combiner is a plain weighted average
    (final = Σ wᵢ·cᵢ / 100, weights summing to 100), so unconstrained OLS with
    no intercept is well-conditioned and recovers the weights directly; we just
    renormalise to sum to exactly 100 for display."""
    coef, *_ = np.linalg.lstsq(C, y, rcond=None)  # final ≈ C · coef
    return coef * 100.0, coef.sum()               # weights (%), raw Σ (≈1.0 = good)


def main():
    csv_path = find_csv(sys.argv[1] if len(sys.argv) > 1 else None)
    rows = list(csv.DictReader(open(csv_path)))
    print(f"trends: {csv_path.name}  ({len(rows)} days)\n")

    for score, contribs in SCORES.items():
        X, Y = [], []
        for r in rows:
            vals = [num(r.get(c)) for c in contribs] + [num(r.get(score))]
            if None not in vals:
                X.append(vals[:-1])
                Y.append(vals[-1])
        if len(X) < len(contribs) + 2:
            print(f"{score}: only {len(X)} complete rows — skipping\n")
            continue
        C, y = np.array(X), np.array(Y)
        w, raw_sum = fit_weights(C, y)
        pred = C @ (w / 100.0)
        resid = pred - y
        r2 = 1 - (resid ** 2).sum() / ((y - y.mean()) ** 2).sum()

        print(f"{score}  —  {len(y)} days,  R²={r2:.4f},  "
              f"RMSE={np.sqrt((resid**2).mean()):.2f},  max|err|={np.abs(resid).max():.1f},  "
              f"round-match: {100*(np.abs(np.round(pred)-y)<=1).mean():.0f}% within ±1")
        for name, weight in sorted(zip(contribs, w), key=lambda t: -t[1]):
            print(f"    {weight:5.1f}%  {name}")
        print(f"    Σ = {w.sum():.1f}%  (raw fit Σ = {raw_sum:.3f}; ~1.0 confirms a weighted average)\n")


if __name__ == "__main__":
    main()
