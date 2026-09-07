//! `STOP` prints `BREAK IN <line>` and hands control to BASIC's warm start.

mod common;

use yabcompiler_core::{CompileOptions, compile_with_options, tokenize_program};

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
