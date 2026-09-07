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

/// Compiles, prints `out`, exits clean with no banner ; on all profiles.
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

/// Compiles, prints `out`, then dies with `banner` ; on all profiles.
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

const TYPE_MISMATCH: &str = "?TYPE MISMATCH  ERROR IN ";
const SYNTAX: &str = "?SYNTAX  ERROR IN ";
const UNDEFD: &str = "?UNDEF'D STATEMENT  ERROR IN ";

#[test]
fn a_reached_numeric_variable_in_a_string_context_raises_type_mismatch() {
    check_error(
        "10 j=5\n20 j$=j+\" x\"\n30 print\"c\"\n",
        "",
        &format!("{TYPE_MISMATCH}20"),
    );
}

#[test]
fn a_numeric_variable_left_of_a_string_concat_raises_type_mismatch() {
    check_error(
        "10 j=5\n20 j$=\" x\"+j\n30 print\"c\"\n",
        "",
        &format!("{TYPE_MISMATCH}20"),
    );
}

/// `IF A$=S` ; a string compared against a numeric variable.
#[test]
fn a_string_compared_to_a_numeric_variable_raises_type_mismatch() {
    check_error(
        "10 s=5:a$=\"q\"\n20 if a$=s then print\"y\"\n30 print\"c\"\n",
        "",
        &format!("{TYPE_MISMATCH}20"),
    );
}

/// `VAL(<numeric>)` ; the argument slot of a string function.
#[test]
fn val_of_a_numeric_variable_raises_type_mismatch() {
    check_error(
        "10 pz=3\n20 on val(pz) goto 100\n30 print\"c\"\n100 print\"t\"\n",
        "",
        &format!("{TYPE_MISMATCH}20"),
    );
}

#[test]
fn a_dropped_opening_quote_raises_type_mismatch_at_its_own_line() {
    check_error(
        "10 a$=whoosshh\":printa$\n20 print\"c\"\n",
        "",
        &format!("{TYPE_MISMATCH}10"),
    );
}

#[test]
fn a_dropped_quote_ending_in_punctuation_raises_type_mismatch() {
    check_error(
        "10 p$=facedagain!\":goto 100\n20 print\"c\"\n100 print\"t\"\n",
        "",
        &format!("{TYPE_MISMATCH}10"),
    );
}

#[test]
fn an_unreached_numeric_in_string_context_compiles_and_runs_clean() {
    check_ok("10 print\"a\"\n20 end\n30 j$=j+\" x\"\n", "A");
}

/// Numeric operands in string assignments raise TYPE MISMATCH.
#[test]
fn an_untaken_then_branch_with_a_numeric_in_string_context_runs_clean() {
    check_ok("10 r=1\n20 if r=59 then j$=j+\" x\"\n30 print\"c\"\n", "C");
}

#[test]
fn a_taken_then_branch_with_a_numeric_in_string_context_raises() {
    check_error(
        "10 r=59\n20 if r=59 then j$=j+\" x\"\n30 print\"c\"\n",
        "",
        &format!("{TYPE_MISMATCH}20"),
    );
}

#[test]
fn output_before_a_numeric_in_string_context_still_prints() {
    check_error(
        "10 print\"a\":j$=j+\" x\"\n20 print\"c\"\n",
        "A",
        &format!("{TYPE_MISMATCH}10"),
    );
}

#[test]
fn ordinary_string_expressions_still_compile_and_run() {
    check_ok("10 a$=\"q\":b$=a$+\"r\"\n20 print b$\n", "QR");
    check_ok("10 a=5:a$=str$(a)+\"!\"\n20 print a$\n", " 5!");
    check_ok("10 a$=\"q\":if a$=\"q\" then print\"y\"\n", "Y");
    check_ok("10 print left$(\"abc\",2);mid$(\"abc\",3)\n", "ABC");
}

#[test]
fn a_string_literal_right_of_a_numeric_operator_raises_type_mismatch() {
    check_error("10 a=1+\"q\"\n20 print\"c\"\n", "", &format!("{TYPE_MISMATCH}10"));
}

/// A literal in an argument slot.
#[test]
fn a_string_literal_as_a_poke_operand_raises_type_mismatch() {
    check_error(
        "10 poke 1024,\"q\"\n20 print\"c\"\n",
        "",
        &format!("{TYPE_MISMATCH}10"),
    );
}

#[test]
fn a_numeric_variable_compared_to_a_string_literal_raises_type_mismatch() {
    check_error(
        "10 a=1\n20 if a=\"q\" then print\"y\"\n30 print\"c\"\n",
        "",
        &format!("{TYPE_MISMATCH}20"),
    );
    check_error(
        "10 a=1\n20 if a<>\"n\" then print\"y\"\n30 print\"c\"\n",
        "",
        &format!("{TYPE_MISMATCH}20"),
    );
}

#[test]
fn a_parenthesised_string_literal_in_numeric_position_raises_type_mismatch() {
    check_error("10 a=(\"q\")\n20 print\"c\"\n", "", &format!("{TYPE_MISMATCH}10"));
}

/// Unreached, it costs nothing ; the whole reason not to refuse.
#[test]
fn an_unreached_string_literal_in_numeric_position_runs_clean() {
    check_ok("10 print\"a\":end\n20 a=1+\"q\"\n", "A");
}

#[test]
fn output_before_a_string_literal_in_numeric_position_still_prints() {
    check_error(
        "10 print\"a\":a=1+\"q\"\n20 print\"c\"\n",
        "A",
        &format!("{TYPE_MISMATCH}10"),
    );
}

#[test]
fn string_literals_in_string_position_are_unaffected() {
    check_ok("10 print\"a\"+\"b\"\n", "AB");
    check_ok("10 if \"a\"<\"b\" then print\"y\"\n", "Y");
    check_ok("10 print len(\"abcd\")\n", " 4");
    check_ok("10 a$=\"q\":if a$>\"p\" then print\"y\"\n", "Y");
    check_ok("10 print val(\"12\")+1\n", " 13");
    check_ok("10 print asc(\"a\")\n", " 65");
}

#[test]
fn a_reached_overlarge_goto_target_raises_syntax_error() {
    check_error(
        "10 print\"a\"\n20 goto 70000\n30 print\"b\"\n",
        "A",
        &format!("{SYNTAX}20"),
    );
}

#[test]
fn a_reached_overlarge_gosub_target_raises_syntax_error() {
    check_error(
        "10 print\"a\"\n20 gosub 70000\n30 print\"b\"\n",
        "A",
        &format!("{SYNTAX}20"),
    );
}

#[test]
fn a_reached_overlarge_then_line_raises_syntax_error() {
    check_error(
        "10 print\"a\"\n20 if 1 then 70000\n30 print\"b\"\n",
        "A",
        &format!("{SYNTAX}20"),
    );
}

#[test]
fn the_linget_ceiling_is_63999_not_65535() {
    check_error(
        "10 print\"a\"\n20 goto 63999\n30 print\"b\"\n",
        "A",
        &format!("{UNDEFD}20"),
    );
    check_error(
        "10 print\"a\"\n20 goto 64000\n30 print\"b\"\n",
        "A",
        &format!("{SYNTAX}20"),
    );
    check_error(
        "10 print\"a\"\n20 goto 65535\n30 print\"b\"\n",
        "A",
        &format!("{SYNTAX}20"),
    );
}

#[test]
fn spaces_swallowed_into_a_target_can_overflow_it() {
    check_error(
        "10 print\"a\"\n20 goto 3360          3344\n30 print\"b\"\n",
        "A",
        &format!("{SYNTAX}20"),
    );
}

/// Unreached, it costs nothing.
#[test]
fn an_unreached_overlarge_target_compiles_and_runs_clean() {
    check_ok("10 print\"a\"\n20 end\n30 goto 70000\n", "A");
}

#[test]
fn an_untaken_overlarge_then_line_runs_clean() {
    check_ok("10 print\"a\"\n20 if 0 then 70000\n30 print\"b\"\n", "A\nB");
}

#[test]
fn targets_that_fit_still_branch_and_still_trap_as_undefd() {
    check_ok("10 goto 100\n20 print\"x\"\n100 print\"a\"\n", "A");
    check_ok("10 gosub 100\n20 end\n100 print\"a\":return\n", "A");
    check_error(
        "10 print\"a\"\n20 goto 999\n",
        "A",
        &format!("{UNDEFD}20"),
    );
}

const ON_LIST: &str = "20 on x goto 100,200 300,400\n\
                       30 print\"fall\"\n\
                       40 end\n\
                       100 print\"t1\":end\n\
                       200 print\"t2\":end\n\
                       300 print\"t3\":end\n\
                       400 print\"t4\":end\n";

fn on_list_program(x: u32) -> String {
    format!("10 x={x}\n{ON_LIST}")
}

#[test]
fn the_first_on_goto_entry_still_branches_past_a_later_overflow() {
    check_ok(&on_list_program(1), "T1");
}

#[test]
fn every_other_on_goto_index_raises_syntax_error() {
    for x in [0, 2, 3, 4, 5] {
        check_error(&on_list_program(x), "", &format!("{SYNTAX}20"));
    }
}

/// When the first entry is the bad one, nothing survives and the whole statement is the trap.
#[test]
fn an_overflow_in_the_first_on_goto_entry_traps_every_index() {
    for x in [0, 1, 2] {
        check_error(
            &format!("10 x={x}\n20 on x goto 70000,100\n30 print\"fall\"\n100 print\"t1\"\n"),
            "",
            &format!("{SYNTAX}20"),
        );
    }
}

/// Known narrowing.
#[test]
fn on_gosub_with_a_late_overflow_traps_the_whole_statement() {
    check_error(
        "10 x=1\n20 on x gosub 100,200 300\n30 print\"fall\"\n100 print\"t1\":return\n",
        "",
        &format!("{SYNTAX}20"),
    );
}

#[test]
fn a_well_formed_on_goto_list_is_unchanged() {
    check_ok(
        "10 x=2\n20 on x goto 100,200\n30 print\"fall\"\n100 print\"t1\":end\n200 print\"t2\":end\n",
        "T2",
    );
    check_ok(
        "10 x=7\n20 on x goto 100,200\n30 print\"fall\":end\n100 print\"t1\":end\n200 print\"t2\":end\n",
        "FALL",
    );
}

#[test]
fn run_to_a_missing_line_raises_undefd_statement() {
    check_error(
        "10 print\"a\"\n20 run 999\n30 print\"b\"\n",
        "A",
        &format!("{UNDEFD}20"),
    );
}

/// Unreached, it costs nothing.
#[test]
fn an_unreached_run_to_a_missing_line_compiles_and_runs_clean() {
    check_ok("10 print\"a\"\n20 end\n30 run 999\n", "A");
}

#[test]
fn run_to_a_line_that_exists_still_branches_and_still_resets() {
    check_ok("10 print\"a\"\n20 goto 40\n30 print\"b\":end\n40 run 30\n", "A\nB");
    // RUN's CLR is observable: A is 7 before the RUN and 0 after it.
    check_ok(
        "10 a=7\n20 goto 40\n30 print a:end\n40 run 30\n",
        " 0",
    );
}
