mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

/// Flat GOSUB loop that never returns: line 100 leaves the subroutine through `GOTO 20`, so every pass abandons one 7-byte record.
const ABANDONED: &str = "10 N=0\n\
                         20 N=N+1:PRINT N;\n\
                         30 GOSUB 100\n\
                         40 END\n\
                         100 GOTO 20\n";

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

fn counted_prefix(out: &str) -> usize {
    let seen: Vec<usize> = out
        .split_whitespace()
        .map(|t| {
            t.parse()
                .unwrap_or_else(|_| panic!("non-numeric {t:?} in {out:?}"))
        })
        .collect();
    assert_eq!(
        seen,
        (1..=seen.len()).collect::<Vec<_>>(),
        "output must be the run 1..n: {out:?}"
    );
    seen.len()
}

/// Repeated GOSUB frames exhaust BASIC stack space at the expected depth.
#[test]
fn a_gosub_abandoned_by_goto_stops_where_the_rom_stops() {
    for profile in PROFILES {
        for extraram in [false, true] {
            let (clean, insns, out) = run(&build(ABANDONED, profile, extraram).prg_bytes);
            assert!(
                !clean,
                "an abandoned-GOSUB loop must end in ?OUT OF MEMORY, not run \
                 away or fall out of the program ({profile:?}, \
                 extraram={extraram}); output {out:?}"
            );
            assert_eq!(
                counted_prefix(&out),
                24,
                "the ROM prints 1..24 and dies on the 24th GOSUB \
                 ({profile:?}, extraram={extraram})"
            );
            assert!(
                insns < common::MAX_INSNS / 10,
                "must not spin: {insns} instructions ({profile:?}, \
                 extraram={extraram})"
            );
        }
    }
}

#[test]
fn the_abandoned_loop_is_bounded_by_the_ordinary_gosub_guard() {
    for profile in PROFILES {
        let asm = build(ABANDONED, profile, false).asm;
        assert!(
            asm.contains("JSR __GOSUB_CHK"),
            "the single GOSUB site must be guarded ({profile:?}):\n{asm}"
        );
        let helper = asm
            .split("__GOSUB_CHK:")
            .nth(1)
            .unwrap_or_else(|| panic!("__GOSUB_CHK emitted:\n{asm}"));
        let line = helper
            .lines()
            .find(|l| l.trim().starts_with("CMP #$"))
            .unwrap_or_else(|| panic!("__GOSUB_CHK compares a limit:\n{helper}"));
        let limit = u8::from_str_radix(line.trim().trim_start_matches("CMP #$"), 16)
            .expect("hex limit");
        assert_eq!(
            limit, 27,
            "a program with no FOR anywhere gets GOSUB's own measured push \
             floor of 179 bytes, i.e. ceil(179/7) + 1 ({profile:?})"
        );
    }
}

#[test]
fn the_same_loop_closed_with_return_runs_to_completion() {
    const CLOSED: &str = "10 N=0\n\
                          20 N=N+1\n\
                          30 GOSUB 100\n\
                          40 IF N<100 THEN 20\n\
                          50 PRINT \"T\";N\n\
                          60 END\n\
                          100 RETURN\n";
    for profile in PROFILES {
        for extraram in [false, true] {
            let (clean, _, out) = run(&build(CLOSED, profile, extraram).prg_bytes);
            assert!(
                clean,
                "a balanced GOSUB loop must not trip the guard ({profile:?}, \
                 extraram={extraram}); output {out:?}"
            );
            assert_eq!(
                out.trim_end(),
                "T 100",
                "({profile:?}, extraram={extraram})"
            );
        }
    }
}

#[test]
fn twenty_three_abandoned_frames_still_run() {
    const NEAR_MISS: &str = "10 N=0\n\
                             20 N=N+1\n\
                             30 IF N=23 THEN 60\n\
                             40 GOSUB 100\n\
                             50 END\n\
                             60 PRINT \"OK\";N\n\
                             70 END\n\
                             100 GOTO 20\n";
    for profile in PROFILES {
        for extraram in [false, true] {
            let (clean, _, out) = run(&build(NEAR_MISS, profile, extraram).prg_bytes);
            assert!(
                clean,
                "22 abandoned frames is inside the ROM's budget ({profile:?}, \
                 extraram={extraram}); output {out:?}"
            );
            assert_eq!(out.trim_end(), "OK 23", "({profile:?}, extraram={extraram})");
        }
    }
}
