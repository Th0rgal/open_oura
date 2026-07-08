//! The curated, user-editable `catalog.json`: single-SNP **traits** (with a
//! genotype→meaning table) and a few illustrative **built-in scores**. Real
//! polygenic scores come from PGS Catalog files instead (see [`crate::pgs`]).

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::score::{Band, ScoreSpec, ScoreVariant};
use crate::util::norm_genotype;

/// The whole catalog file.
#[derive(Debug, Clone, Deserialize)]
pub struct Catalog {
    /// Build the `chrom`/`pos` fields are given in (e.g. `"GRCh38"`). rsID
    /// matching is build-independent regardless.
    #[serde(default = "default_build")]
    pub build: String,
    #[serde(default)]
    pub traits: Vec<TraitDef>,
    /// Illustrative built-in scores. Converted to [`ScoreSpec`]s via
    /// [`Catalog::builtin_scores`].
    #[serde(default)]
    pub scores: Vec<ScoreDef>,
}

fn default_build() -> String {
    "GRCh38".to_string()
}

/// A single-SNP trait with a genotype→meaning table.
#[derive(Debug, Clone, Deserialize)]
pub struct TraitDef {
    pub id: String,
    pub name: String,
    #[serde(default = "default_category")]
    pub category: String,
    pub rsid: String,
    pub chrom: String,
    pub pos: u64,
    #[serde(rename = "ref", default)]
    pub ref_allele: String,
    #[serde(default)]
    pub alt: String,
    /// Genotype (sorted alleles, e.g. `"AG"`) → interpretation.
    #[serde(default)]
    pub genotypes: HashMap<String, GenoInterp>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub source_url: Option<String>,
}

fn default_category() -> String {
    "other".to_string()
}

/// The meaning of one genotype at a trait SNP.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GenoInterp {
    pub label: String,
    /// `"good" | "neutral" | "watch"`.
    #[serde(default = "default_magnitude")]
    pub magnitude: String,
    #[serde(default)]
    pub note: Option<String>,
}

fn default_magnitude() -> String {
    "neutral".to_string()
}

/// An illustrative built-in polygenic score defined inline in the catalog.
#[derive(Debug, Clone, Deserialize)]
pub struct ScoreDef {
    pub id: String,
    pub name: String,
    #[serde(default = "default_category")]
    pub category: String,
    #[serde(default)]
    pub note: Option<String>,
    pub snps: Vec<ScoreDefSnp>,
    #[serde(default)]
    pub bands: Vec<Band>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ScoreDefSnp {
    pub rsid: String,
    pub chrom: String,
    pub pos: u64,
    #[serde(default)]
    pub ref_allele: Option<String>,
    pub effect_allele: String,
    #[serde(default)]
    pub other_allele: Option<String>,
    pub weight: f64,
}

impl Catalog {
    /// Load and normalise a catalog from a JSON file.
    pub fn load(path: &Path) -> Result<Catalog> {
        let bytes =
            std::fs::read(path).with_context(|| format!("reading catalog {}", path.display()))?;
        let mut cat: Catalog = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing catalog {}", path.display()))?;
        cat.normalize();
        Ok(cat)
    }

    /// Fold genotype keys to sorted-uppercase and sort each score's bands.
    fn normalize(&mut self) {
        for t in &mut self.traits {
            let folded: HashMap<String, GenoInterp> = t
                .genotypes
                .drain()
                .map(|(k, v)| (norm_genotype(&k), v))
                .collect();
            t.genotypes = folded;
        }
        for s in &mut self.scores {
            s.bands.sort_by(|a, b| cmp_band_max(a.max, b.max));
        }
    }

    /// The illustrative scores as ready-to-apply [`ScoreSpec`]s.
    pub fn builtin_scores(&self) -> Vec<ScoreSpec> {
        self.scores
            .iter()
            .map(|s| ScoreSpec {
                id: s.id.clone(),
                name: s.name.clone(),
                category: s.category.clone(),
                source: "builtin".to_string(),
                trait_reported: None,
                weight_type: "beta".to_string(),
                genome_build: Some(self.build.clone()),
                note: s.note.clone(),
                variants: s
                    .snps
                    .iter()
                    .map(|v| ScoreVariant {
                        rsid: v.rsid.clone(),
                        chrom: v.chrom.clone(),
                        pos: v.pos,
                        effect: v.effect_allele.clone(),
                        ref_allele: v.ref_allele.clone(),
                        other: v.other_allele.clone(),
                        weight: v.weight,
                    })
                    .collect(),
                bands: s.bands.clone(),
            })
            .collect()
    }
}

/// `None` (open-ended) sorts last; otherwise by ascending bound.
fn cmp_band_max(a: Option<f64>, b: Option<f64>) -> std::cmp::Ordering {
    match (a, b) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, _) => std::cmp::Ordering::Greater,
        (_, None) => std::cmp::Ordering::Less,
        (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal),
    }
}
