//! The runtime FOR-stack: variable identity, the GOSUB barrier, and bounds.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn build(src: &str, profile: Profile, extraram: bool) -> (Vec<u8>, String) {
    let prg = tokenize_program(src).expect("tokenizes");
    let opts = CompileOptions {
        profile,
        extraram,
        force_extraram_off: !extraram,
        ..CompileOptions::default()
    };
    let c = compile_with_options(&prg, opts)
        .unwrap_or_else(|e| panic!("compiles ({profile:?}, extraram={extraram}): {e}"));
    (c.prg_bytes, c.asm)
}
fn check_all_variants(src: &str, expected: &str) {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        for extraram in [false, true] {
            let (bytes, _) = build(src, profile, extraram);
            let got = common::run_clean(&bytes).unwrap_or_else(|| {
                panic!("runs to a clean exit ({profile:?}, extraram={extraram}):\n{src}")
            });
            assert_eq!(
                got, expected,
                "{profile:?} extraram={extraram} mismatch for:\n{src}"
            );
        }
    }
}
fn check_stops_with(src: &str, expected: &str) {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        for extraram in [false, true] {
            let (bytes, _) = build(src, profile, extraram);
            let r = emu64::run_prg_to_end(&bytes, 6_000_000).expect("runs");
            let got = common::petscii_to_string(&r.output);
            assert_eq!(
                got, expected,
                "{profile:?} extraram={extraram} output mismatch for:\n{src}"
            );
            assert!(
                !r.clean_exit,
                "{profile:?} extraram={extraram}: the BASIC error must abort the \
                 program, not let it end normally:\n{src}"
            );
        }
    }
}

fn asm_of(src: &str) -> String {
    build(src, Profile::Default, false).1
}

// Runtime NEXT variable binding.

/// Adjacent statements read the same loop variable.
#[test]
fn next_with_a_variable_finds_that_variables_frame() {
    let src = "10 GOTO 100\n\
               20 NEXT I\n\
               30 PRINT\"a\";I:PRINT\"b\";I:END\n\
               100 FOR I=1 TO 2\n\
               110 FOR J=1 TO 2\n\
               120 GOTO 20\n";
    check_all_variants(src, "A 3 \nB 3 \n");
}

/// FNDFOR's discard.
#[test]
fn next_with_a_variable_discards_the_frames_above_it() {
    let src = "10 GOTO 100\n\
               20 NEXT I\n\
               30 PRINT\"aft\";I;J:END\n\
               100 FOR I=1 TO 3\n\
               110 FOR J=5 TO 9\n\
               120 PRINT\"p\";I;J\n\
               130 GOTO 20\n";
    check_all_variants(
        src,
        "P 1  5 \nP 2  5 \nP 3  5 \nAFT 4  5 \n",
    );
}
#[test]
fn multi_var_next_in_nesting_order() {
    let src = "10 GOTO 100\n\
               20 NEXT J,I\n\
               30 PRINT\"aft\";I;J:END\n\
               100 FOR I=1 TO 2\n\
               110 FOR J=1 TO 2\n\
               120 PRINT\"p\";I;J\n\
               130 GOTO 20\n";
    check_all_variants(
        src,
        "P 1  1 \nP 1  2 \nP 2  1 \nP 2  2 \nAFT 3  3 \n",
    );
}

/// `NEXT I,J` with `I` the *outer* loop.
#[test]
fn multi_var_next_in_reverse_order_runs_out_of_frames() {
    let src = "10 GOTO 100\n\
               20 NEXT I,J\n\
               30 PRINT\"aft\";I;J:END\n\
               100 FOR I=1 TO 2\n\
               110 FOR J=1 TO 2\n\
               120 PRINT\"p\";I;J\n\
               130 GOTO 20\n";
    check_stops_with(src, "P 1  1 \nP 2  1 \n");
}

/// `NEXT K` where no `FOR K` is live.
#[test]
fn next_naming_an_unopened_variable_is_next_without_for() {
    let src = "10 GOTO 100\n\
               20 NEXT K\n\
               30 PRINT\"aft\";I;J;K:END\n\
               100 FOR I=1 TO 2\n\
               110 FOR J=1 TO 2\n\
               120 PRINT\"p\";I;J\n\
               130 GOTO 20\n";
    check_stops_with(src, "P 1  1 \n");
}
#[test]
fn next_i_and_next_j_no_longer_emit_the_same_code() {
    let head = "10 GOTO 100\n";
    let tail = "30 PRINT\"aft\";I;J:END\n\
                100 FOR I=1 TO 2\n\
                110 FOR J=1 TO 2\n\
                120 PRINT\"p\";I;J\n\
                130 GOTO 20\n";
    let a = asm_of(&format!("{head}20 NEXT I\n{tail}"));
    let b = asm_of(&format!("{head}20 NEXT J\n{tail}"));
    assert_ne!(
        a, b,
        "NEXT I and NEXT J must not lower to identical code"
    );
    for asm in [&a, &b] {
        assert!(
            asm.contains("LDY __FOR_STACK_VAR,X"),
            "the dispatcher must compare the frame's variable id:\n{asm}"
        );
    }
}

// GOSUB stack barrier.

/// BASIC's FNDFOR stops at the first non-FOR record, so a `NEXT` in a subroutine cannot close
/// the caller's loop.
#[test]
fn a_next_inside_a_gosub_cannot_close_the_callers_loop() {
    let src = "10 FOR I=1 TO 3\n\
               20 PRINT\"i\";I\n\
               30 GOSUB 100\n\
               40 PRINT\"a\";I\n\
               50 NEXT I\n\
               60 PRINT\"e\";I:END\n\
               100 NEXT I\n\
               110 RETURN\n";
    check_stops_with(src, "I 1 \n");
}
#[test]
fn a_gosub_marker_on_top_stops_a_bare_next() {
    let src = "10 GOSUB 100\n\
               20 PRINT\"back\":END\n\
               30 NEXT\n\
               40 PRINT\"aftn\";I:END\n\
               100 FOR I=1 TO 3\n\
               110 PRINT\"i\";I\n\
               120 GOSUB 200\n\
               130 END\n\
               200 PRINT\"s\"\n\
               210 GOTO 30\n";
    check_stops_with(src, "I 1 \nS\n");
}

/// The named-variable form of the same rule: `NEXT I` must not search *past* a marker to find
/// the `I` frame underneath it.
#[test]
fn next_with_a_variable_stops_at_a_gosub_marker() {
    let src = "10 GOSUB 100\n\
               20 PRINT\"back\":END\n\
               30 NEXT I\n\
               40 PRINT\"aftn\";I:END\n\
               100 FOR I=1 TO 3\n\
               110 GOSUB 200\n\
               120 END\n\
               200 GOTO 30\n";
    check_stops_with(src, "");
}
#[test]
fn return_unwinds_to_its_own_marker_and_the_loop_survives() {
    let src = "10 C=0:GOSUB 100\n\
               20 PRINT\"back\";I;C:END\n\
               100 FOR I=1 TO 3\n\
               110 C=C+1:PRINT\"i\";I;C\n\
               120 IF C<6 THEN 100\n\
               130 NEXT I\n\
               140 RETURN\n";
    check_all_variants(
        src,
        "I 1  1 \nI 1  2 \nI 1  3 \nI 1  4 \nI 1  5 \nI 1  6 \nI 2  7 \nI 3  8 \nBACK 4  8 \n",
    );
}
#[test]
fn re_entering_a_for_header_keeps_the_stack_at_one_frame() {
    let src = "10 C=0:GOTO 100\n\
               20 NEXT\n\
               30 PRINT\"e\";I;C:END\n\
               100 FOR I=1 TO 3\n\
               110 C=C+1\n\
               120 IF C<40 THEN 100\n\
               130 GOTO 20\n";
    check_all_variants(src, "E 4  42 \n");
}
#[test]
fn a_re_entered_for_does_not_overwrite_the_saved_stack_pointer() {
    let src = "10 C=0:GOSUB 90\n\
               20 PRINT\"back\";C:END\n\
               30 NEXT\n\
               40 PRINT\"e\";I;C:END\n\
               90 GOTO 100\n\
               100 FOR I=1 TO 3\n\
               110 C=C+1\n\
               120 IF C<40 THEN 100\n\
               130 GOTO 30\n";
    check_all_variants(src, "E 4  42 \n");
}
#[test]
fn a_second_for_on_the_same_variable_replaces_the_first_frame() {
    let src = "10 P=1:GOTO 100\n\
               20 NEXT\n\
               30 IF P=1 THEN 200\n\
               40 PRINT\"e\";I:END\n\
               100 FOR I=1 TO 3\n\
               110 PRINT\"a\";I:GOTO 20\n\
               200 P=2\n\
               210 FOR I=1 TO 2\n\
               220 PRINT\"b\";I:GOTO 20\n";
    // `210 FOR I` discards the frame `100 FOR I` left, so the second loop runs on its own
    // bounds and the program ends at 40 with the second loop's exit value.
    check_all_variants(src, "A 1 \nA 2 \nA 3 \nB 1 \nB 2 \nE 3 \n");
}
#[test]
fn a_for_site_re_entered_across_a_gosub_marker_stacks_a_second_frame() {
    let src = "10 N=3:GOSUB 100\n\
               20 PRINT\"end\";I;N:END\n\
               100 FOR I=1 TO N\n\
               110 PRINT\"i\";I\n\
               120 IF I=2 AND N=3 THEN N=5:GOSUB 200\n\
               130 NEXT I\n\
               140 RETURN\n\
               200 GOTO 100\n";
    check_all_variants(
        src,
        "I 1 \nI 2 \nI 1 \nI 2 \nI 3 \nI 4 \nI 5 \nEND 7  5 \n",
    );
}
#[test]
fn an_unbounded_gosub_chain_raises_out_of_memory() {
    let src = "10 C=0\n\
               20 FOR I=1 TO 3\n\
               30 C=C+1\n\
               40 GOSUB 100\n\
               50 NEXT I\n\
               60 PRINT\"e\";C:END\n\
               100 PRINT\"x\";\n\
               110 GOTO 30\n";
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        for extraram in [false, true] {
            let (bytes, asm) = build(src, profile, extraram);
            assert!(
                asm.contains("BCS __FOR_OOM"),
                "the push must be bounds-checked ({profile:?}, extraram={extraram})"
            );
            let r = emu64::run_prg_to_end(&bytes, 6_000_000).expect("runs");
            let got = common::petscii_to_string(&r.output);
            assert!(
                !r.clean_exit,
                "{profile:?} extraram={extraram}: must stop with ?OUT OF MEMORY"
            );
            // 18 bytes for the FOR frame plus 7 per leaked marker: the 23rd pass is the one
            // BASIC cannot afford.
            assert_eq!(
                got.matches('X').count(),
                22,
                "{profile:?} extraram={extraram}: unexpected trip count before the \
                 stack filled: {got:?}"
            );
        }
    }
}
#[test]
fn saved_sp_is_laid_out_before_the_for_stack() {
    let asm = asm_of(
        "10 GOSUB 100\n\
         20 PRINT\"back\":END\n\
         30 NEXT\n\
         100 FOR I=1 TO 3\n\
         110 GOTO 30\n",
    );
    let sp = asm.find("__SAVED_SP:").expect("program uses __SAVED_SP");
    let stack = asm.find("__FOR_STACK:").expect("program has a runtime FOR-stack");
    assert!(
        sp < stack,
        "__SAVED_SP must be emitted before __FOR_STACK so an overrun cannot reach it"
    );
}
#[test]
fn a_static_loop_emits_no_runtime_for_stack_at_all() {
    let asm = asm_of("10 FOR I=1 TO 10\n20 PRINT I\n30 NEXT I\n40 END\n");
    for marker in [
        "__FOR_STACK",
        "__FOR_STACK_VAR",
        "__FOR_HANDLER_TABLE",
        "__FOR_DISP_VEC",
        "__FOR_OOM",
        "__NEXT_NO_FOR",
    ] {
        assert!(
            !asm.contains(marker),
            "a statically paired loop must not emit {marker}:\n{asm}"
        );
    }
}
#[test]
fn a_static_nested_loop_with_multi_var_next_stays_inline() {
    let asm = asm_of(
        "10 FOR I=1 TO 3\n\
         20 FOR J=1 TO 3\n\
         30 PRINT I;J\n\
         40 NEXT J,I\n\
         50 END\n",
    );
    assert!(
        !asm.contains("__FOR_STACK"),
        "a static NEXT J,I must stay inline:\n{asm}"
    );
}
#[test]
fn a_static_nested_loop_with_multi_var_next_still_runs() {
    let src = "10 S=0\n\
               20 FOR I=1 TO 3\n\
               30 FOR J=1 TO 3\n\
               40 S=S+I*10+J\n\
               50 NEXT J,I\n\
               60 PRINT\"s\";S;I;J\n";
    check_all_variants(src, "S 198  4  4 \n");
}

/// A bare `NEXT` keeps the cheap top-of-stack path: two loads and a branch, no search loop.
#[test]
fn a_bare_orphan_next_keeps_the_top_of_stack_fast_path() {
    let asm = asm_of(
        "10 GOTO 100\n\
         20 NEXT\n\
         30 PRINT\"aft\";I:END\n\
         100 FOR I=1 TO 3\n\
         110 GOTO 20\n",
    );
    assert!(
        asm.contains("LDX __FOR_STACK_PTR\n    BEQ __NEXT_NO_FOR\n    DEX\n    LDA __FOR_STACK,X\n    BEQ __NEXT_NO_FOR\n    ASL"),
        "bare NEXT must peek the top frame, not search:\n{asm}"
    );
    assert!(
        !asm.contains("__FOR_RT_SRCH_"),
        "bare NEXT must not emit the FNDFOR search loop:\n{asm}"
    );
}
#[test]
fn bare_next_is_still_resolved_to_its_loop_variable_when_static() {
    use yabcompiler_core::ast::{VarKind, VarName};
    use yabcompiler_core::ir::{Expr, Line, Module, Pass, Stmt};
    use yabcompiler_core::passes::ResolveBareNext;

    let fvar = |n: &str| VarName {
        base: n.to_string(),
        kind: VarKind::Float,
    };
    let for_stmt = |v: &str| Stmt::For {
        var: fvar(v),
        start: Expr::Number(1.0),
        end: Expr::Number(3.0),
        step: Expr::Number(1.0),
        body_int_safe: false,
        body_reads_loop_var: true,
        induction_const: None,
        array_inductions: Vec::new(),
    };
    let bare_next = || Stmt::Next { vars: vec![None] };
    let named = |m: &Module| {
        m.lines
            .iter()
            .flat_map(|l| l.stmts.iter())
            .any(|s| matches!(s, Stmt::Next { vars } if vars.iter().any(|v| v.is_some())))
    };

    // Statically paired: the bare NEXT is named, which is what lets liveness see that NEXT
    // reads its counter.
    let mut m = Module {
        lines: vec![
            Line {
                number: 10,
                stmts: vec![for_stmt("I")],
            },
            Line {
                number: 20,
                stmts: vec![bare_next(), Stmt::End],
            },
        ],
    };
    ResolveBareNext.run(&mut m).expect("pass runs");
    assert!(named(&m), "a statically paired bare NEXT must still be named");

    // Runtime-dispatched: it stays bare.
    let mut m = Module {
        lines: vec![
            Line {
                number: 10,
                stmts: vec![Stmt::Goto { target: 100 }],
            },
            Line {
                number: 20,
                stmts: vec![bare_next(), Stmt::End],
            },
            Line {
                number: 100,
                stmts: vec![for_stmt("I")],
            },
            Line {
                number: 110,
                stmts: vec![for_stmt("J")],
            },
            Line {
                number: 120,
                stmts: vec![Stmt::Goto { target: 20 }],
            },
        ],
    };
    ResolveBareNext.run(&mut m).expect("pass runs");
    assert!(
        !named(&m),
        "a runtime-dispatched bare NEXT must not be given a variable"
    );
}

/// The push is only as expensive as it has to be: the FNDFOR discard search is emitted for a
/// runtime `FOR`, and nothing like it appears for a static one.
#[test]
fn the_fndfor_discard_is_emitted_for_runtime_fors_only() {
    let runtime = asm_of(
        "10 C=0:GOTO 100\n\
         20 NEXT\n\
         30 PRINT\"e\";I;C:END\n\
         100 FOR I=1 TO 3\n\
         110 C=C+1\n\
         120 IF C<5 THEN 100\n\
         130 GOTO 20\n",
    );
    assert!(
        runtime.contains("__FOR_PUSH_SRCH_"),
        "a re-enterable runtime FOR must discard its own live frame:\n{runtime}"
    );
    let stat = asm_of("10 FOR I=1 TO 3\n20 PRINT I\n30 NEXT\n40 END\n");
    assert!(
        !stat.contains("__FOR_PUSH_SRCH_"),
        "a static FOR must not emit the discard search:\n{stat}"
    );
}
