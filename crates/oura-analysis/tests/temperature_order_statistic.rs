use oura_analysis::ported::temperature::nightly_temperature;
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixture {
    synthetic_only: bool,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    samples: Vec<u16>,
    expected: i16,
}

#[test]
fn generated_android_reference_cases() {
    // Generated inputs and recorded outputs only; no account data or models.
    let fixture: Fixture = serde_json::from_str(include_str!(
        "../fixtures/nightly-temperature-order-statistic.json"
    ))
    .unwrap();
    assert!(fixture.synthetic_only);
    assert_eq!(fixture.cases.len(), 2);
    for case in fixture.cases {
        assert_eq!(
            nightly_temperature(&case.samples),
            Some(case.expected),
            "{}",
            case.name
        );
    }
}

fn windows(levels: &[u16]) -> Vec<u16> {
    levels.iter().flat_map(|&level| [level; 30]).collect()
}

#[test]
fn four_increasing_windows_select_second_highest() {
    assert_eq!(
        nightly_temperature(&windows(&[3500, 3520, 3540, 3560])),
        Some(3540)
    );
}

#[test]
fn duplicate_highest_windows_count_separately() {
    assert_eq!(
        nightly_temperature(&windows(&[3500, 3520, 3540, 3560, 3560])),
        Some(3560)
    );
}

#[test]
fn incomplete_final_window_does_not_change_selection() {
    let mut samples = windows(&[3500, 3520, 3540, 3560]);
    // This would be a valid range-gated window if incorrectly flushed early.
    samples.extend([3700; 29]);
    assert_eq!(nightly_temperature(&samples), Some(3540));
}
