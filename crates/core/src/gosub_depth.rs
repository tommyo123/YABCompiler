//! Prove a bound on GOSUB nesting from the call graph.

use std::collections::{HashMap, HashSet};

use crate::ast::OnBranchKind;
use crate::cfg::Cfg;
use crate::ir::{Module, Stmt};

/// What the walk proved about a program's GOSUB frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GosubDepth {
    /// The most GOSUB frames live at once on any path.
    pub max_depth: u32,
}

/// Find RETURN lines reachable without a GOSUB frame.
pub fn lines_returning_without_a_call(module: &Module) -> Option<HashSet<u16>> {
    if moves_frames_otherwise(module) {
        return None;
    }
    let cfg = Cfg::build(module);
    if cfg.nodes.is_empty() {
        return Some(HashSet::new());
    }
    let main = walk(&cfg, module, &main_roots(&cfg, module));
    Some(
        main.returns_at
            .iter()
            .map(|&n| cfg.nodes[n].line_no)
            .collect(),
    )
}

/// Entry points with no GOSUB frames.
fn main_roots(cfg: &Cfg, module: &Module) -> Vec<usize> {
    let mut main_roots = vec![cfg.entry];
    for id in 0..cfg.nodes.len() {
        match cfg.stmt_at(id, module) {
            Stmt::Run(target) => {
                let dest = match target {
                    Some(t) => cfg.line_first.get(t).copied(),
                    None => Some(cfg.entry),
                };
                main_roots.extend(dest);
            }
            Stmt::Clr => main_roots.extend(cfg.nodes[id].successors.iter().copied()),
            _ => {}
        }
    }
    main_roots
}

/// Prove that every RETURN has a frame to pop and bound the nesting, or return `None`.
pub fn prove(module: &Module) -> Option<GosubDepth> {
    if moves_frames_otherwise(module) {
        return None;
    }
    let cfg = Cfg::build(module);
    if cfg.nodes.is_empty() {
        return Some(GosubDepth { max_depth: 0 });
    }
    // Start at entry, RUN targets and CLR continuations.
    let main_roots = main_roots(&cfg, module);
    let main = walk(&cfg, module, &main_roots);
    if main.returns {
        return None;
    }
    // Regions of every procedure the main one can reach, then the longest call chain.
    let mut calls: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut pending: Vec<usize> = main.calls.iter().copied().collect();
    while let Some(p) = pending.pop() {
        if calls.contains_key(&p) {
            continue;
        }
        let region = walk(&cfg, module, &[p]);
        pending.extend(region.calls.iter().copied());
        calls.insert(p, region.calls.into_iter().collect());
    }
    let mut depth: HashMap<usize, Option<u32>> = HashMap::new();
    let mut max_depth = 0;
    for &p in &main.calls {
        max_depth = max_depth.max(1 + chain(p, &calls, &mut depth)?);
    }
    Some(GosubDepth { max_depth })
}

struct Region {
    calls: HashSet<usize>,
    returns: bool,
    /// The RETURN nodes the walk reached.
    returns_at: Vec<usize>,
}

fn walk(cfg: &Cfg, module: &Module, roots: &[usize]) -> Region {
    let mut region = Region {
        calls: HashSet::new(),
        returns: false,
        returns_at: Vec::new(),
    };
    let mut seen: HashSet<usize> = HashSet::new();
    let mut work: Vec<usize> = roots.to_vec();
    while let Some(n) = work.pop() {
        if !seen.insert(n) {
            continue;
        }
        let call_targets: Option<Vec<u16>> = match cfg.stmt_at(n, module) {
            Stmt::Return => {
                region.returns = true;
                region.returns_at.push(n);
                continue;
            }
            Stmt::GoSub { target } => Some(vec![*target]),
            Stmt::OnBranch {
                kind: OnBranchKind::GoSub,
                targets,
                ..
            } => Some(targets.clone()),
            _ => None,
        };
        match call_targets {
            Some(targets) => {
                region
                    .calls
                    .extend(targets.iter().filter_map(|t| cfg.line_first.get(t).copied()));
                work.extend(cfg.gosub_continuations.get(&n).copied());
            }
            None => work.extend(cfg.nodes[n].successors.iter().copied()),
        }
    }
    region
}

/// Longest call chain, or None when calls form a cycle.
fn chain(
    p: usize,
    calls: &HashMap<usize, Vec<usize>>,
    depth: &mut HashMap<usize, Option<u32>>,
) -> Option<u32> {
    match depth.get(&p) {
        Some(Some(d)) => return Some(*d),
        Some(None) => return None,
        None => {}
    }
    depth.insert(p, None);
    let mut d = 0;
    for &q in calls.get(&p).map(Vec::as_slice).unwrap_or(&[]) {
        d = d.max(1 + chain(q, calls, depth)?);
    }
    depth.insert(p, Some(d));
    Some(d)
}

/// Control flow that the GOSUB frame analysis cannot track.
fn moves_frames_otherwise(module: &Module) -> bool {
    struct Find(bool);
    impl crate::visit::Visitor for Find {
        fn visit_stmt(&mut self, line: u16, stmt: &Stmt) {
            if matches!(
                stmt,
                Stmt::Resume { .. }
                    | Stmt::OnError { .. }
                    | Stmt::OnKey { .. }
                    | Stmt::ComputedGoto { .. }
            ) {
                self.0 = true;
            } else if !self.0 {
                crate::visit::walk_stmt(self, line, stmt);
            }
        }
    }
    let mut find = Find(false);
    crate::visit::walk_module(&mut find, module);
    find.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::Pass;

    fn module(src: &str) -> Module {
        let bytes = crate::tokenize_program(src).unwrap();
        let program = crate::prg::Program::parse(&bytes).unwrap();
        let ast = crate::parse::program(&program).unwrap();
        let mut module = crate::ir::lower(&ast).unwrap();
        crate::passes::ResolveBareNext.run(&mut module).unwrap();
        module
    }

    fn depth(src: &str) -> Option<u32> {
        prove(&module(src)).map(|d| d.max_depth)
    }

    #[test]
    fn a_main_loop_calling_two_levels_is_two_deep() {
        let src = "10 GOSUB 100:GOSUB 200:GOTO 10\n100 GOSUB 200:RETURN\n200 PRINT \"X\":RETURN\n";
        assert_eq!(depth(src), Some(2));
    }

    #[test]
    fn a_program_without_gosub_is_zero_deep() {
        assert_eq!(depth("10 PRINT \"X\":GOTO 10\n"), Some(0));
    }

    #[test]
    fn falling_into_a_subroutine_reaches_return_without_gosub() {
        assert_eq!(depth("10 GOSUB 20\n20 PRINT \"X\":RETURN\n"), None);
    }

    #[test]
    fn a_subroutine_left_by_goto_and_called_again_is_unbounded() {
        let src = "10 GOSUB 100\n20 END\n100 IF X THEN GOTO 10\n110 RETURN\n";
        assert_eq!(depth(src), None);
    }

    #[test]
    fn recursion_is_unbounded() {
        let src = "10 GOSUB 100:END\n100 IF X<5 THEN X=X+1:GOSUB 100\n110 RETURN\n";
        assert_eq!(depth(src), None);
    }

    #[test]
    fn a_subroutine_left_by_goto_to_an_end_still_bounds() {
        let src = "10 GOSUB 100:GOTO 10\n100 IF X THEN 300\n110 RETURN\n300 END\n";
        assert_eq!(depth(src), Some(1));
    }

    #[test]
    fn on_gosub_calls_every_target() {
        let src = "10 ON X GOSUB 100,200:GOTO 10\n100 GOSUB 200:RETURN\n200 RETURN\n";
        assert_eq!(depth(src), Some(2));
    }

    #[test]
    fn clr_in_the_main_code_keeps_the_proof() {
        assert_eq!(depth("10 GOSUB 100:CLR:GOTO 10\n100 RETURN\n"), Some(1));
    }

    #[test]
    fn a_return_after_clr_in_a_subroutine_has_no_frame() {
        assert_eq!(depth("10 GOSUB 100:END\n100 CLR:RETURN\n"), None);
    }

    #[test]
    fn a_subroutine_after_clr_is_still_counted() {
        let src = "10 GOSUB 100:END\n100 CLR:GOSUB 200:GOTO 300\n200 RETURN\n300 END\n";
        assert_eq!(depth(src), Some(2));
    }

    #[test]
    fn a_run_target_counts_as_main_code() {
        // Line 500 is reached only by RUN, at depth 0, and returns there.
        let src = "10 GOSUB 100:END\n100 RUN 500\n500 RETURN\n";
        assert_eq!(depth(src), None);
    }
}
