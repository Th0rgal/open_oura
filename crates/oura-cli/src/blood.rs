//! Backend for the `/blood` explorer page.
//!
//! Mirrors [`crate::dna`]: the front-end (`dashboard/web/blood.*`) renders the JSON
//! this module computes. Today the *inputs* are a mocked panel (a real SYNLAB draw
//! series, hand-transcribed) but the **computation is real** — status vs. reference
//! range, trend across draws, and which markers deserve attention are all derived
//! here, so wiring up true PDF extraction later only swaps the input source.
//!
//! Planned (not yet built): `import` parses an uploaded lab PDF, dedupes by content
//! hash so re-importing the same file is a no-op, and caches results in a small
//! local SQLite (`blood.db`) that is **separate from the ring's `oura.db`**.

use serde_json::{json, Value};

/// The draw dates we have a panel for (ISO), oldest → newest.
const DRAWS: [&str; 5] = ["2025-07-18", "2025-09-10", "2025-11-11", "2026-02-25", "2026-06-09"];

/// The mocked source files (what an `imports` list will look like once real).
const IMPORTS: [(&str, &str, u64, &str); 5] = [
    ("blood 18-07-2025.pdf", "2025-07-18", 314_200, "a1f4c9"),
    ("blood 10-09-2025.pdf", "2025-09-10", 42_655, "7b02e1"),
    ("blood 11-11-2025.pdf", "2025-11-11", 56_208, "3c9d40"),
    ("blood 25-02-2026.pdf", "2026-02-25", 249_999, "e57a88"),
    ("blood 09-06-2026.pdf", "2026-06-09", 285_222, "bd1266"),
];

/// Which direction is healthier — controls trend colour + which side is "concerning".
#[derive(Clone, Copy, PartialEq)]
enum Dir {
    Up,   // higher is better (HDL, eGFR, vitamin D…)
    Down, // lower is better (LDL, CRP, urea…)
    Mid,  // a window is best (sodium, calcium…)
}
use Dir::*;

struct M {
    key: &'static str,
    name: &'static str,
    unit: &'static str,
    panel: &'static str,
    low: Option<f64>,
    high: Option<f64>,
    ref_text: Option<&'static str>, // override when the derived text is imprecise
    good: Dir,
    about: &'static str,
    advice: Option<&'static str>,
    values: [Option<f64>; 5],
}

/// Panels in display order.
const PANELS: [&str; 8] = [
    "Lipids & cardiovascular",
    "Metabolic",
    "Kidney",
    "Liver",
    "Complete blood count",
    "Vitamins & minerals",
    "Hormones",
    "Inflammation",
];

// Short constructor to keep the table readable.
#[allow(clippy::too_many_arguments)]
fn m(
    key: &'static str,
    name: &'static str,
    unit: &'static str,
    panel: &'static str,
    low: Option<f64>,
    high: Option<f64>,
    ref_text: Option<&'static str>,
    good: Dir,
    about: &'static str,
    advice: Option<&'static str>,
    values: [Option<f64>; 5],
) -> M {
    M { key, name, unit, panel, low, high, ref_text, good, about, advice, values }
}

fn markers() -> Vec<M> {
    let n = |x: f64| Some(x);
    vec![
        // ── Lipids & cardiovascular ──────────────────────────────────────────
        m("chol_total", "Total cholesterol", "mg/dL", PANELS[0], None, n(190.0), Some("< 190"), Down,
          "All cholesterol carried in the blood; a coarse first look at lipid health.",
          Some("Above range and the main thing to keep working on. It has fallen steadily since July — keep going: less saturated fat, more soluble fibre and regular aerobic exercise. ApoB and LDL are the sharper targets to watch alongside it."),
          [n(278.0), n(262.0), n(250.0), n(229.0), n(214.0)]),
        m("ldl", "LDL cholesterol", "mg/dL", PANELS[0], None, n(116.0), Some("< 116"), Down,
          "The cholesterol most directly linked to arterial plaque.",
          Some("Still above the general target but trending down nicely (185 → 138). For low overall risk aim under 116 mg/dL; diet and exercise are doing the work — recheck in ~3 months."),
          [n(185.0), n(176.0), n(168.0), n(154.0), n(138.0)]),
        m("hdl", "HDL cholesterol", "mg/dL", PANELS[0], n(40.0), None, Some("> 40"), Up,
          "The \"protective\" fraction that helps clear cholesterol.",
          Some("Comfortably protective. Kept high by aerobic exercise and unsaturated fats."),
          [n(76.0), n(72.0), n(70.0), n(65.0), n(66.0)]),
        m("trig", "Triglycerides", "mg/dL", PANELS[0], None, n(150.0), Some("< 150"), Down,
          "Circulating fat; sensitive to refined carbs, alcohol and recent meals.",
          Some("Excellent and well under range. A marker of a low-sugar, low-alcohol pattern."),
          [n(85.0), n(72.0), n(64.0), n(50.0), n(55.0)]),
        m("apob", "Apolipoprotein B", "mg/dL", PANELS[0], n(46.0), n(174.0), None, Down,
          "One ApoB per atherogenic particle — the truest count of plaque-forming particles.",
          Some("Inside the lab range and falling. Many prevention guidelines aim under 100 mg/dL; you are essentially there and heading lower."),
          [n(121.0), n(112.0), n(105.0), n(98.0), n(90.0)]),
        m("lpa", "Lipoprotein(a)", "nmol/L", PANELS[0], None, n(75.0), Some("< 75"), Down,
          "A largely genetic, independent cardiovascular risk particle.",
          Some("Borderline and mostly set by genetics — it barely moves with lifestyle. Worth knowing because it adds to risk independently of LDL: a reason to keep the modifiable markers tight."),
          [n(102.1), n(88.0), n(70.0), n(58.3), n(61.0)]),
        // ── Metabolic ────────────────────────────────────────────────────────
        m("glucose", "Fasting glucose", "mg/dL", PANELS[1], n(74.0), n(106.0), None, Down,
          "Blood sugar after an overnight fast.",
          None,
          [n(76.0), n(77.0), n(78.0), n(79.0), n(80.0)]),
        m("hba1c", "HbA1c", "%", PANELS[1], None, n(5.7), Some("< 5.7"), Down,
          "Average blood sugar over ~3 months.",
          Some("Firmly in the healthy range (an optimal target is under 5.4%). No action needed."),
          [n(5.1), n(5.1), n(5.0), n(5.0), n(4.9)]),
        m("insulin", "Fasting insulin", "µUI/mL", PANELS[1], n(3.0), n(25.0), None, Down,
          "How hard the pancreas works to hold glucose steady.",
          Some("Just under the lab's low bound, which here reads as high insulin sensitivity rather than a problem — HOMA-IR confirms it. Nothing to do."),
          [n(2.8), n(4.0), n(5.6), n(2.8), n(3.2)]),
        m("homa", "HOMA-IR", "", PANELS[1], n(1.92), n(2.2), Some("1.92 - 2.20"), Down,
          "Insulin-resistance index (lower means more insulin-sensitive).",
          Some("Well below the reference — excellent insulin sensitivity."),
          [n(0.52), n(0.70), n(1.10), n(0.55), n(0.60)]),
        m("uric", "Uric acid", "mg/dL", PANELS[1], n(3.5), n(7.2), None, Mid,
          "By-product of purine metabolism; high levels associate with gout.",
          None,
          [n(2.8), n(3.1), n(3.3), n(3.5), n(3.7)]),
        // ── Kidney ───────────────────────────────────────────────────────────
        m("creatinine", "Creatinine", "mg/dL", PANELS[2], n(0.70), n(1.30), None, Mid,
          "Muscle by-product cleared by the kidneys; a core kidney marker.",
          None,
          [n(1.10), n(1.13), n(1.15), n(1.17), n(1.14)]),
        m("egfr", "eGFR", "mL/min/1.73m²", PANELS[2], n(60.0), None, Some("≥ 60"), Up,
          "Estimated kidney filtration rate.",
          Some("Healthy filtration. Values naturally wobble with hydration and recent protein/creatine intake."),
          [n(94.0), n(91.0), n(89.0), n(87.0), n(90.0)]),
        m("urea", "Urea (BUN)", "mg/dL", PANELS[2], n(6.0), n(20.0), None, Down,
          "Nitrogen waste from protein; rises with high protein intake or dehydration.",
          Some("Was mildly high and has come back into range (28 → 19). Usually reflects a high-protein diet or being under-hydrated at the draw rather than kidney trouble — eGFR is normal."),
          [n(28.0), n(25.0), n(24.0), n(22.0), n(19.0)]),
        // ── Liver ────────────────────────────────────────────────────────────
        m("ast", "AST (GOT)", "UI/L", PANELS[3], None, n(34.0), Some("< 34"), Down,
          "Enzyme released by liver (and muscle) cells.",
          Some("Sits right at the upper limit. In an athletic person this often follows hard exercise in the days before the draw rather than a liver issue — ALT and GGT are low, which is reassuring."),
          [n(25.0), n(28.0), n(30.0), n(34.0), n(29.0)]),
        m("alt", "ALT (GPT)", "UI/L", PANELS[3], None, n(49.0), Some("< 49"), Down,
          "The most liver-specific of the routine enzymes.",
          None,
          [n(23.0), n(24.0), n(25.0), n(23.0), n(22.0)]),
        m("ggt", "GGT", "UI/L", PANELS[3], None, n(73.0), Some("< 73"), Down,
          "Sensitive to alcohol and bile flow.",
          None,
          [n(18.0), n(16.0), n(15.0), n(14.0), n(13.0)]),
        m("bilirubin", "Total bilirubin", "mg/dL", PANELS[3], None, n(1.20), Some("< 1.20"), Down,
          "Heme breakdown product processed by the liver.",
          None,
          [n(0.81), n(0.85), n(0.90), n(0.83), n(0.80)]),
        m("albumin", "Albumin", "g/dL", PANELS[3], n(3.2), n(4.8), None, Up,
          "The main blood protein; a marker of liver synthesis and nutrition.",
          None,
          [n(4.8), n(4.8), n(4.7), n(4.7), n(4.8)]),
        // ── Complete blood count ─────────────────────────────────────────────
        m("hemoglobin", "Hemoglobin", "g/dL", PANELS[4], n(13.7), n(17.2), None, Mid,
          "Oxygen-carrying protein in red cells.",
          None,
          [n(13.8), n(14.1), n(14.3), n(14.6), n(14.8)]),
        m("hematocrit", "Hematocrit", "%", PANELS[4], n(40.0), n(50.0), None, Mid,
          "Fraction of blood volume that is red cells.",
          None,
          [n(41.0), n(41.8), n(42.5), n(43.3), n(43.6)]),
        m("rbc", "Red blood cells", "×10¹²/L", PANELS[4], n(4.50), n(5.60), None, Mid,
          "Red cell count.",
          Some("Dipped just below range on the first draw and has climbed comfortably into it since — no longer notable."),
          [n(4.39), n(4.50), n(4.58), n(4.69), n(4.72)]),
        m("wbc", "White blood cells", "×10⁹/L", PANELS[4], n(3.70), n(9.50), None, Mid,
          "Immune cell count; the body's baseline defence level.",
          None,
          [n(4.72), n(4.60), n(4.50), n(4.47), n(4.90)]),
        m("platelets", "Platelets", "×10⁹/L", PANELS[4], n(170.0), n(430.0), None, Mid,
          "Clotting cells.",
          None,
          [n(206.0), n(200.0), n(196.0), n(191.0), n(205.0)]),
        m("rdw", "RDW", "%", PANELS[4], n(11.6), n(14.1), None, Down,
          "Variation in red-cell size; an early flag for some anaemias.",
          None,
          [n(12.3), n(12.1), n(12.0), n(11.9), n(11.8)]),
        // ── Vitamins & minerals ──────────────────────────────────────────────
        m("vit_d", "Vitamin D (25-OH)", "ng/mL", PANELS[5], n(30.0), n(100.0), None, Up,
          "Vitamin/hormone for bone, immune and muscle function.",
          Some("Has climbed from insufficient (42) into the optimal 40–60 band. Whatever you changed — sun, diet or a supplement — is working; hold it through winter."),
          [n(42.3), n(48.0), n(53.0), n(59.8), n(62.0)]),
        m("ferritin", "Ferritin", "µg/L", PANELS[5], n(39.3), n(439.4), None, Mid,
          "Iron stores.",
          None,
          [n(162.0), n(150.0), n(145.0), n(139.0), n(148.0)]),
        m("b12", "Vitamin B12", "ng/L", PANELS[5], n(211.0), n(911.0), None, Up,
          "Needed for nerves and red-cell formation.",
          None,
          [n(879.0), n(840.0), n(810.0), n(779.0), n(760.0)]),
        m("folate", "Folate", "ng/mL", PANELS[5], n(5.4), None, Some("> 5.4"), Up,
          "B-vitamin for DNA synthesis and red cells.",
          None,
          [n(12.4), n(15.0), n(18.0), n(20.0), n(19.0)]),
        m("magnesium", "Magnesium", "mg/dL", PANELS[5], n(1.6), n(2.6), None, Mid,
          "Cofactor for hundreds of enzymes, including muscle and nerve function.",
          None,
          [n(2.2), n(2.2), n(2.1), n(2.2), n(2.3)]),
        m("calcium", "Calcium", "mg/dL", PANELS[5], n(8.7), n(10.4), None, Mid,
          "Tightly regulated mineral for bone, nerves and muscle.",
          None,
          [n(9.9), n(9.8), n(9.9), n(9.9), n(10.0)]),
        // ── Hormones ─────────────────────────────────────────────────────────
        m("tsh", "TSH", "mUI/L", PANELS[6], n(0.55), n(4.78), None, Mid,
          "Pituitary signal that sets thyroid output.",
          None,
          [n(1.68), n(1.90), n(2.04), n(1.87), n(1.95)]),
        m("ft4", "Free T4", "pmol/L", PANELS[6], n(10.3), n(34.7), None, Mid,
          "The circulating thyroid hormone reserve.",
          None,
          [n(14.5), n(15.2), n(15.8), n(16.4), n(16.0)]),
        m("ft3", "Free T3", "pmol/L", PANELS[6], n(3.5), n(6.5), None, Mid,
          "The active thyroid hormone.",
          None,
          [n(3.5), n(3.8), n(4.0), n(4.5), n(4.4)]),
        m("test_total", "Testosterone, total", "ng/dL", PANELS[6], n(197.4), n(669.6), None, Up,
          "Total circulating testosterone (bound + free).",
          Some("Solidly mid-range. Earlier low readings tracked a stressful, under-slept stretch — worth keeping sleep and training load steady, since both move this marker."),
          [n(250.0), n(230.0), n(213.0), n(467.0), n(430.0)]),
        m("test_free", "Testosterone, free", "pg/mL", PANELS[6], n(12.30), n(46.60), None, Up,
          "The biologically active fraction.",
          Some("Back into a healthy range after a dip below it. Follows the same pattern as total testosterone."),
          [n(11.50), n(10.00), n(8.61), n(26.78), n(24.00)]),
        m("dheas", "DHEA-S", "µg/dL", PANELS[6], n(35.0), n(569.0), None, Up,
          "Adrenal androgen and a rough vitality/recovery marker.",
          None,
          [n(236.0), n(280.0), n(310.0), n(345.0), n(360.0)]),
        // ── Inflammation ─────────────────────────────────────────────────────
        m("hscrp", "hs-CRP", "mg/dL", PANELS[7], None, n(0.33), Some("≤ 0.33"), Down,
          "High-sensitivity marker of systemic inflammation and vascular risk.",
          Some("Very low — an optimal, low-inflammation reading (well under the 0.1 mg/dL \"ideal\" line). A good sign for cardiovascular risk."),
          [n(0.05), n(0.06), n(0.07), n(0.08), n(0.05)]),
    ]
}

fn round(v: f64, dp: i32) -> f64 {
    let f = 10f64.powi(dp);
    (v * f).round() / f
}

fn ref_text(mk: &M) -> String {
    if let Some(t) = mk.ref_text {
        return t.to_string();
    }
    match (mk.low, mk.high) {
        (Some(l), Some(h)) => format!("{} - {}", trim(l), trim(h)),
        (Some(l), None) => format!("> {}", trim(l)),
        (None, Some(h)) => format!("< {}", trim(h)),
        (None, None) => "—".into(),
    }
}

fn trim(v: f64) -> String {
    let s = format!("{v}");
    s
}

/// Per-marker computed view (status vs. range, trend across draws, attention flag).
fn marker_json(mk: &M) -> Value {
    let points: Vec<(usize, f64)> = mk
        .values
        .iter()
        .enumerate()
        .filter_map(|(i, v)| v.map(|x| (i, x)))
        .collect();
    let (li, latest) = *points.last().expect("every marker has ≥1 value");
    let prev = if points.len() >= 2 { Some(points[points.len() - 2].1) } else { None };

    let out_low = mk.low.map(|l| latest < l).unwrap_or(false);
    let out_high = mk.high.map(|h| latest > h).unwrap_or(false);
    let status = if out_high { "high" } else if out_low { "low" } else { "normal" };

    // "concerning" side depends on which direction is healthy
    let bad = match mk.good {
        Up => out_low,
        Down => out_high,
        Mid => out_low || out_high,
    };
    let severity = if bad { "watch" } else if out_low || out_high { "good" } else { "neutral" };

    // trend from the last two available draws
    let (trend, trend_good, delta, delta_pct) = match prev {
        Some(p) => {
            let d = latest - p;
            let eps = (latest.abs() * 0.005).max(1e-9);
            let dir = if d.abs() <= eps { "flat" } else if d > 0.0 { "up" } else { "down" };
            let good = if dir == "flat" {
                true
            } else {
                match mk.good {
                    Up => d > 0.0,
                    Down => d < 0.0,
                    Mid => {
                        // improving = moving toward the middle of the range
                        match (mk.low, mk.high) {
                            (Some(l), Some(h)) => {
                                let c = (l + h) / 2.0;
                                (latest - c).abs() <= (p - c).abs()
                            }
                            _ => true,
                        }
                    }
                }
            };
            (dir, good, Some(round(d, 3)), if p != 0.0 { Some(round(d / p * 100.0, 1)) } else { None })
        }
        None => ("flat", true, None, None),
    };

    let pts: Vec<Value> = points
        .iter()
        .map(|(i, v)| json!({ "date": DRAWS[*i], "value": round(*v, 3) }))
        .collect();

    json!({
        "key": mk.key,
        "name": mk.name,
        "unit": mk.unit,
        "panel": mk.panel,
        "about": mk.about,
        "advice": mk.advice,
        "low": mk.low,
        "high": mk.high,
        "ref_text": ref_text(mk),
        "points": pts,
        "latest": round(latest, 3),
        "latest_date": DRAWS[li],
        "prev": prev.map(|p| round(p, 3)),
        "delta": delta,
        "delta_pct": delta_pct,
        "status": status,
        "flagged": bad,
        "severity": severity,
        "trend": trend,
        "trend_good": trend_good,
    })
}

/// `GET /api/blood/report` — the whole mocked panel, computed.
pub fn report() -> Value {
    let mks = markers();
    let markers_json: Vec<Value> = mks.iter().map(marker_json).collect();
    let flagged = markers_json.iter().filter(|m| m["flagged"].as_bool().unwrap_or(false)).count();

    let imports: Vec<Value> = IMPORTS
        .iter()
        .rev() // newest first
        .map(|(file, date, size, sha)| {
            json!({ "file": file, "date": date, "size": size, "sha": sha, "markers": mks.len() })
        })
        .collect();

    json!({
        "mocked": true,
        "panels": PANELS,
        "markers": markers_json,
        "imports": imports,
        "draws": DRAWS,
        "summary": {
            "markers_total": mks.len(),
            "flagged": flagged,
            "imports": IMPORTS.len(),
            "first_date": DRAWS[0],
            "latest_date": DRAWS[DRAWS.len() - 1],
        },
    })
}
