mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

fn build(src: &str, profile: Profile, extraram: bool) -> yabcompiler_core::Compiled {
    let prg = tokenize_program(src).expect("tokenize");
    let mut c = compile_with_options(
        &prg,
        CompileOptions {
            profile,
            extraram,
            force_extraram_off: !extraram,
            ..CompileOptions::default()
        },
    )
    .expect("compile");
    c.asm = common::expand_call_entries(&c.asm);
    c
}

fn run(prg: &[u8]) -> (bool, String) {
    let r = emu64::run_prg_to_end(prg, common::MAX_INSNS).expect("emu64 run");
    (r.clean_exit, common::petscii_to_string(&r.output))
}

/// `n` nested `FOR`s on their own lines starting at 10, then `body` on line `10*(n+1)`, then
/// the matching bare `NEXT`s and a final marker.
fn nest(n: usize, body: &str) -> String {
    let names = ["A", "B", "C", "D", "E", "F", "G", "H", "J", "K", "L", "M"];
    let mut s = String::new();
    let mut line = 10;
    for name in names.iter().take(n) {
        s.push_str(&format!("{line} FOR {name}=1 TO 1\n"));
        line += 10;
    }
    if !body.is_empty() {
        s.push_str(&format!("{line} {body}\n"));
        line += 10;
    }
    for name in names.iter().take(n).rev() {
        s.push_str(&format!("{line} NEXT {name}\n"));
        line += 10;
    }
    s.push_str(&format!("{line} PRINT \"OK\"\n"));
    s
}

#[test]
fn nine_frames_evaluate_and_ten_do_not() {
    for profile in PROFILES {
        for extraram in [false, true] {
            let (clean, out) = run(&build(&nest(9, "PRINT \"IN\""), profile, extraram).prg_bytes);
            assert!(
                clean,
                "nine FOR frames leave room for an expression \
                 ({profile:?}, extraram={extraram}); output {out:?}"
            );
            assert_eq!(
                out.split_whitespace().collect::<Vec<_>>(),
                vec!["IN", "OK"],
                "({profile:?}, extraram={extraram})"
            );

            let (clean, out) = run(&build(&nest(10, "PRINT \"IN\""), profile, extraram).prg_bytes);
            assert!(
                !clean,
                "ten FOR frames must stop the program \
                 ({profile:?}, extraram={extraram}); output {out:?}"
            );
            assert_eq!(
                out.trim_end(),
                "",
                "the PRINT never runs ({profile:?}, extraram={extraram})"
            );
        }
    }
}

#[test]
fn ten_frames_unwound_by_bare_nexts_still_run() {
    for profile in PROFILES {
        for extraram in [false, true] {
            let (clean, out) = run(&build(&nest(10, ""), profile, extraram).prg_bytes);
            assert!(
                clean,
                "ten frames and no expression is valid on the ROM \
                 ({profile:?}, extraram={extraram}); output {out:?}"
            );
            assert_eq!(out.trim_end(), "OK", "({profile:?}, extraram={extraram})");
        }
    }
}

#[test]
fn the_eleventh_for_header_is_itself_out_of_memory() {
    for profile in PROFILES {
        let built = build(&nest(11, ""), profile, false);
        let (clean, out) = run(&built.prg_bytes);
        assert!(!clean, "({profile:?}); output {out:?}");
        assert_eq!(out.trim_end(), "", "({profile:?})");

        assert!(
            built.asm.contains("LDA #$6E\n    STA $39"),
            "CURLIN must be stamped with 110 before the trap ({profile:?}):\n{}",
            built.asm
        );
    }
}

#[test]
fn a_normal_nest_pays_nothing() {
    const SRC: &str = "10 T=0\n\
                       20 FOR I=1 TO 3\n\
                       30 FOR J=1 TO 3\n\
                       40 FOR K=1 TO 3\n\
                       50 T=T+1\n\
                       60 NEXT K\n\
                       70 NEXT J\n\
                       80 NEXT I\n\
                       90 PRINT \"T\";T\n";
    for profile in PROFILES {
        for extraram in [false, true] {
            let built = build(SRC, profile, extraram);
            assert!(
                !built.asm.contains("__FOR_OOM"),
                "a three-deep nest must not pull in the trap \
                 ({profile:?}, extraram={extraram}):\n{}",
                built.asm
            );
            let (clean, out) = run(&built.prg_bytes);
            assert!(clean, "({profile:?}, extraram={extraram}); output {out:?}");
            assert_eq!(out.trim_end(), "T 27", "({profile:?}, extraram={extraram})");
        }
    }
}

#[test]
fn sequential_loops_never_reach_the_ceiling() {
    let mut src = String::from("10 T=0\n");
    let mut line = 20;
    for _ in 0..12 {
        src.push_str(&format!("{line} FOR I=1 TO 2:T=T+1:NEXT\n"));
        line += 10;
    }
    src.push_str(&format!("{line} PRINT \"T\";T\n"));
    for profile in PROFILES {
        let built = build(&src, profile, false);
        assert!(
            !built.asm.contains("__FOR_OOM"),
            "twelve sequential loops are one frame deep ({profile:?}):\n{}",
            built.asm
        );
        let (clean, out) = run(&built.prg_bytes);
        assert!(clean, "({profile:?}); output {out:?}");
        assert_eq!(out.trim_end(), "T 24", "({profile:?})");
    }
}

#[test]
fn one_variable_reused_does_not_stack_frames() {
    let mut src = String::new();
    let mut line = 10;
    for _ in 0..12 {
        src.push_str(&format!("{line} FOR I=1 TO 1\n"));
        line += 10;
    }
    src.push_str(&format!("{line} PRINT \"IN\"\n"));
    line += 10;
    src.push_str(&format!("{line} NEXT I\n"));
    line += 10;
    src.push_str(&format!("{line} PRINT \"OK\"\n"));
    for profile in PROFILES {
        let (clean, out) = run(&build(&src, profile, false).prg_bytes);
        assert!(clean, "({profile:?}); output {out:?}");
        assert_eq!(
            out.split_whitespace().collect::<Vec<_>>(),
            vec!["IN", "OK"],
            "({profile:?})"
        );
    }
}

#[test]
fn the_trap_is_a_compile_time_jump_to_the_error_vector() {
    let asm = build(&nest(10, "PRINT \"IN\""), Profile::Default, false).asm;
    let stamp_and_raise = format!(
        "    LDA #$6E\n    STA $39\n    LDX #${:02X}\n",
        yabcompiler_core::runtime::ERR_OUT_OF_MEMORY
    );
    assert!(
        asm.contains(&stamp_and_raise),
        "line 110 must be stamped and then raise ?OUT OF MEMORY:\n{asm}"
    );
    assert!(
        !asm.contains("__FOR_OOM"),
        "no runtime trap block is needed for a compile-time verdict:\n{asm}"
    );
    assert_eq!(
        asm.matches(&format!(
            "LDX #${:02X}",
            yabcompiler_core::runtime::ERR_OUT_OF_MEMORY
        ))
        .count(),
        1,
        "one trap, on the first statement that cannot evaluate:\n{asm}"
    );
}

/// Determinism: the trap must not depend on anything that varies between compiles.
#[test]
fn the_trap_compiles_deterministically() {
    let src = nest(10, "PRINT \"IN\"");
    for profile in PROFILES {
        let a = build(&src, profile, false);
        let b = build(&src, profile, false);
        assert_eq!(a.prg_bytes, b.prg_bytes, "({profile:?})");
    }
}

// The runtime FOR-stack half: bytes, not entries
const LEAKY: &str = "1 GOTO 30\n\
                     20 GOTO 40\n\
                     30 FOR I=1 TO 3\n\
                     40 C=C+1:IF C>9 THEN PRINT \"Z\":END\n\
                     42 PRINT I;:ON (I AND 1)+1 GOSUB 50,60\n\
                     50 NEXT I:GOTO 20\n\
                     60 FOR K=1 TO 3:GOTO 42\n";

#[test]
fn a_leaking_frame_and_marker_pair_stops_on_bytes_not_on_entries() {
    for profile in PROFILES {
        for extraram in [false, true] {
            let built = build(LEAKY, profile, extraram);
            assert!(
                built.asm.contains("__FOR_BYTES_CHK"),
                "the byte budget must be checked on the runtime push path \
                 ({profile:?}, extraram={extraram}):\n{}",
                built.asm
            );
            let (clean, out) = run(&built.prg_bytes);
            assert!(!clean, "({profile:?}, extraram={extraram}); output {out:?}");
            assert_eq!(
                out.split_whitespace().collect::<Vec<_>>(),
                vec!["1", "1", "1", "1", "1", "1", "1"],
                "the ROM prints seven values then ?OUT OF MEMORY IN 42 \
                 ({profile:?}, extraram={extraram})"
            );
        }
    }
}

#[test]
fn a_shallow_runtime_stack_is_untouched() {
    const SRC: &str = "10 FOR I=1 TO 3\n\
                       20 GOSUB 100\n\
                       30 NEXT I\n\
                       40 PRINT \"T\";T\n\
                       50 END\n\
                       100 FOR J=1 TO 2:T=T+1:NEXT J\n\
                       110 GOTO 120\n\
                       120 RETURN\n";
    for profile in PROFILES {
        for extraram in [false, true] {
            let (clean, out) = run(&build(SRC, profile, extraram).prg_bytes);
            assert!(clean, "({profile:?}, extraram={extraram}); output {out:?}");
            assert_eq!(out.trim_end(), "T 6", "({profile:?}, extraram={extraram})");
        }
    }
}

#[test]
fn the_static_for_fast_path_never_sees_the_byte_check() {
    const SRC: &str = "10 T=0\n\
                       20 FOR I=1 TO 20\n\
                       30 FOR J=1 TO 20\n\
                       40 T=T+1\n\
                       50 NEXT J\n\
                       60 NEXT I\n\
                       70 PRINT \"T\";T\n";
    for profile in PROFILES {
        for extraram in [false, true] {
            let built = build(SRC, profile, extraram);
            assert!(
                !built.asm.contains("__FOR_BYTES"),
                "no runtime FOR-stack means no byte check \
                 ({profile:?}, extraram={extraram}):\n{}",
                built.asm
            );
            assert!(
                !built.asm.contains("__FOR_STACK_PTR"),
                "({profile:?}, extraram={extraram}):\n{}",
                built.asm
            );
            let (clean, out) = run(&built.prg_bytes);
            assert!(clean, "({profile:?}, extraram={extraram})");
            assert_eq!(out.trim_end(), "T 400", "({profile:?}, extraram={extraram})");
        }
    }
}

#[test]
fn the_byte_check_is_one_shared_helper() {
    let asm = build(LEAKY, Profile::Default, false).asm;
    assert_eq!(
        asm.matches("__FOR_BYTES_CHK:").count(),
        1,
        "one shared body:\n{asm}"
    );
    assert_eq!(
        asm.matches("__FOR_BYTES_ACC:").count(),
        1,
        "one scratch byte:\n{asm}"
    );
    assert!(
        !asm.contains("__FOR_BYTES_CHK_MARKER") && !asm.contains("__FOR_BYTES_CHK_FRAME"),
        "no per-kind entry point survives:\n{asm}"
    );
    // Both push sites reach the same label.
    assert!(
        asm.matches("JSR __FOR_BYTES_CHK\n").count() >= 2,
        "the GOSUB marker push and the FOR frame push share it:\n{asm}"
    );
}

/// `n` nested `FOR`s, a `PRINT`, then a `CLR`, then the matching named `NEXT`s.
fn clr_nest(n: usize) -> String {
    let names = ["A", "B", "C", "D", "E", "F", "G", "H", "J", "K", "L", "M"];
    let mut s = String::new();
    let mut line = 10;
    for name in names.iter().take(n) {
        s.push_str(&format!("{line} FOR {name}=1 TO 1\n"));
        line += 10;
    }
    s.push_str(&format!("{line} PRINT \"IN\"\n"));
    line += 10;
    s.push_str(&format!("{line} CLR\n"));
    line += 10;
    for name in names.iter().take(n).rev() {
        s.push_str(&format!("{line} NEXT {name}\n"));
        line += 10;
    }
    s.push_str(&format!("{line} PRINT \"OK\"\n"));
    s
}

#[test]
fn ten_runtime_for_frames_fail_on_the_print_and_not_on_the_tenth_push() {
    let src = clr_nest(10);
    for profile in PROFILES {
        for extraram in [false, true] {
            let built = build(&src, profile, extraram);

            assert!(
                built.asm.contains("__FOR_BYTES_CHK"),
                "CLR must push this program onto the runtime FOR-stack \
                 ({profile:?}, extraram={extraram}):\n{}",
                built.asm
            );
            let r = emu64::run_prg_to_end(&built.prg_bytes, common::MAX_INSNS).expect("run");
            assert!(!r.clean_exit, "({profile:?}, extraram={extraram})");
            assert_eq!(
                common::petscii_to_string(&r.output).trim_end(),
                "",
                "line 110 never runs ({profile:?}, extraram={extraram})"
            );
            assert_eq!(
                r.error_banner().as_deref(),
                Some("?OUT OF MEMORY  ERROR IN 110"),
                "the ROM reports line 110, not the tenth FOR's line 100 \
                 ({profile:?}, extraram={extraram})"
            );
        }
    }
}

#[test]
fn nine_runtime_for_frames_are_stopped_by_the_clr_and_not_by_the_budget() {
    let src = clr_nest(9);
    for profile in PROFILES {
        for extraram in [false, true] {
            let built = build(&src, profile, extraram);
            let r = emu64::run_prg_to_end(&built.prg_bytes, common::MAX_INSNS).expect("run");
            assert!(!r.clean_exit, "({profile:?}, extraram={extraram})");
            assert_eq!(
                common::petscii_to_string(&r.output).trim_end(),
                "IN",
                "nine frames still evaluate ({profile:?}, extraram={extraram})"
            );
            assert_eq!(
                r.error_banner().as_deref(),
                Some("?NEXT WITHOUT FOR  ERROR IN 120"),
                "the CLR, not the budget, is what stops this one \
                 ({profile:?}, extraram={extraram})"
            );
        }
    }
}

const CAP: u8 = 168;

fn emitted_floors(asm: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut lines = asm.lines();
    while let Some(l) = lines.next() {
        if l.trim() != "JSR __FOR_BYTES_CHK" {
            continue;
        }
        match lines.next().unwrap_or("").trim().strip_prefix("CMP #$") {
            Some(hex) => out.push(u8::from_str_radix(hex, 16).expect("hex")),
            None => out.push(CAP),
        }
    }
    out
}

fn spelled_out_floors(asm: &str) -> Vec<u8> {
    asm.split("    JSR __FOR_BYTES_CHK\n    CMP #$")
        .skip(1)
        .map(|rest| u8::from_str_radix(&rest[..2], 16).expect("hex"))
        .collect()
}

fn g_family(selector: &str, prelude: &str) -> String {
    format!(
        "{prelude}1 GOTO 30\n\
         10 NEXT I:GOTO 20\n\
         20 GOTO 40\n\
         30 FOR I=1 TO 3\n\
         40 C=C+1:IF C>9 THEN PRINT \"Z\":END\n\
         42 PRINT I;:ON {selector} GOSUB 10,60\n\
         60 FOR K=1 TO 3:GOTO 42\n"
    )
}

#[test]
fn the_emitted_floor_is_the_rom_measured_one_for_each_expression() {

    let cases: [(&str, &str, u8); 3] = [
        ("(I AND 1)+1", "", 159),
        ("FN D(I)", "0 DEF FN D(X)=(X AND 1)+1\n", 143),
        ("I-3*INT(I/3)+1", "", 132),
    ];
    for (sel, prelude, want) in cases {
        for profile in PROFILES {
            let asm = build(&g_family(sel, prelude), profile, false).asm;
            let floors = emitted_floors(&asm);
            assert!(
                floors.contains(&want),
                "ON {sel} must be guarded at {want} ({profile:?}); got {floors:?}"
            );
        }
    }
}

/// End to end: the three variants stop after the number of iterations the ROM stops after, and
/// they are *different* numbers.
#[test]
fn each_on_expression_stops_where_the_rom_stops() {
    let cases: [(&str, &str, usize); 3] = [
        ("(I AND 1)+1", "", 7),
        ("FN D(I)", "0 DEF FN D(X)=(X AND 1)+1\n", 6),
        ("I-3*INT(I/3)+1", "", 6),
    ];
    for (sel, prelude, want) in cases {
        for profile in PROFILES {
            let built = build(&g_family(sel, prelude), profile, false);
            let r = emu64::run_prg_to_end(&built.prg_bytes, common::MAX_INSNS).expect("run");
            let out = common::petscii_to_string(&r.output);
            assert!(!r.clean_exit, "ON {sel} must stop ({profile:?}): {out:?}");
            assert_eq!(
                r.error_banner().as_deref(),
                Some("?OUT OF MEMORY  ERROR IN 42"),
                "ON {sel} ({profile:?})"
            );
            assert_eq!(
                out.split_whitespace().count(),
                want,
                "ON {sel} must print {want} values ({profile:?}); got {out:?}"
            );
        }
    }
}

#[test]
fn a_cheap_selector_keeps_the_old_boundary() {
    for profile in PROFILES {
        let built = build(&g_family("(I AND 1)+1", ""), profile, false);
        let r = emu64::run_prg_to_end(&built.prg_bytes, common::MAX_INSNS).expect("run");
        assert_eq!(
            common::petscii_to_string(&r.output).split_whitespace().count(),
            7,
            "({profile:?})"
        );
    }
}

#[test]
fn no_site_is_guarded_later_than_the_validated_constant() {
    let leak = "10 C=0\n\
                20 FOR I=1 TO 3\n\
                30 C=C+1\n\
                40 GOSUB 100\n\
                50 NEXT I\n\
                60 PRINT\"E\";C:END\n\
                100 PRINT\"X\";\n\
                110 GOTO 30\n";
    for profile in PROFILES {
        for src in [&g_family("(I AND 1)+1", ""), &leak.to_string()] {
            let asm = build(src, profile, false).asm;
            for f in emitted_floors(&asm) {
                assert!(
                    f <= 168,
                    "a site is guarded at {f}, later than the 168 the corpus \
                     was validated against ({profile:?})"
                );
            }
        }
    }
}

#[test]
fn a_program_that_cannot_exhaust_page_one_pays_nothing() {
    const SRC: &str = "10 FOR I=1 TO 3\n\
                       20 GOSUB 100\n\
                       30 NEXT I\n\
                       40 PRINT \"OK\":END\n\
                       100 PRINT \"S\";:RETURN\n";
    for profile in PROFILES {
        for extraram in [false, true] {
            let built = build(SRC, profile, extraram);
            assert!(
                !built.asm.contains("__FOR_BYTES_CHK"),
                "a bounded GOSUB must not pull in the byte walk \
                 ({profile:?}, extraram={extraram}):\n{}",
                built.asm
            );
            let r = emu64::run_prg_to_end(&built.prg_bytes, common::MAX_INSNS).expect("run");
            assert!(r.clean_exit, "({profile:?}, extraram={extraram})");
            assert_eq!(
                common::petscii_to_string(&r.output).trim_end(),
                "SSSOK",
                "({profile:?}, extraram={extraram})"
            );
        }
    }
}

#[test]
fn the_cap_compare_lives_in_the_helper_and_not_at_the_call_site() {
    for profile in PROFILES {
        let asm = build(LEAKY, profile, false).asm;

        let floors = emitted_floors(&asm);
        assert!(
            floors.contains(&CAP) && floors.contains(&159),
            "this program must have both kinds of site ({profile:?}); \
             got {floors:?}"
        );

        assert_eq!(
            spelled_out_floors(&asm),
            vec![159],
            "only the sub-cap floors may spell themselves out, once each \
             ({profile:?}):\n{asm}"
        );

        assert!(
            asm.contains(
                "__FOR_BYTES_TEST:\n    \
                 LDA __FOR_BYTES_ACC\n    \
                 CMP #$A8\n    \
                 BCS __FOR_OOM\n    \
                 RTS\n"
            ),
            "the walk must apply the cap before returning the total \
             ({profile:?}):\n{asm}"
        );
    }
}

#[test]
fn a_gosub_marker_push_is_one_call_not_an_inline_sequence() {
    for profile in PROFILES {
        let asm = build(LEAKY, profile, false).asm;

        assert_eq!(
            asm.matches("    JSR __FOR_MARK_159\n").count(),
            2,
            "both markers must call the shared helper ({profile:?}):\n{asm}"
        );
        assert_eq!(
            asm.matches("__FOR_MARK_159:\n").count(),
            1,
            "exactly one body for the floor ({profile:?}):\n{asm}"
        );

        assert!(
            asm.contains(
                "__FOR_MARK_159:\n    \
                 JSR __FOR_BYTES_CHK\n    \
                 CMP #$9F\n    \
                 BCS __FOR_OOM\n    \
                 LDX __FOR_STACK_PTR\n    \
                 CPX #$20\n    \
                 BCS __FOR_OOM\n    \
                 LDA #$00\n    \
                 STA __FOR_STACK,X\n    \
                 STA __FOR_STACK_VAR,X\n    \
                 INC __FOR_STACK_PTR\n    \
                 RTS\n"
            ),
            "the factored body must be the whole push ({profile:?}):\n{asm}"
        );
        // Nothing of the push is left behind at a call site.
        assert_eq!(
            asm.matches("    LDA #$00\n    STA __FOR_STACK,X\n").count(),
            1,
            "the marker store tail survives only in the helper \
             ({profile:?}):\n{asm}"
        );
    }
}

#[test]
fn factoring_the_marker_push_does_not_move_where_the_program_stops() {
    for profile in PROFILES {
        for extraram in [false, true] {
            let built = build(LEAKY, profile, extraram);
            let r = emu64::run_prg_to_end(&built.prg_bytes, common::MAX_INSNS).expect("run");
            assert_eq!(
                r.error_banner().as_deref(),
                Some("?OUT OF MEMORY  ERROR IN 42"),
                "({profile:?}, extraram={extraram})"
            );
            assert_eq!(
                common::petscii_to_string(&r.output)
                    .split_whitespace()
                    .count(),
                7,
                "({profile:?}, extraram={extraram})"
            );
        }
    }
}

#[test]
fn no_spelled_out_floor_is_above_the_cap() {
    let leak = "10 C=0\n\
                20 FOR I=1 TO 3\n\
                30 C=C+1\n\
                40 GOSUB 100\n\
                50 NEXT I\n\
                60 PRINT\"E\";C:END\n\
                100 PRINT\"X\";\n\
                110 GOTO 30\n";
    for profile in PROFILES {
        for src in [
            LEAKY.to_string(),
            leak.to_string(),
            g_family("FN D(I)", "0 DEF FN D(X)=(X AND 1)+1\n"),
            g_family("I-3*INT(I/3)+1", ""),
            clr_nest(10),
        ] {
            for f in spelled_out_floors(&build(&src, profile, false).asm) {
                assert!(
                    f < CAP,
                    "a site spells out {f}, which the helper's own {CAP} \
                     already covers — it should have been dropped ({profile:?})"
                );
            }
        }
    }
}

#[test]
fn a_dead_store_past_the_ceiling_still_raises() {
    for profile in PROFILES {
        for extraram in [false, true] {
            let built = build(&nest(10, "Z=1"), profile, extraram);
            let r = emu64::run_prg_to_end(&built.prg_bytes, common::MAX_INSNS).expect("run");
            let out = common::petscii_to_string(&r.output);
            assert!(
                !r.clean_exit,
                "({profile:?}, extraram={extraram}); output {out:?}"
            );
            assert_eq!(
                out.trim_end(),
                "",
                "nothing runs past line 110 ({profile:?}, extraram={extraram})"
            );
            assert_eq!(
                r.error_banner().as_deref(),
                Some("?OUT OF MEMORY  ERROR IN 110"),
                "({profile:?}, extraram={extraram})"
            );
        }
    }
}

#[test]
fn a_dead_string_store_past_the_ceiling_still_raises() {
    for profile in PROFILES {
        let built = build(&nest(10, "Z$=\"X\""), profile, false);
        let r = emu64::run_prg_to_end(&built.prg_bytes, common::MAX_INSNS).expect("run");
        assert!(!r.clean_exit, "({profile:?})");
        assert_eq!(
            r.error_banner().as_deref(),
            Some("?OUT OF MEMORY  ERROR IN 110"),
            "({profile:?})"
        );
    }
}

#[test]
fn a_dead_store_under_the_ceiling_is_still_eliminated() {
    for profile in PROFILES {
        for extraram in [false, true] {
            let built = build(&nest(9, "Z=1"), profile, extraram);
            assert!(
                !built.asm.contains("V_Z")
                    && !built.asm.contains("VI_Z")
                    && !built.asm.contains("__FOR_OOM"),
                "the dead store must still go, and nine frames must still \
                 pay nothing ({profile:?}, extraram={extraram}):\n{}",
                built.asm
            );
            let r = emu64::run_prg_to_end(&built.prg_bytes, common::MAX_INSNS).expect("run");
            assert!(r.clean_exit, "({profile:?}, extraram={extraram})");
            assert_eq!(
                common::petscii_to_string(&r.output).trim_end(),
                "OK",
                "({profile:?}, extraram={extraram})"
            );
        }
    }
}

#[test]
fn a_dead_store_outside_a_deep_nest_is_still_eliminated() {
    const SRC: &str = "10 FOR I=1 TO 3\n\
                       20 Z=1\n\
                       30 Q$=\"X\"\n\
                       40 NEXT I\n\
                       50 PRINT \"OK\"\n";
    for profile in PROFILES {
        let built = build(SRC, profile, false);
        assert!(
            !built.asm.contains("V_Z")
                && !built.asm.contains("VI_Z")
                && !built.asm.contains("V_Q$"),
            "({profile:?}):\n{}",
            built.asm
        );
        let r = emu64::run_prg_to_end(&built.prg_bytes, common::MAX_INSNS).expect("run");
        assert!(r.clean_exit, "({profile:?})");
        assert_eq!(
            common::petscii_to_string(&r.output).trim_end(),
            "OK",
            "({profile:?})"
        );
    }
}

#[test]
fn the_kept_store_is_only_the_traps_anchor() {
    let dead = build(&nest(10, "Z=1"), Profile::Default, false);
    let live = build(
        &nest(10, "Z=1").replace("PRINT \"OK\"", "PRINT \"OK\";Z"),
        Profile::Default,
        false,
    );
    for built in [&dead, &live] {
        let r = emu64::run_prg_to_end(&built.prg_bytes, common::MAX_INSNS).expect("run");
        assert_eq!(
            r.error_banner().as_deref(),
            Some("?OUT OF MEMORY  ERROR IN 110"),
            "{}",
            built.asm
        );
    }
    assert!(
        !dead.asm.contains("__FOR_OOM"),
        "the verdict is settled at compile time:\n{}",
        dead.asm
    );
}

const LEAK_UNDER_FOR: &str = "10 N=0\n\
                              20 FOR Q=1 TO 30000\n\
                              30 N=N+1:PRINT N\n\
                              40 GOSUB 100\n\
                              50 NEXT Q\n\
                              60 END\n\
                              100 GOTO 30\n";

#[test]
fn a_for_enclosing_the_recursion_is_paid_once_not_per_level() {
    for profile in PROFILES {
        let built = build(LEAK_UNDER_FOR, profile, false);
        let (clean, out) = run(&built.prg_bytes);
        assert!(!clean, "({profile:?}) must end in ?OUT OF MEMORY: {out:?}");
        let got: Vec<&str> = out.split_whitespace().collect();
        let want: Vec<String> = (1..=22).map(|n| n.to_string()).collect();
        assert_eq!(
            got, want,
            "the real ROM prints 1..22 and stops ({profile:?}); \
             charging the enclosing FOR per level stopped at 7"
        );
    }
}

#[test]
fn a_for_inside_the_recursion_is_still_paid_per_level() {
    const SRC: &str = "10 N=0\n\
                       20 N=N+1:PRINT N\n\
                       30 FOR Q=1 TO 30000\n\
                       40 GOSUB 100\n\
                       50 NEXT Q\n\
                       60 END\n\
                       100 GOTO 30\n";
    for profile in PROFILES {
        let built = build(SRC, profile, false);
        let (clean, out) = run(&built.prg_bytes);
        assert!(!clean, "({profile:?}) must end in ?OUT OF MEMORY: {out:?}");
        let printed = out.split_whitespace().count();
        assert!(
            printed <= 8,
            "a FOR re-entered by the cycle costs 18 bytes a level, so the \
             ceiling must stay near 167/25+1 = 7, got {printed} ({profile:?})"
        );
    }
}

#[test]
fn a_leak_with_no_for_still_stops_where_the_rom_does() {
    const SRC: &str = "10 N=0\n\
                       20 N=N+1:PRINT N\n\
                       30 GOSUB 100\n\
                       40 END\n\
                       100 GOTO 20\n";
    for profile in PROFILES {
        let built = build(SRC, profile, false);
        let (clean, out) = run(&built.prg_bytes);
        assert!(!clean, "({profile:?}) must end in ?OUT OF MEMORY: {out:?}");
        let got: Vec<&str> = out.split_whitespace().collect();
        let want: Vec<String> = (1..=24).map(|n| n.to_string()).collect();
        assert_eq!(got, want, "the real ROM prints 1..24 and stops ({profile:?})");
    }
}

#[test]
fn a_balanced_subroutine_called_from_a_loop_is_unguarded() {
    const SRC: &str = "10 FOR Q=1 TO 40\n\
                       20 GOSUB 100\n\
                       30 NEXT Q\n\
                       40 PRINT \"OK\";T\n\
                       50 END\n\
                       100 T=T+1:RETURN\n";
    for profile in PROFILES {
        let built = build(SRC, profile, false);
        let (clean, out) = run(&built.prg_bytes);
        assert!(clean, "({profile:?}) must run to its final RTS: {out:?}");
        assert!(
            out.contains("OK") && out.contains("40"),
            "the loop must complete all 40 calls ({profile:?}): {out:?}"
        );
    }
}
