

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
fn emitted_floors(asm: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut lines = asm.lines();
    while let Some(l) = lines.next() {
        if l.trim() != "JSR __FOR_BYTES_CHK" {
            continue;
        }
        let cmp = lines.next().unwrap_or("").trim().to_string();
        let hex = cmp
            .strip_prefix("CMP #$")
            .unwrap_or_else(|| panic!("byte check not followed by CMP: {cmp:?}\n{asm}"));
        out.push(u8::from_str_radix(hex, 16).expect("hex"));
    }
    out
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
