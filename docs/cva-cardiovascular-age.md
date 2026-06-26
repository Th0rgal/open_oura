# Cardiovascular age from raw PPG (`cva_ppg` → `cva_raw_ppg_data` → CVA model)

> **Models are not included in this repo.** The decrypted `cva_2_1_0.pt` (and every
> other `.pt`) is Oura's proprietary IP; it is **not committed** (gitignored under
> `notes/models/`). Run this against your own locally-decrypted copy.

When the **`cva_ppg`** feature (`CAP_CVA_PPG_SAMPLER`, id `0x0d`) is enabled, the
ring records short raw-PPG bursts overnight and emits them as **`cva_raw_ppg_data`**
events (BLE tag **`0x81`**). This is the raw photoplethysmography waveform the
cardiovascular-age (CVA) model needs — the one signal the PPG-gated models used to
be blocked on. With it captured, we can run Oura's own CVA model offline.

## Decoding tag `0x81`

Each event body is **signed int8 deltas**; a cumulative sum reconstructs the PPG
ADC samples. The Rust decoder (`oura-protocol`, `decode_cva_raw_ppg`) emits
`{"ppg_samples": [...], "n": N}` per event (per-event cumsum). A full measurement
spans a *burst* of consecutive events **<2 s apart** (~1503 samples ≈ 10 s at the
ring-reported rate, ~140 Hz on Ring 5); concatenate the burst's deltas, then cumsum.

Validation that the decode is correct: the reconstructed waveform is pulsatile and
its **pulse rate matches the independent IBI-derived heart rate** (e.g. 38 bpm from
PPG peaks vs 41 bpm from IBI in the same window).

## The model (`cva_2_1_0`, `CardiovascularAgeV2Model`)

```
forward(ppg_segments [n, 1500] f32, demographics [1, 5] f32)
  -> (daily_cva, quality, raw_quality, daily_pwv, ppg_segment_metrics[n, 11])
```

- **`ppg_segments`**: groups of **exactly 1500** raw samples (the app drops any
  group that isn't 1500). No filtering/normalization in the app — preprocessing is
  inside the `.pt`. Feed raw reconstructed samples.
- **`demographics` = `[sex, height_m, age_years, ring_size, weight_kg]`**, where
  **sex = −1 female / +1 male / 0 other**. These come from the user profile, not
  the ring (the ring's `user_information` event has no anthropometrics).

### Outputs
- **`daily_cva`** = **vascular age (years)**. It is *anchored to the input age* and
  adjusted by PPG morphology (age 25 → 16.0, 40 → 24.6, 60 → 38.4 on our data — i.e.
  consistently ~10–15 y younger than chronological = healthy arteries). **Pass real
  demographics for a meaningful absolute number.**
- **`daily_pwv`** = **pulse-wave velocity (m/s)**, PPG-only (~4.5 m/s on our data;
  lower = less arterial stiffness).
- `quality` / `raw_quality` = signal-quality scores; `ppg_segment_metrics` = 11
  per-segment features.

## Running it

```
python tools/run_cva_model.py [DB] --sex M --age 30 --height 1.78 --weight 75 --ring 10
```

On one night (10 valid 1500-sample segments from 18 measurements) with placeholder
demographics: vascular age ≈ 18.8 y, PWV ≈ 4.5 m/s, quality 2 / raw 0.70.

## Caveats
- Absolute `daily_cva` is only meaningful with the user's real demographics.
- We segment by a 2 s event-gap heuristic; the app groups by the protobuf
  `measurementStartTimestamp` (decoded by the native lib we don't run), so a borderline
  burst could be split differently. The model still produces a stable value.
- The exact training sample rate isn't confirmed from Java; segments are fed as raw
  samples (the model bakes in its canonical rate). See `tools/run_cva_model.py`.
