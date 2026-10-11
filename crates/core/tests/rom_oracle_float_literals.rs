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
        "the compiled program ends ({profile:?}):\n{src}"
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
        assert_eq!(got.0, want.0, "output bytes ({profile:?}):\n{src}");
    }
}

#[test]
fn int_of_forty_times_a_tenth_is_three() {
    same_as_the_rom("10 PRINT INT(40*.1);INT(.1*40)\n20 A=40:PRINT INT(A*.1);INT(.1*A)\n");
}

#[test]
fn a_constant_factor_on_either_side_of_a_stored_value() {
    same_as_the_rom("10 X=7-7^2:PRINT 2*X;X*2;X+X;ABS(2)*X;1*X;X*1;-1*X;X*-1\n");
}

#[test]
fn a_tenth_times_a_variable_against_itself() {
    same_as_the_rom("10 READ A:DATA 40\n20 PRINT (A*.1)<>A*.1;A*.1=A*.1\n");
}

#[test]
fn propagated_constants_are_rounded_as_stored() {
    same_as_the_rom(
        "10 C=-.9:K=40:P=K*C:PRINT (P+36)*1E10\n20 A%=K*C:PRINT A%:A%=P:PRINT A%\n",
    );
}

#[test]
fn literals_print_and_compare_as_the_rom_reads_them() {
    same_as_the_rom(
        "10 PRINT .1;.3;3.14159265;2.718281828;1E-5;1.5E3;123.456;.999999999\n\
         20 PRINT .1+.2=.3;.1*3=.3;1/3=.333333333;.7*10\n",
    );
}
