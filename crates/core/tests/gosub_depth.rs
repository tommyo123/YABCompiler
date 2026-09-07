

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

/// Loosest limit the compiler picks: `167 / 7 + 1` for a program whose recursion holds no FOR
/// frames.
const LOOSE_LIMIT: u8 = 24;
/// Tightest: `167 / (7 + 18) + 1` once a recursive GOSUB sits inside a FOR.
const TIGHT_LIMIT: u8 = 7;
/// Floor the limit is clamped up to, so a heuristic that over-counts FOR frames cannot pull the
/// guard down onto a working call chain.
const FLOOR: u8 = 6;
const CEIL: u8 = 40;

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

fn run(prg: &[u8]) -> (bool, u64, String) {
    let r = emu64::run_prg_to_end(prg, common::MAX_INSNS).expect("emu64 run");
    (
        r.clean_exit,
        r.instructions,
        common::petscii_to_string(&r.output),
    )
}

/// The `CMP #$xx` immediate the depth guard compares against.
fn guard_limit(asm: &str) -> u8 {
    let helper = asm
        .split("__GOSUB_CHK:")
        .nth(1)
        .unwrap_or_else(|| panic!("__GOSUB_CHK emitted:\n{asm}"));
    let line = helper
        .lines()
        .find(|l| l.trim().starts_with("CMP #$"))
        .unwrap_or_else(|| panic!("__GOSUB_CHK compares a limit:\n{helper}"));
    u8::from_str_radix(line.trim().trim_start_matches("CMP #$"), 16).expect("hex limit")
}
fn counted_prefix(out: &str) -> usize {
    let seen: Vec<usize> = out
        .split_whitespace()
        .map(|t| t.parse().unwrap_or_else(|_| panic!("non-numeric {t:?} in {out:?}")))
        .collect();
    assert_eq!(
        seen,
        (1..=seen.len()).collect::<Vec<_>>(),
        "output must be the run 1..n: {out:?}"
    );
    seen.len()
}

/// Plain recursion, no FOR anywhere.
const RECURSE: &str = "10 D=0\n\
                       20 GOSUB 100\n\
                       30 END\n\
                       100 D=D+1\n\
                       110 PRINT D;\n\
                       120 GOSUB 100\n\
                       130 RETURN\n";
const RECURSE_IN_FOR: &str = "10 D=0\n\
                              20 GOSUB 100\n\
                              30 END\n\
                              100 D=D+1\n\
                              110 PRINT D;\n\
                              120 FOR K=1 TO 2\n\
                              130 GOSUB 100\n\
                              140 NEXT\n\
                              150 RETURN\n";

// The guard exists and is wired to the right trap

#[test]
fn every_gosub_site_bumps_and_bounds_the_depth() {
    for profile in PROFILES {
        for extraram in [false, true] {
            let asm = build(RECURSE, profile, extraram).asm;
            assert!(
                asm.contains("    JSR __GOSUB_CHK\n"),
                "GOSUB must go through the depth guard \
                 ({profile:?}, extraram={extraram}):\n{asm}"
            );
            let helper = asm.split("__GOSUB_CHK:").nth(1).expect("helper emitted");
            assert!(
                helper.starts_with("\n    INC __GOSUB_DEPTH\n    LDA __GOSUB_DEPTH\n"),
                "the guard bumps then reads the counter \
                 ({profile:?}, extraram={extraram}):\n{helper}"
            );
            assert!(
                helper.lines().take(6).any(|l| l.contains("BCS __FOR_OOM")),
                "over the limit must reach the ?OUT OF MEMORY trap \
                 ({profile:?}, extraram={extraram}):\n{helper}"
            );
            let trap = asm
                .split("__FOR_OOM:")
                .nth(1)
                .unwrap_or_else(|| panic!("__FOR_OOM emitted:\n{asm}"));
            assert!(
                trap.contains(&format!(
                    "LDX #${:02X}",
                    yabcompiler_core::runtime::ERR_OUT_OF_MEMORY
                )),
                "the trap raises ?OUT OF MEMORY \
                 ({profile:?}, extraram={extraram}):\n{trap}"
            );
        }
    }
}
#[test]
fn the_guard_is_free_at_every_call_site() {
    // The subroutine has to have an effect, or the whole call chain is (correctly) optimised
    // away and there is nothing left to count.
    const MANY: &str = "10 GOSUB 100\n20 GOSUB 100\n30 GOSUB 100\n\
                        40 PRINT T\n50 END\n100 T=T+1\n110 RETURN\n";
    for profile in PROFILES {
        for extraram in [false, true] {
            let asm = build(MANY, profile, extraram).asm;
            assert_eq!(
                asm.matches("JSR __GOSUB_CHK").count(),
                3,
                "one 3-byte call per GOSUB ({profile:?}, extraram={extraram}):\n{asm}"
            );
            assert_eq!(
                asm.matches("__GOSUB_CHK:").count(),
                1,
                "the check itself is emitted once ({profile:?}, extraram={extraram})"
            );
            // Not inlined anywhere: the only INC/CMP pair lives in the helper.
            assert_eq!(
                asm.matches("INC __GOSUB_DEPTH").count(),
                1,
                "the bump must not be duplicated at call sites \
                 ({profile:?}, extraram={extraram}):\n{asm}"
            );
        }
    }
}

// The budget the compiler picks

#[test]
fn plain_recursion_gets_the_loose_budget() {
    for profile in PROFILES {
        let asm = build(RECURSE, profile, false).asm;
        assert_eq!(guard_limit(&asm), LOOSE_LIMIT, "({profile:?}):\n{asm}");
    }
}

#[test]
fn a_recursive_gosub_inside_a_for_tightens_the_budget() {
    for profile in PROFILES {
        let asm = build(RECURSE_IN_FOR, profile, false).asm;
        assert_eq!(guard_limit(&asm), TIGHT_LIMIT, "({profile:?}):\n{asm}");
    }
}
#[test]
fn a_forward_gosub_inside_nested_fors_keeps_the_loose_budget() {
    const SRC: &str = "10 T=0\n\
                       20 FOR I=1 TO 3\n\
                       30 FOR J=1 TO 3\n\
                       40 GOSUB 200\n\
                       50 NEXT J\n\
                       60 NEXT I\n\
                       70 PRINT \"T\";T\n\
                       80 END\n\
                       200 T=T+1\n\
                       210 RETURN\n";
    for profile in PROFILES {
        let built = build(SRC, profile, false);
        assert_eq!(
            guard_limit(&built.asm),
            LOOSE_LIMIT,
            "two enclosing FORs must not tighten a non-recursive call \
             ({profile:?}):\n{}",
            built.asm
        );
        let (clean, _, out) = run(&built.prg_bytes);
        assert!(clean, "runs to completion ({profile:?})");
        assert_eq!(out.trim_end(), "T 9", "({profile:?})");
    }
}
#[test]
fn the_budget_never_leaves_its_safe_window() {
    for src in [
        RECURSE,
        RECURSE_IN_FOR,
        "10 GOSUB 100\n20 PRINT T\n30 END\n100 T=T+1\n110 RETURN\n",
        "10 FOR I=1 TO 2\n20 FOR J=1 TO 2\n30 FOR K=1 TO 2\n40 GOSUB 10\n50 NEXT K\n\
         60 NEXT J\n70 NEXT I\n80 PRINT T\n90 END\n",
    ] {
        for profile in PROFILES {
            let asm = build(src, profile, false).asm;
            let limit = guard_limit(&asm);
            assert!(
                (FLOOR..=CEIL).contains(&limit),
                "limit {limit} outside [{FLOOR}, {CEIL}] ({profile:?}):\n{src}"
            );
        }
    }
}

// Behaviour

#[test]
fn runaway_recursion_stops_instead_of_destroying_the_machine() {
    for profile in PROFILES {
        for extraram in [false, true] {
            let (clean, insns, out) = run(&build(RECURSE, profile, extraram).prg_bytes);
            let depth = counted_prefix(&out);
            assert!(
                !clean,
                "must stop on ?OUT OF MEMORY, not fall out of the program \
                 ({profile:?}, extraram={extraram}); output {out:?}"
            );
            assert_eq!(
                depth, 23,
                "the ROM prints 1..23 and stops; RC-2 put the compiler on \
                 the same boundary ({profile:?}, extraram={extraram})"
            );
            assert!(
                insns < common::MAX_INSNS / 10,
                "must not spin: {insns} instructions ({profile:?}, extraram={extraram})"
            );
        }
    }
}
#[test]
fn recursion_holding_for_frames_stops_where_the_rom_does() {
    for profile in PROFILES {
        for extraram in [false, true] {
            let (clean, insns, out) = run(&build(RECURSE_IN_FOR, profile, extraram).prg_bytes);
            assert_eq!(
                out.split_whitespace().collect::<Vec<_>>(),
                vec!["1", "2", "3", "4", "5", "6"],
                "the ROM prints 1..7 then ?OUT OF MEMORY IN 100; the guard \
                 fires on the seventh GOSUB instead of the eighth PRINT \
                 ({profile:?}, extraram={extraram})"
            );
            assert!(!clean, "({profile:?}, extraram={extraram})");
            assert!(
                insns < common::MAX_INSNS / 10,
                "must not spin: {insns} ({profile:?}, extraram={extraram})"
            );
        }
    }
}
#[test]
fn deep_but_valid_recursion_runs_to_completion() {
    const SRC: &str = "10 D=0\n\
                       20 GOSUB 100\n\
                       30 PRINT \"OK\";D\n\
                       40 END\n\
                       100 D=D+1\n\
                       110 IF D>=20 THEN RETURN\n\
                       120 GOSUB 100\n\
                       130 RETURN\n";
    for profile in PROFILES {
        for extraram in [false, true] {
            let (clean, _, out) = run(&build(SRC, profile, extraram).prg_bytes);
            assert!(
                clean,
                "20-deep recursion is valid on the ROM and must not be \
                 interrupted ({profile:?}, extraram={extraram}); output {out:?}"
            );
            assert_eq!(out.trim_end(), "OK 20", "({profile:?}, extraram={extraram})");
        }
    }
}
#[test]
fn deep_but_valid_recursion_with_for_frames_runs_to_completion() {
    const SRC: &str = "10 D=0\n\
                       20 GOSUB 100\n\
                       30 PRINT D\n\
                       40 END\n\
                       100 D=D+1\n\
                       110 IF D>=6 THEN RETURN\n\
                       120 FOR K=1 TO 1\n\
                       130 GOSUB 100\n\
                       140 NEXT\n\
                       150 RETURN\n";
    for profile in PROFILES {
        for extraram in [false, true] {
            let built = build(SRC, profile, extraram);
            assert_eq!(guard_limit(&built.asm), TIGHT_LIMIT);
            let (clean, _, out) = run(&built.prg_bytes);
            assert!(
                clean,
                "6-deep recursion with a FOR per level is valid on the ROM \
                 ({profile:?}, extraram={extraram}); output {out:?}"
            );
            assert_eq!(out.trim_end(), " 6", "({profile:?}, extraram={extraram})");
        }
    }
}
#[test]
fn a_flat_gosub_loop_never_trips_the_guard() {
    const SRC: &str = "10 T=0\n\
                       20 FOR I=1 TO 30\n\
                       30 GOSUB 100\n\
                       40 NEXT\n\
                       50 PRINT \"T\";T\n\
                       60 END\n\
                       100 T=T+1\n\
                       110 RETURN\n";
    for profile in PROFILES {
        for extraram in [false, true] {
            let (clean, _, out) = run(&build(SRC, profile, extraram).prg_bytes);
            assert!(clean, "({profile:?}, extraram={extraram}); output {out:?}");
            assert_eq!(out.trim_end(), "T 30", "({profile:?}, extraram={extraram})");
        }
    }
}

/// `ON x GOSUB` lowers to the same statement and must be guarded too.
#[test]
fn on_gosub_is_guarded_as_well() {
    const SRC: &str = "10 X=1\n20 ON X GOSUB 100,200\n30 PRINT \"D\";D\n40 END\n\
                       100 D=D+1\n110 RETURN\n200 RETURN\n";
    for profile in PROFILES {
        let built = build(SRC, profile, false);
        assert!(
            built.asm.contains("JSR __GOSUB_CHK"),
            "({profile:?}):\n{}",
            built.asm
        );
        let (clean, _, out) = run(&built.prg_bytes);
        assert!(clean, "({profile:?})");
        assert_eq!(out.trim_end(), "D 1", "({profile:?})");
    }
}
#[test]
fn a_gosub_without_a_return_still_gets_a_counter() {
    const SRC: &str = "10 GOSUB 100\n20 END\n100 PRINT \"X\"\n";
    for profile in PROFILES {
        for extraram in [false, true] {
            let built = build(SRC, profile, extraram);
            assert!(
                built.asm.contains("__GOSUB_DEPTH:"),
                "the counter must be defined ({profile:?}, extraram={extraram}):\n{}",
                built.asm
            );
            assert!(
                built.asm.contains("JSR __GOSUB_CHK"),
                "({profile:?}, extraram={extraram}):\n{}",
                built.asm
            );
            let (clean, _, out) = run(&built.prg_bytes);
            assert!(clean, "({profile:?}, extraram={extraram})");
            assert_eq!(out.trim_end(), "X", "({profile:?}, extraram={extraram})");
        }
    }
}
#[test]
fn return_without_gosub_still_reports() {
    const SRC: &str = "10 PRINT \"A\"\n20 RETURN\n30 PRINT \"B\"\n";
    for profile in PROFILES {
        let built = build(SRC, profile, false);
        assert!(
            built.asm.contains("LDA __GOSUB_DEPTH\n    BEQ __RET_NO_GOSUB"),
            "({profile:?}):\n{}",
            built.asm
        );
        let (clean, _, out) = run(&built.prg_bytes);
        assert!(!clean, "stops on the error ({profile:?})");
        assert_eq!(out.trim_end(), "A", "({profile:?})");
    }
}
#[test]
fn a_program_without_gosub_pays_nothing() {
    const SRC: &str = "10 FOR I=1 TO 3:PRINT I;:NEXT\n20 PRINT \"=\";I\n";
    for profile in PROFILES {
        let asm = build(SRC, profile, false).asm;
        assert!(!asm.contains("__GOSUB_CHK"), "({profile:?}):\n{asm}");
        assert!(!asm.contains("__GOSUB_DEPTH"), "({profile:?}):\n{asm}");
    }
}
