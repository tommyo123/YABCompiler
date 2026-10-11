mod common;

use yabcompiler_core::{CompileOptions, compile_with_options, tokenize_program};

fn error_at(src: &str) -> (Option<u8>, Option<u16>) {
    let prg = common::compile_source(src).expect("compiles");
    let r = emu64::run_prg_to_end(&prg, common::MAX_INSNS).expect("runs");
    (r.error_code, r.error_line)
}

const DIVISION_BY_ZERO: u8 = 20;
const OVERFLOW: u8 = 15;
const ILLEGAL_QUANTITY: u8 = 14;

#[test]
fn an_error_after_print_only_lines_names_its_own_line() {
    let src = "\
10 A$=\"S\":X=0
20 PRINT \"A\";A$;CHR$(13);:PRINT TI$
300 PRINT \"B\":PRINT
310 Y=1/X
";
    assert_eq!(error_at(src), (Some(DIVISION_BY_ZERO), Some(310)));
}

#[test]
fn an_error_inside_a_print_names_the_print_line() {
    let src = "10 X=0\n20 PRINT \"A\"\n300 PRINT \"B\";1/X\n";
    assert_eq!(error_at(src), (Some(DIVISION_BY_ZERO), Some(300)));
}

#[test]
fn an_error_reached_by_a_jump_from_a_print_line_names_its_own_line() {
    let src = "\
10 X=0:GOTO 300
20 PRINT \"NEVER\"
300 PRINT \"B\":GOTO 600
600 Y=1/X
";
    assert_eq!(error_at(src), (Some(DIVISION_BY_ZERO), Some(600)));
}

#[test]
fn a_print_only_line_stamps_no_line_number() {
    let src = "10 X=1/X\n20 PRINT \"HI\";CHR$(13)\n30 X=1/X\n";
    let prg = tokenize_program(src).expect("tokenizes");
    let asm = compile_with_options(&prg, CompileOptions::default())
        .expect("compiles")
        .asm;
    assert!(!asm.contains("LDA #$14"), "line 20 stamps itself:\n{asm}");
    assert!(asm.contains("LDA #$1E"), "line 30 still stamps itself:\n{asm}");
}

#[test]
fn an_error_after_plain_assignment_and_compare_lines_names_its_own_line() {
    let src = "\
10 X=0:Y=5
20 Z=Y:A$=\"T\":IF Z=5 THEN B$=\"Q\"
300 IF A$=\"T\" AND Z=Y THEN PRINT B$
310 W=1/X
";
    assert_eq!(error_at(src), (Some(DIVISION_BY_ZERO), Some(310)));
}

#[test]
fn arithmetic_in_an_assignment_still_names_its_line() {
    let src = "10 Y=1E38\n20 A$=\"X\":Z=Y*10\n";
    assert_eq!(error_at(src), (Some(OVERFLOW), Some(20)));
}

#[test]
fn plain_assignment_and_compare_lines_stamp_no_line_number() {
    let src = "10 X=1/X\n20 Z=X:A$=\"T\":IF A$=\"T\" AND Z=1 THEN B$=\"Q\"\n30 X=1/X\n";
    let prg = tokenize_program(src).expect("tokenizes");
    let asm = compile_with_options(&prg, CompileOptions::default())
        .expect("compiles")
        .asm;
    assert!(!asm.contains("LDA #$14"), "line 20 stamps itself:\n{asm}");
    assert!(asm.contains("LDA #$1E"), "line 30 still stamps itself:\n{asm}");
}

#[test]
fn string_copies_and_get_name_the_next_failing_line() {
    let src = "\
10 X=0:B$=\"Q\":D$=B$+B$
20 A$=B$:E$=D$:GET C$:IF C$=\"\" THEN F$=A$
300 Y=1/X
";
    assert_eq!(error_at(src), (Some(DIVISION_BY_ZERO), Some(300)));
}

#[test]
fn a_get_loop_stamps_no_line_number() {
    let src = "10 X=1/X\n20 GET A$:IF A$=\"\" THEN 20\n30 X=1/X\n";
    let prg = tokenize_program(src).expect("tokenizes");
    let asm = compile_with_options(&prg, CompileOptions::default())
        .expect("compiles")
        .asm;
    assert!(!asm.contains("LDA #$14"), "line 20 stamps itself:\n{asm}");
    assert!(asm.contains("LDA #$1E"), "line 30 still stamps itself:\n{asm}");
}

#[test]
fn peeks_of_high_literal_addresses_stamp_no_line_number() {
    let src = "10 X=1/X\n20 A%=PEEK(53280):B=PEEK(50003):IF PEEK(56320)=127 THEN C=1\n30 X=1/X:PRINT A%;B;C\n";
    let prg = tokenize_program(src).expect("tokenizes");
    let asm = compile_with_options(&prg, CompileOptions::default())
        .expect("compiles")
        .asm;
    assert!(!asm.contains("LDA #$14"), "line 20 stamps itself:\n{asm}");
    assert!(asm.contains("LDA #$1E"), "line 30 still stamps itself:\n{asm}");
}

#[test]
fn a_peek_of_an_address_past_the_top_still_names_its_line() {
    let src = "10 A$=\"X\"\n20 B=PEEK(65536)\n30 PRINT B\n";
    assert_eq!(error_at(src), (Some(ILLEGAL_QUANTITY), Some(20)));
}
