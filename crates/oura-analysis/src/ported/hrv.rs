//! Heart-rate variability. Ported from ecore `hrv @ 0x1e7984`: RMSSD of the
//! inter-beat-interval series — the textbook `sqrt(mean(diff(ibi)^2))`. The ring
//! also reports a 5-min average RMSSD directly in `hrv_event`; this lets us
//! compute RMSSD over any IBI window we decode.
//! See docs/algorithms/hrv.md.

/// RMSSD (ms) over a sequence of inter-beat intervals (ms). Needs >= 2 intervals.
pub fn rmssd(ibi_ms: &[u16]) -> Option<f64> {
    if ibi_ms.len() < 2 {
        return None;
    }
    let mut sum_sq = 0f64;
    for w in ibi_ms.windows(2) {
        let d = w[1] as f64 - w[0] as f64;
        sum_sq += d * d;
    }
    Some((sum_sq / (ibi_ms.len() - 1) as f64).sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rmssd_textbook() {
        // diffs 20,-10,20 -> squares 400,100,400 = 900/3 = 300 -> ~17.32
        let v = rmssd(&[800, 820, 810, 830]).unwrap();
        assert!((v - 17.3205).abs() < 1e-3, "{v}");
    }
}
