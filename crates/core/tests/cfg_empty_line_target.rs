//! A line with no statements is still a branch target.

mod common;

use yabcompiler_core::cfg::Cfg;
use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn module_of(src: &str) -> yabcompiler_core::ir::Module {
    let bytes = tokenize_program(src).expect("tokenizes");
    let prg = yabcompiler_core::Program::parse(&bytes).expect("is a BASIC prg");
    let ast = yabcompiler_core::parse::program(&prg).expect("parses");
    yabcompiler_core::ir::lower(&ast).expect("lowers")
}

fn compiled(src: &str) -> yabcompiler_core::Compiled {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(
        &prg,
        CompileOptions {
            profile: Profile::Default,
            force_extraram_off: true,
            ..CompileOptions::default()
        },
    )
    .expect("compiles")
}

fn run(src: &str) -> (bool, String) {
    let c = compiled(src);
    let r = emu64::run_prg_to_end(&c.prg_bytes, common::MAX_INSNS).expect("emu64 run");
    (r.clean_exit, common::petscii_to_string(&r.output))
}

/// Everything reachable from `start` by following successors.
fn reachable(cfg: &Cfg, start: usize) -> Vec<usize> {
    let mut seen = vec![false; cfg.nodes.len()];
    let mut work = vec![start];
    let mut out = vec![start];
    seen[start] = true;
    while let Some(n) = work.pop() {
        for &s in &cfg.nodes[n].successors {
            if !seen[s] {
                seen[s] = true;
                out.push(s);
                work.push(s);
            }
        }
    }
    out
}

const DODGEM_SHAPE: &str = "\
10 bg=53280:if a$=\"x\" then bg=0
20 goto 1000
40 if a$=\"y\" then run
1000 :
1010 print bg
";

#[test]
fn goto_to_an_empty_line_edges_to_the_next_line() {
    let m = module_of(DODGEM_SHAPE);
    let cfg = Cfg::build(&m);
    let goto = cfg.line_first[&20];
    let target = cfg.line_first[&1010];
    assert!(
        cfg.nodes[goto].successors.contains(&target),
        "`20 GOTO 1000` with `1000 :` empty must edge to line 1010's \
         first node ({target}); successors were {:?}",
        cfg.nodes[goto].successors
    );
    assert!(
        reachable(&cfg, cfg.entry).contains(&target),
        "line 1010 must be reachable from the program entry"
    );
}

#[test]
fn the_store_read_only_past_an_empty_line_target_survives() {
    let c = compiled(DODGEM_SHAPE);
    assert!(
        c.asm.contains("V_BG"),
        "the untaken second assignment keeps BG a real variable"
    );
    assert!(
        c.asm.contains("LDX #<V_BG"),
        "`10 BG=53280` is read at line 1010 and must not be dropped as \
         a dead store; asm had no store to V_BG"
    );
    let (clean, out) = run(DODGEM_SHAPE);
    assert!(clean, "program must terminate");
    assert_eq!(out.trim(), "53280", "line 1010 must print BG, not 0");
}

#[test]
fn gosub_to_an_empty_line_runs_the_body_and_returns() {

    const SRC: &str = "\
10 gosub 100
20 print \"back\"
30 end
100 :
110 print \"sub\"
120 return
";
    let m = module_of(SRC);
    let cfg = Cfg::build(&m);
    let gosub = cfg.line_first[&10];
    let body = cfg.line_first[&110];
    assert!(
        cfg.nodes[gosub].successors.contains(&body),
        "`GOSUB 100` with `100 :` empty must edge to line 110; \
         successors were {:?}",
        cfg.nodes[gosub].successors
    );
    let after = cfg.line_first[&20];
    assert!(
        reachable(&cfg, cfg.entry).contains(&after),
        "the statement after the GOSUB must be reachable — it is only \
         reachable through the RETURN, which is only in the graph if \
         the subroutine body is"
    );
    let (clean, out) = run(SRC);
    assert!(clean);
    assert_eq!(out.trim(), "SUB\nBACK");
}

#[test]
fn a_run_of_empty_lines_lands_on_the_first_real_statement() {
    const SRC: &str = "\
10 goto 100
20 print \"skipped\"
100 :
110 :
120 :
130 print \"here\"
";
    let m = module_of(SRC);
    let cfg = Cfg::build(&m);
    let goto = cfg.line_first[&10];
    let target = cfg.line_first[&130];
    assert_eq!(
        cfg.nodes[goto].successors,
        vec![target],
        "three empty lines in a row must collapse to one edge into 130"
    );
    let (clean, out) = run(SRC);
    assert!(clean);
    assert_eq!(out.trim(), "HERE");
}

#[test]
fn goto_to_a_line_that_does_not_exist_still_has_no_successor() {
    const SRC: &str = "\
10 print \"a\"
20 goto 999
30 print \"b\"
";
    let m = module_of(SRC);
    let cfg = Cfg::build(&m);
    let goto = cfg.line_first[&20];
    assert!(
        cfg.nodes[goto].successors.is_empty(),
        "line 999 does not exist: the branch is `?UNDEF'D STATEMENT`, \
         not a fall-through to line 30; successors were {:?}",
        cfg.nodes[goto].successors
    );
    assert!(
        !cfg.line_first.contains_key(&999),
        "a line that is not in the program must not acquire an entry node"
    );
}

#[test]
fn an_empty_line_at_the_end_of_the_program_is_still_a_dead_end() {
    const SRC: &str = "\
10 print \"a\"
20 goto 100
30 print \"b\"
100 :
";
    let m = module_of(SRC);
    let cfg = Cfg::build(&m);
    let goto = cfg.line_first[&20];
    assert!(
        cfg.nodes[goto].successors.is_empty(),
        "nothing follows the empty line 100, so `GOTO 100` runs off the \
         end of the program; successors were {:?}",
        cfg.nodes[goto].successors
    );
    let (clean, out) = run(SRC);
    assert!(clean);
    assert_eq!(out.trim(), "A", "line 30 must not run");
}

#[test]
fn an_empty_line_gains_no_node_of_its_own() {

    const SRC: &str = "\
10 print \"a\"
20 :
30 print \"b\"
";
    let m = module_of(SRC);
    let cfg = Cfg::build(&m);
    let ten = cfg.line_first[&10];
    let thirty = cfg.line_first[&30];
    assert_eq!(cfg.nodes[ten].successors, vec![thirty]);
    assert_eq!(
        cfg.line_first[&20], thirty,
        "line 20 is empty, so its entry is line 30's node — the same \
         node, not a copy"
    );
    assert!(
        !cfg.nodes.iter().any(|n| n.line_no == 20),
        "an empty line must still allocate no CfgNode of its own"
    );
}

#[test]
fn a_genuinely_unreachable_reader_still_lets_the_store_go() {

    const SRC: &str = "\
10 z=42
20 goto 100
30 print z
40 if a$=\"y\" then run
100 print \"ok\"
";
    let c = compiled(SRC);
    assert!(
        !c.asm.contains("LDX #<V_Z"),
        "`10 Z=42` has no reachable reader and must still be deleted"
    );
    let (clean, out) = run(SRC);
    assert!(clean);
    assert_eq!(out.trim(), "OK");
}

#[test]
fn a_store_read_on_the_ordinary_fall_through_path_is_never_touched() {

    const SRC: &str = "\
10 q=7
20 gosub 100
30 end
100 print q
110 return
";
    let (clean, out) = run(SRC);
    assert!(clean);
    assert_eq!(out.trim(), "7");
}
