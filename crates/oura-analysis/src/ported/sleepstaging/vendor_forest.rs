//! Reader for the vendor sleep-stager forest (file format `NSSAFOR1`).
//!
//! The vendor classifier is a gradient-boosted forest of 2000 trees (4 classes, 182 features)
//! split into 63 code units. The file is not part of this repository and is loaded at run time.
//! [`VendorForest`] implements [`StageClassifier`], so [`stage_night_forest`] runs the same
//! pipeline as [`super::pipeline::stage_night_with`] (wake-trim rerun, smoothing, grid and
//! false-nap check) with the forest in place of the bundled classifier.
//!
//! File layout, little endian:
//!
//! - header: magic `NSSAFOR1`, then `u32` class count, feature count, node count, tree count,
//!   unit count and op count (32 bytes)
//! - one `u32` root node index per tree
//! - 24-byte nodes: `u8` kind (0 leaf, 1 missing test, 2 compare, 3 compare with a missing
//!   branch), `u8` compare code (0 goes to the yes child when `x <= t`, 1 when `x > t` or the
//!   comparison is unordered), `u8` missing-goes-yes, one pad byte, `i32` feature, `f64`
//!   threshold (the value for a leaf), `i32` yes child, `i32` no child
//! - per unit and class, `(u32 first op, u32 op count)`
//! - `u32` ops: `0 << 30 | tree` pushes the tree's value, `1 << 30 | k` pushes the result of
//!   class `k` of the previous unit, `2 << 30` adds the top two stack entries
//!
//! Each unit evaluates one program per class from the previous unit's results. Additions run
//! in the stored order and the softmax follows the vendor operation order, so probabilities
//! equal the vendor op's bit for bit.

use super::classifier::{StageClassifier, STAGER_CLASSES, STAGER_FEATURES};
use super::pipeline::{stage_night_with, Inputs, Output, StagingError};
use crate::ported::sleep_classifier::ClassifierError;
use std::path::Path;

const MAGIC: &[u8; 8] = b"NSSAFOR1";
const HEADER_LEN: usize = 32;
const NODE_LEN: usize = 24;
/// Deepest value stack a program may need; the evaluator keeps it on the call stack.
const MAX_STACK: usize = 64;

const LEAF: u8 = 0;
const MISSING: u8 = 1;
const TEST: u8 = 3;

const OP_TREE: u32 = 0;
const OP_RES: u32 = 1;
const OP_ADD: u32 = 2;
const OP_ARG: u32 = 0x3FFF_FFFF;

/// One tree node, 24 bytes. A leaf keeps its value in `thr`.
#[derive(Clone, Copy)]
struct Node {
    thr: f64,
    yes: u32,
    no: u32,
    feature: u16,
    kind: u8,
    /// Bit 0: compare code, bit 1: missing goes to the yes child.
    flags: u8,
}

/// The vendor forest, validated at load time.
pub struct VendorForest {
    roots: Vec<u32>,
    nodes: Vec<Node>,
    /// `(first op, op count)` per unit and class, unit-major.
    programs: Vec<(u32, u32)>,
    ops: Vec<u32>,
    n_units: usize,
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn i32_at(b: &[u8], at: usize) -> i32 {
    u32_at(b, at) as i32
}

impl VendorForest {
    /// Reads and validates a forest file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ClassifierError> {
        let data = std::fs::read(path).map_err(|e| ClassifierError::Io(e.to_string()))?;
        Self::from_bytes(&data)
    }

    /// Parses and validates a forest. Any malformed input returns an error; nothing panics.
    pub fn from_bytes(b: &[u8]) -> Result<Self, ClassifierError> {
        if b.len() < HEADER_LEN {
            return Err(ClassifierError::Size);
        }
        if &b[..8] != MAGIC {
            return Err(ClassifierError::BadMagic);
        }
        if u32_at(b, 8) as usize != STAGER_CLASSES {
            return Err(ClassifierError::Invalid("class count differs from the stager's"));
        }
        if u32_at(b, 12) as usize != STAGER_FEATURES {
            return Err(ClassifierError::Invalid("feature count differs from the stager's"));
        }
        let n_nodes = u32_at(b, 16) as usize;
        let n_trees = u32_at(b, 20) as usize;
        let n_units = u32_at(b, 24) as usize;
        let n_ops = u32_at(b, 28) as usize;
        if n_nodes >= 1 << 31 || n_trees >= 1 << 30 {
            return Err(ClassifierError::Invalid("node or tree count out of range"));
        }

        let n_progs = n_units.checked_mul(STAGER_CLASSES).ok_or(ClassifierError::Size)?;
        let expect = [(n_trees, 4), (n_nodes, NODE_LEN), (n_progs, 8), (n_ops, 4)]
            .iter()
            .try_fold(HEADER_LEN, |acc, &(n, w)| n.checked_mul(w).and_then(|s| acc.checked_add(s)))
            .ok_or(ClassifierError::Size)?;
        if b.len() != expect {
            return Err(ClassifierError::Size);
        }
        if n_trees == 0 || n_units == 0 {
            return Err(ClassifierError::Invalid("no trees or no units"));
        }

        let mut at = HEADER_LEN;
        let roots: Vec<u32> = (0..n_trees).map(|i| u32_at(b, at + 4 * i)).collect();
        at += 4 * n_trees;
        if roots.iter().any(|&r| r as usize >= n_nodes) {
            return Err(ClassifierError::Invalid("tree root out of range"));
        }

        let mut nodes = Vec::with_capacity(n_nodes);
        for i in 0..n_nodes {
            let p = at + NODE_LEN * i;
            let (kind, cc, miss_yes) = (b[p], b[p + 1], b[p + 2]);
            let feature = i32_at(b, p + 4);
            let (yes, no) = (i32_at(b, p + 16), i32_at(b, p + 20));
            let thr = f64::from_le_bytes(b[p + 8..p + 16].try_into().unwrap());
            if kind > TEST || cc > 1 || miss_yes > 1 {
                return Err(ClassifierError::Invalid("node kind or flag out of range"));
            }
            if kind != LEAF {
                if feature < 0 || feature as usize >= STAGER_FEATURES {
                    return Err(ClassifierError::Invalid("node feature out of range"));
                }
                if yes < 0 || yes as usize >= n_nodes || no < 0 || no as usize >= n_nodes {
                    return Err(ClassifierError::Invalid("node child out of range"));
                }
            }
            let (feature, yes, no) = if kind == LEAF { (0, 0, 0) } else { (feature as u16, yes as u32, no as u32) };
            nodes.push(Node { thr, yes, no, feature, kind, flags: cc | (miss_yes << 1) });
        }
        at += NODE_LEN * n_nodes;
        check_acyclic(&roots, &nodes)?;

        let programs: Vec<(u32, u32)> =
            (0..n_progs).map(|i| (u32_at(b, at + 8 * i), u32_at(b, at + 8 * i + 4))).collect();
        at += 8 * n_progs;
        let ops: Vec<u32> = (0..n_ops).map(|i| u32_at(b, at + 4 * i)).collect();
        for &(first, n) in &programs {
            let end = (first as usize).checked_add(n as usize).filter(|&e| e <= n_ops);
            let Some(end) = end else {
                return Err(ClassifierError::Invalid("program outside the op array"));
            };
            check_program(&ops[first as usize..end], n_trees)?;
        }
        Ok(Self { roots, nodes, programs, ops, n_units })
    }

    /// Number of trees.
    pub fn n_trees(&self) -> usize {
        self.roots.len()
    }

    /// Number of nodes over all trees.
    pub fn n_nodes(&self) -> usize {
        self.nodes.len()
    }

    /// Number of code units.
    pub fn n_units(&self) -> usize {
        self.n_units
    }

    /// Value of one tree on a feature row.
    fn tree_value(&self, tree: usize, x: &[f32; STAGER_FEATURES]) -> f64 {
        let mut i = self.roots[tree] as usize;
        loop {
            let n = &self.nodes[i];
            if n.kind == LEAF {
                return n.thr;
            }
            let v = x[n.feature as usize];
            let missing = v.is_nan();
            // `le` is false for NaN, like the vendor's ordered compare.
            let le = f64::from(v) <= n.thr;
            let cond = if n.flags & 1 == 0 { le } else { !le };
            let take_yes = match n.kind {
                MISSING => missing,
                TEST if missing => n.flags & 2 != 0,
                _ => cond,
            };
            i = if take_yes { n.yes } else { n.no } as usize;
        }
    }
}

/// Rejects node graphs with a cycle reachable from a root, which would loop forever.
fn check_acyclic(roots: &[u32], nodes: &[Node]) -> Result<(), ClassifierError> {
    // 0 unvisited, 1 on the current path, 2 done
    let mut state = vec![0u8; nodes.len()];
    let mut stack: Vec<(u32, bool)> = Vec::new();
    for &r in roots {
        stack.push((r, false));
        while let Some((i, leaving)) = stack.pop() {
            let s = &mut state[i as usize];
            if leaving {
                *s = 2;
                continue;
            }
            match *s {
                2 => continue,
                1 => return Err(ClassifierError::Invalid("tree has a cycle")),
                _ => *s = 1,
            }
            stack.push((i, true));
            let n = &nodes[i as usize];
            if n.kind != LEAF {
                stack.push((n.yes, false));
                stack.push((n.no, false));
            }
        }
    }
    Ok(())
}

/// Checks that a program is a well-formed stack program that leaves one value.
fn check_program(ops: &[u32], n_trees: usize) -> Result<(), ClassifierError> {
    let mut depth = 0usize;
    for &op in ops {
        let arg = (op & OP_ARG) as usize;
        match op >> 30 {
            OP_TREE if arg < n_trees => depth += 1,
            OP_RES if arg < STAGER_CLASSES => depth += 1,
            OP_ADD if op & OP_ARG == 0 && depth >= 2 => depth -= 1,
            OP_TREE | OP_RES | OP_ADD => return Err(ClassifierError::Invalid("op out of range")),
            _ => return Err(ClassifierError::Invalid("unknown op")),
        }
        if depth > MAX_STACK {
            return Err(ClassifierError::Invalid("program stack too deep"));
        }
    }
    if depth != 1 {
        return Err(ClassifierError::Invalid("program does not leave one value"));
    }
    Ok(())
}

impl StageClassifier for VendorForest {
    fn predict_proba(&self, x: &[f32; STAGER_FEATURES]) -> [f32; STAGER_CLASSES] {
        softmax_vendor(&self.margins_of(x))
    }

    fn margins(&self, x: &[f32; STAGER_FEATURES]) -> Option<[f64; STAGER_CLASSES]> {
        Some(self.margins_of(x))
    }
}

impl VendorForest {
    fn margins_of(&self, x: &[f32; STAGER_FEATURES]) -> [f64; STAGER_CLASSES] {
        let mut res = [0f64; STAGER_CLASSES];
        // A tree used by several classes of one unit is evaluated once per unit.
        let mut value = vec![0f64; self.roots.len()];
        let mut stamp = vec![0u32; self.roots.len()];
        for u in 0..self.n_units {
            let mut next = [0f64; STAGER_CLASSES];
            for (k, out) in next.iter_mut().enumerate() {
                let (first, n) = self.programs[u * STAGER_CLASSES + k];
                let mut st = [0f64; MAX_STACK];
                let mut sp = 0;
                for &op in &self.ops[first as usize..(first + n) as usize] {
                    let arg = (op & OP_ARG) as usize;
                    match op >> 30 {
                        OP_TREE => {
                            if stamp[arg] != u as u32 + 1 {
                                value[arg] = self.tree_value(arg, x);
                                stamp[arg] = u as u32 + 1;
                            }
                            st[sp] = value[arg];
                            sp += 1;
                        }
                        OP_RES => {
                            st[sp] = res[arg];
                            sp += 1;
                        }
                        _ => {
                            st[sp - 2] += st[sp - 1];
                            sp -= 1;
                        }
                    }
                }
                *out = st[0];
            }
            res = next;
        }
        // The vendor op adds a global bias of 0.0 after the last unit.
        res.map(|v| v + 0.0)
    }
}

/// Softmax in the vendor operation order: strict-greater maximum, `((e0 + e1) + e2) + e3`.
fn softmax_vendor(m: &[f64; STAGER_CLASSES]) -> [f32; STAGER_CLASSES] {
    let mut mx = m[0];
    for &v in &m[1..] {
        if v > mx {
            mx = v;
        }
    }
    let e = m.map(|v| (v - mx).exp());
    let s = ((e[0] + e[1]) + e[2]) + e[3];
    e.map(|v| (v / s) as f32)
}

/// Stages a night with the vendor forest. Same flags as [`stage_night_with`]; the forest also
/// decides the wake-trim rerun and the false-nap check.
pub fn stage_night_forest(
    forest: &VendorForest,
    inp: &Inputs,
    rerun_after_long_wake: bool,
    false_nap_check: bool,
) -> Result<Output, StagingError> {
    stage_night_with(forest, inp, rerun_after_long_wake, false_nap_check)
}

/// Loads the forest at `path` and stages a night with it. Loading takes a few tens of
/// milliseconds for the vendor file, so callers staging several nights load once and use
/// [`stage_night_forest`].
pub fn stage_night_forest_file(
    path: impl AsRef<Path>,
    inp: &Inputs,
    rerun_after_long_wake: bool,
    false_nap_check: bool,
) -> Result<Output, ForestStagingError> {
    let forest = VendorForest::load(path).map_err(ForestStagingError::Forest)?;
    stage_night_forest(&forest, inp, rerun_after_long_wake, false_nap_check).map_err(ForestStagingError::Staging)
}

/// Failure of [`stage_night_forest_file`].
#[derive(Debug)]
pub enum ForestStagingError {
    /// The forest file could not be read or is malformed.
    Forest(ClassifierError),
    /// The night could not be staged.
    Staging(StagingError),
}

impl std::fmt::Display for ForestStagingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Forest(e) => e.fmt(f),
            Self::Staging(e) => write!(f, "{e:?}"),
        }
    }
}

impl std::error::Error for ForestStagingError {}

#[cfg(test)]
mod tests {
    use super::*;

    const COMPARE: u8 = 2;

    /// Builder for small forests in the file layout.
    #[derive(Default)]
    struct Spec {
        trees: Vec<Vec<[f64; 8]>>, // kind, cc, missyes, feature, thr, yes, no (local), unused
        units: Vec<[Vec<u32>; 4]>,
    }

    fn leaf(v: f64) -> [f64; 8] {
        [0.0, 0.0, 0.0, 0.0, v, 0.0, 0.0, 0.0]
    }
    fn node(kind: u8, cc: u8, my: u8, feat: usize, thr: f64, yes: u32, no: u32) -> [f64; 8] {
        [kind as f64, cc as f64, my as f64, feat as f64, thr, yes as f64, no as f64, 0.0]
    }
    const T: fn(u32) -> u32 = |t| t;
    const R: fn(u32) -> u32 = |k| (1 << 30) | k;
    const ADD: u32 = 2 << 30;

    fn encode(s: &Spec) -> Vec<u8> {
        let mut roots = Vec::new();
        let mut nodes: Vec<u8> = Vec::new();
        let mut n_nodes = 0u32;
        for t in &s.trees {
            roots.push(n_nodes);
            for n in t {
                let (kind, leafnode) = (n[0] as u8, n[0] == 0.0);
                nodes.extend_from_slice(&[kind, n[1] as u8, n[2] as u8, 0]);
                nodes.extend_from_slice(&(if leafnode { -1 } else { n[3] as i32 }).to_le_bytes());
                nodes.extend_from_slice(&n[4].to_le_bytes());
                let (y, no) = if leafnode { (-1, -1) } else { (n[5] as i32 + n_nodes as i32, n[6] as i32 + n_nodes as i32) };
                nodes.extend_from_slice(&y.to_le_bytes());
                nodes.extend_from_slice(&no.to_le_bytes());
            }
            n_nodes += t.len() as u32;
        }
        let (mut progs, mut ops) = (Vec::new(), Vec::<u32>::new());
        for u in &s.units {
            for p in u {
                progs.push((ops.len() as u32, p.len() as u32));
                ops.extend_from_slice(p);
            }
        }
        let mut b = Vec::new();
        b.extend_from_slice(MAGIC);
        for v in [4, STAGER_FEATURES as u32, n_nodes, s.trees.len() as u32, s.units.len() as u32, ops.len() as u32] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        for r in roots {
            b.extend_from_slice(&r.to_le_bytes());
        }
        b.extend_from_slice(&nodes);
        for (f, n) in progs {
            b.extend_from_slice(&f.to_le_bytes());
            b.extend_from_slice(&n.to_le_bytes());
        }
        for o in ops {
            b.extend_from_slice(&o.to_le_bytes());
        }
        b
    }

    /// Five trees over features 0 to 3 and two units. Unit 0 sums trees per class, unit 1 adds
    /// tree 4 to the unit 0 results of the same class and of class 0.
    fn spec() -> Spec {
        let t0 = vec![node(COMPARE, 0, 0, 0, 1.5, 1, 2), leaf(0.25), leaf(-0.5)]; // x0 <= 1.5
        let t1 = vec![node(COMPARE, 1, 0, 1, 0.0, 1, 2), leaf(1.0), leaf(2.0)]; // x1 > 0 or unordered
        let t2 = vec![node(MISSING, 0, 0, 2, 0.0, 1, 2), leaf(10.0), leaf(0.125)]; // x2 is NaN
        let t3 = vec![node(TEST, 0, 1, 3, 2.0, 1, 2), leaf(3.0), leaf(-3.0)]; // NaN goes yes
        let t4 = vec![
            node(TEST, 1, 0, 3, 0.5, 1, 4), // NaN goes no; else x3 > 0.5 -> yes
            node(COMPARE, 0, 0, 0, -1.0, 2, 3),
            leaf(0.5),
            leaf(0.75),
            leaf(-1.0),
        ];
        let u0 = [
            vec![T(0), T(1), ADD],
            vec![T(1), T(2), ADD, T(3), ADD],
            vec![T(0)],
            vec![T(3), T(2), ADD, T(0), ADD, T(1), ADD],
        ];
        let u1 = [
            vec![R(0), T(4), ADD],
            vec![R(1), R(0), ADD, T(4), ADD],
            vec![R(2)],
            vec![R(3), T(4), ADD],
        ];
        Spec { trees: vec![t0, t1, t2, t3, t4], units: vec![u0, u1] }
    }

    fn row(vals: &[(usize, f32)]) -> [f32; STAGER_FEATURES] {
        let mut x = [0f32; STAGER_FEATURES];
        for &(i, v) in vals {
            x[i] = v;
        }
        x
    }

    /// Straightforward reference for `spec()`.
    fn reference(x: &[f32; STAGER_FEATURES]) -> [f64; 4] {
        let le = |v: f32, t: f64| f64::from(v) <= t;
        let t0 = if le(x[0], 1.5) { 0.25 } else { -0.5 };
        let t1 = if !le(x[1], 0.0) { 1.0 } else { 2.0 };
        let t2 = if x[2].is_nan() { 10.0 } else { 0.125 };
        let t3 = if x[3].is_nan() || le(x[3], 2.0) { 3.0 } else { -3.0 };
        let t4 = if x[3].is_nan() || le(x[3], 0.5) {
            -1.0
        } else if le(x[0], -1.0) {
            0.5
        } else {
            0.75
        };
        let r0 = t0 + t1;
        let r1 = t1 + t2 + t3;
        let r2 = t0;
        let r3 = t3 + t2 + t0 + t1;
        [r0 + t4, (r1 + r0) + t4, r2, r3 + t4]
    }

    #[test]
    fn evaluates_all_node_and_op_types() {
        let f = VendorForest::from_bytes(&encode(&spec())).unwrap();
        assert_eq!((f.n_trees(), f.n_nodes(), f.n_units()), (5, 3 + 3 + 3 + 3 + 5, 2));
        let nan = f32::NAN;
        let rows = [
            row(&[]),
            row(&[(0, 2.0), (1, -1.0), (2, 1.0), (3, 1.0)]),
            row(&[(0, -2.0), (1, nan), (2, nan), (3, nan)]),
            row(&[(0, nan), (1, 0.0), (2, nan), (3, 3.0)]),
            row(&[(0, 1.5), (1, 1e-3), (3, 0.5)]),
            row(&[(0, -1.0), (3, 0.5000001)]),
        ];
        for x in &rows {
            let want = reference(x);
            assert_eq!(f.margins(x), Some(want), "{x:?}");
            let p = f.predict_proba(x);
            let mx = want.iter().cloned().fold(f64::MIN, f64::max);
            let e: Vec<f64> = want.iter().map(|v| (v - mx).exp()).collect();
            let s = ((e[0] + e[1]) + e[2]) + e[3];
            for k in 0..4 {
                assert_eq!(p[k], (e[k] / s) as f32);
            }
            assert!((p.iter().map(|&v| f64::from(v)).sum::<f64>() - 1.0).abs() < 1e-6);
        }
        assert_eq!(f.predict_proba_rows(&[rows[1], rows[2]].concat()), vec![f.predict_proba(&rows[1]), f.predict_proba(&rows[2])]);
    }

    #[test]
    fn bad_headers_and_lengths_are_errors() {
        let blob = encode(&spec());
        assert_eq!(VendorForest::from_bytes(&[]).err(), Some(ClassifierError::Size));
        let mut b = blob.clone();
        b[0] = b'X';
        assert_eq!(VendorForest::from_bytes(&b).err(), Some(ClassifierError::BadMagic));
        let mut b = blob.clone();
        b[8] = 3;
        assert!(matches!(VendorForest::from_bytes(&b), Err(ClassifierError::Invalid(m)) if m.contains("class")));
        let mut b = blob.clone();
        b[12] = 7;
        assert!(matches!(VendorForest::from_bytes(&b), Err(ClassifierError::Invalid(m)) if m.contains("feature")));
        let mut b = blob.clone();
        b.push(0);
        assert_eq!(VendorForest::from_bytes(&b).err(), Some(ClassifierError::Size));
        let mut b = blob.clone();
        b[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(VendorForest::from_bytes(&b).is_err());
        assert!(matches!(VendorForest::load("/nonexistent/forest.bin"), Err(ClassifierError::Io(_))));
    }

    #[test]
    fn truncations_and_byte_changes_never_panic() {
        let blob = encode(&spec());
        for n in 0..blob.len() {
            assert!(VendorForest::from_bytes(&blob[..n]).is_err(), "truncated to {n}");
        }
        let x = row(&[(0, 1.0), (3, 1.0)]);
        for i in 0..blob.len() {
            for v in [0u8, 1, 2, 3, 0x7f, 0x80, 0xff] {
                let mut b = blob.clone();
                b[i] = v;
                if let Ok(f) = VendorForest::from_bytes(&b) {
                    let _ = f.predict_proba(&x);
                }
            }
        }
    }

    #[test]
    fn cycles_and_bad_programs_are_errors() {
        let mut s = spec();
        s.trees[0][1] = node(COMPARE, 0, 0, 0, 0.0, 0, 2); // child points back to the root
        assert!(matches!(VendorForest::from_bytes(&encode(&s)), Err(ClassifierError::Invalid(m)) if m.contains("cycle")));
        let mut s = spec();
        s.units[0][0] = vec![T(0), ADD];
        assert!(VendorForest::from_bytes(&encode(&s)).is_err());
        let mut s = spec();
        s.units[0][0] = vec![T(0), T(1)];
        assert!(VendorForest::from_bytes(&encode(&s)).is_err());
        let mut s = spec();
        s.units[0][0] = vec![T(9)];
        assert!(VendorForest::from_bytes(&encode(&s)).is_err());
    }

    /// With `OURA_NSSA_FOREST` set to the vendor file: 2000 trees, probabilities sum to 1.
    #[test]
    #[ignore]
    fn real_forest_loads_and_normalizes() {
        let Ok(path) = std::env::var("OURA_NSSA_FOREST") else {
            eprintln!("OURA_NSSA_FOREST not set");
            return;
        };
        let t = std::time::Instant::now();
        let f = VendorForest::load(&path).unwrap();
        eprintln!("loaded in {:?}", t.elapsed());
        assert_eq!(f.n_trees(), 2000);
        for x in [[0f32; STAGER_FEATURES], [f32::NAN; STAGER_FEATURES]] {
            let p = f.predict_proba(&x);
            let s: f64 = p.iter().map(|&v| f64::from(v)).sum();
            assert!((s - 1.0).abs() < 1e-5, "{p:?}");
        }
    }
}
