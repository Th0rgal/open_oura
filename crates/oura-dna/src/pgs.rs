//! Parser for [PGS Catalog](https://www.pgscatalog.org/) scoring files.
//!
//! A scoring file is a tab-separated table preceded by `#key=value` metadata
//! lines. We read the metadata (trait, build, `weight_type`, variant count) and
//! the per-variant rows (`effect_allele`, `other_allele`, `effect_weight`, plus
//! position/rsID), preferring the **harmonized** `hm_*` columns when present so
//! positions are on the requested build. Both plain `.txt` and gzipped `.txt.gz`
//! are accepted. The result is a [`ScoreSpec`] the scoring engine applies exactly
//! like a built-in score.

use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use flate2::read::MultiGzDecoder;
use serde::Serialize;

use crate::score::{ScoreSpec, ScoreVariant};

/// Lightweight metadata read from a scoring file's header (no variant parse).
#[derive(Debug, Clone, Serialize)]
pub struct PgsMeta {
    /// The file's basename (what the selector uses to identify it).
    pub file: String,
    pub id: Option<String>,
    pub name: Option<String>,
    pub trait_reported: Option<String>,
    pub genome_build: Option<String>,
    pub weight_type: Option<String>,
    pub variants_number: Option<usize>,
}

fn open_reader(path: &Path) -> Result<Box<dyn BufRead>> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let is_gz = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("gz"));
    let inner: Box<dyn Read> = if is_gz {
        Box::new(MultiGzDecoder::new(file))
    } else {
        Box::new(file)
    };
    Ok(Box::new(BufReader::with_capacity(1 << 20, inner)))
}

fn basename(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}

/// Parse a `#key=value` header line into `(key, value)`, if it is one.
fn header_kv(line: &str) -> Option<(String, String)> {
    let l = line.trim_start_matches('#').trim();
    let (k, v) = l.split_once('=')?;
    Some((k.trim().to_ascii_lowercase(), v.trim().to_string()))
}

/// Read only the header metadata (cheap — used to populate the score selector).
pub fn read_meta(path: &Path) -> Result<PgsMeta> {
    let mut reader = open_reader(path)?;
    let mut meta = PgsMeta {
        file: basename(path),
        id: None,
        name: None,
        trait_reported: None,
        genome_build: None,
        weight_type: None,
        variants_number: None,
    };
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        if !line.starts_with('#') {
            break; // reached the column header row
        }
        if let Some((k, v)) = header_kv(&line) {
            match k.as_str() {
                "pgs_id" => meta.id = Some(v),
                "pgs_name" => meta.name = Some(v),
                "trait_reported" => meta.trait_reported = Some(v),
                "genome_build" | "hmpos_build" => meta.genome_build = Some(v),
                "weight_type" => meta.weight_type = Some(v),
                "variants_number" => meta.variants_number = v.parse().ok(),
                _ => {}
            }
        }
    }
    Ok(meta)
}

/// Resolve the column layout of a scoring file, preferring harmonized columns.
struct Columns {
    rsid: Option<usize>,
    hm_rsid: Option<usize>,
    chr: Option<usize>,
    pos: Option<usize>,
    hm_chr: Option<usize>,
    hm_pos: Option<usize>,
    effect: usize,
    other: Option<usize>,
    hm_infer_other: Option<usize>,
    weight: usize,
}

impl Columns {
    fn from_header(cols: &[&str]) -> Result<Columns> {
        let find = |name: &str| cols.iter().position(|c| c.eq_ignore_ascii_case(name));
        Ok(Columns {
            rsid: find("rsID"),
            hm_rsid: find("hm_rsID"),
            chr: find("chr_name"),
            pos: find("chr_position"),
            hm_chr: find("hm_chr"),
            hm_pos: find("hm_pos"),
            effect: find("effect_allele")
                .ok_or_else(|| anyhow!("scoring file has no effect_allele column"))?,
            other: find("other_allele"),
            hm_infer_other: find("hm_inferOtherAllele"),
            weight: find("effect_weight")
                .ok_or_else(|| anyhow!("scoring file has no effect_weight column (non-additive scores are unsupported)"))?,
        })
    }
}

fn cell<'a>(fields: &[&'a str], idx: Option<usize>) -> Option<&'a str> {
    idx.and_then(|i| fields.get(i)).map(|s| s.trim()).filter(|s| !s.is_empty() && *s != ".")
}

/// Fully parse a scoring file into a [`ScoreSpec`].
pub fn load(path: &Path) -> Result<ScoreSpec> {
    let meta = read_meta(path)?;
    let mut reader = open_reader(path)?;
    let mut line = String::new();

    // Skip header metadata, capturing the column header row.
    let mut header: Option<Vec<String>> = None;
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        if line.starts_with('#') {
            continue;
        }
        header = Some(line.trim_end_matches(['\n', '\r']).split('\t').map(|s| s.to_string()).collect());
        break;
    }
    let header = header.ok_or_else(|| anyhow!("scoring file {} has no data", path.display()))?;
    let col_refs: Vec<&str> = header.iter().map(|s| s.as_str()).collect();
    let cols = Columns::from_header(&col_refs)?;

    let mut variants = Vec::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed.is_empty() {
            continue;
        }
        let f: Vec<&str> = trimmed.split('\t').collect();

        let effect = match cell(&f, Some(cols.effect)) {
            Some(a) => a.to_ascii_uppercase(),
            None => continue,
        };
        let weight: f64 = match cell(&f, Some(cols.weight)).and_then(|w| w.parse().ok()) {
            Some(w) => w,
            None => continue,
        };
        // harmonized position first, then original
        let chr = cell(&f, cols.hm_chr).or_else(|| cell(&f, cols.chr));
        let pos = cell(&f, cols.hm_pos)
            .or_else(|| cell(&f, cols.pos))
            .and_then(|p| p.parse::<u64>().ok());
        let rsid = cell(&f, cols.rsid).or_else(|| cell(&f, cols.hm_rsid)).unwrap_or(".");
        let other = cell(&f, cols.other)
            .or_else(|| cell(&f, cols.hm_infer_other))
            .map(|s| s.to_ascii_uppercase());

        // must be locatable by rsID or position, else it can never match
        let has_rsid = rsid.starts_with("rs");
        let (chrom, pos) = match pos {
            Some(p) => (chr.unwrap_or("").to_string(), p),
            None => {
                if !has_rsid {
                    continue;
                }
                (String::new(), 0)
            }
        };

        variants.push(ScoreVariant {
            rsid: rsid.to_string(),
            chrom,
            pos,
            effect,
            ref_allele: None,
            other,
            weight,
        });
    }

    if variants.is_empty() {
        return Err(anyhow!("scoring file {} yielded no usable variants", path.display()));
    }

    let id = meta.id.clone().unwrap_or_else(|| meta.file.clone());
    let name = meta
        .trait_reported
        .clone()
        .or_else(|| meta.name.clone())
        .unwrap_or_else(|| id.clone());
    Ok(ScoreSpec {
        id,
        name,
        category: "pgs".to_string(),
        source: "pgs".to_string(),
        trait_reported: meta.trait_reported.clone(),
        weight_type: meta.weight_type.clone().unwrap_or_else(|| "NR".to_string()),
        genome_build: meta.genome_build.clone(),
        note: None,
        variants,
        bands: vec![],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
        let p = dir.join(name);
        File::create(&p).unwrap().write_all(body.as_bytes()).unwrap();
        p
    }

    #[test]
    fn parses_harmonized_columns_and_meta() {
        let dir = std::env::temp_dir().join(format!("pgs-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let body = "#pgs_id=PGS000999\n#trait_reported=Test trait\n#genome_build=GRCh38\n#weight_type=OR\n#variants_number=2\n\
rsID\tchr_name\tchr_position\teffect_allele\tother_allele\teffect_weight\thm_chr\thm_pos\thm_rsID\n\
rs1\t1\t100\tT\tC\t1.5\t1\t100\trs1\n\
rs2\t2\t200\tA\tG\t0.8\t2\t250\trs2\n";
        let p = write(&dir, "PGS000999.txt", body);

        let meta = read_meta(&p).unwrap();
        assert_eq!(meta.id.as_deref(), Some("PGS000999"));
        assert_eq!(meta.trait_reported.as_deref(), Some("Test trait"));
        assert_eq!(meta.weight_type.as_deref(), Some("OR"));
        assert_eq!(meta.variants_number, Some(2));

        let spec = load(&p).unwrap();
        assert_eq!(spec.variants.len(), 2);
        // harmonized position wins for rs2 (250, not the original 200)
        assert_eq!(spec.variants[1].pos, 250);
        assert_eq!(spec.variants[0].effect, "T");
        assert_eq!(spec.variants[0].other.as_deref(), Some("C"));
        assert_eq!(spec.weight_type, "OR");
    }
}
