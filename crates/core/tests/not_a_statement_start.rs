//! A byte that cannot begin a statement is a runtime `?SYNTAX ERROR`, not a deleted statement.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

fn build(src: &str, profile: Profile) -> Vec<u8> {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(
        &prg,
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("must compile ({profile:?}): {e}\n{src}"))
    .prg_bytes
}

/// Assert `src` compiles, prints `out`, and exits cleanly with no error banner ; on every profile.
fn check_ok(src: &str, out: &str) {
    for profile in PROFILES {
        let prg = build(src, profile);
        let r = emu64::run_prg_to_end(&prg, common::MAX_INSNS).expect("run");
        assert_eq!(
            r.error_banner(),
            None,
            "no error expected ({profile:?}):\n{src}"
        );
        assert!(r.clean_exit, "clean exit expected ({profile:?}):\n{src}");
        assert_eq!(
            common::petscii_to_string(&r.output).trim_end(),
            out,
            "({profile:?}):\n{src}"
        );
    }
}

/// Assert `src` compiles, prints `out`, then dies with `banner`.
fn check_error(src: &str, out: &str, banner: &str) {
    for profile in PROFILES {
        let prg = build(src, profile);
        let r = emu64::run_prg_to_end(&prg, common::MAX_INSNS).expect("run");
        assert!(!r.clean_exit, "must not exit cleanly ({profile:?}):\n{src}");
        assert_eq!(
            common::petscii_to_string(&r.output).trim_end(),
            out,
            "output before the error ({profile:?}):\n{src}"
        );
        assert_eq!(
            r.error_banner().as_deref(),
            Some(banner),
            "({profile:?}):\n{src}"
        );
    }
}

#[test]
fn tab_inside_print_still_works() {
    check_ok("10 print tab(5);\"a\"\n", "     A");
}

#[test]
fn spc_inside_print_still_works() {
    check_ok("10 print\"a\";spc(3);\"b\"\n", "A   B");
}

#[test]
fn go_to_still_works() {
    check_ok("10 go to 30\n20 print\"skipped\"\n30 print\"a\"\n", "A");
}

/// Function tokens are `$B4..$CA`.
#[test]
fn function_tokens_in_expressions_still_work() {
    check_ok("10 print sgn(-5);int(2.7);abs(-3)\n", "-1  2  3");
}

/// Operator tokens `$AA..$B3` in their ordinary infix position.
#[test]
fn operator_tokens_in_expressions_still_work() {
    check_ok("10 print 2+3*4;7>2 and 1<2\n", " 14 -1");
}

#[test]
fn implicit_let_still_works() {
    check_ok("10 a=5:print a\n", " 5");
}

#[test]
fn an_unreachable_stray_byte_line_compiles_and_runs() {
    check_ok("10 print\"a\"\n20 end\n30 ]\n", "A");
}

/// Same for a whole `]`-prefixed statement with operands, in an uncalled subroutine.
#[test]
fn a_stray_byte_in_an_uncalled_subroutine_compiles_and_runs() {
    check_ok("10 print\"a\"\n20 end\n100 ]1,2,3\n110 return\n", "A");
}

/// Guarded by a false condition ; reachability, not syntax, decides.
#[test]
fn a_stray_byte_under_a_false_condition_is_free() {
    check_ok("10 if 0 then ]\n20 print\"b\"\n", "B");
}

/// A statement-initial operator token on an unreachable line.
#[test]
fn an_unreachable_operator_token_line_is_free() {
    check_ok("10 print\"a\"\n20 end\n30 and 5\n", "A");
}

#[test]
fn a_reached_stray_byte_raises_syntax_error_at_its_own_line() {
    check_error(
        "10 print\"a\"\n20 ]\n30 print\"c\"\n",
        "A",
        "?SYNTAX  ERROR IN 20",
    );
}

/// An operator token, `$AF` (`AND`).
#[test]
fn a_reached_operator_token_raises_syntax_error() {
    check_error(
        "10 print\"a\"\n20 and 5\n30 print\"c\"\n",
        "A",
        "?SYNTAX  ERROR IN 20",
    );
}

/// A function token, `$B4` (`SGN`).
#[test]
fn a_reached_function_token_raises_syntax_error() {
    check_error(
        "10 print\"a\"\n20 sgn(1)\n30 print\"c\"\n",
        "A",
        "?SYNTAX  ERROR IN 20",
    );
}

/// `TAB(` (`$A3`) statement-initial.
#[test]
fn a_reached_statement_initial_tab_raises_syntax_error() {
    check_error(
        "10 print\"a\"\n20 tab(5)\n30 print\"c\"\n",
        "A",
        "?SYNTAX  ERROR IN 20",
    );
}

#[test]
fn the_trap_is_not_hoisted_to_the_head_of_its_line() {
    check_error(
        "10 print\"a\":]\n20 print\"b\"\n",
        "A",
        "?SYNTAX  ERROR IN 10",
    );
}

/// Everything after it on the line is dead ; `C` never prints.
#[test]
fn the_rest_of_the_line_is_dead_after_the_trap() {
    check_error(
        "10 print\"a\":]:print\"c\"\n20 print\"d\"\n",
        "A",
        "?SYNTAX  ERROR IN 10",
    );
}

/// Reached under a true condition, inside a `THEN` body.
#[test]
fn a_stray_byte_under_a_true_condition_raises() {
    check_error("10 if 1 then ]\n20 print\"b\"\n", "", "?SYNTAX  ERROR IN 10");
}

/// The cited line is the bad one, not the first.
#[test]
fn the_cited_line_is_the_bad_line() {
    check_error(
        "10 print\"a\"\n2000 ]\n3000 print\"c\"\n",
        "A",
        "?SYNTAX  ERROR IN 2000",
    );
}
