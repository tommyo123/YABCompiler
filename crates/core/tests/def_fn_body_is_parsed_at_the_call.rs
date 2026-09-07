//! A `DEF FN` body the parser cannot read is not an error at the `DEF`.

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

/// Runs clean on every profile and prints exactly `out`.
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

/// Prints `out`, then dies with `banner`, on every profile.
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

const POSTMAN: &str = "10 print\"a\"\n\
                       20 def fna(o)=int(sqr((abs((1-2)/8)^2)+(abs((3-4)/8)^2))\n\
                       30 print\"b\"\n\
                       40 end\n";

#[test]
fn an_unparseable_body_that_is_never_called_runs_clean() {
    check_ok(POSTMAN, "A\nB");
}

/// Reachability decides here exactly as it does for the statement trap: the call sits in an untaken `THEN`, so it never evaluates.
#[test]
fn a_call_in_an_untaken_branch_runs_clean() {
    check_ok(
        "10 print\"a\"\n\
         20 def fna(o)=int(sqr((abs((1-2)/8)^2)\n\
         30 if 0 then print fna(1)\n\
         40 print\"b\"\n",
        "A\nB",
    );
}

#[test]
fn a_call_raises_syntax_error_at_the_calling_line() {
    check_error(
        "10 print\"a\"\n\
         20 def fna(o)=int(sqr((abs((1-2)/8)^2)+(abs((3-4)/8)^2))\n\
         30 print fna(1)\n\
         40 print\"b\"\n",
        "A",
        "?SYNTAX  ERROR IN 30",
    );
}

#[test]
fn the_raise_is_not_hoisted_above_earlier_output() {
    check_error(
        "10 print\"a\"\n\
         20 def fna(o)=int(sqr((abs((1-2)/8)^2)\n\
         30 print\"c\";:print fna(1)\n\
         40 print\"b\"\n",
        "A\nC",
        "?SYNTAX  ERROR IN 30",
    );
}

#[test]
fn a_later_call_raises_too_and_the_argument_is_evaluated_first() {
    check_error(
        "10 print\"a\"\n\
         20 def fna(o)=int(sqr((abs((1-2)/8)^2)\n\
         30 if 0 then print fna(1)\n\
         40 print fna(1/0)\n",
        "A",
        "?DIVISION BY ZERO  ERROR IN 40",
    );
}

#[test]
fn a_malformed_header_still_raises_at_the_def() {
    check_error(
        "10 print\"a\"\n20 def fna(o)+1\n30 print\"b\"\n",
        "A",
        "?SYNTAX  ERROR IN 20",
    );
}

#[test]
fn a_well_formed_body_still_ends_at_the_colon() {
    check_ok(
        "10 print\"a\"\n20 def fna(o)=o+1:print\"c\"\n30 print fna(4)\n40 print\"b\"\n",
        "A\nC\n 5 \nB",
    );
}

/// A colon inside a string literal in the body is not a statement boundary.
#[test]
fn a_colon_inside_a_string_in_a_dropped_body_is_not_the_boundary() {
    check_ok(
        "10 print\"a\"\n20 def fna(o)=val(\"1:2\"+left$(\"x\"\n30 print\"b\"\n",
        "A\nB",
    );
}

/// Ordinary functions still work, still see the argument, and still restore the parameter's outer value.
#[test]
fn ordinary_def_fn_still_works() {
    check_ok(
        "10 def fnf(x)=x*x+1\n20 x=7\n30 print fnf(3)\n40 print x\n",
        " 10 \n 7",
    );

    check_ok(
        "10 def fna(x)=x+1\n20 print fna(1)\n30 def fna(x)=x+2\n40 print fna(1)\n",
        " 2 \n 3",
    );
}

/// A call to a name no `DEF FN` ever mentions still reports `?UNDEF'D FUNCTION`, not `?SYNTAX ERROR`.
#[test]
fn an_undefined_function_still_says_undefd_function() {
    check_error(
        "10 print\"a\"\n20 print fng(1)\n30 print\"b\"\n",
        "A",
        "?UNDEF'D FUNCTION  ERROR IN 20",
    );
}

#[test]
fn every_dropped_body_is_reported() {
    let prg = tokenize_program(POSTMAN).expect("tokenizes");
    let c = compile_with_options(&prg, CompileOptions::default()).expect("compiles");
    let d = &c.diagnostics.deferred_fn_bodies;
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0].line, 20);
    assert_eq!(d[0].name.0, "A");
    assert_eq!(d[0].code, 11);
    assert!(d[0].detail.contains("line 20"), "{:?}", d[0].detail);

    assert!(
        c.diagnostics.syntax_traps.is_empty(),
        "{:?}",
        c.diagnostics.syntax_traps
    );
}

/// A clean program reports none, on either channel.
#[test]
fn a_well_formed_program_reports_nothing() {
    let prg = tokenize_program("10 def fnf(x)=x*x\n20 print fnf(3)\n").expect("tokenizes");
    let c = compile_with_options(&prg, CompileOptions::default()).expect("compiles");
    assert!(
        c.diagnostics.deferred_fn_bodies.is_empty(),
        "{:?}",
        c.diagnostics.deferred_fn_bodies
    );
    assert!(
        c.diagnostics.syntax_traps.is_empty(),
        "{:?}",
        c.diagnostics.syntax_traps
    );
}
