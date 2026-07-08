//! Backend for the `/blood` explorer page.
//!
//! The dashboard renders the JSON this module computes. PDF imports are local:
//! `pdftotext -layout` extracts text, deterministic parsers normalize known
//! SYNLAB/CUF rows, and the results are cached in `blood.db`, separate from the
//! ring's `oura.db`.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use regex::Regex;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// Which direction is healthier - controls trend colour + which side is
/// "concerning".
#[derive(Clone, Copy, PartialEq)]
enum Dir {
    Up,   // higher is better (HDL, eGFR, vitamin D...)
    Down, // lower is better (LDL, CRP, urea...)
    Mid,  // a window is best (sodium, calcium...)
}
use Dir::*;

#[derive(Clone, Copy)]
struct MarkerDef {
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
    aliases: &'static [&'static str],
}

#[derive(Clone, Debug)]
struct ParsedMarker {
    key: String,
    raw_name: String,
    value: f64,
    unit: String,
    low: Option<f64>,
    high: Option<f64>,
    ref_text: Option<String>,
}

#[derive(Clone, Debug)]
struct ParsedReport {
    filename: String,
    path: String,
    sha: String,
    size: u64,
    lab: String,
    collection_date: String,
    text_hash: String,
    status: String,
    warnings: Vec<String>,
    markers: Vec<ParsedMarker>,
}

#[derive(Clone, Debug)]
struct ImportRow {
    sha: String,
    filename: String,
    path: String,
    lab: String,
    collection_date: String,
    size: u64,
    text_hash: String,
    status: String,
    warnings: Vec<String>,
}

#[derive(Clone, Debug)]
struct MarkerRow {
    import_sha: String,
    key: String,
    raw_name: String,
    value: f64,
    unit: String,
    low: Option<f64>,
    high: Option<f64>,
    ref_text: Option<String>,
    date: String,
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

const MIN_EXPECTED_MARKERS: usize = 4;
static BLOOD_DIR: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Called from `main.rs` with the `--blood-files` value, if any.
pub fn set_files_dir(dir: Option<PathBuf>) {
    let _ = BLOOD_DIR.set(dir.map(expand_tilde));
}

fn expand_tilde(p: PathBuf) -> PathBuf {
    if let Ok(rest) = p.strip_prefix("~") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    p
}

fn default_health_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let p = PathBuf::from(home).join("Documents/official/health");
    p.exists().then_some(p)
}

fn blood_dir() -> Option<PathBuf> {
    if let Some(Some(d)) = BLOOD_DIR.get() {
        return Some(d.clone());
    }
    if let Some(env) = std::env::var_os("OURA_BLOOD_FILES") {
        if !env.is_empty() {
            return Some(expand_tilde(PathBuf::from(env)));
        }
    }
    default_health_dir()
}

fn db_path_for(dir: &Path) -> PathBuf {
    dir.join("blood.db")
}

fn open_db(dir: &Path) -> Result<Connection> {
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let conn = Connection::open(db_path_for(dir))?;
    init_db(&conn)?;
    Ok(conn)
}

fn init_db(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        PRAGMA foreign_keys = ON;
        CREATE TABLE IF NOT EXISTS imports (
            sha TEXT PRIMARY KEY,
            filename TEXT NOT NULL,
            path TEXT NOT NULL,
            lab TEXT NOT NULL,
            collection_date TEXT NOT NULL,
            size INTEGER NOT NULL,
            text_hash TEXT NOT NULL,
            status TEXT NOT NULL,
            warnings_json TEXT NOT NULL,
            imported_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS marker_rows (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            import_sha TEXT NOT NULL REFERENCES imports(sha) ON DELETE CASCADE,
            key TEXT NOT NULL,
            raw_name TEXT NOT NULL,
            value REAL NOT NULL,
            unit TEXT NOT NULL,
            ref_low REAL,
            ref_high REAL,
            ref_text TEXT,
            UNIQUE(import_sha, key)
        );
        "#,
    )?;
    Ok(())
}

fn marker_defs() -> Vec<MarkerDef> {
    let n = |x: f64| Some(x);
    vec![
        md("chol_total", "Total cholesterol", "mg/dL", PANELS[0], None, n(190.0), Some("< 190"), Down,
          "All cholesterol carried in the blood; a coarse first look at lipid health.",
          Some("Above range and the main thing to keep working on. ApoB and LDL are the sharper targets to watch alongside it."),
          &["colesterol total"]),
        md("ldl", "LDL cholesterol", "mg/dL", PANELS[0], None, n(116.0), Some("< 116"), Down,
          "The cholesterol most directly linked to arterial plaque.",
          Some("Still above the general target. For low overall risk aim under 116 mg/dL; diet, exercise, and ApoB are the sharper context to watch alongside it."),
          &["colesterol da fraccao ldl", "colesterol ldl"]),
        md("hdl", "HDL cholesterol", "mg/dL", PANELS[0], n(40.0), None, Some("> 40"), Up,
          "The protective fraction that helps clear cholesterol.",
          Some("Comfortably protective. Kept high by aerobic exercise and unsaturated fats."),
          &["colesterol da fraccao hdl", "colesterol hdl"]),
        md("trig", "Triglycerides", "mg/dL", PANELS[0], None, n(150.0), Some("< 150"), Down,
          "Circulating fat; sensitive to refined carbs, alcohol and recent meals.",
          Some("Excellent and well under range. A marker of a low-sugar, low-alcohol pattern."),
          &["trigliceridos"]),
        md("apob", "Apolipoprotein B", "mg/dL", PANELS[0], n(46.0), n(174.0), None, Down,
          "One ApoB per atherogenic particle - the truest count of plaque-forming particles.",
          Some("Inside the lab range. Many prevention guidelines aim under 100 mg/dL; trend this alongside LDL."),
          &["apolipoproteina b", "apo b"]),
        md("lpa", "Lipoprotein(a)", "nmol/L", PANELS[0], None, n(75.0), Some("< 75"), Down,
          "A largely genetic, independent cardiovascular risk particle.",
          Some("Mostly set by genetics and barely moves with lifestyle. It adds to risk independently of LDL: a reason to keep the modifiable markers tight."),
          &["lipoproteina a", "lp(a)", "lp a"]),
        md("glucose", "Fasting glucose", "mg/dL", PANELS[1], n(74.0), n(106.0), None, Down,
          "Blood sugar after an overnight fast.", None, &["glucose", "glicemia"]),
        md("hba1c", "HbA1c", "%", PANELS[1], None, n(5.7), Some("< 5.7"), Down,
          "Average blood sugar over about 3 months.",
          Some("Firmly in the healthy range. No action needed."),
          &["hemoglobina a1c", "hba1c"]),
        md("insulin", "Fasting insulin", "µUI/mL", PANELS[1], n(3.0), n(25.0), None, Down,
          "How hard the pancreas works to hold glucose steady.",
          Some("Low-normal readings often reflect high insulin sensitivity when glucose and HbA1c are healthy."),
          &["insulina"]),
        md("homa", "HOMA-IR", "", PANELS[1], n(1.92), n(2.2), Some("1.92 - 2.20"), Down,
          "Insulin-resistance index (lower means more insulin-sensitive).",
          Some("Well below the reference - excellent insulin sensitivity."),
          &["indice de resistencia a insulina", "homa"]),
        md("uric", "Uric acid", "mg/dL", PANELS[1], n(3.5), n(7.2), None, Mid,
          "By-product of purine metabolism; high levels associate with gout.", None, &["acido urico"]),
        md("creatinine", "Creatinine", "mg/dL", PANELS[2], n(0.70), n(1.30), None, Mid,
          "Muscle by-product cleared by the kidneys; a core kidney marker.", None, &["creatinina", "creatininemia"]),
        md("egfr", "eGFR", "mL/min/1.73m²", PANELS[2], n(60.0), None, Some(">= 60"), Up,
          "Estimated kidney filtration rate.",
          Some("Healthy filtration. Values naturally wobble with hydration and recent protein/creatine intake."),
          &["taxa de filtracao glomerular", "tfge", "ckd-epi"]),
        md("urea", "Urea (BUN)", "mg/dL", PANELS[2], n(19.0), n(49.0), None, Down,
          "Nitrogen waste from protein; rises with high protein intake or dehydration.",
          Some("Often reflects a high-protein diet or being under-hydrated at the draw when eGFR is normal."),
          &["ureia"]),
        md("ast", "AST (GOT)", "UI/L", PANELS[3], None, n(34.0), Some("< 34"), Down,
          "Enzyme released by liver (and muscle) cells.",
          Some("Near the upper limit can follow hard exercise in the days before the draw; ALT and GGT give useful context."),
          &["aspartato-aminotransferase", "aspartato aminotransferase", "ast/got", "(ast)"]),
        md("alt", "ALT (GPT)", "UI/L", PANELS[3], None, n(49.0), Some("< 49"), Down,
          "The most liver-specific of the routine enzymes.", None, &["alanina-aminotransferase", "alanina aminotransferase", "alt/gpt", "(alt)"]),
        md("ggt", "GGT", "UI/L", PANELS[3], None, n(73.0), Some("< 73"), Down,
          "Sensitive to alcohol and bile flow.", None, &["gamaglutamil transferase", "ggt"]),
        md("bilirubin", "Total bilirubin", "mg/dL", PANELS[3], None, n(1.20), Some("< 1.20"), Down,
          "Heme breakdown product processed by the liver.", None, &["bilirrubina total"]),
        md("albumin", "Albumin", "g/dL", PANELS[3], n(3.2), n(4.8), None, Up,
          "The main blood protein; a marker of liver synthesis and nutrition.", None, &["albumina", "albuminemia"]),
        md("hemoglobin", "Hemoglobin", "g/dL", PANELS[4], n(13.7), n(17.2), None, Mid,
          "Oxygen-carrying protein in red cells.", None, &["hemoglobina"]),
        md("hematocrit", "Hematocrit", "%", PANELS[4], n(40.0), n(50.0), None, Mid,
          "Fraction of blood volume that is red cells.", None, &["hematocrito"]),
        md("rbc", "Red blood cells", "x10^12/L", PANELS[4], n(4.50), n(5.60), None, Mid,
          "Red cell count.", Some("Dipped just below range on the first draw and has climbed into it since."),
          &["eritrocitos"]),
        md("wbc", "White blood cells", "x10^9/L", PANELS[4], n(3.70), n(9.50), None, Mid,
          "Immune cell count; the body's baseline defence level.", None, &["leucocitos"]),
        md("platelets", "Platelets", "x10^9/L", PANELS[4], n(170.0), n(430.0), None, Mid,
          "Clotting cells.", None, &["plaquetas"]),
        md("rdw", "RDW", "%", PANELS[4], n(11.6), n(14.1), None, Down,
          "Variation in red-cell size; an early flag for some anaemias.", None, &["rdw"]),
        md("vit_d", "Vitamin D (25-OH)", "ng/mL", PANELS[5], n(30.0), n(100.0), None, Up,
          "Vitamin/hormone for bone, immune and muscle function.",
          Some("Track this through winter; the useful target band is usually interpreted against season, sun exposure, and supplementation."),
          &["vitamina d"]),
        md("ferritin", "Ferritin", "µg/L", PANELS[5], n(39.3), n(439.4), None, Mid,
          "Iron stores.", None, &["ferritina"]),
        md("b12", "Vitamin B12", "ng/L", PANELS[5], n(211.0), n(911.0), None, Up,
          "Needed for nerves and red-cell formation.", None, &["vitamina b12"]),
        md("folate", "Folate", "ng/mL", PANELS[5], n(5.4), None, Some("> 5.4"), Up,
          "B-vitamin for DNA synthesis and red cells.", None, &["acido folico", "folatos"]),
        md("magnesium", "Magnesium", "mg/dL", PANELS[5], n(1.6), n(2.6), None, Mid,
          "Cofactor for hundreds of enzymes, including muscle and nerve function.", None, &["magnesio"]),
        md("calcium", "Calcium", "mg/dL", PANELS[5], n(8.7), n(10.4), None, Mid,
          "Tightly regulated mineral for bone, nerves and muscle.", None, &["calcio", "calcemia"]),
        md("tsh", "TSH", "mUI/L", PANELS[6], n(0.55), n(4.78), None, Mid,
          "Pituitary signal that sets thyroid output.", None, &["hormona tirostimulante", "tsh"]),
        md("ft4", "Free T4", "pmol/L", PANELS[6], n(10.3), n(34.7), None, Mid,
          "The circulating thyroid hormone reserve.", None, &["tiroxina livre", "ft4"]),
        md("ft3", "Free T3", "pmol/L", PANELS[6], n(3.5), n(6.5), None, Mid,
          "The active thyroid hormone.", None, &["triiodotironina livre", "ft3"]),
        md("test_total", "Testosterone, total", "ng/dL", PANELS[6], n(197.4), n(669.6), None, Up,
          "Total circulating testosterone (bound + free).",
          Some("Sleep, energy balance, stress, and training load can move this marker substantially."),
          &["testosterona total", "testosterona total plasmatica"]),
        md("test_free", "Testosterone, free", "pg/mL", PANELS[6], n(12.30), n(46.60), None, Up,
          "The biologically active fraction.", Some("Follows the same pattern as total testosterone."),
          &["testosterona livre"]),
        md("dheas", "DHEA-S", "µg/dL", PANELS[6], n(35.0), n(569.0), None, Up,
          "Adrenal androgen and a rough vitality/recovery marker.", None, &["dehidroepiandrosterona sulfato", "dhea"]),
        md("hscrp", "hs-CRP", "mg/dL", PANELS[7], None, n(0.33), Some("<= 0.33"), Down,
          "High-sensitivity marker of systemic inflammation and vascular risk.",
          Some("Very low readings are a good sign for cardiovascular risk; interpret spikes against infections or hard training."),
          &["proteina c reactiva ultra-sensivel", "proteina c reativa ultra-sensivel", "pcr ultra"]),
    ]
}

#[allow(clippy::too_many_arguments)]
fn md(
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
    aliases: &'static [&'static str],
) -> MarkerDef {
    MarkerDef {
        key,
        name,
        unit,
        panel,
        low,
        high,
        ref_text,
        good,
        about,
        advice,
        aliases,
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

fn extract_pdf_text(path: &Path) -> Result<String> {
    let out = Command::new("pdftotext")
        .arg("-layout")
        .arg(path)
        .arg("-")
        .output()
        .with_context(|| "running pdftotext -layout (install poppler if missing)")?;
    if !out.status.success() {
        return Err(anyhow!(
            "pdftotext failed for {}: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn parse_pdf(path: &Path) -> Result<ParsedReport> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let text = extract_pdf_text(path)?;
    let filename = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let lab = detect_lab(&text);
    let collection_date = collection_date(&text)
        .or_else(|| date_from_filename(&filename))
        .unwrap_or_else(|| "unknown".to_string());
    let mut warnings = Vec::new();
    if collection_date == "unknown" {
        warnings.push("collection date not found".to_string());
    }
    let markers = parse_markers(&text, &mut warnings);
    let status = if markers.len() >= MIN_EXPECTED_MARKERS {
        "ok"
    } else {
        "partial"
    }
    .to_string();
    if markers.len() < MIN_EXPECTED_MARKERS {
        warnings.push(format!("only {} known markers extracted", markers.len()));
    }
    Ok(ParsedReport {
        filename,
        path: path.display().to_string(),
        sha: sha256_hex(&bytes),
        size: bytes.len() as u64,
        lab,
        collection_date,
        text_hash: sha256_hex(text.as_bytes()),
        status,
        warnings,
        markers,
    })
}

fn detect_lab(text: &str) -> String {
    let n = normalize(text);
    if n.contains("synlab") {
        "SYNLAB".to_string()
    } else if n.contains("hospital cuf") || n.contains("cuf tejo") {
        "CUF".to_string()
    } else {
        "unknown".to_string()
    }
}

fn collection_date(text: &str) -> Option<String> {
    let syn = Regex::new(r"Data Colheita\s+(\d{2})-(\d{2})-(\d{4})").unwrap();
    if let Some(c) = syn.captures(text) {
        return Some(format!("{}-{}-{}", &c[3], &c[2], &c[1]));
    }
    let cuf = Regex::new(r"Data de inscrição:\s*(\d{2})/(\d{2})/(\d{4})").unwrap();
    if let Some(c) = cuf.captures(text) {
        return Some(format!("{}-{}-{}", &c[3], &c[2], &c[1]));
    }
    None
}

fn date_from_filename(name: &str) -> Option<String> {
    let re = Regex::new(r"(\d{2})-(\d{2})-(\d{4})").unwrap();
    let c = re.captures(name)?;
    Some(format!("{}-{}-{}", &c[3], &c[2], &c[1]))
}

fn parse_markers(text: &str, warnings: &mut Vec<String>) -> Vec<ParsedMarker> {
    let defs = marker_defs();
    let mut out: BTreeMap<String, ParsedMarker> = BTreeMap::new();
    for line in text.lines() {
        let Some((raw_name, cells)) = split_result_line(line) else {
            continue;
        };
        let Some(def) = match_def(&raw_name, &defs) else {
            continue;
        };
        if out.contains_key(def.key) {
            continue;
        }
        if def.key == "hemoglobin" && normalize(&raw_name).contains("a1c") {
            continue;
        }
        match parse_cells(&cells, *def) {
            Some((value, unit, low, high, ref_text)) => {
                out.insert(
                    def.key.to_string(),
                    ParsedMarker {
                        key: def.key.to_string(),
                        raw_name,
                        value,
                        unit,
                        low,
                        high,
                        ref_text,
                    },
                );
            }
            None => warnings.push(format!("could not parse row: {raw_name}")),
        }
    }
    out.into_values().collect()
}

fn split_result_line(line: &str) -> Option<(String, Vec<String>)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with("Mét.:") || line.starts_with("Met.:") {
        return None;
    }
    let splitter = Regex::new(r"\s{2,}").unwrap();
    let parts: Vec<String> = splitter
        .split(line)
        .map(clean_cell)
        .filter(|s| !s.is_empty())
        .collect();
    if parts.len() < 2 {
        return None;
    }
    let name = clean_name(&parts[0]);
    if name.is_empty() || looks_like_header(&name) {
        return None;
    }
    Some((name, parts[1..].to_vec()))
}

fn clean_cell(s: &str) -> String {
    s.trim().trim_matches(':').trim().to_string()
}

fn clean_name(s: &str) -> String {
    s.trim()
        .trim_start_matches("(*)")
        .trim_start_matches("(1)")
        .trim()
        .trim_start_matches('-')
        .trim()
        .trim_end_matches(':')
        .trim()
        .to_string()
}

fn looks_like_header(name: &str) -> bool {
    let n = normalize(name);
    n.contains("impressao")
        || n.contains("pagina")
        || n.contains("data colheita")
        || n.contains("resultado")
        || n.contains("hematologia")
        || n.contains("bioquimica")
        || n.contains("patologia quimica")
        || n.contains("val ref")
}

fn match_def<'a>(name: &str, defs: &'a [MarkerDef]) -> Option<&'a MarkerDef> {
    let n = normalize(name);
    defs.iter()
        .find(|d| d.aliases.iter().any(|a| n.contains(a)))
}

fn parse_cells(
    cells: &[String],
    def: MarkerDef,
) -> Option<(f64, String, Option<f64>, Option<f64>, Option<String>)> {
    let first = cells.first()?.trim();
    if normalize(first).contains("em curso") {
        return None;
    }
    let (value, mut unit, mut next) = parse_value_unit(first)?;
    if unit.is_empty() && cells.get(1).is_some_and(|c| !is_ref_cell(c)) {
        unit = cells[1].trim().to_string();
        next = 2;
    }
    if unit.is_empty() {
        unit = def.unit.to_string();
    }
    let mut low = None;
    let mut high = None;
    let mut ref_text = None;
    for c in cells.iter().skip(next) {
        if let Some((l, h, t)) = parse_ref(c) {
            low = l;
            high = h;
            ref_text = Some(t);
            break;
        }
    }
    if low.is_none() && high.is_none() {
        low = def.low;
        high = def.high;
        ref_text = def.ref_text.map(str::to_string);
    }
    Some((value, unit.trim().to_string(), low, high, ref_text))
}

fn parse_value_unit(s: &str) -> Option<(f64, String, usize)> {
    let re = Regex::new(r"(?i)^\s*(?:[<>]=?\s*)?(\d+(?:[.,]\d+)?)\s*(.*?)\s*$").unwrap();
    let c = re.captures(s)?;
    let value = c[1].replace(',', ".").parse().ok()?;
    let unit = c
        .get(2)
        .map(|m| m.as_str().trim().to_string())
        .unwrap_or_default();
    Some((value, unit, 1))
}

fn is_ref_cell(s: &str) -> bool {
    parse_ref(s).is_some()
}

fn parse_ref(s: &str) -> Option<(Option<f64>, Option<f64>, String)> {
    let s = s.trim();
    let norm = s.replace(',', ".");
    let range = Regex::new(r"(?i)(\d+(?:\.\d+)?)\s*-\s*(\d+(?:\.\d+)?)").unwrap();
    if let Some(c) = range.captures(&norm) {
        let l = c[1].parse().ok()?;
        let h = c[2].parse().ok()?;
        return Some((Some(l), Some(h), format!("{} - {}", trim(l), trim(h))));
    }
    let cmp = Regex::new(r"(?i)(<=|>=|<|>)\s*(\d+(?:\.\d+)?)").unwrap();
    if let Some(c) = cmp.captures(&norm) {
        let op = &c[1];
        let v: f64 = c[2].parse().ok()?;
        return match op {
            "<" | "<=" => Some((None, Some(v), format!("{op} {}", trim(v)))),
            ">" | ">=" => Some((Some(v), None, format!("{op} {}", trim(v)))),
            _ => None,
        };
    }
    None
}

fn normalize(s: &str) -> String {
    let lower = s.to_lowercase();
    let mapped: String = lower
        .chars()
        .map(|c| match c {
            'á' | 'à' | 'ã' | 'â' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'õ' | 'ô' | 'ö' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            other => other,
        })
        .collect();
    mapped.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn store_report(conn: &mut Connection, report: &ParsedReport) -> Result<bool> {
    let exists: Option<String> = conn
        .query_row(
            "SELECT sha FROM imports WHERE sha = ?1",
            params![report.sha],
            |r| r.get(0),
        )
        .optional()?;
    if exists.is_some() {
        return Ok(false);
    }
    let tx = conn.transaction()?;
    let imported_at = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
    tx.execute(
        "INSERT INTO imports
         (sha, filename, path, lab, collection_date, size, text_hash, status, warnings_json, imported_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            report.sha,
            report.filename,
            report.path,
            report.lab,
            report.collection_date,
            report.size as i64,
            report.text_hash,
            report.status,
            serde_json::to_string(&report.warnings)?,
            imported_at,
        ],
    )?;
    for m in &report.markers {
        tx.execute(
            "INSERT INTO marker_rows
             (import_sha, key, raw_name, value, unit, ref_low, ref_high, ref_text)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![report.sha, m.key, m.raw_name, m.value, m.unit, m.low, m.high, m.ref_text,],
        )?;
    }
    tx.commit()?;
    Ok(true)
}

fn read_imports(conn: &Connection) -> Result<Vec<ImportRow>> {
    let mut st = conn.prepare(
        "SELECT sha, filename, path, lab, collection_date, size, text_hash, status, warnings_json
         FROM imports ORDER BY collection_date DESC, filename DESC",
    )?;
    let rows = st
        .query_map([], |r| {
            let warnings_json: String = r.get(8)?;
            let warnings = serde_json::from_str(&warnings_json).unwrap_or_default();
            Ok(ImportRow {
                sha: r.get(0)?,
                filename: r.get(1)?,
                path: r.get(2)?,
                lab: r.get(3)?,
                collection_date: r.get(4)?,
                size: r.get::<_, i64>(5)? as u64,
                text_hash: r.get(6)?,
                status: r.get(7)?,
                warnings,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn read_marker_rows(conn: &Connection) -> Result<Vec<MarkerRow>> {
    let mut st = conn.prepare(
        "SELECT m.import_sha, m.key, m.raw_name, m.value, m.unit, m.ref_low, m.ref_high, m.ref_text, i.collection_date
         FROM marker_rows m
         JOIN imports i ON i.sha = m.import_sha
         ORDER BY i.collection_date ASC, m.key ASC",
    )?;
    let rows = st
        .query_map([], |r| {
            Ok(MarkerRow {
                import_sha: r.get(0)?,
                key: r.get(1)?,
                raw_name: r.get(2)?,
                value: r.get(3)?,
                unit: r.get(4)?,
                low: r.get(5)?,
                high: r.get(6)?,
                ref_text: r.get(7)?,
                date: r.get(8)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn scan_pdf_paths(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_ascii_lowercase();
            if name.starts_with("blood ") && name.ends_with(".pdf") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn import_path_at(dir: &Path, path: &Path) -> Result<Value> {
    let mut conn = open_db(dir)?;
    let report = parse_pdf(path)?;
    let inserted = store_report(&mut conn, &report)?;
    Ok(json!({
        "ok": true,
        "imported": inserted,
        "duplicate": !inserted,
        "file": report.filename,
        "date": report.collection_date,
        "lab": report.lab,
        "sha": report.sha,
        "markers": report.markers.len(),
        "status": report.status,
        "warnings": report.warnings,
    }))
}

fn import_dir_at(dir: &Path) -> Result<Value> {
    let paths = scan_pdf_paths(dir);
    let mut imported = 0usize;
    let mut duplicates = 0usize;
    let mut failed = 0usize;
    let mut rows = Vec::new();
    for p in paths {
        match import_path_at(dir, &p) {
            Ok(v) => {
                if v["imported"].as_bool().unwrap_or(false) {
                    imported += 1;
                } else {
                    duplicates += 1;
                }
                rows.push(v);
            }
            Err(e) => {
                failed += 1;
                rows.push(json!({
                    "ok": false,
                    "file": p.file_name().unwrap_or_default().to_string_lossy(),
                    "message": e.to_string(),
                }));
            }
        }
    }
    Ok(json!({
        "ok": failed == 0,
        "dir": dir.display().to_string(),
        "scanned": rows.len(),
        "imported": imported,
        "duplicates": duplicates,
        "failed": failed,
        "imports": rows,
    }))
}

/// `POST /api/blood/import-dir` - scan the configured directory for `blood *.pdf`.
pub fn import_dir() -> Value {
    let Some(dir) = blood_dir() else {
        return json!({ "ok": false, "message": "no blood directory configured; pass --blood-files DIR" });
    };
    match import_dir_at(&dir) {
        Ok(v) => v,
        Err(e) => json!({ "ok": false, "message": e.to_string() }),
    }
}

/// `POST /api/blood/import {"path": "/path/to/report.pdf"}`.
pub fn import_path(path: &str) -> Value {
    let Some(dir) = blood_dir() else {
        return json!({ "ok": false, "message": "no blood directory configured; pass --blood-files DIR" });
    };
    let path = expand_tilde(PathBuf::from(path));
    if path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.eq_ignore_ascii_case("pdf"))
        != Some(true)
    {
        return json!({ "ok": false, "message": "expected a PDF path" });
    }
    match import_path_at(&dir, &path) {
        Ok(v) => v,
        Err(e) => json!({ "ok": false, "message": e.to_string() }),
    }
}

/// `GET /api/blood/report` - computed panel from imported PDF rows.
pub fn report() -> Value {
    let Some(dir) = blood_dir() else {
        return empty_report("no blood directory configured; pass --blood-files DIR");
    };
    match report_from_dir(&dir) {
        Ok(v) => v,
        Err(e) => json!({ "error": e.to_string() }),
    }
}

fn report_from_dir(dir: &Path) -> Result<Value> {
    let conn = open_db(dir)?;
    if read_imports(&conn)?.is_empty() {
        drop(conn);
        let _ = import_dir_at(dir);
    }
    let conn = open_db(dir)?;
    let imports = read_imports(&conn)?;
    let rows = read_marker_rows(&conn)?;
    Ok(build_report_json(&imports, &rows, &db_path_for(dir)))
}

fn empty_report(message: &str) -> Value {
    json!({
        "mocked": false,
        "source": "pdf",
        "panels": PANELS,
        "markers": [],
        "imports": [],
        "draws": [],
        "summary": {
            "markers_total": 0,
            "flagged": 0,
            "imports": 0,
            "first_date": null,
            "latest_date": null,
        },
        "warnings": [message],
    })
}

fn build_report_json(imports: &[ImportRow], rows: &[MarkerRow], db_path: &Path) -> Value {
    let defs = marker_defs();
    let mut by_key: HashMap<String, Vec<&MarkerRow>> = HashMap::new();
    for r in rows {
        by_key.entry(r.key.clone()).or_default().push(r);
    }
    let markers_json: Vec<Value> = defs
        .iter()
        .filter_map(|d| by_key.get(d.key).map(|points| marker_json(d, points)))
        .collect();
    let flagged = markers_json
        .iter()
        .filter(|m| m["flagged"].as_bool().unwrap_or(false))
        .count();
    let mut draws: Vec<String> = imports.iter().map(|i| i.collection_date.clone()).collect();
    draws.sort();
    draws.dedup();
    let imports_json: Vec<Value> = imports
        .iter()
        .map(|i| {
            let markers = rows.iter().filter(|r| r.import_sha == i.sha).count();
            json!({
                "file": i.filename,
                "path": i.path,
                "date": i.collection_date,
                "lab": i.lab,
                "size": i.size,
                "sha": &i.sha[..12.min(i.sha.len())],
                "text_hash": &i.text_hash[..12.min(i.text_hash.len())],
                "markers": markers,
                "status": i.status,
                "warnings": i.warnings,
                "source": "pdf",
            })
        })
        .collect();
    json!({
        "mocked": false,
        "source": "pdf",
        "db": db_path.display().to_string(),
        "panels": PANELS,
        "markers": markers_json,
        "imports": imports_json,
        "draws": draws,
        "summary": {
            "markers_total": markers_json.len(),
            "flagged": flagged,
            "imports": imports.len(),
            "first_date": imports.iter().map(|i| i.collection_date.as_str()).min(),
            "latest_date": imports.iter().map(|i| i.collection_date.as_str()).max(),
        },
    })
}

fn round(v: f64, dp: i32) -> f64 {
    let f = 10f64.powi(dp);
    (v * f).round() / f
}

fn ref_text(def: &MarkerDef, latest: Option<&MarkerRow>) -> String {
    if let Some(t) = latest.and_then(|r| r.ref_text.clone()) {
        return t;
    }
    if let Some(t) = def.ref_text {
        return t.to_string();
    }
    match (
        latest.and_then(|r| r.low).or(def.low),
        latest.and_then(|r| r.high).or(def.high),
    ) {
        (Some(l), Some(h)) => format!("{} - {}", trim(l), trim(h)),
        (Some(l), None) => format!("> {}", trim(l)),
        (None, Some(h)) => format!("< {}", trim(h)),
        (None, None) => "-".into(),
    }
}

fn trim(v: f64) -> String {
    if (v.fract()).abs() < 1e-9 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

fn marker_json(def: &MarkerDef, points_in: &[&MarkerRow]) -> Value {
    let mut points = points_in.to_vec();
    points.sort_by(|a, b| a.date.cmp(&b.date));
    let latest_row = *points.last().expect("marker has at least one row");
    let latest = latest_row.value;
    let prev = if points.len() >= 2 {
        Some(points[points.len() - 2].value)
    } else {
        None
    };
    let low = latest_row.low.or(def.low);
    let high = latest_row.high.or(def.high);

    let out_low = low.map(|l| latest < l).unwrap_or(false);
    let out_high = high.map(|h| latest > h).unwrap_or(false);
    let status = if out_high {
        "high"
    } else if out_low {
        "low"
    } else {
        "normal"
    };
    let bad = match def.good {
        Up => out_low,
        Down => out_high,
        Mid => out_low || out_high,
    };
    let severity = if bad {
        "watch"
    } else if out_low || out_high {
        "good"
    } else {
        "neutral"
    };
    let (trend, trend_good, delta, delta_pct) = match prev {
        Some(p) => {
            let d = latest - p;
            let eps = (latest.abs() * 0.005).max(1e-9);
            let dir = if d.abs() <= eps {
                "flat"
            } else if d > 0.0 {
                "up"
            } else {
                "down"
            };
            let good = if dir == "flat" {
                true
            } else {
                match def.good {
                    Up => d > 0.0,
                    Down => d < 0.0,
                    Mid => match (low, high) {
                        (Some(l), Some(h)) => {
                            let c = (l + h) / 2.0;
                            (latest - c).abs() <= (p - c).abs()
                        }
                        _ => true,
                    },
                }
            };
            (
                dir,
                good,
                Some(round(d, 3)),
                if p != 0.0 {
                    Some(round(d / p * 100.0, 1))
                } else {
                    None
                },
            )
        }
        None => ("flat", true, None, None),
    };
    let pts: Vec<Value> = points
        .iter()
        .map(|r| json!({ "date": r.date, "value": round(r.value, 3) }))
        .collect();

    json!({
        "key": def.key,
        "name": def.name,
        "raw_name": latest_row.raw_name,
        "unit": if latest_row.unit.is_empty() { def.unit } else { latest_row.unit.as_str() },
        "panel": def.panel,
        "about": def.about,
        "advice": def.advice,
        "low": low,
        "high": high,
        "ref_text": ref_text(def, Some(latest_row)),
        "points": pts,
        "latest": round(latest, 3),
        "latest_date": latest_row.date,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "open_oura_blood_test_{name}_{}_{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn parses_synlab_rows() {
        let text = r#"
Data Colheita                   09-06-2026 09:13
      Glucose                                                                          87 mg/dL                        74 - 106               79             76
(*) Taxa de filtração glomerular, estimada (TFGe)                                      79 mL/min/1,73m2                  >= 60                87             94
      Colesterol total                                                                235 mg/dL                               < 190                 229           278
      Colesterol da fracção HDL                                                         62 mg/dL                              > 40                  65            76
(*) Colesterol da fracção LDL                                                         158 mg/dL                               < 116                 154           185
      Creatinina                                                                      1.26 mg/dL                     0.70 - 1.30             1.17           1.10
"#;
        let mut warnings = Vec::new();
        let rows = parse_markers(text, &mut warnings);
        let by_key: HashMap<_, _> = rows.iter().map(|r| (r.key.as_str(), r.value)).collect();
        assert_eq!(by_key["chol_total"], 235.0);
        assert_eq!(by_key["ldl"], 158.0);
        assert_eq!(by_key["hdl"], 62.0);
        assert_eq!(by_key["glucose"], 87.0);
        assert_eq!(by_key["creatinine"], 1.26);
        assert_eq!(by_key["egfr"], 79.0);
    }

    #[test]
    fn parses_cuf_rows() {
        let text = r#"
Data de inscrição: 09/09/2025   09:16:41
 Glicémia                             88     mg/dL                      70 - 110
 Colesterol Total                  248       mg/dL                         < 190
 Colesterol LDL                    166       mg/dL                         < 115
 Colesterol HDL                       64     mg/dL                       35 - 55
 Creatininémia                          1.02 mg/dL                      0.70 - 1.30
  TFGe [CKD-EPI 2009]                 103    ml/min/1,73 m 2                     60
 Testosterona Total Plasmática (TT)       144.7     ng/dl                 241.0 - 827.0
"#;
        assert_eq!(collection_date(text).as_deref(), Some("2025-09-09"));
        let mut warnings = Vec::new();
        let rows = parse_markers(text, &mut warnings);
        let by_key: HashMap<_, _> = rows.iter().map(|r| (r.key.as_str(), r.value)).collect();
        assert_eq!(by_key["glucose"], 88.0);
        assert_eq!(by_key["ldl"], 166.0);
        assert_eq!(by_key["egfr"], 103.0);
        assert_eq!(by_key["test_total"], 144.7);
    }

    #[test]
    fn normalizes_aliases() {
        let defs = marker_defs();
        assert_eq!(match_def("Ácido úrico", &defs).unwrap().key, "uric");
        assert_eq!(
            match_def("Colesterol da fracção LDL", &defs).unwrap().key,
            "ldl"
        );
        assert_eq!(match_def("Creatininémia", &defs).unwrap().key, "creatinine");
    }

    #[test]
    fn parses_reference_ranges() {
        assert_eq!(
            parse_ref("< 190").unwrap(),
            (None, Some(190.0), "< 190".into())
        );
        assert_eq!(
            parse_ref("> 40").unwrap(),
            (Some(40.0), None, "> 40".into())
        );
        assert_eq!(
            parse_ref("<= 0.33").unwrap(),
            (None, Some(0.33), "<= 0.33".into())
        );
        assert_eq!(
            parse_ref(">= 60").unwrap(),
            (Some(60.0), None, ">= 60".into())
        );
        assert_eq!(
            parse_ref("0.70 - 1.30").unwrap(),
            (Some(0.70), Some(1.30), "0.7 - 1.3".into())
        );
    }

    #[test]
    fn dedupes_by_file_hash() {
        let dir = temp_dir("dedupe");
        let mut conn = open_db(&dir).unwrap();
        let report = ParsedReport {
            filename: "blood fake.pdf".into(),
            path: "/tmp/blood fake.pdf".into(),
            sha: "abc".into(),
            size: 10,
            lab: "SYNLAB".into(),
            collection_date: "2026-06-09".into(),
            text_hash: "def".into(),
            status: "ok".into(),
            warnings: vec![],
            markers: vec![ParsedMarker {
                key: "glucose".into(),
                raw_name: "Glucose".into(),
                value: 87.0,
                unit: "mg/dL".into(),
                low: Some(74.0),
                high: Some(106.0),
                ref_text: Some("74 - 106".into()),
            }],
        };
        assert!(store_report(&mut conn, &report).unwrap());
        assert!(!store_report(&mut conn, &report).unwrap());
        assert_eq!(read_imports(&conn).unwrap().len(), 1);
        assert_eq!(read_marker_rows(&conn).unwrap().len(), 1);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn report_shape_matches_frontend() {
        let imports = vec![ImportRow {
            sha: "abcdef123456".into(),
            filename: "blood fake.pdf".into(),
            path: "/tmp/blood fake.pdf".into(),
            lab: "SYNLAB".into(),
            collection_date: "2026-06-09".into(),
            size: 100,
            text_hash: "feedbeef1234".into(),
            status: "ok".into(),
            warnings: vec![],
        }];
        let rows = vec![MarkerRow {
            import_sha: "abcdef123456".into(),
            key: "glucose".into(),
            raw_name: "Glucose".into(),
            value: 87.0,
            unit: "mg/dL".into(),
            low: Some(74.0),
            high: Some(106.0),
            ref_text: Some("74 - 106".into()),
            date: "2026-06-09".into(),
        }];
        let v = build_report_json(&imports, &rows, Path::new("/tmp/blood.db"));
        assert_eq!(v["mocked"], false);
        assert_eq!(v["source"], "pdf");
        assert!(v["summary"].get("markers_total").is_some());
        assert!(v["imports"].as_array().unwrap()[0]
            .get("warnings")
            .is_some());
        assert!(v["markers"].as_array().unwrap()[0].get("points").is_some());
    }

    #[test]
    fn real_june_pdf_matches_golden_when_present() {
        let path = PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
            .join("Documents/official/health/blood 09-06-2026.pdf");
        if !path.exists() {
            return;
        }
        let expected: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/blood_june_2026_key_rows.json"
        ))
        .unwrap();
        let report = parse_pdf(&path).unwrap();
        assert_eq!(
            report.collection_date,
            expected["collection_date"].as_str().unwrap()
        );
        let by_key: HashMap<_, _> = report
            .markers
            .iter()
            .map(|r| (r.key.as_str(), r.value))
            .collect();
        for (key, value) in expected["rows"].as_object().unwrap() {
            assert_eq!(by_key[key.as_str()], value.as_f64().unwrap());
        }
    }
}
