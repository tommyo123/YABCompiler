mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

fn build(src: &str, profile: Profile, extraram: bool) -> yabcompiler_core::Compiled {
    let prg = tokenize_program(src).expect("tokenize");
    compile_with_options(
        &prg,
        CompileOptions {
            profile,
            extraram,
            force_extraram_off: !extraram,
            ..CompileOptions::default()
        },
    )
    .expect("compile")
}

/// Run and report `(clean_exit, instructions, decoded output)`.
fn run(prg: &[u8]) -> (bool, u64, String) {
    let r = emu64::run_prg_to_end(prg, common::MAX_INSNS).expect("emu64 run");
    (
        r.clean_exit,
        r.instructions,
        common::petscii_to_string(&r.output),
    )
}

/// A `NEXT` lowered inline never mentions the runtime FOR stack.
fn uses_runtime_for_stack(asm: &str) -> bool {
    asm.contains("__FOR_STACK_PTR")
}

/// `CLR` in a loop body.
const CLR_IN_BODY: &str = "10 FOR I=1 TO 3\n\
                           20 PRINT I;\n\
                           30 CLR\n\
                           40 NEXT I\n\
                           50 PRINT \"=\";I\n\
                           60 END\n";

const CLR_VIA_GOTO: &str = "10 F=0\n\
                            20 FOR I=1 TO 5\n\
                            30 IF I=2 AND F=0 THEN F=1:GOTO 200\n\
                            40 PRINT I;\n\
                            50 NEXT I\n\
                            60 PRINT \"=\";I\n\
                            70 END\n\
                            200 CLR\n\
                            210 GOTO 40\n";

/// Header, reset and `NEXT` all on one line: the reset is *after* the header, so the
/// statement-order refinement inside the reachability walk has to catch it.
const CLR_SAME_LINE: &str = "10 FOR I=1 TO 3:PRINT I;:CLR:NEXT I\n\
                             20 PRINT \"=\";I\n";

const CLR_UNDER_THEN: &str = "10 FOR I=1 TO 3\n\
                              20 PRINT I;\n\
                              30 IF I=2 THEN CLR\n\
                              40 NEXT I\n\
                              50 PRINT \"=\";I\n\
                              60 END\n";

#[test]
fn a_reset_inside_a_loop_puts_its_next_on_the_runtime_path() {
    for (name, src) in [
        ("CLR_IN_BODY", CLR_IN_BODY),
        ("CLR_VIA_GOTO", CLR_VIA_GOTO),
        ("CLR_SAME_LINE", CLR_SAME_LINE),
        ("CLR_UNDER_THEN", CLR_UNDER_THEN),
    ] {
        for profile in PROFILES {
            for extraram in [false, true] {
                let asm = build(src, profile, extraram).asm;
                assert!(
                    uses_runtime_for_stack(&asm),
                    "{name}: the NEXT must dispatch through the runtime FOR \
                     stack, which is the only lowering that can raise \
                     ?NEXT WITHOUT FOR ({profile:?}, extraram={extraram}):\n{asm}"
                );
                assert!(
                    asm.contains("__NEXT_NO_FOR:"),
                    "{name}: ?NEXT WITHOUT FOR trap must be present \
                     ({profile:?}, extraram={extraram}):\n{asm}"
                );
            }
        }
    }
}

/// `__STATE_RESET` is what `CLR` calls.
#[test]
fn state_reset_zeroes_the_for_stack_pointer() {
    for profile in PROFILES {
        for extraram in [false, true] {
            let asm = build(CLR_IN_BODY, profile, extraram).asm;
            let reset = asm
                .split("__STATE_RESET:")
                .nth(1)
                .unwrap_or_else(|| panic!("__STATE_RESET emitted:\n{asm}"));
            let body = reset.split("\n    RTS").next().unwrap();
            assert!(
                body.contains("STA __FOR_STACK_PTR"),
                "CLR must abandon every open FOR frame \
                 ({profile:?}, extraram={extraram}):\n{body}"
            );
        }
    }
}

#[test]
fn a_reset_inside_a_loop_stops_after_one_pass() {
    for profile in PROFILES {
        for extraram in [false, true] {
            let (clean, insns, out) = run(&build(CLR_IN_BODY, profile, extraram).prg_bytes);
            assert_eq!(
                out.trim_end(),
                " 1",
                "ROM prints ` 1 ` and stops with ?NEXT WITHOUT FOR IN 40 \
                 ({profile:?}, extraram={extraram})"
            );
            assert!(
                !clean,
                "the program must stop on the error, not run to its END \
                 ({profile:?}, extraram={extraram})"
            );
            assert!(
                insns < common::MAX_INSNS / 100,
                "must not spin: took {insns} instructions \
                 ({profile:?}, extraram={extraram})"
            );
        }
    }
}

#[test]
fn a_reset_reached_by_goto_stops_at_the_next() {
    for profile in PROFILES {
        for extraram in [false, true] {
            let (clean, insns, out) = run(&build(CLR_VIA_GOTO, profile, extraram).prg_bytes);
            assert_eq!(
                out.trim_end(),
                " 1  0",
                "ROM prints ` 1  0 ` then ?NEXT WITHOUT FOR IN 50 \
                 ({profile:?}, extraram={extraram})"
            );
            assert!(
                !clean,
                "stops on the error ({profile:?}, extraram={extraram})"
            );
            assert!(
                insns < common::MAX_INSNS / 100,
                "must not spin: {insns} instructions ({profile:?}, extraram={extraram})"
            );
        }
    }
}

#[test]
fn a_reset_inside_a_subroutine_kills_the_gosub_frame() {
    const SRC: &str = "10 C=0\n\
                       20 FOR I=1 TO 3\n\
                       30 GOSUB 100\n\
                       40 NEXT\n\
                       50 PRINT C;I\n\
                       60 END\n\
                       100 C=C+1\n\
                       110 CLR\n\
                       120 RETURN\n";
    for profile in PROFILES {
        for extraram in [false, true] {
            let built = build(SRC, profile, extraram);
            let reset = built
                .asm
                .split("__STATE_RESET:")
                .nth(1)
                .unwrap_or_else(|| panic!("__STATE_RESET emitted:\n{}", built.asm));
            let body = reset.split("\n    RTS").next().unwrap();
            assert!(
                body.contains("STA __GOSUB_DEPTH"),
                "CLR abandons pending GOSUB frames \
                 ({profile:?}, extraram={extraram}):\n{body}"
            );
            let (clean, insns, out) = run(&built.prg_bytes);
            assert_eq!(
                out.trim_end(),
                "",
                "ROM prints nothing and stops with ?RETURN WITHOUT GOSUB IN 120 \
                 ({profile:?}, extraram={extraram})"
            );
            assert!(
                !clean,
                "stops on the error ({profile:?}, extraram={extraram})"
            );
            assert!(
                insns < common::MAX_INSNS / 100,
                "must not spin: {insns} instructions ({profile:?}, extraram={extraram})"
            );
        }
    }
}

#[test]
fn a_reset_before_the_loop_keeps_the_inline_lowering() {
    const SRC: &str = "10 CLR\n20 FOR I=1 TO 3:PRINT I;:NEXT\n30 PRINT \"=\";I\n";
    for profile in PROFILES {
        for extraram in [false, true] {
            let built = build(SRC, profile, extraram);
            assert!(
                !uses_runtime_for_stack(&built.asm),
                "a CLR ahead of the loop must not force the runtime FOR stack \
                 ({profile:?}, extraram={extraram}):\n{}",
                built.asm
            );
            let (clean, _, out) = run(&built.prg_bytes);
            assert!(
                clean,
                "runs to completion ({profile:?}, extraram={extraram})"
            );
            assert_eq!(
                out.trim_end(),
                " 1  2  3 = 4",
                "({profile:?}, extraram={extraram})"
            );
        }
    }
}

#[test]
fn a_reset_after_the_loop_keeps_the_inline_lowering() {
    const SRC: &str = "10 FOR I=1 TO 3:S=S+I:NEXT\n20 PRINT S;I\n30 CLR\n40 PRINT S;I\n";
    for profile in PROFILES {
        for extraram in [false, true] {
            let built = build(SRC, profile, extraram);
            assert!(
                !uses_runtime_for_stack(&built.asm),
                "a CLR after the loop must not force the runtime FOR stack \
                 ({profile:?}, extraram={extraram}):\n{}",
                built.asm
            );
            let (clean, _, out) = run(&built.prg_bytes);
            assert!(
                clean,
                "runs to completion ({profile:?}, extraram={extraram})"
            );
            assert_eq!(
                out.split_whitespace().collect::<Vec<_>>(),
                vec!["6", "4", "0", "0"],
                "S=6 I=4 before CLR, both 0 after ({profile:?}, extraram={extraram})"
            );
        }
    }
}

#[test]
fn a_reset_between_two_loops_keeps_both_inline() {
    const SRC: &str = "10 N=5\n\
                       20 FOR I=0 TO N:S=S+I:NEXT\n\
                       30 PRINT S;I\n\
                       40 CLR\n\
                       50 N=3\n\
                       60 FOR I=0 TO N:T=T+I:NEXT\n\
                       70 PRINT T;I;S\n";
    for profile in PROFILES {
        for extraram in [false, true] {
            let built = build(SRC, profile, extraram);
            assert!(
                !uses_runtime_for_stack(&built.asm),
                "neither loop needs the runtime FOR stack \
                 ({profile:?}, extraram={extraram}):\n{}",
                built.asm
            );
            let (clean, _, out) = run(&built.prg_bytes);
            assert!(
                clean,
                "runs to completion ({profile:?}, extraram={extraram})"
            );
            assert_eq!(
                out.split_whitespace().collect::<Vec<_>>(),
                vec!["15", "6", "6", "4", "0"],
                "({profile:?}, extraram={extraram})"
            );
        }
    }
}

#[test]
fn corpus_dim_dyn_clr_keeps_both_loops_inline() {
    let prg = std::fs::read(common::repo_path("test_corpus/dim_dyn_clr.prg")).expect("read prg");
    let golden =
        std::fs::read_to_string(common::repo_path("test_corpus/emu_golden/dim_dyn_clr.out"))
            .expect("read golden");
    for profile in PROFILES {
        for extraram in [false, true] {
            let built = compile_with_options(
                &prg,
                CompileOptions {
                    profile,
                    extraram,
                    force_extraram_off: !extraram,
                    ..CompileOptions::default()
                },
            )
            .expect("compile");
            assert!(
                !uses_runtime_for_stack(&built.asm),
                "dim_dyn_clr's loops are both fully re-entered after the CLR \
                 and must stay inline ({profile:?}, extraram={extraram})"
            );
            let out = common::run_clean(&built.prg_bytes)
                .unwrap_or_else(|| panic!("ran clean ({profile:?}, extraram={extraram})"));
            assert_eq!(
                out.trim_end(),
                golden.trim_end(),
                "({profile:?}, extraram={extraram})"
            );
        }
    }
}

#[test]
fn a_reset_ahead_of_the_header_on_the_same_line_is_harmless() {
    for (name, src, expect) in [
        (
            "one-liner",
            "10 CLR:FOR I=1 TO 3:PRINT I;:NEXT I\n20 PRINT \"=\";I\n",
            " 1  2  3 = 4",
        ),
        (
            "header on the reset's line, NEXT on the next",
            "10 CLR:FOR I=1 TO 3\n20 PRINT I;\n30 NEXT I\n40 PRINT \"=\";I\n",
            " 1  2  3 = 4",
        ),
    ] {
        for profile in PROFILES {
            for extraram in [false, true] {
                let built = build(src, profile, extraram);
                assert!(
                    !uses_runtime_for_stack(&built.asm),
                    "{name}: a reset before the header must not force the \
                     runtime FOR stack ({profile:?}, extraram={extraram}):\n{}",
                    built.asm
                );
                let (clean, _, out) = run(&built.prg_bytes);
                assert!(
                    clean,
                    "{name}: must run to completion \
                     ({profile:?}, extraram={extraram}); output {out:?}"
                );
                assert_eq!(
                    out.trim_end(),
                    expect,
                    "{name} ({profile:?}, extraram={extraram})"
                );
            }
        }
    }
}

#[test]
fn a_reset_after_the_header_on_the_same_line_still_stops_the_loop() {
    for profile in PROFILES {
        for extraram in [false, true] {
            let built = build(CLR_SAME_LINE, profile, extraram);
            assert!(
                uses_runtime_for_stack(&built.asm),
                "({profile:?}, extraram={extraram}):\n{}",
                built.asm
            );
            let (clean, insns, out) = run(&built.prg_bytes);
            assert_eq!(
                out.trim_end(),
                " 1",
                "one pass then ?NEXT WITHOUT FOR ({profile:?}, extraram={extraram})"
            );
            assert!(!clean, "({profile:?}, extraram={extraram})");
            assert!(
                insns < common::MAX_INSNS / 100,
                "must not spin: {insns} ({profile:?}, extraram={extraram})"
            );
        }
    }
}

/// A program with no reset at all must be untouched by any of this.
#[test]
fn a_program_without_a_reset_is_unaffected() {
    const SRC: &str = "10 FOR I=1 TO 3:PRINT I;:NEXT\n20 PRINT \"=\";I\n";
    for profile in PROFILES {
        let built = build(SRC, profile, false);
        assert!(
            !uses_runtime_for_stack(&built.asm),
            "({profile:?}):\n{}",
            built.asm
        );
        let (clean, _, out) = run(&built.prg_bytes);
        assert!(clean, "({profile:?})");
        assert_eq!(out.trim_end(), " 1  2  3 = 4", "({profile:?})");
    }
}

// Layout: the FOR slots have to be inside the wipe

/// Byte offset of the line consisting solely of `label`.
fn label_pos(asm: &str, label: &str) -> Option<usize> {
    let mut off = 0usize;
    for line in asm.lines() {
        if line.trim() == label {
            return Some(off);
        }
        off += line.len() + 1;
    }
    None
}

/// Labels defined with a body between the `__VARS_START`/`__VARS_END` anchors.
fn labels_inside_anchors(asm: &str) -> Vec<String> {
    let start = label_pos(asm, "__VARS_START:").expect("__VARS_START defined");
    let end = label_pos(asm, "__VARS_END:").expect("__VARS_END defined");
    assert!(start < end, "__VARS_START must precede __VARS_END");
    asm[start..end]
        .lines()
        .skip(1)
        .map(str::trim)
        .filter(|t| t.ends_with(':') && !t.starts_with(';'))
        .map(|t| t.trim_end_matches(':').to_string())
        .collect()
}

/// Every `FE_`/`FI_`/`FU_` label the program *defines* (ZP-promoted counters are `EQU`d, so
/// they correctly don't appear).
fn defined_for_slots(asm: &str) -> Vec<String> {
    asm.lines()
        .map(str::trim)
        .filter(|t| t.ends_with(':'))
        .map(|t| t.trim_end_matches(':').to_string())
        .filter(|l| {
            (l.starts_with("FE_") || l.starts_with("FI_") || l.starts_with("FU_"))
                && l[3..].chars().all(|c| c.is_ascii_digit())
        })
        .collect()
}

/// The limit slot is where the hang came from: `CLR` zeroed the counter and left the limit
/// standing.
#[test]
fn for_limit_and_counter_slots_sit_inside_the_vars_anchors() {
    // A float loop with a runtime limit, which is what allocates FE_.
    const SRC: &str = "10 N=3.5\n\
                       20 FOR I=1 TO N STEP 0.5\n\
                       30 S=S+I\n\
                       40 NEXT\n\
                       50 PRINT S;I\n\
                       60 CLR\n\
                       70 PRINT S;I\n";
    for profile in PROFILES {
        for extraram in [false, true] {
            let asm = build(SRC, profile, extraram).asm;
            let slots = defined_for_slots(&asm);
            assert!(
                !slots.is_empty(),
                "this program must allocate a FOR limit slot \
                 ({profile:?}, extraram={extraram}):\n{asm}"
            );
            let inside = labels_inside_anchors(&asm);
            for slot in &slots {
                assert!(
                    inside.contains(slot),
                    "{slot} is outside __VARS_START..__VARS_END, so CLR would \
                     leave it standing ({profile:?}, extraram={extraram}):\n{asm}"
                );
            }
        }
    }
}

#[test]
fn the_wipe_is_still_one_memzero_over_the_label_difference() {
    const SRC: &str =
        "10 N=3.5\n20 FOR I=1 TO N STEP 0.5\n30 S=S+I\n40 NEXT\n50 CLR\n60 PRINT S;I\n";
    for profile in PROFILES {
        for extraram in [false, true] {
            let asm = build(SRC, profile, extraram).asm;
            let reset = asm
                .split("__STATE_RESET:")
                .nth(1)
                .unwrap_or_else(|| panic!("__STATE_RESET emitted:\n{asm}"));
            let body = reset.split("\n    RTS").next().unwrap();
            assert!(
                body.contains("LDA #<(__VARS_END-__VARS_START)"),
                "count is still the live label difference \
                 ({profile:?}, extraram={extraram}):\n{body}"
            );
            assert_eq!(
                body.matches("LDA #<__VARS_START").count(),
                1,
                "exactly one bulk wipe ({profile:?}, extraram={extraram}):\n{body}"
            );
        }
    }
}
