//! An unparseable statement is a runtime `?SYNTAX ERROR`, not a compile-time refusal.

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
fn an_unreachable_bad_line_compiles_and_runs() {
    check_ok("10 print\"a\"\n20 end\n30 wibble 1\n", "A");
}

/// Same, with the bad line inside a subroutine nothing ever `GOSUB`s.
#[test]
fn a_bad_line_in_an_uncalled_subroutine_compiles_and_runs() {
    check_ok("10 print\"a\"\n20 end\n100 wibble 1\n110 return\n", "A");
}

#[test]
fn an_unreachable_malformed_operand_list_compiles_and_runs() {
    check_ok("10 print\"a\"\n20 end\n30 poke 1,\n", "A");
}

#[test]
fn an_untaken_then_branch_holding_a_bad_statement_runs_clean() {
    check_ok("10 if 0 then wibble 1\n20 print\"b\"\n", "B");
}

#[test]
fn a_bad_statement_in_one_arm_leaves_the_other_arm_alive() {
    check_ok(
        "10 if 0 then wibble 1 else print\"e\"\n20 print\"b\"\n",
        "E\nB",
    );
    check_ok(
        "10 if 1 then print\"t\" else wibble 1\n20 print\"b\"\n",
        "T\nB",
    );
}

#[test]
fn a_reached_typo_raises_syntax_error_at_its_own_line() {
    check_error(
        "10 prnit \"hello\"\n20 print\"after\"\n",
        "",
        "?SYNTAX  ERROR IN 10",
    );
    check_error(
        "10 pokd 53280,0\n20 print\"after\"\n",
        "",
        "?SYNTAX  ERROR IN 10",
    );
    check_error(
        "10 wibble 42\n20 print\"after\"\n",
        "",
        "?SYNTAX  ERROR IN 10",
    );
}

/// The bad line is cited, not the first line: `30`, not `10`.
#[test]
fn the_cited_line_is_the_bad_line_not_the_first() {
    check_error(
        "10 print\"a\"\n30 wibble 1\n40 print\"c\"\n",
        "A",
        "?SYNTAX  ERROR IN 30",
    );
}

/// The trap sits where the statement sat.
#[test]
fn the_trap_is_not_hoisted_to_the_head_of_the_line() {
    check_error(
        "10 print\"a\":wibble 1\n20 print\"b\"\n",
        "A",
        "?SYNTAX  ERROR IN 10",
    );
}

/// The rest of the line is dead too ; `C` never prints.
#[test]
fn the_rest_of_the_line_after_the_trap_is_dead() {
    check_error(
        "10 print\"a\":wibble 1:print\"c\"\n20 print\"d\"\n",
        "A",
        "?SYNTAX  ERROR IN 10",
    );
}

/// A taken `THEN` body does raise.
#[test]
fn a_taken_then_branch_holding_a_bad_statement_raises() {
    check_error(
        "10 if 1 then wibble 1\n20 print\"b\"\n",
        "",
        "?SYNTAX  ERROR IN 10",
    );
}

/// Inside a loop it raises on the first iteration.
#[test]
fn a_bad_line_in_a_loop_raises_on_the_first_pass() {
    check_error(
        "10 for i=1 to 3\n20 print i\n30 wibble 1\n40 next\n",
        " 1",
        "?SYNTAX  ERROR IN 30",
    );
}

#[test]
fn a_mistyped_next_raises_rather_than_falling_through() {
    check_error(
        "10 for i=1 to 3\n20 print\"x\";i\n30 nxet i\n40 print\"done\"\n",
        "X 1",
        "?SYNTAX  ERROR IN 30",
    );
}

/// Defer malformed assignments until the statement is reached.
#[test]
fn the_two_real_programs_shape_raises_where_the_rom_raises() {
    check_error(
        "5 console 1:print chr$(147);\n20 print\"after\"\n",
        "",
        "?SYNTAX  ERROR IN 5",
    );
    check_error(
        "10 console1\n20 print\"after\"\n",
        "",
        "?SYNTAX  ERROR IN 10",
    );
}

#[test]
fn every_trap_is_reported_as_a_build_warning() {
    let prg = tokenize_program("10 print\"a\"\n20 end\n30 wibble 1\n").expect("tokenizes");
    let c = compile_with_options(&prg, CompileOptions::default()).expect("compiles");
    let t = &c.diagnostics.syntax_traps;
    assert_eq!(t.len(), 1, "{t:?}");
    assert_eq!(t[0].line, 30);
    assert_eq!(t[0].code, 11);
    assert!(t[0].detail.contains("line 30"), "{:?}", t[0].detail);
}

#[test]
fn traps_are_reported_once_each_in_line_order() {
    let src = "10 print\"a\"\n20 end\n30 wibble 1\n40 poke 1,\n50 a=a$\n";
    let prg = tokenize_program(src).expect("tokenizes");
    let c = compile_with_options(&prg, CompileOptions::default()).expect("compiles");
    let got: Vec<(u16, u8)> = c
        .diagnostics
        .syntax_traps
        .iter()
        .map(|t| (t.line, t.code))
        .collect();
    assert_eq!(got, vec![(30, 11), (40, 11), (50, 22)]);
}

#[test]
fn a_clean_program_reports_no_traps() {
    let prg = tokenize_program("10 for i=1 to 3\n20 print i\n30 next\n").expect("tokenizes");
    let c = compile_with_options(&prg, CompileOptions::default()).expect("compiles");
    assert!(
        c.diagnostics.syntax_traps.is_empty(),
        "{:?}",
        c.diagnostics.syntax_traps
    );
}

#[test]
fn an_untaken_then_branch_does_not_raise_type_mismatch() {
    check_ok("10 if 0 then a=a$\n20 print\"b\"\n", "B");
}

#[test]
fn a_reached_type_mismatch_still_raises() {
    check_error(
        "10 print\"a\":a=a$\n20 print\"b\"\n",
        "A",
        "?TYPE MISMATCH  ERROR IN 10",
    );
    check_error(
        "10 if 1 then a=a$\n20 print\"b\"\n",
        "",
        "?TYPE MISMATCH  ERROR IN 10",
    );
}

#[test]
fn a_lone_identifier_that_a_proc_defines_still_calls_it() {
    check_ok(
        "10 goto 50\n20 proc c\n30 print\"in c\"\n40 end proc\n50 c\n60 print\"after\"\n",
        "IN C\nAFTER",
    );
}

/// A lone identifier that no `PROC` defines raises, like every other statement the machine cannot execute.
#[test]
fn a_lone_unresolved_identifier_raises_syntax_error() {
    check_error(
        "10 print\"a\"\n20 zz\n30 print\"b\"\n",
        "A",
        "?SYNTAX  ERROR IN 20",
    );
    // Mid-line, after output, exactly like probe (e): the `A` prints first and the `C` after it never does.
    check_error(
        "10 print\"a\":zz:print\"c\"\n20 print\"b\"\n",
        "A",
        "?SYNTAX  ERROR IN 10",
    );
    // Reachability still decides, as it does for every other trap.
    check_ok("10 if 0 then zz\n20 print\"b\"\n", "B");
    check_error("10 if 1 then zz\n20 print\"b\"\n", "", "?SYNTAX  ERROR IN 10");
}

#[test]
fn an_unreachable_lone_identifier_is_still_clean() {
    // Skip malformed statements behind an unconditional branch.
    check_ok(
        "10 print\"a\"\n20 goto 50\n30 zz\n50 print\"b\"\n",
        "A\nB",
    );
    // Skip malformed statements after END.
    check_ok("10 print\"a\"\n20 end\n30 malformed\n", "A");
    // Skip malformed statements after RETURN.
    check_ok(
        "10 gosub 100\n20 print\"a\"\n30 end\n100 return\n110 bbb\n",
        "A",
    );
}

#[test]
fn a_well_formed_program_is_unaffected() {
    check_ok("10 for i=1 to 3\n20 print i\n30 next\n", " 1 \n 2 \n 3");
    check_ok("10 a=5:print a\n20 a$=\"x\":print a$\n", " 5 \nX");
    check_ok("10 if 0 then print\"n\"\n20 print\"y\"\n", "Y");
}
