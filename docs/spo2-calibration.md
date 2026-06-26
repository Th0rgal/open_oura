# SpO2: turning the ring's R-ratio into a percentage

The ring's **`spo2_r_pi_event`** (tag `0x8b`) carries, per sample, an `r`
(ratio-of-ratios) and a perfusion index (`pi`) — **not** an SpO2 percentage. Two
paths convert it:

1. **Production nightly feature** — uses `API_SPO2_EVENT` (0x6f) /
   `API_SPO2_SMOOTHED_EVENT` (0x70), which are **percentages already computed in
   the ring firmware**. We don't capture those, and that R→% math lives in firmware.
2. **"SpO2 Simple" path** — converts the `r` of `spo2_r_pi_event` in-app with a
   per-hardware quadratic. **This is the one we can reproduce** from our data.

## The formula

```
SpO2(%) = a·r² + b·r + c          clamped to [85, 100]
```

The polynomial runs inside `libecore` (`EcoreWrapper.nativeCalculateSpO2Simple`),
but the **coefficients are in the app** (`com/ouraring/oura/workitem/data/items/d.java`),
selected by ring hardware type:

| hardware | a | b | c |
| --- | --- | --- | --- |
| gen4 / oreo | −13.4 | −5.1 | 105.2 |
| cooper | −12.1 | −6.9 | 106.3 |

`pi` (perfusion index) is passed to the native algo as a quality input; there is no
in-app PI threshold. The daily value is clamped to integer **85–100**.

**Ring 5 caveat:** its exact hardware→coefficient mapping isn't pinned down in the
decompiled app (v7.18 predates Ring 5 in that table); the two sets differ by <1% on
our data, so the result is robust either way. Default is gen4/oreo.

## Running it

```
python tools/run_spo2.py [DB] [--hw gen4|cooper] [--night]
```

One night (26 492 samples, `--night`): mean **93.4%**, median 93%, p5 90%, min 85%
(clamp floor); ~5% of samples <90%. (A crude generic `110−25·r` gives ~91%, which is
wrong — use the calibrated quadratic.)
