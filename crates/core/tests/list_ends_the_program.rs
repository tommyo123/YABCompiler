use yabcompiler_core::ast::Statement;
use yabcompiler_core::tokenize_program;

mod common;

const MAX_INSNS: u64 = 20_000_000;

fn run(src: &str) -> String {
    let prg = common::compile_source(src).expect("compile");
    let r = emu64::run_prg_to_end(&prg, MAX_INSNS).expect("run");
    assert!(r.clean_exit, "program should terminate: {src:?}");
    assert_eq!(r.error_code, None, "no error expected: {src:?}");
    common::petscii_to_string(&r.output)
}

#[test]
fn list_parses_to_end() {
    for src in [
        "10 list\n",
        "10 list 10\n",
        "10 list 100-200\n",
        "10 list -50\n",
        "10 list 50-\n",
    ] {
        let bytes = tokenize_program(src).expect("tokenize");
        let prg = yabcompiler_core::Program::parse(&bytes).expect("is a BASIC prg");
        let ast = yabcompiler_core::parse::program(&prg).expect("parse");
        assert!(
            matches!(ast.lines[0].statements[0], Statement::End),
            "{src:?} should parse to Statement::End, got {:?}",
            ast.lines[0].statements[0]
        );
    }
}

/// Bare `LIST` stops the run.
#[test]
fn list_stops_the_run() {
    assert_eq!(
        run("10 print \"before\"\n20 list\n30 print \"after\"\n"),
        "BEFORE\n"
    );
}

#[test]
fn list_mid_line_stops_the_rest_of_the_line() {
    assert_eq!(
        run("10 print \"b\":list:print \"after\"\n20 print \"next\"\n"),
        "B\n"
    );
}

/// An argument does not change that, and does not fail to parse.
#[test]
fn list_with_a_range_stops_the_run() {
    for src in [
        "10 print \"before\"\n20 list 10\n30 print \"after\"\n",
        "10 print \"before\"\n20 list 9000-9001\n30 print \"after\"\n",
        "10 print \"before\"\n20 list 10-30\n30 print \"after\"\n",
    ] {
        assert_eq!(run(src), "BEFORE\n", "{src:?}");
    }
}

#[test]
fn list_in_a_subroutine_does_not_return() {
    assert_eq!(
        run("10 gosub 100\n20 print \"back\"\n30 end\n100 print \"sub\"\n110 list\n120 return\n"),
        "SUB\n"
    );
}

#[test]
fn list_only_ends_when_it_is_actually_reached() {
    assert_eq!(
        run("10 if 0 then list\n20 print \"reached\"\n"),
        "REACHED\n",
        "an untaken LIST must not end the program"
    );
    assert_eq!(
        run("10 if 1 then list\n20 print \"reached\"\n"),
        "",
        "a taken LIST must end the program"
    );
}

#[test]
fn the_listing_text_itself_is_a_recorded_divergence() {
    let out = run("10 print \"before\"\n20 list\n30 print \"after\"\n");
    assert!(
        !out.contains("PRINT"),
        "compiled LIST does not reproduce the ROM's listing text; \
         native emits `10 PRINT \"BEFORE\"` etc. after BEFORE. \
         got {out:?}"
    );
}
