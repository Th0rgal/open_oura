//! Sleep debt. Ported from `sleep_debt_calculate @ 0x215658`: a linear-decay
//! weighted sum of nightly shortfall (need − actual), capped and rounded.
//! Index 0 = most recent night. See `docs/algorithms/sleep-debt.md`.

/// Tunable config (defaults from `sleep_debt_get_default_config @ 0x215a2c`).
#[derive(Clone, Copy, Debug)]
pub struct SleepDebtConfig {
    pub min_valid_days: u32,
    pub max_debt_s: i32,
    pub rounding_step_s: i32,
}

impl Default for SleepDebtConfig {
    fn default() -> Self {
        Self {
            min_valid_days: 5,
            max_debt_s: 36_000,    // 10 h
            rounding_step_s: 2700, // 45 min
        }
    }
}

#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct SleepDebt {
    pub debt_s: i32,
    pub recent_shortfall_s: i32,
    pub valid: bool,
}

const SENTINEL: i32 = i32::MAX;

/// Sleep debt from per-day actual and needed sleep (seconds), index 0 = most
/// recent. Days with actual/need of 0 or `i32::MAX` are treated as invalid.
pub fn sleep_debt(actual_s: &[i32], need_s: &[i32], cfg: &SleepDebtConfig) -> SleepDebt {
    let n = actual_s.len().min(need_s.len());
    if n == 0 {
        return SleepDebt {
            debt_s: SENTINEL,
            recent_shortfall_s: SENTINEL,
            valid: false,
        };
    }
    let shortfall = |d: usize| -> Option<f64> {
        let (a, need) = (actual_s[d], need_s[d]);
        if a == 0 || a == SENTINEL || need == 0 || need == SENTINEL {
            None
        } else {
            Some(need as f64 - a as f64)
        }
    };
    // newest day weight 1.0, oldest ~0.25 (linear decay 0.75/(n-1))
    let decay = if n == 1 { 0.0 } else { 0.75 / (n as f64 - 1.0) };
    let mut debt = 0.0;
    let mut valid = 0u32;
    for d in 0..n {
        if let Some(sf) = shortfall(d) {
            valid += 1;
            debt += (1.0 - decay * d as f64) * sf;
        }
    }
    let recent = match shortfall(0) {
        Some(v) => v as i32,
        None => {
            return SleepDebt {
                debt_s: 0,
                recent_shortfall_s: SENTINEL,
                valid: false,
            }
        }
    };
    debt = debt.max(0.0).min(cfg.max_debt_s as f64);
    if cfg.rounding_step_s != 0 {
        debt = (debt / cfg.rounding_step_s as f64).round() * cfg.rounding_step_s as f64;
    }
    SleepDebt {
        debt_s: debt as i32,
        recent_shortfall_s: recent,
        valid: valid >= cfg.min_valid_days,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn five_nights_one_hour_short() {
        // need 8 h, slept 7 h, 5 nights -> shortfall 3600 each
        let debt = sleep_debt(&[25200; 5], &[28800; 5], &SleepDebtConfig::default());
        // weights 1,0.8125,0.625,0.4375,0.25 sum 3.125 -> 11250 -> round/2700 -> 10800
        assert_eq!(debt.debt_s, 10800);
        assert_eq!(debt.recent_shortfall_s, 3600);
        assert!(debt.valid);
    }
}
