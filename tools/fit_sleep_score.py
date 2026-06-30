#!/usr/bin/env python3
"""Reconstruct Oura's Sleep Score end-to-end from raw sleep metrics.

Two layers:
  1. combiner weights  (tools/fit_scores.py)         : final = Σ wᵢ·subᵢ / 100
  2. contributor curves (this file)                  : subᵢ = fᵢ(raw metricᵢ)

Each contributor sub-score is a deterministic function of a sleep metric, so we
recover fᵢ from the trends export:
  * monotone metrics (total sleep, efficiency, REM, deep, latency) -> isotonic
    regression (Pool-Adjacent-Violators) + linear interpolation — near-exact.
  * multi-input metrics (restfulness, timing) -> linear fit on the available raw
    drivers — only approximate (their true inputs/circadian logic aren't fully in
    the export), which is where the end-to-end residual comes from.

Reports per-contributor fit, then the end-to-end Sleep Score on a held-out split:
the "ceiling" (recovered weights on Oura's *actual* sub-scores) vs the achieved
(weights on *our predicted* sub-scores).

Usage: python tools/fit_sleep_score.py [CSV]
"""
import csv
import datetime as dt
import sys
from pathlib import Path

import numpy as np

REPO = Path(__file__).resolve().parent.parent

# recovered weights (tools/fit_scores.py / docs/algorithms/score-weights.md)
WEIGHTS = {
    "Total Sleep Score": 35, "Restfulness Score": 15, "Sleep Efficiency Score": 10,
    "REM Sleep Score": 10, "Deep Sleep Score": 10, "Sleep Latency Score": 10,
    "Sleep Timin Score": 10,
}
# contributor -> (raw driver columns, kind)
#   "mono" : single monotone driver  -> isotonic curve (near-exact)
#   "emp"  : single non-monotone driver -> binned empirical curve (e.g. latency U-shape)
#   "lin+" : several drivers -> linear fit then a 1-D empirical recalibration
DRIVERS = {
    "Total Sleep Score": (["Total Sleep Duration"], "mono"),
    "Sleep Efficiency Score": (["Sleep Efficiency"], "mono"),
    "REM Sleep Score": (["REM Sleep Duration"], "mono"),
    "Deep Sleep Score": (["Deep Sleep Duration"], "mono"),
    "Sleep Latency Score": (["Sleep Latency"], "emp"),
    # Restfulness caps ~R²0.70 here: its true driver (Oura's internal restless-period
    # / wake-up count, computed from movement) isn't in the export — only aggregates.
    "Restfulness Score": (["awake_frac", "restless_frac", "Sleep Efficiency"], "lin"),
    # Timing is circadian: a peaked curve on bedtime-midpoint (optimal ~3am) plus
    # day-to-day regularity — engineered below from the Bedtime Start/End timestamps.
    "Sleep Timin Score": (["mid_opt", "mid_reg", "mid_hour"], "lin"),
}


def parse_dt(ts):
    ts = (ts or "").strip()
    if not ts or ts == "None":
        return None
    try:
        return dt.datetime.fromisoformat(ts)
    except ValueError:
        return None


def engineer(rows):
    """Add derived feature columns the raw export doesn't have directly:
    bedtime-midpoint hour, distance-from-optimal, 7-day regularity, awake fraction.
    Regularity is causal (trailing window) so rows must be in date order."""
    rows = sorted(rows, key=lambda r: r.get("date", ""))
    mids = []
    for r in rows:
        s, e = parse_dt(r.get("Bedtime Start")), parse_dt(r.get("Bedtime End"))
        if s and e:
            m = s + (e - s) / 2
            h = m.hour + m.minute / 60.0
            if h < 12:
                h += 24            # wrap small-hours so ~24-30 (midnight-6am)
        else:
            h = None
        r["mid_hour"] = h
        r["mid_opt"] = -((h - 27.0) ** 2) if h is not None else None  # peak at 03:00
        prev = [x for x in mids[-7:] if x is not None]
        r["mid_reg"] = (abs(h - np.mean(prev)) if (h is not None and prev) else 0.0)
        mids.append(h)
        aw = num(r.get("Awake Time"))
        rs = num(r.get("Restless Sleep"))
        tb = num(r.get("Total Bedtime "))           # note: trailing space in header
        if tb is None:
            ts = num(r.get("Total Sleep Duration"))
            tb = (aw + ts) if aw is not None and ts is not None else None
        r["awake_frac"] = (aw / tb if aw is not None and tb else None)
        r["restless_frac"] = (rs / tb if rs is not None and tb else None)
    return rows


def num(v):
    v = (v or "").strip()
    try:
        return float(v) if v and v != "None" else None
    except ValueError:
        return None


def pav(y):
    """Pool-Adjacent-Violators: nearest monotone-increasing fit to y (in order)."""
    y = y.astype(float).copy()
    w = np.ones_like(y)
    # stack of (value, weight, count)
    vals, wts, cnt = [], [], []
    for yi in y:
        v, ww, c = yi, 1.0, 1
        while vals and vals[-1] > v:
            pv, pw, pc = vals.pop(), wts.pop(), cnt.pop()
            v = (v * ww + pv * pw) / (ww + pw)
            ww += pw
            c += pc
        vals.append(v); wts.append(ww); cnt.append(c)
    out = np.empty_like(y)
    i = 0
    for v, c in zip(vals, cnt):
        out[i:i + c] = v
        i += c
    return out


class MonoCurve:
    """Isotonic + linear-interpolation curve for a single monotone driver.
    Direction-aware: if the driver correlates negatively with the sub-score
    (e.g. inactive-time → stay-active), fit on the negated driver so the
    increasing-isotonic fit still applies."""
    def __init__(self, raw, sub):
        self.sign = -1.0 if np.corrcoef(raw, sub)[0, 1] < 0 else 1.0
        x = self.sign * raw
        order = np.argsort(x)
        self.xs = x[order]
        self.ys = pav(sub[order])

    def predict(self, x):
        return np.interp(self.sign * np.asarray(x, float), self.xs, self.ys)


class EmpCurve:
    """Binned empirical curve for a single non-monotone driver (e.g. latency)."""
    def __init__(self, raw, sub, bins=24):
        qs = np.quantile(raw, np.linspace(0, 1, bins + 1))
        qs = np.unique(qs)
        centers, means = [], []
        for lo, hi in zip(qs[:-1], qs[1:]):
            m = (raw >= lo) & (raw <= hi)
            if m.sum():
                centers.append(raw[m].mean())
                means.append(sub[m].mean())
        self.xs = np.array(centers)
        self.ys = np.array(means)

    def predict(self, x):
        return np.interp(x, self.xs, self.ys)


class LinFit:
    """Plain linear fit on >=1 drivers (for multi-input contributors whose true
    micro-inputs aren't fully in the export — so only approximate)."""
    def __init__(self, X, y):
        A = np.hstack([X, np.ones((len(X), 1))])
        self.b, *_ = np.linalg.lstsq(A, y, rcond=None)

    def predict(self, X):
        return np.hstack([X, np.ones((len(X), 1))]) @ self.b


def getval(r, c):
    v = r.get(c)
    return v if isinstance(v, (int, float)) or v is None else num(v)


def load(csv_path):
    rows = engineer(list(csv.DictReader(open(csv_path))))
    cols = {"Sleep Score", *WEIGHTS}
    for drv, _ in DRIVERS.values():
        cols.update(drv)
    clean = []
    for r in rows:
        d = {c: getval(r, c) for c in cols}
        if all(v is not None for v in d.values()):
            clean.append(d)
    return clean


def r2(pred, y):
    return 1 - ((pred - y) ** 2).sum() / ((y - y.mean()) ** 2).sum()


def main():
    cands = ([Path(sys.argv[1])] if len(sys.argv) > 1 else
             list(Path.home().glob("Desktop/oura_*trends.csv")) + list(REPO.glob("*trends*.csv")))
    if not cands:
        sys.exit("no trends CSV — pass the path")
    data = load(cands[0])
    print(f"trends: {cands[0].name}  ({len(data)} complete days)\n")

    rng = np.random.default_rng(0)
    idx = rng.permutation(len(data))
    cut = int(len(data) * 0.8)
    tr, te = idx[:cut], idx[cut:]

    def colv(rows_idx, name):
        return np.array([data[i][name] for i in rows_idx])

    print("contributor curves (held-out R²):")
    curves = {}
    for sub, (drivers, kind) in DRIVERS.items():
        ytr, yte = colv(tr, sub), colv(te, sub)
        if kind == "mono":
            c = MonoCurve(colv(tr, drivers[0]), ytr)
            pte = c.predict(colv(te, drivers[0]))
        elif kind == "emp":
            c = EmpCurve(colv(tr, drivers[0]), ytr)
            pte = c.predict(colv(te, drivers[0]))
        else:  # lin+
            c = LinFit(np.column_stack([colv(tr, d) for d in drivers]), ytr)
            pte = c.predict(np.column_stack([colv(te, d) for d in drivers]))
        curves[sub] = (c, drivers, kind)
        print(f"  {WEIGHTS[sub]:2d}%  {kind:4s}  {sub:24s} R²={r2(pte, yte):+.3f}  "
              f"RMSE={np.sqrt(((pte-yte)**2).mean()):4.1f}  <- {', '.join(drivers)}")

    # end-to-end Sleep Score on the held-out set
    def predict_sub(rows_idx, sub):
        c, drivers, kind = curves[sub]
        if kind in ("mono", "emp"):
            return c.predict(colv(rows_idx, drivers[0]))
        return c.predict(np.column_stack([colv(rows_idx, d) for d in drivers]))

    w = np.array([WEIGHTS[s] for s in WEIGHTS])
    actual_sub = np.column_stack([colv(te, s) for s in WEIGHTS])
    pred_sub = np.column_stack([predict_sub(te, s) for s in WEIGHTS])
    y = colv(te, "Sleep Score")
    ceiling = actual_sub @ w / 100.0          # weights on Oura's real sub-scores
    achieved = pred_sub @ w / 100.0           # weights on our predicted sub-scores

    print("\nend-to-end Sleep Score (held-out days):")
    for label, p in [("ceiling (weights × Oura's actual sub-scores)", ceiling),
                     ("achieved (weights × our sub-scores from raw)", achieved)]:
        print(f"  {label}")
        print(f"      R²={r2(p, y):.4f}  RMSE={np.sqrt(((p-y)**2).mean()):.2f}  "
              f"max|err|={np.abs(p-y).max():.1f}  "
              f"within±1: {100*(np.abs(np.round(p)-y)<=1).mean():.0f}%  "
              f"±3: {100*(np.abs(np.round(p)-y)<=3).mean():.0f}%")


if __name__ == "__main__":
    main()
