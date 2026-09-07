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

#[track_caller]
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
            common::petscii_to_string(&r.output).trim(),
            out,
            "({profile:?}):\n{src}"
        );
    }
}

#[track_caller]
fn check_banner(src: &str, banner: &str) {
    for profile in PROFILES {
        let prg = build(src, profile);
        let r = emu64::run_prg_to_end(&prg, common::MAX_INSNS).expect("run");
        assert!(!r.clean_exit, "must not exit cleanly ({profile:?}):\n{src}");
        assert_eq!(
            r.error_banner().as_deref(),
            Some(banner),
            "({profile:?}):\n{src}"
        );
    }
}

#[track_caller]
fn check_triple(op: &str, want: &str) {
    check_ok(&format!("10 print 3{op}3;2{op}5;5{op}2\n"), want);
}

#[track_caller]
fn check_triple_via_vars(op: &str, want: &str) {
    check_ok(
        &format!("10 a=3:b=2:c=5\n20 print a{op}a;b{op}c;c{op}b\n"),
        want,
    );
}

#[test]
fn single_token_relations_match_the_rom() {
    check_triple(">", "0  0 -1");
    check_triple("=", "-1  0  0");
    check_triple("<", "0 -1  0");
    check_triple_via_vars(">", "0  0 -1");
    check_triple_via_vars("=", "-1  0  0");
    check_triple_via_vars("<", "0 -1  0");
}

#[test]
fn two_token_aliases_are_symmetric_and_match_the_rom() {

    check_triple(">=", "-1  0 -1");
    check_triple("=>", "-1  0 -1");

    check_triple("<>", "0 -1 -1");
    check_triple("><", "0 -1 -1");

    check_triple("<=", "-1 -1  0");
    check_triple("=<", "-1 -1  0");
}

#[test]
fn two_token_aliases_survive_through_variables() {
    check_triple_via_vars(">=", "-1  0 -1");
    check_triple_via_vars("=>", "-1  0 -1");
    check_triple_via_vars("<>", "0 -1 -1");
    check_triple_via_vars("><", "0 -1 -1");
    check_triple_via_vars("<=", "-1 -1  0");
    check_triple_via_vars("=<", "-1 -1  0");
}

#[test]
fn three_distinct_tokens_are_true_for_every_pair() {
    for op in ["><=", ">=<", "<>=", "<=>", "=><", "=<>"] {
        check_triple(op, "-1 -1 -1");
    }
}

#[test]
fn mask_seven_survives_through_variables() {
    for op in ["><=", ">=<", "<>=", "<=>", "=><", "=<>"] {
        check_triple_via_vars(op, "-1 -1 -1");
    }
}

#[test]
fn mask_seven_in_an_if_always_branches() {
    check_ok("10 a=7:b=9\n20 if a>=<b then print\"t\"\n30 print\"e\"\n", "T\nE");
    check_ok("10 a=9:b=7\n20 if a>=<b then print\"t\"\n30 print\"e\"\n", "T\nE");
    check_ok("10 a=7:b=7\n20 if a=<>b then print\"t\"\n30 print\"e\"\n", "T\nE");
}

/// A fourth token always repeats a bit, so `?SYNTAX ERROR`.
#[test]
fn a_fourth_token_repeats_a_bit_and_raises() {
    check_banner("10 print 3>=<>3\n", "?SYNTAX  ERROR IN 10");
    check_banner("10 print 3<=><3\n", "?SYNTAX  ERROR IN 10");
}

/// Every run that repeats a bit is `?SYNTAX ERROR` ; all 24 the ROM rejected, none of the 15 it accepted.
#[test]
fn every_repeated_bit_is_a_runtime_syntax_error() {
    for op in [
        ">>", ">>>", ">><", ">>=", "><>", "><<", ">=>", ">==", "<<", "<<>", "<<<", "<<=", "<>>",
        "<><", "<=<", "<==", "=>>", "=>=", "=<<", "=<=", "==", "==>", "==<", "===",
    ] {
        check_banner(&format!("10 print 3{op}3\n"), "?SYNTAX  ERROR IN 10");
    }
}

#[test]
fn an_unreachable_repeated_bit_still_compiles_and_runs() {
    check_ok("10 print\"a\"\n20 end\n30 print 3>>3\n", "A");
}

#[test]
fn mask_seven_still_evaluates_the_left_operand() {
    check_banner(
        "10 dim a(2)\n20 i=9\n30 if a(i)>=<0 then print\"t\"\n40 print\"e\"\n",
        "?BAD SUBSCRIPT  ERROR IN 30",
    );
}

#[test]
fn mask_seven_still_evaluates_the_right_operand() {
    check_banner(
        "10 dim a(2)\n20 i=9\n30 if 0>=<a(i) then print\"t\"\n40 print\"e\"\n",
        "?BAD SUBSCRIPT  ERROR IN 30",
    );
}

#[test]
fn mask_seven_still_raises_from_an_operand_expression() {
    check_banner(
        "10 z=0\n20 if 1/z>=<1 then print\"t\"\n30 print\"e\"\n",
        "?DIVISION BY ZERO  ERROR IN 20",
    );
}

#[test]
fn string_mask_seven_is_true_for_unequal_strings() {
    check_ok(
        "10 a$=\"x\"\n20 if a$=<>\"y\" then print\"m1\"\n30 if a$<>=\"y\" then print\"m2\"\n",
        "M1\nM2",
    );
}

#[test]
fn string_mask_seven_is_true_for_equal_strings_too() {
    check_ok(
        "10 a$=\"x\"\n20 if a$=<>\"x\" then print\"m1\"\n30 if a$><=\"x\" then print\"m2\"\n",
        "M1\nM2",
    );
}

/// `40 PRINT A$=<>"Y";A$=<>"X"` printed `-1 -1`.
#[test]
fn string_mask_seven_prints_minus_one_for_both() {
    check_ok("10 a$=\"x\"\n20 print a$=<>\"y\";a$=<>\"x\"\n", "-1 -1");
}

/// `50 IF A$=<"Y"` ; mask 6 on strings is lexicographic `<=`, and it took the branch.
#[test]
fn string_two_token_aliases_still_work() {
    check_ok(
        "10 a$=\"x\"\n20 if a$=<\"y\" then print\"le\"\n30 if a$><\"y\" then print\"ne\"\n\
         40 if a$=>\"x\" then print\"ge\"\n",
        "LE\nNE\nGE",
    );
}

/// A repeated bit is still `?SYNTAX ERROR` on the string path.
#[test]
fn string_repeated_bit_is_a_runtime_syntax_error() {
    check_banner("10 a$=\"x\"\n20 print a$==\"y\"\n", "?SYNTAX  ERROR IN 20");
    check_banner("10 a$=\"x\"\n20 print a$<<\"y\"\n", "?SYNTAX  ERROR IN 20");
}

#[test]
fn chained_comparisons_still_parse_left_associatively() {
    check_ok("10 print 3=3=3\n", "0");
    check_ok("10 print 3=3=-1\n", "-1");
    check_ok("10 a=3\n20 print a=a=a\n", "0");
}
