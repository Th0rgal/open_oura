# Nightly temperature

The nightly aggregation below matches recorded generated controls from Oura
Android 7.26.0, build 260930084, `nightly_temperature_calculate` at ELF offset
`0x10c9e4`. This version-specific evidence does not establish the behavior of
every Oura release.

1. Apply a seven-sample sliding median, initially filled with zeroes.
2. Divide the filtered stream into complete 30-sample windows. Ignore zero
   medians when calculating each window's minimum and maximum.
3. Keep a window's maximum if its nonzero range is strictly less than 250
   centidegrees. Discard the incomplete final window.
4. Require at least four valid windows. Select the **second-highest** retained
   maximum, counting repeated values separately.

For example, window maxima `[3374, 3356, 3372, 3387]` produce `3374`;
`[3029, 3029, 3022, 3015]` produce `3029`. Selecting the minimum, maximum, or
second distinct value gives different results on these controls.

The public Rust API remains `Option<i16>` in centidegrees Celsius, with `None`
for insufficient valid windows. Its existing signed conversion is unchanged.
`temperature_deviation` still subtracts the supplied personal baseline.

## Regression evidence

`crates/oura-analysis/fixtures/nightly-temperature-order-statistic.json` contains
the exact generated inputs and recorded expected outputs for the two examples.
It contains no personal health data, model weights, or native library bytes.
The fixture records the APK/library fingerprints and the retained reference
digest so the source of the expected values is explicit. The regression target
also checks increasing windows, duplicate maxima and a partial final window:

```sh
cargo test -p oura-analysis --locked --test temperature_order_statistic
```

Across the retained 4,096 generated shaped controls, selecting the minimum
mismatched 2,033 outputs; selecting the second-highest with duplicates
mismatched none. These are aggregation controls, not evidence of full nightly
input preparation, personal baseline parity or clinical accuracy.
