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
    assert!(r.clean_exit || r.error_code.is_some(), "ends ({profile:?}):\n{src}");
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
fn a_sum_past_32767_compares_as_positive() {
    same_as_the_rom("10 READ A%,B%:IF A%+B%>0 THEN PRINT \"P\"\n15 PRINT \"E\"\n20 DATA 30000,30000\n");
}

#[test]
fn a_difference_below_minus_32768_compares_as_negative() {
    same_as_the_rom("10 READ A%,B%:IF A%-B%<0 THEN PRINT \"N\"\n15 PRINT \"E\"\n20 DATA -30000,30000\n");
}

#[test]
fn a_sum_past_32767_is_not_its_wrapped_value() {
    same_as_the_rom(
        "10 READ A%,B%:IF A%+B%=-5536 THEN PRINT \"W\"\n15 PRINT \"E\"\n20 DATA 30000,30000\n",
    );
}

#[test]
fn fuzz_a_wrapped_difference_took_the_wrong_branch() {
    same_as_the_rom(
        "40 I%=-32768\n70 J%=1:GOSUB 800\n80 IF I%-J%<0 THEN 790\n100 J%=(Y)+J%+A\n790 END\n\
         800 A=ABS(I%+D(J%))-(255)-I%:RETURN\n",
    );
}
