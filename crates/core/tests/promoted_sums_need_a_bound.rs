//! Promote float sums only when their full range is proven.

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
fn a_sum_read_often_in_a_for_loop() {
    same_as_the_rom("10 FOR I=1 TO 50:S=S+1000:A=S+S:B=S-S:C=S+1:D=S+2:NEXT\n20 PRINT S;A\n");
}

#[test]
fn two_steps_per_round() {
    same_as_the_rom("10 FOR I=1 TO 100:V=V+200:V=V+200:NEXT:PRINT V\n");
}

#[test]
fn nested_loops_count_past_32767() {
    same_as_the_rom("10 FOR I=1 TO 100:FOR J=1 TO 40:S=S+9:A=S+S:B=S-1:NEXT:NEXT\n20 PRINT S;A\n");
}

#[test]
fn a_guarded_counter_past_32767() {
    same_as_the_rom("10 N=N+1000:A=N+1:B=N+2:C=N-1\n20 IF N<40000 THEN 10\n30 PRINT N;A\n");
}
