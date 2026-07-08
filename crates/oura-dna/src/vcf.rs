//! Streaming reader for a `.vcf.gz` genome and resolution of the sample genotype
//! at a record.
//!
//! Whole-genome files are far too large to hold in memory, so we never build a
//! genome-wide index. Callers drive a **single streaming pass** ([`VcfSource::scan`])
//! and keep only what they need. [`flate2::read::MultiGzDecoder`] transparently
//! handles both plain gzip and the multi-member *bgzip* WGS files use.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use anyhow::{Context, Result};
use flate2::read::MultiGzDecoder;
use serde::Serialize;

use crate::util::{norm_chrom, norm_genotype};

/// The fields of one VCF data line we care about (borrowed from the read buffer).
pub struct Record<'a> {
    pub chrom: &'a str,
    pub pos: u64,
    pub ids: &'a str,       // raw ID column, may be "." or "rsA;rsB"
    pub ref_allele: &'a str,
    pub alt: &'a str,       // raw ALT column, may be "A,C"
    pub info: &'a str,      // INFO column (carries END= for gVCF blocks)
    pub gt: Option<&'a str>, // GT subfield of the first sample, if present
}

impl Record<'_> {
    /// A record with no real alternate allele — a gVCF **reference block** or a
    /// reference-only site. ALT is `.`, `<NON_REF>`, or `<*>`.
    pub fn is_reference_only(&self) -> bool {
        matches!(self.alt.trim(), "." | "" | "<NON_REF>" | "<*>")
    }

    /// The last position this record spans. gVCF reference blocks carry `END=` in
    /// INFO; everything else spans just its own position.
    pub fn end(&self) -> u64 {
        for kv in self.info.split(';') {
            if let Some(v) = kv.strip_prefix("END=") {
                if let Ok(e) = v.trim().parse::<u64>() {
                    return e.max(self.pos);
                }
            }
        }
        self.pos
    }

    /// True when the GT is an explicit homozygous-reference call (`0/0`, `0|0`,
    /// `0`) — i.e. the sample genuinely matches the reference across this span,
    /// as opposed to a `./.` no-call block.
    pub fn is_homozygous_ref(&self) -> bool {
        match self.gt {
            Some(gt) => {
                let mut any = false;
                for tok in gt.split(['/', '|']) {
                    let tok = tok.trim();
                    if tok.is_empty() {
                        continue;
                    }
                    if tok != "0" {
                        return false;
                    }
                    any = true;
                }
                any
            }
            None => false,
        }
    }
}

/// Parse a tab-separated VCF data line. `None` for malformed lines.
pub fn parse_record(line: &str) -> Option<Record<'_>> {
    let mut f = line.split('\t');
    let chrom = f.next()?;
    let pos: u64 = f.next()?.trim().parse().ok()?;
    let ids = f.next()?;
    let ref_allele = f.next()?;
    let alt = f.next()?;
    let _qual = f.next()?;
    let _filter = f.next()?;
    let info = f.next().unwrap_or("");
    // GT lives in the first sample column, indexed via FORMAT.
    let format = f.next();
    let sample = f.next();
    let gt = match (format, sample) {
        (Some(fmt), Some(sample)) => fmt
            .split(':')
            .position(|k| k == "GT")
            .and_then(|i| sample.split(':').nth(i)),
        _ => None,
    };
    Some(Record {
        chrom,
        pos,
        ids,
        ref_allele,
        alt,
        info,
        gt,
    })
}

/// A synthetic no-call genotype (used for `./.` reference blocks over a target).
pub fn no_call(chrom: &str, pos: u64) -> Genotype {
    Genotype {
        alleles: Vec::new(),
        key: String::new(),
        chrom: norm_chrom(chrom),
        pos,
        rsid: ".".to_string(),
        ref_allele: String::new(),
        alt: ".".to_string(),
        no_call: true,
    }
}

/// Construct a synthetic homozygous-reference genotype at a target whose
/// reference base we know (from the catalog), used when the target falls inside a
/// gVCF reference block that has no explicit record at that exact position.
pub fn homozygous_ref(chrom: &str, pos: u64, ref_allele: &str) -> Genotype {
    let r = ref_allele.trim().to_ascii_uppercase();
    let (alleles, key) = if r.is_empty() {
        (Vec::new(), String::new())
    } else {
        (vec![r.clone(), r.clone()], norm_genotype(&format!("{r}{r}")))
    };
    Genotype {
        alleles,
        key,
        chrom: norm_chrom(chrom),
        pos,
        rsid: ".".to_string(),
        ref_allele: r.clone(),
        alt: ".".to_string(),
        no_call: false,
    }
}

/// A resolved genotype at one locus, as read from the file.
#[derive(Debug, Clone, Serialize)]
pub struct Genotype {
    /// Alleles actually called, e.g. `["A","G"]`. Empty if no call.
    pub alleles: Vec<String>,
    /// Sorted-joined key, e.g. `"AG"`. Empty when no call.
    pub key: String,
    pub chrom: String,
    pub pos: u64,
    /// The rsID(s) on the matched record, `;`-joined, or `"."`.
    pub rsid: String,
    #[serde(rename = "ref")]
    pub ref_allele: String,
    pub alt: String,
    /// True when the sample had no call (`./.`) at this locus.
    pub no_call: bool,
}

impl Genotype {
    /// How many of the called alleles equal `effect` (0/1/2). `None` if no call.
    pub fn dosage(&self, effect: &str) -> Option<u8> {
        if self.no_call || self.alleles.is_empty() {
            return None;
        }
        let e = effect.trim().to_ascii_uppercase();
        Some(self.alleles.iter().filter(|a| a.eq_ignore_ascii_case(&e)).count() as u8)
    }

    /// The set of distinct alleles observed at the site (REF plus every ALT),
    /// uppercased — used for strict allele-set matching in scoring.
    pub fn site_alleles(&self) -> Vec<String> {
        let mut v = vec![self.ref_allele.clone()];
        for a in self.alt.split(',') {
            let a = a.trim().to_ascii_uppercase();
            if !a.is_empty() && a != "." {
                v.push(a);
            }
        }
        v
    }
}

/// Resolve a record's GT into concrete alleles.
pub fn genotype_from_record(rec: &Record<'_>) -> Genotype {
    let alt_alleles: Vec<&str> = rec.alt.split(',').collect();
    let allele_at = |idx: usize| -> Option<String> {
        if idx == 0 {
            Some(rec.ref_allele.to_ascii_uppercase())
        } else {
            alt_alleles.get(idx - 1).map(|a| a.trim().to_ascii_uppercase())
        }
    };

    let mut alleles = Vec::new();
    let mut no_call = false;
    match rec.gt {
        Some(gt) => {
            for tok in gt.split(['/', '|']) {
                let tok = tok.trim();
                if tok == "." || tok.is_empty() {
                    no_call = true;
                    continue;
                }
                match tok.parse::<usize>() {
                    Ok(idx) => {
                        if let Some(a) = allele_at(idx) {
                            alleles.push(a);
                        }
                    }
                    Err(_) => no_call = true,
                }
            }
        }
        None => no_call = true,
    }
    if alleles.is_empty() {
        no_call = true;
    }

    let key = if alleles.is_empty() {
        String::new()
    } else {
        norm_genotype(&alleles.concat())
    };

    Genotype {
        alleles,
        key,
        chrom: norm_chrom(rec.chrom),
        pos: rec.pos,
        rsid: rec.ids.to_string(),
        ref_allele: rec.ref_allele.to_ascii_uppercase(),
        alt: rec.alt.to_ascii_uppercase(),
        no_call,
    }
}

/// A genome file on disk. Opening is cheap — the file is only read when scanned.
pub struct VcfSource {
    path: PathBuf,
}

impl VcfSource {
    pub fn open(path: impl Into<PathBuf>) -> VcfSource {
        VcfSource { path: path.into() }
    }

    /// Stream the file line by line. `visit` returns `false` to stop early.
    pub fn scan<F: FnMut(&Record<'_>) -> bool>(&self, mut visit: F) -> Result<()> {
        let file =
            File::open(&self.path).with_context(|| format!("opening {}", self.path.display()))?;
        let gz = MultiGzDecoder::new(file);
        let mut reader = BufReader::with_capacity(1 << 20, gz);
        let mut line = String::new();
        loop {
            line.clear();
            let n = reader
                .read_line(&mut line)
                .with_context(|| format!("reading {}", self.path.display()))?;
            if n == 0 {
                break; // EOF
            }
            let trimmed = line.trim_end_matches(['\n', '\r']);
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if let Some(rec) = parse_record(trimmed) {
                if !visit(&rec) {
                    break;
                }
            }
        }
        Ok(())
    }

    /// Look up a single variant by rsID (`rs123`) or position (`chr2:135851076`).
    /// Scans with an early exit on the first match.
    pub fn lookup(&self, query: &str) -> Result<Option<Genotype>> {
        let q = query.trim();
        if q.is_empty() {
            return Ok(None);
        }
        let want_rsid = q.to_ascii_lowercase();
        let want_pos = parse_locus_query(q);
        let mut hit: Option<Genotype> = None;
        self.scan(|rec| {
            let matches = rec.ids.split(';').any(|id| id.trim().eq_ignore_ascii_case(&want_rsid))
                || want_pos
                    .as_ref()
                    .is_some_and(|(c, p)| norm_chrom(rec.chrom) == *c && rec.pos == *p);
            if matches {
                hit = Some(genotype_from_record(rec));
                return false; // stop at first match
            }
            true
        })?;
        Ok(hit)
    }
}

/// Parse a `chr2:135851076` / `2:135851076` position query.
fn parse_locus_query(q: &str) -> Option<(String, u64)> {
    let (c, p) = q.rsplit_once(':')?;
    let pos: u64 = p.trim().parse().ok()?;
    Some((norm_chrom(c), pos))
}
