//! Read a VCF (`.vcf.gz`) genome and score it against a local, editable catalog of
//! single-SNP **traits** and against **polygenic scores** — both the illustrative
//! built-ins in `catalog.json` and real [PGS Catalog](https://www.pgscatalog.org/)
//! scoring files. Everything is computed on this machine; a genome never leaves it.
//!
//! Modules:
//! - [`vcf`] — streaming `.vcf.gz` reader + genotype resolution.
//! - [`catalog`] — the curated trait + built-in-score catalog.
//! - [`pgs`] — PGS Catalog scoring-file parser.
//! - [`score`] — the strict, streaming polygenic-score engine.
//!
//! [`build_report`] combines a trait catalog and a set of [`score::ScoreSpec`]s
//! into a single genome pass: it builds one locus index (rsID + chrom:pos) over
//! every trait and score variant, streams the file once, and dispatches each
//! matching record to the trait table or the relevant score accumulator. Peak
//! memory scales with the *catalog* (trait genotypes + bounded score state), not
//! the genome — so a million-variant PGS is one pass, not a million-entry map.

pub mod catalog;
pub mod pgs;
pub mod score;
pub mod util;
pub mod vcf;

use std::collections::{HashMap, HashSet};

use anyhow::{anyhow, Result};
use serde::Serialize;

pub use catalog::{Catalog, GenoInterp, ScoreDef, TraitDef};
pub use pgs::PgsMeta;
pub use score::{Band, Contributor, ScoreResult, ScoreSpec, ScoreVariant};
pub use vcf::{Genotype, VcfSource};

use score::ScoreAccumulator;
use util::{norm_chrom, pos_key};

// ── report types ─────────────────────────────────────────────────────────────

/// The full result rendered by the `/dna` page.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub build: String,
    pub traits: Vec<TraitResult>,
    pub scores: Vec<ScoreResult>,
    /// Trait loci the file had no record for.
    pub traits_missing: usize,
    pub traits_total: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct TraitResult {
    pub id: String,
    pub name: String,
    pub category: String,
    pub rsid: String,
    pub note: Option<String>,
    pub source_url: Option<String>,
    pub genotype: String,
    pub found: bool,
    pub no_call: bool,
    pub interp: Option<GenoInterp>,
    pub chrom: String,
    pub pos: u64,
}

// ── single-pass report builder ───────────────────────────────────────────────

/// Which catalog entry a matched record feeds.
#[derive(Clone, Copy)]
enum Target {
    Trait(usize),
    Score { score: usize, variant: usize },
}

/// How the covering record describes the sample at a locus.
#[derive(Clone, Copy)]
enum RecMode {
    /// A real variant record → use its called genotype.
    Variant,
    /// A homozygous-reference gVCF block → the sample matches the reference here.
    HomRef,
    /// An explicit no-call (`./.`) block → no data; excluded from scoring.
    NoCall,
}

/// Register one catalog locus into the rsID index, the exact chrom:pos index, and
/// the per-chromosome sorted list used for reference-block range resolution.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn index_target(
    by_rsid: &mut HashMap<String, Vec<Target>>,
    by_pos: &mut HashMap<String, Vec<Target>>,
    chrom_tmp: &mut HashMap<String, Vec<(u64, Target)>>,
    pos_set: &mut HashSet<u64>,
    rsid: &str,
    chrom: &str,
    pos: u64,
    t: Target,
) {
    let rs = rsid.trim().to_ascii_lowercase();
    if rs.starts_with("rs") {
        by_rsid.entry(rs).or_default().push(t);
    }
    if pos > 0 && !chrom.is_empty() {
        by_pos.entry(pos_key(chrom, pos)).or_default().push(t);
        chrom_tmp.entry(norm_chrom(chrom)).or_default().push((pos, t));
        pos_set.insert(pos);
    }
}

/// Look up a trait interpretation for an observed genotype, tolerant of strand.
///
/// A VCF is always on the reference-forward strand, but catalog trait alleles are
/// often written in the classic/dbSNP orientation (e.g. `rs4988235` as C/T while
/// the GRCh38 forward strand is A/G). So if the observed genotype key isn't in the
/// table, we retry its reverse-complement — but only for **non-palindromic** SNPs,
/// where the flip is unambiguous. Palindromic (A/T, C/G) traits are matched
/// directly only, never guessed.
/// Returns the genotype to *display* (flipped to the catalog's strand when a
/// reverse-complement match was needed, so all genotypes for a SNP read on one
/// strand) and its interpretation, if any.
fn interpret_trait(t: &TraitDef, genotype: String, no_call: bool) -> (String, Option<GenoInterp>) {
    if no_call || genotype.is_empty() {
        return (genotype, None);
    }
    if let Some(x) = t.genotypes.get(&genotype) {
        return (genotype, Some(x.clone()));
    }
    if !util::is_palindromic(&t.ref_allele, &t.alt) {
        let rc = util::rc_genotype(&genotype);
        if let Some(x) = t.genotypes.get(&rc) {
            return (rc, Some(x.clone()));
        }
    }
    (genotype, None)
}

/// Record the genotype (or hom-ref / no-call state) for one target, once.
#[allow(clippy::too_many_arguments)]
fn resolve_target(
    trait_geno: &mut [Option<Genotype>],
    accs: &mut [ScoreAccumulator],
    consumed: &mut [Vec<bool>],
    remaining: &mut usize,
    catalog: &Catalog,
    t: Target,
    tpos: u64,
    chrom: &str,
    mode: RecMode,
    homref_ref: Option<&str>,
    real: Option<&Genotype>,
) {
    match t {
        Target::Trait(i) => {
            if trait_geno[i].is_none() {
                let g = match mode {
                    RecMode::Variant => real.cloned().unwrap_or_else(|| vcf::no_call(chrom, tpos)),
                    RecMode::HomRef => {
                        vcf::homozygous_ref(chrom, tpos, &catalog.traits[i].ref_allele)
                    }
                    RecMode::NoCall => vcf::no_call(chrom, tpos),
                };
                trait_geno[i] = Some(g);
                *remaining -= 1;
            }
        }
        Target::Score { score, variant } => {
            if !consumed[score][variant] {
                consumed[score][variant] = true;
                match mode {
                    RecMode::Variant => {
                        if let Some(g) = real {
                            accs[score].add(variant, g);
                        }
                    }
                    RecMode::HomRef => accs[score].add_homref(variant, homref_ref),
                    RecMode::NoCall => {} // explicit no-call → excluded from coverage
                }
                *remaining -= 1;
            }
        }
    }
}

/// Build the trait + polygenic-score report for one genome. `scores` are the
/// already-selected specs (built-in and/or PGS) to apply.
pub fn build_report(src: &VcfSource, catalog: &Catalog, scores: &[ScoreSpec]) -> Result<Report> {
    if catalog.traits.is_empty() && scores.is_empty() {
        return Err(anyhow!("nothing to compute: no traits and no scores selected"));
    }

    // Indexes: by rsID and exact chrom:pos (order-independent), plus a
    // per-chromosome position-sorted list to resolve gVCF reference-block *ranges*
    // (a hom-ref block covers many positions with one record). `pos_set` is a
    // cheap, allocation-free gate so the vast majority of genome records — which
    // match no target — skip the per-line lookups entirely.
    let mut by_rsid: HashMap<String, Vec<Target>> = HashMap::new();
    let mut by_pos: HashMap<String, Vec<Target>> = HashMap::new();
    let mut chrom_tmp: HashMap<String, Vec<(u64, Target)>> = HashMap::new();
    let mut pos_set: HashSet<u64> = HashSet::new();

    for (ti, t) in catalog.traits.iter().enumerate() {
        index_target(&mut by_rsid, &mut by_pos, &mut chrom_tmp, &mut pos_set, &t.rsid, &t.chrom, t.pos, Target::Trait(ti));
    }
    for (si, s) in scores.iter().enumerate() {
        for (vi, v) in s.variants.iter().enumerate() {
            index_target(&mut by_rsid, &mut by_pos, &mut chrom_tmp, &mut pos_set, &v.rsid, &v.chrom, v.pos, Target::Score { score: si, variant: vi });
        }
    }

    // Group each chromosome's targets by position, sorted ascending (matches the
    // coordinate-sorted order of a gVCF, enabling a merge-join cursor).
    let mut chrom_sorted: HashMap<String, Vec<(u64, Vec<Target>)>> = HashMap::new();
    for (ck, mut v) in chrom_tmp {
        v.sort_by_key(|(p, _)| *p);
        let mut grouped: Vec<(u64, Vec<Target>)> = Vec::new();
        for (p, t) in v {
            match grouped.last_mut() {
                Some((lp, ts)) if *lp == p => ts.push(t),
                _ => grouped.push((p, vec![t])),
            }
        }
        chrom_sorted.insert(ck, grouped);
    }
    let mut cursor: HashMap<String, usize> = HashMap::new();

    // Per-target state.
    let mut trait_geno: Vec<Option<Genotype>> = vec![None; catalog.traits.len()];
    let mut accs: Vec<ScoreAccumulator> = scores.iter().map(ScoreAccumulator::new).collect();
    let mut consumed: Vec<Vec<bool>> =
        scores.iter().map(|s| vec![false; s.variants.len()]).collect();
    let mut remaining = catalog.traits.len() + scores.iter().map(|s| s.variants.len()).sum::<usize>();
    let have_rsids = !by_rsid.is_empty();

    let mut hits: Vec<Target> = Vec::new();
    src.scan(|rec| {
        let ref_only = rec.is_reference_only();
        let homref_block = ref_only && rec.is_homozygous_ref();

        // Step A — exact chrom:pos + rsID (order-independent), gated cheaply.
        hits.clear();
        if have_rsids && rec.ids.as_bytes().first() == Some(&b'r') {
            for id in rec.ids.split(';') {
                let rs = id.trim().to_ascii_lowercase();
                if let Some(ts) = by_rsid.get(&rs) {
                    hits.extend_from_slice(ts);
                }
            }
        }
        if pos_set.contains(&rec.pos) {
            if let Some(ts) = by_pos.get(&pos_key(rec.chrom, rec.pos)) {
                hits.extend_from_slice(ts);
            }
        }
        if !hits.is_empty() {
            let mode = if ref_only {
                if homref_block { RecMode::HomRef } else { RecMode::NoCall }
            } else {
                RecMode::Variant
            };
            let real = matches!(mode, RecMode::Variant).then(|| vcf::genotype_from_record(rec));
            for t in &hits {
                let homref_ref = matches!(mode, RecMode::HomRef).then_some(rec.ref_allele);
                resolve_target(&mut trait_geno, &mut accs, &mut consumed, &mut remaining, catalog, *t, rec.pos, rec.chrom, mode, homref_ref, real.as_ref());
            }
        }

        // Step B — gVCF reference block: resolve interior positions (POS, END].
        if homref_block {
            let end = rec.end();
            if end > rec.pos {
                let ck = norm_chrom(rec.chrom);
                if let Some(list) = chrom_sorted.get(&ck) {
                    let cur = cursor.entry(ck).or_insert(0);
                    // targets before the block are resolved already or in a gap → missing
                    while *cur < list.len() && list[*cur].0 < rec.pos {
                        *cur += 1;
                    }
                    let mut i = *cur;
                    while i < list.len() && list[i].0 <= end {
                        if list[i].0 > rec.pos {
                            // pos == POS was handled in Step A above
                            for t in &list[i].1 {
                                resolve_target(&mut trait_geno, &mut accs, &mut consumed, &mut remaining, catalog, *t, list[i].0, rec.chrom, RecMode::HomRef, None, None);
                            }
                        }
                        i += 1;
                    }
                    *cur = i;
                }
            }
        }

        remaining > 0 // stop early once every locus is captured
    })?;

    // Assemble trait results.
    let mut traits = Vec::with_capacity(catalog.traits.len());
    let mut missing = 0;
    for (i, t) in catalog.traits.iter().enumerate() {
        let g = trait_geno[i].as_ref();
        if g.is_none() {
            missing += 1;
        }
        let (genotype, no_call, chrom, pos) = match g {
            Some(g) => (g.key.clone(), g.no_call, g.chrom.clone(), g.pos),
            None => (String::new(), false, norm_chrom(&t.chrom), t.pos),
        };
        let (genotype, interp) = interpret_trait(t, genotype, no_call);
        traits.push(TraitResult {
            id: t.id.clone(),
            name: t.name.clone(),
            category: t.category.clone(),
            rsid: t.rsid.clone(),
            note: t.note.clone(),
            source_url: t.source_url.clone(),
            genotype,
            found: g.is_some(),
            no_call,
            interp,
            chrom,
            pos,
        });
    }

    let scores_out: Vec<ScoreResult> = accs.into_iter().map(|a| a.finish()).collect();

    Ok(Report {
        build: catalog.build.clone(),
        traits,
        scores: scores_out,
        traits_missing: missing,
        traits_total: catalog.traits.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;
    use std::path::{Path, PathBuf};

    fn write_vcf_gz(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        let f = std::fs::File::create(&path).unwrap();
        let mut enc = GzEncoder::new(f, Compression::default());
        enc.write_all(body.as_bytes()).unwrap();
        enc.finish().unwrap();
        path
    }

    const HEADER: &str = "##fileformat=VCFv4.2\n#CHROM\tPOS\tID\tREF\tALT\tQUAL\tFILTER\tINFO\tFORMAT\tSAMPLE\n";

    fn tmpdir() -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let d = std::env::temp_dir().join(format!("oura-dna-lib-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Write an inline catalog to a temp file and load it through the real
    /// `Catalog::load` (so genotype-key folding + band sorting are exercised).
    fn catalog(dir: &Path) -> Catalog {
        let json = r#"{
          "build": "GRCh38",
          "traits": [
            {"id":"lactase","name":"Lactase","category":"nutrition",
             "rsid":"rs4988235","chrom":"2","pos":135851076,"ref":"C","alt":"T",
             "genotypes":{"TT":{"label":"Persistent","magnitude":"good"},
                          "CT":{"label":"Intermediate","magnitude":"neutral"},
                          "CC":{"label":"Non-persistent","magnitude":"watch"}}},
            {"id":"eye","name":"Eye","category":"traits",
             "rsid":"rs12913832","chrom":"15","pos":28120472,"ref":"A","alt":"G",
             "genotypes":{"GG":{"label":"Brown","magnitude":"neutral"}}}
          ],
          "scores": [
            {"id":"demo","name":"Demo","category":"health",
             "bands":[{"label":"Higher"},{"max":1.0,"label":"Lower"},{"max":2.5,"label":"Average"}],
             "snps":[
               {"rsid":"rs4988235","chrom":"2","pos":135851076,"effect_allele":"T","weight":1.0},
               {"rsid":"rs12913832","chrom":"15","pos":28120472,"effect_allele":"G","weight":0.5}
             ]}
          ]
        }"#;
        let p = dir.join("cat.json");
        std::fs::write(&p, json).unwrap();
        Catalog::load(&p).unwrap()
    }

    #[test]
    fn traits_and_builtin_score() {
        let dir = tmpdir();
        let cat = catalog(&dir);

        let body = format!(
            "{HEADER}\
2\t135851076\trs4988235\tC\tT\t.\t.\t.\tGT\t0/1\n\
chr15\t28120472\t.\tA\tG\t.\t.\t.\tGT\t1|1\n"
        );
        let path = write_vcf_gz(&dir, "g.vcf.gz", &body);
        let specs = cat.builtin_scores();
        let rep = build_report(&VcfSource::open(&path), &cat, &specs).unwrap();

        assert_eq!(rep.traits[0].genotype, "CT");
        assert!(rep.traits[1].found); // matched by position (ID ".")
        let sc = &rep.scores[0];
        // T dosage 1 * 1.0 + G dosage 2 * 0.5 = 2.0 → "Average"
        assert!((sc.value - 2.0).abs() < 1e-9, "{}", sc.value);
        assert_eq!(sc.band.as_ref().unwrap().label, "Average");
        assert_eq!(sc.matched, 2);
    }

    #[test]
    fn gvcf_reference_blocks_resolve_homref() {
        let dir = tmpdir();
        let cat = catalog(&dir);
        // A gVCF: lactase (chr2:135851076) sits INSIDE a hom-ref block (no explicit
        // record at its position); eye (chr15:28120472) has an explicit variant.
        // Records are coordinate-sorted, as gVCFs always are.
        let body = format!(
            "{HEADER}\
2\t135851000\t.\tC\t.\t.\tPASS\tEND=135851100;MinDP=20\tGT:DP\t0/0:22\n\
15\t28120472\trs12913832\tA\tG\t.\tPASS\t.\tGT\t0/1\n"
        );
        let path = write_vcf_gz(&dir, "gvcf.vcf.gz", &body);
        let specs = cat.builtin_scores();
        let rep = build_report(&VcfSource::open(&path), &cat, &specs).unwrap();

        // lactase resolved from the reference block → homozygous reference "CC".
        let lact = &rep.traits[0];
        assert!(lact.found, "lactase should resolve from the ref block");
        assert!(!lact.no_call);
        assert_eq!(lact.genotype, "CC");
        assert_eq!(lact.interp.as_ref().unwrap().label, "Non-persistent");

        // eye from the explicit variant → A/G.
        assert_eq!(rep.traits[1].genotype, "AG");

        // Score: rs4988235 hom-ref → dosage 0 (matched, no contribution);
        // rs12913832 effect G dosage 1 → 0.5. Coverage is full (2/2).
        let sc = &rep.scores[0];
        assert_eq!(sc.matched, 2, "both variants covered (one via ref block)");
        assert!((sc.value - 0.5).abs() < 1e-9, "value {}", sc.value);
        assert_eq!(sc.contributors.len(), 1, "only the non-ref site contributes");
        assert_eq!(sc.contributors[0].rsid, "rs12913832");
    }

    #[test]
    fn trait_interpretation_is_strand_aware() {
        let dir = tmpdir();
        let cat = catalog(&dir);
        // The catalog writes rs4988235 as C/T (classic strand), but a GRCh38
        // forward-strand VCF stores it as G/A. A homozygous-derived sample is A/A
        // on the forward strand → should reverse-complement to "TT" (Persistent).
        let body = format!(
            "{HEADER}2\t135851076\trs4988235\tG\tA\t.\tPASS\t.\tGT\t1/1\n"
        );
        let path = write_vcf_gz(&dir, "strand.vcf.gz", &body);
        let rep = build_report(&VcfSource::open(&path), &cat, &[]).unwrap();
        let lact = &rep.traits[0];
        assert!(lact.found);
        assert_eq!(lact.genotype, "TT", "displayed on the catalog strand");
        assert_eq!(lact.interp.as_ref().unwrap().label, "Persistent");
    }

    #[test]
    fn shipped_catalog_loads_and_scores() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../dna/catalog.json");
        let cat = Catalog::load(&path).expect("shipped catalog loads");
        assert!(!cat.traits.is_empty());
        let specs = cat.builtin_scores();
        assert!(!specs.is_empty());
        for s in &specs {
            assert!(!s.variants.is_empty(), "{}", s.id);
        }
    }
}
