//! Numeric facts start at zero and join backward jumps and reset statements.

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
fn a_counter_on_the_first_line() {
    same_as_the_rom("10 N=N+1\n20 IF N<300 THEN 10\n30 PRINT N\n");
    same_as_the_rom("10 N=N+70:POKE 1024,N AND 255\n20 IF N<7000 THEN 10\n30 PRINT N\n");
}

#[test]
fn a_subroutine_on_the_first_line() {
    same_as_the_rom(
        "10 GOTO 100\n20 N=N+100:RETURN\n100 FOR I=1 TO 40:GOSUB 20:NEXT\n110 PRINT N\n",
    );
}

#[test]
fn clr_resets_a_counter() {
    same_as_the_rom(
        "10 FOR I=1 TO 30:N=N+1000:NEXT\n20 PRINT N\n30 CLR\n40 FOR I=1 TO 3:N=N+1:NEXT\n50 PRINT N\n",
    );
}

#[test]
fn a_variable_never_written_reads_as_zero() {
    same_as_the_rom("10 A=B+1:C%=D%-1\n20 POKE 1024+B,1\n30 PRINT A;C%;B\n");
}
