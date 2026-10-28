//! Literal unit-step loops compare directly with the first value past the end.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

type Outcome = (Vec<u8>, Option<u8>, Option<u16>);

const BUDGET: u64 = 40_000_000;

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
fn empty_loops_leave_the_value_past_the_end() {
    let src = "\
10 FOR I=1 TO 8000:NEXT:PRINT I
20 FOR I=300 TO 300:NEXT:PRINT I
30 FOR I=32000 TO 32766:NEXT:PRINT I
40 FOR I=3000 TO -300 STEP -1:NEXT I:PRINT I
50 FOR I=-200 TO -700 STEP -1:NEXT:PRINT I
";
    same_as_the_rom(src);
    let asm = compiled(src, Profile::Speed).1;
    assert!(asm.contains("CMP #$41"), "8001 is the stop value:\n{asm}");
}

#[test]
fn a_body_that_reads_the_counter() {
    same_as_the_rom(
        "\
10 S=0:FOR I=250 TO 1100:S=S+I:NEXT:PRINT S;I
20 FOR I=999 TO 256 STEP -1:IF I=300 THEN PRINT \"AT\";I
30 NEXT:PRINT I
",
    );
}

#[test]
fn nested_loops_and_an_early_exit() {
    same_as_the_rom(
        "\
10 FOR J=1 TO 3:FOR I=255 TO 520:C=C+1:NEXT I,J:PRINT C;I;J
20 FOR I=1 TO 5000:IF I=777 THEN 40
30 NEXT:PRINT \"NO\"
40 PRINT I
",
    );
}
