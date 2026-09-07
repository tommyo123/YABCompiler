//! `STOP` prints `BREAK IN <line>` and hands control to BASIC's warm start.

mod common;

use yabcompiler_core::{CompileOptions, compile_with_options, tokenize_program};

fn break_bytes(src: &str) -> Vec<u8> {
    let prg = common::compile_source(src).unwrap_or_else(|e| panic!("compile: {e}"));
    let r = emu64::run_prg_to_end(&prg, 40_000_000).expect("runs");
    assert!(
        r.ready_reached,
        "STOP never handed control back to BASIC (READY not reached) for:\n{src}"
    );
    r.output
}

/// Run to BASIC's READY prompt and return everything the program sent to CHROUT.
fn break_output(src: &str) -> String {
    let prg = common::compile_source(src).unwrap_or_else(|e| panic!("compile: {e}"));
    let r = emu64::run_prg_to_end(&prg, 40_000_000).expect("runs");
    assert!(
        r.ready_reached,
        "STOP never handed control back to BASIC (READY not reached) for:\n{src}"
    );
    common::petscii_to_string(&r.output)
}

fn asm_of(src: &str) -> String {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(&prg, CompileOptions::default())
        .unwrap_or_else(|e| panic!("compiles: {e}"))
        .asm
}

/// The message itself: a leading CR (that is the blank line native leaves above it), no doubled
/// space, and the line number once.
#[test]
fn stop_prints_break_in_the_current_line() {
    assert_eq!(break_output("10 STOP\n"), "\nBREAK IN 10");
    assert_eq!(
        break_output("10 PRINT \"A\"\n20 STOP\n"),
        "A\n\nBREAK IN 20"
    );
}

/// A `STOP` reached through GOSUB reports the line it is on, not the caller's.
#[test]
fn stop_inside_a_subroutine_reports_its_own_line() {
    assert_eq!(
        break_output("10 GOSUB 100\n20 END\n100 STOP\n"),
        "\nBREAK IN 100"
    );
}

/// Line numbers above 32767.
#[test]
fn stop_on_a_line_above_32767_prints_and_returns_to_ready() {
    assert_eq!(
        break_output("10 PRINT \"X\"\n40000 STOP\n"),
        "X\n\nBREAK IN 40000"
    );
    assert_eq!(
        break_output("10 PRINT \"Y\"\n63999 STOP\n"),
        "Y\n\nBREAK IN 63999"
    );
}

#[test]
fn stop_emits_the_roms_cr_lf_before_break() {
    assert_eq!(
        break_bytes("10 STOP\n"),
        b"\r\nBREAK IN 10".to_vec(),
        "the ROM's BREAK message at $A381 is 0D 0A 'BREAK'; the line \
         feed is a ROM string byte, not decoration"
    );
}

fn petscii(s: &str) -> Vec<u8> {
    s.bytes()
        .map(|b| if b.is_ascii_uppercase() { b + 0x80 } else { b })
        .collect()
}

#[test]
fn the_line_feed_is_not_doubled_and_not_transposed() {
    assert_eq!(
        break_bytes("10 PRINT \"A\"\n20 STOP\n"),
        [petscii("A"), b"\r\r\nBREAK IN 20".to_vec()].concat()
    );
    assert_eq!(
        break_bytes("10 PRINT \"X\"\n40000 STOP\n"),
        [petscii("X"), b"\r\r\nBREAK IN 40000".to_vec()].concat()
    );
}

#[test]
fn an_error_banner_still_has_a_bare_cr_and_no_line_feed() {
    let prg = common::compile_source("10 PRINT \"A\"\n20 PRINT 1/PEEK(2)\n")
        .unwrap_or_else(|e| panic!("compile: {e}"));
    let r = emu64::run_prg_to_end(&prg, 40_000_000).expect("runs");
    assert_eq!(
        r.output,
        [petscii("A"), b"\r".to_vec()].concat(),
        "program output, before the ERROR routine takes the stream over"
    );
    assert_eq!(
        r.error_output,
        b"\r?DIVISION BY ZERO  ERROR IN 20".to_vec(),
        "$A369 is \"  ERROR\" with no CR/LF and the ERROR routine's CRDO \
         emits a bare $0D — one, and no line feed"
    );
}

/// `END` shares the STOP statement's exit but takes the `BCC $A854` branch in the ROM, so it prints nothing at all.
#[test]
fn end_prints_no_banner_at_all() {
    let prg = common::compile_source("10 PRINT \"A\"\n20 END\n")
        .unwrap_or_else(|e| panic!("compile: {e}"));
    let r = emu64::run_prg_to_end(&prg, 40_000_000).expect("runs");
    assert_eq!(r.output, [petscii("A"), b"\r".to_vec()].concat());
}

#[test]
fn linprt_is_not_followed_by_a_second_strout() {
    let asm = asm_of("10 STOP\n");
    let body: Vec<&str> = asm
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with(';'))
        .collect();
    let i = body
        .iter()
        .position(|l| *l == "JSR $BDCD")
        .unwrap_or_else(|| panic!("no LINPRT call in:\n{asm}"));
    assert_eq!(
        body[i + 1],
        "JMP $E37B",
        "LINPRT already ends in STROUT; the next instruction must be the \
         warm-start jump, not another print. Got:\n{:?}",
        &body[i..(i + 3).min(body.len())]
    );
}
