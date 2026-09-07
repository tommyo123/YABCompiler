

mod common;

use yabcompiler_core::cfg::Cfg;
use yabcompiler_core::ir::Stmt;
use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

/// `S` accumulates once per iteration, and the only path from the loop tail back to `40 S=S+1`
/// runs through the dispatched `NEXT` on line 10.
const ACCUMULATOR: &str = "\
1 s=0
5 goto 30
10 next:goto 50
30 for i=1 to 2:goto 40
40 s=s+1:goto 10
50 print \"e\";i;s
";
const FORTY_WITH_COUNTER: &str = "\
5 goto 10
6 next:if a=0 then end
7 goto 50
8 next:goto 50
10 for i=0 to 3
20 print i;:c=c+1
30 goto 80
40 a=0:goto 6
50 print:for i=1 to 4
60 print i;
70 goto 40
80 a=1:on (c and 1)+1 goto 6,8
";
const CLEAN_NEST: &str = "\
10 for i=1 to 3
20 for j=1 to 3
30 s=s+i*j
40 next j
50 next i
60 print s
";

fn module_of(src: &str) -> yabcompiler_core::ir::Module {
    let bytes = tokenize_program(src).expect("tokenizes");
    let prg = yabcompiler_core::Program::parse(&bytes).expect("is a BASIC prg");
    let ast = yabcompiler_core::parse::program(&prg).expect("parses");
    yabcompiler_core::ir::lower(&ast).expect("lowers")
}

fn run(src: &str, profile: Profile) -> (bool, String) {
    let prg = tokenize_program(src).expect("tokenizes");
    let c = compile_with_options(
        &prg,
        CompileOptions {
            profile,
            force_extraram_off: true,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}"));
    let r = emu64::run_prg_to_end(&c.prg_bytes, common::MAX_INSNS).expect("emu64 run");
    (r.clean_exit, common::petscii_to_string(&r.output))
}

/// Node ids of every `NEXT` in the graph.
fn next_nodes(cfg: &Cfg, m: &yabcompiler_core::ir::Module) -> Vec<usize> {
    (0..cfg.nodes.len())
        .filter(|&id| matches!(cfg.stmt_at(id, m), Stmt::Next { .. }))
        .collect()
}

/// Everything reachable from `start` by following successors.
fn reachable(cfg: &Cfg, start: usize) -> Vec<usize> {
    let mut seen = vec![false; cfg.nodes.len()];
    let mut work = vec![start];
    let mut out = Vec::new();
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

// Behaviour: the numbers the ROM produces

#[test]
fn loop_carried_accumulator_is_not_frozen_across_a_dispatched_next() {
    for profile in PROFILES {
        let (clean, out) = run(ACCUMULATOR, profile);
        assert!(clean, "{profile:?}: program should end cleanly, got {out:?}");
        let got: String = out.split_whitespace().collect::<Vec<_>>().join(" ");
        assert_eq!(
            got, "E 3 2",
            "{profile:?}: ROM prints `E 3  2`; `E 3  1` means the \
             accumulator was read from its pre-loop definition"
        );
    }
}
#[test]
fn dispatched_loop_counter_reaches_the_branch_that_ends_the_program() {
    for profile in PROFILES {
        let (clean, out) = run(FORTY_WITH_COUNTER, profile);
        let got: String = out.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            clean,
            "{profile:?}: a frozen counter makes this run forever; got {got:?}"
        );
        assert_eq!(
            got, "0 1 2 3 1 2 3 4",
            "{profile:?}: real-ROM output is ` 0  1  2  3` then ` 1  2  3  4`"
        );
    }
}

// Structure: the edges themselves

/// The graph-level statement of the same fact.
#[test]
fn dispatched_next_reaches_the_body_of_the_loop_it_runs() {
    let m = module_of(ACCUMULATOR);
    let orphans = yabcompiler_core::for_next_runtime::analyze(&m);
    assert!(
        orphans.needs_runtime_stack(),
        "line 10's NEXT is the dispatched shape this test is about"
    );

    let cfg = Cfg::build(&m);
    let next = cfg.line_first[&10];
    let for_hdr = cfg.line_first[&30];
    let body = *cfg.nodes[for_hdr]
        .successors
        .first()
        .expect("FOR falls through into its body");
    assert!(
        cfg.nodes[next].successors.contains(&body),
        "NEXT at line 10 must edge to the FOR's body start (node {body}); \
         successors were {:?}",
        cfg.nodes[next].successors
    );
    assert!(cfg.nodes[body].predecessors.contains(&next));

    let accumulate = cfg.line_first[&40];
    assert!(
        reachable(&cfg, next).contains(&accumulate),
        "`40 S=S+1` must be reachable from the loop tail, or no forward \
         dataflow can ever see the value the previous iteration wrote"
    );
}
#[test]
fn both_dispatched_nexts_reach_both_loop_bodies() {
    let m = module_of(FORTY_WITH_COUNTER);
    let cfg = Cfg::build(&m);
    for next_line in [6u16, 8] {
        let next = cfg.line_first[&next_line];
        for body_line in [20u16, 60] {
            let body = cfg.line_first[&body_line];
            assert!(
                cfg.nodes[next].successors.contains(&body),
                "NEXT at line {next_line} must reach the body at line \
                 {body_line}; successors were {:?}",
                cfg.nodes[next].successors
            );
        }
    }
}

/// A well-nested pair of loops keeps the static lowering: no runtime FOR stack in the output at
/// all, so the CFG gains nothing.
#[test]
fn clean_nest_keeps_the_static_lowering() {
    let m = module_of(CLEAN_NEST);
    let orphans = yabcompiler_core::for_next_runtime::analyze(&m);
    assert!(
        !orphans.needs_runtime_stack(),
        "nothing crosses either body, so nothing needs a frame"
    );

    let cfg = Cfg::build(&m);
    // `NEXT J` (line 40): back-edge to line 30 plus exit to line 50.
    for (next_line, back, exit) in [(40u16, 30u16, 50u16), (50, 20, 60)] {
        let n = cfg.line_first[&next_line];
        let mut succ = cfg.nodes[n].successors.clone();
        succ.sort_unstable();
        let mut want = vec![cfg.line_first[&back], cfg.line_first[&exit]];
        want.sort_unstable();
        assert_eq!(
            succ, want,
            "line {next_line}'s NEXT must have exactly a back-edge and an \
             exit — extra edges here are pure lost optimisation"
        );
    }

    for profile in PROFILES {
        let prg = tokenize_program(CLEAN_NEST).expect("tokenizes");
        let c = compile_with_options(
            &prg,
            CompileOptions {
                profile,
                force_extraram_off: true,
                ..CompileOptions::default()
            },
        )
        .expect("compiles");
        assert!(
            !c.asm.contains("__FOR_STACK_PTR"),
            "{profile:?}: a clean nest must not pull in the runtime FOR stack"
        );
    }
}
#[test]
fn an_untouched_loop_in_a_dispatching_program_gains_no_edges() {
    const MIXED: &str = "\
1 s=0
5 goto 30
10 next:goto 50
30 for i=1 to 2:goto 40
40 s=s+1:goto 10
50 print \"e\";i;s
100 for k=1 to 3
110 t=t+k
120 next k
130 print t
";
    let m = module_of(MIXED);
    let cfg = Cfg::build(&m);
    let n = cfg.line_first[&120];
    let mut succ = cfg.nodes[n].successors.clone();
    succ.sort_unstable();
    let mut want = vec![cfg.line_first[&110], cfg.line_first[&130]];
    want.sort_unstable();
    assert_eq!(
        succ, want,
        "the clean K loop must keep exactly its back-edge and its exit"
    );
    let k_body = cfg.line_first[&110];
    for id in next_nodes(&cfg, &m) {
        if id == n {
            continue;
        }
        assert!(
            !cfg.nodes[id].successors.contains(&k_body),
            "node {id} (a NEXT) must not reach the K loop's body"
        );
    }
}

/// `NEXT V` compares `__FOR_STACK_VAR`, so it can only ever stop on a frame a `FOR V` opened.
#[test]
fn a_named_dispatched_next_stays_within_its_own_variable() {
    const TWO_VARS: &str = "\
5 goto 40
10 next i:goto 90
20 next j:goto 90
40 for i=1 to 2
50 for j=1 to 2
60 goto 10
90 print i;j
";
    let m = module_of(TWO_VARS);
    let cfg = Cfg::build(&m);
    let next_i = cfg.line_first[&10];
    let body_i = cfg.line_first[&50]; // FOR I's body starts at line 50
    let body_j = cfg.line_first[&60]; // FOR J's body starts at line 60
    assert!(
        cfg.nodes[next_i].successors.contains(&body_i),
        "NEXT I reaches FOR I's body; successors {:?}",
        cfg.nodes[next_i].successors
    );
    assert!(
        !cfg.nodes[next_i].successors.contains(&body_j),
        "NEXT I cannot select a J frame, so it must not reach FOR J's body"
    );
}
