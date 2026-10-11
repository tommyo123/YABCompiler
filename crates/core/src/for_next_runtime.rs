//! Detect orphan `NEXT` statements and the FOR/GOSUB/RETURN sites
//! that need a runtime FOR-stack. Codegen consults the result and
//! falls back to the inline path when this analysis comes up empty.

use std::collections::{HashMap, HashSet};

use crate::ast::{OnBranchKind, VarName};
use crate::ir::{Module, Stmt, ThenIr};

/// How a `FOR` is lowered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForTier {
    /// Nothing crosses the body boundary.
    Static,
    /// Keep a runtime frame with a direct back-edge and no dispatch handler.
    Framed,
    /// A `NEXT` genuinely cannot be resolved statically.
    Dispatched,
}

/// Owned form of `cfg::StmtPath` keyed by source location.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StmtId {
    pub line_no: u16,
    pub line_idx: usize,
    pub path: Vec<usize>,
}

/// Result of the orphan-NEXT pass.
#[derive(Debug, Default, Clone)]
pub struct OrphanAnalysis {
    /// `NEXT` statements with no static FOR partner.
    pub orphan_nexts: Vec<StmtId>,
    /// Runtime FOR statements with their handler and variable IDs.
    pub runtime_fors: HashMap<StmtId, (u8, u8)>,
    /// The tier-2 subset of `runtime_fors`: framed `FOR`s that also get a
    /// `__FOR_NEXT_HANDLER_n` because some dispatching `NEXT` can select their frame.
    pub handler_fors: HashSet<StmtId>,
    /// Handler id stamped into every tier-1 frame, if there are any.
    pub tier1_handler_id: Option<u8>,
    /// Variable IDs start at 1; zero is reserved for GOSUB markers.
    pub var_ids: HashMap<VarName, u8>,
    /// `GOSUB`s that wrap a runtime `FOR`.
    pub runtime_gosubs: HashSet<StmtId>,
    /// `RETURN`s reachable from a runtime `GOSUB`.
    pub runtime_returns: HashSet<StmtId>,
    /// FOR frames left open at the end of a conditional body.
    pub conditional_fors: HashSet<StmtId>,
    /// `NEXT`s that close a `conditional_fors` frame and are themselves guarded by that frame's own condition, so the inline tail is sound after all.
    pub guard_covered_closures: HashSet<StmtId>,
    /// Runtime FOR statements that can skip the same-variable stack search.
    pub search_free_fors: HashSet<StmtId>,

    pub discarding_static_fors: HashSet<StmtId>,
    /// The pairing this analysis computed, keyed by `NEXT` location.
    pub static_pairs: HashMap<StmtId, Vec<Option<StmtId>>>,
    /// Set when a single-byte id space ran out.
    pub id_space_exhausted: Option<&'static str>,
    /// FOR bodies that a branch can enter without running their header.
    pub entered_fors: HashSet<StmtId>,
}

impl OrphanAnalysis {
    pub fn needs_runtime_stack(&self) -> bool {
        !self.orphan_nexts.is_empty() || !self.runtime_fors.is_empty()
    }

    /// `(handler_id, var_id)` for the runtime-mode `FOR` at this location, or `None` for static
    /// `FOR`s that keep the inline fast path.
    pub fn runtime_for_id(&self, line_no: u16, line_idx: usize, path: &[usize]) -> Option<(u8, u8)> {
        let key = StmtId {
            line_no,
            line_idx,
            path: path.to_vec(),
        };
        self.runtime_fors.get(&key).copied()
    }

    /// Which of the three lowerings the `FOR` at this location gets.
    pub fn for_tier(&self, line_no: u16, line_idx: usize, path: &[usize]) -> ForTier {
        let key = StmtId {
            line_no,
            line_idx,
            path: path.to_vec(),
        };
        if !self.runtime_fors.contains_key(&key) {
            ForTier::Static
        } else if self.handler_fors.contains(&key) {
            ForTier::Dispatched
        } else {
            ForTier::Framed
        }
    }

    /// True when the framed `FOR` at this location also needs a `__FOR_NEXT_HANDLER_n`.
    pub fn for_has_handler(&self, line_no: u16, line_idx: usize, path: &[usize]) -> bool {
        self.for_tier(line_no, line_idx, path) == ForTier::Dispatched
    }

    /// Identity id of a `FOR`/`NEXT` variable.
    pub fn var_id(&self, var: &VarName) -> Option<u8> {
        self.var_ids.get(var).copied()
    }

    /// Whether a runtime FOR must search for an active frame with the same variable.
    pub fn for_push_needs_search(&self, line_no: u16, line_idx: usize, path: &[usize]) -> bool {
        let key = StmtId {
            line_no,
            line_idx,
            path: path.to_vec(),
        };
        !self.search_free_fors.contains(&key)
    }

    pub fn static_for_needs_discard(
        &self,
        line_no: u16,
        line_idx: usize,
        path: &[usize],
    ) -> bool {
        let key = StmtId {
            line_no,
            line_idx,
            path: path.to_vec(),
        };
        self.discarding_static_fors.contains(&key)
    }

    /// Whether a branch can enter this FOR body from outside it.
    pub fn for_is_externally_entered(&self, line_no: u16, line_idx: usize, path: &[usize]) -> bool {
        let key = StmtId {
            line_no,
            line_idx,
            path: path.to_vec(),
        };
        self.entered_fors.contains(&key)
    }

    pub fn is_runtime_gosub(&self, line_no: u16, line_idx: usize, path: &[usize]) -> bool {
        let key = StmtId {
            line_no,
            line_idx,
            path: path.to_vec(),
        };
        self.runtime_gosubs.contains(&key)
    }

    pub fn is_runtime_return(&self, line_no: u16, line_idx: usize, path: &[usize]) -> bool {
        let key = StmtId {
            line_no,
            line_idx,
            path: path.to_vec(),
        };
        self.runtime_returns.contains(&key)
    }

    pub fn is_orphan_next(&self, line_no: u16, line_idx: usize, path: &[usize]) -> bool {
        self.orphan_nexts.iter().any(|s| {
            s.line_no == line_no && s.line_idx == line_idx && s.path == path
        })
    }

    pub fn is_conditional_for(&self, line_no: u16, line_idx: usize, path: &[usize]) -> bool {
        let key = StmtId {
            line_no,
            line_idx,
            path: path.to_vec(),
        };
        self.conditional_fors.contains(&key)
    }

    /// The `FOR` this analysis paired with the `idx`-th variable of the `NEXT` at the given
    /// location.
    #[allow(clippy::result_unit_err)]
    pub fn paired_for(
        &self,
        line_no: u16,
        line_idx: usize,
        path: &[usize],
        idx: usize,
    ) -> Result<Option<&StmtId>, ()> {
        let key = StmtId {
            line_no,
            line_idx,
            path: path.to_vec(),
        };
        match self.static_pairs.get(&key) {
            Some(slots) if idx < slots.len() => Ok(slots[idx].as_ref()),
            _ => Err(()),
        }
    }
}

/// Close frames through the named loop, or the top frame for bare NEXT.
pub(crate) fn close_textual_frames<T>(
    stack: &mut Vec<T>,
    vars: &[Option<VarName>],
    var_of: impl Fn(&T) -> VarName,
) -> (Vec<T>, Vec<T>) {
    let mut closed = Vec::new();
    let mut abandoned = Vec::new();
    for v in vars {
        match v {
            None => closed.extend(stack.pop()),
            Some(target) => {
                let Some(pos) = stack.iter().rposition(|e| &var_of(e) == target) else {
                    continue;
                };
                while stack.len() > pos + 1 {
                    abandoned.extend(stack.pop());
                }
                closed.extend(stack.pop());
            }
        }
    }
    (closed, abandoned)
}

impl StmtId {
    /// The location of a graph node's statement.
    pub fn of_node(node: &crate::cfg::CfgNode) -> Self {
        StmtId {
            line_no: node.line_no,
            line_idx: node.stmt.line_idx,
            path: node.stmt.path.clone(),
        }
    }
}

impl OrphanAnalysis {
    /// Static FOR partner, or None when NEXT searches at runtime.
    pub fn next_partner(&self, next: &StmtId, idx: usize) -> Option<&StmtId> {
        if self.orphan_nexts.contains(next) {
            return None;
        }
        self.static_pairs.get(next)?.get(idx)?.as_ref()
    }

    /// Whether this NEXT can use the given FOR frame.
    pub fn next_may_use_frame(
        &self,
        next: &StmtId,
        vars: &[Option<VarName>],
        header: &StmtId,
        var: &VarName,
    ) -> bool {
        vars.iter().enumerate().any(|(idx, named)| match self.next_partner(next, idx) {
            Some(partner) => partner == header,
            None => named.as_ref().is_none_or(|n| n == var),
        })
    }

    /// Whether this NEXT always uses the given FOR frame.
    pub fn next_always_uses_frame(
        &self,
        next: &StmtId,
        vars: &[Option<VarName>],
        header: &StmtId,
        var: &VarName,
    ) -> bool {
        let [named] = vars else {
            return false;
        };
        match self.next_partner(next, 0) {
            Some(partner) => partner == header,
            None => named.as_ref() == Some(var),
        }
    }
}

/// `Analysis` wrapper so the registry caches one `analyze` per IR snapshot.
pub struct ForNextModel;

impl crate::analysis::Analysis for ForNextModel {
    type Output = OrphanAnalysis;
    fn name(&self) -> &'static str {
        "for-next-runtime"
    }
    fn run(&self, m: &Module, _deps: &mut crate::analysis::Registry) -> Self::Output {
        analyze(m)
    }
}

/// Run the analysis over an entire `Module`.
pub fn analyze(module: &Module) -> OrphanAnalysis {
    let mut analysis = OrphanAnalysis::default();
    let (static_pairs, conditional_live) = static_pair_walk(module, &mut analysis);
    let on_key_gosubs = collect_all(module, |s| matches!(s,
        Stmt::OnKey { target: Some(crate::ast::OnKeyAction::GoSub(_)), .. }));
    let async_gosub = !on_key_gosubs.is_empty();
    let error_continuation = module.has_error_continuation();
    if async_gosub || error_continuation {
        // A trap can enter at every statement, including a lexically paired NEXT inside its caller's loop.
        analysis.orphan_nexts.extend(collect_all(module, |s| matches!(s, Stmt::Next { .. })));
    }
    let goto_targets_in_for_body = collect_goto_targets_by_enclosing_for(module);
    // Source-order pairing is a lexical guess. A `GOTO` that leaves one
    // loop and lands on another loop's `NEXT` breaks it — v2 resolves
    // that `NEXT` against the runtime stack, so it can't stay inline.
    for id in cross_reached_nexts(module, &static_pairs, &goto_targets_in_for_body) {
        if !analysis.orphan_nexts.contains(&id) {
            analysis.orphan_nexts.push(id);
        }
    }
    // Check for branches entering a loop body.
    for id in externally_entered_nexts(module, &static_pairs, &goto_targets_in_for_body) {
        if !analysis.orphan_nexts.contains(&id) {
            analysis.orphan_nexts.push(id);
        }
    }
    // Check body entry at statement granularity.
    let crossings = for_body_crossings(module, &static_pairs);
    analysis.entered_fors = for_bodies_entered_from_outside(module, &crossings);
    // Search when a branch can leave a frame open beyond its textual loop.
    let leaving: HashSet<VarName> = crossings
        .iter()
        .filter(|(_, cross)| cross.leaves)
        .filter_map(|(for_id, _)| match stmt_at(module, for_id) {
            Some(Stmt::For { var, .. }) => Some(var.clone()),
            _ => None,
        })
        .collect();
    if !leaving.is_empty() {
        let mut partnerless: Vec<StmtId> = analysis
            .static_pairs
            .iter()
            .filter(|(next_id, slots)| {
                !analysis.orphan_nexts.contains(next_id)
                    && matches!(stmt_at(module, next_id), Some(Stmt::Next { vars })
                        if slots.iter().zip(vars).any(|(slot, named)| {
                            slot.is_none() && named.as_ref().is_none_or(|v| leaving.contains(v))
                        }))
            })
            .map(|(next_id, _)| next_id.clone())
            .collect();
        partnerless.sort_by(|a, b| a.line_idx.cmp(&b.line_idx).then_with(|| a.path.cmp(&b.path)));
        analysis.orphan_nexts.extend(partnerless);
    }
    for (for_id, cross) in &crossings {
        if !cross.enters {
            continue;
        }
        let Some((_, next_id)) = static_pairs.iter().find(|(f, _)| f == for_id) else {
            continue;
        };
        if !analysis.orphan_nexts.contains(next_id) {
            analysis.orphan_nexts.push(next_id.clone());
        }
    }
    // Third route: a `GOSUB` marker between the frame and the `NEXT`.
    for id in nexts_behind_gosub_marker(module, &analysis.static_pairs) {
        if !analysis.orphan_nexts.contains(&id) {
            analysis.orphan_nexts.push(id);
        }
    }

    for id in nexts_cleared_by_reset(module, &analysis.static_pairs) {
        if !analysis.orphan_nexts.contains(&id) {
            analysis.orphan_nexts.push(id);
        }
    }
    analysis.orphan_nexts.sort_by(|a, b| {
        a.line_idx
            .cmp(&b.line_idx)
            .then_with(|| a.path.cmp(&b.path))
    });
    analysis.orphan_nexts.dedup();

    if analysis.orphan_nexts.is_empty()
        && !((async_gosub || error_continuation)
            && !collect_all(module, |s| matches!(s, Stmt::For { .. })).is_empty())
    {
        // Common path — no orphans, nothing else to compute.
        return analysis;
    }

    // `ON n GOSUB` is a GOSUB.
    let mut all_gosubs = collect_all(module, |s| {
        matches!(
            s,
            Stmt::GoSub { .. }
                | Stmt::OnBranch {
                    kind: OnBranchKind::GoSub,
                    ..
                }
        )
    });
    all_gosubs.extend(on_key_gosubs);
    let all_returns = collect_all(module, |s| matches!(s, Stmt::Return));

    // A FOR is runtime-mode if some line inside its lexical body can
    // reach an orphan NEXT via GOTO or fall-through.
    let orphan_lines: HashSet<u16> = analysis.orphan_nexts.iter().map(|s| s.line_no).collect();

    // Source-order id assignment keeps the handler table stable.
    // Id 0 is reserved for the GOSUB marker.
    let mut sorted: Vec<(&StmtId, &ForBodyInfo)> = goto_targets_in_for_body.iter().collect();
    sorted.sort_by(|(a, _), (b, _)| {
        a.line_idx
            .cmp(&b.line_idx)
            .then_with(|| a.path.cmp(&b.path))
    });
    let mut needs_id: Vec<StmtId> = Vec::new();
    for (for_id, body_lines) in sorted {
        // A FOR reaches an orphan NEXT when (a) something in its body
        // GOTOs the orphan's line, or (b) the orphan sits on a line
        // the FOR's body covers — body lines collected by the nested-
        // aware walk so a FOR with a nested static NEXT can still
        // have its body extend past it.
        let reaches_orphan = body_lines
            .goto_targets
            .iter()
            .any(|(_, t)| orphan_lines.contains(t))
            || body_lines
                .lines_in_body
                .iter()
                .any(|l| orphan_lines.contains(l));
        if reaches_orphan {
            needs_id.push(for_id.clone());
        }
    }

    for id in conditional_live {
        needs_id.push(id);
    }
    // Every *other* conditionally-opened FOR as well, for the same reason one step removed.
    for id in &analysis.conditional_fors {
        needs_id.push(id.clone());
    }

    for next_id in &analysis.orphan_nexts {
        let Some(slots) = analysis.static_pairs.get(next_id) else {
            continue;
        };
        for for_id in slots.iter().flatten() {
            needs_id.push(for_id.clone());
        }
    }
    // A conditional NEXT can leave its frame live past the line.
    for (next_id, slots) in &analysis.static_pairs {
        let parent = |id: &StmtId| (id.line_idx, id.path[..id.path.len() - 1].to_vec());
        if next_id.path.len() < 2 {
            continue;
        }
        for for_id in slots.iter().flatten() {
            if parent(next_id) != parent(for_id) {
                needs_id.push(for_id.clone());
            }
        }
    }
    // The polarity inversion.
    let heuristic_only = needs_id.clone();
    for (id, _) in &crossings {
        needs_id.push(id.clone());
    }
    needs_id.sort_by(|a, b| {
        a.line_idx
            .cmp(&b.line_idx)
            .then_with(|| a.path.cmp(&b.path))
    });
    needs_id.dedup();
    // A handler id is one byte and 0 is the GOSUB marker, so 254 frames plus the one shared
    // tier-1 id is the ceiling.
    const MAX_FRAMES: usize = u8::MAX as usize - 1;
    if needs_id.len() > MAX_FRAMES {
        needs_id = heuristic_only;
        needs_id.sort_by(|a, b| {
            a.line_idx
                .cmp(&b.line_idx)
                .then_with(|| a.path.cmp(&b.path))
        });
        needs_id.dedup();
    }

    // Variable identity ids.
    let (var_ids, var_ids_exhausted) = assign_var_ids(module);
    analysis.var_ids = var_ids;
    if var_ids_exhausted {
        analysis.id_space_exhausted = Some("distinct FOR/NEXT loop variables");
    }

    // Tier 1 vs tier 2.
    let reach = DispatchReach::new(module, &analysis);
    let mut framed: Vec<(StmtId, u8, bool)> = Vec::with_capacity(needs_id.len());
    for for_id in needs_id {
        let var = match stmt_at(module, &for_id) {
            Some(Stmt::For { var, .. }) => var.clone(),

            _ => panic!("runtime FOR id assigned to a non-FOR statement"),
        };
        // Normally every FOR variable is numbered.
        let Some(var_id) = analysis.var_ids.get(&var).copied() else {
            debug_assert!(analysis.id_space_exhausted.is_some());
            continue;
        };
        let dispatched = reach.can_be_selected(&for_id, var_id);
        framed.push((for_id, var_id, dispatched));
    }

    // Tier-2 ids come first and are contiguous from 1, so the handler table stays exactly as
    // long as the number of real handlers plus the one shared tier-1 slot.
    let mut next_id: u8 = 1;
    for (for_id, var_id, dispatched) in &framed {
        if !*dispatched {
            continue;
        }
        analysis
            .runtime_fors
            .insert(for_id.clone(), (next_id, *var_id));
        analysis.handler_fors.insert(for_id.clone());
        // One byte per handler id, 0 reserved for the GOSUB marker.
        match next_id.checked_add(1) {
            Some(n) => next_id = n,
            None => {
                analysis.id_space_exhausted = Some("runtime FOR loops");
                break;
            }
        }
    }
    if framed.iter().any(|(_, _, d)| !*d) {
        let shared = next_id;
        analysis.tier1_handler_id = Some(shared);
        for (for_id, var_id, dispatched) in &framed {
            if *dispatched {
                continue;
            }
            analysis
                .runtime_fors
                .insert(for_id.clone(), (shared, *var_id));
        }
    }

    // Every GOSUB leaves a marker, and every RETURN walks back to one.
    let _ = &static_pairs;
    for g in &all_gosubs {
        analysis.runtime_gosubs.insert(g.clone());
    }
    for r in &all_returns {
        analysis.runtime_returns.insert(r.clone());
    }

    let meeting = fors_meeting_a_live_frame(module, &analysis);
    analysis.search_free_fors = search_free_runtime_fors(&analysis.runtime_fors, meeting.as_ref());
    analysis.discarding_static_fors =
        static_fors_needing_discard(module, &analysis.runtime_fors, &analysis.var_ids, meeting.as_ref());

    analysis
}

// --- helpers ---------------------------------------------------------------

/// One static branch edge, tagged with the *statement* it leaves from.
#[derive(Debug, Clone)]
struct BranchEdge {
    src: StmtId,
    target: u16,
    /// `GOSUB`/`ON..GOSUB`.
    gosub: bool,
}

fn collect_edges_with_source(module: &Module) -> Vec<BranchEdge> {
    fn of_stmt(stmt: &Stmt, id: StmtId, out: &mut Vec<BranchEdge>) {
        match stmt {
            Stmt::Goto { target } => out.push(BranchEdge {
                src: id,
                target: *target,
                gosub: false,
            }),
            Stmt::GoSub { target } => out.push(BranchEdge {
                src: id,
                target: *target,
                gosub: true,
            }),
            Stmt::OnBranch { kind, targets, .. } => {
                let gosub = matches!(kind, OnBranchKind::GoSub);
                for t in targets {
                    out.push(BranchEdge {
                        src: id.clone(),
                        target: *t,
                        gosub,
                    });
                }
            }
            Stmt::If { then, .. } => of_then(then, &id, out),
            Stmt::IfElse {
                then, else_then, ..
            } => {
                of_then(then, &id, out);
                of_then(else_then, &id, out);
            }
            Stmt::Rcomp { then, else_then } => {
                of_then(then, &id, out);
                if let Some(et) = else_then {
                    of_then(et, &id, out);
                }
            }
            _ => {}
        }
    }
    fn of_then(then: &ThenIr, parent: &StmtId, out: &mut Vec<BranchEdge>) {
        match then {
            // `IF c THEN 90` branches from the IF's own position.
            ThenIr::Goto(t) => out.push(BranchEdge {
                src: parent.clone(),
                target: *t,
                gosub: false,
            }),
            ThenIr::Stmts(stmts) => {
                for (idx, inner) in stmts.iter().enumerate() {
                    let mut path = parent.path.clone();
                    path.push(idx);
                    of_stmt(
                        inner,
                        StmtId {
                            line_no: parent.line_no,
                            line_idx: parent.line_idx,
                            path,
                        },
                        out,
                    );
                }
            }
        }
    }
    let mut out = Vec::new();
    for (line_idx, line) in module.lines.iter().enumerate() {
        for (top_idx, stmt) in line.stmts.iter().enumerate() {
            of_stmt(
                stmt,
                StmtId {
                    line_no: line.number,
                    line_idx,
                    path: vec![top_idx],
                },
                &mut out,
            );
        }
    }
    out
}

/// Source order key.
fn before_or_at(a: &StmtId, b: &StmtId) -> bool {
    (a.line_idx, &a.path) <= (b.line_idx, &b.path)
}

/// Which way a transfer crossed a `FOR`'s body boundary.
#[derive(Debug, Clone, Copy, Default)]
struct Crossing {
    /// Something left the body.
    leaves: bool,
    /// Something arrived in the body from outside it.
    enters: bool,
}

impl Crossing {
    fn crossed(&self) -> bool {
        self.leaves || self.enters
    }
}

/// Find branches crossing a paired FOR body's boundary.
fn for_body_crossings(module: &Module, pairs: &[(StmtId, StmtId)]) -> Vec<(StmtId, Crossing)> {
    let all_fors = collect_all(module, |s| matches!(s, Stmt::For { .. }));
    if all_fors.is_empty() {
        return Vec::new();
    }
    if has_unmodelled_control_flow(module) {
        // Missing control-flow edges prevent a static proof.
        return all_fors
            .into_iter()
            .map(|f| {
                (
                    f,
                    Crossing {
                        leaves: true,
                        enters: false,
                    },
                )
            })
            .collect();
    }
    let line_index: HashMap<u16, usize> = module
        .lines
        .iter()
        .enumerate()
        .map(|(i, l)| (l.number, i))
        .collect();
    let edges = collect_edges_with_source(module);
    let partner: HashMap<&StmtId, &StmtId> = pairs.iter().map(|(f, n)| (f, n)).collect();

    let mut out = Vec::new();
    for for_id in &all_fors {
        let end = partner.get(for_id).copied();
        let mut cross = Crossing::default();
        for edge in &edges {
            // An undefined target is `?UNDEF'D STATEMENT` at run time: control never arrives,
            // so nothing crosses.
            let Some(&target_idx) = line_index.get(&edge.target) else {
                continue;
            };
            let target_inside = target_idx > for_id.line_idx
                && end.is_none_or(|n| target_idx <= n.line_idx);
            let src_inside = !before_or_at(&edge.src, for_id)
                && end.is_none_or(|n| before_or_at(&edge.src, n));
            cross.leaves |= src_inside && !target_inside && !edge.gosub;
            cross.enters |= !src_inside && target_inside;
            if cross.leaves && cross.enters {
                break;
            }
        }
        // A skipped FOR can leave control inside its loop body.
        let next_line = for_id.line_idx + 1;
        if for_id.path.len() > 1
            && next_line < module.lines.len()
            && end.is_none_or(|n| next_line <= n.line_idx)
        {
            cross.enters = true;
        }
        if cross.crossed() {
            out.push((for_id.clone(), cross));
        }
    }
    out
}

/// The `enters` half of `for_body_crossings`, re-asked for the question
/// `OrphanAnalysis::entered_fors` documents.
fn for_bodies_entered_from_outside(
    module: &Module,
    crossings: &[(StmtId, Crossing)],
) -> HashSet<StmtId> {
    let all_fors = collect_all(module, |s| matches!(s, Stmt::For { .. }));
    if all_fors.is_empty() {
        return HashSet::new();
    }
    if has_unmodelled_control_flow(module) {
        return all_fors.into_iter().collect();
    }
    crossings
        .iter()
        .filter(|(_, c)| c.enters)
        .map(|(f, _)| f.clone())
        .collect()
}

/// Can a dispatching `NEXT` select this frame?
struct DispatchReach {
    /// Var ids named by a dispatching `NEXT V`.
    named: HashSet<u8>,
    /// Line indices (and in-line positions) of dispatching bare `NEXT`s.
    bare: Vec<StmtId>,
    succ: Vec<Vec<usize>>,
    unmodelled: bool,
    cache: std::cell::RefCell<HashMap<usize, HashSet<usize>>>,
}

impl DispatchReach {
    fn new(module: &Module, analysis: &OrphanAnalysis) -> Self {
        let mut named = HashSet::new();
        let mut bare = Vec::new();
        for id in &analysis.orphan_nexts {
            // Codegen decides orphan-ness per statement, not per listed variable, so every slot
            // of an orphan `NEXT I,J` dispatches.
            let Some(Stmt::Next { vars }) = stmt_at(module, id) else {
                continue;
            };
            for v in vars {
                match v {
                    None => bare.push(id.clone()),
                    Some(var) => {
                        if let Some(vid) = analysis.var_ids.get(var) {
                            named.insert(*vid);
                        }
                    }
                }
            }
        }
        Self {
            named,
            bare,
            succ: line_successors(module),
            unmodelled: has_unmodelled_control_flow(module),
            cache: std::cell::RefCell::new(HashMap::new()),
        }
    }

    fn can_be_selected(&self, for_id: &StmtId, var_id: u8) -> bool {
        if self.named.contains(&var_id) {
            return true;
        }
        if self.bare.is_empty() {
            return false;
        }
        if self.unmodelled {
            return true;
        }
        self.bare.iter().any(|n| self.reaches(for_id, n))
    }

    /// Can control get from the `FOR` header to this statement?
    fn reaches(&self, from: &StmtId, to: &StmtId) -> bool {
        // Statements after the header on its own line are reached without leaving the line at
        // all.
        if to.line_idx == from.line_idx && to.path > from.path {
            return true;
        }
        let mut cache = self.cache.borrow_mut();
        let reached = cache.entry(from.line_idx).or_insert_with(|| {
            let mut seen: HashSet<usize> = HashSet::new();
            let mut work: Vec<usize> = self.succ[from.line_idx].clone();
            for &s in &work {
                seen.insert(s);
            }
            while let Some(i) = work.pop() {
                for &n in &self.succ[i] {
                    if seen.insert(n) {
                        work.push(n);
                    }
                }
            }
            seen
        });
        reached.contains(&to.line_idx)
    }
}

/// One identity id per distinct `FOR`/`NEXT` variable, numbered from 1 in source order.
fn assign_var_ids(module: &Module) -> (HashMap<VarName, u8>, bool) {
    let mut ids: HashMap<VarName, u8> = HashMap::new();
    let mut next: u8 = 1;
    let mut exhausted = false;
    let add = |var: &VarName,
               ids: &mut HashMap<VarName, u8>,
               next: &mut u8,
               exhausted: &mut bool| {
        if ids.contains_key(var) {
            return;
        }
        if *exhausted {
            return;
        }
        ids.insert(var.clone(), *next);
        // 0 is the GOSUB marker's var slot, so 255 distinct names is the ceiling.
        match next.checked_add(1) {
            Some(n) => *next = n,
            None => *exhausted = true,
        }
    };
    for id in collect_all(module, |s| {
        matches!(s, Stmt::For { .. } | Stmt::Next { .. })
    }) {
        match stmt_at(module, &id) {
            Some(Stmt::For { var, .. }) => add(var, &mut ids, &mut next, &mut exhausted),
            Some(Stmt::Next { vars }) => {
                for v in vars.iter().flatten() {
                    add(v, &mut ids, &mut next, &mut exhausted);
                }
            }
            _ => {}
        }
    }
    (ids, exhausted)
}

/// `NEXT`s belonging to a loop whose body can be entered by a branch from OUTSIDE the loop.
fn externally_entered_nexts(
    module: &Module,
    pairs: &[(StmtId, StmtId)],
    bodies: &HashMap<StmtId, ForBodyInfo>,
) -> Vec<StmtId> {
    let edges = collect_branch_edges(module);
    let mut out: Vec<StmtId> = Vec::new();
    for (for_id, info) in bodies {
        let Some((_, next_id)) = pairs.iter().find(|(f, _)| f == for_id) else {
            continue;
        };
        let body: HashSet<u16> = info.lines_in_body.iter().copied().collect();
        // The FOR's own line is not in `lines_in_body`, so a jump back to the header (which
        // re-enters the loop properly, pushing a fresh frame) is correctly not counted here.
        let entered = edges
            .iter()
            .any(|(from, target)| body.contains(target) && !body.contains(from));
        if entered && !out.contains(next_id) {
            out.push(next_id.clone());
        }
    }
    out
}

/// Line successors that respect unconditional terminators.
fn terminating_line_successors(module: &Module) -> Vec<Vec<usize>> {
    terminating_line_successors_with(module, true)
}

/// Follow calls or their continuations according to the requested graph.
fn terminating_line_successors_with(module: &Module, calls: bool) -> Vec<Vec<usize>> {
    let index: HashMap<u16, usize> = module
        .lines
        .iter()
        .enumerate()
        .map(|(i, l)| (l.number, i))
        .collect();
    let edges = collect_branch_edges_with(module, calls);
    let mut succ: Vec<Vec<usize>> = vec![Vec::new(); module.lines.len()];
    for (from, to) in edges {
        let (Some(&f), Some(&t)) = (index.get(&from), index.get(&to)) else {
            continue;
        };
        if !succ[f].contains(&t) {
            succ[f].push(t);
        }
    }
    for (i, line) in module.lines.iter().enumerate() {
        let terminates = line.stmts.iter().any(|s| {
            matches!(
                s,
                Stmt::Goto { .. }
                    | Stmt::Return
                    | Stmt::End
                    | Stmt::Stop
                    | Stmt::RaiseError(_)
                    | Stmt::Run(_)
                    | Stmt::ComputedGoto { .. }
            )
        });
        if !terminates && i + 1 < module.lines.len() && !succ[i].contains(&(i + 1)) {
            succ[i].push(i + 1);
        }
    }
    succ
}

/// Whether the statement continues along its line.
fn stmt_falls_through(stmt: &Stmt) -> bool {
    !matches!(
        stmt,
        Stmt::Goto { .. }
            | Stmt::ComputedGoto { .. }
            | Stmt::OnBranch {
                kind: OnBranchKind::Goto,
                ..
            }
            | Stmt::If { .. }
            | Stmt::IfElse { .. }
            | Stmt::Rcomp { .. }
            | Stmt::Return
            | Stmt::End
            | Stmt::Stop
            | Stmt::Run(_)
            | Stmt::RaiseError(_)
            | Stmt::Hang
            | Stmt::Resume { .. }
            | Stmt::ErrorRaise { .. }
            | Stmt::Do
            | Stmt::DoIf { .. }
            | Stmt::DoNull
            | Stmt::Done
            | Stmt::Else
            | Stmt::Repeat
            | Stmt::Until { .. }
            | Stmt::Loop
            | Stmt::EndLoop
            | Stmt::ExitLoop { .. }
    )
}

/// A GOSUB in any form, plain or nested in an IF.
fn stmt_calls(stmt: &Stmt) -> bool {
    struct Find(bool);
    impl crate::visit::Visitor for Find {
        fn visit_stmt(&mut self, line: u16, stmt: &Stmt) {
            if matches!(
                stmt,
                Stmt::GoSub { .. }
                    | Stmt::OnBranch {
                        kind: OnBranchKind::GoSub,
                        ..
                    }
            ) {
                self.0 = true;
            }
            crate::visit::walk_stmt(self, line, stmt);
        }
    }
    let mut f = Find(false);
    crate::visit::Visitor::visit_stmt(&mut f, 0, stmt);
    f.0
}

/// A RETURN, plain or nested in an IF.
fn stmt_returns(stmt: &Stmt) -> bool {
    struct Find(bool);
    impl crate::visit::Visitor for Find {
        fn visit_stmt(&mut self, line: u16, stmt: &Stmt) {
            if matches!(stmt, Stmt::Return) {
                self.0 = true;
            }
            crate::visit::walk_stmt(self, line, stmt);
        }
    }
    let mut f = Find(false);
    crate::visit::Visitor::visit_stmt(&mut f, 0, stmt);
    f.0
}

/// Lines reachable from `start` without entering line `barrier`.
fn reach_avoiding(succ: &[Vec<usize>], start: usize, barrier: usize) -> HashSet<usize> {
    let mut seen: HashSet<usize> = HashSet::new();
    if start == barrier {
        return seen;
    }
    let mut work = vec![start];
    seen.insert(start);
    while let Some(i) = work.pop() {
        for &n in &succ[i] {
            if n != barrier && seen.insert(n) {
                work.push(n);
            }
        }
    }
    seen
}

/// Find lines reachable from a subroutine entry without crossing RETURN or termination.
fn subroutine_region(succ: &[Vec<usize>], target: usize) -> HashSet<usize> {
    let mut region: HashSet<usize> = HashSet::new();
    let mut work = vec![target];
    region.insert(target);
    while let Some(i) = work.pop() {
        for &n in &succ[i] {
            if region.insert(n) {
                work.push(n);
            }
        }
    }
    region
}

/// Does the marker a `GOSUB <target>` pushes do observable work?
pub(crate) fn gosub_targets_needing_marker(
    module: &Module,
    targets: impl IntoIterator<Item = u16>,
) -> HashSet<u16> {
    let mut out = HashSet::new();
    let mut targets = targets.into_iter().peekable();
    if targets.peek().is_none() {
        return out;
    }
    let has_next: Vec<bool> = module
        .lines
        .iter()
        .map(|l| stmts_have_next(&l.stmts))
        .collect();
    if !has_next.iter().any(|h| *h) {
        return out;
    }
    let succ = terminating_line_successors(module);
    for target in targets {
        let Some(start) = module.lines.iter().position(|l| l.number == target) else {
            continue;
        };
        if subroutine_region(&succ, start)
            .iter()
            .any(|&i| has_next[i])
        {
            out.insert(target);
        }
    }
    out
}

/// Find subroutine calls that can reach themselves through their target.
pub(crate) fn recursive_gosub_call_lines(
    module: &Module,
    calls: impl IntoIterator<Item = (u16, u16)>,
) -> HashSet<(u16, u16)> {
    let mut out = HashSet::new();
    let mut calls = calls.into_iter().peekable();
    if calls.peek().is_none() {
        return out;
    }
    let succ = terminating_line_successors(module);
    let index: HashMap<u16, usize> = module
        .lines
        .iter()
        .enumerate()
        .map(|(i, l)| (l.number, i))
        .collect();
    let mut region_of: HashMap<u16, HashSet<usize>> = HashMap::new();
    for (from, target) in calls {
        let (Some(&f), Some(&t)) = (index.get(&from), index.get(&target)) else {
            continue;
        };
        let region = region_of
            .entry(target)
            .or_insert_with(|| subroutine_region(&succ, t));
        if region.contains(&f) {
            out.insert((from, target));
        }
    }
    out
}

/// A `NEXT` anywhere in this line, including under a `THEN`.
fn stmts_have_next(stmts: &[Stmt]) -> bool {
    fn then_has(then: &ThenIr) -> bool {
        matches!(then, ThenIr::Stmts(inner) if stmts_have_next(inner))
    }
    stmts.iter().any(|s| match s {
        Stmt::Next { .. } => true,
        Stmt::If { then, .. } => then_has(then),
        Stmt::IfElse {
            then, else_then, ..
        } => then_has(then) || then_has(else_then),
        Stmt::Rcomp { then, else_then } => {
            then_has(then) || else_then.as_ref().is_some_and(then_has)
        }
        _ => false,
    })
}

/// `NEXT`s that can run with a `GOSUB` marker sitting above the frame they lexically belong to.
fn nexts_behind_gosub_marker(
    module: &Module,
    static_pairs: &HashMap<StmtId, Vec<Option<StmtId>>>,
) -> Vec<StmtId> {
    if static_pairs.is_empty() {
        return Vec::new();
    }
    let index: HashMap<u16, usize> = module
        .lines
        .iter()
        .enumerate()
        .map(|(i, l)| (l.number, i))
        .collect();
    let mut targets: Vec<usize> = Vec::new();
    for id in collect_all(module, |s| {
        matches!(
            s,
            Stmt::GoSub { .. }
                | Stmt::OnBranch {
                    kind: OnBranchKind::GoSub,
                    ..
                }
        )
    }) {
        match stmt_at(module, &id) {
            Some(Stmt::GoSub { target }) => {
                if let Some(&i) = index.get(target) {
                    targets.push(i);
                }
            }
            Some(Stmt::OnBranch { targets: ts, .. }) => {
                for t in ts {
                    if let Some(&i) = index.get(t) {
                        targets.push(i);
                    }
                }
            }
            _ => {}
        }
    }
    targets.sort_unstable();
    targets.dedup();
    if targets.is_empty() {
        return Vec::new();
    }

    // Nested calls have their own frame region.
    let succ = terminating_line_successors_with(module, false);
    let mut out: Vec<StmtId> = Vec::new();
    for start in targets {
        let region = subroutine_region(&succ, start);
        let mut before_for: HashMap<usize, HashSet<usize>> = HashMap::new();
        for (next_id, slots) in static_pairs {
            if !region.contains(&next_id.line_idx) {
                continue;
            }
            // A frame remains behind the call marker until its FOR runs.
            let behind = slots.iter().flatten().any(|for_id| {
                if !region.contains(&for_id.line_idx) {
                    return true;
                }
                if next_id.line_idx == for_id.line_idx {
                    let (last, parent) = for_id.path.split_last().expect("paths are non-empty");
                    if next_id.path.len() > parent.len()
                        && next_id.path.starts_with(parent)
                        && next_id.path[parent.len()] > *last
                    {
                        return false;
                    }
                }
                let [at] = for_id.path.as_slice() else {
                    return true;
                };
                if !module.lines[for_id.line_idx].stmts[..*at]
                    .iter()
                    .all(stmt_falls_through)
                {
                    return true;
                }
                if next_id.line_idx == for_id.line_idx {
                    return false;
                }
                before_for
                    .entry(for_id.line_idx)
                    .or_insert_with(|| reach_avoiding(&succ, start, for_id.line_idx))
                    .contains(&next_id.line_idx)
            });
            if behind && !out.contains(next_id) {
                out.push(next_id.clone());
            }
        }
    }
    // `static_pairs` is a HashMap; sort so the output doesn't depend on its iteration order.
    out.sort_by(|a, b| {
        a.line_idx
            .cmp(&b.line_idx)
            .then_with(|| a.path.cmp(&b.path))
    });
    out
}

/// Find NEXT statements whose frames may be cleared by CLR or RUN.
fn nexts_cleared_by_reset(
    module: &Module,
    static_pairs: &HashMap<StmtId, Vec<Option<StmtId>>>,
) -> Vec<StmtId> {
    // Seeds: where control stands the instant the reset has run.
    let mut clr_seeds: Vec<StmtId> = Vec::new();
    let mut jump_seeds: Vec<usize> = Vec::new();
    for id in collect_all(module, |s| matches!(s, Stmt::Clr | Stmt::Run(_))) {
        match stmt_at(module, &id) {
            Some(Stmt::Clr) => clr_seeds.push(id),
            Some(Stmt::Run(target)) => {
                let idx = match target {
                    None => (!module.lines.is_empty()).then_some(0),
                    Some(line) => module.lines.iter().position(|l| l.number == *line),
                };
                if let Some(idx) = idx {
                    jump_seeds.push(idx);
                }
            }
            _ => {}
        }
    }
    if clr_seeds.is_empty() && jump_seeds.is_empty() {
        return Vec::new();
    }

    let succ = line_successors(module);
    // `block` is a line index that the walk may not enter.
    let reach = |starts: &[usize], block: usize| -> HashSet<usize> {
        let mut seen: HashSet<usize> = HashSet::new();
        let mut work: Vec<usize> = Vec::new();
        for &s in starts {
            if s != block && seen.insert(s) {
                work.push(s);
            }
        }
        while let Some(i) = work.pop() {
            for &n in &succ[i] {
                if n != block && seen.insert(n) {
                    work.push(n);
                }
            }
        }
        seen
    };

    let mut out: Vec<StmtId> = Vec::new();
    let mut cache: HashMap<(usize, usize), HashSet<usize>> = HashMap::new();
    for (next_id, slots) in static_pairs {
        for for_id in slots.iter().flatten() {
            let block = for_id.line_idx;
            let mut hit = false;
            for seed in &clr_seeds {

                if for_id.line_idx == seed.line_idx && for_id.path > seed.path {
                    continue;
                }
                let reached = cache
                    .entry((seed.line_idx, block))
                    .or_insert_with(|| reach(&succ[seed.line_idx], block));
                // Statements after the reset on its own line are in reach without leaving the
                // line at all; the rest of the line is in reach only if the walk gets back to
                // it.
                if reached.contains(&next_id.line_idx)
                    || (next_id.line_idx == seed.line_idx && next_id.path > seed.path)
                {
                    hit = true;
                    break;
                }
            }
            if !hit {
                for &seed in &jump_seeds {
                    // `usize::MAX` can't collide with a real line index.
                    let reached = cache
                        .entry((usize::MAX - seed, block))
                        .or_insert_with(|| reach(&[seed], block));
                    if reached.contains(&next_id.line_idx) {
                        hit = true;
                        break;
                    }
                }
            }
            if hit && !out.contains(next_id) {
                out.push(next_id.clone());
            }
        }
    }
    // `static_pairs` is a HashMap; sort so the output doesn't depend on its iteration order.
    out.sort_by(|a, b| {
        a.line_idx
            .cmp(&b.line_idx)
            .then_with(|| a.path.cmp(&b.path))
    });
    out
}

/// Find FOR statements that can encounter an open frame of the same variable.
fn fors_meeting_a_live_frame(module: &Module, analysis: &OrphanAnalysis) -> Option<HashSet<StmtId>> {
    if analysis.runtime_fors.is_empty() {
        return Some(HashSet::new());
    }
    if has_unmodelled_control_flow(module) {
        return None;
    }
    let cfg = crate::cfg::Cfg::build_with(module, analysis);
    let node_of: HashMap<(usize, &[usize]), usize> = cfg
        .nodes
        .iter()
        .enumerate()
        .map(|(id, n)| ((n.stmt.line_idx, n.stmt.path.as_slice()), id))
        .collect();
    let mut met = HashSet::new();
    for id in analysis.runtime_fors.keys() {
        let header = *node_of.get(&(id.line_idx, id.path.as_slice()))?;
        let Stmt::For { var, .. } = cfg.stmt_at(header, module) else {
            return None;
        };
        let mut seen: HashSet<usize> = HashSet::new();
        let mut work: Vec<usize> = cfg.nodes[header].successors.clone();
        while let Some(n) = work.pop() {
            if !seen.insert(n) {
                continue;
            }
            let here = StmtId::of_node(&cfg.nodes[n]);
            let stmt = cfg.stmt_at(n, module);
            if matches!(stmt, Stmt::For { var: v, .. } if v == var) {
                met.insert(here.clone());
            }
            let uses = match stmt {
                Stmt::Next { vars } => analysis.next_always_uses_frame(&here, vars, id, var),
                _ => false,
            };
            work.extend(cfg.frame_successors(module, header, n, var, uses));
        }
    }
    Some(met)
}

/// Runtime FOR statements that need no frame search.
fn search_free_runtime_fors(
    runtime_fors: &HashMap<StmtId, (u8, u8)>,
    meeting: Option<&HashSet<StmtId>>,
) -> HashSet<StmtId> {
    let Some(meeting) = meeting else {
        return HashSet::new();
    };
    runtime_fors
        .keys()
        .filter(|id| !meeting.contains(*id))
        .cloned()
        .collect()
}

/// Static FOR statements that must discard an open runtime frame.
fn static_fors_needing_discard(
    module: &Module,
    runtime_fors: &HashMap<StmtId, (u8, u8)>,
    var_ids: &HashMap<VarName, u8>,
    meeting: Option<&HashSet<StmtId>>,
) -> HashSet<StmtId> {
    if runtime_fors.is_empty() {
        return HashSet::new();
    }
    let framed_vars: HashSet<u8> = runtime_fors.values().map(|(_, var)| *var).collect();
    collect_all(module, |s| matches!(s, Stmt::For { .. }))
        .into_iter()
        .filter(|id| !runtime_fors.contains_key(id))
        .filter(|id| match meeting {
            Some(meeting) => meeting.contains(id),
            None => matches!(
                stmt_at(module, id),
                Some(Stmt::For { var, .. }) if var_ids.get(var).is_some_and(|v| framed_vars.contains(v))
            ),
        })
        .collect()
}

/// Control transfers `line_successors` has no edge for.
fn has_unmodelled_control_flow(module: &Module) -> bool {
    fn hit(stmt: &Stmt) -> bool {
        matches!(
            stmt,
            Stmt::ComputedGoto { .. }
                | Stmt::Do
                | Stmt::DoIf { .. }
                | Stmt::DoNull
                | Stmt::Done
                | Stmt::Else
                | Stmt::Repeat
                | Stmt::Until { .. }
                | Stmt::Loop
                | Stmt::EndLoop
                | Stmt::ExitLoop { .. }
                | Stmt::OnKey { .. }
                | Stmt::OnError { .. }
                | Stmt::Resume { .. }
                | Stmt::ErrorRaise { .. }
        )
    }
    !collect_all(module, hit).is_empty()
}

/// Successor line *indices* per line: every static branch target plus unconditional
/// fall-through to the next line.
fn line_successors(module: &Module) -> Vec<Vec<usize>> {
    let index: HashMap<u16, usize> = module
        .lines
        .iter()
        .enumerate()
        .map(|(i, l)| (l.number, i))
        .collect();
    let edges = collect_branch_edges(module);
    let mut succ: Vec<Vec<usize>> = vec![Vec::new(); module.lines.len()];
    for (from, to) in edges {
        // GOSUB edges are recorded with a sentinel origin by other helpers, never here; every
        // entry is a real line pair.
        let (Some(&f), Some(&t)) = (index.get(&from), index.get(&to)) else {
            continue;
        };
        if !succ[f].contains(&t) {
            succ[f].push(t);
        }
    }
    for (i, s) in succ.iter_mut().enumerate() {
        if i + 1 < module.lines.len() && !s.contains(&(i + 1)) {
            s.push(i + 1);
        }
    }
    succ
}

/// Every static `(source line, target line)` branch edge in the module.
fn collect_branch_edges(module: &Module) -> HashSet<(u16, u16)> {
    collect_branch_edges_with(module, true)
}

/// Collect branch edges with optional GOSUB target edges.
fn collect_branch_edges_with(module: &Module, calls: bool) -> HashSet<(u16, u16)> {
    fn scan(line_no: u16, stmts: &[Stmt], calls: bool, out: &mut HashSet<(u16, u16)>) {
        for stmt in stmts {
            match stmt {
                Stmt::Goto { target } => {
                    out.insert((line_no, *target));
                }
                Stmt::GoSub { target } if calls => {
                    out.insert((line_no, *target));
                }
                Stmt::OnBranch { kind, targets, .. }
                    if calls || !matches!(kind, OnBranchKind::GoSub) =>
                {
                    for t in targets {
                        out.insert((line_no, *t));
                    }
                }
                Stmt::If { then, .. } => scan_then(line_no, then, calls, out),
                Stmt::IfElse {
                    then, else_then, ..
                } => {
                    scan_then(line_no, then, calls, out);
                    scan_then(line_no, else_then, calls, out);
                }
                Stmt::Rcomp { then, else_then } => {
                    scan_then(line_no, then, calls, out);
                    if let Some(e) = else_then {
                        scan_then(line_no, e, calls, out);
                    }
                }
                _ => {}
            }
        }
    }
    fn scan_then(line_no: u16, then: &ThenIr, calls: bool, out: &mut HashSet<(u16, u16)>) {
        match then {
            ThenIr::Goto(t) => {
                out.insert((line_no, *t));
            }
            ThenIr::Stmts(inner) => scan(line_no, inner, calls, out),
        }
    }
    let mut out = HashSet::new();
    for line in &module.lines {
        scan(line.number, &line.stmts, calls, &mut out);
    }
    out
}

/// Does a branch that *lands on this line* necessarily re-run the `FOR` header this `NEXT` is
/// paired with?
fn header_reruns_on_line_entry(next_id: &StmtId, for_id: &StmtId) -> bool {
    for_id.line_idx == next_id.line_idx
        && for_id.path.len() == 1
        && for_id.path < next_id.path
}

/// `NEXT`s that some FOR body branches to but that are not that FOR's
/// own lexical partner. `forty.bas` is the shape: two loops that each
/// `GOTO` into the other's `NEXT`, so source order pairs them the wrong
/// way round and both loops would run off their neighbour's counter.
fn cross_reached_nexts(
    module: &Module,
    pairs: &[(StmtId, StmtId)],
    bodies: &HashMap<StmtId, ForBodyInfo>,
) -> Vec<StmtId> {
    let partner_line: HashMap<&StmtId, u16> = pairs.iter().map(|(f, n)| (f, n.line_no)).collect();
    let next_partner_for: HashMap<&StmtId, &StmtId> =
        pairs.iter().map(|(f, n)| (n, f)).collect();
    let nexts = collect_all(module, |s| matches!(s, Stmt::Next { .. }));

    let innermost_open_at = |line: u16| -> Option<&StmtId> {
        bodies
            .iter()
            .filter(|(f, i)| f.line_no == line || i.lines_in_body.contains(&line))
            .map(|(f, _)| f)
            .max_by(|a, b| {
                a.line_idx
                    .cmp(&b.line_idx)
                    .then_with(|| a.path.cmp(&b.path))
            })
    };

    let is_bare_next = |n: &StmtId| {
        matches!(stmt_at(module, n), Some(Stmt::Next { vars })
            if vars.iter().any(Option::is_none))
    };
    let mut out: Vec<StmtId> = Vec::new();
    for (for_id, info) in bodies {
        let partner = partner_line.get(for_id).copied();
        for (from, target) in &info.goto_targets {
            if Some(*target) == partner {
                continue;
            }
            // Nested-loop exemption.
            let line_nexts = || nexts.iter().filter(|n| n.line_no == *target);
            let every_next_owned = line_nexts().count() > 0
                && line_nexts().all(|n| {
                    next_partner_for.get(n).is_some_and(|g| {
                        bodies
                            .get(*g)
                            .is_some_and(|gi| gi.lines_in_body.contains(from))
                            // "Its owning frame is live at the branch" is enough for `NEXT V`,
                            // which runs FNDFOR's search and finds V wherever it sits.
                            && (!is_bare_next(n)
                                || innermost_open_at(*from) == Some(*g))
                    })
                });
            if every_next_owned {
                continue;
            }
            for n in line_nexts() {
                // Self-contained one-line loop exemption.
                if next_partner_for
                    .get(n)
                    .is_some_and(|f| header_reruns_on_line_entry(n, f))
                {
                    continue;
                }
                if !out.contains(n) {
                    out.push(n.clone());
                }
            }
        }
    }
    // The loop above only sees a `NEXT` on the branch target line. A branch
    // that lands elsewhere can still fall through into one, so walk forward
    // and check the first `NEXT` on each path.
    if !has_unmodelled_control_flow(module) {
        let line_index: HashMap<u16, usize> = module
            .lines
            .iter()
            .enumerate()
            .map(|(i, l)| (l.number, i))
            .collect();
        let succ = fallthrough_successors(module);
        let marks = for_next_line_marks(module);
        let innermost_by_stmt = innermost_open_by_stmt(module);
        let mut memo: HashMap<usize, Vec<StmtId>> = HashMap::new();
        for edge in collect_edges_with_source(module) {
            // GOSUB pushes a FNDFOR marker, so the callee's `NEXT`s cannot
            // see the caller's frames. `nexts_behind_gosub_marker` owns that.
            if edge.gosub {
                continue;
            }
            // Only a branch taken from inside a loop body can leave a frame.
            let Some(open) = innermost_by_stmt.get(&edge.src) else {
                continue;
            };
            // `?UNDEF'D STATEMENT`: control never arrives.
            let Some(&target_idx) = line_index.get(&edge.target) else {
                continue;
            };
            // A `NEXT` on the target line is already handled above.
            if matches!(marks[target_idx], LineMark::Next(_)) {
                continue;
            }
            for n in first_nexts_from(&succ, &marks, &mut memo, target_idx) {
                // `NEXT V` finds its own frame. Only a bare `NEXT` takes the top.
                if !is_bare_next(&n) {
                    continue;
                }
                let Some(&partner) = next_partner_for.get(&n) else {
                    continue;
                };
                // It closes the frame the branch left open, so the static
                // binding is right.
                if partner == open {
                    continue;
                }
                if header_reruns_on_line_entry(&n, partner) {
                    continue;
                }
                if !out.contains(&n) {
                    out.push(n);
                }
            }
        }
    }

    // `bodies` is a HashMap; sort so the output doesn't depend on its
    // iteration order.
    out.sort_by(|a, b| {
        a.line_idx
            .cmp(&b.line_idx)
            .then_with(|| a.path.cmp(&b.path))
    });
    out
}

/// First `FOR` or `NEXT` on a line, for the forward walk in [`first_nexts_from`].
enum LineMark {
    /// A `NEXT` appears before any `FOR`.
    Next(StmtId),
    /// A `FOR` appears first. The walk stops: a later bare `NEXT` takes that frame.
    OpensFor,
    /// Nothing that moves the FOR stack.
    Plain,
}

/// The first `FOR` or `NEXT` on each line, in statement order.
fn for_next_line_marks(module: &Module) -> Vec<LineMark> {
    let mut marks: Vec<LineMark> = module.lines.iter().map(|_| LineMark::Plain).collect();
    for id in collect_all(module, |s| {
        matches!(s, Stmt::For { .. } | Stmt::Next { .. })
    }) {
        if !matches!(marks[id.line_idx], LineMark::Plain) {
            continue;
        }
        let is_next = matches!(stmt_at(module, &id), Some(Stmt::Next { .. }));
        let slot = id.line_idx;
        marks[slot] = if is_next {
            LineMark::Next(id)
        } else {
            LineMark::OpensFor
        };
    }
    marks
}

/// Static `GOTO` / `ON..GOTO` / `IF..THEN <line>` edges plus fall-through.
/// `GOSUB` is omitted: see the note at the call site.
fn fallthrough_successors(module: &Module) -> Vec<Vec<usize>> {
    let index: HashMap<u16, usize> = module
        .lines
        .iter()
        .enumerate()
        .map(|(i, l)| (l.number, i))
        .collect();
    let mut succ: Vec<Vec<usize>> = vec![Vec::new(); module.lines.len()];
    for edge in collect_edges_with_source(module) {
        if edge.gosub {
            continue;
        }
        let Some(&t) = index.get(&edge.target) else {
            continue;
        };
        let f = edge.src.line_idx;
        if !succ[f].contains(&t) {
            succ[f].push(t);
        }
    }
    for (i, line) in module.lines.iter().enumerate() {
        let terminates = line.stmts.iter().any(|s| {
            matches!(
                s,
                Stmt::Goto { .. }
                    | Stmt::Return
                    | Stmt::End
                    | Stmt::Stop
                    | Stmt::RaiseError(_)
                    | Stmt::Run(_)
                    | Stmt::ComputedGoto { .. }
            )
        });
        if !terminates && i + 1 < module.lines.len() && !succ[i].contains(&(i + 1)) {
            succ[i].push(i + 1);
        }
    }
    succ
}

/// Innermost `FOR` open at each branch statement, not per line.
///
/// `FOR J=1 TO 5:IF J=2 THEN 35` is inside J. `ON-(X)GOTO 7:FOR I=...` is not.
fn innermost_open_by_stmt(module: &Module) -> HashMap<StmtId, StmtId> {
    fn record(stmt: &Stmt, here: &StmtId, stack: &[StmtId], out: &mut HashMap<StmtId, StmtId>) {
        let Some(top) = stack.last() else {
            return;
        };
        if matches!(
            stmt,
            Stmt::Goto { .. }
                | Stmt::OnBranch { .. }
                | Stmt::If { .. }
                | Stmt::IfElse { .. }
                | Stmt::Rcomp { .. }
        ) {
            out.insert(here.clone(), top.clone());
        }
        match stmt {
            Stmt::If { then, .. } => record_then(then, here, stack, out),
            Stmt::IfElse {
                then, else_then, ..
            } => {
                record_then(then, here, stack, out);
                record_then(else_then, here, stack, out);
            }
            Stmt::Rcomp { then, else_then } => {
                record_then(then, here, stack, out);
                if let Some(et) = else_then {
                    record_then(et, here, stack, out);
                }
            }
            _ => {}
        }
    }
    fn record_then(
        then: &ThenIr,
        parent: &StmtId,
        stack: &[StmtId],
        out: &mut HashMap<StmtId, StmtId>,
    ) {
        if let ThenIr::Stmts(stmts) = then {
            for (idx, inner) in stmts.iter().enumerate() {
                let mut path = parent.path.clone();
                path.push(idx);
                let id = StmtId {
                    line_no: parent.line_no,
                    line_idx: parent.line_idx,
                    path,
                };
                record(inner, &id, stack, out);
            }
        }
    }
    let mut out: HashMap<StmtId, StmtId> = HashMap::new();
    let mut stack: Vec<StmtId> = Vec::new();
    let mut vars_of: HashMap<StmtId, VarName> = HashMap::new();
    for (line_idx, line) in module.lines.iter().enumerate() {
        for (top_idx, stmt) in line.stmts.iter().enumerate() {
            let here = StmtId {
                line_no: line.number,
                line_idx,
                path: vec![top_idx],
            };
            record(stmt, &here, &stack, &mut out);
            match stmt {
                Stmt::For { var, .. } => {
                    vars_of.insert(here.clone(), var.clone());
                    stack.push(here);
                }
                Stmt::Next { vars } => {
                    close_textual_frames(&mut stack, vars, |id| vars_of[id].clone());
                }
                _ => {}
            }
        }
    }
    out
}

/// The `NEXT`s ordinary forward execution can run into from `start` without
/// opening a `FOR` first. The visited set is the termination guard: every line
/// index is pushed at most once, so a `GOTO` cycle simply stops.
fn first_nexts_from(
    succ: &[Vec<usize>],
    marks: &[LineMark],
    memo: &mut HashMap<usize, Vec<StmtId>>,
    start: usize,
) -> Vec<StmtId> {
    if let Some(hit) = memo.get(&start) {
        return hit.clone();
    }
    let mut seen: HashSet<usize> = HashSet::new();
    seen.insert(start);
    let mut work = vec![start];
    let mut out: Vec<StmtId> = Vec::new();
    while let Some(i) = work.pop() {
        match &marks[i] {
            LineMark::Next(id) => {
                if !out.contains(id) {
                    out.push(id.clone());
                }
            }
            LineMark::OpensFor => {}
            LineMark::Plain => {
                for &n in &succ[i] {
                    if seen.insert(n) {
                        work.push(n);
                    }
                }
            }
        }
    }
    // `seen` is a HashSet, so fix the order before anyone can observe it.
    out.sort_by(|a, b| {
        a.line_idx
            .cmp(&b.line_idx)
            .then_with(|| a.path.cmp(&b.path))
    });
    memo.insert(start, out.clone());
    out
}

/// Identifies one conditional-branch body: the enclosing IF's location plus which arm.
type Guard = (usize, Vec<usize>, u8);

/// One entry of the simulated compile-time FOR stack.
#[derive(Clone)]
struct SimFrame {
    id: StmtId,
    var: VarName,
    guard: Vec<Guard>,
}

/// State for pairing FOR and NEXT across conditional paths.
struct PairWalk {
    for_stack: Vec<SimFrame>,
    /// Set once a `NEXT` has popped a frame whose existence depends on a branch.
    diverged: bool,

    must_be_runtime: Vec<StmtId>,

    discarded: Option<Discard>,

    maybe_live: Vec<SimFrame>,

    /// Line successors, with every RETURN going back to every GOSUB's next line.
    line_succ: Vec<Vec<usize>>,
    /// Fallthrough prefix length and whether a call follows it.
    line_prefix: Vec<(usize, bool)>,
}

#[derive(Clone)]
struct Discard {
    /// Guard in force at the discarding `FOR`.
    guard: Vec<Guard>,

    reached_past_live_frames: bool,
    /// Frames discarded by each FOR in the current nest.
    history: Vec<(StmtId, Vec<StmtId>)>,
}

impl PairWalk {
    /// Whether every path to NEXT passes through the discarding FOR.
    fn discard_dominates(&self, d: &Discard, next: &StmtId) -> bool {
        d.history.iter().all(|(by, frames)| {
            let [at] = by.path.as_slice() else {
                return false;
            };
            let by_line = by.line_idx;
            let (clean_prefix, gosub_later) = self.line_prefix[by_line];
            if *at > clean_prefix || gosub_later {
                return false;
            }
            if next.line_idx == by_line {
                return next.path.first().is_some_and(|k| k > at);
            }
            frames.iter().all(|f| {
                f.line_idx == by_line
                    || !reach_avoiding(&self.line_succ, f.line_idx, by_line)
                        .contains(&next.line_idx)
            })
        })
    }
}

fn guard_covers(for_guard: &[Guard], next_guard: &[Guard]) -> bool {
    for_guard.len() <= next_guard.len() && next_guard[..for_guard.len()] == *for_guard
}

/// Walk in source order with a simulated FOR-stack; unmatched NEXTs land in
/// `analysis.orphan_nexts`.
fn static_pair_walk(
    module: &Module,
    analysis: &mut OrphanAnalysis,
) -> (Vec<(StmtId, StmtId)>, Vec<StmtId>) {
    let mut pairs = Vec::new();
    let mut line_succ = terminating_line_successors_with(module, true);
    let continuations: Vec<usize> = module
        .lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.stmts.iter().any(stmt_calls))
        .map(|(i, _)| i + 1)
        .filter(|&i| i < module.lines.len())
        .collect();
    for (i, l) in module.lines.iter().enumerate() {
        if l.stmts.iter().any(stmt_returns) {
            for &c in &continuations {
                if !line_succ[i].contains(&c) {
                    line_succ[i].push(c);
                }
            }
        }
    }
    let line_prefix = module
        .lines
        .iter()
        .map(|l| {
            let clean = l.stmts.iter().take_while(|s| stmt_falls_through(s)).count();
            let gosub_later = l.stmts.iter().any(stmt_calls);
            (clean, gosub_later)
        })
        .collect();
    let mut walk = PairWalk {
        for_stack: Vec::new(),
        diverged: false,
        must_be_runtime: Vec::new(),
        discarded: None,
        maybe_live: Vec::new(),
        line_succ,
        line_prefix,
    };
    for (line_idx, line) in module.lines.iter().enumerate() {
        for (top_idx, stmt) in line.stmts.iter().enumerate() {
            let here = StmtId {
                line_no: line.number,
                line_idx,
                path: vec![top_idx],
            };
            walk_stmt_for_pairing(stmt, &here, &[], &mut walk, &mut pairs, analysis);
        }
    }
    (pairs, walk.must_be_runtime)
}

fn walk_stmt_for_pairing(
    stmt: &Stmt,
    here: &StmtId,
    guard: &[Guard],
    walk: &mut PairWalk,
    pairs: &mut Vec<(StmtId, StmtId)>,
    analysis: &mut OrphanAnalysis,
) {
    match stmt {
        Stmt::For { var, .. } => {

            if let Some(pos) = walk.for_stack.iter().rposition(|f| &f.var == var) {

                let reached_past_live_frames = pos + 1 < walk.for_stack.len();
                let frames = walk.for_stack[pos..].iter().map(|f| f.id.clone()).collect();
                walk.for_stack.truncate(pos);
                let mut history = walk.discarded.take().map(|d| d.history).unwrap_or_default();
                history.push((here.clone(), frames));
                walk.discarded = Some(Discard {
                    guard: guard.to_vec(),
                    reached_past_live_frames,
                    history,
                });
            } else if walk.for_stack.is_empty() {
                // A `FOR` opening on an empty stack starts a fresh nest, and nothing a discard did earlier explains an underflow inside it.
                walk.discarded = None;
            }

            if let Some(pos) = walk.maybe_live.iter().rposition(|f| &f.var == var) {
                let stale = walk.maybe_live.remove(pos);
                walk.diverged = true;

                walk.must_be_runtime.push(stale.id);
                walk.must_be_runtime.push(here.clone());
                for live in &walk.for_stack {
                    walk.must_be_runtime.push(live.id.clone());
                }
            }
            walk.for_stack.push(SimFrame {
                id: here.clone(),
                var: var.clone(),
                guard: guard.to_vec(),
            });
        }
        Stmt::Next { vars } => {
            for v in vars {
                // Frames a `NEXT V` walks *past* matter as much as the one it matches.
                let mut walked_conditional = false;
                let mut walked: Vec<StmtId> = Vec::new();
                let popped = match v {
                    None => walk.for_stack.pop(),
                    // A failed NEXT search leaves frames unchanged.
                    Some(target) if !walk.for_stack.iter().any(|f| &f.var == target) => None,
                    Some(target) => {
                        let mut found = None;
                        while let Some(frame) = walk.for_stack.last() {
                            if &frame.var == target {
                                found = walk.for_stack.pop();
                                break;
                            }
                            let abandoned =
                                walk.for_stack.pop().expect("last() just returned Some");
                            if !guard_covers(&abandoned.guard, guard) {
                                walked_conditional = true;
                            }
                            walked.push(abandoned.id);
                        }
                        found
                    }
                };
                match popped {
                    Some(frame) => {
                        // A conditional NEXT may leave a later frame above this one.
                        let opened_after = |f: &SimFrame| {
                            (f.id.line_idx, &f.id.path) > (frame.id.line_idx, &frame.id.path)
                        };
                        let maybe_above: Vec<StmtId> = walk
                            .maybe_live
                            .iter()
                            .filter(|f| opened_after(f) && v.as_ref().is_none_or(|t| &f.var == t))
                            .map(|f| f.id.clone())
                            .collect();
                        // The frame is only guaranteed to be on the runtime stack when this
                        // NEXT runs if the FOR executed unconditionally relative to it.
                        let ambiguous = walk.diverged
                            || walked_conditional
                            || !maybe_above.is_empty()
                            || !guard_covers(&frame.guard, guard);
                        if ambiguous {
                            walk.diverged = true;
                            if !analysis.orphan_nexts.contains(here) {
                                analysis.orphan_nexts.push(here.clone());
                            }
                            walk.must_be_runtime.push(frame.id.clone());
                            walk.must_be_runtime.extend(walked.iter().cloned());
                            walk.must_be_runtime.extend(maybe_above);
                            for live in &walk.for_stack {
                                walk.must_be_runtime.push(live.id.clone());
                            }
                        } else if !frame.guard.is_empty() {

                            analysis.guard_covered_closures.insert(here.clone());
                        }
                        analysis
                            .static_pairs
                            .entry(here.clone())
                            .or_default()
                            .push(Some(frame.id.clone()));
                        pairs.push((frame.id, here.clone()));
                    }
                    None => {
                        analysis
                            .static_pairs
                            .entry(here.clone())
                            .or_default()
                            .push(None);
                        // "No partner" is recorded either way, because that is what codegen will conclude too.
                        let discard_explains_this = walk.discarded.as_ref().is_some_and(|d| {
                            guard_covers(&d.guard, guard)
                                && !d.reached_past_live_frames
                                && walk.discard_dominates(d, here)
                        });

                        let maybe_live_target = v
                            .as_ref()
                            .is_some_and(|t| walk.maybe_live.iter().any(|f| &f.var == t));

                        if !discard_explains_this || maybe_live_target {
                            // A runtime NEXT may invalidate the remaining static stack.
                            let finds_only_above_held = maybe_live_target
                                && v.as_ref().is_some_and(|t| {
                                    walk.maybe_live.iter().filter(|f| &f.var == t).all(|f| {
                                        walk.for_stack.iter().all(|held| {
                                            (f.id.line_idx, &f.id.path)
                                                > (held.id.line_idx, &held.id.path)
                                        })
                                    })
                                });
                            if !finds_only_above_held {
                                walk.diverged = true;
                            }
                            analysis.orphan_nexts.push(here.clone());
                        }
                    }
                }
                if walk.for_stack.is_empty() {
                    // Both models are back to "nothing open"; a loop opened after this point
                    // pairs statically again.
                    walk.diverged = false;
                }
            }
        }
        Stmt::If { then, .. } => walk_then(then, here, guard, 0, walk, pairs, analysis),
        Stmt::IfElse {
            then, else_then, ..
        } => {
            walk_then(then, here, guard, 0, walk, pairs, analysis);
            walk_then(else_then, here, guard, 1, walk, pairs, analysis);
        }
        Stmt::Rcomp { then, else_then } => {
            walk_then(then, here, guard, 0, walk, pairs, analysis);
            if let Some(et) = else_then {
                walk_then(et, here, guard, 1, walk, pairs, analysis);
            }
        }
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn walk_then(
    then: &ThenIr,
    parent: &StmtId,
    guard: &[Guard],
    arm: u8,
    walk: &mut PairWalk,
    pairs: &mut Vec<(StmtId, StmtId)>,
    analysis: &mut OrphanAnalysis,
) {
    if let ThenIr::Stmts(stmts) = then {
        let mut inner_guard = guard.to_vec();
        inner_guard.push((parent.line_idx, parent.path.clone(), arm));
        let depth_before = walk.for_stack.len();
        let saved = walk.for_stack.clone();
        for (idx, inner) in stmts.iter().enumerate() {
            let mut path = parent.path.clone();
            path.push(idx);
            let id = StmtId {
                line_no: parent.line_no,
                line_idx: parent.line_idx,
                path,
            };
            walk_stmt_for_pairing(inner, &id, &inner_guard, walk, pairs, analysis);
        }

        for frame in walk.for_stack.iter().skip(depth_before) {
            analysis.conditional_fors.insert(frame.id.clone());
        }

        if walk.for_stack.len() < depth_before {
            if matches!(
                stmts.last(),
                Some(Stmt::End) | Some(Stmt::Stop) | Some(Stmt::Run(_))
            ) {
                walk.for_stack = saved;
            } else {

                for frame in saved.into_iter().skip(walk.for_stack.len()).take(
                    depth_before.saturating_sub(walk.for_stack.len()),
                ) {
                    if !walk.maybe_live.iter().any(|f| f.id == frame.id) {
                        walk.maybe_live.push(frame);
                    }
                }
            }
        }
    }
}

/// Sentinel "from" line for GOSUB-reached targets: no lexical body ever contains it, so the
/// nested-loop exemption in `cross_reached_nexts` can never fire for a GOSUB.
const GOSUB_FROM: u16 = u16::MAX;

#[derive(Debug, Clone, Default)]
struct ForBodyInfo {
    /// Lines covered between the FOR header and its matching NEXT.
    lines_in_body: Vec<u16>,
    /// `(from_line, target_line)` branches reachable from inside the body.
    goto_targets: HashSet<(u16, u16)>,
    /// True when no static NEXT was found.
    no_static_match: bool,
    /// The FOR's loop variable.
    var: Option<VarName>,
}

fn collect_all(module: &Module, pred: impl Fn(&Stmt) -> bool + Copy) -> Vec<StmtId> {
    let mut out = Vec::new();
    for (line_idx, line) in module.lines.iter().enumerate() {
        for (top_idx, stmt) in line.stmts.iter().enumerate() {
            collect_stmt(
                stmt,
                StmtId {
                    line_no: line.number,
                    line_idx,
                    path: vec![top_idx],
                },
                &mut out,
                &pred,
            );
        }
    }
    out
}

fn collect_stmt(
    stmt: &Stmt,
    id: StmtId,
    out: &mut Vec<StmtId>,
    pred: &impl Fn(&Stmt) -> bool,
) {
    if pred(stmt) {
        out.push(id.clone());
    }
    match stmt {
        Stmt::If { then, .. } => recurse_then(then, &id, out, pred),
        Stmt::IfElse {
            then, else_then, ..
        } => {
            recurse_then(then, &id, out, pred);
            recurse_then(else_then, &id, out, pred);
        }
        Stmt::Rcomp { then, else_then } => {
            recurse_then(then, &id, out, pred);
            if let Some(et) = else_then {
                recurse_then(et, &id, out, pred);
            }
        }
        _ => {}
    }
}

fn recurse_then(
    then: &ThenIr,
    parent: &StmtId,
    out: &mut Vec<StmtId>,
    pred: &impl Fn(&Stmt) -> bool,
) {
    if let ThenIr::Stmts(stmts) = then {
        for (idx, inner) in stmts.iter().enumerate() {
            let mut path = parent.path.clone();
            path.push(idx);
            let id = StmtId {
                line_no: parent.line_no,
                line_idx: parent.line_idx,
                path,
            };
            collect_stmt(inner, id, out, pred);
        }
    }
}

/// Per-FOR body info: lines covered, GOTO targets, static-match flag.
/// Only top-level NEXTs close a FOR's lexical body; NEXTs inside an
/// IF/IFELSE/RCOMP body fire conditionally, so the FOR can stay
/// active past them at runtime.
fn collect_goto_targets_by_enclosing_for(module: &Module) -> HashMap<StmtId, ForBodyInfo> {
    let mut result: HashMap<StmtId, ForBodyInfo> = HashMap::new();
    let mut for_stack: Vec<(StmtId, ForBodyInfo)> = Vec::new();

    for (line_idx, line) in module.lines.iter().enumerate() {
        for (_, info) in for_stack.iter_mut() {
            info.lines_in_body.push(line.number);
        }
        for (top_idx, stmt) in line.stmts.iter().enumerate() {
            let here = StmtId {
                line_no: line.number,
                line_idx,
                path: vec![top_idx],
            };
            walk_stmt_for_body_info(stmt, &here, &mut for_stack, &mut result, false);
        }
    }
    while let Some((id, mut info)) = for_stack.pop() {
        info.no_static_match = true;
        result.insert(id, info);
    }
    result
}

fn walk_stmt_for_body_info(
    stmt: &Stmt,
    here: &StmtId,
    for_stack: &mut Vec<(StmtId, ForBodyInfo)>,
    result: &mut HashMap<StmtId, ForBodyInfo>,
    nested: bool,
) {
    match stmt {
        Stmt::For { var, .. } => {
            if !nested {
                for_stack.push((
                    here.clone(),
                    ForBodyInfo {
                        var: Some(var.clone()),
                        ..ForBodyInfo::default()
                    },
                ));
            }
        }
        Stmt::Next { vars } => {
            if !nested {
                let (closed, abandoned) = close_textual_frames(for_stack, vars, |(_, info)| {
                    info.var.clone().expect("every pushed frame names its variable")
                });
                for (id, info) in closed {
                    result.insert(id, info);
                }
                // A frame a NEXT abandons has no static NEXT of its own.
                for (id, mut info) in abandoned {
                    info.no_static_match = true;
                    result.insert(id, info);
                }
            }
        }
        Stmt::Goto { target } => {
            for (_, info) in for_stack.iter_mut() {
                info.goto_targets.insert((here.line_no, *target));
            }
        }
        Stmt::GoSub { target } => {
            for (_, info) in for_stack.iter_mut() {
                info.goto_targets.insert((GOSUB_FROM, *target));
            }
        }
        // `ON n GOTO`/`ON n GOSUB` is a branch like any other.
        Stmt::OnBranch { kind, targets, .. } => {
            let from = match kind {
                OnBranchKind::GoSub => GOSUB_FROM,
                OnBranchKind::Goto => here.line_no,
            };
            for (_, info) in for_stack.iter_mut() {
                for t in targets {
                    info.goto_targets.insert((from, *t));
                }
            }
        }
        Stmt::ComputedGoto { .. } => {}
        Stmt::If { then, .. } => walk_then_body(then, here, for_stack, result),
        Stmt::IfElse {
            then, else_then, ..
        } => {
            walk_then_body(then, here, for_stack, result);
            walk_then_body(else_then, here, for_stack, result);
        }
        Stmt::Rcomp { then, else_then } => {
            walk_then_body(then, here, for_stack, result);
            if let Some(et) = else_then {
                walk_then_body(et, here, for_stack, result);
            }
        }
        _ => {}
    }
}

fn walk_then_body(
    then: &ThenIr,
    parent: &StmtId,
    for_stack: &mut Vec<(StmtId, ForBodyInfo)>,
    result: &mut HashMap<StmtId, ForBodyInfo>,
) {
    match then {
        ThenIr::Stmts(stmts) => {
            for (idx, inner) in stmts.iter().enumerate() {
                let mut path = parent.path.clone();
                path.push(idx);
                let id = StmtId {
                    line_no: parent.line_no,
                    line_idx: parent.line_idx,
                    path,
                };
                walk_stmt_for_body_info(inner, &id, for_stack, result, true);
            }
        }
        ThenIr::Goto(target) => {
            for (_, info) in for_stack.iter_mut() {
                info.goto_targets.insert((parent.line_no, *target));
            }
        }
    }
}

fn stmt_at<'a>(module: &'a Module, id: &StmtId) -> Option<&'a Stmt> {
    let line = module.lines.get(id.line_idx)?;
    let mut stmt = line.stmts.get(*id.path.first()?)?;
    for &idx in &id.path[1..] {
        stmt = match stmt {
            Stmt::If {
                then: ThenIr::Stmts(inner),
                ..
            } => inner.get(idx)?,
            Stmt::IfElse {
                then: ThenIr::Stmts(inner),
                else_then,
                ..
            } => {
                if idx < inner.len() {
                    &inner[idx]
                } else if let ThenIr::Stmts(es) = else_then {
                    es.get(idx - inner.len())?
                } else {
                    return None;
                }
            }
            Stmt::Rcomp { then, else_then } => {
                let then_len = if let ThenIr::Stmts(inner) = then {
                    if idx < inner.len() {
                        stmt = &inner[idx];
                        continue;
                    }
                    inner.len()
                } else {
                    0
                };
                if let Some(ThenIr::Stmts(es)) = else_then {
                    es.get(idx - then_len)?
                } else {
                    return None;
                }
            }
            _ => return None,
        };
    }
    Some(stmt)
}

// --- tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{VarKind, VarName};
    use crate::ir::{Expr, Line};

    fn fvar(name: &str) -> VarName {
        VarName {
            base: name.to_string(),
            kind: VarKind::Float,
        }
    }

    fn for_stmt(var: &str, start: f64, end: f64) -> Stmt {
        Stmt::For {
            var: fvar(var),
            start: Expr::Number(start),
            end: Expr::Number(end),
            step: Expr::Number(1.0),
            body_int_safe: false,
            body_reads_loop_var: true,
            induction_const: None,
            array_inductions: Vec::new(),
        }
    }

    fn next_stmt() -> Stmt {
        Stmt::Next { vars: vec![None] }
    }

    fn next_var(v: &str) -> Stmt {
        Stmt::Next {
            vars: vec![Some(fvar(v))],
        }
    }

    fn line(n: u16, stmts: Vec<Stmt>) -> Line {
        Line { number: n, stmts }
    }

    fn if_goto(target: u16) -> Stmt {
        Stmt::If {
            cond: Expr::Number(1.0),
            then: crate::ir::ThenIr::Goto(target),
        }
    }

    /// Tier of the (single) `FOR` on the given source line.
    fn tier_of(m: &Module, a: &OrphanAnalysis, line_no: u16) -> ForTier {
        let id = collect_all(m, |s| matches!(s, Stmt::For { .. }))
            .into_iter()
            .find(|s| s.line_no == line_no)
            .unwrap_or_else(|| panic!("no FOR on line {line_no}"));
        a.for_tier(id.line_no, id.line_idx, &id.path)
    }

    #[test]
    fn no_orphan_in_well_formed_program() {
        let m = Module {
            lines: vec![
                line(10, vec![for_stmt("I", 1.0, 10.0)]),
                line(20, vec![next_stmt()]),
                line(30, vec![Stmt::End]),
            ],
        };
        let a = analyze(&m);
        assert!(!a.needs_runtime_stack());
        assert!(a.orphan_nexts.is_empty());
        assert!(a.runtime_fors.is_empty());
    }

    #[test]
    fn detects_orphan_next() {
        // 10 GOTO 30 : 20 NEXT : 30 FOR I=0 TO 10 : 40 GOTO 20
        let m = Module {
            lines: vec![
                line(10, vec![Stmt::Goto { target: 30 }]),
                line(20, vec![next_stmt(), Stmt::End]),
                line(30, vec![for_stmt("I", 0.0, 10.0)]),
                line(40, vec![Stmt::Goto { target: 20 }]),
            ],
        };
        let a = analyze(&m);
        assert!(a.needs_runtime_stack());
        assert_eq!(a.orphan_nexts.len(), 1);
        assert_eq!(a.orphan_nexts[0].line_no, 20);
        assert!(a.runtime_fors.keys().any(|s| s.line_no == 30));
    }

    #[test]
    fn then_jump_to_own_nested_next_stays_static() {
        // The bubble-sort nest: 20 FOR I : 30 FOR X : 50 IF ..
        let m = Module {
            lines: vec![
                line(20, vec![for_stmt("I", 1.0, 10.0)]),
                line(30, vec![for_stmt("X", 0.0, 238.0)]),
                line(
                    50,
                    vec![Stmt::If {
                        cond: Expr::Number(1.0),
                        then: crate::ir::ThenIr::Goto(70),
                    }],
                ),
                line(70, vec![next_stmt()]),
                line(80, vec![next_stmt(), Stmt::End]),
            ],
        };
        let a = analyze(&m);
        assert!(
            !a.needs_runtime_stack(),
            "THEN into the inner loop's own NEXT must stay static: {:?}",
            a.orphan_nexts
        );
    }

    /// A `NEXT V` abandons every frame it walks past.
    #[test]
    fn next_var_walking_past_a_conditional_frame_is_an_orphan() {
        // 10 FOR I : 20 IF ..
        let m = Module {
            lines: vec![
                line(10, vec![for_stmt("I", 1.0, 2.0)]),
                line(
                    20,
                    vec![Stmt::If {
                        cond: Expr::Number(1.0),
                        then: crate::ir::ThenIr::Stmts(vec![for_stmt("J", 1.0, 2.0)]),
                    }],
                ),
                line(30, vec![next_var("I"), Stmt::End]),
            ],
        };
        let a = analyze(&m);
        assert!(
            a.is_orphan_next(30, 2, &[0]),
            "NEXT I walks past a frame that only exists on the taken \
             path, so it must dispatch: {:?}",
            a.orphan_nexts
        );
    }

    #[test]
    fn next_var_walking_past_an_unconditional_frame_stays_static() {
        // 10 FOR I : 20 FOR J : 30 NEXT I
        let m = Module {
            lines: vec![
                line(10, vec![for_stmt("I", 1.0, 2.0)]),
                line(20, vec![for_stmt("J", 1.0, 2.0)]),
                line(30, vec![next_var("I"), Stmt::End]),
            ],
        };
        let a = analyze(&m);
        assert!(
            !a.needs_runtime_stack(),
            "a wholly unconditional nest must stay static: {:?}",
            a.orphan_nexts
        );
    }

    #[test]
    fn jump_into_closed_inner_next_stays_orphan() {
        // 10 FOR I : 20 FOR X : 30 NEXT (X) : 40 IF ..
        let m = Module {
            lines: vec![
                line(10, vec![for_stmt("I", 1.0, 3.0)]),
                line(20, vec![for_stmt("X", 0.0, 9.0)]),
                line(30, vec![next_stmt()]),
                line(
                    40,
                    vec![Stmt::If {
                        cond: Expr::Number(1.0),
                        then: crate::ir::ThenIr::Goto(30),
                    }],
                ),
                line(50, vec![next_stmt(), Stmt::End]),
            ],
        };
        let a = analyze(&m);
        assert!(
            a.needs_runtime_stack(),
            "re-entry into a closed loop's NEXT must stay on the runtime path"
        );
    }

    #[test]
    fn clr_inside_the_loop_orphans_its_next() {
        // 10 FOR I=1 TO 3 : 20 CLR : 30 NEXT
        let m = Module {
            lines: vec![
                line(10, vec![for_stmt("I", 1.0, 3.0)]),
                line(20, vec![Stmt::Clr]),
                line(30, vec![next_stmt(), Stmt::End]),
            ],
        };
        let a = analyze(&m);
        assert!(a.needs_runtime_stack());
        assert_eq!(a.orphan_nexts.len(), 1);
        assert_eq!(a.orphan_nexts[0].line_no, 30);
        assert!(
            a.runtime_fors.keys().any(|s| s.line_no == 10),
            "the FOR must get a handler id or the dispatcher has no frame to find"
        );
    }

    /// The other half, and the one that keeps the fast path alive: a reset the loop header
    /// re-runs after is harmless, so the `NEXT` stays inline.
    #[test]
    fn clr_before_the_loop_leaves_it_inline() {
        // 10 CLR : 20 FOR I=1 TO 3 : 30 NEXT
        let m = Module {
            lines: vec![
                line(10, vec![Stmt::Clr]),
                line(20, vec![for_stmt("I", 1.0, 3.0)]),
                line(30, vec![next_stmt(), Stmt::End]),
            ],
        };
        let a = analyze(&m);
        assert!(
            !a.needs_runtime_stack(),
            "a CLR ahead of the header cannot reach the NEXT: {:?}",
            a.orphan_nexts
        );
    }

    #[test]
    fn clr_and_header_on_one_line_go_by_statement_order() {
        // 10 CLR : FOR I=1 TO 3 : NEXT  -> header re-runs, stays inline
        let after = Module {
            lines: vec![line(
                10,
                vec![Stmt::Clr, for_stmt("I", 1.0, 3.0), next_stmt(), Stmt::End],
            )],
        };
        assert!(
            !analyze(&after).needs_runtime_stack(),
            "header after the reset rebuilds the frame"
        );

        // 10 FOR I=1 TO 3 : CLR : NEXT  -> frame is gone at the NEXT
        let before = Module {
            lines: vec![line(
                10,
                vec![for_stmt("I", 1.0, 3.0), Stmt::Clr, next_stmt(), Stmt::End],
            )],
        };
        assert!(
            analyze(&before).needs_runtime_stack(),
            "reset after the header leaves the NEXT with no frame"
        );
    }

    #[test]
    fn shared_next_between_two_fors() {
        // Two FORs sharing one orphan NEXT via GOTO / GOSUB / RETURN.
        let m = Module {
            lines: vec![
                line(10, vec![Stmt::Goto { target: 30 }]),
                line(20, vec![next_stmt(), Stmt::End]),
                line(30, vec![for_stmt("I", 0.0, 10.0)]),
                line(50, vec![Stmt::GoSub { target: 100 }]),
                line(60, vec![Stmt::Goto { target: 20 }]),
                line(100, vec![for_stmt("P", 0.0, 40.0)]),
                line(120, vec![Stmt::Return]),
                line(130, vec![Stmt::Goto { target: 20 }]),
            ],
        };
        let a = analyze(&m);
        assert!(a.needs_runtime_stack());
        assert_eq!(a.orphan_nexts.len(), 1);
        assert!(a.runtime_fors.keys().any(|s| s.line_no == 30));
        assert!(a.runtime_fors.keys().any(|s| s.line_no == 100));
        assert!(a.runtime_gosubs.iter().any(|s| s.line_no == 50));
        assert!(a.runtime_returns.iter().any(|s| s.line_no == 120));
        // IDs are 1-based and unique.
        let ids: Vec<u8> = a.runtime_fors.values().map(|(h, _)| *h).collect();
        assert!(ids.iter().all(|&v| v >= 1));
        // Every runtime FOR carries its loop variable's identity, and the two loops here use
        // different variables.
        let vars: Vec<u8> = a.runtime_fors.values().map(|(_, v)| *v).collect();
        assert!(vars.iter().all(|&v| v >= 1));
        assert_eq!(
            {
                let mut v = vars.clone();
                v.sort();
                v.dedup();
                v.len()
            },
            vars.len(),
            "FOR I and FOR P must get distinct var ids"
        );
        let mut sorted = ids.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len());
    }

    /// Tier 0 has to survive the presence of a dispatcher elsewhere in the program.
    #[test]
    fn uncrossed_loop_stays_tier0_next_to_a_dispatcher() {

        let m = Module {
            lines: vec![
                line(10, vec![Stmt::Goto { target: 30 }]),
                line(20, vec![next_stmt(), Stmt::End]),
                line(30, vec![for_stmt("I", 0.0, 10.0)]),
                line(40, vec![Stmt::Goto { target: 20 }]),
                line(50, vec![for_stmt("K", 1.0, 3.0)]),
                line(60, vec![next_var("K")]),
                line(70, vec![Stmt::End]),
            ],
        };
        let a = analyze(&m);
        assert!(a.needs_runtime_stack());
        assert_eq!(tier_of(&m, &a, 30), ForTier::Dispatched);
        assert_eq!(
            tier_of(&m, &a, 50),
            ForTier::Static,
            "a loop nothing branches across must keep the fast path"
        );
    }

    /// Tier 1: the body boundary is crossed, so the frame has to exist, but every dispatching
    /// `NEXT` in the program names a different variable and cannot select it.
    #[test]
    fn crossed_loop_with_no_reachable_dispatcher_is_tier1() {
        // 10 GOTO 50 / 20 NEXT J:END / 30 END / 50 FOR J 60 FOR W / 70 IF c THEN 100 / 80 NEXT
        // W / 90 GOTO 20 / 100 END
        let m = Module {
            lines: vec![
                line(10, vec![Stmt::Goto { target: 50 }]),
                line(20, vec![next_var("J"), Stmt::End]),
                line(30, vec![Stmt::End]),
                line(50, vec![for_stmt("J", 1.0, 3.0)]),
                line(60, vec![for_stmt("W", 1.0, 3.0)]),
                line(70, vec![if_goto(100)]),
                line(80, vec![next_var("W")]),
                line(90, vec![Stmt::Goto { target: 20 }]),
                line(100, vec![Stmt::End]),
            ],
        };
        let a = analyze(&m);
        assert_eq!(tier_of(&m, &a, 50), ForTier::Dispatched);
        assert_eq!(
            tier_of(&m, &a, 60),
            ForTier::Framed,
            "`70 IF c THEN 100` leaves W's body, so W needs a frame — but \
             the only dispatcher is `NEXT J`, which can never match it"
        );

        let w = collect_all(&m, |s| matches!(s, Stmt::For { .. }))
            .into_iter()
            .find(|s| s.line_no == 60)
            .unwrap();
        let (handler, _) = a.runtime_fors[&w];
        assert_eq!(a.tier1_handler_id, Some(handler));
        assert!(!a.handler_fors.contains(&w));
    }

    #[test]
    fn dispatcher_naming_the_same_variable_forces_tier2() {
        let m = Module {
            lines: vec![
                line(10, vec![Stmt::Goto { target: 50 }]),
                line(20, vec![next_var("W"), Stmt::End]),
                line(30, vec![Stmt::End]),
                line(50, vec![for_stmt("J", 1.0, 3.0)]),
                line(60, vec![for_stmt("W", 1.0, 3.0)]),
                line(70, vec![if_goto(100)]),
                line(80, vec![next_var("W")]),
                line(90, vec![Stmt::Goto { target: 20 }]),
                line(100, vec![Stmt::End]),
            ],
        };
        let a = analyze(&m);
        assert_eq!(
            tier_of(&m, &a, 60),
            ForTier::Dispatched,
            "`NEXT W` at line 20 can land on this frame"
        );
    }

    #[test]
    fn reachable_bare_dispatcher_forces_tier2() {
        let m = Module {
            lines: vec![
                line(10, vec![Stmt::Goto { target: 50 }]),
                line(20, vec![next_stmt(), Stmt::End]),
                line(30, vec![Stmt::End]),
                line(50, vec![for_stmt("J", 1.0, 3.0)]),
                line(60, vec![for_stmt("W", 1.0, 3.0)]),
                line(70, vec![if_goto(100)]),
                line(80, vec![next_var("W")]),
                line(90, vec![Stmt::Goto { target: 20 }]),
                line(100, vec![Stmt::End]),
            ],
        };
        let a = analyze(&m);
        assert_eq!(tier_of(&m, &a, 60), ForTier::Dispatched);
    }

    #[test]
    fn unreachable_bare_dispatcher_leaves_tier1_intact() {

        let m = Module {
            lines: vec![
                line(10, vec![Stmt::Goto { target: 50 }]),
                line(20, vec![next_stmt(), Stmt::Goto { target: 100 }]),
                line(50, vec![for_stmt("J", 1.0, 3.0)]),
                line(60, vec![Stmt::Goto { target: 20 }]),
                line(100, vec![for_stmt("W", 1.0, 3.0)]),
                line(110, vec![if_goto(130)]),
                line(120, vec![next_var("W")]),
                line(130, vec![Stmt::End]),
            ],
        };
        let a = analyze(&m);
        assert_eq!(tier_of(&m, &a, 50), ForTier::Dispatched);
        assert_eq!(
            tier_of(&m, &a, 100),
            ForTier::Framed,
            "the bare NEXT at line 20 is not reachable from line 100"
        );
    }

    /// A crossed body in a program with no dispatcher at all costs nothing.
    #[test]
    fn crossing_without_a_dispatcher_still_costs_nothing() {
        // 10 FOR I=1 TO 3 / 20 GOTO 40 / 30 NEXT I / 40 END
        let m = Module {
            lines: vec![
                line(10, vec![for_stmt("I", 1.0, 3.0)]),
                line(20, vec![Stmt::Goto { target: 40 }]),
                line(30, vec![next_var("I")]),
                line(40, vec![Stmt::End]),
            ],
        };
        let a = analyze(&m);
        assert!(!a.needs_runtime_stack());
        assert!(a.runtime_fors.is_empty());
        assert_eq!(tier_of(&m, &a, 10), ForTier::Static);
    }

    #[test]
    fn on_goto_out_of_a_body_is_a_crossing() {
        let m = Module {
            lines: vec![
                line(5, vec![Stmt::Goto { target: 10 }]),
                line(6, vec![next_stmt(), Stmt::End]),
                line(10, vec![for_stmt("I", 0.0, 10.0)]),
                line(30, vec![Stmt::Goto { target: 80 }]),
                line(40, vec![Stmt::Goto { target: 6 }]),
                line(50, vec![for_stmt("I", 11.0, 15.0)]),
                line(70, vec![Stmt::Goto { target: 40 }]),
                line(
                    80,
                    vec![Stmt::OnBranch {
                        value: Expr::Number(1.0),
                        kind: crate::ast::OnBranchKind::Goto,
                        targets: vec![6],
                    }],
                ),
            ],
        };
        let a = analyze(&m);
        assert_eq!(tier_of(&m, &a, 10), ForTier::Dispatched);
        assert_eq!(
            tier_of(&m, &a, 50),
            ForTier::Dispatched,
            "the second loop must get its own frame and handler, or the \
             shared bare NEXT dispatches on the first loop's frame"
        );
    }

    /// `ON n GOSUB` is a GOSUB: it needs the 0 marker that stops FNDFOR, or a `NEXT` in the
    /// subroutine closes the caller's loop.
    #[test]
    fn on_gosub_pushes_a_marker() {
        // 5 GOTO 30 / 10 NEXT:GOTO 50 / 30 FOR I=1 TO 2 32 ON 1 GOSUB 10 / 50 END
        let m = Module {
            lines: vec![
                line(5, vec![Stmt::Goto { target: 30 }]),
                line(10, vec![next_stmt(), Stmt::Goto { target: 50 }]),
                line(30, vec![for_stmt("I", 1.0, 2.0)]),
                line(
                    32,
                    vec![Stmt::OnBranch {
                        value: Expr::Number(1.0),
                        kind: crate::ast::OnBranchKind::GoSub,
                        targets: vec![10],
                    }],
                ),
                line(50, vec![Stmt::End]),
            ],
        };
        let a = analyze(&m);
        assert!(a.needs_runtime_stack());
        assert!(
            a.runtime_gosubs.iter().any(|s| s.line_no == 32),
            "ON..GOSUB must push the FNDFOR barrier: {:?}",
            a.runtime_gosubs
        );
    }

    // the one-line loop, and the invariant behind it
    #[test]
    fn one_line_loop_gosubbed_from_a_body_stays_tier0() {
        // 10 FOR K=1 TO 3 / 20 GOSUB 100 / 30 NEXT K / 40 END 100 FOR Z=1 TO 5:NEXT / 110
        // RETURN
        let m = Module {
            lines: vec![
                line(10, vec![for_stmt("K", 1.0, 3.0)]),
                line(20, vec![Stmt::GoSub { target: 100 }]),
                line(30, vec![next_var("K")]),
                line(40, vec![Stmt::End]),
                line(100, vec![for_stmt("Z", 1.0, 5.0), next_stmt()]),
                line(110, vec![Stmt::Return]),
            ],
        };
        let a = analyze(&m);
        assert!(
            !a.needs_runtime_stack(),
            "a one-line loop the branch's own entry re-opens is not orphaned: {:?}",
            a.orphan_nexts
        );
        assert_eq!(tier_of(&m, &a, 10), ForTier::Static);
        assert_eq!(tier_of(&m, &a, 100), ForTier::Static);
    }

    /// The exemption is about the *header*, not about being on the branch target line.
    #[test]
    fn next_without_a_header_on_its_line_stays_orphan() {
        // 10 FOR K=1 TO 3 / 20 GOSUB 100 / 30 NEXT K / 40 END 100 NEXT / 110 RETURN
        let m = Module {
            lines: vec![
                line(10, vec![for_stmt("K", 1.0, 3.0)]),
                line(20, vec![Stmt::GoSub { target: 100 }]),
                line(30, vec![next_var("K")]),
                line(40, vec![Stmt::End]),
                line(100, vec![next_stmt()]),
                line(110, vec![Stmt::Return]),
            ],
        };
        let a = analyze(&m);
        assert!(
            a.orphan_nexts.iter().any(|n| n.line_no == 100),
            "a headerless NEXT on the target line must still dispatch"
        );
    }

    /// Order on the line decides it.
    #[test]
    fn header_after_the_next_on_its_line_does_not_exempt_it() {
        let m = Module {
            lines: vec![
                line(10, vec![for_stmt("K", 1.0, 3.0)]),
                line(20, vec![Stmt::GoSub { target: 100 }]),
                line(30, vec![next_var("K")]),
                line(40, vec![Stmt::End]),
                line(100, vec![next_stmt(), for_stmt("Z", 1.0, 5.0)]),
                line(110, vec![Stmt::Return]),
            ],
        };
        let a = analyze(&m);
        assert!(
            a.orphan_nexts.iter().any(|n| n.line_no == 100),
            "the header runs after this NEXT, so entry does not rebuild its frame"
        );
    }

    #[test]
    fn every_dispatching_next_has_a_framed_partner() {
        let modules: Vec<(&str, Module)> = vec![
            (
                "one-line loop with a headerless twin",
                // 10 FOR K / 20 GOSUB 100 / 30 NEXT K / 40 END 100 FOR Z:NEXT / 105 NEXT / 110
                // RETURN  Line 105's NEXT has no header of its own and is orphaned; line 100's
                // is exempt.
                Module {
                    lines: vec![
                        line(10, vec![for_stmt("K", 1.0, 3.0)]),
                        line(20, vec![Stmt::GoSub { target: 100 }]),
                        line(30, vec![next_var("K")]),
                        line(40, vec![Stmt::End]),
                        line(100, vec![for_stmt("Z", 1.0, 5.0), next_stmt()]),
                        line(105, vec![next_stmt()]),
                        line(110, vec![Stmt::Return]),
                    ],
                },
            ),
            (
                "GOTO onto a foreign one-line loop",
                Module {
                    lines: vec![
                        line(10, vec![for_stmt("K", 1.0, 3.0)]),
                        line(20, vec![Stmt::Goto { target: 100 }]),
                        line(30, vec![next_var("K")]),
                        line(40, vec![Stmt::End]),
                        line(100, vec![for_stmt("Z", 1.0, 5.0), next_stmt()]),
                        line(110, vec![Stmt::Goto { target: 30 }]),
                    ],
                },
            ),
            (
                "the forty shape",
                Module {
                    lines: vec![
                        line(5, vec![Stmt::Goto { target: 10 }]),
                        line(6, vec![next_stmt(), Stmt::End]),
                        line(10, vec![for_stmt("I", 0.0, 10.0)]),
                        line(30, vec![Stmt::Goto { target: 80 }]),
                        line(40, vec![Stmt::Goto { target: 6 }]),
                        line(50, vec![for_stmt("I", 11.0, 15.0)]),
                        line(70, vec![Stmt::Goto { target: 40 }]),
                        line(
                            80,
                            vec![Stmt::OnBranch {
                                value: Expr::Number(1.0),
                                kind: crate::ast::OnBranchKind::Goto,
                                targets: vec![6],
                            }],
                        ),
                    ],
                },
            ),
        ];
        for (name, m) in &modules {
            let a = analyze(m);
            for next_id in &a.orphan_nexts {
                let Some(slots) = a.static_pairs.get(next_id) else {
                    continue;
                };
                for for_id in slots.iter().flatten() {
                    assert!(
                        a.runtime_fors.contains_key(for_id),
                        "{name}: the NEXT on line {} dispatches, but the FOR it \
                         pairs with on line {} pushes no frame",
                        next_id.line_no,
                        for_id.line_no
                    );
                }
            }
        }
    }
}
