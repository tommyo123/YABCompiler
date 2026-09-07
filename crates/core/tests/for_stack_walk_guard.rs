mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn build(src: &str, profile: Profile) -> yabcompiler_core::Compiled {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(
        &prg,
        CompileOptions {
            profile,
            force_extraram_off: true,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}\n{src}"))
}

fn asm_of(src: &str) -> String {
    build(src, Profile::Default).asm
}

fn check(src: &str, expected: &str, clean_expected: bool) {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let bytes = build(src, profile).prg_bytes;
        let r = emu64::run_prg_to_end(&bytes, 8_000_000).expect("runs");
        let got = common::petscii_to_string(&r.output);
        assert_eq!(got, expected, "{profile:?} output mismatch for:\n{src}");
        assert_eq!(
            r.clean_exit, clean_expected,
            "{profile:?} termination mismatch for:\n{src}"
        );
    }
}

const TWO_RETURNS: &str = "10 GOSUB 100\n\
                           20 GOSUB 200\n\
                           30 NEXT\n\
                           40 PRINT\"NEVER\"\n\
                           100 FOR I=1 TO 3\n\
                           110 RETURN\n\
                           200 FOR M=1 TO 3\n\
                           210 RETURN\n";

#[test]
fn the_walk_is_one_shared_helper_not_an_inline_copy() {
    let asm = asm_of(TWO_RETURNS);
    let bodies = asm.lines().filter(|l| l.trim() == "__FOR_WALK_MARK:").count();
    let calls = asm
        .lines()
        .map(str::trim)
        .filter(|l| *l == "JSR __FOR_WALK_MARK" || *l == "JMP __FOR_WALK_MARK")
        .count();
    assert_eq!(bodies, 1, "expected exactly one helper body:\n{asm}");
    assert!(calls >= 1, "no site reaches the helper:\n{asm}");
    assert!(
        !asm.contains("__FOR_RT_WALK_"),
        "an inline copy of the walk survived:\n{asm}"
    );
}

#[test]
fn the_walk_cannot_decrement_past_the_bottom_of_the_array() {
    let asm = asm_of(TWO_RETURNS);
    let body: Vec<&str> = asm
        .lines()
        .map(str::trim)
        .skip_while(|l| *l != "__FOR_WALK_MARK:")
        .take_while(|l| *l != "RTS")
        .filter(|l| !l.is_empty() && !l.starts_with(';'))
        .collect();
    assert!(!body.is_empty(), "no helper body found:\n{asm}");
    let dex = body
        .iter()
        .position(|l| *l == "DEX")
        .unwrap_or_else(|| panic!("the walk must decrement: {body:?}"));
    assert_eq!(
        body[dex + 1], "BEQ __FOR_WALK_END",
        "the DEX is not followed by its own bottom-of-array test: {body:?}"
    );

    let back = body
        .iter()
        .position(|l| *l == "BNE __FOR_WALK_SRCH")
        .unwrap_or_else(|| panic!("the walk must loop: {body:?}"));
    assert!(
        back > dex + 1,
        "the loop-back jumps over the guard: {body:?}"
    );
}

#[test]
fn a_program_with_no_runtime_return_pays_nothing() {
    let asm = asm_of("10 GOSUB 100\n20 PRINT\"BACK\"\n30 END\n100 FOR I=1 TO 3:NEXT\n110 RETURN\n");
    assert!(
        !asm.contains("__FOR_WALK_MARK"),
        "a statically-paired program must not pull in the walk:\n{asm}"
    );
}

#[test]
fn return_discards_the_frames_the_subroutine_opened() {
    check(
        "10 GOSUB 100\n\
         20 PRINT\"BACK\"\n\
         30 NEXT\n\
         40 PRINT\"NEVER\"\n\
         100 FOR I=1 TO 3\n\
         110 RETURN\n",
        "BACK\n",
        false,
    );
}

#[test]
fn return_leaves_the_callers_own_loop_alone() {
    check(
        "5 GOTO 10\n\
         6 NEXT:GOTO 50\n\
         10 FOR J=1 TO 2\n\
         20 GOSUB 100\n\
         30 PRINT\"J\";J\n\
         40 GOTO 6\n\
         50 PRINT\"DONE\":END\n\
         100 FOR I=1 TO 3\n\
         110 RETURN\n",
        "J 1 \nJ 2 \nDONE\n",
        true,
    );
}

/// Two markers on the stack.
#[test]
fn a_nested_return_stops_at_its_own_marker() {
    check(
        "5 GOTO 10\n\
         6 NEXT:GOTO 90\n\
         10 GOSUB 100\n\
         90 PRINT\"END\":END\n\
         100 FOR K=1 TO 2\n\
         110 GOSUB 200\n\
         120 PRINT\"K\";K\n\
         130 GOTO 6\n\
         200 FOR I=1 TO 3\n\
         210 RETURN\n",
        "K 1 \nK 2 \nEND\n",
        true,
    );
}
