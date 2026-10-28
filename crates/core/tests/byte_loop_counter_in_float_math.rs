//! Float expressions read the current value of byte FOR counters.

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

/// T also gets a fraction after the loop, so it stays a float variable.
fn in_a_byte_loop(test: &str, next: &str) -> String {
    format!("10 FOR T=1 TO 8\n20 {test}\n30 {next}\n40 T=RND(1)/3:PRINT \"E\"\n")
}

#[test]
fn a_test_whose_integer_range_is_not_proven() {
    for next in ["NEXT", "NEXT T", "FOR TT=1 TO 2:NEXT:NEXT"] {
        same_as_the_rom(&in_a_byte_loop("IF T*8192-20000>0 THEN PRINT \"X\";", next));
        same_as_the_rom(&in_a_byte_loop("IF T*4096+T>20000 THEN PRINT \"X\";", next));
    }
}

#[test]
fn small_tests_after_a_nested_loop() {
    for test in [
        "IF T-5>0 THEN PRINT \"X\";",
        "IF T-4 THEN PRINT \"X\";",
        "IF T*T>20 THEN PRINT \"X\";",
    ] {
        same_as_the_rom(&in_a_byte_loop(test, "FOR TT=1 TO 2:NEXT:NEXT"));
    }
}
