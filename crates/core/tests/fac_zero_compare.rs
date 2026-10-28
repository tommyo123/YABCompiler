//! Float comparisons with zero preserve rounding and overflow checks.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

type Outcome = (Vec<u8>, Option<u8>, Option<u16>);

const BUDGET: u64 = 20_000_000;

fn interpreted(src: &str) -> Outcome {
    let prg = tokenize_program(src).expect("tokenizes");
    let r = emu64::run_basic_to_end(&prg, BUDGET).expect("runs in the interpreter");
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
    ((r.output, r.error_code, r.error_line), c.asm)
}

fn same_as_the_rom(src: &str) -> Vec<String> {
    let want = interpreted(src);
    let mut asms = Vec::new();
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let (got, asm) = compiled(src, profile);
        assert_eq!(
            (common::petscii_to_string(&got.0), got.1, got.2),
            (common::petscii_to_string(&want.0), want.1, want.2),
            "{profile:?}:\n{src}"
        );
        asms.push(asm);
    }
    asms
}

#[test]
fn every_operator_on_both_sides() {
    let asms = same_as_the_rom(
        "10 FOR I=-2 TO 2:A=I/3:B=-I*0\n\
         20 IF A-B=0 THEN PRINT \"E\";\n21 IF A-B<>0 THEN PRINT \"N\";\n\
         22 IF A*3<0 THEN PRINT \"L\";\n23 IF A*3>0 THEN PRINT \"G\";\n\
         24 IF A+B<=0 THEN PRINT \"LE\";\n25 IF A+B>=0 THEN PRINT \"GE\";\n\
         30 IF 0=A*B THEN PRINT \"e\";\n31 IF 0<>A-B THEN PRINT \"n\";\n\
         32 IF 0<A*3 THEN PRINT \"l\";\n33 IF 0>A*3 THEN PRINT \"g\";\n\
         34 IF 0<=A+B THEN PRINT \"le\";\n35 IF 0>=A+B THEN PRINT \"ge\";\n\
         36 IF B*-1=0 THEN PRINT \"z\";\n40 PRINT:NEXT\n",
    );
    for asm in asms {
        assert!(!asm.contains("JSR $BC5B"), "no FCOMP left:\n{asm}");
    }
}

#[test]
fn a_left_operand_that_rounds_past_the_largest_float_overflows() {
    for cond in [
        "X+Z>0", "0<X+Z", "X+Z=0", "0=X+Z", "0<>X+Z", "X+Z-X>=0", "0>=X+Z-X",
    ] {
        same_as_the_rom(&format!(
            "10 READ X,Z\n20 IF {cond} THEN PRINT \"T\"\n30 PRINT \"END\"\n\
             40 DATA 1.7014118342E38,1.9807041E28\n"
        ));
    }
}
