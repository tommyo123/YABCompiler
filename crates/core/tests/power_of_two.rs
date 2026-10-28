//! Build exact powers of two for integer exponents in 0..126.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

type Outcome = (Vec<u8>, Option<u8>, Option<u16>);

const BUDGET: u64 = 40_000_000;

fn interpreted(src: &str) -> Outcome {
    let prg = tokenize_program(src).expect("tokenizes");
    let r = emu64::run_basic_to_end(&prg, BUDGET).expect("runs in the interpreter");
    assert!(r.ready_reached, "the interpreter reaches READY:\n{src}");
    (r.output, r.error_code, r.error_line)
}

fn compiled(src: &str, profile: Profile) -> (Outcome, String) {
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
    ((r.output, r.error_code, r.error_line), c.asm)
}

fn same_as_the_rom(src: &str, direct: bool) {
    let want = interpreted(src);
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let (got, asm) = compiled(src, profile);
        assert_eq!(
            (common::petscii_to_string(&got.0), got.1, got.2),
            (common::petscii_to_string(&want.0), want.1, want.2),
            "{profile:?}:\n{src}"
        );
        assert_eq!(!asm.contains("JSR $BF7B"), direct, "{profile:?}:\n{src}");
    }
}

#[test]
fn counters_and_bit_tests() {
    same_as_the_rom("10 FOR K=0 TO 126 STEP 5:A=2^K:PRINT A;:NEXT:PRINT\n", true);
    same_as_the_rom("10 FOR K=120 TO 126:PRINT 2^K;-2^K;1/2^K:NEXT\n", true);
    same_as_the_rom(
        "10 L=37:FOR X=.TO 7:IF (2^X AND L) THEN PRINT X;\n20 NEXT:FOR I=0 TO 30:B=B+2^I:NEXT:PRINT B\n",
        true,
    );
    same_as_the_rom("10 X=RND(-7):FOR I=1 TO 20:Y=INT(8*RND(1)):PRINT 2^Y;2^(Y+1)-1;:NEXT\n", true);
}

#[test]
fn unproved_exponents_keep_fpwrt() {
    same_as_the_rom("10 FOR K=-3 TO 3:PRINT 2^K;:NEXT\n", false);
    same_as_the_rom("10 X=127:PRINT 2^X\n", false);
    same_as_the_rom("10 X=SQR(2):PRINT 2^X;2^(X+X)\n", false);
}
