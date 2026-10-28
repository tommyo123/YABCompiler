//! Binary arithmetic keeps the rounded first operand in ARG.

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

fn compiled(src: &str, profile: Profile, extraram: bool) -> (Outcome, String) {
    let prg = tokenize_program(src).expect("tokenizes");
    let c = compile_with_options(
        &prg,
        CompileOptions {
            profile,
            extraram,
            force_extraram_off: !extraram,
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
        for extraram in [false, true] {
            let (got, _) = compiled(src, profile, extraram);
            assert_eq!(
                (common::petscii_to_string(&got.0), got.1, got.2),
                (common::petscii_to_string(&want.0), want.1, want.2),
                "{profile:?}, extraram {extraram}:\n{src}"
            );
        }
    }
}

#[test]
fn sums_products_and_quotients() {
    let src = "\
10 READ A,B,C,D
20 PRINT (A+B)*C;(A*B)/D;(A-B)-C;(D/A)+B;(C*C)*C;(A+C)-D
30 PRINT (A+B)+(C+D);(A*B)*(C-D);1/(A+B)*C;(A-B)/7
40 DATA 3.14159,-2.5,0.1,123.456
";
    same_as_the_rom(src);
    assert!(
        compiled(src, Profile::Speed, false)
            .1
            .contains("__FAC_TO_ARG:")
    );
    assert!(
        !compiled(src, Profile::Default, false)
            .1
            .contains("__FAC_TO_ARG:")
    );
}

#[test]
fn zero_operands_on_either_side() {
    same_as_the_rom(
        "\
10 READ A,B,C,D
20 PRINT (A+A)+B;(B-B)+C;(A*B)*C;(C-B)-A;(B+C)*A;(A-A)/B;(D+D)/B
30 DATA 0,5,-3.25,1E-9
",
    );
}

#[test]
fn rounding_of_the_first_operand() {
    same_as_the_rom(
        "\
10 READ X,Y,Z:X=X/3:Y=Y/3
20 PRINT (X+Y)-1;(X*3)-1;(Z+X)-Z;(X/7)*7-X;(Y-X)-X
30 DATA 1,2,1E9
",
    );
}

#[test]
fn division_by_zero_and_overflow() {
    same_as_the_rom("10 READ A,B:PRINT (A+A)/B\n20 DATA 1,0\n");
    same_as_the_rom("10 READ A,B:PRINT (A*A)*B\n20 DATA 1E30,1E20\n");
}
