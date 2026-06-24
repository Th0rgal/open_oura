//! Blood-oxygen saturation. Ported from ecore `spo2_simple_calculate @ 0x22ad50`:
//! a quadratic calibration of the red/IR ratio `R` → SpO2 %, clamped to [0, 120].
//! The ring streams raw `R` (and perfusion index) in `spo2_r_pi_event` (tag 0x8b);
//! the per-device calibration coefficients {a, b, c} come from the ring.
//! See docs/algorithms/spo2.md.

/// SpO2 (%) from the ratio `r` and quadratic calibration `a + b*r + c*r^2`.
pub fn spo2_simple(r: f64, a: f64, b: f64, c: f64) -> f64 {
    (a + b * r + c * r * r).clamp(0.0, 120.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quadratic_and_clamp() {
        assert_eq!(spo2_simple(0.5, 100.0, -20.0, 0.0), 90.0);
        assert_eq!(spo2_simple(5.0, 100.0, -20.0, 0.0), 0.0); // clamped
    }
}
