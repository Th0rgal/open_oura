//! Nightly skin temperature + deviation. Ported from
//! `nightly_temperature_calculate @ 0x203520`: a 7-sample sliding median, then
//! 30-sample windows; each window contributes its max when its range < 2.50 °C,
//! and the nightly value is the **minimum of those window maxima** (needs >= 4
//! valid windows). Units: ring temperature is i16 centi-°C (value/100 = °C).
//! See `docs/algorithms/temperature.md`.

const WINDOW: usize = 30; // samples (0x1e)
const RANGE_THRESHOLD: u16 = 250; // 2.50 °C in centi-°C (0xfa)
const MIN_WINDOWS: usize = 4;

fn median7(buf: &[u16; 7]) -> u16 {
    let mut s = *buf;
    s.sort_unstable();
    s[3]
}

/// Nightly temperature (centi-°C) from per-sample temps (centi-°C; 0 = invalid).
/// `None` if fewer than 4 valid 30-sample windows passed the range gate.
pub fn nightly_temperature(samples: &[u16]) -> Option<i16> {
    let mut ring = [0u16; 7];
    let mut idx = 0usize;
    let mut win_min = u16::MAX;
    let mut win_max = 0u16;
    let mut maxima: Vec<u16> = Vec::new();
    for (i, &s) in samples.iter().enumerate() {
        ring[idx] = s;
        idx = if idx == 6 { 0 } else { idx + 1 };
        let m = median7(&ring);
        if m != 0 {
            win_min = win_min.min(m);
            win_max = win_max.max(m);
        }
        if (i + 1) % WINDOW == 0 {
            if win_max >= win_min && win_max != 0 && win_max - win_min < RANGE_THRESHOLD {
                maxima.push(win_max);
            }
            win_min = u16::MAX;
            win_max = 0;
        }
    }
    if maxima.len() < MIN_WINDOWS {
        return None;
    }
    maxima.into_iter().min().map(|v| v as i16)
}

/// Temperature deviation (centi-°C) = nightly temperature − personal baseline mean.
pub fn temperature_deviation(nightly_centi: i16, baseline_mean_centi: f64) -> f64 {
    nightly_centi as f64 - baseline_mean_centi
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_night() {
        // 150 steady samples at 35.00 °C -> 5 windows, all max 3500 -> 3500
        let s = vec![3500u16; 150];
        assert_eq!(nightly_temperature(&s), Some(3500));
    }

    #[test]
    fn too_few_windows() {
        // 60 samples -> 2 windows, 50 -> 1 window; both < 4 required -> None
        assert_eq!(nightly_temperature(&[3500u16; 60]), None);
        assert_eq!(nightly_temperature(&[3500u16; 50]), None);
    }
}
