//! Byte FOR counters are integer operands in array indices and addresses.

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

fn same_as_the_rom(src: &str) {
    let want = interpreted(src);
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let (got, _) = compiled(src, profile);
        assert_eq!(
            (common::petscii_to_string(&got.0), got.1, got.2),
            (common::petscii_to_string(&want.0), want.1, want.2),
            "{profile:?}:\n{src}"
        );
    }
}

#[test]
fn a_counter_read_by_the_body_still_indexes_as_an_integer() {
    // The array named like the counter keeps the float variable current inside the loop.
    let src = "\
10 DIM Q(12,10):J=1:Q(1,1)=5
20 IF Q(A(J),B(J)+1)=0 THEN B(J)=B(J)+1
30 FOR A=1 TO 10
40 FOR B=1 TO 12
50 M=M+Q(B,A):Q(B,A)=A+B
60 NEXT B,A:PRINT M;B(1)
70 FOR B=1 TO 12:POKE 49152+B,Q(B,10):NEXT:PRINT PEEK(49164)
";
    same_as_the_rom(src);
    let asm = compiled(src, Profile::Default).1;
    assert!(
        asm.matches("JSR __LD_B\n").count() <= 1,
        "the index reads the counter slot:\n{asm}"
    );
}
