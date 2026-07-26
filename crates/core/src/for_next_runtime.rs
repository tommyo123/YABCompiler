//! Detect orphan `NEXT` statements and the FOR/GOSUB/RETURN sites
//! that need a runtime FOR-stack. Codegen consults the result and
//! falls back to the inline path when this analysis comes up empty.

use std::collections::{HashMap, HashSet};

use crate::ast::VarName;
use crate::ir::{Module, Stmt, ThenIr};

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
    /// Runtime-mode `FOR`s, each tagged with a 1-byte dispatch id.
    pub runtime_fors: HashMap<StmtId, u8>,
    /// `GOSUB`s that wrap a runtime `FOR`.
    pub runtime_gosubs: HashSet<StmtId>,
    /// `RETURN`s reachable from a runtime `GOSUB`.
    pub runtime_returns: HashSet<StmtId>,
}

impl OrphanAnalysis {
    pub fn needs_runtime_stack(&self) -> bool {
        !self.orphan_nexts.is_empty()
    }

    pub fn runtime_for_id(&self, line_no: u16, line_idx: usize, path: &[usize]) -> Option<u8> {
        let key = StmtId {
            line_no,
            line_idx,
            path: path.to_vec(),
        };
        self.runtime_fors.get(&key).copied()
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
}

/// Run the analysis over an entire `Module`.
pub fn analyze(module: &Module) -> OrphanAnalysis {
    let mut analysis = OrphanAnalysis::default();
    let static_pairs = static_pair_walk(module, &mut analysis);

    if analysis.orphan_nexts.is_empty() {
        // Common path — no orphans, nothing else to compute.
        return analysis;
    }

    let line_index = build_line_index(module);
    let all_gosubs = collect_all(module, |s| matches!(s, Stmt::GoSub { .. }));
    let all_returns = collect_all(module, |s| matches!(s, Stmt::Return));
    let goto_targets_in_for_body = collect_goto_targets_by_enclosing_for(module);

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
    let mut next_id: u8 = 1;
    for (for_id, body_lines) in sorted {
        // A FOR reaches an orphan NEXT when (a) something in its body
        // GOTOs the orphan's line, or (b) the orphan sits on a line
        // the FOR's body covers — body lines collected by the nested-
        // aware walk so a FOR with a nested static NEXT can still
        // have its body extend past it.
        let reaches_orphan = body_lines
            .goto_targets
            .iter()
            .any(|t| orphan_lines.contains(t))
            || body_lines
                .lines_in_body
                .iter()
                .any(|l| orphan_lines.contains(l));
        if reaches_orphan {
            analysis.runtime_fors.insert(for_id.clone(), next_id);
            next_id = next_id
                .checked_add(1)
                .expect("more than 255 runtime FORs in one program");
        }
    }

    // GOSUBs that can reach a runtime FOR.
    let runtime_for_lines: HashSet<u16> =
        analysis.runtime_fors.keys().map(|s| s.line_no).collect();
    for g in &all_gosubs {
        if let Some(Stmt::GoSub { target }) = stmt_at(module, g) {
            let in_scope = lines_reachable_from(module, *target, &line_index);
            if in_scope.iter().any(|l| runtime_for_lines.contains(l)) {
                analysis.runtime_gosubs.insert(g.clone());
            }
        }
    }

    // Conservative: every RETURN participates when any runtime GOSUB
    // exists. Push and walk have to stay symmetric — a RETURN walks
    // the stack down to the first 0 marker, so every GOSUB must leave
    // one, not just the ones whose subroutine holds a runtime FOR.
    // Otherwise `GOSUB <sub with no FOR>` inside a runtime FOR body
    // returns through a walk that eats the live FOR frames instead.
    let _ = (&static_pairs,);
    if !analysis.runtime_gosubs.is_empty() {
        for g in &all_gosubs {
            analysis.runtime_gosubs.insert(g.clone());
        }
        for r in &all_returns {
            analysis.runtime_returns.insert(r.clone());
        }
    }

    analysis
}

// --- helpers ---------------------------------------------------------------

/// Walk in source order with a simulated FOR-stack; unmatched NEXTs
/// land in `analysis.orphan_nexts`.
fn static_pair_walk(module: &Module, analysis: &mut OrphanAnalysis) -> Vec<(StmtId, StmtId)> {
    let mut pairs = Vec::new();
    let mut for_stack: Vec<(StmtId, VarName)> = Vec::new();
    for (line_idx, line) in module.lines.iter().enumerate() {
        for (top_idx, stmt) in line.stmts.iter().enumerate() {
            let here = StmtId {
                line_no: line.number,
                line_idx,
                path: vec![top_idx],
            };
            walk_stmt_for_pairing(stmt, &here, &mut for_stack, &mut pairs, analysis);
        }
    }
    pairs
}

fn walk_stmt_for_pairing(
    stmt: &Stmt,
    here: &StmtId,
    for_stack: &mut Vec<(StmtId, VarName)>,
    pairs: &mut Vec<(StmtId, StmtId)>,
    analysis: &mut OrphanAnalysis,
) {
    match stmt {
        Stmt::For { var, .. } => {
            for_stack.push((here.clone(), var.clone()));
        }
        Stmt::Next { vars } => {
            for v in vars {
                let popped = match v {
                    None => for_stack.pop(),
                    Some(target) => {
                        let mut found = None;
                        while let Some((id, fvar)) = for_stack.last() {
                            if fvar == target {
                                found = Some((id.clone(), fvar.clone()));
                                for_stack.pop();
                                break;
                            }
                            for_stack.pop();
                        }
                        found
                    }
                };
                match popped {
                    Some((for_id, _)) => pairs.push((for_id, here.clone())),
                    None => analysis.orphan_nexts.push(here.clone()),
                }
            }
        }
        Stmt::If { then, .. } => walk_then(then, here, for_stack, pairs, analysis),
        Stmt::IfElse {
            then, else_then, ..
        } => {
            walk_then(then, here, for_stack, pairs, analysis);
            walk_then(else_then, here, for_stack, pairs, analysis);
        }
        Stmt::Rcomp { then, else_then } => {
            walk_then(then, here, for_stack, pairs, analysis);
            if let Some(et) = else_then {
                walk_then(et, here, for_stack, pairs, analysis);
            }
        }
        _ => {}
    }
}

fn walk_then(
    then: &ThenIr,
    parent: &StmtId,
    for_stack: &mut Vec<(StmtId, VarName)>,
    pairs: &mut Vec<(StmtId, StmtId)>,
    analysis: &mut OrphanAnalysis,
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
            walk_stmt_for_pairing(inner, &id, for_stack, pairs, analysis);
        }
    }
}

fn build_line_index(module: &Module) -> HashMap<u16, usize> {
    module
        .lines
        .iter()
        .enumerate()
        .map(|(i, l)| (l.number, i))
        .collect()
}

#[derive(Debug, Clone, Default)]
struct ForBodyInfo {
    /// Lines covered between the FOR header and its matching NEXT.
    lines_in_body: Vec<u16>,
    /// GOTO targets reachable from inside the body.
    goto_targets: HashSet<u16>,
    /// True when no static NEXT was found.
    no_static_match: bool,
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
        Stmt::For { .. } => {
            if !nested {
                for_stack.push((here.clone(), ForBodyInfo::default()));
            }
        }
        Stmt::Next { vars } => {
            if !nested {
                for _ in vars {
                    if let Some((id, info)) = for_stack.pop() {
                        result.insert(id, info);
                    }
                }
            }
        }
        Stmt::Goto { target } | Stmt::GoSub { target } => {
            for (_, info) in for_stack.iter_mut() {
                info.goto_targets.insert(*target);
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
                info.goto_targets.insert(*target);
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

/// Lines reachable from `start_line`: source-order suffix plus any
/// GOTO targets along the way.
fn lines_reachable_from(
    module: &Module,
    start_line: u16,
    line_index: &HashMap<u16, usize>,
) -> HashSet<u16> {
    let Some(&start_idx) = line_index.get(&start_line) else {
        return HashSet::new();
    };
    let mut reached: HashSet<u16> = HashSet::new();
    let mut worklist: Vec<usize> = vec![start_idx];
    while let Some(idx) = worklist.pop() {
        if idx >= module.lines.len() {
            continue;
        }
        let line = &module.lines[idx];
        if !reached.insert(line.number) {
            continue;
        }
        let mut stop = false;
        for stmt in &line.stmts {
            collect_branch_targets(stmt, line_index, &mut worklist, &mut stop);
            if stop {
                break;
            }
        }
        if !stop && idx + 1 < module.lines.len() {
            worklist.push(idx + 1);
        }
    }
    reached
}

fn collect_branch_targets(
    stmt: &Stmt,
    line_index: &HashMap<u16, usize>,
    worklist: &mut Vec<usize>,
    stop: &mut bool,
) {
    match stmt {
        Stmt::Goto { target } => {
            if let Some(&t) = line_index.get(target) {
                worklist.push(t);
            }
            *stop = true;
        }
        Stmt::GoSub { target } => {
            if let Some(&t) = line_index.get(target) {
                worklist.push(t);
            }
        }
        Stmt::Return | Stmt::End | Stmt::Stop => *stop = true,
        Stmt::If { then, .. } => collect_then_targets(then, line_index, worklist),
        Stmt::IfElse {
            then, else_then, ..
        } => {
            collect_then_targets(then, line_index, worklist);
            collect_then_targets(else_then, line_index, worklist);
        }
        Stmt::Rcomp { then, else_then } => {
            collect_then_targets(then, line_index, worklist);
            if let Some(et) = else_then {
                collect_then_targets(et, line_index, worklist);
            }
        }
        _ => {}
    }
}

fn collect_then_targets(then: &ThenIr, line_index: &HashMap<u16, usize>, worklist: &mut Vec<usize>) {
    match then {
        ThenIr::Goto(target) => {
            if let Some(&t) = line_index.get(target) {
                worklist.push(t);
            }
        }
        ThenIr::Stmts(stmts) => {
            let mut stop = false;
            for s in stmts {
                collect_branch_targets(s, line_index, worklist, &mut stop);
                if stop {
                    break;
                }
            }
        }
    }
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

    fn line(n: u16, stmts: Vec<Stmt>) -> Line {
        Line { number: n, stmts }
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
        let ids: Vec<u8> = a.runtime_fors.values().copied().collect();
        assert!(ids.iter().all(|&v| v >= 1));
        let mut sorted = ids.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len());
    }
}
