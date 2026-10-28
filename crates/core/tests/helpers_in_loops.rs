//! Inline factored helpers into short loops in the speed profile.

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

fn helper_calls(asm: &str) -> usize {
    asm.lines()
        .map(str::trim)
        .filter(|l| {
            ["JSR __SEQ_", "JSR __JSR2_", "JSR __JSR3_", "JSR __TAIL_", "JSR __PH326_"]
                .iter()
                .any(|p| l.starts_with(p))
        })
        .count()
}

#[test]
fn a_loop_calls_fewer_helpers_in_the_speed_profile() {
    let src = "\
10 DIM A(20),B(20):FOR I=0 TO 20:A(I)=I*1.5:B(I)=20-I:NEXT
20 FOR K=1 TO 3:FOR I=1 TO 19:S=S+A(I)*B(I)-A(I-1)*B(I+1):T=T+A(I)*B(I)-A(I+1)*B(I-1):NEXT:NEXT
30 PRINT S;T
";
    same_as_the_rom(src);
    let default = helper_calls(&compiled(src, Profile::Default).1);
    let speed = helper_calls(&compiled(src, Profile::Speed).1);
    assert!(speed < default, "speed {speed}, default {default}");
}
