mod common;

use yabcompiler_core::{
    CompileOptions, Dialect, Profile, compile_with_options, tokenize_program,
    tokenize_program_with,
};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];
const TYPE_MISMATCH: &str = "?TYPE MISMATCH  ERROR IN ";

/// Compile on every profile.
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

/// Compiles, prints `out`, then dies with `?TYPE MISMATCH ERROR IN <line>` ; on all profiles.
fn traps_at(src: &str, out: &str, line: u32) {
    let want = format!("{TYPE_MISMATCH}{line}");
    for profile in PROFILES {
        let prg = build(src, profile);
        let r = emu64::run_prg_to_end(&prg, common::MAX_INSNS).expect("run");
        assert!(!r.clean_exit, "must not exit cleanly ({profile:?}):\n{src}");
        assert_eq!(
            r.error_banner().as_deref(),
            Some(want.as_str()),
            "({profile:?}):\n{src}"
        );
        assert_eq!(
            common::petscii_to_string(&r.output).trim_end(),
            out,
            "output before the trap ({profile:?}):\n{src}"
        );
    }
}

/// Compiles, prints `out`, exits clean with no banner ; on all profiles.
fn runs_clean(src: &str, out: &str) {
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

#[test]
fn negating_a_string_array_element_traps_instead_of_panicking() {
    traps_at("10 a=-b$(0)\n", "", 10);
}

/// The sibling prefix operators.
#[test]
fn every_prefix_operator_over_a_string_array_element_traps() {
    traps_at("10 a=+b$(0)\n", "", 10);
    traps_at("10 a=not b$(0)\n", "", 10);
}

#[test]
fn a_string_array_element_after_an_infix_operator_traps() {
    traps_at("10 a=1+b$(0)\n", "", 10);
    traps_at("10 a=2*b$(0)\n", "", 10);
    traps_at("10 a=2^b$(0)\n", "", 10);
    traps_at("10 a=2^-b$(0)\n", "", 10);
    traps_at("10 a=-b$(0)+1\n", "", 10);
}

#[test]
fn the_same_shape_traps_in_print_poke_if_and_a_function_argument() {
    traps_at("10 print-b$(0)\n", "", 10);
    traps_at("10 poke 1024,-b$(0)\n", "", 10);
    traps_at("10 if -b$(0) then print\"t\"\n", "", 10);
    traps_at("10 a=abs(-b$(0))\n", "", 10);
}

#[test]
fn a_dimmed_string_array_traps_the_same_way() {
    traps_at("10 dim b$(3)\n20 a=-b$(1)\n", "", 20);
}

#[test]
fn a_string_scalar_after_an_operator_traps_instead_of_reading_its_pointer() {
    traps_at("10 a=-b$\n", "", 10);
    traps_at("10 a=+b$\n", "", 10);
    traps_at("10 a=1+b$\n", "", 10);
}

/// Parenthesised, which takes the type of its contents exactly as FRMEVL does.
#[test]
fn a_parenthesised_string_after_an_operator_traps() {
    traps_at("10 a=-(b$)\n", "", 10);
}

#[test]
fn a_string_function_after_an_operator_is_type_mismatch_not_syntax() {
    traps_at("10 a=-chr$(65)\n", "", 10);
    traps_at("10 a=-str$(1)\n", "", 10);
    traps_at("10 a=-left$(\"xy\",1)\n", "", 10);
    traps_at("10 a=-right$(\"xy\",1)\n", "", 10);
    traps_at("10 a=-mid$(\"xyz\",2,1)\n", "", 10);
}

#[test]
fn an_unreachable_bad_line_costs_nothing() {
    runs_clean("10 print\"a\"\n20 end\n30 a=-b$(0)\n", "A");
}

#[test]
fn an_untaken_branch_to_a_bad_statement_costs_nothing() {
    runs_clean("10 r=1\n20 if r=59 then a=-b$(0)\n30 print\"ok\"\n", "OK");
}

#[test]
fn a_taken_branch_to_a_bad_statement_traps_on_the_branch_line() {
    traps_at("10 r=59\n20 if r=59 then a=-b$(0)\n30 print\"no\"\n", "", 20);
}

#[test]
fn output_before_the_trap_survives_and_the_line_is_stamped() {
    traps_at("10 print\"a\"\n20 a=-b$(0)\n30 print\"b\"\n", "A", 20);
}

#[test]
fn an_uncalled_def_fn_with_a_string_body_costs_nothing() {
    runs_clean("10 def fna(x)=-b$(0)\n20 print\"a\"\n", "A");
}

#[test]
fn negating_a_numeric_array_element_still_works() {
    runs_clean("10 dim b(3):b(1)=7\n20 print -b(1)\n", "-7");
    runs_clean("10 dim b%(3):b%(1)=7\n20 print -b%(1)\n", "-7");
}

#[test]
fn ordinary_numeric_expressions_are_untouched() {
    runs_clean("10 a=5:print -a\n", "-5");
    runs_clean("10 a=1:print a*2+3^2\n", " 11");
    runs_clean("10 print -len(\"abc\")\n", "-3");
    runs_clean("10 a$=\"12\":print -val(a$)\n", "-12");
}

#[test]
fn the_string_functions_still_work_where_a_string_belongs() {
    runs_clean("10 print chr$(65)\n", "A");
    runs_clean("10 print left$(\"abc\",2)\n", "AB");
    runs_clean("10 print right$(\"abc\",2)\n", "BC");
    runs_clean("10 print mid$(\"abc\",2,1)\n", "B");
    runs_clean("10 print str$(12)\n", " 12");
}

/// String variables and string arrays in string positions.
#[test]
fn string_variables_and_arrays_still_read_normally() {
    runs_clean("10 a$=\"q\":print len(a$)\n", " 1");
    runs_clean("10 a$=\"ab\":print asc(a$)\n", " 65");
    runs_clean("10 a$=\"q\":print \"x\"+a$\n", "XQ");
    runs_clean("10 dim a$(2):a$(1)=\"q\":print a$(1)\n", "Q");
    runs_clean("10 a$=\"q\":if a$=\"q\" then print\"y\"\n", "Y");
    runs_clean("10 a=5:b$=\"q\":if a=5 then print b$\n", "Q");
}

#[test]
fn the_extension_spellings_are_still_plain_numeric_variables() {
    for src in ["10 a=1+dup\n20 print a\n", "10 a=1+insert\n20 print a\n"] {

        let prg = tokenize_program_with(src, Dialect::BasicV2).expect("tokenizes");
        let out = compile_with_options(
            &prg,
            CompileOptions {
                dialect: Dialect::BasicV2,
                ..CompileOptions::default()
            },
        )
        .unwrap_or_else(|e| panic!("must compile: {e}\n{src}"));
        let r = emu64::run_prg_to_end(&out.prg_bytes, common::MAX_INSNS).expect("run");
        assert_eq!(r.error_banner(), None, "{src}");
        assert_eq!(common::petscii_to_string(&r.output).trim_end(), " 1", "{src}");
    }
}

#[test]
fn the_head_of_the_expression_still_traps_as_it_did() {
    traps_at("10 a=b$\n", "", 10);
    traps_at("10 a=b$(0)*2\n", "", 10);
    traps_at("10 a=-\"q\"\n", "", 10);
}

#[test]
fn lenient_syntax_yields_val_of_the_string_instead_of_trapping() {
    let prg = tokenize_program("10 b$=\"12\"\n20 a=-b$\n30 print a\n").expect("tokenizes");
    let out = compile_with_options(
        &prg,
        CompileOptions {
            lenient_syntax: true,
            ..CompileOptions::default()
        },
    )
    .expect("lenient build");
    let r = emu64::run_prg_to_end(&out.prg_bytes, common::MAX_INSNS).expect("run");
    assert_eq!(r.error_banner(), None, "lenient must not trap");
    assert_eq!(common::petscii_to_string(&r.output).trim_end(), "-12");
}
