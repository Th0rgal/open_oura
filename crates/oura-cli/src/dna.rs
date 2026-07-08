//! Backend for the `/dna` explorer page.
//!
//! Parsing + scoring live in the `oura-dna` crate; this module is the HTTP glue:
//! it locates `dna/` in the repo tree, lists the available **genomes**
//! (`dna/files/*.vcf.gz`) and **scores** (built-in catalog scores + PGS Catalog
//! files in `dna/scores/`), builds the report for a chosen genome + selected
//! scores (memoised), and fetches PGS Catalog scoring files on demand.
//!
//! Privacy: the genome never leaves this machine. The only outbound request is
//! [`fetch`], which downloads a **public** PGS Catalog score *definition* from EBI
//! on explicit user action — no personal data is ever sent.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

use oura_dna::{build_report, pgs, Catalog, Genotype, ScoreSpec, TraitDef, VcfSource};

/// The repo `dna/` directory — holds `catalog.json` and the fetched PGS
/// `scores/`. `None` when not running from within the repo tree.
fn dna_dir() -> Option<PathBuf> {
    crate::pyrunner::repo_root(Path::new("tools/run_activity_model.py")).map(|r| r.join("dna"))
}

fn catalog_path(dir: &Path) -> PathBuf {
    dir.join("catalog.json")
}
fn scores_dir(dir: &Path) -> PathBuf {
    dir.join("scores")
}

/// Where genome `*.vcf.gz` files are read from. Set once at dashboard startup
/// from `--dna-files`; overridable by `$OURA_DNA_FILES`; defaults to the repo's
/// `dna/files/`. Keeping genomes outside the repo (they're large and private) is
/// the common case.
static GENOMES_DIR: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Called from `dashboard::serve` with the `--dna-files` value (if any).
pub fn set_genomes_dir(dir: Option<PathBuf>) {
    let _ = GENOMES_DIR.set(dir.map(expand_tilde));
}

/// Expand a leading `~` to `$HOME` (shells don't expand it inside quotes).
fn expand_tilde(p: PathBuf) -> PathBuf {
    if let Ok(rest) = p.strip_prefix("~") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    p
}

/// The resolved genomes directory, honouring the flag, then the env var, then the
/// repo default.
fn genomes_dir() -> Option<PathBuf> {
    if let Some(Some(d)) = GENOMES_DIR.get() {
        return Some(d.clone());
    }
    if let Some(env) = std::env::var_os("OURA_DNA_FILES") {
        if !env.is_empty() {
            return Some(expand_tilde(PathBuf::from(env)));
        }
    }
    dna_dir().map(|d| d.join("files"))
}

/// Accept only a bare basename with an expected extension — no separators, no
/// `..`, no leading dot — so a name can never escape its directory.
fn safe_name(name: &str, exts: &[&str]) -> Option<String> {
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || name.starts_with('.')
    {
        return None;
    }
    exts.iter()
        .any(|e| name.ends_with(e))
        .then(|| name.to_string())
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

// ── listing ──────────────────────────────────────────────────────────────────

/// `GET /api/dna/files` — genomes to explore and scores available to apply.
pub fn list_files() -> Value {
    let Some(dir) = dna_dir() else {
        return json!({ "genomes": [], "scores": [], "error": "DNA explorer must run from the repo (dna/ not found)" });
    };
    let gdir = genomes_dir();

    // genomes — from the configured directory (may be outside the repo). Each is
    // classified so the UI can steer toward the SNP/indel file (the only kind with
    // the point genotypes traits + PGS need) and away from CNV/SV callsets.
    let mut genomes: Vec<(bool, String, Value)> = Vec::new();
    if let Some(gdir) = &gdir {
        if let Ok(rd) = std::fs::read_dir(gdir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if !name.ends_with(".vcf.gz") {
                    continue;
                }
                let size = e.metadata().map(|m| m.len()).unwrap_or(0);
                let (kind, scoreable) = classify_genome(&name);
                genomes.push((
                    scoreable,
                    name.clone(),
                    json!({ "name": name, "size": size, "kind": kind, "scoreable": scoreable }),
                ));
            }
        }
    }
    // scoreable files first, then alphabetical
    genomes.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));

    // scores: built-in (from catalog) + PGS files
    let mut scores: Vec<Value> = Vec::new();
    if let Ok(cat) = Catalog::load(&catalog_path(&dir)) {
        for s in cat.builtin_scores() {
            scores.push(json!({
                "key": format!("builtin:{}", s.id),
                "source": "builtin",
                "id": s.id,
                "name": s.name,
                "category": s.category,
                "variants": s.variants.len(),
                "trait": null,
                "build": s.genome_build,
                "weight_type": s.weight_type,
            }));
        }
    }
    let mut pgs_rows: Vec<(String, Value)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(scores_dir(&dir)) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !(name.ends_with(".txt") || name.ends_with(".txt.gz")) {
                continue;
            }
            let path = e.path();
            let meta = pgs::read_meta(&path).ok();
            pgs_rows.push((
                name.clone(),
                json!({
                    "key": format!("pgs:{}", name),
                    "source": "pgs",
                    "file": name,
                    "id": meta.as_ref().and_then(|m| m.id.clone()),
                    "name": meta.as_ref().and_then(|m| m.trait_reported.clone().or_else(|| m.name.clone())),
                    "trait": meta.as_ref().and_then(|m| m.trait_reported.clone()),
                    "variants": meta.as_ref().and_then(|m| m.variants_number),
                    "build": meta.as_ref().and_then(|m| m.genome_build.clone()),
                    "weight_type": meta.as_ref().and_then(|m| m.weight_type.clone()),
                }),
            ));
        }
    }
    pgs_rows.sort_by(|a, b| a.0.cmp(&b.0));
    scores.extend(pgs_rows.into_iter().map(|(_, v)| v));

    let genomes: Vec<Value> = genomes.into_iter().map(|(_, _, v)| v).collect();
    json!({
        "genomes": genomes,
        "scores": scores,
        "catalog_ok": catalog_path(&dir).exists(),
        "genomes_dir": gdir.as_ref().map(|d| d.display().to_string()),
    })
}

/// Classify a genome file by its name. Only SNP/indel callsets carry the point
/// genotypes that traits + PGS scoring need; copy-number (`.cnv`) and structural
/// (`.sv`) callsets don't, so the UI can steer away from them. Returns
/// `(kind, scoreable)`.
fn classify_genome(name: &str) -> (&'static str, bool) {
    let n = name.to_ascii_lowercase();
    if n.contains(".cnv.") {
        ("cnv", false)
    } else if n.contains(".sv.") {
        ("sv", false)
    } else if n.contains("snp") || n.contains("indel") || n.contains("genome") {
        ("snp-indel", true)
    } else {
        // an ordinary VCF (23andMe export, imputed panel, …) — assume scoreable
        ("vcf", true)
    }
}

// ── report (genome + selected scores) ────────────────────────────────────────
// Keyed by genome name + the selected score keys; token folds in the mtimes of
// the genome, the catalog, and every selected PGS file so any edit rebuilds.

struct Cached {
    token: Vec<Option<SystemTime>>,
    value: Arc<Value>,
}

fn cache() -> &'static Mutex<HashMap<String, Cached>> {
    static C: OnceLock<Mutex<HashMap<String, Cached>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// `GET /api/dna/report?file=NAME&scores=builtin:x,pgs:y.txt.gz`
///
/// With no `scores`, applies every built-in score (cheap) and no PGS files.
pub fn report(file: &str, scores_param: &str) -> Value {
    let (vcf, name) = match resolve_genome(file) {
        Ok(v) => v,
        Err(e) => return json!({ "error": e.to_string() }),
    };
    let Some(dir) = dna_dir() else {
        return json!({ "error": "dna/ not found (catalog + scores live in the repo)" });
    };
    let selected: Vec<String> = scores_param
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let cat_path = catalog_path(&dir);

    // cache key + token
    let mut keys_sorted = selected.clone();
    keys_sorted.sort();
    let cache_key = format!("{name}|{}", keys_sorted.join(","));
    let mut token = vec![mtime(&vcf), mtime(&cat_path)];
    for k in &keys_sorted {
        if let Some(fname) = k.strip_prefix("pgs:") {
            token.push(mtime(&scores_dir(&dir).join(fname)));
        }
    }
    if let Some(c) = cache().lock().unwrap().get(&cache_key) {
        if c.token == token {
            return (*c.value).clone();
        }
    }

    let value = match build(&dir, &vcf, &cat_path, &name, &selected) {
        Ok(v) => v,
        Err(e) => return json!({ "error": e.to_string() }),
    };
    let arc = Arc::new(value);
    cache().lock().unwrap().insert(
        cache_key,
        Cached {
            token,
            value: arc.clone(),
        },
    );
    (*arc).clone()
}

fn build(dir: &Path, vcf: &Path, cat_path: &Path, name: &str, selected: &[String]) -> Result<Value> {
    let catalog = Catalog::load(cat_path)?;
    let specs = resolve_scores(dir, &catalog, selected)?;
    let src = VcfSource::open(vcf);
    let rep = build_report(&src, &catalog, &specs)?;
    let mut v = serde_json::to_value(&rep)?;
    if let Value::Object(ref mut m) = v {
        m.insert("file".into(), json!(name));
        m.insert("applied_scores".into(), json!(specs.len()));
    }
    Ok(v)
}

/// Turn selected score keys into concrete [`ScoreSpec`]s. No selection ⇒ all
/// built-in scores.
fn resolve_scores(dir: &Path, catalog: &Catalog, selected: &[String]) -> Result<Vec<ScoreSpec>> {
    let builtins = catalog.builtin_scores();
    if selected.is_empty() {
        return Ok(builtins);
    }
    let mut out = Vec::new();
    for key in selected {
        if let Some(id) = key.strip_prefix("builtin:") {
            if let Some(s) = builtins.iter().find(|s| s.id == id) {
                out.push(s.clone());
            }
        } else if let Some(fname) = key.strip_prefix("pgs:") {
            let fname = safe_name(fname, &[".txt", ".txt.gz"])
                .ok_or_else(|| anyhow!("invalid score file"))?;
            let path = scores_dir(dir).join(&fname);
            if !path.exists() {
                return Err(anyhow!("{fname} not found in dna/scores"));
            }
            out.push(pgs::load(&path)?);
        }
    }
    Ok(out)
}

// ── single-variant lookup ────────────────────────────────────────────────────

/// `GET /api/dna/lookup?file=NAME&q=rs123|chr2:135851076`
pub fn lookup(file: &str, q: &str) -> Value {
    let (vcf, _name) = match resolve_genome(file) {
        Ok(v) => v,
        Err(e) => return json!({ "error": e.to_string() }),
    };
    let dir = dna_dir();
    let src = VcfSource::open(vcf);
    match src.lookup(q) {
        Ok(Some(g)) => {
            let annotation = dir
                .as_deref()
                .map(|d| catalog_annotation(d, q, &g))
                .unwrap_or(Value::Null);
            let variant = serde_json::to_value(&g).unwrap_or(Value::Null);
            json!({ "found": true, "variant": variant, "annotation": annotation })
        }
        Ok(None) => json!({ "found": false }),
        Err(e) => json!({ "error": e.to_string() }),
    }
}

fn catalog_annotation(dir: &Path, query: &str, g: &Genotype) -> Value {
    let Ok(catalog) = Catalog::load(&catalog_path(dir)) else {
        return Value::Null;
    };
    let q = query.trim().to_ascii_lowercase();
    let matches = |t: &TraitDef| {
        t.rsid.eq_ignore_ascii_case(&q)
            || g.rsid.split(';').any(|id| id.trim().eq_ignore_ascii_case(&t.rsid))
    };
    let Some(t) = catalog.traits.iter().find(|t| matches(t)) else {
        return Value::Null;
    };
    let interp = if g.no_call {
        None
    } else {
        t.genotypes.get(&g.key)
    };
    json!({
        "trait": t.name,
        "category": t.category,
        "note": t.note,
        "source_url": t.source_url,
        "interp": interp,
    })
}

// ── PGS Catalog fetch ────────────────────────────────────────────────────────

/// Official EBI mirror of the harmonized (GRCh38) scoring files.
const PGS_BASE: &str = "https://ftp.ebi.ac.uk/pub/databases/spot/pgs/scores";
/// Refuse absurdly large downloads (a safety cap; real files are far smaller).
const MAX_SCORE_BYTES: u64 = 1_500_000_000;

fn valid_pgs_id(id: &str) -> Option<String> {
    let id = id.trim().to_ascii_uppercase();
    let ok = id.starts_with("PGS")
        && id.len() >= 9
        && id.len() <= 12
        && id[3..].bytes().all(|b| b.is_ascii_digit());
    ok.then_some(id)
}

/// `POST /api/dna/fetch {pgs_id}` — download a public harmonized GRCh38 scoring
/// file from EBI into `dna/scores/`. Only public score definitions are fetched.
pub fn fetch(pgs_id: &str) -> Value {
    let Some(dir) = dna_dir() else {
        return json!({ "ok": false, "message": "DNA explorer must run from the repo (dna/ not found)" });
    };
    let Some(id) = valid_pgs_id(pgs_id) else {
        return json!({ "ok": false, "message": "expected a PGS Catalog ID like PGS000018" });
    };
    let fname = format!("{id}_hmPOS_GRCh38.txt.gz");
    let url = format!("{PGS_BASE}/{id}/ScoringFiles/Harmonized/{fname}");
    let dest = scores_dir(&dir).join(&fname);

    match download(&url, &dest) {
        Ok(bytes) => {
            let meta = pgs::read_meta(&dest).ok();
            json!({
                "ok": true,
                "file": fname,
                "bytes": bytes,
                "meta": meta,
                "message": format!("Fetched {id} ({} KB)", bytes / 1024),
            })
        }
        Err(e) => {
            let _ = std::fs::remove_file(&dest); // don't leave a partial file
            json!({ "ok": false, "message": format!("Couldn't fetch {id}: {e}") })
        }
    }
}

/// Stream `url` to `dest` (atomically via a temp file), enforcing the size cap.
fn download(url: &str, dest: &Path) -> Result<u64> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(15))
        .timeout_read(std::time::Duration::from_secs(120))
        .build();
    let resp = agent.get(url).call().map_err(|e| match e {
        ureq::Error::Status(404, _) => anyhow!("not found on PGS Catalog (check the ID)"),
        ureq::Error::Status(code, _) => anyhow!("server returned HTTP {code}"),
        other => anyhow!(other.to_string()),
    })?;

    let tmp = dest.with_extension("part");
    let mut out = std::fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
    let mut reader = resp.into_reader().take(MAX_SCORE_BYTES + 1);
    let written = std::io::copy(&mut reader, &mut out).context("downloading scoring file")?;
    if written > MAX_SCORE_BYTES {
        let _ = std::fs::remove_file(&tmp);
        return Err(anyhow!("file exceeds the {} MB cap", MAX_SCORE_BYTES / 1_000_000));
    }
    drop(out);
    std::fs::rename(&tmp, dest).with_context(|| format!("saving {}", dest.display()))?;
    Ok(written)
}

// ── shared ───────────────────────────────────────────────────────────────────

/// Resolve a genome `file` to its full path inside the configured genomes
/// directory. Returns `(vcf_path, basename)`.
fn resolve_genome(file: &str) -> Result<(PathBuf, String)> {
    let gdir = genomes_dir().context("no genomes directory configured")?;
    let name = safe_name(file, &[".vcf.gz"]).ok_or_else(|| anyhow!("invalid file name"))?;
    let vcf = gdir.join(&name);
    if !vcf.exists() {
        return Err(anyhow!("{name} not found in {}", gdir.display()));
    }
    Ok((vcf, name))
}
