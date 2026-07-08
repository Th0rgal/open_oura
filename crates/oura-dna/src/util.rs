//! Allele / locus string normalisation shared across the crate.

/// `chr2` / `Chr2` / `2` all normalise to `2`; `chrM`/`MT`/`M` to `mt`.
pub fn norm_chrom(chrom: &str) -> String {
    let c = chrom
        .trim()
        .trim_start_matches("chr")
        .trim_start_matches("Chr")
        .trim_start_matches("CHR");
    if c.eq_ignore_ascii_case("m") || c.eq_ignore_ascii_case("mt") {
        return "mt".to_string();
    }
    c.to_ascii_uppercase()
}

/// `"2:135851076"` — the normalised chrom:pos key used for position matching.
pub fn pos_key(chrom: &str, pos: u64) -> String {
    format!("{}:{}", norm_chrom(chrom), pos)
}

/// Sorted-uppercase genotype key: `"GA"` → `"AG"`, single allele stays as-is.
pub fn norm_genotype(g: &str) -> String {
    let mut chars: Vec<char> = g
        .trim()
        .to_ascii_uppercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    chars.sort_unstable();
    chars.into_iter().collect()
}

/// Reverse-complement a single-base allele (`A↔T`, `C↔G`). Non-SNP / indel
/// alleles (length ≠ 1 or non-ACGT) are returned unchanged, which keeps them out
/// of strand-flip matching (they can only match directly).
pub fn complement(allele: &str) -> String {
    if allele.len() != 1 {
        return allele.to_ascii_uppercase();
    }
    match allele.to_ascii_uppercase().as_str() {
        "A" => "T",
        "T" => "A",
        "C" => "G",
        "G" => "C",
        other => other,
    }
    .to_string()
}

/// Reverse-complement a genotype key (`"AA"` → `"TT"`, `"AG"` → `"CT"`), keeping
/// the sorted-key normalisation. Used to reconcile a catalog trait whose alleles
/// are written on the opposite strand from the reference-forward-strand VCF.
pub fn rc_genotype(key: &str) -> String {
    let flipped: String = key
        .chars()
        .map(|c| match c.to_ascii_uppercase() {
            'A' => 'T',
            'T' => 'A',
            'C' => 'G',
            'G' => 'C',
            other => other,
        })
        .collect();
    norm_genotype(&flipped)
}

/// An A/T or C/G SNP is strand-ambiguous ("palindromic"): its two alleles are
/// each other's complement, so a strand flip is unresolvable from the alleles
/// alone. Full-rigor scoring refuses to flip these.
pub fn is_palindromic(a: &str, b: &str) -> bool {
    let (a, b) = (a.to_ascii_uppercase(), b.to_ascii_uppercase());
    matches!(
        (a.as_str(), b.as_str()),
        ("A", "T") | ("T", "A") | ("C", "G") | ("G", "C")
    )
}
