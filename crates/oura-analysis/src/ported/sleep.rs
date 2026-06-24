//! Sleep-stage decode and duration summary.
//!
//! The packed 30-second hypnogram is produced by the staging model (not ecore —
//! see `sleepnet`); ecore decodes it in `calculate_sleep_score_numerical @
//! 0x1f4444` as packed nibbles (two epochs per byte, `stage = nibble & 0xF`
//! clamped `< 5`). The stage enum follows `SleepPhase_OSSAv1` (0=deep, 1=light,
//! 2=rem, 3=awake, 4=other/non-wear). The exact per-stage aggregation helper isn't
//! in the decompile, so the summary below counts epochs per stage (the standard
//! reconstruction). See `docs/algorithms/sleep-summary.md`.

pub const EPOCH_SECONDS: u32 = 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Deep,
    Light,
    Rem,
    Awake,
    Other,
}

impl Stage {
    fn from_code(c: u8) -> Stage {
        match c {
            0 => Stage::Deep,
            1 => Stage::Light,
            2 => Stage::Rem,
            3 => Stage::Awake,
            _ => Stage::Other,
        }
    }
    fn is_asleep(self) -> bool {
        matches!(self, Stage::Deep | Stage::Light | Stage::Rem)
    }
}

/// Decode the packed-nibble 30-second stage array into per-epoch stage codes
/// (0..4). Two epochs per byte; a byte `< 0x10` ends the series.
pub fn decode_stages(packed: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(packed.len() * 2);
    for &b in packed {
        let lo = b & 0x0f;
        out.push(if lo < 5 { lo } else { 0 });
        if b < 0x10 {
            break;
        }
        let hi = b >> 4;
        out.push(if hi < 5 { hi } else { 0 });
    }
    out
}

/// Sleep summary derived from a 30-second epoch stage series.
#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct SleepSummary {
    pub total_sleep_s: u32,
    pub deep_s: u32,
    pub light_s: u32,
    pub rem_s: u32,
    pub awake_s: u32,
    pub onset_latency_s: u32,
    pub wake_count: u32,
    pub efficiency: f64,
}

/// Summarize an epoch stage-code series (+ optional time-in-bed seconds) into
/// stage durations, sleep-onset latency, wake count and efficiency.
pub fn summarize(stage_codes: &[u8], time_in_bed_s: Option<u32>) -> SleepSummary {
    let mut s = SleepSummary::default();
    let mut first_sleep: Option<usize> = None;
    let mut prev_awake = true;
    for (i, &c) in stage_codes.iter().enumerate() {
        match Stage::from_code(c) {
            Stage::Deep => s.deep_s += EPOCH_SECONDS,
            Stage::Light => s.light_s += EPOCH_SECONDS,
            Stage::Rem => s.rem_s += EPOCH_SECONDS,
            Stage::Awake => s.awake_s += EPOCH_SECONDS,
            Stage::Other => {}
        }
        let st = Stage::from_code(c);
        if st.is_asleep() && first_sleep.is_none() {
            first_sleep = Some(i);
        }
        if first_sleep.is_some() {
            let awake = matches!(st, Stage::Awake);
            if awake && !prev_awake {
                s.wake_count += 1;
            }
            prev_awake = awake;
        }
    }
    s.total_sleep_s = s.deep_s + s.light_s + s.rem_s;
    s.onset_latency_s = first_sleep.map(|i| i as u32 * EPOCH_SECONDS).unwrap_or(0);
    let denom = time_in_bed_s
        .unwrap_or(stage_codes.len() as u32 * EPOCH_SECONDS)
        .max(1);
    s.efficiency = s.total_sleep_s as f64 / denom as f64;
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_packed_nibbles() {
        // 0x21 -> [light(1), rem(2)]; 0x03 -> [awake(3)] then stop (b<0x10)
        assert_eq!(decode_stages(&[0x21, 0x03]), vec![1, 2, 3]);
    }

    #[test]
    fn summary_durations() {
        let s = summarize(&[1, 2, 3], None); // light, rem, awake
        assert_eq!(s.light_s, 30);
        assert_eq!(s.rem_s, 30);
        assert_eq!(s.awake_s, 30);
        assert_eq!(s.total_sleep_s, 60);
        assert_eq!(s.wake_count, 1);
        assert!((s.efficiency - 60.0 / 90.0).abs() < 1e-9);
    }
}
