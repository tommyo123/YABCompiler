//! Generic dataflow framework + concrete analyses on top of the CFG.
//!
//! The framework is a worklist solver over a `Lattice` and a per-node
//! transfer function. Forward and backward analyses share the same
//! solver — direction is chosen by the `DataflowAnalysis` impl.
//!
//! Currently implements:
//!   * `LiveVars` — backward: which variables are live (read on some
//!     path before being killed) at each CFG node's entry/exit.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::ast::{BinOp, Func1, VarKind, VarName};
use crate::cfg::{Cfg, CfgNode, StmtPath};
use crate::ir::{Expr, Module, ReadTarget, Stmt, StrExpr};

// ===== Lattice / framework =================================================

pub trait Lattice: Clone + PartialEq {
    fn bottom() -> Self;
    fn join(&self, other: &Self) -> Self;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Forward,
    Backward,
}

pub trait DataflowAnalysis {
    type Lattice: Lattice;
    fn direction(&self) -> Direction;
    /// Initial state at the program entry (forward) or exit (backward)
    /// — usually `Lattice::bottom()` but some analyses want a richer
    /// boundary (e.g., LiveVars uses ⊥ since nothing is live after END).
    fn boundary(&self) -> Self::Lattice {
        Self::Lattice::bottom()
    }
    /// `transfer(node, in)` returns the state on the OUT side of
    /// `node` given its IN side. Direction-agnostic — caller handles
    /// which side is which. Inputs `module` and `cfg` for context the
    /// analysis may need (e.g. inspecting the underlying Stmt).
    fn transfer(
        &self,
        module: &Module,
        cfg: &Cfg,
        node: &CfgNode,
        node_id: usize,
        in_state: &Self::Lattice,
    ) -> Self::Lattice;

    /// Optional path-sensitive overlay: produce a separate OUT state
    /// per successor edge. Returning `None` means "uniform OUT — use
    /// the result of `transfer` for every edge", which is what the
    /// vast majority of analyses want.
    ///
    /// When implemented, the solver uses the per-edge OUT value
    /// matching the predecessor→current-node edge instead of the
    /// uniform `transfer` result. The Vec must have exactly
    /// `node.successors.len()` entries; entry `i` corresponds to
    /// `node.successors[i]`.
    ///
    /// Used by `NumericFactsFlow` to refine variable ranges across
    /// `IF cond THEN target` edges — on the THEN edge the cond is
    /// known to be true, on the fall-through edge it's known to be
    /// false. Without this hook the dataflow widens at loop
    /// backedges in shapes like `IF X = K THEN exit` because it can't
    /// drop the K value from X's range on the continue path.
    fn transfer_per_successor(
        &self,
        _module: &Module,
        _cfg: &Cfg,
        _node: &CfgNode,
        _node_id: usize,
        _in_state: &Self::Lattice,
    ) -> Option<Vec<Self::Lattice>> {
        None
    }
}

/// Worklist solver. Returns a map from node id to (in, out) lattice
/// values once the analysis converges. For backward analyses, "in" /
/// "out" are still named from the FORWARD perspective — the solver
/// just flips successor/predecessor traversal and the join direction.
pub fn solve<D: DataflowAnalysis>(
    module: &Module,
    cfg: &Cfg,
    analysis: &D,
) -> Vec<(D::Lattice, D::Lattice)> {
    let n = cfg.nodes.len();
    let mut state: Vec<(D::Lattice, D::Lattice)> =
        vec![(D::Lattice::bottom(), D::Lattice::bottom()); n];
    // Cache of per-successor OUT states — only populated for nodes
    // whose analysis returns Some from transfer_per_successor.
    // Without an entry, predecessors fall back to the uniform OUT.
    let mut per_succ_out: HashMap<usize, Vec<D::Lattice>> = HashMap::new();
    if n == 0 {
        return state;
    }
    let dir = analysis.direction();
    // Seed boundary node(s).
    match dir {
        Direction::Forward => {
            state[cfg.entry].0 = analysis.boundary();
        }
        Direction::Backward => {
            // Every "exit" node (no successors) gets the boundary as
            // its OUT state. For a typical BASIC program this is END
            // / STOP / fallthrough off the last line.
            for (id, node) in cfg.nodes.iter().enumerate() {
                if node.successors.is_empty() {
                    state[id].1 = analysis.boundary();
                }
            }
        }
    }
    // Initial worklist: every node, since nothing is computed yet.
    let mut work: VecDeque<usize> = (0..n).collect();
    let mut in_queue: Vec<bool> = vec![true; n];
    let mut visit_counts: Vec<u64> = vec![0; n];

    // Helper: pull the OUT state coming OUT of `pred` toward `target`.
    // When `pred` has per-successor refinement, use the entry whose
    // index matches the (pred → target) edge slot. Otherwise fall back
    // to the uniform OUT.
    let edge_out = |state: &[(D::Lattice, D::Lattice)],
                    per_succ_out: &HashMap<usize, Vec<D::Lattice>>,
                    pred: usize,
                    target: usize|
     -> D::Lattice {
        if let Some(per_succ) = per_succ_out.get(&pred)
            && let Some(idx) = cfg.nodes[pred].successors.iter().position(|&s| s == target)
            && let Some(s) = per_succ.get(idx)
        {
            return s.clone();
        }
        state[pred].1.clone()
    };

    // Per-node visit cap for widening. When a node has been re-evaluated
    // more than this many times the analysis is no longer converging
    // along a narrow lattice path (typically a `V=V+1` GOTO-loop whose
    // refined back-edge keeps shifting one endpoint by 1 per round).
    // Replace its IN state with `bottom()` — which for the range
    // lattice means "no facts known", i.e. top in analysis semantics —
    // so the next transfer over-approximates and the fixpoint settles.
    const VISIT_WIDEN_THRESHOLD: u64 = 32;
    let mut widened: Vec<bool> = vec![false; n];
    while let Some(id) = work.pop_front() {
        in_queue[id] = false;
        visit_counts[id] += 1;
        let node = &cfg.nodes[id];
        match dir {
            Direction::Forward => {
                if !widened[id] && visit_counts[id] > VISIT_WIDEN_THRESHOLD {
                    widened[id] = true;
                }
                let new_in = if id == cfg.entry {
                    analysis.boundary()
                } else if widened[id] {
                    // Once a node is widened, freeze its IN at "no
                    // facts known" so the back-edge join can't keep
                    // crawling the bounds one value at a time.
                    D::Lattice::bottom()
                } else {
                    // Only join predecessors that have already been
                    // visited at least once. For must-style lattices
                    // (e.g. NumericState's key-intersect join), an
                    // unvisited pred's bottom OUT is "no facts" — not
                    // ⊤ — and joining with it drops every key the
                    // other preds tried to contribute. Cycles like
                    // L20→…→L40→L20 then never recover the killed
                    // key: L40's OUT can only carry M once L20's OUT
                    // carries M, and L20's OUT can only carry M once
                    // L40's OUT carries M. Filtering unvisited preds
                    // out of the first join breaks the deadlock —
                    // subsequent visits naturally pick up the back
                    // edge once it's been computed.
                    let mut preds = node
                        .predecessors
                        .iter()
                        .copied()
                        .filter(|&p| visit_counts[p] > 0);
                    if let Some(first) = preds.next() {
                        let mut acc = edge_out(&state, &per_succ_out, first, id);
                        for p in preds {
                            acc = acc.join(&edge_out(&state, &per_succ_out, p, id));
                        }
                        acc
                    } else {
                        D::Lattice::bottom()
                    }
                };
                let new_out = analysis.transfer(module, cfg, node, id, &new_in);
                let new_per_succ = analysis.transfer_per_successor(module, cfg, node, id, &new_in);
                let per_succ_changed = match (per_succ_out.get(&id), &new_per_succ) {
                    (Some(prev), Some(curr)) => prev != curr,
                    (None, None) => false,
                    _ => true,
                };
                let changed = new_in != state[id].0 || new_out != state[id].1 || per_succ_changed;
                state[id] = (new_in, new_out);
                match new_per_succ {
                    Some(v) => {
                        per_succ_out.insert(id, v);
                    }
                    None => {
                        per_succ_out.remove(&id);
                    }
                }
                if changed {
                    for &succ in &node.successors {
                        if !in_queue[succ] {
                            work.push_back(succ);
                            in_queue[succ] = true;
                        }
                    }
                }
            }
            Direction::Backward => {
                let new_out = if node.successors.is_empty() {
                    analysis.boundary()
                } else {
                    let mut succs = node.successors.iter().copied();
                    if let Some(first) = succs.next() {
                        let mut acc = state[first].0.clone();
                        for s in succs {
                            acc = acc.join(&state[s].0);
                        }
                        acc
                    } else {
                        D::Lattice::bottom()
                    }
                };
                let new_in = analysis.transfer(module, cfg, node, id, &new_out);
                let changed = new_in != state[id].0 || new_out != state[id].1;
                state[id] = (new_in, new_out);
                if changed {
                    for &pred in &node.predecessors {
                        if !in_queue[pred] {
                            work.push_back(pred);
                            in_queue[pred] = true;
                        }
                    }
                }
            }
        }
    }
    state
}

// ===== HashSet<VarName> as a lattice ======================================

impl Lattice for HashSet<VarName> {
    fn bottom() -> Self {
        HashSet::new()
    }
    fn join(&self, other: &Self) -> Self {
        // Set union — standard "may" lattice for live-vars,
        // reaching-defs, etc.
        let mut out = self.clone();
        out.extend(other.iter().cloned());
        out
    }
}

// ===== Live variables =====================================================

/// Live-variable analysis: for each program point, the set of
/// variables that may be read on some path forward without first
/// being overwritten. Backward-flowing.
///
/// Useful as a building block for:
///   * dead-store elimination (a var written but not live afterward)
///   * register allocation (the set of values needing storage at any
///     given point)
///   * integer-promotion safety (a promoted var must stay int across
///     every live edge)
pub struct LiveVars;

impl crate::analysis::Analysis for LiveVars {
    type Output = LiveVarsResult;
    fn name(&self) -> &'static str {
        "live-vars"
    }
    fn run(&self, module: &Module, deps: &mut crate::analysis::Registry) -> Self::Output {
        let cfg = deps.get(module, &crate::cfg::CfgBuild).clone();
        // Note: cloning the CFG keeps the dataflow `solve` function
        // free of borrow gymnastics with the registry. Cheap for
        // small modules; revisit if it ever shows up in profiles.
        let solved = solve(module, &cfg, &Self);
        LiveVarsResult { per_node: solved }
    }
}

#[derive(Debug, Clone)]
pub struct LiveVarsResult {
    /// `per_node[id] = (live_in, live_out)`. live_in is the set of
    /// variables that are live AT ENTRY to the node — i.e., before
    /// the node executes. live_out is the set live AFTER it executes.
    pub per_node: Vec<(HashSet<VarName>, HashSet<VarName>)>,
}

impl DataflowAnalysis for LiveVars {
    type Lattice = HashSet<VarName>;
    fn direction(&self) -> Direction {
        Direction::Backward
    }
    fn transfer(
        &self,
        module: &Module,
        cfg: &Cfg,
        _node: &CfgNode,
        node_id: usize,
        out_state: &Self::Lattice,
    ) -> Self::Lattice {
        // live_in = use(stmt) ∪ (live_out − def(stmt))
        let stmt = cfg.stmt_at(node_id, module);
        let mut live = out_state.clone();
        // Apply def first (kill written vars)…
        for v in stmt_defs(stmt) {
            live.remove(&v);
        }
        // …then use (revive read vars).
        for v in stmt_uses(stmt) {
            live.insert(v);
        }
        live
    }
}

// ===== Variable interference ==============================================

/// Undirected interference graph over variables: an edge `u — v` means
/// `u` and `v` are simultaneously live at some program point, so they
/// cannot share a single storage slot. The complement — two vars with
/// no edge — may safely alias the same zero-page slot, which is what
/// the codegen ZP allocator uses to pack more hot variables into the
/// tiny ZP pool than there are physical slots.
#[derive(Debug, Clone, Default)]
pub struct InterferenceGraph {
    edges: HashMap<VarName, HashSet<VarName>>,
}

impl InterferenceGraph {
    fn add_edge(&mut self, a: &VarName, b: &VarName) {
        if a == b {
            return;
        }
        self.edges.entry(a.clone()).or_default().insert(b.clone());
        self.edges.entry(b.clone()).or_default().insert(a.clone());
    }

    /// Record every distinct pair in `set` as mutually interfering.
    fn add_clique(&mut self, set: &HashSet<VarName>) {
        // O(n²) over the live set at one point; live sets are small for
        // the modules we compile, and this runs once per CFG node.
        let vars: Vec<&VarName> = set.iter().collect();
        for (i, a) in vars.iter().enumerate() {
            for b in &vars[i + 1..] {
                self.add_edge(a, b);
            }
        }
    }

    /// True when `a` and `b` are live at the same time anywhere — i.e.
    /// they must NOT share a slot. Vars never seen live (e.g. neither
    /// appears in any live set) trivially don't interfere.
    pub fn interferes(&self, a: &VarName, b: &VarName) -> bool {
        if a == b {
            // A variable always "interferes with itself" in the sense
            // that it can't be given two meanings — but callers ask
            // about distinct candidates, so report the honest answer:
            // a slot holds one var, asking whether it conflicts with
            // itself is vacuously false for sharing purposes.
            return false;
        }
        self.edges
            .get(a)
            .is_some_and(|neighbours| neighbours.contains(b))
    }

    /// The set of variables `v` interferes with, if any.
    pub fn neighbours(&self, v: &VarName) -> Option<&HashSet<VarName>> {
        self.edges.get(v)
    }

    /// Test-only: build a graph directly from a list of interfering
    /// pairs, so consumers can be unit-tested without standing up a
    /// full module + CFG + liveness solve.
    #[cfg(test)]
    pub(crate) fn from_pairs(pairs: &[(VarName, VarName)]) -> Self {
        let mut g = Self::default();
        for (a, b) in pairs {
            g.add_edge(a, b);
        }
        g
    }
}

/// Builds the [`InterferenceGraph`] from live-variable results: at
/// every CFG node, all variables live together (on entry and on exit)
/// form a clique of mutual interference.
///
/// Uses both the live-IN and live-OUT sets at each node so a value
/// that is live across a statement boundary still conflicts with its
/// neighbours regardless of whether the statement is a def or a use.
/// This is a sound over-approximation: it may add a few edges at node
/// boundaries that a finer def/use analysis would omit, but it never
/// *misses* an interference — and a missed edge is the dangerous
/// direction (it would let two simultaneously-live vars share a slot
/// and silently corrupt one).
pub struct VarInterference;

impl crate::analysis::Analysis for VarInterference {
    type Output = InterferenceGraph;
    fn name(&self) -> &'static str {
        "var-interference"
    }
    fn run(&self, module: &Module, deps: &mut crate::analysis::Registry) -> Self::Output {
        let live = deps.get(module, &LiveVars).clone();
        let mut graph = InterferenceGraph::default();
        for (live_in, live_out) in &live.per_node {
            graph.add_clique(live_in);
            graph.add_clique(live_out);
        }
        graph
    }
}

// ===== Defined-before-use (must-analysis) =================================

/// Forward "definitely-defined" set. A variable is in the set at a
/// program point iff it has been written on EVERY path from program
/// entry to that point. The join is intersection (a var counts as
/// defined only when defined on all incoming paths); the empty set is
/// the program-entry state (BASIC vars start at their default 0, which
/// is not a write). Mirrors `NumericState`'s must-style key-intersect
/// join, so it relies on the same unvisited-predecessor filtering in
/// the solver.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct DefinedSet {
    vars: HashSet<VarName>,
}

impl Lattice for DefinedSet {
    fn bottom() -> Self {
        Self::default()
    }
    fn join(&self, other: &Self) -> Self {
        DefinedSet {
            vars: self.vars.intersection(&other.vars).cloned().collect(),
        }
    }
}

struct DefinedFlow;

impl DataflowAnalysis for DefinedFlow {
    type Lattice = DefinedSet;
    fn direction(&self) -> Direction {
        Direction::Forward
    }
    fn transfer(
        &self,
        module: &Module,
        cfg: &Cfg,
        _node: &CfgNode,
        node_id: usize,
        in_state: &Self::Lattice,
    ) -> Self::Lattice {
        // A write makes the target definitely-defined from here on.
        // Nothing ever un-defines (CLR/RUN reset to 0, which is still a
        // defined value; we conservatively don't model their defs, only
        // ever under-counting — the safe direction).
        let stmt = cfg.stmt_at(node_id, module);
        let mut out = in_state.clone();
        for d in stmt_defs(stmt) {
            out.vars.insert(d);
        }
        out
    }
}

/// The set of variables that are **always written before they are
/// read**, on every path from program entry — i.e. they never observe
/// BASIC's default-0 from an unwritten slot. Built on the
/// `DefinedFlow` must-analysis.
///
/// This is the soundness certificate the ZP allocator needs before it
/// may let two non-interfering scalars *share* a slot: a variable that
/// might read its slot before writing it could pick up a co-tenant's
/// leftover bytes instead of 0. FOR counters get this for free (the
/// FOR header writes the counter before the body reads it); ordinary
/// scalars need this analysis to prove it.
pub struct DefinedBeforeUse;

impl crate::analysis::Analysis for DefinedBeforeUse {
    type Output = HashSet<VarName>;
    fn name(&self) -> &'static str {
        "defined-before-use"
    }
    fn run(&self, module: &Module, deps: &mut crate::analysis::Registry) -> Self::Output {
        let cfg = deps.get(module, &crate::cfg::CfgBuild).clone();
        let solved = solve(module, &cfg, &DefinedFlow);
        // A variable is "unsafe" if it is read at any node where it is
        // not yet definitely-defined on entry. The IN set (solved.0)
        // holds the must-defined facts before the statement runs, so a
        // self-modifying first write (`X = X + 1` with no prior X) is
        // correctly flagged — the use is checked against IN, before
        // this statement's own def lands.
        let mut unsafe_vars: HashSet<VarName> = HashSet::new();
        let mut written: HashSet<VarName> = HashSet::new();
        for (id, _node) in cfg.nodes.iter().enumerate() {
            let stmt = cfg.stmt_at(id, module);
            let in_defined = &solved[id].0;
            for used in stmt_uses(stmt) {
                if !in_defined.vars.contains(&used) {
                    unsafe_vars.insert(used);
                }
            }
            for def in stmt_defs(stmt) {
                written.insert(def);
            }
        }
        // Safe = written at least once AND never read before a write.
        written.difference(&unsafe_vars).cloned().collect()
    }
}

// ===== Numeric range facts =================================================

/// Inclusive integer range for expressions/variables that are known to
/// evaluate to an integer. Unknown or fractional values are represented
/// by absence of a range rather than by a top element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntRange {
    pub min: i32,
    pub max: i32,
}

impl IntRange {
    pub fn new(min: i32, max: i32) -> Self {
        debug_assert!(min <= max);
        Self { min, max }
    }

    pub fn singleton(v: i32) -> Self {
        Self { min: v, max: v }
    }
    pub fn i16() -> Self {
        Self {
            min: i16::MIN as i32,
            max: i16::MAX as i32,
        }
    }
    pub fn u8() -> Self {
        Self {
            min: 0,
            max: u8::MAX as i32,
        }
    }

    pub fn fits_i16(self) -> bool {
        self.min >= i16::MIN as i32 && self.max <= i16::MAX as i32
    }

    pub fn fits_u8(self) -> bool {
        self.min >= 0 && self.max <= u8::MAX as i32
    }

    fn join(self, other: Self) -> Self {
        Self {
            min: self.min.min(other.min),
            max: self.max.max(other.max),
        }
    }

    fn neg(self) -> Option<Self> {
        Some(Self::new(
            (-self.max as i64).try_into().ok()?,
            (-self.min as i64).try_into().ok()?,
        ))
    }

    fn abs(self) -> Option<Self> {
        if self.min >= 0 {
            Some(self)
        } else if self.max <= 0 {
            self.neg()
        } else {
            Some(Self::new(0, self.min.abs().max(self.max.abs())))
        }
    }

    fn add(self, other: Self) -> Option<Self> {
        Some(Self::new(
            clamp_fact_i64(self.min as i64 + other.min as i64)?,
            clamp_fact_i64(self.max as i64 + other.max as i64)?,
        ))
    }

    fn sub(self, other: Self) -> Option<Self> {
        Some(Self::new(
            clamp_fact_i64(self.min as i64 - other.max as i64)?,
            clamp_fact_i64(self.max as i64 - other.min as i64)?,
        ))
    }

    fn mul(self, other: Self) -> Option<Self> {
        let vals = [
            self.min as i64 * other.min as i64,
            self.min as i64 * other.max as i64,
            self.max as i64 * other.min as i64,
            self.max as i64 * other.max as i64,
        ];
        let min = *vals.iter().min().unwrap();
        let max = *vals.iter().max().unwrap();
        Some(Self::new(clamp_fact_i64(min)?, clamp_fact_i64(max)?))
    }
}

/// Keep ranges inside a practical interval. Wider facts are still
/// semantically valid, but they are not useful for i16/u8 decisions and
/// can make loop fixpoints crawl upward one value at a time.
fn clamp_fact_i64(v: i64) -> Option<i32> {
    const LIMIT: i64 = 1_000_000;
    if (-LIMIT..=LIMIT).contains(&v) {
        Some(v as i32)
    } else {
        None
    }
}

/// Known integer facts at a program point. Missing key = unknown.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NumericState {
    pub vars: HashMap<VarName, IntRange>,
}

impl NumericState {
    pub fn get(&self, v: &VarName) -> Option<IntRange> {
        self.vars.get(v).copied()
    }
    fn set(&mut self, v: VarName, r: IntRange) {
        self.vars.insert(v, r);
    }
    fn kill(&mut self, v: &VarName) {
        self.vars.remove(v);
    }
}

impl Lattice for NumericState {
    fn bottom() -> Self {
        Self::default()
    }

    fn join(&self, other: &Self) -> Self {
        let mut vars = HashMap::new();
        for (v, a) in &self.vars {
            if let Some(b) = other.vars.get(v) {
                vars.insert(v.clone(), a.join(*b));
            }
        }
        Self { vars }
    }
}

/// Forward integer range analysis. It tracks only facts we can prove
/// integral; float/fractional/opaque values become unknown.
pub struct NumericFactAnalysis;

impl crate::analysis::Analysis for NumericFactAnalysis {
    type Output = NumericFactsResult;
    fn name(&self) -> &'static str {
        "numeric-facts"
    }
    fn run(&self, module: &Module, deps: &mut crate::analysis::Registry) -> Self::Output {
        let cfg = deps.get(module, &crate::cfg::CfgBuild).clone();
        let data_range = compute_data_range(module);
        let per_node = solve(module, &cfg, &NumericFactsFlow { data_range });
        let node_by_stmt = cfg
            .nodes
            .iter()
            .enumerate()
            .map(|(id, node)| (node.stmt.clone(), id))
            .collect();
        let (shadow_int_vars, shadow_int_read_counts, shadow_only_vars) =
            compute_shadow_int_vars(module, &cfg, &per_node, data_range);
        NumericFactsResult {
            per_node,
            node_by_stmt,
            shadow_int_vars,
            shadow_int_read_counts,
            shadow_only_vars,
        }
    }
}

#[derive(Debug, Clone)]
pub struct NumericFactsResult {
    /// `per_node[id] = (in, out)` from the forward analysis.
    #[allow(dead_code)]
    pub per_node: Vec<(NumericState, NumericState)>,
    /// Stable path → CFG node lookup. This lets codegen ask for the
    /// facts at "the statement currently being emitted" without
    /// rebuilding or re-walking the CFG.
    pub node_by_stmt: HashMap<StmtPath, usize>,
    /// Float scalar vars whose every write leaves a known int16 range.
    /// Codegen can keep a 2-byte shadow slot for these without changing
    /// the BASIC-visible 5-byte float slot.
    pub shadow_int_vars: HashSet<VarName>,
    /// Per-shadow-int-var count of int-context reads. Used by the
    /// codegen ZP allocator to pick the hottest candidates first
    /// when the pool is too small for every shadow.
    pub shadow_int_read_counts: HashMap<VarName, usize>,
    /// Subset of `shadow_int_vars` whose reads are entirely in
    /// int-context. For these, codegen can skip the V_var MOVMF
    /// sync at every write — the shadow slot is the only
    /// observable copy of the value. Saves ~150 cycles per write
    /// in tight loops.
    pub shadow_only_vars: HashSet<VarName>,
}

#[allow(dead_code)] // codegen consumes the entry-range subset first; the rest is analysis API.
impl NumericFactsResult {
    pub fn node_for_path(&self, path: &StmtPath) -> Option<usize> {
        self.node_by_stmt.get(path).copied()
    }

    pub fn state_in_at(&self, node_id: usize) -> Option<&NumericState> {
        self.per_node.get(node_id).map(|(state, _)| state)
    }

    pub fn state_out_at(&self, node_id: usize) -> Option<&NumericState> {
        self.per_node.get(node_id).map(|(_, state)| state)
    }

    pub fn state_in_before_path(&self, path: &StmtPath) -> Option<&NumericState> {
        self.node_for_path(path).and_then(|id| self.state_in_at(id))
    }

    pub fn state_out_after_path(&self, path: &StmtPath) -> Option<&NumericState> {
        self.node_for_path(path)
            .and_then(|id| self.state_out_at(id))
    }

    pub fn expr_int_range_in(&self, node_id: usize, expr: &Expr) -> Option<IntRange> {
        expr_int_range(expr, self.state_in_at(node_id)?)
    }

    pub fn expr_int_range_out(&self, node_id: usize, expr: &Expr) -> Option<IntRange> {
        expr_int_range(expr, self.state_out_at(node_id)?)
    }

    pub fn expr_int_range_before_path(&self, path: &StmtPath, expr: &Expr) -> Option<IntRange> {
        expr_int_range(expr, self.state_in_before_path(path)?)
    }

    pub fn expr_int_range_after_path(&self, path: &StmtPath, expr: &Expr) -> Option<IntRange> {
        expr_int_range(expr, self.state_out_after_path(path)?)
    }
}

struct NumericFactsFlow {
    /// Range of `int(value)` over every numeric DATA entry in the
    /// module, or `None` when the module has no DATA / any DATA value
    /// is non-finite or doesn't truncate to i16. Used by READ to
    /// produce a tight post-state range — without it READ would have
    /// to kill the target's range, blocking shadow-int promotion for
    /// READ-fed Float vars.
    data_range: Option<IntRange>,
}

impl DataflowAnalysis for NumericFactsFlow {
    type Lattice = NumericState;
    fn direction(&self) -> Direction {
        Direction::Forward
    }

    fn transfer(
        &self,
        module: &Module,
        cfg: &Cfg,
        node: &CfgNode,
        node_id: usize,
        in_state: &Self::Lattice,
    ) -> Self::Lattice {
        let stmt = cfg.stmt_at(node_id, module);
        let mut out = transfer_numeric_stmt(stmt, in_state, self.data_range);
        apply_monotone_inc_loop_cap(&mut out, stmt, cfg, module, node, in_state);
        out
    }

    fn transfer_per_successor(
        &self,
        module: &Module,
        cfg: &Cfg,
        node: &CfgNode,
        node_id: usize,
        in_state: &Self::Lattice,
    ) -> Option<Vec<Self::Lattice>> {
        let stmt = cfg.stmt_at(node_id, module);

        // NEXT refinement: the uniform transfer kills the loop counter
        // (its post-loop value `end+step` is out of range). But that
        // killed state also flows along the *back-edge* into the loop
        // body, and NumericState's intersection join then wipes the
        // counter's range out of the body — so without this, no FOR
        // counter ever has a usable range inside its own loop. Split
        // the edges: on the back-edge (loop continues, counter still in
        // [start,end]) preserve the incoming range; on the exit edge
        // kill it as the uniform transfer does. Edges are told apart by
        // line: the body sits at/above the NEXT, the fall-through below.
        if let Stmt::Next { vars } = stmt {
            if vars.is_empty() || vars.iter().any(Option::is_none) {
                return None; // bare NEXT — leave to the uniform kill
            }
            let killed = transfer_numeric_stmt(stmt, in_state, self.data_range);
            let preserved = in_state.clone();
            let self_line = node.line_no;
            let mut outs = Vec::with_capacity(node.successors.len());
            let mut any_back_edge = false;
            for &succ in &node.successors {
                if cfg.nodes[succ].line_no <= self_line {
                    outs.push(preserved.clone()); // back-edge: counter in range
                    any_back_edge = true;
                } else {
                    outs.push(killed.clone()); // exit: counter stepped past end
                }
            }
            return any_back_edge.then_some(outs);
        }

        // Path-sensitive overlay: refine var ranges across the
        // `IF cond THEN target / fall-through` split. Skip anything
        // that doesn't have exactly two successors with the canonical
        // ordering [THEN, fall-through] — IfElse, Rcomp, and
        // structured-IF bodies use multi-edge layouts that the
        // CFG-builder lays out differently and the refinement gain
        // there is small enough that we leave them at uniform.
        if node.successors.len() != 2 {
            return None;
        }
        let cond = match stmt {
            Stmt::If { cond, .. } => cond,
            _ => return None,
        };
        let base = transfer_numeric_stmt(stmt, in_state, self.data_range);
        let then_state = refine_state_for_cond(&base, cond, true);
        let fall_state = refine_state_for_cond(&base, cond, false);
        Some(vec![then_state, fall_state])
    }
}

/// Apply a guard refinement to `state` based on `cond` evaluated to
/// `taken`. Returns a (possibly tighter) NumericState. Conservative:
/// any shape this code can't prove a refinement for falls back to
/// the input state unchanged — soundness over precision.
///
/// Recognised shapes for v1:
/// * `Var <relop> Number` for the six relational ops + AND chains.
/// * Negation of `<relop>` (the `taken=false` branch flips the op).
fn refine_state_for_cond(state: &NumericState, cond: &Expr, taken: bool) -> NumericState {
    // Walk past a NOT once, flipping `taken`.
    if let Expr::Not(inner) = cond {
        return refine_state_for_cond(state, inner, !taken);
    }
    // AND-of-comparisons: on the TRUE branch both sides must hold,
    // so refine sequentially. On the FALSE branch one or both could
    // be false — we can't tighten without a union, so skip.
    if let Expr::Bin(BinOp::And, l, r) = cond {
        if taken {
            let s1 = refine_state_for_cond(state, l, true);
            return refine_state_for_cond(&s1, r, true);
        }
        return state.clone();
    }
    // OR-of-comparisons: dual — refine on FALSE branch, skip TRUE.
    if let Expr::Bin(BinOp::Or, l, r) = cond {
        if !taken {
            let s1 = refine_state_for_cond(state, l, false);
            return refine_state_for_cond(&s1, r, false);
        }
        return state.clone();
    }
    let Expr::Bin(op, l, r) = cond else {
        return state.clone();
    };
    let mut effective_op = *op;
    if !taken {
        effective_op = match effective_op {
            BinOp::Eq => BinOp::Ne,
            BinOp::Ne => BinOp::Eq,
            BinOp::Lt => BinOp::Ge,
            BinOp::Le => BinOp::Gt,
            BinOp::Gt => BinOp::Le,
            BinOp::Ge => BinOp::Lt,
            _ => return state.clone(),
        };
    }
    let mut refined = state.clone();
    // `Var <op> Number` and the symmetric `Number <op> Var`.
    if let (Expr::Var(v), Some(n)) = (l.as_ref(), expr_as_int_literal(r))
        && let Some(prev) = refine_basis(state, v)
        && let Some(refined_range) = refine_range(prev, effective_op, n)
    {
        refined.set(v.clone(), refined_range);
    } else if let (Some(n), Expr::Var(v)) = (expr_as_int_literal(l), r.as_ref()) {
        // Flip the operator to match `Var <op'> Number`.
        let flipped = match effective_op {
            BinOp::Lt => BinOp::Gt,
            BinOp::Le => BinOp::Ge,
            BinOp::Gt => BinOp::Lt,
            BinOp::Ge => BinOp::Le,
            other => other, // Eq, Ne are commutative
        };
        if let Some(prev) = refine_basis(state, v)
            && let Some(refined_range) = refine_range(prev, flipped, n)
        {
            refined.set(v.clone(), refined_range);
        }
    }
    refined
}

/// Pick the IntRange to refine `v` against, or `None` to skip
/// refinement. A Float scalar with no prior int fact returns `None`
/// — the comparison `floatVar > 50` is true for `50.5` too, so
/// claiming the result is integer-valued in `[51, ∞]` would be
/// unsound and downstream consumers (INT-elision, shadow-int promo)
/// would fold the var to a clean integer when it isn't one.
/// Integer-kinded vars default to i16 when state has no entry —
/// they're always integer-valued so the comparison range is sound.
fn refine_basis(state: &NumericState, v: &VarName) -> Option<IntRange> {
    if let Some(r) = state.get(v) {
        return Some(r);
    }
    if v.kind == VarKind::Integer && v.base != "TI" && v.base != "ST" {
        return Some(IntRange::i16());
    }
    None
}

/// Compute the tightest single IntRange that holds when
/// `var <op> n` is known true, given `var`'s pre-state range.
///
/// Returns `None` when no refinement is provable. The narrowing
/// is intentionally limited to bounds that fit a single contiguous
/// interval; cases like `var <> K` for K in the interior are left
/// alone because we'd need an exclusion set to represent them.
fn refine_range(prev: IntRange, op: BinOp, n: i32) -> Option<IntRange> {
    let new = match op {
        BinOp::Eq => IntRange::singleton(n),
        BinOp::Ne => {
            // Single-range refinement only when n is at a boundary —
            // otherwise the result would be a union we can't represent.
            if n == prev.min && prev.min < prev.max {
                IntRange::new(prev.min + 1, prev.max)
            } else if n == prev.max && prev.min < prev.max {
                IntRange::new(prev.min, prev.max - 1)
            } else {
                return None;
            }
        }
        BinOp::Lt => IntRange::new(i32::MIN, n.saturating_sub(1)),
        BinOp::Le => IntRange::new(i32::MIN, n),
        BinOp::Gt => IntRange::new(n.saturating_add(1), i32::MAX),
        BinOp::Ge => IntRange::new(n, i32::MAX),
        _ => return None,
    };
    // Intersect with the existing range to keep both bounds tight.
    let lo = prev.min.max(new.min);
    let hi = prev.max.min(new.max);
    if lo > hi {
        // The intersection is empty — the branch is unreachable,
        // and no fact we add here is sound for downstream consumers.
        // Return `prev` unchanged so the dataflow doesn't widen
        // toward a meaningless interval.
        return None;
    }
    Some(IntRange::new(lo, hi))
}

/// Best-effort i32 extraction from an integer-typed Expr literal.
fn expr_as_int_literal(e: &Expr) -> Option<i32> {
    match e {
        Expr::Number(n) if n.is_finite() && n.fract() == 0.0 => {
            let v = *n as i64;
            if (i32::MIN as i64..=i32::MAX as i64).contains(&v) {
                Some(v as i32)
            } else {
                None
            }
        }
        Expr::Neg(inner) => expr_as_int_literal(inner).and_then(i32::checked_neg),
        _ => None,
    }
}

/// Walk every Stmt::Data in source order and compute the i32 range
/// covering every numeric entry's truncation toward zero. Returns
/// `None` when the module has no DATA, any value is non-finite, or
/// any value's truncation falls outside i16 (would trap FAC→i16
/// downstream — better to kill the READ target's range than promise
/// a fact that becomes unsound at runtime).
fn compute_data_range(module: &Module) -> Option<IntRange> {
    let mut min = i32::MAX;
    let mut max = i32::MIN;
    let mut any = false;
    for line in &module.lines {
        for stmt in &line.stmts {
            if let Stmt::Data(values) = stmt {
                for value in values {
                    if let crate::ast::DataValue::Float(f) = value {
                        if !f.is_finite() {
                            return None;
                        }
                        // BASIC v2's FAC→i16 truncates toward zero
                        // (matches Rust's `as i32`). Reject anything
                        // whose truncation can't fit i16 — sync would
                        // trap on the runtime conversion.
                        let v = *f as i64;
                        if !(i16::MIN as i64..=i16::MAX as i64).contains(&v) {
                            return None;
                        }
                        let v = v as i32;
                        min = min.min(v);
                        max = max.max(v);
                        any = true;
                    }
                    // String DATA values are unreadable into numeric
                    // targets at runtime (SYNTAX ERROR via __VAL_HELPER);
                    // skipping them keeps the range sound for the
                    // numeric-target case.
                }
            }
        }
    }
    if any {
        Some(IntRange::new(min, max))
    } else {
        None
    }
}

fn transfer_numeric_stmt(
    stmt: &Stmt,
    in_state: &NumericState,
    data_range: Option<IntRange>,
) -> NumericState {
    let mut out = in_state.clone();
    match stmt {
        Stmt::Let { var, value } => assign_expr_range(&mut out, var, value, in_state),
        Stmt::LetStr { var, .. } => out.kill(var),
        Stmt::For {
            var,
            start,
            end,
            step,
            ..
        } => {
            if let (Some(s), Some(e), Some(st)) = (
                expr_int_range(start, in_state),
                expr_int_range(end, in_state),
                expr_int_range(step, in_state),
            ) {
                // FOR always enters the body once; track the useful
                // counter envelope when all loop parameters are integral.
                let mut r = s.join(e);
                if st.min < 0 || st.max > 0 {
                    r = r.join(st);
                }
                out.set(var.clone(), r);
            } else {
                out.kill(var);
            }
        }
        Stmt::Next { vars } => {
            // NEXT mutates the loop variable. After loop exit the
            // value is generally end+step (float/int FOR) or otherwise
            // depends on the selected codegen route, so the body
            // envelope from FOR is not a sound post-NEXT fact.
            if vars.is_empty() || vars.iter().any(Option::is_none) {
                out.vars.clear();
            } else {
                for v in vars.iter().flatten() {
                    out.kill(v);
                }
            }
        }
        Stmt::Get { var } | Stmt::KeyGet { var } => {
            // BASIC's `GET` to a numeric var routes through the
            // compiler's `__GET_NUM` helper, which accepts only the
            // digit chars 0-9 (with `+ , - . E` plus no-key all
            // mapped to 0; any other char SYNTAXes out). So a
            // successful return always lands in `0..=9` for both
            // Integer and Float kinds — tighter than the generic
            // u8 fact, which lets `%`-vars stay in u8-storage and
            // Float vars qualify for shadow-int promotion. KEYGET
            // shares the same numeric path (just blocks until a
            // key is pressed).
            if matches!(var.kind, VarKind::Integer | VarKind::Float)
                && var.base != "TI"
                && var.base != "ST"
            {
                out.set(var.clone(), IntRange::new(0, 9));
            } else {
                out.kill(var);
            }
        }
        Stmt::GetFile { vars, .. } => {
            for v in vars {
                out.kill(v);
            }
        }
        Stmt::Fetch { target, .. } => {
            // FETCH writes a string descriptor pointing at
            // __FETCH_BUF. No numeric facts to track; just kill
            // any prior range for that var (defensive — target is
            // string-only per the parser, so this is a no-op for
            // a clean numeric state, but matches the kill pattern
            // used by GetFile / Read for consistency).
            out.kill(target);
        }
        Stmt::Read(targets) => {
            // READ pulls the next entry from the module-level DATA
            // list. When that list is bounded — every entry's
            // truncation fits i16 — give numeric scalar targets the
            // union range so shadow-int promotion sees a sound
            // `all_i16` fact at the write site. Falls back to
            // \`kill\` when DATA is unbounded or the target is an
            // array element (those go through emit_read_array which
            // doesn't drive shadow promotion).
            for target in targets {
                match target {
                    crate::ir::ReadTarget::Scalar(v) => match (data_range, v.kind) {
                        (Some(r), VarKind::Integer | VarKind::Float)
                            if v.base != "TI" && v.base != "ST" =>
                        {
                            out.set(v.clone(), r);
                        }
                        _ => out.kill(v),
                    },
                    crate::ir::ReadTarget::Array { .. } => {}
                }
            }
        }
        Stmt::Input { targets, .. } => kill_read_targets(&mut out, targets),
        Stmt::InputFile { targets, .. } => kill_read_targets(&mut out, targets),
        Stmt::Clr => {
            for r in out.vars.values_mut() {
                *r = IntRange::singleton(0);
            }
        }
        Stmt::Run(_) => out.vars.clear(),
        _ => {}
    }
    out
}

/// Detect the `var = var ± Number(k)` pattern with `|k| ≤ 1024` —
/// the typical FOR-counter-loop step shape that the range-clamp
/// in `assign_expr_range` ends up killing once the fixpoint widens
/// enough. We trust that real BASIC programs bound such loops via
/// an IF exit, even though the dataflow doesn't model conditions,
/// so the runtime range stays inside i16. Used by the shadow-int
/// gate to opt out of the otherwise-too-strict `all_i16` check.
fn stmt_self_modify_small_step(stmt: &Stmt, target: &VarName) -> bool {
    let Stmt::Let { var, value } = stmt else {
        return false;
    };
    if var != target {
        return false;
    }
    fn matches(e: &Expr, target: &VarName) -> bool {
        matches!(e, Expr::Var(v) if v == target)
    }
    fn const_fits(n: f64) -> bool {
        n.is_finite() && n.fract() == 0.0 && n.abs() <= 1024.0
    }
    match value {
        Expr::Bin(BinOp::Add, l, r) | Expr::Bin(BinOp::Sub, l, r) => {
            if matches(l, target) {
                if let Expr::Number(n) = r.as_ref() {
                    return const_fits(*n);
                }
            }
            if let Expr::Bin(BinOp::Add, _, _) = value
                && matches(r, target)
                && let Expr::Number(n) = l.as_ref()
            {
                return const_fits(*n);
            }
            false
        }
        _ => false,
    }
}

fn assign_expr_range(out: &mut NumericState, var: &VarName, value: &Expr, in_state: &NumericState) {
    if expr_reads_var(value, var) {
        match in_state.get(var) {
            // Singleton self-modify already evaluates exactly through
            // expr_int_range below — fall through.
            Some(r) if r.min == r.max => {}
            // Self-modify with a multi-value range: try the small-step
            // shape `var = var ± SmallConst` first so we keep the bound
            // across loop iterations (e.g. `M = M - 1` in a 240→0 down-
            // counter); killing it here would leave the path-sensitive
            // refinement nothing to tighten. Anything else falls back to
            // kill.
            Some(r) => {
                if let Some(stepped) = self_modify_step_range(value, var, r) {
                    out.set(var.clone(), stepped);
                    return;
                }
                out.kill(var);
                return;
            }
            None => {
                out.kill(var);
                return;
            }
        }
    }
    if let Some(r) = expr_int_range(value, in_state) {
        out.set(var.clone(), r);
    } else {
        out.kill(var);
    }
}

/// Match `var ± SmallConst` and compute the resulting range from the
/// var's existing range. Lets multi-value self-modify loops (e.g.
/// `M = M - 1` with M ∈ [1, 240]) carry their bounds through the
/// dataflow fixpoint instead of being killed at the first widen.
///
/// Once the range crosses out of i16 the fixpoint would otherwise
/// crawl forward one value per iteration until it hit the i32
/// hard-clamp at ±1,000,000 — that's millions of dataflow rounds
/// for any unbounded down-counter like `Y = Y - 1` inside a
/// `GOTO`-loop. Detect the i16-overflow case up front and widen to
/// the full i16 range (the largest BASIC v2 integer range we model
/// anyway) so the lattice stabilises in a single step instead.
fn self_modify_step_range(value: &Expr, var: &VarName, prev: IntRange) -> Option<IntRange> {
    let (op, lit) = match value {
        Expr::Bin(op @ (BinOp::Add | BinOp::Sub), l, r) => match (l.as_ref(), r.as_ref()) {
            (Expr::Var(v), Expr::Number(n)) if v == var => (*op, *n),
            (Expr::Number(n), Expr::Var(v)) if v == var && *op == BinOp::Add => (BinOp::Add, *n),
            _ => return None,
        },
        _ => return None,
    };
    if !lit.is_finite() || lit.fract() != 0.0 || lit.abs() > 1024.0 {
        return None;
    }
    let k = lit as i32;
    let lo = match op {
        BinOp::Add => prev.min as i64 + k as i64,
        BinOp::Sub => prev.min as i64 - k as i64,
        _ => return None,
    };
    let hi = match op {
        BinOp::Add => prev.max as i64 + k as i64,
        BinOp::Sub => prev.max as i64 - k as i64,
        _ => return None,
    };
    // Widening: once the stepped range straddles outside i16, jump
    // straight to the full i16 interval. Without this the fixpoint
    // walks the range outward one value per iteration through every
    // monotone loop, taking up to 2 million rounds before
    // `clamp_fact_i64` finally drops the var.
    if lo < i16::MIN as i64 || hi > i16::MAX as i64 {
        return Some(IntRange::i16());
    }
    // Monotone-step widening with thresholds: when the step pushes a
    // bound past its previous value, snap to the nearest "interesting"
    // threshold instead of jumping straight to ±i16. The thresholds
    // line up with the boundaries used by per-edge IF refinement
    // (`refine_range`), so a guard like `IF M = 0 THEN exit` can
    // tighten the fall-through range back to `[1, prev.max]` once the
    // widening parks the lower bound on 0. Without thresholded
    // widening the bound jumps past 0 to `i16::MIN` in a single step
    // and the `Ne 0` refinement, which only fires when 0 sits at a
    // range boundary, never gets the chance to re-tighten.
    //
    // The fixpoint takes a handful of extra iterations (one per
    // threshold crossed) but still converges in O(1) rounds rather than
    // crawling one step at a time. A loop with no exit guard (e.g. a
    // bare `V=V+1` GOTO loop) settles once it crosses the outermost
    // threshold into the i16 boundary check above.
    let widened_lo = if (lo as i32) < prev.min {
        widen_lo_to_threshold(lo as i32)
    } else {
        clamp_fact_i64(lo)?
    };
    let widened_hi = if (hi as i32) > prev.max {
        widen_hi_to_threshold(hi as i32)
    } else {
        clamp_fact_i64(hi)?
    };
    Some(IntRange::new(widened_lo, widened_hi))
}

/// Narrow `var`'s OUT range after a `var = var + 1` step when the
/// CFG successor is `IF var = Other THEN ...` AND there's a clear
/// loop back to this LET. In that shape `var` is a monotone counter
/// approaching `Other` for the eq-exit; the loop terminates when
/// `var = Other`, so `var` never exceeds `Other.max` at the LET's
/// OUT. Without this, dataflow widening on `X = X + 1` runs `X`'s
/// upper bound up to `i16::MAX` and downstream u8/abs-X fast paths
/// can't fire.
///
/// Soundness gate: a predecessor of the LET node must have a line
/// number >= the LET node's own line — that's the structural sign
/// of a back edge into this line (a `GOTO N` or `IF ... THEN N` from
/// a later line). Without the back edge we can't conclude this is a
/// loop counter; falls back to the normal widened range.
fn apply_monotone_inc_loop_cap(
    out: &mut NumericState,
    stmt: &Stmt,
    cfg: &Cfg,
    module: &Module,
    node: &CfgNode,
    in_state: &NumericState,
) {
    let Stmt::Let { var, value } = stmt else {
        return;
    };
    // Must be `var = var + 1` or `1 + var`. Step magnitude > 1 can
    // overshoot the eq-exit and is unsound for this cap.
    let is_inc1 = match value {
        Expr::Bin(BinOp::Add, l, r) => match (l.as_ref(), r.as_ref()) {
            (Expr::Var(v), Expr::Number(n)) if v == var && *n == 1.0 => true,
            (Expr::Number(n), Expr::Var(v)) if v == var && *n == 1.0 => true,
            _ => false,
        },
        _ => false,
    };
    if !is_inc1 {
        return;
    }
    // Unique CFG successor must be `IF var = Other` (eq).
    if node.successors.len() != 1 {
        return;
    }
    let succ_id = node.successors[0];
    let succ_stmt = cfg.stmt_at(succ_id, module);
    let Stmt::If { cond, .. } = succ_stmt else {
        return;
    };
    let Expr::Bin(BinOp::Eq, l, r) = cond else {
        return;
    };
    let other = if matches!(l.as_ref(), Expr::Var(v) if v == var) {
        r.as_ref()
    } else if matches!(r.as_ref(), Expr::Var(v) if v == var) {
        l.as_ref()
    } else {
        return;
    };
    // The "this is a loop" gate: at least one CFG predecessor of
    // the LET node must come from a line >= this one (back edge).
    // For a one-shot `X=X+1: IF X=M` outside any loop, every pred
    // sits on a strictly earlier line — the cap doesn't apply and
    // X stays at its full widened range.
    let self_line = node.line_no;
    let has_back_edge = node
        .predecessors
        .iter()
        .any(|&p| cfg.nodes[p].line_no >= self_line);
    if !has_back_edge {
        return;
    }
    // Cap using Other's range from the LET's IN state. Other isn't
    // modified by the LET itself, so this is the value that drives
    // the IF in the successor.
    let Some(other_range) = expr_int_range(other, in_state) else {
        return;
    };
    let cap = other_range.max;
    let Some(prev) = out.get(var) else {
        return;
    };
    if prev.max > cap {
        out.set(var.clone(), IntRange::new(prev.min, cap));
    }
}

/// Widening thresholds for [`self_modify_step_range`]. Sorted
/// ascending. Each value is a boundary the per-edge IF refinement
/// can latch onto (`refine_range` for `Eq`/`Ne` only tightens when the
/// constant sits exactly on a range boundary, and `Lt`/`Le`/`Gt`/`Ge`
/// snap to the literal). Keeping widening on these landings lets
/// loop-exit guards re-narrow the range instead of running away to
/// ±i16.
const WIDENING_THRESHOLDS: [i32; 8] = [i16::MIN as i32, -128, -1, 0, 1, 127, 255, i16::MAX as i32];

/// Largest threshold `≤ v`. Used when widening a range's lower bound
/// — the widened bound must stay `≤` the actual computed bound for
/// soundness. Falls back to `i16::MIN` when `v` is below every
/// threshold (the caller already gated `v ≥ i16::MIN`).
fn widen_lo_to_threshold(v: i32) -> i32 {
    WIDENING_THRESHOLDS
        .iter()
        .copied()
        .filter(|&t| t <= v)
        .max()
        .unwrap_or(i16::MIN as i32)
}

/// Smallest threshold `≥ v`. Used when widening a range's upper bound
/// — the widened bound must stay `≥` the actual computed bound for
/// soundness. Falls back to `i16::MAX` when `v` is above every
/// threshold (the caller already gated `v ≤ i16::MAX`).
fn widen_hi_to_threshold(v: i32) -> i32 {
    WIDENING_THRESHOLDS
        .iter()
        .copied()
        .filter(|&t| t >= v)
        .min()
        .unwrap_or(i16::MAX as i32)
}

fn expr_reads_var(e: &Expr, target: &VarName) -> bool {
    struct Finder<'a> {
        target: &'a VarName,
        found: bool,
    }

    impl<'a> crate::visit::Visitor for Finder<'a> {
        fn visit_var_read(&mut self, v: &VarName) {
            if v == self.target {
                self.found = true;
            }
        }
    }

    let mut finder = Finder {
        target,
        found: false,
    };
    crate::visit::Visitor::visit_expr(&mut finder, e);
    finder.found
}

fn kill_read_targets(out: &mut NumericState, targets: &[ReadTarget]) {
    for target in targets {
        if let ReadTarget::Scalar(v) = target {
            out.kill(v);
        }
    }
}

pub fn expr_int_range(e: &Expr, state: &NumericState) -> Option<IntRange> {
    match e {
        Expr::Number(n) => {
            if n.is_finite() && n.fract() == 0.0 {
                clamp_fact_i64(*n as i64).map(IntRange::singleton)
            } else {
                None
            }
        }
        Expr::Var(v) => {
            if v.kind == VarKind::Integer && v.base != "TI" && v.base != "ST" {
                Some(state.get(v).unwrap_or_else(IntRange::i16))
            } else {
                state.get(v)
            }
        }
        Expr::String(_) => None,
        Expr::Neg(inner) => expr_int_range(inner, state)?.neg(),
        Expr::Not(inner) => {
            let r = expr_int_range(inner, state)?;
            if r.fits_i16() {
                Some(IntRange::i16())
            } else {
                None
            }
        }
        Expr::Bin(op, l, r) => {
            // AND with a non-negative constant mask clamps the
            // result to `[0, mask]` regardless of what the other
            // operand's range looks like — the runtime AND on the
            // 6502 ANDs both bytes of the i16 representation, so a
            // mask ≤ 255 zeroes out the high byte. Even an
            // unbounded operand falls into `[0, mask]` (or the
            // BASIC runtime traps the FAC→i16 conversion before
            // the AND fires, in which case there's no result to
            // observe). Pull this check out so it doesn't depend
            // on the `?` short-circuit of the operand reads.
            if matches!(op, BinOp::And) {
                let mask_lit = match (l.as_ref(), r.as_ref()) {
                    (_, Expr::Number(n)) if n.is_finite() && n.fract() == 0.0 => Some(*n as i32),
                    (Expr::Number(n), _) if n.is_finite() && n.fract() == 0.0 => Some(*n as i32),
                    _ => None,
                };
                if let Some(m) = mask_lit {
                    if (0..=i16::MAX as i32).contains(&m) {
                        let other = if matches!(r.as_ref(), Expr::Number(_)) {
                            l
                        } else {
                            r
                        };
                        let upper = match expr_int_range(other, state) {
                            Some(or) if or.fits_i16() => or.max.min(m).max(0),
                            _ => m,
                        };
                        return Some(IntRange::new(0, upper));
                    }
                }
            }
            let a = expr_int_range(l, state)?;
            let b = expr_int_range(r, state)?;
            match op {
                BinOp::Add => a.add(b),
                BinOp::Sub => a.sub(b),
                BinOp::Mul => a.mul(b),
                BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                    Some(compare_int_range(*op, a, b))
                }
                BinOp::And => {
                    // Non-constant-mask fallback: both operands
                    // need known i16 ranges before we can claim
                    // anything about the bitwise result.
                    if !(a.fits_i16() && b.fits_i16()) {
                        return None;
                    }
                    // Both u8 → result u8 (bitwise AND of two
                    // unsigned bytes can't exceed a byte).
                    if a.fits_u8() && b.fits_u8() {
                        return Some(IntRange::u8());
                    }
                    Some(IntRange::i16())
                }
                BinOp::Or => {
                    if !(a.fits_i16() && b.fits_i16()) {
                        return None;
                    }
                    // Both operands non-negative and fit u8 → result
                    // also fits u8 (the bitwise OR of two unsigned
                    // bytes can't overflow a byte). Useful for hot
                    // mask-set patterns like `b% OR $80` where
                    // downstream sees the result as still byte-sized.
                    if a.fits_u8() && b.fits_u8() {
                        return Some(IntRange::u8());
                    }
                    Some(IntRange::i16())
                }
                BinOp::Xor => {
                    if !(a.fits_i16() && b.fits_i16()) {
                        return None;
                    }
                    if a.fits_u8() && b.fits_u8() {
                        return Some(IntRange::u8());
                    }
                    Some(IntRange::i16())
                }
                BinOp::Div | BinOp::Pow => None,
            }
        }
        Expr::Func1(f, arg) => {
            let r = expr_int_range(arg, state)?;
            match f {
                Func1::Abs => r.abs(),
                Func1::Int => Some(r),
                Func1::Sgn => Some(IntRange::new(-1, 1)),
                _ => None,
            }
        }
        Expr::Peek(_) | Expr::MemPeek(_) => Some(IntRange::u8()),
        Expr::ArrayRef(name, _) if name.kind == VarKind::Integer => Some(IntRange::i16()),
        Expr::ArrayRef(_, _) => None,
        Expr::Len(_) => Some(IntRange::u8()),
        Expr::Asc(_) => Some(IntRange::u8()),
        Expr::StrCompare(_, _, _) => Some(IntRange::new(-1, 0)),
        Expr::Pos(_) => Some(IntRange::new(0, 39)),
        Expr::Joy(_) => Some(IntRange::new(0, 136)),
        Expr::Pot(_) | Expr::Inkey => Some(IntRange::u8()),
        Expr::Lin => Some(IntRange::new(0, 24)),
        Expr::At(_, _) => Some(IntRange::u8()),
        Expr::Test(_, _) => Some(IntRange::u8()),
        Expr::Check { .. } => Some(IntRange::new(0, 1)),
        Expr::Inst { .. } => Some(IntRange::u8()),
        Expr::Fre(_) | Expr::Val(_) | Expr::Nrm(_) | Expr::FnCall(_, _) | Expr::Usr(_) => None,
    }
}

fn compare_int_range(op: BinOp, a: IntRange, b: IntRange) -> IntRange {
    let always_true = match op {
        BinOp::Eq => a.min == a.max && b.min == b.max && a.min == b.min,
        BinOp::Ne => a.max < b.min || b.max < a.min,
        BinOp::Lt => a.max < b.min,
        BinOp::Le => a.max <= b.min,
        BinOp::Gt => a.min > b.max,
        BinOp::Ge => a.min >= b.max,
        _ => false,
    };
    let always_false = match op {
        BinOp::Eq => a.max < b.min || b.max < a.min,
        BinOp::Ne => a.min == a.max && b.min == b.max && a.min == b.min,
        BinOp::Lt => a.min >= b.max,
        BinOp::Le => a.min > b.max,
        BinOp::Gt => a.max <= b.min,
        BinOp::Ge => a.max < b.min,
        _ => false,
    };
    if always_true {
        IntRange::singleton(-1)
    } else if always_false {
        IntRange::singleton(0)
    } else {
        IntRange::new(-1, 0)
    }
}

#[derive(Default)]
struct ShadowWriteInfo {
    writes: HashMap<VarName, Vec<usize>>,
    bad: HashSet<VarName>,
    scalar_used: HashSet<VarName>,
    array_used: HashSet<VarName>,
    /// Reads of v that occur in syntactic positions where the codegen
    /// would actually take the int-fast path and read the shadow —
    /// POKE/PEEK byte args, WAIT/SYS, ArrayRef index, ON-GOTO index,
    /// LET-to-int RHS, plus inner positions of int-safe operator
    /// chains rooted in those. Used by the cost-model gate to keep
    /// the shadow's setup cost outweighed by read savings.
    int_ctx_reads: HashMap<VarName, usize>,
    /// Reads of v that route through FAC — anywhere we walked
    /// with `ShadowCtx::Float`. When this is zero for a shadow
    /// candidate, codegen can skip the V_var MOVMF sync at every
    /// write (the shadow holds the only observable copy).
    fac_ctx_reads: HashMap<VarName, usize>,
    /// Vars whose write set includes a READ — those writes are
    /// always preceded by a FAC roundtrip (\`__VAL_HELPER\` produces
    /// FAC), so the shadow sync needs an extra \`__FAC_TO_INT16\` +
    /// 2-byte store inline (~10 bytes per site). Used to tighten
    /// the cost-model gate for READ-fed candidates: factored helper
    /// loads (PH301) already make non-shadow reads cheap, so
    /// shadow only wins when reads outnumber writes by a healthy
    /// margin.
    read_writes: HashSet<VarName>,
    /// Vars whose write set includes an INPUT. The non-trapping
    /// sync helper used at INPUT sites costs ~5 bytes more than
    /// the regular FAC→i16 path, plus the helper definition itself
    /// (~30 bytes one-time). Even tighter gate than READ-fed.
    input_writes: HashSet<VarName>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShadowCtx {
    Int,
    Float,
}

fn compute_shadow_int_vars(
    module: &Module,
    cfg: &Cfg,
    per_node: &[(NumericState, NumericState)],
    data_range: Option<IntRange>,
) -> (HashSet<VarName>, HashMap<VarName, usize>, HashSet<VarName>) {
    let mut info = ShadowWriteInfo::default();
    for (id, _node) in cfg.nodes.iter().enumerate() {
        let stmt = cfg.stmt_at(id, module);
        collect_shadow_stmt(stmt, id, data_range, &mut info);
    }

    for v in info.scalar_used.intersection(&info.array_used) {
        info.bad.insert(v.clone());
    }

    let mut out = HashSet::new();
    for (var, writes) in &info.writes {
        if var.kind != VarKind::Float || var.base == "TI" || var.base == "ST" {
            continue;
        }
        if info.bad.contains(var) || writes.is_empty() {
            continue;
        }
        let input_fed = info.input_writes.contains(var);
        let all_i16 = writes.iter().all(|&node_id| {
            // Standard path: out-state's range for `var` fits i16.
            let direct = per_node
                .get(node_id)
                .and_then(|(_, out_state)| out_state.get(var))
                .map_or(false, IntRange::fits_i16);
            if direct {
                return true;
            }
            // Self-modifying write (`var = var ± const`) escape
            // hatch: the dataflow's range-clamp eventually kills
            // self-incrementing vars in unbounded loops, but typical
            // BASIC programs bound the iteration count via an IF
            // exit condition that the analysis doesn't model.
            // Accept the step if it's `var ± const` with a small
            // constant — runtime stays in i16 for any sensible loop.
            let stmt = cfg.stmt_at(node_id, module);
            if stmt_self_modify_small_step(stmt, var) {
                return true;
            }
            // INPUT writes leave the post-state unbounded; the
            // codegen syncs the shadow via the non-trapping helper,
            // which stamps 0 on overflow rather than trapping. The
            // shadow's low byte is correct for any value the user
            // could have entered (FACWORD wraps mod 65536 inside
            // the safe-range check), so we accept INPUT as a sound
            // "i16 fact" for the purposes of this gate. Other
            // writes still need to fit i16 the standard way.
            input_fed && matches!(stmt, Stmt::Input { .. })
        });
        if !all_i16 {
            continue;
        }
        // Cost model: each shadowed write adds ~6 bytes (sync to
        // shadow slot + still write to the float slot for any FAC
        // consumers). Each int-context read saves ~3 bytes (LDA/LDY
        // shadow vs. JSR FAC-load + convert). Counted reads are
        // those whose entire enclosing subtree is int-shaped — i.e.,
        // codegen will actually take the int-island path and read
        // the shadow. Require ≥2× to leave headroom for the cases
        // where we still over-count (e.g., subtree references a
        // Float scalar that doesn't end up shadowed).
        let n_writes = writes.len();
        let n_int_reads = info.int_ctx_reads.get(var).copied().unwrap_or(0);
        if n_int_reads == 0 {
            continue;
        }
        // Cost gate. Each int-context read of a shadowed var saves
        // ~10 bytes across the int-island forms (AND/OR/NOT/Neg/PEEK/
        // integer-ArrayRef/u16-addr): it skips GIVAYF/PASSY +
        // FAC_TO_INT16 + the LINNUM stash. The shadow slot costs 2 bytes
        // BSS plus ~5 bytes per write to keep V_var and the shadow in
        // sync. So a var is worth shadowing once it has at least one
        // int-context read and its read count is not strictly less than
        // its write count; at that 1:1 ratio per-iteration reads in
        // tight loops already dominate.
        //
        // Vars fed by INPUT/READ use a tighter gate. INPUT writes also
        // drag in the ~30-byte non-trapping helper, so they need the
        // strictest threshold; READ writes pay only the FAC_TO_INT16
        // helper (often already used elsewhere). LET/GET-fed vars use
        // the plain 1× ratio.
        let read_fed = info.read_writes.contains(var);
        let min_reads = if input_fed {
            n_writes.saturating_mul(3).max(n_writes + 3)
        } else if read_fed {
            n_writes.saturating_mul(2).max(n_writes + 2)
        } else {
            n_writes
        };
        if n_int_reads < min_reads {
            continue;
        }
        out.insert(var.clone());
    }
    let shadow_only_vars: HashSet<VarName> = out
        .iter()
        .filter(|v| !info.fac_ctx_reads.contains_key(*v))
        .cloned()
        .collect();
    (out, info.int_ctx_reads, shadow_only_vars)
}

fn collect_shadow_stmt(
    stmt: &Stmt,
    node_id: usize,
    data_range: Option<IntRange>,
    info: &mut ShadowWriteInfo,
) {
    match stmt {
        Stmt::Let { var, value } => {
            info.scalar_used.insert(var.clone());
            info.writes.entry(var.clone()).or_default().push(node_id);
            // RHS is int-context iff the destination is itself an
            // integer (or a candidate-shadow Float, which we approximate
            // as "any Float scalar" — same as the eligibility filter).
            let ctx = match var.kind {
                VarKind::Integer => ShadowCtx::Int,
                VarKind::Float => ShadowCtx::Int,
                _ => ShadowCtx::Float,
            };
            collect_shadow_expr(value, ctx, info);
        }
        Stmt::LetStr { var, value } => {
            info.scalar_used.insert(var.clone());
            info.bad.insert(var.clone());
            collect_shadow_str(value, info);
        }
        Stmt::For {
            var,
            start,
            end,
            step,
            ..
        } => {
            info.scalar_used.insert(var.clone());
            info.writes.entry(var.clone()).or_default().push(node_id);
            // FOR header expressions only take the int-fast path when
            // start/end/step are all literal. With a Var operand the
            // codegen evaluates to FAC (and stores to a float slot) —
            // shadow reads don't fire — so charging Int context here
            // would over-count. Treat as Float.
            collect_shadow_expr(start, ShadowCtx::Float, info);
            collect_shadow_expr(end, ShadowCtx::Float, info);
            collect_shadow_expr(step, ShadowCtx::Float, info);
        }
        Stmt::Get { var } => {
            info.scalar_used.insert(var.clone());
            // GET produces a 0..=9 byte (per `__GET_NUM`) — well
            // within i16, so the shadow gate's `all_i16` check
            // passes for the post-state range we set in
            // `transfer_numeric_stmt`. Counting GET as a write
            // (rather than poisoning the var) lets shadow-int
            // promotion fire for hot \`GET A : POKE x, A AND 255\`
            // shapes. Paired with the u8 var-compare fast path so
            // \`IF A = const\` doesn't regress vs the FAC route.
            info.writes.entry(var.clone()).or_default().push(node_id);
        }
        Stmt::GetFile { vars, .. } => {
            for var in vars {
                info.scalar_used.insert(var.clone());
                info.bad.insert(var.clone());
            }
        }
        Stmt::Read(targets) => {
            // READ targets count as writes when DATA is bounded —
            // mirrors GET, just with a wider 0..=DATA-max range.
            // Falls back to the legacy bad-insert path when DATA
            // is unbounded (any value outside i16 or non-finite),
            // since we can't promise an i16 fact at the write site.
            for target in targets {
                match target {
                    crate::ir::ReadTarget::Scalar(v) => match (data_range, v.kind) {
                        (Some(_), VarKind::Integer | VarKind::Float)
                            if v.base != "TI" && v.base != "ST" =>
                        {
                            info.scalar_used.insert(v.clone());
                            info.writes.entry(v.clone()).or_default().push(node_id);
                            info.read_writes.insert(v.clone());
                        }
                        _ => {
                            info.scalar_used.insert(v.clone());
                            info.bad.insert(v.clone());
                        }
                    },
                    crate::ir::ReadTarget::Array { name, indices } => {
                        info.array_used.insert(name.clone());
                        info.bad.insert(name.clone());
                        for e in indices {
                            collect_shadow_expr(e, ShadowCtx::Int, info);
                        }
                    }
                }
            }
        }
        Stmt::Input { targets, .. } => {
            // INPUT to a numeric scalar counts as a write — the
            // codegen's `emit_sync_shadow_from_fac_notrap` keeps
            // the shadow in sync without trapping on out-of-range
            // user input, so we can stage shadow promotion the
            // same way GET / READ do. The cost gate (see
            // `compute_shadow_int_vars`) treats INPUT-fed vars
            // even more strictly than READ-fed because each write
            // pays the helper's overflow check on top of FAC→i16.
            for target in targets {
                match target {
                    crate::ir::ReadTarget::Scalar(v) => match v.kind {
                        VarKind::Integer | VarKind::Float if v.base != "TI" && v.base != "ST" => {
                            info.scalar_used.insert(v.clone());
                            info.writes.entry(v.clone()).or_default().push(node_id);
                            info.input_writes.insert(v.clone());
                        }
                        _ => {
                            info.scalar_used.insert(v.clone());
                            info.bad.insert(v.clone());
                        }
                    },
                    crate::ir::ReadTarget::Array { name, indices } => {
                        info.array_used.insert(name.clone());
                        info.bad.insert(name.clone());
                        for e in indices {
                            collect_shadow_expr(e, ShadowCtx::Int, info);
                        }
                    }
                }
            }
        }
        Stmt::InputFile { targets, .. } => collect_shadow_read_targets(targets, info),
        Stmt::ArrayLet {
            name,
            indices,
            value,
        } => {
            info.array_used.insert(name.clone());
            for e in indices {
                collect_shadow_expr(e, ShadowCtx::Int, info);
            }
            let ctx = if name.kind == VarKind::Integer {
                ShadowCtx::Int
            } else {
                ShadowCtx::Float
            };
            collect_shadow_expr(value, ctx, info);
        }
        Stmt::ArrayLetStr {
            name,
            indices,
            value,
        } => {
            info.array_used.insert(name.clone());
            info.bad.insert(name.clone());
            for e in indices {
                collect_shadow_expr(e, ShadowCtx::Int, info);
            }
            collect_shadow_str(value, info);
        }
        Stmt::Dim(specs) => {
            for spec in specs {
                info.array_used.insert(spec.name.clone());
                for e in &spec.dims {
                    collect_shadow_expr(e, ShadowCtx::Int, info);
                }
            }
        }
        Stmt::DefFn { param, body, .. } => {
            info.scalar_used.insert(param.clone());
            info.bad.insert(param.clone());
            collect_shadow_expr(body, ShadowCtx::Float, info);
        }
        Stmt::Poke { addr, value } => {
            collect_shadow_expr(addr, ShadowCtx::Int, info);
            collect_shadow_expr(value, ShadowCtx::Int, info);
        }
        Stmt::Dpoke { addr, value } => {
            collect_shadow_expr(addr, ShadowCtx::Int, info);
            collect_shadow_expr(value, ShadowCtx::Int, info);
        }
        Stmt::ScreenRect {
            row,
            col,
            width,
            height,
            ch,
            color,
            ..
        } => {
            for e in [row, col, width, height] {
                collect_shadow_expr(e, ShadowCtx::Int, info);
            }
            if let Some(e) = ch {
                collect_shadow_expr(e, ShadowCtx::Int, info);
            }
            if let Some(e) = color {
                collect_shadow_expr(e, ShadowCtx::Int, info);
            }
        }
        Stmt::ScreenMove {
            row,
            col,
            width,
            height,
            dest_row,
            dest_col,
        } => {
            for e in [row, col, width, height, dest_row, dest_col] {
                collect_shadow_expr(e, ShadowCtx::Int, info);
            }
        }
        Stmt::ScreenScroll {
            row,
            col,
            width,
            height,
            ..
        } => {
            for e in [row, col, width, height] {
                collect_shadow_expr(e, ShadowCtx::Int, info);
            }
        }
        Stmt::Color {
            border,
            background,
            pen,
        } => {
            for e in border.iter().chain(background.iter()).chain(pen.iter()) {
                collect_shadow_expr(e, ShadowCtx::Int, info);
            }
        }
        Stmt::MobEnable { index, .. } => collect_shadow_expr(index, ShadowCtx::Int, info),
        Stmt::Multi { .. } | Stmt::HiCol => {}
        Stmt::MultiColors { c1, c2, c3 } => {
            collect_shadow_expr(c1, ShadowCtx::Int, info);
            collect_shadow_expr(c2, ShadowCtx::Int, info);
            collect_shadow_expr(c3, ShadowCtx::Int, info);
        }
        Stmt::Sound { voice, freq } => {
            collect_shadow_expr(voice, ShadowCtx::Int, info);
            collect_shadow_expr(freq, ShadowCtx::Int, info);
        }
        Stmt::Envelope {
            voice,
            attack,
            decay,
            sustain,
            release,
        } => {
            for e in [voice, attack, decay, sustain, release] {
                collect_shadow_expr(e, ShadowCtx::Int, info);
            }
        }
        Stmt::Wave {
            voice,
            control,
            pulse,
        } => {
            collect_shadow_expr(voice, ShadowCtx::Int, info);
            collect_shadow_expr(control, ShadowCtx::Int, info);
            if let Some(e) = pulse {
                collect_shadow_expr(e, ShadowCtx::Int, info);
            }
        }
        Stmt::LowCol {
            color1,
            color2,
            color3,
        } => {
            collect_shadow_expr(color1, ShadowCtx::Int, info);
            collect_shadow_expr(color2, ShadowCtx::Int, info);
            if let Some(e) = color3 {
                collect_shadow_expr(e, ShadowCtx::Int, info);
            }
        }
        Stmt::Mmob { index, x, y } => {
            collect_shadow_expr(index, ShadowCtx::Int, info);
            collect_shadow_expr(x, ShadowCtx::Int, info);
            collect_shadow_expr(y, ShadowCtx::Int, info);
        }
        Stmt::MmobGlide {
            index,
            sx,
            sy,
            ex,
            ey,
            size,
            speed,
        } => {
            for e in [index, sx, sy, ex, ey] {
                collect_shadow_expr(e, ShadowCtx::Int, info);
            }
            if let Some(e) = size {
                collect_shadow_expr(e, ShadowCtx::Int, info);
            }
            if let Some(e) = speed {
                collect_shadow_expr(e, ShadowCtx::Int, info);
            }
        }
        Stmt::MobSet {
            index,
            block,
            color,
            priority,
            multicolor,
            size,
            speed,
        } => {
            for e in [index, block, color, priority, multicolor] {
                collect_shadow_expr(e, ShadowCtx::Int, info);
            }
            if let Some(e) = size {
                collect_shadow_expr(e, ShadowCtx::Int, info);
            }
            if let Some(e) = speed {
                collect_shadow_expr(e, ShadowCtx::Int, info);
            }
        }
        Stmt::Rlocmob {
            index,
            dx,
            dy,
            speed,
        } => {
            collect_shadow_expr(index, ShadowCtx::Int, info);
            collect_shadow_expr(dx, ShadowCtx::Int, info);
            collect_shadow_expr(dy, ShadowCtx::Int, info);
            if let Some(e) = speed {
                collect_shadow_expr(e, ShadowCtx::Int, info);
            }
        }
        Stmt::Detect { mode } => collect_shadow_expr(mode, ShadowCtx::Int, info),
        Stmt::Cmob { color1, color2 } => {
            collect_shadow_expr(color1, ShadowCtx::Int, info);
            collect_shadow_expr(color2, ShadowCtx::Int, info);
        }
        Stmt::Bckgnds {
            color0,
            color1,
            color2,
            color3,
        } => {
            collect_shadow_expr(color0, ShadowCtx::Int, info);
            collect_shadow_expr(color1, ShadowCtx::Int, info);
            collect_shadow_expr(color2, ShadowCtx::Int, info);
            collect_shadow_expr(color3, ShadowCtx::Int, info);
        }
        Stmt::Cset { mode } => collect_shadow_expr(mode, ShadowCtx::Int, info),
        Stmt::Pause { message, ticks } => {
            if let Some(m) = message {
                collect_shadow_str(m, info);
            }
            collect_shadow_expr(ticks, ShadowCtx::Int, info);
        }
        Stmt::Sys { addr, regs } => {
            collect_shadow_expr(addr, ShadowCtx::Int, info);
            for r in regs {
                collect_shadow_expr(r, ShadowCtx::Int, info);
            }
        }
        Stmt::Wait { addr, mask, eor } => {
            collect_shadow_expr(addr, ShadowCtx::Int, info);
            collect_shadow_expr(mask, ShadowCtx::Int, info);
            if let Some(e) = eor {
                collect_shadow_expr(e, ShadowCtx::Int, info);
            }
        }
        Stmt::OnBranch { value, .. } => collect_shadow_expr(value, ShadowCtx::Int, info),
        Stmt::If { cond, then } => {
            // IF cond reads are int-context when codegen will take
            // the int-compare fast path (`try_emit_if_int_compare`),
            // which fires whenever both compare operands route
            // through `int16_leaf` — Integer scalars, ZP / BSS
            // shadow-int Float scalars, active FOR counters,
            // integer ArrayRef. Treating these as int-context lets
            // shadow promote on tight `IF C < D` shapes where the
            // int compare is both smaller AND faster than the FAC
            // alternative.
            collect_shadow_expr(cond, ShadowCtx::Int, info);
            if let crate::ir::ThenIr::Stmts(stmts) = then {
                for s in stmts {
                    collect_shadow_stmt(s, node_id, data_range, info);
                }
            }
        }
        Stmt::IfElse {
            cond,
            then,
            else_then,
        } => {
            collect_shadow_expr(cond, ShadowCtx::Int, info);
            collect_shadow_then(then, node_id, data_range, info);
            collect_shadow_then(else_then, node_id, data_range, info);
        }
        Stmt::DoIf { cond } | Stmt::Until { cond } => {
            collect_shadow_expr(cond, ShadowCtx::Int, info);
        }
        Stmt::ExitLoop { cond } => {
            if let Some(cond) = cond {
                collect_shadow_expr(cond, ShadowCtx::Int, info);
            }
        }
        Stmt::ComputedGoto { target } => collect_shadow_expr(target, ShadowCtx::Int, info),
        Stmt::Rcomp { then, else_then } => {
            collect_shadow_then(then, node_id, data_range, info);
            if let Some(else_then) = else_then {
                collect_shadow_then(else_then, node_id, data_range, info);
            }
        }
        Stmt::OnKey { keys, .. } => collect_shadow_str(keys, info),
        _ => {
            // Fall back to a context-blind walk for the long tail
            // (PRINT, OPEN/CLOSE/LOAD/SAVE/CMD, etc.) — these are
            // dominated by Float-context uses (CHROUT, FAC), so we
            // count reads as Float by default.
            let mut collector = ShadowExprCollector {
                info,
                ctx: ShadowCtx::Float,
            };
            crate::visit::Visitor::visit_stmt(&mut collector, 0, stmt);
        }
    }
}

fn collect_shadow_then(
    then: &crate::ir::ThenIr,
    node_id: usize,
    data_range: Option<IntRange>,
    info: &mut ShadowWriteInfo,
) {
    if let crate::ir::ThenIr::Stmts(stmts) = then {
        for s in stmts {
            collect_shadow_stmt(s, node_id, data_range, info);
        }
    }
}

fn collect_shadow_read_targets(targets: &[ReadTarget], info: &mut ShadowWriteInfo) {
    for target in targets {
        match target {
            ReadTarget::Scalar(v) => {
                info.scalar_used.insert(v.clone());
                info.bad.insert(v.clone());
            }
            ReadTarget::Array { name, indices } => {
                info.array_used.insert(name.clone());
                info.bad.insert(name.clone());
                for e in indices {
                    collect_shadow_expr(e, ShadowCtx::Int, info);
                }
            }
        }
    }
}

fn collect_shadow_expr(e: &Expr, ctx: ShadowCtx, info: &mut ShadowWriteInfo) {
    walk_expr_ctx(e, ctx, info);
}

fn collect_shadow_str(s: &StrExpr, info: &mut ShadowWriteInfo) {
    let mut collector = ShadowExprCollector {
        info,
        ctx: ShadowCtx::Float,
    };
    crate::visit::Visitor::visit_str_expr(&mut collector, s);
}

fn walk_expr_ctx(e: &Expr, ctx: ShadowCtx, info: &mut ShadowWriteInfo) {
    // For Int context we treat `e` as the enclosing "island root":
    // a Var read of v counts as int-context iff the root is still
    // an int-island when we treat v as the (potentially-promoted)
    // self leaf and require every OTHER leaf to be already-int.
    // That matches what codegen actually does — taking the int-fast
    // path for a whole expression, only for it to fail because some
    // sibling leaf is a Float scalar that didn't get shadowed.
    walk_expr_inner(e, ctx, e, info);
}

fn walk_expr_inner(e: &Expr, ctx: ShadowCtx, root: &Expr, info: &mut ShadowWriteInfo) {
    match e {
        Expr::Var(v) => {
            info.scalar_used.insert(v.clone());
            if ctx == ShadowCtx::Int {
                let in_island = expr_potential_int_island_for(root, v, &info.bad);
                if in_island {
                    *info.int_ctx_reads.entry(v.clone()).or_default() += 1;
                } else {
                    // Int-context syntactically, but the int-island
                    // gate disqualifies the surrounding tree (e.g.
                    // a Func1/FnCall sibling forces the parent to
                    // FAC). Codegen falls back to FAC for this read,
                    // so it counts toward the FAC-read tally.
                    *info.fac_ctx_reads.entry(v.clone()).or_default() += 1;
                }
            } else {
                *info.fac_ctx_reads.entry(v.clone()).or_default() += 1;
            }
        }
        Expr::Number(_) | Expr::String(_) => {}
        Expr::Neg(inner) | Expr::Not(inner) => walk_expr_inner(inner, ctx, root, info),
        Expr::Bin(_, l, r) => {
            walk_expr_inner(l, ctx, root, info);
            walk_expr_inner(r, ctx, root, info);
        }
        Expr::Func1(_, arg) => walk_expr_inner(arg, ctx, root, info),
        Expr::Peek(addr) | Expr::MemPeek(addr) => walk_expr_ctx(addr, ShadowCtx::Int, info),
        Expr::ArrayRef(name, idx) => {
            info.array_used.insert(name.clone());
            for e in idx {
                walk_expr_ctx(e, ShadowCtx::Int, info);
            }
        }
        Expr::Len(s) | Expr::Asc(s) | Expr::Val(s) | Expr::Nrm(s) => collect_shadow_str(s, info),
        Expr::StrCompare(_, a, b) => {
            collect_shadow_str(a, info);
            collect_shadow_str(b, info);
        }
        Expr::Pos(arg) | Expr::Fre(arg) | Expr::Usr(arg) => {
            walk_expr_inner(arg, ShadowCtx::Float, arg, info);
        }
        Expr::Joy(arg) | Expr::Pot(arg) => walk_expr_ctx(arg, ShadowCtx::Int, info),
        Expr::At(row, col) => {
            walk_expr_ctx(row, ShadowCtx::Int, info);
            walk_expr_ctx(col, ShadowCtx::Int, info);
        }
        Expr::Test(x, y) => {
            walk_expr_ctx(x, ShadowCtx::Int, info);
            walk_expr_ctx(y, ShadowCtx::Int, info);
        }
        Expr::Check { first, second } => {
            walk_expr_ctx(first, ShadowCtx::Int, info);
            if let Some(e) = second {
                walk_expr_ctx(e, ShadowCtx::Int, info);
            }
        }
        Expr::Inst {
            haystack,
            needle,
            start,
        } => {
            collect_shadow_str(haystack, info);
            collect_shadow_str(needle, info);
            if let Some(e) = start {
                walk_expr_ctx(e, ShadowCtx::Int, info);
            }
        }
        Expr::Inkey | Expr::Lin => {}
        Expr::FnCall(_, arg) => walk_expr_inner(arg, ShadowCtx::Float, arg, info),
    }
}

/// Conservative variant of `expr_type::is_int_island` used for
/// counting shadow-favouring reads. It accepts the var being scored
/// (the "self" leaf) anywhere it appears, plus any other Float
/// scalar that's still a *plausible* shadow candidate (i.e. not in
/// the bad-set). The bad-set already excludes vars that codegen
/// can't shadow (READ/INPUT/GET/DefFn-param targets, strings,
/// arrays). The cost gate later filters Float candidates that
/// don't pay back; expressions like `PEEK(W+X+1)` where W and X
/// are both Float candidates count as int-context for both vars.
/// Local pot2-literal check — duplicated from passes.rs because
/// dataflow doesn't depend on passes. Same shape: a finite,
/// integer-valued, positive power of two that fits a u32.
fn is_pot2_literal_local(e: &Expr) -> bool {
    if let Expr::Number(n) = e {
        if *n > 0.0 && n.fract() == 0.0 && *n <= u32::MAX as f64 {
            let v = *n as u32;
            return v.is_power_of_two();
        }
    }
    false
}

fn expr_potential_int_island_for(e: &Expr, self_var: &VarName, bad: &HashSet<VarName>) -> bool {
    match e {
        Expr::Number(n) => n.is_finite() && n.fract() == 0.0 && (-32768.0..=32767.0).contains(n),
        Expr::Var(v) => {
            if v == self_var {
                return true;
            }
            if v.base == "TI" || v.base == "ST" {
                return false;
            }
            if v.kind == VarKind::Integer {
                return true;
            }
            // Float scalar: assume it's also a shadow candidate
            // unless the bad-set already disqualifies it.
            v.kind == VarKind::Float && !bad.contains(v)
        }
        Expr::Neg(inner) | Expr::Not(inner) => expr_potential_int_island_for(inner, self_var, bad),
        Expr::Bin(BinOp::Mul, l, r) => {
            // pot2 Mul lowers to ASL/ROL chain — pure int.
            let pot2 = is_pot2_literal_local(l) || is_pot2_literal_local(r);
            if pot2 {
                return expr_potential_int_island_for(l, self_var, bad)
                    && expr_potential_int_island_for(r, self_var, bad);
            }
            // Number * Number folds at compile time.
            if matches!(l.as_ref(), Expr::Number(_)) && matches!(r.as_ref(), Expr::Number(_)) {
                return true;
            }
            // Anything else with a Var operand may lower through FAC
            // math. Treating that as int-routable can leave V_var stale.
            false
        }
        Expr::Bin(op, l, r) => {
            matches!(
                op,
                BinOp::Add
                    | BinOp::Sub
                    | BinOp::And
                    | BinOp::Or
                    | BinOp::Xor
                    | BinOp::Eq
                    | BinOp::Ne
                    | BinOp::Lt
                    | BinOp::Le
                    | BinOp::Gt
                    | BinOp::Ge
            ) && expr_potential_int_island_for(l, self_var, bad)
                && expr_potential_int_island_for(r, self_var, bad)
        }
        Expr::Func1(f, arg) => {
            matches!(f, Func1::Abs | Func1::Int | Func1::Sgn)
                && expr_potential_int_island_for(arg, self_var, bad)
        }
        Expr::Peek(addr) | Expr::MemPeek(addr) => {
            expr_potential_int_island_for(addr, self_var, bad)
        }
        Expr::Len(_) | Expr::Pos(_) | Expr::Fre(_) => true,
        Expr::Asc(_) => false,
        Expr::Val(_)
        | Expr::Nrm(_)
        | Expr::Usr(_)
        | Expr::FnCall(_, _)
        | Expr::Joy(_)
        | Expr::Pot(_)
        | Expr::At(_, _)
        | Expr::Test(_, _)
        | Expr::Check { .. }
        | Expr::Inst { .. }
        | Expr::Inkey
        | Expr::Lin
        | Expr::String(_) => false,
        Expr::ArrayRef(name, idx) => {
            name.kind == VarKind::Integer
                && idx
                    .iter()
                    .all(|e| expr_potential_int_island_for(e, self_var, bad))
        }
        Expr::StrCompare(_, _, _) => false,
    }
}

struct ShadowExprCollector<'a> {
    info: &'a mut ShadowWriteInfo,
    ctx: ShadowCtx,
}

impl<'a> crate::visit::Visitor for ShadowExprCollector<'a> {
    fn visit_var_read(&mut self, v: &VarName) {
        self.info.scalar_used.insert(v.clone());
        // Fallback path (only used for stmts not handled in the
        // explicit context-aware dispatch). Treated as Float ctx,
        // so int_ctx_reads stays untouched here.
        if self.ctx == ShadowCtx::Int {
            *self.info.int_ctx_reads.entry(v.clone()).or_default() += 1;
        } else {
            *self.info.fac_ctx_reads.entry(v.clone()).or_default() += 1;
        }
    }

    fn visit_expr(&mut self, e: &Expr) {
        if let Expr::ArrayRef(name, _) = e {
            self.info.array_used.insert(name.clone());
        }
        crate::visit::walk_expr(self, e);
    }

    fn visit_str_expr(&mut self, s: &StrExpr) {
        if let StrExpr::ArrayRef(name, _) = s {
            self.info.array_used.insert(name.clone());
        }
        crate::visit::walk_str_expr(self, s);
    }
}

/// Variables this statement may write (a "def" in classical dataflow
/// terminology). LET / FOR header / INPUT / READ / GET targets count;
/// array element writes don't kill scalar liveness so they're left
/// out. NEXT writes the loop counter (implicit increment).
fn stmt_defs(stmt: &Stmt) -> Vec<VarName> {
    let mut out = Vec::new();
    match stmt {
        Stmt::Let { var, .. } | Stmt::LetStr { var, .. } => out.push(var.clone()),
        Stmt::For { var, .. } => out.push(var.clone()),
        Stmt::Next { vars } => {
            for v in vars.iter().flatten() {
                out.push(v.clone());
            }
        }
        Stmt::Get { var } | Stmt::KeyGet { var } => out.push(var.clone()),
        Stmt::GetFile { vars, .. } => {
            for v in vars {
                out.push(v.clone());
            }
        }
        Stmt::Fetch { target, .. } => out.push(target.clone()),
        Stmt::Read(targets) | Stmt::Input { targets, .. } => {
            for t in targets {
                if let crate::ir::ReadTarget::Scalar(v) = t {
                    out.push(v.clone());
                }
            }
        }
        Stmt::InputFile { targets, .. } => {
            for t in targets {
                if let crate::ir::ReadTarget::Scalar(v) = t {
                    out.push(v.clone());
                }
            }
        }
        // Clr nominally clears all variables, but modelling that as
        // killing every var would explode the lattice. Leave it out
        // — the conservative result is "vars stay live across CLR",
        // which is harmless since CLR also resets the runtime state.
        _ => {}
    }
    out
}

/// Variables this statement may read. Walks the IR via the Visitor
/// framework so every nested expression contributes. Plus a couple
/// of special cases the IR doesn't model explicitly — most notably
/// NEXT, which performs an implicit increment-and-compare on the
/// loop counter so the counter must be considered both read and
/// written.
fn stmt_uses(stmt: &Stmt) -> Vec<VarName> {
    let mut collector = UseCollector { uses: Vec::new() };
    crate::visit::Visitor::visit_stmt(&mut collector, 0, stmt);
    if let Stmt::Next { vars } = stmt {
        for v in vars.iter().flatten() {
            if !collector.uses.contains(v) {
                collector.uses.push(v.clone());
            }
        }
    }
    collector.uses
}

struct UseCollector {
    uses: Vec<VarName>,
}

impl crate::visit::Visitor for UseCollector {
    fn visit_var_read(&mut self, v: &VarName) {
        // De-dup at insertion time — the bag grows small in practice.
        if !self.uses.contains(v) {
            self.uses.push(v.clone());
        }
    }
}

// ===== Tests ==============================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::Registry;
    use crate::ast::{BinOp, VarKind, VarName};
    use crate::ir::{Expr, Line, Module, Stmt};

    fn fvar(name: &str) -> VarName {
        VarName {
            base: name.to_string(),
            kind: VarKind::Float,
        }
    }

    fn ivar(name: &str) -> VarName {
        VarName {
            base: name.to_string(),
            kind: VarKind::Integer,
        }
    }

    #[test]
    fn live_vars_simple_let_print() {
        // 10 X = 5
        // 20 PRINT X
        // 30 END
        let m = Module {
            lines: vec![
                Line {
                    number: 10,
                    stmts: vec![Stmt::Let {
                        var: fvar("X"),
                        value: Expr::Number(5.0),
                    }],
                },
                Line {
                    number: 20,
                    stmts: vec![Stmt::Print {
                        items: vec![crate::ir::PrintPiece::Expr(Expr::Var(fvar("X")))],
                        newline: true,
                    }],
                },
                Line {
                    number: 30,
                    stmts: vec![Stmt::End],
                },
            ],
        };
        let mut reg = Registry::new();
        let live = reg.get(&m, &LiveVars).clone();
        // Node 0 = LET X=5. After it, X is live (used in PRINT). Before
        // it, X is NOT live (the LET both defs and the prior uses are
        // empty).
        let live_after_let = &live.per_node[0].1;
        assert!(live_after_let.contains(&fvar("X")));
        let live_before_let = &live.per_node[0].0;
        assert!(!live_before_let.contains(&fvar("X")));
        // Node 1 = PRINT X. Before it, X is live; after it, X is not
        // (no further uses).
        let live_before_print = &live.per_node[1].0;
        let live_after_print = &live.per_node[1].1;
        assert!(live_before_print.contains(&fvar("X")));
        assert!(!live_after_print.contains(&fvar("X")));
    }

    #[test]
    fn live_vars_dead_store() {
        // 10 X = 5     ← dead: X never read
        // 20 X = 7     ← live: X read on line 30
        // 30 PRINT X
        // 40 END
        let m = Module {
            lines: vec![
                Line {
                    number: 10,
                    stmts: vec![Stmt::Let {
                        var: fvar("X"),
                        value: Expr::Number(5.0),
                    }],
                },
                Line {
                    number: 20,
                    stmts: vec![Stmt::Let {
                        var: fvar("X"),
                        value: Expr::Number(7.0),
                    }],
                },
                Line {
                    number: 30,
                    stmts: vec![Stmt::Print {
                        items: vec![crate::ir::PrintPiece::Expr(Expr::Var(fvar("X")))],
                        newline: true,
                    }],
                },
                Line {
                    number: 40,
                    stmts: vec![Stmt::End],
                },
            ],
        };
        let mut reg = Registry::new();
        let live = reg.get(&m, &LiveVars).clone();
        // After node 0 (X=5), X is NOT live — the next stmt (X=7)
        // immediately overwrites it. This is the dead-store signal.
        let after_first = &live.per_node[0].1;
        assert!(
            !after_first.contains(&fvar("X")),
            "store at line 10 is dead"
        );
        // After node 1 (X=7), X IS live — read on line 30.
        let after_second = &live.per_node[1].1;
        assert!(after_second.contains(&fvar("X")));
    }

    #[test]
    fn live_vars_for_loop_back_edge() {
        let i = fvar("I");
        let s = fvar("S");
        // 10 S = 0
        // 20 FOR I=1 TO 10
        // 30 S = S + I
        // 40 NEXT I
        // 50 PRINT S
        // 60 END
        let m = Module {
            lines: vec![
                Line {
                    number: 10,
                    stmts: vec![Stmt::Let {
                        var: s.clone(),
                        value: Expr::Number(0.0),
                    }],
                },
                Line {
                    number: 20,
                    stmts: vec![Stmt::For {
                        var: i.clone(),
                        start: Expr::Number(1.0),
                        end: Expr::Number(10.0),
                        step: Expr::Number(1.0),
                        body_int_safe: false,
                        body_reads_loop_var: false,
                        induction_const: None,
                        array_inductions: Vec::new(),
                    }],
                },
                Line {
                    number: 30,
                    stmts: vec![Stmt::Let {
                        var: s.clone(),
                        value: Expr::Bin(
                            crate::ast::BinOp::Add,
                            Box::new(Expr::Var(s.clone())),
                            Box::new(Expr::Var(i.clone())),
                        ),
                    }],
                },
                Line {
                    number: 40,
                    stmts: vec![Stmt::Next {
                        vars: vec![Some(i.clone())],
                    }],
                },
                Line {
                    number: 50,
                    stmts: vec![Stmt::Print {
                        items: vec![crate::ir::PrintPiece::Expr(Expr::Var(s.clone()))],
                        newline: true,
                    }],
                },
                Line {
                    number: 60,
                    stmts: vec![Stmt::End],
                },
            ],
        };
        let mut reg = Registry::new();
        let live = reg.get(&m, &LiveVars).clone();
        // Live-out at NEXT (node 3): I is live (next iteration reads
        // it via FOR's compare), S is live (later PRINT). The loop
        // back-edge is what makes I live here.
        let live_after_next = &live.per_node[3].1;
        assert!(
            live_after_next.contains(&i),
            "I must be live across NEXT back-edge"
        );
        assert!(
            live_after_next.contains(&s),
            "S must be live for later PRINT"
        );
    }

    #[test]
    fn interference_disjoint_lifetimes_do_not_conflict() {
        // 10 A = 1
        // 20 PRINT A     ' A dies here
        // 30 B = 2
        // 40 PRINT B     ' B's lifetime never overlaps A's
        // 50 END
        let a = ivar("A");
        let b = ivar("B");
        let m = Module {
            lines: vec![
                Line {
                    number: 10,
                    stmts: vec![Stmt::Let {
                        var: a.clone(),
                        value: Expr::Number(1.0),
                    }],
                },
                Line {
                    number: 20,
                    stmts: vec![Stmt::Print {
                        items: vec![crate::ir::PrintPiece::Expr(Expr::Var(a.clone()))],
                        newline: true,
                    }],
                },
                Line {
                    number: 30,
                    stmts: vec![Stmt::Let {
                        var: b.clone(),
                        value: Expr::Number(2.0),
                    }],
                },
                Line {
                    number: 40,
                    stmts: vec![Stmt::Print {
                        items: vec![crate::ir::PrintPiece::Expr(Expr::Var(b.clone()))],
                        newline: true,
                    }],
                },
                Line {
                    number: 50,
                    stmts: vec![Stmt::End],
                },
            ],
        };
        let mut reg = Registry::new();
        let g = reg.get(&m, &VarInterference).clone();
        // A is dead before B is born — they can share a ZP slot.
        assert!(
            !g.interferes(&a, &b),
            "A and B have disjoint lifetimes and must not interfere"
        );
    }

    #[test]
    fn interference_overlapping_lifetimes_conflict() {
        // 10 A = 1
        // 20 B = 2
        // 30 PRINT A + B   ' both live simultaneously here
        // 40 END
        let a = ivar("A");
        let b = ivar("B");
        let m = Module {
            lines: vec![
                Line {
                    number: 10,
                    stmts: vec![Stmt::Let {
                        var: a.clone(),
                        value: Expr::Number(1.0),
                    }],
                },
                Line {
                    number: 20,
                    stmts: vec![Stmt::Let {
                        var: b.clone(),
                        value: Expr::Number(2.0),
                    }],
                },
                Line {
                    number: 30,
                    stmts: vec![Stmt::Print {
                        items: vec![crate::ir::PrintPiece::Expr(Expr::Bin(
                            crate::ast::BinOp::Add,
                            Box::new(Expr::Var(a.clone())),
                            Box::new(Expr::Var(b.clone())),
                        ))],
                        newline: true,
                    }],
                },
                Line {
                    number: 40,
                    stmts: vec![Stmt::End],
                },
            ],
        };
        let mut reg = Registry::new();
        let g = reg.get(&m, &VarInterference).clone();
        // A is still live when B is defined (both read on line 30), so
        // they must NOT be allowed to share a slot.
        assert!(
            g.interferes(&a, &b),
            "A and B are simultaneously live and must interfere"
        );
        assert!(g.interferes(&b, &a), "interference is symmetric");
    }

    #[test]
    fn defined_before_use_accepts_write_then_read() {
        // 10 X = 5
        // 20 PRINT X     ' X always written before read -> safe
        let x = ivar("X");
        let m = Module {
            lines: vec![
                Line {
                    number: 10,
                    stmts: vec![Stmt::Let {
                        var: x.clone(),
                        value: Expr::Number(5.0),
                    }],
                },
                Line {
                    number: 20,
                    stmts: vec![Stmt::Print {
                        items: vec![crate::ir::PrintPiece::Expr(Expr::Var(x.clone()))],
                        newline: true,
                    }],
                },
            ],
        };
        let mut reg = Registry::new();
        let safe = reg.get(&m, &DefinedBeforeUse).clone();
        assert!(safe.contains(&x), "X is written before every read");
    }

    #[test]
    fn defined_before_use_rejects_self_modify_first_write() {
        // 10 X = X + 1   ' reads X (default 0) before it is ever written
        // 20 PRINT X
        let x = ivar("X");
        let m = Module {
            lines: vec![
                Line {
                    number: 10,
                    stmts: vec![Stmt::Let {
                        var: x.clone(),
                        value: Expr::Bin(
                            BinOp::Add,
                            Box::new(Expr::Var(x.clone())),
                            Box::new(Expr::Number(1.0)),
                        ),
                    }],
                },
                Line {
                    number: 20,
                    stmts: vec![Stmt::Print {
                        items: vec![crate::ir::PrintPiece::Expr(Expr::Var(x.clone()))],
                        newline: true,
                    }],
                },
            ],
        };
        let mut reg = Registry::new();
        let safe = reg.get(&m, &DefinedBeforeUse).clone();
        assert!(
            !safe.contains(&x),
            "X relies on its default-0 value at the first read"
        );
    }

    #[test]
    fn defined_before_use_rejects_goto_skipped_definition() {
        // 10 GOTO 30
        // 20 X = 5        ' skipped on the taken path
        // 30 PRINT X      ' X may be default-0 here -> unsafe
        let x = ivar("X");
        let m = Module {
            lines: vec![
                Line {
                    number: 10,
                    stmts: vec![Stmt::Goto { target: 30 }],
                },
                Line {
                    number: 20,
                    stmts: vec![Stmt::Let {
                        var: x.clone(),
                        value: Expr::Number(5.0),
                    }],
                },
                Line {
                    number: 30,
                    stmts: vec![Stmt::Print {
                        items: vec![crate::ir::PrintPiece::Expr(Expr::Var(x.clone()))],
                        newline: true,
                    }],
                },
            ],
        };
        let mut reg = Registry::new();
        let safe = reg.get(&m, &DefinedBeforeUse).clone();
        assert!(
            !safe.contains(&x),
            "X is not defined on the GOTO path that reaches its read"
        );
    }

    #[test]
    fn interference_self_is_never_a_conflict() {
        let a = ivar("A");
        let m = Module {
            lines: vec![
                Line {
                    number: 10,
                    stmts: vec![Stmt::Let {
                        var: a.clone(),
                        value: Expr::Number(1.0),
                    }],
                },
                Line {
                    number: 20,
                    stmts: vec![Stmt::Print {
                        items: vec![crate::ir::PrintPiece::Expr(Expr::Var(a.clone()))],
                        newline: true,
                    }],
                },
            ],
        };
        let mut reg = Registry::new();
        let g = reg.get(&m, &VarInterference).clone();
        assert!(!g.interferes(&a, &a));
    }

    #[test]
    fn for_counter_range_survives_in_loop_body() {
        // The NEXT back-edge must not flow a killed counter back into
        // the body, or NumericState's intersection join would wipe the
        // counter's range out of its own loop. The counter must stay
        // bounded to [start, end] inside the body.
        // 10 FOR I=1 TO 8
        // 20 A = I
        // 30 NEXT I
        let i = ivar("I");
        let a = ivar("A");
        let m = Module {
            lines: vec![
                Line {
                    number: 10,
                    stmts: vec![Stmt::For {
                        var: i.clone(),
                        start: Expr::Number(1.0),
                        end: Expr::Number(8.0),
                        step: Expr::Number(1.0),
                        body_int_safe: false,
                        body_reads_loop_var: true,
                        induction_const: None,
                        array_inductions: Vec::new(),
                    }],
                },
                Line {
                    number: 20,
                    stmts: vec![Stmt::Let {
                        var: a.clone(),
                        value: Expr::Var(i.clone()),
                    }],
                },
                Line {
                    number: 30,
                    stmts: vec![Stmt::Next {
                        vars: vec![Some(i.clone())],
                    }],
                },
            ],
        };
        let mut reg = Registry::new();
        let facts = reg.get(&m, &NumericFactAnalysis).clone();
        // Node 1 = LET A=I (the loop body). I must be known [1,8] on
        // entry, not wiped by the NEXT back-edge.
        let r = facts.per_node[1]
            .0
            .get(&i)
            .expect("loop counter must keep a range inside the body");
        assert_eq!((r.min, r.max), (1, 8));
    }

    #[test]
    fn numeric_facts_shadow_float_addsub_candidate() {
        let x = fvar("X");
        // Program: X is set to 1 then bumped by 2, and is then read
        // many times in int-favoring contexts (POKE addr/value pairs,
        // SYS addr, ON-GOTO index). With 1 write and ≥8 int-context
        // reads the cost-model gate should promote X.
        let mut lines = vec![Line {
            number: 10,
            stmts: vec![Stmt::Let {
                var: x.clone(),
                value: Expr::Number(3.0),
            }],
        }];
        // Generate 4 POKE statements (each POKE is 2 int-context reads).
        for i in 0..4 {
            lines.push(Line {
                number: 20 + i * 10,
                stmts: vec![Stmt::Poke {
                    addr: Expr::Var(x.clone()),
                    value: Expr::Var(x.clone()),
                }],
            });
        }
        let m = Module { lines };
        let mut reg = Registry::new();
        let facts = reg.get(&m, &NumericFactAnalysis).clone();
        assert!(facts.shadow_int_vars.contains(&x));
        assert_eq!(facts.per_node[0].1.get(&x), Some(IntRange::singleton(3)));
    }

    #[test]
    fn numeric_facts_exposes_range_by_stmt_path() {
        let x = fvar("X");
        let m = Module {
            lines: vec![
                Line {
                    number: 10,
                    stmts: vec![Stmt::Let {
                        var: x.clone(),
                        value: Expr::Number(5.0),
                    }],
                },
                Line {
                    number: 20,
                    stmts: vec![Stmt::Poke {
                        addr: Expr::Var(x.clone()),
                        value: Expr::Number(0.0),
                    }],
                },
            ],
        };
        let mut reg = Registry::new();
        let facts = reg.get(&m, &NumericFactAnalysis).clone();
        let path = crate::cfg::StmtPath {
            line_idx: 1,
            path: vec![0],
        };
        assert_eq!(
            facts.expr_int_range_before_path(&path, &Expr::Var(x)),
            Some(IntRange::singleton(5))
        );
    }

    #[test]
    fn numeric_facts_next_kills_loop_var_range_after_loop() {
        let i = fvar("I");
        let m = Module {
            lines: vec![
                Line {
                    number: 10,
                    stmts: vec![Stmt::For {
                        var: i.clone(),
                        start: Expr::Number(1.0),
                        end: Expr::Number(10.0),
                        step: Expr::Number(1.0),
                        body_int_safe: false,
                        body_reads_loop_var: true,
                        induction_const: None,
                        array_inductions: Vec::new(),
                    }],
                },
                Line {
                    number: 20,
                    stmts: vec![Stmt::Next {
                        vars: vec![Some(i.clone())],
                    }],
                },
                Line {
                    number: 30,
                    stmts: vec![Stmt::Poke {
                        addr: Expr::Var(i.clone()),
                        value: Expr::Number(0.0),
                    }],
                },
            ],
        };
        let mut reg = Registry::new();
        let facts = reg.get(&m, &NumericFactAnalysis).clone();
        let path = crate::cfg::StmtPath {
            line_idx: 2,
            path: vec![0],
        };
        assert_eq!(facts.expr_int_range_before_path(&path, &Expr::Var(i)), None);
    }

    #[test]
    fn numeric_facts_shadow_gate_rejects_low_read_to_write() {
        let x = fvar("X");
        // 2 writes, 1 int-context read — read count below write
        // count, so the cost model should still reject X under the
        // current ≥1 read/write gate.
        let m = Module {
            lines: vec![
                Line {
                    number: 10,
                    stmts: vec![Stmt::Let {
                        var: x.clone(),
                        value: Expr::Number(7.0),
                    }],
                },
                Line {
                    number: 20,
                    stmts: vec![Stmt::Let {
                        var: x.clone(),
                        value: Expr::Number(11.0),
                    }],
                },
                Line {
                    number: 30,
                    stmts: vec![Stmt::Poke {
                        addr: Expr::Var(x.clone()),
                        value: Expr::Number(0.0),
                    }],
                },
            ],
        };
        let mut reg = Registry::new();
        let facts = reg.get(&m, &NumericFactAnalysis).clone();
        assert!(!facts.shadow_int_vars.contains(&x));
    }

    #[test]
    fn numeric_facts_get_disqualifies_shadow_float() {
        let x = fvar("X");
        let m = Module {
            lines: vec![
                Line {
                    number: 10,
                    stmts: vec![Stmt::Let {
                        var: x.clone(),
                        value: Expr::Number(1.0),
                    }],
                },
                Line {
                    number: 20,
                    stmts: vec![Stmt::Get { var: x.clone() }],
                },
                Line {
                    number: 30,
                    stmts: vec![Stmt::Let {
                        var: x.clone(),
                        value: Expr::Number(2.0),
                    }],
                },
                Line {
                    number: 40,
                    stmts: vec![Stmt::Print {
                        items: vec![crate::ir::PrintPiece::Expr(Expr::Var(x.clone()))],
                        newline: true,
                    }],
                },
            ],
        };
        let mut reg = Registry::new();
        let facts = reg.get(&m, &NumericFactAnalysis).clone();
        assert!(!facts.shadow_int_vars.contains(&x));
    }

    #[test]
    fn numeric_facts_self_ref_loop_drops_shadow_candidate() {
        let x = fvar("X");
        let m = Module {
            lines: vec![
                Line {
                    number: 10,
                    stmts: vec![Stmt::Let {
                        var: x.clone(),
                        value: Expr::Number(0.0),
                    }],
                },
                Line {
                    number: 20,
                    stmts: vec![Stmt::Let {
                        var: x.clone(),
                        value: Expr::Bin(
                            BinOp::Add,
                            Box::new(Expr::Var(x.clone())),
                            Box::new(Expr::Number(1.0)),
                        ),
                    }],
                },
                Line {
                    number: 30,
                    stmts: vec![Stmt::Goto { target: 20 }],
                },
            ],
        };
        let mut reg = Registry::new();
        let facts = reg.get(&m, &NumericFactAnalysis).clone();
        assert!(!facts.shadow_int_vars.contains(&x));
    }

    #[test]
    fn widen_thresholds_snap_to_nearest_useful_boundary() {
        // Lower bound: largest threshold ≤ v.
        assert_eq!(widen_lo_to_threshold(239), 127);
        assert_eq!(widen_lo_to_threshold(127), 127);
        assert_eq!(widen_lo_to_threshold(126), 1);
        assert_eq!(widen_lo_to_threshold(0), 0);
        assert_eq!(widen_lo_to_threshold(-1), -1);
        assert_eq!(widen_lo_to_threshold(-2), -128);
        assert_eq!(widen_lo_to_threshold(-129), i16::MIN as i32);
        // Upper bound: smallest threshold ≥ v.
        assert_eq!(widen_hi_to_threshold(1), 1);
        assert_eq!(widen_hi_to_threshold(2), 127);
        assert_eq!(widen_hi_to_threshold(128), 255);
        assert_eq!(widen_hi_to_threshold(256), i16::MAX as i32);
    }

    #[test]
    fn down_counter_with_zero_exit_narrows_to_byte_range() {
        // A down-counter with a zero-exit guard:
        //   10 M = 240
        //   20 M = M - 1: IF M = 0 THEN 80
        //   30 GOTO 20
        //   80 PRINT M
        // The down-counter pattern plus the IF M=0 exit-guard must
        // converge to a range that fits a byte at the M=M-1 RHS.
        let m = VarName {
            base: "M".to_string(),
            kind: VarKind::Integer,
        };
        let module = Module {
            lines: vec![
                Line {
                    number: 10,
                    stmts: vec![Stmt::Let {
                        var: m.clone(),
                        value: Expr::Number(240.0),
                    }],
                },
                Line {
                    number: 20,
                    stmts: vec![
                        Stmt::Let {
                            var: m.clone(),
                            value: Expr::Bin(
                                BinOp::Sub,
                                Box::new(Expr::Var(m.clone())),
                                Box::new(Expr::Number(1.0)),
                            ),
                        },
                        Stmt::If {
                            cond: Expr::Bin(
                                BinOp::Eq,
                                Box::new(Expr::Var(m.clone())),
                                Box::new(Expr::Number(0.0)),
                            ),
                            then: crate::ir::ThenIr::Goto(80),
                        },
                    ],
                },
                Line {
                    number: 30,
                    stmts: vec![Stmt::Goto { target: 20 }],
                },
                Line {
                    number: 80,
                    stmts: vec![Stmt::End],
                },
            ],
        };
        let mut reg = Registry::new();
        let facts = reg.get(&module, &NumericFactAnalysis).clone();
        // M=M-1 lives at line_idx=1 (line 20), path=[0].
        let path = crate::cfg::StmtPath {
            line_idx: 1,
            path: vec![0],
        };
        let rhs = Expr::Bin(
            BinOp::Sub,
            Box::new(Expr::Var(m.clone())),
            Box::new(Expr::Number(1.0)),
        );
        let range = facts
            .expr_int_range_before_path(&path, &rhs)
            .expect("M-1 must have a known range");
        assert!(
            range.fits_u8(),
            "M-1 must fit u8 after threshold widening, got {range:?}"
        );
    }

    #[test]
    fn monotone_inc_loop_with_eq_exit_caps_counter_via_bound_var() {
        // A monotone-increment counter with an equality exit:
        //   30 X = -1
        //   40 X = X + 1: IF X = M THEN 80
        //   50 ... (loop body)
        //   60 GOTO 40
        //   80 END
        // M is a separately-bounded var (here a literal-fed counter).
        // Without the loop cap, widening pushes X.max to i16::MAX
        // and downstream u8/abs,X fast paths can't fire.
        let x = ivar("X");
        let m = ivar("M");
        let module = Module {
            lines: vec![
                // 10 M=240: X=-1
                Line {
                    number: 10,
                    stmts: vec![
                        Stmt::Let {
                            var: m.clone(),
                            value: Expr::Number(240.0),
                        },
                        Stmt::Let {
                            var: x.clone(),
                            value: Expr::Number(-1.0),
                        },
                    ],
                },
                // 40 X=X+1: IF X=M THEN 80
                Line {
                    number: 40,
                    stmts: vec![
                        Stmt::Let {
                            var: x.clone(),
                            value: Expr::Bin(
                                BinOp::Add,
                                Box::new(Expr::Var(x.clone())),
                                Box::new(Expr::Number(1.0)),
                            ),
                        },
                        Stmt::If {
                            cond: Expr::Bin(
                                BinOp::Eq,
                                Box::new(Expr::Var(x.clone())),
                                Box::new(Expr::Var(m.clone())),
                            ),
                            then: crate::ir::ThenIr::Goto(80),
                        },
                    ],
                },
                // 60 GOTO 40
                Line {
                    number: 60,
                    stmts: vec![Stmt::Goto { target: 40 }],
                },
                // 80 END
                Line {
                    number: 80,
                    stmts: vec![Stmt::End],
                },
            ],
        };
        let mut reg = Registry::new();
        let facts = reg.get(&module, &NumericFactAnalysis).clone();
        // Find the LET node for X=X+1 on line 40 (line_idx=1, path=[0]).
        let path = crate::cfg::StmtPath {
            line_idx: 1,
            path: vec![0],
        };
        let node_id = facts
            .node_by_stmt
            .get(&path)
            .copied()
            .expect("CFG must have an entry for X=X+1");
        let out = &facts.per_node[node_id].1;
        let x_range = out.get(&x).expect("X must have a range after the step");
        assert!(
            x_range.max <= 240,
            "loop cap must bound X.max at M.max (=240), got {x_range:?}"
        );
        assert!(
            x_range.fits_u8(),
            "X after step must fit u8 once the loop cap is applied, got {x_range:?}"
        );
    }

    #[test]
    fn monotone_inc_without_back_edge_does_not_cap() {
        // No GOTO loop — `X=X+1: IF X=M` runs once. Without a back
        // edge the cap heuristic must NOT fire (otherwise we'd
        // narrow X past what static facts can actually prove).
        let x = ivar("X");
        let m = ivar("M");
        let module = Module {
            lines: vec![
                Line {
                    number: 10,
                    stmts: vec![
                        Stmt::Let {
                            var: m.clone(),
                            value: Expr::Number(240.0),
                        },
                        Stmt::Let {
                            var: x.clone(),
                            value: Expr::Number(1000.0),
                        },
                    ],
                },
                Line {
                    number: 20,
                    stmts: vec![
                        Stmt::Let {
                            var: x.clone(),
                            value: Expr::Bin(
                                BinOp::Add,
                                Box::new(Expr::Var(x.clone())),
                                Box::new(Expr::Number(1.0)),
                            ),
                        },
                        Stmt::If {
                            cond: Expr::Bin(
                                BinOp::Eq,
                                Box::new(Expr::Var(x.clone())),
                                Box::new(Expr::Var(m.clone())),
                            ),
                            then: crate::ir::ThenIr::Goto(30),
                        },
                    ],
                },
                Line {
                    number: 30,
                    stmts: vec![Stmt::End],
                },
            ],
        };
        let mut reg = Registry::new();
        let facts = reg.get(&module, &NumericFactAnalysis).clone();
        let path = crate::cfg::StmtPath {
            line_idx: 1,
            path: vec![0],
        };
        let node_id = facts
            .node_by_stmt
            .get(&path)
            .copied()
            .expect("CFG must have an entry for X=X+1");
        let out = &facts.per_node[node_id].1;
        let x_range = out.get(&x).expect("X must have a range");
        // X started at 1000, +1 = 1001. Without back-edge cap, the
        // range must still reflect that, NOT be clamped to 240.
        assert!(
            x_range.max > 240,
            "no back edge means no cap; X must keep its widened range, got {x_range:?}"
        );
    }
}
