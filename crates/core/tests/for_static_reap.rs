//! A `FOR` that pushes no frame still has to reap one.

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

fn asm_of(src: &str) -> String {
    build(src, Profile::Default, false).1
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

const REAP: &str = "\
10 FOR I=1 TO 3\n\
20 PRINT\"a\";I\n\
30 IF I=2 THEN 200\n\
40 NEXT I\n\
50 PRINT\"e\":END\n\
200 FOR I=1 TO 2:PRINT\"s\";I:NEXT I\n\
205 GOTO 210\n\
207 NEXT I,T\n\
209 GOTO 250\n\
210 FOR T=1 TO 2\n\
220 FOR I=1 TO 2\n\
230 PRINT\"x\";T;I:GOTO 207\n\
250 PRINT\"done\"\n";

const BARRIER: &str = "\
10 FOR I=1 TO 3\n\
20 PRINT\"a\";I\n\
30 IF I=2 THEN 200\n\
40 NEXT I\n\
50 PRINT\"e\":END\n\
200 GOSUB 300\n\
205 GOTO 210\n\
207 NEXT I,T\n\
209 GOTO 250\n\
210 FOR T=1 TO 2\n\
220 FOR I=1 TO 2\n\
230 PRINT\"x\";T;I:GOTO 207\n\
250 PRINT\"done\":END\n\
300 FOR I=1 TO 2:PRINT\"s\";I:NEXT I\n\
310 RETURN\n";

/// Line 200 is a static `FOR J`.
const OTHER_VAR: &str = "\
10 FOR I=1 TO 3\n\
20 PRINT\"a\";I\n\
30 IF I=2 THEN 200\n\
40 NEXT I\n\
50 PRINT\"e\":END\n\
200 FOR J=1 TO 2:PRINT\"s\";J:NEXT J\n\
205 GOTO 210\n\
207 NEXT I,T\n\
209 GOTO 250\n\
210 FOR T=1 TO 2\n\
220 FOR I=1 TO 2\n\
230 PRINT\"x\";T;I:GOTO 207\n\
250 PRINT\"done\"\n";

#[test]
fn a_static_for_reaps_the_frame_a_goto_left_live() {
    check_all_variants(
        REAP,
        "A 1 \nA 2 \nS 1 \nS 2 \nX 1  1 \nX 1  2 \nX 2  1 \nX 2  2 \nDONE\n",
    );
}

#[test]
fn a_gosub_marker_stops_the_static_reap() {
    check_stops_with(BARRIER, "A 1 \nA 2 \nS 1 \nS 2 \nX 1  1 \nX 1  2 \n");
}

#[test]
fn a_static_for_on_another_variable_reaps_nothing() {
    check_stops_with(OTHER_VAR, "A 1 \nA 2 \nS 1 \nS 2 \nX 1  1 \nX 1  2 \n");
}

#[test]
fn the_reap_is_emitted_once_and_called_per_site() {
    let asm = asm_of(REAP);
    assert_eq!(
        asm.matches("__FOR_DISCARD:").count(),
        1,
        "one helper body:\n{asm}"
    );
    assert_eq!(
        asm.matches("JSR __FOR_DISCARD").count(),
        1,
        "one call, for line 200:\n{asm}"
    );
}

#[test]
fn an_ordinary_program_emits_no_reap() {
    let asm = asm_of(
        "10 FOR I=1 TO 3\n\
         20 FOR J=1 TO 3\n\
         30 PRINT I;J\n\
         40 NEXT J\n\
         50 NEXT I\n",
    );
    assert!(
        !asm.contains("__FOR_STACK"),
        "no runtime FOR-stack in an ordinary nest:\n{asm}"
    );
    assert!(!asm.contains("__FOR_DISCARD"), "and no reap:\n{asm}");
}

#[test]
fn a_static_for_whose_variable_no_frame_uses_emits_no_reap() {
    let asm = asm_of(OTHER_VAR);
    assert!(
        asm.contains("__FOR_STACK"),
        "this program does have a runtime FOR-stack:\n{asm}"
    );
    assert!(
        !asm.contains("JSR __FOR_DISCARD"),
        "but J needs no reap:\n{asm}"
    );
}

#[test]
fn a_reaping_site_does_not_also_decrement_the_pointer() {
    let asm = asm_of(BARRIER);
    let call = asm
        .find("JSR __FOR_DISCARD")
        .unwrap_or_else(|| panic!("line 300 reaps:\n{asm}"));
    // The 40 lines before the call cover line 300's whole prologue.
    let before: String = asm[..call].lines().rev().take(40).collect::<Vec<_>>().join("\n");
    assert!(
        !before.contains("DEC __FOR_STACK_PTR"),
        "no fixed decrement in front of the reap:\n{before}"
    );
}
