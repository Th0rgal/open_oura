use oura_analysis::ported::baseline::{Baseline, EcoreBaselineConfig, InvalidBaselineMetric};
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixture {
    synthetic_only: bool,
    cases: Vec<Case>,
}
#[derive(Deserialize)]
struct Case {
    metric: usize,
    feature23: bool,
    mean_x8: i32,
    dev_x8: i32,
    sample: i16,
    age: u32,
    output: [i32; 2],
    mean_min_x8: Option<[i16; 9]>,
}
fn check(fixture: Fixture) {
    assert!(fixture.synthetic_only);
    assert!(!fixture.cases.is_empty());
    for (index, case) in fixture.cases.into_iter().enumerate() {
        let mut state = Baseline {
            mean_x8: case.mean_x8,
            dev_x8: case.dev_x8,
        };
        let mut config = EcoreBaselineConfig {
            feature_23: case.feature23,
            ..Default::default()
        };
        if let Some(minima) = case.mean_min_x8 {
            config.mean_min_x8 = minima;
        }
        state
            .update_ecore_with_config(case.sample, case.age, case.metric, &config)
            .unwrap();
        assert_eq!(
            [state.mean_x8, state.dev_x8],
            case.output,
            "reference case {index}, metric {}, age {}, feature23 {}",
            case.metric,
            case.age,
            case.feature23
        );
    }
}

#[test]
fn generated_current_android_reference() {
    let fixture: Fixture =
        serde_json::from_str(include_str!("../fixtures/baseline-current-android.json")).unwrap();
    assert_eq!(fixture.cases.len(), 128);
    check(fixture);
}

#[test]
fn current_metric_limits_differ_from_unclamped_ema() {
    let mut legacy = Baseline::new();
    legacy.update(100, 0);
    assert_eq!([legacy.mean_x8, legacy.dev_x8], [400, 50]);
    let mut current = Baseline::new();
    current
        .update_ecore_with_config(100, 0, 0, &EcoreBaselineConfig::default())
        .unwrap();
    assert_eq!([current.mean_x8, current.dev_x8], [680, 50]);
}

#[test]
fn invalid_metric_does_not_mutate_state() {
    let mut state = Baseline {
        mean_x8: -1,
        dev_x8: 40000,
    };
    let before = state;
    assert_eq!(
        state.update_ecore_with_config(100, 30, 9, &EcoreBaselineConfig::default()),
        Err(InvalidBaselineMetric(9))
    );
    assert_eq!(state, before);
}

#[test]
fn configured_minimum_above_maximum_is_preserved() {
    let mut config = EcoreBaselineConfig::default();
    config.mean_min_x8[0] = 900;
    let mut state = Baseline::new();
    state.update_ecore_with_config(0, 0, 0, &config).unwrap();
    assert_eq!([state.mean_x8, state.dev_x8], [900, 0]);
}

#[test]
#[ignore = "optional complete generated reference; checked-in subset runs normally"]
fn full_generated_reference() {
    let path = std::env::var("OURA_SYNTHETIC_BASELINE_REFERENCE").unwrap();
    let fixture: Fixture = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(fixture.cases.len(), 82346);
    check(fixture);
}
