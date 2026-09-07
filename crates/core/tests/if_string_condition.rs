//! Runtime behavior of string expressions in IF conditions.

use yabcompiler_core::ast::{Expr, Statement, StrExpr};
use yabcompiler_core::tokenize_program;

mod common;

const MAX_INSNS: u64 = 20_000_000;

fn run(src: &str) -> String {
    let prg = common::compile_source(src).expect("compile");
    let r = emu64::run_prg_to_end(&prg, MAX_INSNS).expect("run");
    assert!(r.clean_exit, "program should terminate: {src:?}");
    common::petscii_to_string(&r.output)
}

fn stmts(src: &str) -> Vec<Statement> {
    let bytes = tokenize_program(src).expect("tokenize");
    let prg = yabcompiler_core::Program::parse(&bytes).expect("is a BASIC prg");
    let ast = yabcompiler_core::parse::program(&prg).expect("parse");
    ast.lines.into_iter().flat_map(|l| l.statements).collect()
}

/// Empty strings make IF conditions false.
#[test]
fn empty_string_conditions_print_nothing() {
    let src = "1 B=0:IF A$ THEN PRINT \"1\"\n\
               2 C=1:IF A$ THEN PRINT \"2\"\n\
               3 REM\n";
    assert_eq!(run(src), "");
}

#[test]
fn bare_string_condition_lowers_to_len() {
    let s = stmts("10 IF A$ THEN PRINT \"1\"\n");
    let Statement::If { cond, .. } = &s[0] else {
        panic!("expected If, got {:?}", s[0]);
    };
    assert!(
        matches!(cond, Expr::Len(b) if matches!(**b, StrExpr::Var(_))),
        "IF <string> must lower to LEN(<string>), got {cond:?}"
    );
}

/// The sign-flipping case: non-empty string, preceding number zero.
#[test]
fn nonempty_string_condition_is_true_for_us_and_false_natively() {
    let src = "1 A$=\"X\":B=0:IF A$ THEN PRINT \"1\"\n";
    assert_eq!(run(src), "1\n");
}

#[test]
fn numeric_condition_control_matches_native() {
    let src = "1 B=0:IF B THEN PRINT \"1\"\n\
               2 C=1:IF C THEN PRINT \"2\"\n\
               3 REM\n";
    assert_eq!(run(src), "2\n");
}

/// A non-empty literal string condition is true, an empty one is false ; the `LEN` semantics, stated directly.
#[test]
fn string_condition_truthiness_is_length() {
    assert_eq!(run("10 A$=\"X\":IF A$ THEN PRINT \"T\"\n"), "T\n");
    assert_eq!(
        run("10 A$=\"\":IF A$ THEN PRINT \"T\"\n20 PRINT \"E\"\n"),
        "E\n"
    );
}

#[test]
fn string_comparison_condition_still_compares() {
    let s = stmts("10 IF A$=\"X\" THEN PRINT \"T\"\n");
    let Statement::If { cond, .. } = &s[0] else {
        panic!("expected If, got {:?}", s[0]);
    };
    assert!(
        matches!(cond, Expr::StrCompare(..)),
        "IF A$=\"X\" must stay a comparison, got {cond:?}"
    );
    assert_eq!(run("10 A$=\"X\":IF A$=\"X\" THEN PRINT \"T\"\n"), "T\n");
    assert_eq!(
        run("10 A$=\"Y\":IF A$=\"X\" THEN PRINT \"T\"\n20 PRINT \"E\"\n"),
        "E\n"
    );
}
