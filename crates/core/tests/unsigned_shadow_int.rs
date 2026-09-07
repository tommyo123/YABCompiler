mod common;

use yabcompiler_core::{CompileOptions, compile_with_options, tokenize_program};

fn asm_of(src: &str) -> String {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(&prg, CompileOptions::default())
        .unwrap_or_else(|e| panic!("compiles: {e}"))
        .asm
}

fn out(src: &str) -> String {
    let prg = common::compile_source(src).unwrap_or_else(|e| panic!("compile: {e}"));
    common::run_clean(&prg).unwrap_or_else(|| panic!("no clean exit:\n{src}"))
}

#[test]
fn a_u16_for_loop_terminates_and_pokes_the_right_bytes() {
    let src = "10 N=0\n\
               20 FOR I=51200 TO 51204\n\
               30 POKE I,I-51100 : N=N+1 : NEXT\n\
               40 PRINT N\n\
               50 FOR J=51200 TO 51204 : PRINT PEEK(J); : NEXT\n\
               60 PRINT\n";
    assert_eq!(out(src), " 5 \n 100  101  102  103  104 \n");
}

/// The loop variable itself has to survive as a *float* too: after the loop, `I` is one past
/// the end, and the ROM prints it unsigned.
#[test]
fn a_u16_for_counter_reads_back_as_an_unsigned_float() {
    assert_eq!(
        out("10 FOR I=51200 TO 51204 : NEXT\n20 PRINT I\n"),
        " 51205 \n"
    );

    assert_eq!(
        out("10 FOR I=51200 TO 51202 : PRINT I; : NEXT\n20 PRINT\n"),
        " 51200  51201  51202 \n"
    );
}

#[test]
fn an_unsigned_shadow_scalar_prints_unsigned() {
    assert_eq!(out("10 UC=55296 : PRINT UC\n"), " 55296 \n");
    assert_eq!(
        out("10 UC=49152 : POKE UC,1 : PRINT UC, PEEK(UC)\n"),
        " 49152     1 \n"
    );
}

/// Arithmetic on the shadow stays a u16 across the FAC round trip.
#[test]
fn unsigned_shadow_arithmetic_stays_unsigned() {
    assert_eq!(out("10 A=51200 : A=A+300 : PRINT A\n"), " 51500 \n");
    assert_eq!(
        out("10 A=51200 : POKE A+5,7 : A=A+5 : PRINT A; PEEK(A)\n"),
        " 51205  7 \n"
    );
}

/// The signed `BVC` / `EOR #$80` int16 compare reads bit 15 as a sign, so it must not be handed
/// a u16 shadow.
#[test]
fn comparisons_against_an_unsigned_shadow_are_unsigned() {
    assert_eq!(
        out("10 A=51200 : POKE A,0 : IF A>100 THEN PRINT \"BIG\"\n"),
        "BIG\n"
    );
    assert_eq!(
        out("10 A=51200 : POKE A,0 : IF A<100 THEN PRINT \"SMALL\"\n20 PRINT \"DONE\"\n"),
        "DONE\n"
    );

    assert_eq!(
        out("10 A=51200 : POKE A,0 : IF A+40>100 THEN PRINT \"BIG\"\n"),
        "BIG\n"
    );
}

/// Unsigned shadows retain zero-page indirect POKE access.
#[test]
fn a_u16_poke_target_still_uses_the_zero_page_pointer_store() {
    let asm = asm_of("10 FOR I=51200 TO 52054 : POKE I,PEEK(I)+1 : NEXT\n");
    assert!(
        asm.contains("STA (ZSI_V_I),Y"),
        "u16 shadow lost its ZP indirect POKE fast path:\n{asm}"
    );
}

#[test]
fn a_u16_for_loop_converts_through_the_unsigned_helper() {
    let asm = asm_of("10 FOR I=51200 TO 52054 : POKE I,PEEK(I)+1 : NEXT\n");
    assert!(
        asm.contains("JSR __GIVAYF_U16"),
        "u16 shadow did not use the unsigned FAC conversion:\n{asm}"
    );
    assert!(
        asm.contains("__GIVAYF_U16:"),
        "helper referenced but not emitted:\n{asm}"
    );
}

#[test]
fn a_signed_shadow_still_uses_plain_givayf() {
    let asm = asm_of("10 FOR I=1024 TO 1064 : POKE I,PEEK(I)+1 : NEXT\n");
    assert!(
        !asm.contains("__GIVAYF_U16"),
        "signed shadow should not pull in the unsigned helper:\n{asm}"
    );
}

#[test]
fn a_signed_shadow_still_takes_the_int16_print_fast_path() {
    let asm = asm_of("10 FOR I=1 TO 5 : A=I*2 : POKE 1024+A,1 : PRINT A : NEXT\n");
    assert!(
        asm.contains("JSR __PRINT_INT16"),
        "int16 PRINT fast path stopped firing for a signed shadow:\n{asm}"
    );
}

/// CONTROL: programs with no unsigned shadows must not gain the helper or lose the signed
/// compare.
#[test]
fn programs_without_unsigned_shadows_are_unchanged() {
    let asm = asm_of("10 FOR I=1 TO 10 : IF I>5 THEN PRINT I\n20 NEXT\n");
    assert!(!asm.contains("__GIVAYF_U16"), "unexpected helper:\n{asm}");
    assert_eq!(
        out("10 FOR I=1 TO 4 : IF I>2 THEN PRINT I;\n20 NEXT\n30 PRINT\n"),
        " 3  4 \n"
    );
}
