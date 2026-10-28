//! FOR trip counts bound variable steps in single-entry loop bodies.

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
fn a_pointer_stepped_once_per_round() {
    same_as_the_rom("5 X=832\n10 FOR I=0 TO 62:POKE X,I:X=X+1:NEXT:PRINT X;PEEK(894)\n");
}

#[test]
fn a_count_with_a_computed_end() {
    same_as_the_rom("10 N=PEEK(1000)+700:FOR I=1 TO N:T=T+1:POKE 1024,T AND 255:NEXT:PRINT T;I\n");
}

#[test]
fn a_jump_back_to_the_body_start() {
    same_as_the_rom(
        "10 FOR I=1 TO 10\n20 V=V+1000\n30 IF V>5000 AND V<40000 THEN 20\n40 NEXT:PRINT V\n",
    );
}

#[test]
fn the_for_run_again_from_its_body() {
    same_as_the_rom("10 FOR I=1 TO 10\n20 V=V+1000\n30 IF V<40000 THEN 10\n40 NEXT\n50 PRINT V\n");
}

#[test]
fn a_step_inside_an_inner_loop() {
    same_as_the_rom("10 FOR I=1 TO 10:FOR J=1 TO 10:V=V+400:NEXT:NEXT:PRINT V\n");
    same_as_the_rom("10 FOR K=1 TO 3:FOR I=1 TO 4000:V=V+3:NEXT:NEXT:PRINT V\n");
}

#[test]
fn the_counter_changed_in_the_body() {
    same_as_the_rom("10 FOR I=1 TO 40:V=V+1000:IF I=20 AND V<50000 THEN I=1\n20 NEXT:PRINT V\n");
}

#[test]
fn a_subroutine_that_steps() {
    same_as_the_rom("10 FOR I=1 TO 30:GOSUB 100:NEXT:PRINT V\n20 END\n100 V=V+1500:RETURN\n");
}

#[test]
fn a_negative_step() {
    same_as_the_rom("10 FOR I=40 TO 1 STEP -1:V=V-1000:NEXT:PRINT V\n");
}
