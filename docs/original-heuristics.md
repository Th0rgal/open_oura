# Original heuristics (open_oura — NOT Oura algorithms)

Code under `crates/oura-analysis/src/original/` is **our own** logic. It does not
reproduce any Oura algorithm and will not match the Oura app. Use it for things
Oura computes in its cloud (and we therefore cannot port) or that Oura doesn't
offer. Everything Oura-derived lives under `ported/` instead and cites its source
`ecore function @ address`.

## `activity_session` — workout / exposure detector

A transparent, threshold-based detector built on the ring signals we decode (MET,
high-intensity motion, skin temperature, heart rate). It classifies each minute
and groups same-kind runs into sessions:

- **Swim** — active **and** a low temperature *floor* (the ring equilibrating to
  pool water).
- **Sauna** — a high-temperature *spike* (heat exposure), regardless of motion.
- **ColdExposure** — sustained low temperature without swim-level activity.
- **Workout** — elevated activity at normal skin temperature.

Distinct kinds never merge, so an adjacent swim and sauna stay separate. Thresholds
(`Config`) are *our* choices, documented in code.

### Validation (captured 2026-06-24)
Run against a real session it correctly recovered, with the right wall-clock times
(anchored to the stored `captured_unix`):

```
10:54–11:20  Swim   27min  peakMET 9.3   temp[30.4, 33.7]   <- the swim
11:22–11:29  Sauna   8min  peakMET 2.9   temp[35.0, 42.4]   <- the sauna
11:54–12:11  ColdExposure  temp[22.0, 30.0]                 <- post-sauna plunge
```

Note: the ring's temperature sensor reads skin/environment, so water and sauna
dominate the reading — which is exactly what makes environment classification
possible from temperature alone.
