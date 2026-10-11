mod common;

use yabcompiler_core::{CompileOptions, compile_with_options, tokenize_program};

const SRC: &str = "\
10 FOR I=1 TO 9
20 A$=MID$(\"HELLO WORLD\",I,3)
30 PRINT A$;\"/\";
40 NEXT
50 PRINT LEN(A$)
";

const LENGTHS: &str = "\
10 FOR I=0 TO 32:A$=LEFT$(\"ABCDEFGHIJKLMNOPQRSTUVWXYZ012345\",I):PRINT LEN(A$);RIGHT$(A$,1):NEXT
20 FOR I=32 TO 0 STEP -8:A$=LEFT$(\"ABCDEFGHIJKLMNOPQRSTUVWXYZ012345\",I):PRINT LEN(A$);A$:NEXT
";

#[test]
fn a_buffered_string_takes_every_length_up_to_its_cap() {
    let prg = tokenize_program(LENGTHS).expect("tokenizes");
    let asm = compile_with_options(&prg, CompileOptions::default())
        .expect("compiles")
        .asm;
    assert!(asm.contains("__SBUF_COPY:"), "the fixture must use the buffer copy:\n{asm}");
    let letters = "ABCDEFGHIJKLMNOPQRSTUVWXYZ012345";
    let mut want = String::new();
    for i in 0..=32 {
        want.push_str(&format!(" {i} {}\n", &letters[i.max(1) - 1..i]));
    }
    for i in (0..=32).rev().step_by(8) {
        want.push_str(&format!(" {i} {}\n", &letters[..i]));
    }
    let prg = common::compile_source(LENGTHS).expect("compiles");
    assert_eq!(common::run_clean(&prg).expect("reaches a clean exit"), want);
}

#[test]
fn a_buffered_string_takes_each_new_value() {
    let prg = tokenize_program(SRC).expect("tokenizes");
    let asm = compile_with_options(&prg, CompileOptions::default())
        .expect("compiles")
        .asm;
    assert!(asm.contains("__SBUF_COPY:"), "the fixture must use the buffer copy:\n{asm}");
    let prg = common::compile_source(SRC).expect("compiles");
    assert_eq!(
        common::run_clean(&prg).expect("reaches a clean exit"),
        "HEL/ELL/LLO/LO /O W/ WO/WOR/ORL/RLD/ 3 \n"
    );
}
