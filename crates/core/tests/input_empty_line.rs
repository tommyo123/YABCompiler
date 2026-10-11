//! A bare RETURN at an `INPUT` prompt is not an empty answer.

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn asm_of(src: &str, profile: Profile) -> String {
    let prg = tokenize_program(src).expect("tokenizes");
    let opts = CompileOptions {
        profile,
        force_extraram_off: true,
        ..CompileOptions::default()
    };
    compile_with_options(&prg, opts)
        .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}"))
        .asm
}

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

#[test]
fn empty_line_branches_around_every_store() {
    for profile in PROFILES {
        let asm = asm_of("10 INPUT A$,B$\n20 PRINT A$;B$\n", profile);
        let lines: Vec<&str> = asm.lines().map(str::trim).collect();
        let helper_ret = lines
            .iter()
            .position(|l| *l == "__INPUT_LINE:")
            .and_then(|at| lines[at..].iter().position(|l| *l == "RTS").map(|r| at + r))
            .unwrap_or_else(|| panic!("{profile:?}: no __INPUT_LINE helper:\n{asm}"));
        assert_eq!(
            lines[helper_ret - 1], "LDA __INPUT_BUF",
            "{profile:?}: __INPUT_LINE must return with the length byte loaded:\n{asm}"
        );
        let test_at = lines
            .iter()
            .position(|l| *l == "JSR __INPUT_LINE")
            .unwrap_or_else(|| panic!("{profile:?}: no INPUT line read:\n{asm}"));
        let join = lines
            .iter()
            .position(|l| l.starts_with("__IN_EMPTY_") && l.ends_with(':'))
            .unwrap_or_else(|| panic!("{profile:?}: no empty-line join label:\n{asm}"));
        assert!(
            join > test_at,
            "{profile:?}: the join label must come after the test:\n{asm}"
        );
        // Both target stores have to sit inside the skipped region.
        let stores: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.starts_with("JSR __EXTRACT_FIELD"))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(stores.len(), 2, "{profile:?}: expected two fields:\n{asm}");
        for s in stores {
            assert!(
                s > test_at && s < join,
                "{profile:?}: a target store at line {s} is outside the skipped \
                 region {test_at}..{join}:\n{asm}"
            );
        }
    }
}

#[test]
fn input_file_keeps_storing_unconditionally() {
    for profile in PROFILES {
        let asm = asm_of("10 OPEN 1,8,2,\"D\"\n20 INPUT#1,A$\n30 PRINT A$\n", profile);
        assert!(
            !asm.contains("__IN_EMPTY_"),
            "{profile:?}: INPUT# must not get the keyboard empty-line guard:\n{asm}"
        );
    }
}

/// Liveness: the value a skipped `INPUT` preserves must still be there.
#[test]
fn a_value_an_input_may_preserve_is_not_dead() {
    for profile in PROFILES {
        let asm = asm_of("10 A$=\"OA\"\n20 INPUT A$\n30 PRINT A$\n", profile);
        // "OA" in PETSCII, as the string pool emits it.
        assert!(
            asm.contains("$CF,$C1") || asm.contains("$4F,$41"),
            "{profile:?}: the pre-INPUT assignment was dead-stripped, so the \
             empty-line path has nothing to preserve:\n{asm}"
        );
    }
}

/// Live-range splitting: an `INPUT` target keeps one storage slot.
#[test]
fn an_input_target_is_not_split_into_two_slots() {
    for profile in PROFILES {
        let asm = asm_of("10 N=7\n20 INPUT N\n30 PRINT N\n", profile);
        assert!(
            !asm.contains("V_N__1") && !asm.contains("VI_N__1"),
            "{profile:?}: N was live-range split across the INPUT:\n{asm}"
        );
    }
}

#[test]
fn splitting_still_happens_without_an_input() {
    let asm = asm_of(
        "10 U=RND(0)\n\
         20 PRINT U\n\
         30 U=5\n\
         40 POKE 1024,U\n\
         50 PRINT PEEK(1024)\n",
        Profile::Default,
    );
    assert!(
        asm.contains("U__1"),
        "the multi-type splitter must still fire when no INPUT is involved:\n{asm}"
    );
}
