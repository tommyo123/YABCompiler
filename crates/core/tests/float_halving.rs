//! Division by powers of two preserves signs, zero, and small exponents.

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
fn small_values() {
    same_as_the_rom("10 X=7:PRINT X/2;X/4;X/1024;-X/2;0/2;(X+0.1)/8\n");
    same_as_the_rom("10 A=5:B=-3:PRINT (A+B)/2;(A*B)/4;SQR(A)/2;INT(A/2);A/2*2\n");
}

#[test]
fn random_values() {
    same_as_the_rom(
        "10 FOR I=1 TO 40:X=(I*1237 AND 1023)/3.7-138:Y=X/2:Z=(X*3)/16:PRINT Y;Z;:NEXT\n",
    );
    same_as_the_rom("10 FOR I=1 TO 30:A=(I*7919 AND 4095)*1.3E26:PRINT A/32768;(A-1)/2;:NEXT\n");
}

#[test]
fn values_near_underflow() {
    same_as_the_rom("10 X=1E-38:FOR I=1 TO 12:X=X/2:PRINT X;:NEXT\n");
    same_as_the_rom("10 X=-3E-38:FOR I=1 TO 12:X=X/4:PRINT X;:NEXT\n");
}

#[test]
fn values_near_overflow() {
    same_as_the_rom("10 X=1.7E38:PRINT X/2;X/32768;-X/16\n");
}

#[test]
fn a_collatz_run() {
    same_as_the_rom(
        "10 N=27:C=0\n20 IF N=1 THEN 60\n30 IF N/2=INT(N/2) THEN N=N/2:GOTO 50\n40 N=3*N+1\n50 C=C+1:GOTO 20\n60 PRINT C\n",
    );
}
