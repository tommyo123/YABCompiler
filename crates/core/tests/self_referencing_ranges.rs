//! Self-referencing sums retain only ranges bounded by program flow.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

type Outcome = (Vec<u8>, Option<u8>, Option<u16>);

const BUDGET: u64 = 20_000_000;

fn interpreted(src: &str) -> Outcome {
    let prg = tokenize_program(src).expect("tokenizes");
    let r = emu64::run_basic_to_end(&prg, BUDGET).expect("runs in the interpreter");
    assert!(r.ready_reached, "the interpreter reaches READY:\n{src}");
    (r.output, r.error_code, r.error_line)
}

fn compiled(src: &str, profile: Profile) -> Outcome {
    let prg = tokenize_program(src).expect("tokenizes");
    let c = compile_with_options(
        &prg,
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}\n{src}"));
    let r = emu64::run_prg_to_end(&c.prg_bytes, BUDGET).expect("runs");
    assert!(
        r.clean_exit || r.error_code.is_some(),
        "ends ({profile:?}):\n{src}"
    );
    (r.output, r.error_code, r.error_line)
}

fn same_as_the_rom(src: &str) {
    let want = interpreted(src);
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let got = compiled(src, profile);
        assert_eq!(
            (common::petscii_to_string(&got.0), got.1, got.2),
            (common::petscii_to_string(&want.0), want.1, want.2),
            "{profile:?}:\n{src}"
        );
    }
}

#[test]
fn a_sum_clamped_by_an_if() {
    same_as_the_rom(
        "10 FOR I=1 TO 300\n20 C=C+I\n30 IF C>10000 THEN C=C-10000\n40 NEXT\n50 PRINT C\n",
    );
}

#[test]
fn a_sum_masked_each_round() {
    same_as_the_rom("10 FOR I=1 TO 300\n20 C=(C+I) AND 255\n40 NEXT\n50 PRINT C\n");
}

#[test]
fn a_bouncing_position() {
    same_as_the_rom(
        "10 X=10:D=1\n20 FOR I=1 TO 300\n30 X=X+D\n40 IF X<0 OR X>39 THEN D=-D\n50 NEXT\n60 PRINT X;D\n",
    );
}

#[test]
fn a_sum_that_is_not_clamped() {
    same_as_the_rom("10 FOR I=1 TO 300\n20 C=C+I\n30 IF C>100000 THEN C=0\n40 NEXT\n50 PRINT C\n");
}

#[test]
fn a_doubling_value() {
    same_as_the_rom("10 X=1:FOR I=1 TO 20:X=X*2+1:POKE 1024,X AND 255:NEXT:PRINT X\n");
}

#[test]
fn an_integer_sum_that_overflows() {
    same_as_the_rom("10 FOR I=1 TO 300:A%=A%+I:NEXT:PRINT A%\n");
}
