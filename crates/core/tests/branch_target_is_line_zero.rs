//! `GOTO <var>` / `GOSUB <var>` ; the target is line 0, not an error.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

fn run(src: &str, profile: Profile) -> (String, bool, Option<String>) {
    let prg = tokenize_program(src).expect("tokenize");
    let c = compile_with_options(
        &prg,
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .expect("must build");
    let r = emu64::run_prg_to_end(&c.prg_bytes, 20_000_000).expect("runs");
    (
        common::petscii_to_string(&r.output),
        r.clean_exit,
        r.error_banner(),
    )
}

fn assert_traps(src: &str, banner: &str, before: &str) {
    for p in PROFILES {
        let (out, clean, got) = run(src, p);
        assert!(!clean, "{p:?}: must not finish cleanly, got {out:?}");
        assert_eq!(got.as_deref(), Some(banner), "{p:?}: wrong banner");
        assert_eq!(out.trim_end(), before, "{p:?}: output before the trap");
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
fn a_variable_target_branches_to_line_zero_when_one_exists() {
    assert_clean(
        "0 if q=1 then print\"zero\":end\n10 q=1\n20 goto zn\n30 print\"back\"\n",
        "ZERO",
    );
    assert_clean(
        "0 if q=1 then print\"zero\":end\n10 q=1\n20 gosub zn\n30 print\"back\"\n",
        "ZERO",
    );
}

#[test]
fn goto_a_variable_target_is_undefd_statement() {
    assert_traps(
        "10 print\"a\"\n20 goto zn\n30 print\"b\"\n",
        "?UNDEF'D STATEMENT  ERROR IN 20",
        "A",
    );
}

#[test]
fn gosub_a_variable_target_is_undefd_statement() {
    assert_traps(
        "10 print\"a\"\n20 gosub zn\n30 print\"b\"\n",
        "?UNDEF'D STATEMENT  ERROR IN 20",
        "A",
    );
}

#[test]
fn any_non_digit_target_is_line_zero() {
    for src in [
        "10 print\"a\"\n20 goto a$\n30 print\"b\"\n",
        "10 print\"a\"\n20 gosub(5)\n30 print\"b\"\n",
        "10 print\"a\"\n20 goto zn+20\n30 print\"b\"\n",
    ] {
        assert_traps(src, "?UNDEF'D STATEMENT  ERROR IN 20", "A");
    }
}

#[test]
fn gosub_parenthesised_five_does_not_call_line_five() {
    let (out, clean, banner) = run(
        "5 if q=1 then print\"five\":return\n10 q=1:print\"a\"\n20 gosub(5)\n30 print\"b\"\n",
        Profile::Default,
    );
    assert!(!clean);
    assert_eq!(banner.as_deref(), Some("?UNDEF'D STATEMENT  ERROR IN 20"));
    assert_eq!(out.trim_end(), "A", "line 5 must not have run");
}

/// The rest of the line dies with the branch.
#[test]
fn a_variable_target_kills_the_rest_of_its_line() {
    assert_traps(
        "10 print\"a\"\n20 goto zn:print\"x\"\n30 print\"b\"\n",
        "?UNDEF'D STATEMENT  ERROR IN 20",
        "A",
    );
}

#[test]
fn an_unreached_variable_target_costs_nothing() {
    assert_clean("10 print\"a\"\n20 end\n30 goto zn\n", "A");
    assert_clean("10 print\"a\"\n20 end\n30 zn=100:gosub zn\n", "A");
    assert_clean("10 print\"a\"\n20 if 0 then goto zn\n30 print\"b\"\n", "A\nB");
}

#[test]
fn ordinary_numeric_targets_still_branch() {
    assert_clean(
        "10 gosub 100\n20 print\"back\"\n30 end\n100 print\"sub\"\n110 return\n",
        "SUB\nBACK",
    );
    assert_clean("10 print\"a\"\n20 goto 40\n30 print\"b\"\n40 print\"c\"\n", "A\nC");
}

#[test]
fn a_target_with_a_trailing_label_still_branches() {
    assert_clean(
        "10 print\"a\"\n20 goto 40x\n30 print\"b\"\n40 print\"c\"\n",
        "A\nC",
    );
    assert_clean(
        "10 gosub 100done\n20 print\"back\"\n30 end\n100 print\"sub\"\n110 return\n",
        "SUB\nBACK",
    );
}

#[test]
fn spaces_before_the_digits_still_find_the_line() {
    assert_clean(
        "10 print\"a\"\n20 goto   40\n30 print\"b\"\n40 print\"c\"\n",
        "A\nC",
    );
}

#[test]
fn the_conditional_and_on_forms_stay_correct() {
    assert_traps(
        "10 print\"a\"\n20 if 1 then goto zn\n30 print\"b\"\n",
        "?UNDEF'D STATEMENT  ERROR IN 20",
        "A",
    );
    assert_traps(
        "10 print\"a\"\n20 on 1 goto zn\n30 print\"b\"\n",
        "?UNDEF'D STATEMENT  ERROR IN 20",
        "A",
    );
}

#[test]
fn spaces_between_the_digits_still_find_the_line() {
    assert_clean(
        "10 print\"a\"\n20 goto 4 0\n30 print\"b\"\n40 print\"c\"\n",
        "A\nC",
    );
}

#[test]
fn a_variable_target_gosub_that_returns_resumes_cleanly() {
    assert_clean(
        "0 if q=1 then print\"zero\":return\n10 q=1\n20 gosub zn\n30 print\"back\"\n",
        "ZERO\nBACK",
    );
    assert_clean(
        "0 if q=1 then print\"zero\":return\n10 q=1\n20 gosub\n30 print\"back\"\n",
        "ZERO\nBACK",
    );
}

#[test]
fn return_resumes_at_the_statement_after_a_variable_target_gosub() {
    assert_clean(
        "0 if q=1 then print\"zero\":return\n10 q=1\n20 gosub zn:print\"mid\"\n30 print\"back\"\n",
        "ZERO\nMID\nBACK",
    );
}
