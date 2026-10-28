//! Counter bounds inferred from equality branches that exit loops.

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
fn the_exit_leaves_the_loop() {
    same_as_the_rom(
        "10 X=1\n20 POKE 1024+X,1:X=X+1:IF X=253 THEN 40\n30 GOTO 20\n40 PRINT X;PEEK(1276)\n",
    );
}

#[test]
fn a_then_that_prints_does_not_leave() {
    same_as_the_rom("10 X=X+1:IF X=5 THEN PRINT \"A\";\n20 IF X<300 THEN 10\n30 PRINT X\n");
    same_as_the_rom(
        "10 X=X+1:IF X=5 THEN PRINT \"A\";\n20 POKE 1024+X,1:IF X<300 THEN 10\n30 PRINT X;PEEK(1324)\n",
    );
}

#[test]
fn a_counter_that_starts_past_the_exit() {
    same_as_the_rom("5 X=10\n10 X=X+1:IF X=5 THEN 30\n20 IF X<300 THEN 10\n30 PRINT X\n");
}

#[test]
fn a_jump_back_into_the_loop() {
    same_as_the_rom(
        "10 X=X+1:IF X=5 THEN 20\n20 POKE 1024+(X AND 255),1:IF X<300 THEN 10\n30 PRINT X\n",
    );
}
