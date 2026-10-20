//! **Sleep-stage classifier**: a gradient-boosted tree ensemble for the 30 s stager
//! (182 features, 4 classes: deep, light, REM, wake).
//!
//! The bundled model is in `data/sleep_stage_gbt.bin` (800 rounds per class); another can be
//! loaded with `load_classifier`. A model is a flat blob read by [`SleepClassifier::from_bytes`].
//! The blob layout is documented in `docs/algorithms/staging-classifier.md`.
//!
//!
//! Semantics:
//!
//! * A feature is a float32. NaN is missing. A node sends `x <= t` to its `le` child and
//!   `x > t` to its `gt` child (the comparison against the float64 threshold equals the float32
//!   comparison against the largest float32 not above it); a missing value goes to the node's
//!   missing child.
//! * The margin of a class is the float64 sum of its leaf values, added in the order that a
//!   postfix program stored in the blob defines (the blob format fixes the association of the
//!   additions), plus the base margin.
//! * Probabilities: `e_c = exp(m_c - max m)`, `total = e_3 + (e_2 + (e_0 + e_1))`,
//!   `p_c = (e_c / total) as f32`. The `exp` is `f64::exp` of the build target, so the last
//!   bit of an intermediate float64 can differ between platforms and flip a float32 probability
//!   in rare cases.

use std::fmt;
use std::path::Path;

/// Number of input features.
pub const N_FEATURES: usize = 182;
/// Number of output classes, in the model's order: deep, light, REM, wake.
pub const N_CLASSES: usize = 4;

const MAGIC: &[u8; 4] = b"OSCF";
const VERSION: u32 = 1;
const HEADER_LEN: usize = 64;
const LEAF: u16 = 0x8000;
const ADD: u16 = 0xFFFF;

/// Why a blob was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClassifierError {
    /// The blob ends before a section does, or has bytes after the last one.
    Size,
    /// The first four bytes are not `OSCF`.
    BadMagic,
    /// Format version other than 1.
    UnsupportedVersion(u32),
    /// A header value or array entry is out of range (the message names it).
    Invalid(&'static str),
    /// The file could not be read.
    Io(String),
}

impl fmt::Display for ClassifierError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Size => write!(f, "sleep classifier blob has the wrong size"),
            Self::BadMagic => write!(f, "not a sleep classifier blob (bad magic)"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported sleep classifier blob version {v}"),
            Self::Invalid(what) => write!(f, "invalid sleep classifier blob: {what}"),
            Self::Io(e) => write!(f, "cannot read sleep classifier blob: {e}"),
        }
    }
}

impl std::error::Error for ClassifierError {}

/// A decision-tree ensemble with a postfix addition program.
#[derive(Debug, Clone)]
pub struct SleepClassifier {
    n_rounds: usize,
    base: f64,
    prog: Vec<u16>,
    stack_depth: usize,
    roots: Vec<u16>,
    node_off: Vec<u32>,
    leaf_off: Vec<u32>,
    feat: Vec<u8>,
    miss: Vec<u8>,
    thr: Vec<f32>,
    le: Vec<u16>,
    gt: Vec<u16>,
    /// `(global node index, child)` for nodes whose missing child is neither `le` nor `gt`.
    side: Vec<(u32, u16)>,
    leaf: Vec<f64>,
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize, width: usize) -> Result<&'a [u8], ClassifierError> {
        let len = count.checked_mul(width).ok_or(ClassifierError::Size)?;
        let end = self.pos.checked_add(len).ok_or(ClassifierError::Size)?;
        let s = self.data.get(self.pos..end).ok_or(ClassifierError::Size)?;
        self.pos = end;
        Ok(s)
    }
    fn u8s(&mut self, n: usize) -> Result<Vec<u8>, ClassifierError> {
        Ok(self.take(n, 1)?.to_vec())
    }
    fn u16s(&mut self, n: usize) -> Result<Vec<u16>, ClassifierError> {
        Ok(self.take(n, 2)?.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect())
    }
    fn u32s(&mut self, n: usize) -> Result<Vec<u32>, ClassifierError> {
        Ok(self.take(n, 4)?.chunks_exact(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect())
    }
    fn f32s(&mut self, n: usize) -> Result<Vec<f32>, ClassifierError> {
        Ok(self.take(n, 4)?.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect())
    }
    fn f64s(&mut self, n: usize) -> Result<Vec<f64>, ClassifierError> {
        Ok(self
            .take(n, 8)?
            .chunks_exact(8)
            .map(|b| f64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
            .collect())
    }
}

fn hdr_u32(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]])
}

impl SleepClassifier {
    /// Parse and validate a blob. Validation guarantees that [`Self::margins`] terminates and
    /// does not index out of range: children have larger indices than their parents, every
    /// reference is inside its tree, and the addition program pushes each round once.
    pub fn from_bytes(data: &[u8]) -> Result<Self, ClassifierError> {
        if data.len() < HEADER_LEN {
            return Err(ClassifierError::Size);
        }
        if &data[..4] != MAGIC {
            return Err(ClassifierError::BadMagic);
        }
        let version = hdr_u32(data, 4);
        if version != VERSION {
            return Err(ClassifierError::UnsupportedVersion(version));
        }
        if hdr_u32(data, 8) as usize != N_FEATURES {
            return Err(ClassifierError::Invalid("feature count"));
        }
        if hdr_u32(data, 12) as usize != N_CLASSES {
            return Err(ClassifierError::Invalid("class count"));
        }
        let n_rounds = hdr_u32(data, 16) as usize;
        let n_nodes = hdr_u32(data, 20) as usize;
        let n_leaves = hdr_u32(data, 24) as usize;
        let n_prog = hdr_u32(data, 28) as usize;
        let n_side = hdr_u32(data, 32) as usize;
        let base = f64::from_le_bytes(data[36..44].try_into().unwrap());
        if n_rounds == 0 || n_rounds > 0x7FFF || n_rounds.checked_mul(N_CLASSES).is_none() {
            return Err(ClassifierError::Invalid("round count"));
        }
        let n_trees = n_rounds * N_CLASSES;
        let mut r = Reader { data, pos: HEADER_LEN };
        let prog = r.u16s(n_prog)?;
        let roots = r.u16s(n_trees)?;
        let node_off = r.u32s(n_trees + 1)?;
        let leaf_off = r.u32s(n_trees + 1)?;
        let feat = r.u8s(n_nodes)?;
        let miss = r.u8s(n_nodes)?;
        let thr = r.f32s(n_nodes)?;
        let le = r.u16s(n_nodes)?;
        let gt = r.u16s(n_nodes)?;
        let side_node = r.u32s(n_side)?;
        let side_child = r.u16s(n_side)?;
        let leaf = r.f64s(n_leaves)?;
        if r.pos != data.len() {
            return Err(ClassifierError::Size);
        }
        let side: Vec<(u32, u16)> = side_node.into_iter().zip(side_child).collect();
        let stack_depth = Self::validate_program(&prog, n_rounds)?;
        let c = SleepClassifier { n_rounds, base, prog, stack_depth, roots, node_off, leaf_off, feat, miss, thr, le, gt, side, leaf };
        c.validate_trees(n_nodes, n_leaves)?;
        Ok(c)
    }

    /// Read and parse a blob file.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, ClassifierError> {
        let data = std::fs::read(path).map_err(|e| ClassifierError::Io(e.to_string()))?;
        Self::from_bytes(&data)
    }

    fn validate_program(prog: &[u16], n_rounds: usize) -> Result<usize, ClassifierError> {
        let mut seen = vec![false; n_rounds];
        let (mut depth, mut max_depth) = (0usize, 0usize);
        for &tok in prog {
            if tok == ADD {
                if depth < 2 {
                    return Err(ClassifierError::Invalid("addition program underflow"));
                }
                depth -= 1;
            } else {
                let t = tok as usize;
                if t >= n_rounds || seen[t] {
                    return Err(ClassifierError::Invalid("addition program round"));
                }
                seen[t] = true;
                depth += 1;
                max_depth = max_depth.max(depth);
            }
        }
        if depth != 1 || seen.iter().any(|s| !s) {
            return Err(ClassifierError::Invalid("addition program shape"));
        }
        Ok(max_depth)
    }

    fn validate_trees(&self, n_nodes: usize, n_leaves: usize) -> Result<(), ClassifierError> {
        let bad = ClassifierError::Invalid;
        if self.node_off[0] != 0 || self.leaf_off[0] != 0 {
            return Err(bad("offsets start"));
        }
        if self.node_off.last().copied() != Some(n_nodes as u32) || self.leaf_off.last().copied() != Some(n_leaves as u32) {
            return Err(bad("offsets end"));
        }
        if self.side.windows(2).any(|w| w[0].0 >= w[1].0) {
            return Err(bad("side table order"));
        }
        let mut side_used = 0usize;
        for t in 0..self.roots.len() {
            let (n0, n1) = (self.node_off[t] as usize, self.node_off[t + 1] as usize);
            let (l0, l1) = (self.leaf_off[t] as usize, self.leaf_off[t + 1] as usize);
            if n1 < n0 || l1 < l0 || n1 > n_nodes || l1 > n_leaves {
                return Err(bad("tree offsets"));
            }
            let (nn, nl) = (n1 - n0, l1 - l0);
            // a reference at node `i` (None: a root) must stay inside the tree and point forward
            let ok = |r: u16, i: Option<usize>| -> bool {
                if r & LEAF != 0 {
                    ((r & !LEAF) as usize) < nl
                } else {
                    let r = r as usize;
                    r < nn && i.map_or(true, |i| r > i)
                }
            };
            if !ok(self.roots[t], None) {
                return Err(bad("root"));
            }
            for i in 0..nn {
                let g = n0 + i;
                if self.feat[g] as usize >= N_FEATURES {
                    return Err(bad("feature"));
                }
                if !ok(self.le[g], Some(i)) || !ok(self.gt[g], Some(i)) {
                    return Err(bad("child"));
                }
                match self.miss[g] {
                    0 | 1 => {}
                    2 => match self.side.binary_search_by_key(&(g as u32), |s| s.0) {
                        Ok(k) if ok(self.side[k].1, Some(i)) => side_used += 1,
                        _ => return Err(bad("missing child")),
                    },
                    _ => return Err(bad("missing kind")),
                }
            }
        }
        if side_used != self.side.len() {
            return Err(bad("unused side entry"));
        }
        Ok(())
    }

    /// Number of boosting rounds (trees per class).
    pub fn n_rounds(&self) -> usize {
        self.n_rounds
    }

    /// Total number of decision nodes.
    pub fn n_nodes(&self) -> usize {
        self.feat.len()
    }

    fn tree_value(&self, t: usize, x: &[f32; N_FEATURES]) -> f64 {
        let nb = self.node_off[t] as usize;
        let lb = self.leaf_off[t] as usize;
        let mut r = self.roots[t];
        while r & LEAF == 0 {
            let g = nb + r as usize;
            let v = x[self.feat[g] as usize];
            r = if v.is_nan() {
                match self.miss[g] {
                    0 => self.le[g],
                    1 => self.gt[g],
                    _ => match self.side.binary_search_by_key(&(g as u32), |s| s.0) {
                        Ok(k) => self.side[k].1,
                        Err(_) => unreachable!("validated"),
                    },
                }
            } else if v <= self.thr[g] {
                self.le[g]
            } else {
                self.gt[g]
            };
        }
        self.leaf[lb + (r & !LEAF) as usize]
    }

    /// Per-class sums of leaf values, added in the order of the blob's program. NaN entries are missing values.
    pub fn margins(&self, features: &[f32; N_FEATURES]) -> [f64; N_CLASSES] {
        let mut out = [0.0f64; N_CLASSES];
        let mut stack: Vec<f64> = Vec::with_capacity(self.stack_depth);
        for (c, m) in out.iter_mut().enumerate() {
            stack.clear();
            for &tok in &self.prog {
                if tok == ADD {
                    let b = stack.pop().unwrap_or(0.0);
                    let a = stack.pop().unwrap_or(0.0);
                    stack.push(a + b);
                } else {
                    stack.push(self.tree_value(c * self.n_rounds + tok as usize, features));
                }
            }
            *m = stack.pop().unwrap_or(0.0) + self.base;
        }
        out
    }

    /// Class probabilities as the op returns them: softmax of the margins in float64
    /// (maximum subtracted, `e_3 + (e_2 + (e_0 + e_1))` for the denominator), cast to float32.
    pub fn predict_proba(&self, features: &[f32; N_FEATURES]) -> [f32; N_CLASSES] {
        softmax(&self.margins(features))
    }
}

/// Softmax of four margins in float64.
pub fn softmax(s: &[f64; N_CLASSES]) -> [f32; N_CLASSES] {
    let mx = s[0].max(s[1]).max(s[2]).max(s[3]);
    let e = [(s[0] - mx).exp(), (s[1] - mx).exp(), (s[2] - mx).exp(), (s[3] - mx).exp()];
    let total = e[3] + (e[2] + (e[0] + e[1]));
    [(e[0] / total) as f32, (e[1] / total) as f32, (e[2] / total) as f32, (e[3] / total) as f32]
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- a hand-built blob -------------------------------------------------------------

    #[derive(Clone, Copy)]
    enum Miss {
        Le,
        Gt,
        Side(u16),
    }

    struct Node {
        feat: u8,
        thr: f32,
        le: u16,
        gt: u16,
        miss: Miss,
    }

    struct Tree {
        nodes: Vec<Node>,
        root: u16,
        leaves: Vec<f64>,
    }

    fn leaf(i: u16) -> u16 {
        LEAF | i
    }

    fn constant(v: f64) -> Tree {
        Tree { nodes: vec![], root: leaf(0), leaves: vec![v] }
    }

    fn stump(feat: u8, thr: f32, lo: f64, hi: f64, miss: Miss) -> Tree {
        Tree { nodes: vec![Node { feat, thr, le: leaf(0), gt: leaf(1), miss }], root: 0, leaves: vec![lo, hi] }
    }

    fn build_blob(trees: &[Tree], n_rounds: usize, prog: &[u16], base: f64) -> Vec<u8> {
        assert_eq!(trees.len(), n_rounds * N_CLASSES);
        let (mut node_off, mut leaf_off) = (vec![0u32], vec![0u32]);
        let (mut feat, mut miss, mut thr, mut le, mut gt) = (vec![], vec![], vec![], vec![], vec![]);
        let (mut side_node, mut side_child, mut leaves) = (vec![], vec![], vec![]);
        for t in trees {
            let base_node = *node_off.last().unwrap();
            for (i, n) in t.nodes.iter().enumerate() {
                feat.push(n.feat);
                thr.push(n.thr);
                le.push(n.le);
                gt.push(n.gt);
                match n.miss {
                    Miss::Le => miss.push(0u8),
                    Miss::Gt => miss.push(1u8),
                    Miss::Side(c) => {
                        miss.push(2u8);
                        side_node.push(base_node + i as u32);
                        side_child.push(c);
                    }
                }
            }
            node_off.push(base_node + t.nodes.len() as u32);
            leaves.extend_from_slice(&t.leaves);
            leaf_off.push(leaves.len() as u32);
        }
        let mut b = Vec::new();
        b.extend_from_slice(MAGIC);
        for v in [VERSION, N_FEATURES as u32, N_CLASSES as u32, n_rounds as u32, feat.len() as u32, leaves.len() as u32, prog.len() as u32, side_node.len() as u32] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&base.to_le_bytes());
        b.resize(HEADER_LEN, 0);
        for v in prog {
            b.extend_from_slice(&v.to_le_bytes());
        }
        for t in trees {
            b.extend_from_slice(&t.root.to_le_bytes());
        }
        for v in node_off.iter().chain(leaf_off.iter()) {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&feat);
        b.extend_from_slice(&miss);
        for v in &thr {
            b.extend_from_slice(&v.to_le_bytes());
        }
        for v in le.iter().chain(gt.iter()) {
            b.extend_from_slice(&v.to_le_bytes());
        }
        for v in &side_node {
            b.extend_from_slice(&v.to_le_bytes());
        }
        for v in &side_child {
            b.extend_from_slice(&v.to_le_bytes());
        }
        for v in &leaves {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b
    }

    /// Three rounds per class; the program computes `v0 + (v1 + v2)`.
    fn synthetic() -> (SleepClassifier, Vec<u8>) {
        let two_level = Tree {
            // node 0: feature 5 <= 0.5 -> leaf 0, else node 1 (missing goes to the gt child)
            // node 1: feature 7 <= -1 -> leaf 1, else leaf 2 (missing goes to leaf 0)
            nodes: vec![
                Node { feat: 5, thr: 0.5, le: leaf(0), gt: 1, miss: Miss::Gt },
                Node { feat: 7, thr: -1.0, le: leaf(1), gt: leaf(2), miss: Miss::Side(leaf(0)) },
            ],
            root: 0,
            leaves: vec![1.0, 2.0, 4.0],
        };
        // feature 9 only separates missing values: threshold +inf, both children the same leaf
        let missing_only = Tree {
            nodes: vec![Node { feat: 9, thr: f32::INFINITY, le: leaf(0), gt: leaf(0), miss: Miss::Side(leaf(1)) }],
            root: 0,
            leaves: vec![0.5, -0.5],
        };
        let trees = vec![
            // class 0: 1 + (1 + 1e16) differs from (1 + 1) + 1e16 in float64
            constant(1.0),
            constant(1.0),
            constant(1e16),
            // class 1
            two_level,
            missing_only,
            constant(0.0625),
            // class 2
            stump(0, 0.0, 0.125, 0.375, Miss::Le),
            constant(-0.25),
            stump(1, 3.0, -1.0, 1.0, Miss::Gt),
            // class 3
            constant(0.5),
            constant(0.25),
            constant(0.125),
        ];
        let blob = build_blob(&trees, 3, &[0, 1, 2, ADD, ADD], 0.0);
        (SleepClassifier::from_bytes(&blob).expect("synthetic blob"), blob)
    }

    fn row(set: &[(usize, f32)]) -> [f32; N_FEATURES] {
        let mut x = [0.0f32; N_FEATURES];
        for &(i, v) in set {
            x[i] = v;
        }
        x
    }

    #[test]
    fn synthetic_walks_and_addition_order() {
        let (clf, _) = synthetic();
        let sum = |a: f64, b: f64, c: f64| a + (b + c);
        // class 0 is a constant: the program's association is the order of the additions
        assert_eq!(clf.margins(&row(&[]))[0], 1e16);
        assert_ne!(clf.margins(&row(&[]))[0], (1.0 + 1.0) + 1e16);
        let cases: Vec<([f32; N_FEATURES], f64, f64, f64)> = vec![
            // (row, class 1 margin, class 2 margin, class 3 margin)
            (row(&[]), sum(1.0, 0.5, 0.0625), sum(0.125, -0.25, -1.0), sum(0.5, 0.25, 0.125)),
            // feature 5 above 0.5 goes to node 1; feature 7 equal to the threshold goes le
            (row(&[(5, 0.500_000_1), (7, -1.0)]), sum(2.0, 0.5, 0.0625), sum(0.125, -0.25, -1.0), sum(0.5, 0.25, 0.125)),
            (row(&[(5, 1.0), (7, -0.999_999_9)]), sum(4.0, 0.5, 0.0625), sum(0.125, -0.25, -1.0), sum(0.5, 0.25, 0.125)),
            // missing feature 5 goes to the gt child (node 1); missing feature 7 goes to leaf 0
            (row(&[(5, f32::NAN), (7, f32::NAN)]), sum(1.0, 0.5, 0.0625), sum(0.125, -0.25, -1.0), sum(0.5, 0.25, 0.125)),
            // missing feature 9 takes the side child of the missing-only node
            (row(&[(9, f32::NAN)]), sum(1.0, -0.5, 0.0625), sum(0.125, -0.25, -1.0), sum(0.5, 0.25, 0.125)),
            // thresholds: equal goes le (also -0.0 and -inf); +inf and tiny positives go gt
            (row(&[(0, -0.0)]), sum(1.0, 0.5, 0.0625), sum(0.125, -0.25, -1.0), sum(0.5, 0.25, 0.125)),
            (row(&[(0, f32::NEG_INFINITY), (1, 3.0)]), sum(1.0, 0.5, 0.0625), sum(0.125, -0.25, -1.0), sum(0.5, 0.25, 0.125)),
            (row(&[(0, 1e-30), (1, 3.000_000_2)]), sum(1.0, 0.5, 0.0625), sum(0.375, -0.25, 1.0), sum(0.5, 0.25, 0.125)),
            (row(&[(0, f32::INFINITY), (1, f32::INFINITY)]), sum(1.0, 0.5, 0.0625), sum(0.375, -0.25, 1.0), sum(0.5, 0.25, 0.125)),
            // feature 0 missing goes le; feature 1 missing goes gt
            (row(&[(0, f32::NAN), (1, f32::NAN)]), sum(1.0, 0.5, 0.0625), sum(0.125, -0.25, 1.0), sum(0.5, 0.25, 0.125)),
        ];
        for (i, (x, m1, m2, m3)) in cases.iter().enumerate() {
            let m = clf.margins(x);
            assert_eq!(m[0], 1e16, "case {i}");
            assert_eq!(m[1].to_bits(), m1.to_bits(), "case {i} class 1");
            assert_eq!(m[2].to_bits(), m2.to_bits(), "case {i} class 2");
            assert_eq!(m[3].to_bits(), m3.to_bits(), "case {i} class 3");
        }
    }

    /// A blob shaped like a small model: five rounds per class, a left-fold addition program
    /// and a non-zero base margin.
    #[test]
    fn left_fold_program_with_other_round_count() {
        let rounds = 5usize;
        let mut trees = Vec::new();
        for c in 0..N_CLASSES {
            for r in 0..rounds {
                let v = 0.1 * (c as f64 + 1.0) + 0.01 * r as f64;
                trees.push(stump(c as u8, r as f32 - 2.0, v, -v, if r % 2 == 0 { Miss::Le } else { Miss::Gt }));
            }
        }
        let mut prog = vec![0u16];
        for r in 1..rounds as u16 {
            prog.extend([r, ADD]);
        }
        let blob = build_blob(&trees, rounds, &prog, 0.25);
        let clf = SleepClassifier::from_bytes(&blob).expect("left-fold blob");
        assert_eq!(clf.n_rounds(), rounds);
        let x = row(&[(0, 0.5), (1, f32::NAN), (2, -3.0), (3, 10.0)]);
        let m = clf.margins(&x);
        for c in 0..N_CLASSES {
            let mut want = 0.0f64;
            for r in 0..rounds {
                let v = 0.1 * (c as f64 + 1.0) + 0.01 * r as f64;
                let t = r as f32 - 2.0;
                let go_le = if x[c].is_nan() { r % 2 == 0 } else { x[c] <= t };
                let leaf = if go_le { v } else { -v };
                want = if r == 0 { leaf } else { want + leaf };
            }
            assert_eq!(m[c], want + 0.25, "class {c}");
        }
    }

    #[test]
    fn softmax_matches_definition() {
        let (clf, _) = synthetic();
        let m = clf.margins(&row(&[]));
        let mx = m.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let e: Vec<f64> = m.iter().map(|v| (v - mx).exp()).collect();
        let total = e[3] + (e[2] + (e[0] + e[1]));
        let want: Vec<f32> = e.iter().map(|v| (v / total) as f32).collect();
        assert_eq!(clf.predict_proba(&row(&[])).to_vec(), want);
        // class 0 dominates by 1e16, so the other three round to zero
        assert_eq!(want, vec![1.0f32, 0.0, 0.0, 0.0]);
        let p = softmax(&[0.0, 1.0, 2.0, 3.0]);
        let sum: f32 = p.iter().sum();
        assert!((sum - 1.0).abs() < 1e-6 && p[3] > p[2] && p[2] > p[1] && p[1] > p[0]);
    }

    #[test]
    fn rejects_malformed_blobs() {
        let (_, blob) = synthetic();
        assert_eq!(SleepClassifier::from_bytes(&blob[..10]).unwrap_err(), ClassifierError::Size);
        assert_eq!(SleepClassifier::from_bytes(&blob[..blob.len() - 1]).unwrap_err(), ClassifierError::Size);
        let mut longer = blob.clone();
        longer.push(0);
        assert_eq!(SleepClassifier::from_bytes(&longer).unwrap_err(), ClassifierError::Size);
        let mut bad = blob.clone();
        bad[0] = b'X';
        assert_eq!(SleepClassifier::from_bytes(&bad).unwrap_err(), ClassifierError::BadMagic);
        let mut bad = blob.clone();
        bad[4] = 2;
        assert_eq!(SleepClassifier::from_bytes(&bad).unwrap_err(), ClassifierError::UnsupportedVersion(2));
        // a child that points back to its parent would loop forever
        let prog_len = 5 * 2;
        let roots_len = 12 * 2;
        let offs_len = 2 * 13 * 4;
        let nodes = HEADER_LEN + prog_len + roots_len + offs_len;
        let n_nodes = 5;
        let le_at = nodes + n_nodes * (1 + 1 + 4);
        let mut bad = blob.clone();
        // node 1 of tree 3 (global node 1): le = 0
        bad[le_at + 2] = 0;
        bad[le_at + 3] = 0;
        assert!(matches!(SleepClassifier::from_bytes(&bad), Err(ClassifierError::Invalid("child"))));
        // a feature index past the end
        let mut bad = blob.clone();
        bad[nodes] = 200;
        assert!(matches!(SleepClassifier::from_bytes(&bad), Err(ClassifierError::Invalid("feature"))));
        // addition program that repeats a round
        let mut bad = blob;
        bad[HEADER_LEN + 2] = 0;
        assert!(matches!(SleepClassifier::from_bytes(&bad), Err(ClassifierError::Invalid("addition program round"))));
    }

}
