#![allow(dead_code)]
// first consumer of the CFG (live-vars) lands in
// the next phase; until then the whole module is
// dormant infrastructure.

//! Control-flow graph for the IR.
//!
//! Each statement (top-level on a line, or nested inside an IF THEN
//! body) becomes a node. Edges follow control flow:
//!
//!   * Sequential statements: fall-through to the next stmt on the
//!     line, or to the first stmt of the next line.
//!   * GOTO target: edge to the first stmt of the target line.
//!   * GOSUB target: edge to the target's first stmt; the
//!     after-GOSUB stmt is recorded as a "return target" so every
//!     RETURN can edge back to it.
//!   * IF body = ThenIr::Goto: branch to target + fall-through.
//!   * IF body = ThenIr::Stmts: branch to first inner stmt + fall-
//!     through. Last inner stmt re-joins the fall-through path.
//!   * ON GOTO/GOSUB: edges to every listed target plus fall-through
//!     (out-of-range selector falls through per BASIC v2).
//!   * FOR / NEXT: NEXT has a back-edge to the FOR's first body stmt
//!     plus a forward fall-through (loop exits).
//!   * END / STOP: no successors.
//!
//! GOSUB/RETURN modelling is necessarily over-approximate without
//! interprocedural analysis: every RETURN edges back to every
//! after-GOSUB site we recorded. Sound for the analyses we plan to
//! run, just less precise than ideal.

use std::collections::HashMap;

use crate::ast::VarName;
use crate::ir::{Module, Stmt, ThenIr};

/// Path identifying one Stmt in the IR. `line_idx` indexes
/// `module.lines`. `path` walks down through nested IF THEN bodies:
/// `path[0]` is the top-level stmt index on that line, `path[1]` is
/// the index inside that stmt's THEN body, and so on.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StmtPath {
    pub line_idx: usize,
    pub path: Vec<usize>,
}

/// A single node in the CFG. `successors` and `predecessors` are
/// indices into `Cfg::nodes`. Predecessors are computed by inversion
/// after all successors are placed.
#[derive(Debug, Clone)]
pub struct CfgNode {
    pub stmt: StmtPath,
    pub line_no: u16,
    pub successors: Vec<usize>,
    pub predecessors: Vec<usize>,
}

#[derive(Debug, Clone)]
pub struct Cfg {
    pub nodes: Vec<CfgNode>,
    /// First node — typically node 0 (first stmt of first line).
    pub entry: usize,
    /// BASIC line number → id of that line's first stmt node.
    pub line_first: HashMap<u16, usize>,
    /// Stmt-paths that follow each GOSUB at runtime; every RETURN
    /// edges back to these. Over-approximation.
    pub return_targets: Vec<usize>,
}

impl Cfg {
    pub fn build(module: &Module) -> Self {
        let mut builder = Builder::new(module);
        builder.build();
        builder.finish()
    }

    /// Resolve a node's `StmtPath` back to the actual `Stmt`.
    pub fn stmt_at<'a>(&self, id: usize, module: &'a Module) -> &'a Stmt {
        let p = &self.nodes[id].stmt;
        let mut s = &module.lines[p.line_idx].stmts[p.path[0]];
        for &idx in &p.path[1..] {
            match s {
                Stmt::If {
                    then: ThenIr::Stmts(inner),
                    ..
                } => s = &inner[idx],
                Stmt::IfElse {
                    then, else_then, ..
                } => {
                    let then_len = if let ThenIr::Stmts(inner) = then {
                        if idx < inner.len() {
                            s = &inner[idx];
                            continue;
                        }
                        inner.len()
                    } else {
                        0
                    };
                    if let ThenIr::Stmts(inner) = else_then {
                        s = &inner[idx - then_len];
                    } else {
                        unreachable!("invalid CFG IF/ELSE path")
                    }
                }
                Stmt::Rcomp { then, else_then } => {
                    let then_len = if let ThenIr::Stmts(inner) = then {
                        if idx < inner.len() {
                            s = &inner[idx];
                            continue;
                        }
                        inner.len()
                    } else {
                        0
                    };
                    if let Some(ThenIr::Stmts(inner)) = else_then {
                        s = &inner[idx - then_len];
                    } else {
                        unreachable!("invalid CFG RCOMP path")
                    }
                }
                _ => panic!("CFG path {:?} traversed into a non-IF/Stmts statement", p),
            }
        }
        s
    }
}

// ----- Builder state --------------------------------------------------------

struct Builder<'a> {
    module: &'a Module,
    nodes: Vec<CfgNode>,
    /// Reverse index: StmtPath → CfgNode id. Lets edges resolve back-
    /// references after both endpoints exist.
    by_path: HashMap<StmtPathKey, usize>,
    line_first: HashMap<u16, usize>,
    return_targets: Vec<usize>,
    /// FOR/NEXT pairing stack. Each entry holds the FOR node id and
    /// its loop variable so a NEXT V can pop until matching.
    for_stack: Vec<ForFrame>,
    structured_loop_stack: Vec<LoopFrame>,
    do_stack: Vec<DoFrame>,
    /// Pre-computed succession plan for every line: line N's last stmt
    /// falls through to line[N+1]'s first stmt (or nowhere if N is
    /// the last line). Built up front so individual stmt visitors can
    /// look up where to fall through to.
    line_after: Vec<Option<usize>>,
}

#[derive(Clone, Copy)]
struct ForFrame {
    /// First statement that runs as the loop body — i.e. the FOR's
    /// fallthrough successor. The matching NEXT edges back HERE, not
    /// to the FOR itself, because BASIC v2 doesn't re-initialise the
    /// counter on every iteration; it only re-runs the body.
    body_start: Option<usize>,
    /// Var the FOR opens with — NEXT V matches when this equals V.
    var: u16,
    var_kind_marker: u8,
}

struct LoopFrame {
    body_start: Option<usize>,
    exit_nodes: Vec<usize>,
}

struct DoFrame {
    false_nodes: Vec<usize>,
    done_jump_nodes: Vec<usize>,
    seen_else: bool,
}

/// Hashable wrapper so we can key the `by_path` map.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct StmtPathKey {
    line_idx: usize,
    path: Vec<usize>,
}

impl<'a> Builder<'a> {
    fn new(module: &'a Module) -> Self {
        Self {
            module,
            nodes: Vec::new(),
            by_path: HashMap::new(),
            line_first: HashMap::new(),
            return_targets: Vec::new(),
            for_stack: Vec::new(),
            structured_loop_stack: Vec::new(),
            do_stack: Vec::new(),
            line_after: Vec::new(),
        }
    }

    fn build(&mut self) {
        if self.module.lines.is_empty() {
            return;
        }
        // Pass 1: allocate a node per stmt, record line→first-node.
        for (line_idx, line) in self.module.lines.iter().enumerate() {
            for (top_idx, stmt) in line.stmts.iter().enumerate() {
                self.allocate(line_idx, &mut Vec::from([top_idx]), stmt, line.number);
            }
            // line.stmts may be empty (post-DeadLineElim with kept GOTO
            // targets). Such lines have no first node — record nothing.
        }
        // Walk lines a second time to fill line_after: each line's
        // "fall-through to next line" target. For an empty line this
        // is the same as the next non-empty line.
        self.compute_line_after();
        // Pass 2: lay edges.
        // We re-traverse each line's top-level stmts, threading the
        // "fallthrough" target down through IF nesting.
        for (line_idx, line) in self.module.lines.iter().enumerate() {
            self.connect_top_level(line_idx, &line.stmts);
        }
        // Pass 3: every RETURN node edges to every recorded return
        // target — over-approximation but sound.
        let return_targets = self.return_targets.clone();
        for node_id in 0..self.nodes.len() {
            let path = self.nodes[node_id].stmt.clone();
            let stmt = self.resolve_path(&path);
            if matches!(stmt, Stmt::Return) {
                for &t in &return_targets {
                    self.add_edge(node_id, t);
                }
            }
        }
        // Pass 4: build predecessor lists from successors.
        let mut preds: Vec<Vec<usize>> = vec![Vec::new(); self.nodes.len()];
        for (id, node) in self.nodes.iter().enumerate() {
            for &succ in &node.successors {
                preds[succ].push(id);
            }
        }
        for (id, p) in preds.into_iter().enumerate() {
            self.nodes[id].predecessors = p;
        }
    }

    fn finish(self) -> Cfg {
        Cfg {
            nodes: self.nodes,
            entry: 0,
            line_first: self.line_first,
            return_targets: self.return_targets,
        }
    }

    /// Walk the IR to assign a `CfgNode` per stmt. Recurses into IF
    /// bodies so nested stmts get their own ids.
    fn allocate(&mut self, line_idx: usize, path: &mut Vec<usize>, stmt: &Stmt, line_no: u16) {
        let id = self.nodes.len();
        let key = StmtPathKey {
            line_idx,
            path: path.clone(),
        };
        self.nodes.push(CfgNode {
            stmt: StmtPath {
                line_idx,
                path: path.clone(),
            },
            line_no,
            successors: Vec::new(),
            predecessors: Vec::new(),
        });
        self.by_path.insert(key, id);
        // First top-level stmt on a line is that line's entry node.
        if path.len() == 1 && path[0] == 0 {
            self.line_first.entry(line_no).or_insert(id);
        }
        self.allocate_branch_children(line_idx, path, stmt, line_no);
    }

    fn allocate_branch_children(
        &mut self,
        line_idx: usize,
        path: &mut Vec<usize>,
        stmt: &Stmt,
        line_no: u16,
    ) {
        match stmt {
            Stmt::If {
                then: ThenIr::Stmts(inner),
                ..
            } => {
                for (i, inner_stmt) in inner.iter().enumerate() {
                    path.push(i);
                    self.allocate(line_idx, path, inner_stmt, line_no);
                    path.pop();
                }
            }
            Stmt::IfElse {
                then, else_then, ..
            } => {
                let then_len = if let ThenIr::Stmts(inner) = then {
                    for (i, inner_stmt) in inner.iter().enumerate() {
                        path.push(i);
                        self.allocate(line_idx, path, inner_stmt, line_no);
                        path.pop();
                    }
                    inner.len()
                } else {
                    0
                };
                if let ThenIr::Stmts(inner) = else_then {
                    for (i, inner_stmt) in inner.iter().enumerate() {
                        path.push(then_len + i);
                        self.allocate(line_idx, path, inner_stmt, line_no);
                        path.pop();
                    }
                }
            }
            Stmt::Rcomp { then, else_then } => {
                let then_len = if let ThenIr::Stmts(inner) = then {
                    for (i, inner_stmt) in inner.iter().enumerate() {
                        path.push(i);
                        self.allocate(line_idx, path, inner_stmt, line_no);
                        path.pop();
                    }
                    inner.len()
                } else {
                    0
                };
                if let Some(ThenIr::Stmts(inner)) = else_then {
                    for (i, inner_stmt) in inner.iter().enumerate() {
                        path.push(then_len + i);
                        self.allocate(line_idx, path, inner_stmt, line_no);
                        path.pop();
                    }
                }
            }
            _ => {}
        }
    }

    fn compute_line_after(&mut self) {
        // For each line index, the "after this line" target is the
        // first node of the next non-empty line.
        let n = self.module.lines.len();
        self.line_after = vec![None; n];
        let mut next_first: Option<usize> = None;
        for line_idx in (0..n).rev() {
            self.line_after[line_idx] = next_first;
            let line_no = self.module.lines[line_idx].number;
            if let Some(&id) = self.line_first.get(&line_no) {
                next_first = Some(id);
            }
        }
    }

    fn connect_top_level(&mut self, line_idx: usize, stmts: &[Stmt]) {
        for (i, stmt) in stmts.iter().enumerate() {
            let node_id = self.lookup_top_level(line_idx, i);
            // Fall-through is the next top-level stmt on the same
            // line, else the line_after target.
            let fallthrough = if i + 1 < stmts.len() {
                Some(self.lookup_top_level(line_idx, i + 1))
            } else {
                self.line_after[line_idx]
            };
            self.connect_stmt(node_id, line_idx, &[i], stmt, fallthrough);
        }
    }

    fn lookup_top_level(&self, line_idx: usize, top_idx: usize) -> usize {
        let key = StmtPathKey {
            line_idx,
            path: vec![top_idx],
        };
        *self.by_path.get(&key).expect("top-level node missing")
    }

    /// Add control-flow edges out of `node_id` based on `stmt`'s
    /// shape. `fallthrough` is the natural "next" node when control
    /// drops past the end of this stmt.
    fn connect_stmt(
        &mut self,
        node_id: usize,
        line_idx: usize,
        path_prefix: &[usize],
        stmt: &Stmt,
        fallthrough: Option<usize>,
    ) {
        match stmt {
            Stmt::Goto { target } => {
                if let Some(&t) = self.line_first.get(target) {
                    self.add_edge(node_id, t);
                }
                // GOTO into a non-existent line is a runtime error;
                // codegen still emits the JMP, but for CFG we just
                // leave a dead-end (matches "control transfers, then
                // crashes — never falls through").
            }
            Stmt::GoSub { target } => {
                if let Some(&t) = self.line_first.get(target) {
                    self.add_edge(node_id, t);
                }
                // The site control returns to AFTER the GOSUB is
                // whatever fallthrough was — record it so RETURNs
                // edge back here.
                if let Some(after) = fallthrough {
                    if !self.return_targets.contains(&after) {
                        self.return_targets.push(after);
                    }
                }
            }
            Stmt::Return | Stmt::End | Stmt::Stop => {
                // No fall-through. RETURN's edges to return-targets
                // are added in a separate pass after construction.
            }
            Stmt::Run(target) => {
                // RUN restarts execution. With an explicit target, jump
                // there; otherwise hop to the program's first line.
                let dest = match target {
                    Some(t) => self.line_first.get(t).copied(),
                    None => self
                        .module
                        .lines
                        .first()
                        .and_then(|l| self.line_first.get(&l.number).copied()),
                };
                if let Some(d) = dest {
                    self.add_edge(node_id, d);
                }
            }
            Stmt::OnBranch { kind, targets, .. } => {
                for t in targets {
                    if let Some(&n) = self.line_first.get(t) {
                        self.add_edge(node_id, n);
                    }
                }
                // Out-of-range selector falls through; for ON-GOSUB
                // the same applies plus return targets.
                if let Some(a) = fallthrough {
                    self.add_edge(node_id, a);
                    if matches!(kind, crate::ast::OnBranchKind::GoSub) {
                        if !self.return_targets.contains(&a) {
                            self.return_targets.push(a);
                        }
                    }
                }
            }
            Stmt::If { then, .. } => {
                self.connect_then_branch(node_id, line_idx, path_prefix, then, 0, fallthrough);
                // The IF can also be skipped (cond false) → fall-
                // through directly past the IF.
                if let Some(a) = fallthrough {
                    self.add_edge(node_id, a);
                }
            }
            Stmt::IfElse {
                then, else_then, ..
            } => {
                self.connect_then_branch(node_id, line_idx, path_prefix, then, 0, fallthrough);
                let offset = match then {
                    ThenIr::Stmts(inner) => inner.len(),
                    ThenIr::Goto(_) => 0,
                };
                self.connect_then_branch(
                    node_id,
                    line_idx,
                    path_prefix,
                    else_then,
                    offset,
                    fallthrough,
                );
            }
            Stmt::Rcomp { then, else_then } => {
                self.connect_then_branch(node_id, line_idx, path_prefix, then, 0, fallthrough);
                if let Some(else_then) = else_then {
                    let offset = match then {
                        ThenIr::Stmts(inner) => inner.len(),
                        ThenIr::Goto(_) => 0,
                    };
                    self.connect_then_branch(
                        node_id,
                        line_idx,
                        path_prefix,
                        else_then,
                        offset,
                        fallthrough,
                    );
                } else if let Some(a) = fallthrough {
                    self.add_edge(node_id, a);
                }
            }
            Stmt::For { var, .. } => {
                // FOR falls through to its body (whatever comes next
                // in source order). The matching NEXT will edge back
                // to that body start, NOT to the FOR itself — BASIC
                // doesn't re-run the start expression on each
                // iteration.
                if let Some(a) = fallthrough {
                    self.add_edge(node_id, a);
                }
                self.for_stack.push(ForFrame {
                    body_start: fallthrough,
                    var: var_hash(var),
                    var_kind_marker: var_kind_marker(var),
                });
            }
            Stmt::Next { vars } => {
                let target_var = vars.iter().find_map(|v| v.as_ref());
                let frame = match target_var {
                    None => self.for_stack.pop(),
                    Some(target) => {
                        let key = (var_hash(target), var_kind_marker(target));
                        let mut popped = None;
                        while let Some(top) = self.for_stack.last() {
                            if (top.var, top.var_kind_marker) == key {
                                popped = self.for_stack.pop();
                                break;
                            }
                            self.for_stack.pop();
                        }
                        popped
                    }
                };
                if let Some(f) = frame {
                    if let Some(body) = f.body_start {
                        // Back-edge to the first body stmt — re-runs
                        // the body from the top with the incremented
                        // counter.
                        self.add_edge(node_id, body);
                    }
                }
                // Forward fall-through when the loop exits.
                if let Some(a) = fallthrough {
                    self.add_edge(node_id, a);
                }
            }
            Stmt::Repeat | Stmt::Loop => {
                if let Some(a) = fallthrough {
                    self.add_edge(node_id, a);
                }
                self.structured_loop_stack.push(LoopFrame {
                    body_start: fallthrough,
                    exit_nodes: Vec::new(),
                });
            }
            Stmt::Until { .. } => {
                if let Some(mut frame) = self.structured_loop_stack.pop() {
                    if let Some(body) = frame.body_start {
                        self.add_edge(node_id, body);
                    }
                    if let Some(a) = fallthrough {
                        self.add_edge(node_id, a);
                    }
                    for exit in frame.exit_nodes.drain(..) {
                        if let Some(a) = fallthrough {
                            self.add_edge(exit, a);
                        }
                    }
                }
            }
            Stmt::EndLoop => {
                if let Some(mut frame) = self.structured_loop_stack.pop() {
                    if let Some(body) = frame.body_start {
                        self.add_edge(node_id, body);
                    }
                    for exit in frame.exit_nodes.drain(..) {
                        if let Some(a) = fallthrough {
                            self.add_edge(exit, a);
                        }
                    }
                }
            }
            Stmt::ExitLoop { cond } => {
                if cond.is_some() {
                    if let Some(a) = fallthrough {
                        self.add_edge(node_id, a);
                    }
                }
                if let Some(frame) = self.structured_loop_stack.last_mut() {
                    frame.exit_nodes.push(node_id);
                }
            }
            Stmt::DoIf { .. } => {
                if let Some(a) = fallthrough {
                    self.add_edge(node_id, a);
                }
                self.do_stack.push(DoFrame {
                    false_nodes: vec![node_id],
                    done_jump_nodes: Vec::new(),
                    seen_else: false,
                });
            }
            Stmt::Else => {
                if let Some(frame) = self.do_stack.last_mut() {
                    frame.seen_else = true;
                    frame.done_jump_nodes.push(node_id);
                    let false_nodes = frame.false_nodes.clone();
                    if let Some(a) = fallthrough {
                        for f in false_nodes {
                            self.add_edge(f, a);
                        }
                    }
                }
            }
            Stmt::Done => {
                if let Some(frame) = self.do_stack.pop() {
                    if let Some(a) = fallthrough {
                        if !frame.seen_else {
                            for f in frame.false_nodes {
                                self.add_edge(f, a);
                            }
                        }
                        for j in frame.done_jump_nodes {
                            self.add_edge(j, a);
                        }
                        self.add_edge(node_id, a);
                    }
                } else if let Some(a) = fallthrough {
                    self.add_edge(node_id, a);
                }
            }
            Stmt::ComputedGoto { .. } => {
                for target in self.line_first.values().copied().collect::<Vec<_>>() {
                    self.add_edge(node_id, target);
                }
            }
            // RESUME jumps to a runtime-determined line — the same
            // line that errored, the line after it, or a literal
            // RESUME <line>. Conservatively add edges to every
            // line so live-vars sees variables set in the handler
            // as live for downstream lines.
            Stmt::Resume { .. } => {
                for target in self.line_first.values().copied().collect::<Vec<_>>() {
                    self.add_edge(node_id, target);
                }
            }
            // ERROR n jumps to the ON ERROR handler — likewise a
            // runtime-determined edge to one of the line targets.
            Stmt::ErrorRaise { .. } => {
                for target in self.line_first.values().copied().collect::<Vec<_>>() {
                    self.add_edge(node_id, target);
                }
            }
            // Default: pure sequential.
            _ => {
                if let Some(a) = fallthrough {
                    self.add_edge(node_id, a);
                }
            }
        }
    }

    fn connect_then_branch(
        &mut self,
        node_id: usize,
        line_idx: usize,
        path_prefix: &[usize],
        branch: &ThenIr,
        offset: usize,
        fallthrough: Option<usize>,
    ) {
        match branch {
            ThenIr::Goto(target) => {
                if let Some(&t) = self.line_first.get(target) {
                    self.add_edge(node_id, t);
                }
            }
            ThenIr::Stmts(inner) => {
                if inner.is_empty() {
                    if let Some(a) = fallthrough {
                        self.add_edge(node_id, a);
                    }
                    return;
                }
                let mut p = path_prefix.to_vec();
                p.push(offset);
                let first_inner = *self
                    .by_path
                    .get(&StmtPathKey { line_idx, path: p })
                    .expect("inner branch node missing");
                self.add_edge(node_id, first_inner);
                for (j, inner_stmt) in inner.iter().enumerate() {
                    let mut p = path_prefix.to_vec();
                    p.push(offset + j);
                    let inner_id = *self
                        .by_path
                        .get(&StmtPathKey {
                            line_idx,
                            path: p.clone(),
                        })
                        .expect("inner node missing");
                    let inner_after = if j + 1 < inner.len() {
                        let mut q = path_prefix.to_vec();
                        q.push(offset + j + 1);
                        Some(
                            *self
                                .by_path
                                .get(&StmtPathKey { line_idx, path: q })
                                .expect("inner-next node missing"),
                        )
                    } else {
                        fallthrough
                    };
                    self.connect_stmt(inner_id, line_idx, &p, inner_stmt, inner_after);
                }
            }
        }
    }

    fn add_edge(&mut self, from: usize, to: usize) {
        if !self.nodes[from].successors.contains(&to) {
            self.nodes[from].successors.push(to);
        }
    }

    fn resolve_path(&self, p: &StmtPath) -> &Stmt {
        let mut s = &self.module.lines[p.line_idx].stmts[p.path[0]];
        for &idx in &p.path[1..] {
            match s {
                Stmt::If {
                    then: ThenIr::Stmts(inner),
                    ..
                } => s = &inner[idx],
                Stmt::IfElse {
                    then, else_then, ..
                } => {
                    let then_len = if let ThenIr::Stmts(inner) = then {
                        if idx < inner.len() {
                            s = &inner[idx];
                            continue;
                        }
                        inner.len()
                    } else {
                        0
                    };
                    if let ThenIr::Stmts(inner) = else_then {
                        s = &inner[idx - then_len];
                    } else {
                        unreachable!("invalid CFG IF/ELSE path")
                    }
                }
                Stmt::Rcomp { then, else_then } => {
                    let then_len = if let ThenIr::Stmts(inner) = then {
                        if idx < inner.len() {
                            s = &inner[idx];
                            continue;
                        }
                        inner.len()
                    } else {
                        0
                    };
                    if let Some(ThenIr::Stmts(inner)) = else_then {
                        s = &inner[idx - then_len];
                    } else {
                        unreachable!("invalid CFG RCOMP path")
                    }
                }
                _ => unreachable!("invalid CFG stmt path"),
            }
        }
        s
    }
}

/// Hash a VarName's base into a u16 — collisions are tolerated by the
/// FOR/NEXT matcher (it falls back to bare-NEXT semantics when no
/// match found).
fn var_hash(v: &VarName) -> u16 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.base.hash(&mut h);
    h.finish() as u16
}

fn var_kind_marker(v: &VarName) -> u8 {
    match v.kind {
        crate::ast::VarKind::Float => 0,
        crate::ast::VarKind::Integer => 1,
        crate::ast::VarKind::String => 2,
    }
}

// ----- Analysis registration -----------------------------------------------

/// `Analysis` wrapper so passes can pull a cached CFG from the registry.
pub struct CfgBuild;

impl crate::analysis::Analysis for CfgBuild {
    type Output = Cfg;
    fn name(&self) -> &'static str {
        "cfg"
    }
    fn run(&self, m: &Module, _deps: &mut crate::analysis::Registry) -> Self::Output {
        Cfg::build(m)
    }
}

// ----- Tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Line, Module, Stmt, ThenIr};

    fn make_line(number: u16, stmts: Vec<Stmt>) -> Line {
        Line { number, stmts }
    }

    #[test]
    fn simple_sequence_falls_through() {
        // 10 END
        // 20 END
        let m = Module {
            lines: vec![
                make_line(10, vec![Stmt::End]),
                make_line(20, vec![Stmt::End]),
            ],
        };
        let cfg = Cfg::build(&m);
        assert_eq!(cfg.nodes.len(), 2);
        // END has no successors.
        assert!(cfg.nodes[0].successors.is_empty());
        assert!(cfg.nodes[1].successors.is_empty());
    }

    #[test]
    fn goto_jumps_to_target() {
        // 10 GOTO 30
        // 20 END
        // 30 END
        let m = Module {
            lines: vec![
                make_line(10, vec![Stmt::Goto { target: 30 }]),
                make_line(20, vec![Stmt::End]),
                make_line(30, vec![Stmt::End]),
            ],
        };
        let cfg = Cfg::build(&m);
        assert_eq!(cfg.nodes.len(), 3);
        // node 0 = GOTO 30 → node 2 (line 30)
        assert_eq!(cfg.nodes[0].successors, vec![2]);
        // node 1 = END at line 20 → no successors
        assert!(cfg.nodes[1].successors.is_empty());
        // line 30 is a successor only of node 0
        assert_eq!(cfg.nodes[2].predecessors, vec![0]);
    }

    #[test]
    fn if_branches_to_then_and_fallthrough() {
        // 10 IF 1 THEN GOTO 30
        // 20 END
        // 30 END
        let m = Module {
            lines: vec![
                make_line(
                    10,
                    vec![Stmt::If {
                        cond: crate::ir::Expr::Number(1.0),
                        then: ThenIr::Goto(30),
                    }],
                ),
                make_line(20, vec![Stmt::End]),
                make_line(30, vec![Stmt::End]),
            ],
        };
        let cfg = Cfg::build(&m);
        // IF node has two successors: target (line 30) + fallthrough
        // (line 20).
        assert_eq!(cfg.nodes[0].successors.len(), 2);
        assert!(cfg.nodes[0].successors.contains(&1)); // line 20
        assert!(cfg.nodes[0].successors.contains(&2)); // line 30
    }

    #[test]
    fn gosub_records_return_target() {
        // 10 GOSUB 30
        // 20 END
        // 30 RETURN
        let m = Module {
            lines: vec![
                make_line(10, vec![Stmt::GoSub { target: 30 }]),
                make_line(20, vec![Stmt::End]),
                make_line(30, vec![Stmt::Return]),
            ],
        };
        let cfg = Cfg::build(&m);
        // GOSUB → 30 (the subroutine entry).
        assert!(cfg.nodes[0].successors.contains(&2));
        // The line-20 node is a return target so RETURN edges back here.
        assert!(cfg.nodes[2].successors.contains(&1));
        // After-GOSUB site (line 20) recorded.
        assert!(cfg.return_targets.contains(&1));
    }

    #[test]
    fn for_next_back_edge() {
        let var = crate::ast::VarName {
            base: "I".to_string(),
            kind: crate::ast::VarKind::Float,
        };
        // 10 FOR I=1 TO 10
        // 20 X = I
        // 30 NEXT I
        // 40 END
        let m = Module {
            lines: vec![
                make_line(
                    10,
                    vec![Stmt::For {
                        var: var.clone(),
                        start: crate::ir::Expr::Number(1.0),
                        end: crate::ir::Expr::Number(10.0),
                        step: crate::ir::Expr::Number(1.0),
                        body_int_safe: false,
                        body_reads_loop_var: false,
                        induction_const: None,
                        array_inductions: Vec::new(),
                    }],
                ),
                make_line(
                    20,
                    vec![Stmt::Let {
                        var: crate::ast::VarName {
                            base: "X".to_string(),
                            kind: crate::ast::VarKind::Float,
                        },
                        value: crate::ir::Expr::Var(var.clone()),
                    }],
                ),
                make_line(
                    30,
                    vec![Stmt::Next {
                        vars: vec![Some(var.clone())],
                    }],
                ),
                make_line(40, vec![Stmt::End]),
            ],
        };
        let cfg = Cfg::build(&m);
        // NEXT (node 2) edges back to the FOR body's first stmt
        // (node 1, the LET inside the loop) AND falls through to END
        // (node 3) on loop exit. NOTE: the back-edge does NOT go to
        // the FOR itself — that would re-run the start initialiser.
        let next_succ = &cfg.nodes[2].successors;
        assert!(next_succ.contains(&1), "back-edge to body start");
        assert!(next_succ.contains(&3), "forward exit to END");
    }
}
