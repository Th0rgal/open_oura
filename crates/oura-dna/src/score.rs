//! Polygenic score engine.
//!
//! One [`ScoreSpec`] is a list of weighted variants plus metadata; it can come
//! from the illustrative catalog ([`crate::catalog`]) or a real PGS Catalog
//! scoring file ([`crate::pgs`]). Scoring is *streaming*: the report builder makes
//! one pass over the genome and feeds each matching genotype to a
//! [`ScoreAccumulator`], so a million-variant PGS never materialises a
//! million-genotype map.
//!
//! ## Rigour
//!
//! For each variant we match the effect/other alleles against the site's alleles
//! (REF + ALTs):
//!   * **direct** — `{effect, other}` present at the site → count the effect allele;
//!   * **strand-flip** — the reverse-complements are present (and the SNP is not
//!     A/T or C/G palindromic) → count the complemented effect allele;
//!   * otherwise the variant is **ambiguous** (palindromic, unresolvable) or a
//!     **mismatch** and is *excluded* from the sum.
//!
//! `weight_type` is honoured: `OR`/`HR` weights are log-transformed (`ln`) so the
//! sum is on the additive log scale; `beta`/`NR` are used as-is.
//!
//! A raw weighted sum has **no absolute meaning** without an ancestry-matched
//! reference distribution, which we do not ship — so PGS results report the sum,
//! the coverage (matched/total), and a caveat rather than a bogus percentile.

use serde::{Deserialize, Serialize};

use crate::util::{complement, is_palindromic};
use crate::vcf::Genotype;

/// A qualitative band for a score value (illustrative catalog scores only).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Band {
    /// Upper bound (inclusive). `None` = open-ended top band.
    #[serde(default)]
    pub max: Option<f64>,
    pub label: String,
    #[serde(default)]
    pub magnitude: Option<String>,
}

/// One weighted variant in a score.
#[derive(Debug, Clone)]
pub struct ScoreVariant {
    pub rsid: String,
    pub chrom: String,
    pub pos: u64,
    pub effect: String,
    /// Known reference allele for this target, when the score source carries it.
    /// Exact hom-ref VCF records override this with their REF column.
    pub ref_allele: Option<String>,
    /// The non-effect allele, when the source provides it (PGS files do; the toy
    /// catalog scores may not). Enables strict allele-set + strand matching.
    pub other: Option<String>,
    pub weight: f64,
}

/// A fully-resolved score definition ready to apply to a genome.
#[derive(Debug, Clone)]
pub struct ScoreSpec {
    pub id: String,
    pub name: String,
    pub category: String,
    /// `"builtin"` (illustrative catalog) or `"pgs"` (PGS Catalog file).
    pub source: String,
    pub trait_reported: Option<String>,
    /// `"beta" | "OR" | "HR" | "NR" | …` — drives the log transform.
    pub weight_type: String,
    pub genome_build: Option<String>,
    pub note: Option<String>,
    pub variants: Vec<ScoreVariant>,
    /// Qualitative bands (illustrative scores). Empty for PGS files.
    pub bands: Vec<Band>,
}

impl ScoreSpec {
    /// Whether weights are odds/hazard ratios that must be `ln`-transformed to sum
    /// on the additive scale.
    fn log_transform(&self) -> bool {
        matches!(self.weight_type.to_ascii_uppercase().as_str(), "OR" | "HR")
    }

    fn effective_weight(&self, w: f64) -> f64 {
        if self.log_transform() && w > 0.0 {
            w.ln()
        } else {
            w
        }
    }
}

/// Outcome of matching one variant's alleles against a genotype's site alleles.
enum Match {
    /// Effect-allele dosage (0/1/2) after a direct or strand-flip match.
    Dosage(u8),
    /// A/T or C/G SNP whose strand can't be resolved — excluded.
    Ambiguous,
    /// Alleles don't correspond to the site — excluded.
    Mismatch,
}

/// Decide the effect-allele dosage for `variant` given the observed `geno`,
/// applying the strand rules described in the module docs.
fn match_variant(variant: &ScoreVariant, geno: &Genotype) -> Match {
    if geno.no_call {
        return Match::Mismatch;
    }
    let site: Vec<String> = geno.site_alleles();
    let e = variant.effect.to_ascii_uppercase();

    // When we know both alleles we can verify the site and detect strand flips.
    if let Some(other) = &variant.other {
        let o = other.to_ascii_uppercase();
        let at_site = |a: &str| site.iter().any(|s| s == a);
        // direct: both alleles present at the site
        if at_site(&e) && at_site(&o) {
            return Match::Dosage(count(&geno.alleles, &e));
        }
        // strand flip: reverse-complements present, but never for palindromes
        let (ec, oc) = (complement(&e), complement(&o));
        if !is_palindromic(&e, &o) && at_site(&ec) && at_site(&oc) {
            return Match::Dosage(count(&geno.alleles, &ec));
        }
        // palindromic and not a direct match → unresolvable strand
        if is_palindromic(&e, &o) {
            return Match::Ambiguous;
        }
        return Match::Mismatch;
    }

    // No other allele given (toy catalog scores): count directly if the effect
    // allele is present at the site, else treat as a mismatch.
    if site.contains(&e) {
        Match::Dosage(count(&geno.alleles, &e))
    } else {
        Match::Mismatch
    }
}

fn count(alleles: &[String], target: &str) -> u8 {
    alleles
        .iter()
        .filter(|a| a.eq_ignore_ascii_case(target))
        .count() as u8
}

/// Resolve effect-allele dosage for a homozygous-reference call. Unlike a normal
/// variant record, a gVCF reference block may not expose the target's alternate
/// allele, so we use the known REF allele directly.
fn match_homref(variant: &ScoreVariant, ref_allele: &str) -> Match {
    let r = ref_allele.trim().to_ascii_uppercase();
    if r.is_empty() {
        return Match::Dosage(0);
    }
    let e = variant.effect.to_ascii_uppercase();
    if let Some(other) = &variant.other {
        let o = other.to_ascii_uppercase();
        if e == r || o == r {
            return Match::Dosage(if e == r { 2 } else { 0 });
        }
        let (ec, oc) = (complement(&e), complement(&o));
        if !is_palindromic(&e, &o) && (ec == r || oc == r) {
            return Match::Dosage(if ec == r { 2 } else { 0 });
        }
        if is_palindromic(&e, &o) && (ec == r || oc == r) {
            return Match::Ambiguous;
        }
        return Match::Mismatch;
    }

    Match::Dosage(if e == r { 2 } else { 0 })
}

/// The kept part of a per-variant contribution (for the "top contributors" list).
#[derive(Debug, Clone, Serialize)]
pub struct Contributor {
    pub rsid: String,
    pub effect_allele: String,
    pub weight: f64,
    pub dosage: u8,
    pub contribution: f64,
}

/// Accumulates a score over a streaming pass. Only bounded state is kept, so it
/// scales to million-variant PGS files.
pub struct ScoreAccumulator<'a> {
    spec: &'a ScoreSpec,
    sum: f64,
    matched: usize,
    ambiguous: usize,
    mismatched: usize,
    /// Sum of `2·|effective_weight|` over matched variants — the theoretical span
    /// used only to place the toy-score bands' gauge, never shown as a percentile.
    span: f64,
    /// Bounded top-|contribution| variants for display.
    top: Vec<Contributor>,
}

const TOP_CONTRIBUTORS: usize = 14;

impl<'a> ScoreAccumulator<'a> {
    pub fn new(spec: &'a ScoreSpec) -> Self {
        ScoreAccumulator {
            spec,
            sum: 0.0,
            matched: 0,
            ambiguous: 0,
            mismatched: 0,
            span: 0.0,
            top: Vec::new(),
        }
    }

    /// Feed the genotype observed at `variant_idx`.
    pub fn add(&mut self, variant_idx: usize, geno: &Genotype) {
        let v = &self.spec.variants[variant_idx];
        self.add_match(variant_idx, match_variant(v, geno));
    }

    fn add_match(&mut self, variant_idx: usize, m: Match) {
        let v = &self.spec.variants[variant_idx];
        match m {
            Match::Dosage(d) => {
                self.matched += 1;
                let w = self.spec.effective_weight(v.weight);
                self.span += 2.0 * w.abs();
                let contribution = d as f64 * w;
                self.sum += contribution;
                if d > 0 {
                    self.record_top(Contributor {
                        rsid: v.rsid.clone(),
                        effect_allele: v.effect.clone(),
                        weight: round4(w),
                        dosage: d,
                        contribution: round4(contribution),
                    });
                }
            }
            Match::Ambiguous => self.ambiguous += 1,
            Match::Mismatch => self.mismatched += 1,
        }
    }

    /// Feed a **homozygous-reference** call at `variant_idx` — the sample matches
    /// the reference genome here (e.g. a gVCF reference block). When the target
    /// REF allele is known and is the effect allele, the dosage is 2; otherwise
    /// the matched reference call contributes dosage 0.
    pub fn add_homref(&mut self, variant_idx: usize, ref_allele: Option<&str>) {
        let v = &self.spec.variants[variant_idx];
        let known_ref = ref_allele.or(v.ref_allele.as_deref());
        let m = known_ref
            .map(|r| match_homref(v, r))
            .unwrap_or(Match::Ambiguous);
        self.add_match(variant_idx, m);
    }

    fn record_top(&mut self, c: Contributor) {
        // keep the TOP_CONTRIBUTORS largest by |contribution|
        if self.top.len() < TOP_CONTRIBUTORS {
            self.top.push(c);
            return;
        }
        if let Some((i, min)) = self.top.iter().enumerate().min_by(|a, b| {
            a.1.contribution
                .abs()
                .partial_cmp(&b.1.contribution.abs())
                .unwrap()
        }) {
            if c.contribution.abs() > min.contribution.abs() {
                self.top[i] = c;
            }
        }
    }

    pub fn finish(mut self) -> ScoreResult {
        self.top.sort_by(|a, b| {
            b.contribution
                .abs()
                .partial_cmp(&a.contribution.abs())
                .unwrap()
        });
        let total = self.spec.variants.len();
        let band = band_for(&self.spec.bands, self.sum);
        // Bands (illustrative scores) drive a value/span gauge; PGS files have no
        // bands, so their gauge shows coverage instead.
        let metric = if self.spec.bands.is_empty() {
            "coverage"
        } else {
            "band"
        };
        let fraction = if metric == "coverage" {
            if total > 0 {
                self.matched as f64 / total as f64
            } else {
                0.0
            }
        } else if self.span > 0.0 {
            (self.sum / self.span).clamp(0.0, 1.0)
        } else {
            0.0
        };
        ScoreResult {
            id: self.spec.id.clone(),
            name: self.spec.name.clone(),
            category: self.spec.category.clone(),
            source: self.spec.source.clone(),
            trait_reported: self.spec.trait_reported.clone(),
            weight_type: self.spec.weight_type.clone(),
            log_scaled: self.spec.log_transform(),
            genome_build: self.spec.genome_build.clone(),
            note: self.spec.note.clone(),
            value: round4(self.sum),
            metric: metric.to_string(),
            fraction,
            band,
            matched: self.matched,
            total,
            ambiguous: self.ambiguous,
            mismatched: self.mismatched,
            contributors: self.top,
        }
    }
}

/// First band whose `max` the value does not exceed; the open-ended band catches
/// anything above all finite bounds.
fn band_for(bands: &[Band], value: f64) -> Option<Band> {
    bands
        .iter()
        .find(|b| b.max.map(|m| value <= m).unwrap_or(true))
        .cloned()
}

/// The rendered result of one score.
#[derive(Debug, Clone, Serialize)]
pub struct ScoreResult {
    pub id: String,
    pub name: String,
    pub category: String,
    pub source: String,
    pub trait_reported: Option<String>,
    pub weight_type: String,
    /// True when OR/HR weights were `ln`-transformed before summing.
    pub log_scaled: bool,
    pub genome_build: Option<String>,
    pub note: Option<String>,
    /// The weighted sum (log-scale when `log_scaled`).
    pub value: f64,
    /// How `fraction` should be read: `"band"` (value within span) or
    /// `"coverage"` (matched/total).
    pub metric: String,
    /// `[0,1]` gauge fill — see `metric`.
    pub fraction: f64,
    /// Qualitative band, for illustrative scores only.
    pub band: Option<Band>,
    pub matched: usize,
    pub total: usize,
    /// Variants excluded because strand was unresolvable (A/T, C/G).
    pub ambiguous: usize,
    /// Variants excluded because their alleles didn't correspond to the site.
    pub mismatched: usize,
    pub contributors: Vec<Contributor>,
}

fn round4(v: f64) -> f64 {
    (v * 10000.0).round() / 10000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vcf::Genotype;

    fn geno(chrom: &str, pos: u64, refa: &str, alt: &str, alleles: &[&str]) -> Genotype {
        Genotype {
            alleles: alleles.iter().map(|a| a.to_string()).collect(),
            key: String::new(),
            chrom: chrom.into(),
            pos,
            rsid: ".".into(),
            ref_allele: refa.into(),
            alt: alt.into(),
            no_call: false,
        }
    }

    fn spec(weight_type: &str, variants: Vec<ScoreVariant>) -> ScoreSpec {
        ScoreSpec {
            id: "t".into(),
            name: "t".into(),
            category: "health".into(),
            source: "pgs".into(),
            trait_reported: None,
            weight_type: weight_type.into(),
            genome_build: None,
            note: None,
            variants,
            bands: vec![],
        }
    }

    fn var(effect: &str, other: Option<&str>, w: f64) -> ScoreVariant {
        ScoreVariant {
            rsid: "rs1".into(),
            chrom: "1".into(),
            pos: 100,
            effect: effect.into(),
            ref_allele: None,
            other: other.map(|s| s.into()),
            weight: w,
        }
    }

    #[test]
    fn direct_match_dosage() {
        let s = spec("beta", vec![var("T", Some("C"), 0.5)]);
        let mut acc = ScoreAccumulator::new(&s);
        acc.add(0, &geno("1", 100, "C", "T", &["C", "T"])); // 1 effect allele
        let r = acc.finish();
        assert_eq!(r.matched, 1);
        assert!((r.value - 0.5).abs() < 1e-9);
    }

    #[test]
    fn strand_flip_non_palindromic() {
        // effect T / other C, but site is A/G (reverse-complement) → flip to A.
        let s = spec("beta", vec![var("T", Some("C"), 1.0)]);
        let mut acc = ScoreAccumulator::new(&s);
        acc.add(0, &geno("1", 100, "G", "A", &["A", "A"])); // 2 copies of complement(T)=A
        let r = acc.finish();
        assert_eq!(r.matched, 1);
        assert_eq!(r.ambiguous, 0);
        assert!((r.value - 2.0).abs() < 1e-9);
    }

    #[test]
    fn palindromic_flip_is_ambiguous() {
        // effect A / other T is palindromic; site C/G can't be resolved → excluded.
        let s = spec("beta", vec![var("A", Some("T"), 1.0)]);
        let mut acc = ScoreAccumulator::new(&s);
        acc.add(0, &geno("1", 100, "C", "G", &["C", "G"]));
        let r = acc.finish();
        assert_eq!(r.matched, 0);
        assert_eq!(r.ambiguous, 1);
        assert_eq!(r.value, 0.0);
    }

    #[test]
    fn palindromic_direct_match_is_kept() {
        // same palindromic SNP, but the site alleles match directly → counted.
        let s = spec("beta", vec![var("A", Some("T"), 1.0)]);
        let mut acc = ScoreAccumulator::new(&s);
        acc.add(0, &geno("1", 100, "T", "A", &["A", "T"]));
        let r = acc.finish();
        assert_eq!(r.matched, 1);
        assert_eq!(r.ambiguous, 0);
        assert!((r.value - 1.0).abs() < 1e-9);
    }

    #[test]
    fn odds_ratio_is_log_transformed() {
        // OR weight of e^1 → ln = 1.0 per allele.
        let s = spec("OR", vec![var("T", Some("C"), std::f64::consts::E)]);
        let mut acc = ScoreAccumulator::new(&s);
        acc.add(0, &geno("1", 100, "C", "T", &["T", "T"])); // dosage 2
        let r = acc.finish();
        assert!(r.log_scaled);
        assert!((r.value - 2.0).abs() < 1e-6, "value {}", r.value);
    }

    #[test]
    fn mismatch_excluded() {
        // effect T / other C: site A/C is neither a direct match nor the
        // reverse-complement {A,G} of the variant → a true mismatch.
        let s = spec("beta", vec![var("T", Some("C"), 1.0)]);
        let mut acc = ScoreAccumulator::new(&s);
        acc.add(0, &geno("1", 100, "A", "C", &["A", "C"]));
        let r = acc.finish();
        assert_eq!(r.matched, 0);
        assert_eq!(r.mismatched, 1);
    }

    #[test]
    fn homref_effect_allele_counts_as_two() {
        let mut v = var("A", Some("G"), 0.75);
        v.ref_allele = Some("A".into());
        let s = spec("beta", vec![v]);
        let mut acc = ScoreAccumulator::new(&s);
        acc.add_homref(0, None);
        let r = acc.finish();
        assert_eq!(r.matched, 1);
        assert!((r.value - 1.5).abs() < 1e-9);
    }

    #[test]
    fn homref_other_allele_is_zero_dosage() {
        let mut v = var("A", Some("G"), 0.75);
        v.ref_allele = Some("G".into());
        let s = spec("beta", vec![v]);
        let mut acc = ScoreAccumulator::new(&s);
        acc.add_homref(0, None);
        let r = acc.finish();
        assert_eq!(r.matched, 1);
        assert_eq!(r.value, 0.0);
    }

    #[test]
    fn homref_unknown_ref_does_not_inflate_coverage() {
        let s = spec("beta", vec![var("A", None, 0.75)]);
        let mut acc = ScoreAccumulator::new(&s);
        acc.add_homref(0, None);
        let r = acc.finish();
        assert_eq!(r.matched, 0);
        assert_eq!(r.ambiguous, 1);
        assert_eq!(r.value, 0.0);
    }
}
