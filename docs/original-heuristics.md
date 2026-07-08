# Original heuristics (retired)

open_oura used to ship its own logic under `crates/oura-analysis/src/original/`
for things Oura computes in its cloud. The only such module was
`activity_session` — a threshold-based workout/swim/sauna/cold detector built on
MET, motion, skin temperature and HR.

**It has been removed.** Activity detection now belongs to the app layer in
[`open_health`](https://github.com/Th0rgal/open_health), which can run Oura's
decrypted `automatic_activity_detection` model instead of the old heuristic.

Why the switch: the heuristic classified purely by temperature, so it mislabeled
a morning run as a "Swim" (the ring's skin-temperature reading tripped the
swim/water branch) and emitted spurious short "Swim" blips from warm-water hand
contact. The model labels the same run `running 0.91` from MET/motion/HR.

Everything Oura-derived (faithfully ported from the on-device `ecore` engine,
citing `ecore function @ address`) still lives under
`crates/oura-analysis/src/ported/`.
