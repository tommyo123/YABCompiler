mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

fn build(src: &str, profile: Profile) -> yabcompiler_core::Compiled {
    let prg = tokenize_program(src).expect("tokenize");
    compile_with_options(
        &prg,
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .expect("compile")
}

fn run(prg: &[u8]) -> (bool, String) {
    let r = emu64::run_prg_to_end(prg, common::MAX_INSNS).expect("emu64 run");
    (r.clean_exit, common::petscii_to_string(&r.output))
}

fn banner(prg: &[u8]) -> Option<String> {
    emu64::run_prg_to_end(prg, common::MAX_INSNS)
        .expect("emu64 run")
        .error_banner()
}

fn check_sites(asm: &str) -> usize {
    let lines: Vec<&str> = asm.lines().map(str::trim).collect();
    let mut n = 0;
    for i in 0..lines.len() {
        if lines[i] == "LDA __GOSUB_DEPTH"
            && lines.get(i + 1).is_some_and(|l| l.starts_with("CMP #$"))
            && lines.get(i + 2) == Some(&"BCS __FOR_OOM")
            && (i == 0 || lines[i - 1] != "INC __GOSUB_DEPTH")
        {
            n += 1;
        }
    }
    n
}

/// `gosubmix/gm045` and its byte-identical twin `gm226`.
const LEAK_GOSUB: &str = "10 I=0\n\
                          20 I=I+1:GOSUB 100\n\
                          30 PRINT I;:GOTO 20\n\
                          100 GOTO 30\n";

#[test]
fn a_leaking_gosub_loop_stops_on_the_let_the_rom_stops_on() {
    for profile in PROFILES {
        let (clean, out) = run(&build(LEAK_GOSUB, profile).prg_bytes);
        assert!(!clean, "must raise ?OUT OF MEMORY ({profile:?}): {out:?}");
        let seen: Vec<&str> = out.split_whitespace().collect();
        assert_eq!(
            seen,
            (1..=24).map(|n| n.to_string()).collect::<Vec<_>>(),
            "the ROM prints 1..24 and dies on line 20's I=I+1, not on the \
             GOSUB one iteration earlier ({profile:?})"
        );
    }
}

#[test]
fn each_statement_carries_its_own_measured_floor() {
    for profile in PROFILES {
        let asm = build(LEAK_GOSUB, profile).asm;
        // ceil(166/7) = 24 for the LET, ceil(180/7) = 26 for the PRINT.
        assert!(
            asm.contains("    LDA __GOSUB_DEPTH\n    CMP #$18\n    BCS __FOR_OOM\n"),
            "the LET's floor of 166 is 24 records ({profile:?}):\n{asm}"
        );
        assert!(
            asm.contains("    LDA __GOSUB_DEPTH\n    CMP #$1A\n    BCS __FOR_OOM\n"),
            "the PRINT's floor of 180 is 26 records ({profile:?}):\n{asm}"
        );

        assert!(
            asm.contains(
                "__GOSUB_CHK:\n    INC __GOSUB_DEPTH\n    LDA __GOSUB_DEPTH\n    CMP #$1B\n"
            ),
            "the push guard uses GOSUB's own floor once the statements \
             carry theirs ({profile:?}):\n{asm}"
        );
    }
}

#[test]
fn plain_recursion_still_stops_at_twenty_three() {
    const SRC: &str = "10 D=0\n\
                       20 GOSUB 100\n\
                       30 END\n\
                       100 D=D+1\n\
                       110 PRINT D;\n\
                       120 GOSUB 100\n\
                       130 RETURN\n";
    for profile in PROFILES {
        let (clean, out) = run(&build(SRC, profile).prg_bytes);
        assert!(!clean, "must raise ?OUT OF MEMORY ({profile:?}): {out:?}");
        assert_eq!(
            out.split_whitespace().collect::<Vec<_>>(),
            (1..=23).map(|n| n.to_string()).collect::<Vec<_>>(),
            "raising the push guard to the ROM's own 179 must not let this \
             run a level further — the per-statement check on D=D+1 is what \
             catches it now ({profile:?})"
        );
    }
}

#[test]
fn bounded_recursion_under_the_gate_still_runs_to_completion() {
    const SRC: &str = "10 D=0\n\
                       20 GOSUB 100\n\
                       30 PRINT \"OK\";D\n\
                       40 END\n\
                       100 D=D+1\n\
                       110 IF D>=20 THEN RETURN\n\
                       120 GOSUB 100\n\
                       130 RETURN\n";
    for profile in PROFILES {
        let built = build(SRC, profile);
        assert!(
            check_sites(&built.asm) > 0,
            "this program is under the gate; if it stops being, the test \
             below proves nothing ({profile:?})"
        );
        let (clean, out) = run(&built.prg_bytes);
        assert!(
            clean,
            "20-deep recursion is valid on the ROM ({profile:?}): {out:?}"
        );
        assert_eq!(out.trim_end(), "OK 20", "({profile:?})");
    }
}

#[test]
fn sequential_gosubs_never_trip_the_floor() {
    let mut src = String::from("5 T=0\n");
    for i in 0..30 {
        src.push_str(&format!("{} GOSUB 900\n", 10 + i));
    }
    src.push_str("800 PRINT \"T\";T\n810 END\n900 T=T+1\n910 RETURN\n");
    for profile in PROFILES {
        let (clean, out) = run(&build(&src, profile).prg_bytes);
        assert!(clean, "({profile:?}): {out:?}");
        assert_eq!(out.trim_end(), "T 30", "({profile:?})");
    }
}

#[test]
fn a_gosub_only_line_reports_its_own_number() {
    for (src, want) in [
        (
            "10 PRINT\"A\";\n20 GOSUB 20\n",
            "?OUT OF MEMORY  ERROR IN 20",
        ),
        ("10 GOSUB 10\n", "?OUT OF MEMORY  ERROR IN 10"),
    ] {
        for profile in PROFILES {
            let built = build(src, profile);
            assert_eq!(
                banner(&built.prg_bytes).as_deref(),
                Some(want),
                "({profile:?}):\n{src}\n{}",
                built.asm
            );
        }
    }
}

#[test]
fn a_gated_program_with_conditionals_computes_the_same_answer() {
    const SRC: &str = "10 D=0:S=0\n\
                       20 GOSUB 100\n\
                       30 PRINT \"S\";S\n\
                       40 END\n\
                       100 D=D+1\n\
                       110 IF D>3 THEN S=S+100:RETURN\n\
                       120 ON D GOSUB 200,210,220\n\
                       130 GOSUB 100\n\
                       140 RETURN\n\
                       200 S=S+1:RETURN\n\
                       210 S=S+2:RETURN\n\
                       220 S=S+4:RETURN\n";
    for profile in PROFILES {
        let built = build(SRC, profile);
        assert!(
            check_sites(&built.asm) > 0,
            "must be under the gate for this to prove anything \
             ({profile:?}):\n{}",
            built.asm
        );
        let (clean, out) = run(&built.prg_bytes);
        assert!(clean, "({profile:?}): {out:?}");
        // 1 + 2 + 4 from the three ON GOSUB arms, then +100 at D=4.
        assert_eq!(out.trim_end(), "S 107", "({profile:?})");
    }
}

#[test]
fn a_single_for_anywhere_switches_the_gate_off() {
    // Same recursion as `LEAK_GOSUB`, with one live FOR loop in front of it.
    const WITH_FOR: &str = "5 FOR Q=1 TO 2:I=I+Q:NEXT\n\
                            10 I=0\n\
                            20 I=I+1:GOSUB 100\n\
                            30 PRINT I;:GOTO 20\n\
                            100 GOTO 30\n";
    for profile in PROFILES {
        let asm = build(WITH_FOR, profile).asm;
        assert_eq!(
            check_sites(&asm),
            0,
            "no per-statement check may be emitted once a FOR frame is \
             representable ({profile:?}):\n{asm}"
        );
    }
}

#[test]
fn a_straight_line_gosub_chain_gets_no_checks() {
    const SRC: &str = "10 GOSUB 100\n\
                       20 PRINT \"T\";T\n\
                       30 END\n\
                       100 T=T+1\n\
                       110 RETURN\n";
    for profile in PROFILES {
        let asm = build(SRC, profile).asm;
        assert_eq!(
            check_sites(&asm),
            0,
            "nothing here can stack records ({profile:?}):\n{asm}"
        );
    }
}

#[test]
fn a_spin_loop_around_a_returning_subroutine_gets_no_checks() {
    const SRC: &str = "10 C=0\n\
                       20 C=C+1\n\
                       30 IF C<10 THEN 20\n\
                       40 GOSUB 100\n\
                       50 PRINT \"W\";W\n\
                       60 END\n\
                       100 IF C>5 THEN W=1:RETURN\n\
                       110 W=2\n\
                       120 RETURN\n";
    for profile in PROFILES {
        let built = build(SRC, profile);
        assert_eq!(
            check_sites(&built.asm),
            0,
            "one GOSUB record can never reach a floor ({profile:?}):\n{}",
            built.asm
        );
        let (clean, out) = run(&built.prg_bytes);
        assert!(clean, "({profile:?}): {out:?}");
        assert_eq!(out.trim_end(), "W 1", "({profile:?})");
    }
}

#[test]
fn a_subroutine_that_reaches_a_gosub_again_stays_gated() {
    const SRC: &str = "10 N=1\n\
                       20 GOSUB 100\n\
                       30 END\n\
                       100 IF N>5 THEN RETURN\n\
                       110 N=N+1\n\
                       120 GOSUB 100\n\
                       130 RETURN\n";
    for profile in PROFILES {
        let built = build(SRC, profile);
        assert!(
            check_sites(&built.asm) > 0,
            "line 100 falls through to line 120's GOSUB, so records stack \
             ({profile:?}):\n{}",
            built.asm
        );
        let (clean, out) = run(&built.prg_bytes);
        assert!(
            clean,
            "six levels is nowhere near a floor ({profile:?}): {out:?}"
        );
    }
}

#[test]
fn the_gate_stays_off_for_ordinary_programs() {
    const ORDINARY: &[&str] = &[
        // A plain FOR loop.
        "10 FOR I=1 TO 5:PRINT I;:NEXT\n20 END\n",

        "10 C=0\n20 C=C+1\n30 IF C<10 THEN 20\n40 PRINT C\n",
        // GOSUB inside a loop, properly returned.
        "10 FOR I=1 TO 3:GOSUB 100:NEXT\n20 END\n100 T=T+1:RETURN\n",
        // Nested loops with a backwards GOSUB that returns.
        "10 FOR I=1 TO 2\n20 FOR J=1 TO 2\n30 GOSUB 5\n40 NEXT J\n\
         50 NEXT I\n60 PRINT T\n70 END\n5 T=T+1:RETURN\n",
    ];
    for profile in PROFILES {
        for src in ORDINARY {
            let asm = build(src, profile).asm;
            assert_eq!(
                check_sites(&asm),
                0,
                "ordinary program pays for the page-1 check ({profile:?}):\n{src}"
            );
        }
    }
}

#[test]
fn expressionless_statements_carry_no_check() {

    const MORE_GOTOS: &str = "10 I=0\n\
                              20 I=I+1:GOSUB 100\n\
                              30 PRINT I;:GOTO 15\n\
                              15 GOTO 20\n\
                              100 GOTO 105\n\
                              105 GOTO 30\n";
    for profile in PROFILES {
        let base = build(LEAK_GOSUB, profile).asm;
        let more = build(MORE_GOTOS, profile).asm;
        assert!(
            check_sites(&base) > 0,
            "the baseline must be under the gate ({profile:?}):\n{base}"
        );
        assert_eq!(
            check_sites(&more),
            check_sites(&base),
            "GOTO and GOSUB reach no CHKMEM call and must add no check \
             ({profile:?}):\n{more}"
        );

        let (clean, out) = run(&build(MORE_GOTOS, profile).prg_bytes);
        assert!(!clean, "({profile:?}): {out:?}");
        assert_eq!(
            out.split_whitespace().collect::<Vec<_>>(),
            (1..=24).map(|n| n.to_string()).collect::<Vec<_>>(),
            "({profile:?})"
        );
    }
}
