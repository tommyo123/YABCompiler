//! Loop frequency weights the cost of integer variable promotion.

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
fn a_step_added_to_a_float() {
    same_as_the_rom(
        "10 X=10.5:D=1\n20 FOR I=1 TO 300\n30 X=X+D\n40 IF X<0 OR X>39 THEN D=-D\n50 NEXT\n60 PRINT X;D\n",
    );
}

#[test]
fn a_product_feeding_a_float_sum() {
    same_as_the_rom("10 FOR I=1 TO 30:FOR J=1 TO 20:P=I*J:S=S+P:NEXT:NEXT:PRINT P;S\n");
}

#[test]
fn a_mask_in_a_loop() {
    same_as_the_rom("10 FOR I=1 TO 300:C=(C+I) AND 255:NEXT:PRINT C\n");
}
