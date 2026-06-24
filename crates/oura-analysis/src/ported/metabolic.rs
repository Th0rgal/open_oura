//! Metabolic helpers ported from ecore's activity/calorie path. These match the
//! standard published formulas the decompile uses (constants confirmed against
//! `libappecore.so` .rodata). See `docs/algorithms/activity.md`.

/// Anthropometric VO2max estimate (Jackson NES), ecore ELF `0x12bc78`:
/// `79.9 − 0.39·age − 13.7·female − 0.127·(weight_kg·2.20462)`, floored at 0.
pub fn vo2max_jackson(age_years: f64, female: bool, weight_kg: f64) -> f64 {
    let weight_lb = weight_kg * 2.20462;
    (79.9 - 0.39 * age_years - 13.7 * (female as i32 as f64) - 0.127 * weight_lb).max(0.0)
}

/// Schofield basal metabolic rate (kcal/day) by age band and sex — the WHO/
/// Schofield coefficients ecore's BMR path (ELF `0x10d7e4`) uses. `sex`: 0=male,
/// 1=female; anything else returns the male/female average.
pub fn bmr_schofield(age_years: f64, sex: u8, weight_kg: f64) -> f64 {
    fn band(coeffs: &[(f64, f64, f64)], age: f64, kg: f64) -> f64 {
        let &(_, s, i) = coeffs
            .iter()
            .find(|&&(hi, _, _)| age < hi)
            .unwrap_or(coeffs.last().unwrap());
        s * kg + i
    }
    // (upper-age-bound, slope, intercept)
    const MALE: [(f64, f64, f64); 6] = [
        (3.0, 59.512, -30.4),
        (10.0, 22.706, 504.3),
        (18.0, 17.686, 658.2),
        (30.0, 15.057, 692.2),
        (60.0, 11.472, 873.1),
        (f64::INFINITY, 11.711, 587.7),
    ];
    const FEMALE: [(f64, f64, f64); 6] = [
        (3.0, 58.317, -31.1),
        (10.0, 20.315, 485.9),
        (18.0, 13.384, 692.6),
        (30.0, 14.818, 486.6),
        (60.0, 8.126, 845.6),
        (f64::INFINITY, 9.082, 658.5),
    ];
    match sex {
        0 => band(&MALE, age_years, weight_kg),
        1 => band(&FEMALE, age_years, weight_kg),
        _ => (band(&MALE, age_years, weight_kg) + band(&FEMALE, age_years, weight_kg)) / 2.0,
    }
}

/// Walking distance from step count, ecore `actinfo_steps_to_meters @ 0x1cd624`:
/// `0.762 m` per step.
pub fn steps_to_meters(steps: u32) -> u32 {
    (steps as f64 * 0.762) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vo2max_plausible() {
        // 30yo male, 75 kg -> ~47
        let v = vo2max_jackson(30.0, false, 75.0);
        assert!((44.0..50.0).contains(&v), "{v}");
    }

    #[test]
    fn bmr_adult_male() {
        // 30-60 band: 11.472*80 + 873.1 = 1790.86
        assert!((bmr_schofield(40.0, 0, 80.0) - 1790.86).abs() < 0.01);
    }

    #[test]
    fn distance() {
        assert_eq!(steps_to_meters(10000), 7620);
    }
}
