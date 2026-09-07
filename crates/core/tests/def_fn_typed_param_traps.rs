mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

fn opts(profile: Profile, lenient: bool) -> CompileOptions {
    CompileOptions {
        profile,
        lenient_syntax: lenient,
        ..CompileOptions::default()
    }
}

fn run_with(src: &str, profile: Profile, lenient: bool) -> (String, bool, Option<String>) {
    let prg = tokenize_program(src).expect("tokenize");
    let c = compile_with_options(&prg, opts(profile, lenient))
        .expect("must build: the ROM runs this program");
    let r = emu64::run_prg_to_end(&c.prg_bytes, 20_000_000).expect("runs");
    (
        common::petscii_to_string(&r.output),
        r.clean_exit,
        r.error_banner(),
    )
}

fn run(src: &str, profile: Profile) -> (String, bool, Option<String>) {
    run_with(src, profile, false)
}

/// Assert the exact banner AND that everything printed before the trap still ran.
fn assert_traps(src: &str, banner: &str, before: &str) {
    for p in PROFILES {
        let (out, clean, got) = run(src, p);
        assert!(!clean, "{p:?}: must not finish cleanly, got {out:?}");
        assert_eq!(got.as_deref(), Some(banner), "{p:?}: wrong banner");
        assert_eq!(
            out.trim_end(),
            before,
            "{p:?}: output before the trap must survive"
        );
    }
}

fn assert_clean(src: &str, want: &str) {
    for p in PROFILES {
        let (out, clean, banner) = run(src, p);
        assert!(clean, "{p:?}: must finish cleanly, got {out:?} {banner:?}");
        assert_eq!(banner, None, "{p:?}: must not raise");
        assert_eq!(out.trim_end(), want, "{p:?}");
    }
}

#[test]
fn def_fn_string_parameter_traps_type_mismatch_where_it_runs() {
    assert_traps(
        "10 print\"a\"\n20 deffna(b$)=1\n30 print\"c\"\n",
        "?TYPE MISMATCH  ERROR IN 20",
        "A",
    );
}

#[test]
fn the_parameter_check_precedes_the_body() {
    assert_traps(
        "10 print\"a\"\n20 deffna(b$)=b$\n30 print\"c\"\n",
        "?TYPE MISMATCH  ERROR IN 20",
        "A",
    );
}

/// Space-transparent spelling: CHRGET skips spaces, so `B $` is the variable `B$`.
#[test]
fn the_string_parameter_is_space_transparent() {
    assert_traps(
        "10 print\"a\"\n20 deffna(b $)=1\n30 print\"c\"\n",
        "?TYPE MISMATCH  ERROR IN 20",
        "A",
    );
}

#[test]
fn it_kills_the_rest_of_its_line_but_not_the_start_of_it() {
    assert_traps(
        "10 print\"a\"\n20 print\"b\":deffna(c$)=1:print\"d\"\n30 print\"e\"\n",
        "?TYPE MISMATCH  ERROR IN 20",
        "A\nB",
    );
}

#[test]
fn an_unreached_string_parameter_costs_nothing() {
    assert_clean("10 print\"a\"\n20 end\n30 deffna(b$)=1\n", "A");
}

#[test]
fn lenient_syntax_does_not_launder_it_into_a_rem() {
    for p in PROFILES {
        let (out, clean, banner) = run_with("10 print\"a\"\n20 deffna(b$)=1\n30 print\"c\"\n", p, true);
        assert!(!clean, "{p:?}: lenient mode must still trap, got {out:?}");
        assert_eq!(
            banner.as_deref(),
            Some("?TYPE MISMATCH  ERROR IN 20"),
            "{p:?}: lenient mode must raise the same error"
        );
        assert_eq!(out.trim_end(), "A", "{p:?}");
    }
}

#[test]
fn the_lenient_hatch_still_covers_a_decorative_def() {
    for p in PROFILES {
        let (out, clean, banner) = run_with("10 print\"a\"\n20 definitionen\n30 print\"c\"\n", p, true);
        assert!(clean, "{p:?}: decorative DEF must still compile away: {banner:?}");
        assert_eq!(out.trim_end(), "A\nC", "{p:?}");
    }
}

#[test]
fn an_ordinary_def_fn_still_works() {
    assert_clean("10 print\"a\"\n20 deffna(b)=b*2\n30 print fna(3)\n", "A\n 6");
}

#[test]
fn a_valid_def_fn_before_the_bad_one_still_runs() {
    assert_traps(
        "10 deffnb(x)=x+1\n20 print fnb(4)\n30 deffna(c$)=1\n40 print\"z\"\n",
        "?TYPE MISMATCH  ERROR IN 30",
        " 5",
    );
}

#[test]
fn an_integer_parameter_is_still_refused_which_the_rom_does_not_do() {
    let prg = tokenize_program("10 print\"a\"\n20 deffna(b%)=1\n30 print\"c\"\n").expect("tokenize");
    let err = compile_with_options(&prg, opts(Profile::Default, false))
        .err()
        .expect("still a build refusal today");
    let msg = err.to_string();
    assert!(
        msg.contains("DEF FN with non-numeric parameter"),
        "unexpected refusal reason: {msg}"
    );
}

#[test]
fn a_typed_function_name_still_gets_the_wrong_banner() {
    for p in PROFILES {
        let (_, clean, banner) = run("10 print\"a\"\n20 deffna$(b)=1\n30 print\"c\"\n", p);
        assert!(!clean, "{p:?}: it does stop, on both sides");
        assert_eq!(
            banner.as_deref(),
            Some("?SYNTAX  ERROR IN 20"),
            "{p:?}: the ROM says ?TYPE MISMATCH  ERROR IN 20 here; this pins \
             OUR current answer so the gap stays measured"
        );
    }
}

/// The asymmetry is deliberate and must stay visible: the string case builds, the integer case does not, from otherwise identical programs.
#[test]
fn the_two_typed_parameters_are_treated_differently_on_purpose() {
    let s = tokenize_program("10 deffna(b$)=1\n").expect("tokenize");
    let i = tokenize_program("10 deffna(b%)=1\n").expect("tokenize");
    assert!(
        compile_with_options(&s, opts(Profile::Default, false)).is_ok(),
        "the string parameter must BUILD, and trap at run time"
    );
    assert!(
        compile_with_options(&i, opts(Profile::Default, false)).is_err(),
        "the integer parameter is still refused — see the test above"
    );
}
