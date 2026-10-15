# Personal baseline updates

`Baseline::update` preserves the original community port's unclamped asymmetric
EMA. That operation alone does not reproduce the current native per-metric
update: ecore also normalizes stored 16-bit values, limits outliers and clamps
results using metric-specific tables.

`Baseline::update_ecore_with_config` adds that current operation, matched against
recorded generated controls from Oura Android 7.26.0, build 260930084,
`baseline_update_lt_mean_and_dev` at ELF offset `0xe1aa8`.

The caller must supply:

- The native metric index, in `0..=8`, with its corresponding sample units.
- The signed 16-bit sample and actual baseline age in days.
- The nine accepted native lower limits, stored as signed fixed-point values.
- The actual feature-23 setting. It changes the outlier/clamp policy for indices
  4 and 5.

The configuration's default values describe the measured fresh table. Native
initialization can change the first two lower limits, including with profile
age and feature selection. A caller reproducing an existing initialized context
must supply those accepted values rather than assuming the defaults describe it.
The updater does not select features, initialize account history or persist it.

Mean and deviation are stored as real values multiplied by eight. The existing
Rust fields remain `i32` for API compatibility; the native operation narrows the
stored inputs and update steps with signed 16-bit semantics. Negative stored
values normalize to the native `0x7eb6` no-data limit. A lower limit greater than
its corresponding maximum follows the native comparison order rather than
panicking in `clamp`. Invalid metric indices return `InvalidBaselineMetric`
before changing either field.

For example, the original unclamped update from zero with sample 100 at age 0
produces `[400, 50]`. The current native update for metric 0 produces `[680, 50]`
after applying that metric's minimum. Both operations remain available so callers
can choose the intended contract explicitly.

## Regression evidence

`crates/oura-analysis/fixtures/baseline-current-android.json` contains 128 generated
input/output controls selected from a retained 82,346-case Android reference.
They cover all nine indices, both feature settings, age-band boundaries,
16-bit normalization and supplied lower limits. The fixture contains reference
fingerprints and generated values only; it contains no personal health data,
model weights or native binaries.

```sh
cargo test -p oura-analysis --locked --test baseline_current
```

An optional ignored test accepts the complete generated reference, whose digest
is recorded in the fixture:

```sh
OURA_SYNTHETIC_BASELINE_REFERENCE=/path/to/android-baseline-reference.json \
  cargo test -p oura-analysis --locked --release --test baseline_current \
  full_generated_reference -- --ignored
```

All 82,346 recorded updates match. This establishes the configured update
operation for the measured version; choosing the correct metric inputs,
configuration, day order and retained state remains the host's responsibility.
