//! `ER` and `EL` are only the `ON ERROR` pseudo-variables when the program never writes them.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

fn build(src: &str, profile: Profile) -> yabcompiler_core::Compiled {
    let prg = tokenize_program(src).unwrap_or_else(|e| panic!("tokenize: {e:?}"));
    compile_with_options(
        &prg,
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compile: {e}"))
}

fn output(src: &str, profile: Profile) -> String {
    let c = build(src, profile);
    let r = emu64::run_prg_to_end(&c.prg_bytes, common::MAX_INSNS).expect("emu64 run");
    assert!(
        r.clean_exit,
        "program did not reach READY under {profile:?}:\n{src}"
    );
    common::petscii_to_string(&r.output)
}

/// Canonical form of a PRINT stream: one trimmed line per printed item, joined with `|`.
fn lines(out: &str) -> String {
    out.split('\n')
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("|")
}

/// Every profile must agree, and must agree with the interpreter.
fn assert_prints(src: &str, want: &str) {
    for p in PROFILES {
        let got = lines(&output(src, p));
        assert_eq!(
            got, want,
            "\n{p:?} printed {got:?}, interpreter prints {want:?}, for:\n{src}"
        );
    }
}

#[test]
fn for_loop_control_variable_named_el_is_a_user_variable() {

    assert_prints("10 FOR EL=1 TO 5\n20 NEXT EL\n30 PRINT EL\n", "6");
}

#[test]
fn for_loop_named_el_with_fractional_step_is_a_user_variable() {

    assert_prints(
        "10 FOR EL=1 TO 3 STEP 0.5\n20 PRINT EL\n30 NEXT EL\n",
        "1|1.5|2|2.5|3",
    );
}

#[test]
fn a_for_with_a_bare_next_still_counts() {
    // `NEXT` with no variable is the ordinary spelling.
    assert_prints("10 FOR EL=1 TO 5\n20 NEXT\n30 PRINT EL\n", "6");
}

#[test]
fn a_conditionally_opened_for_with_a_bare_next_still_counts() {

    assert_prints("10 IF 1 THEN FOR EL=1 TO 5\n20 NEXT\n30 PRINT EL\n", "6");
}

#[test]
fn a_runtime_conditional_for_with_a_bare_next_still_counts() {

    assert_prints(
        "10 DATA 64\n20 READ A\n30 IF A=64 THEN FOR EL=1 TO 5\n40 NEXT\n50 PRINT EL\n",
        "6",
    );
}

#[test]
fn read_into_er_is_a_user_variable() {
    // `Stmt::Read` carries `ReadTarget::Scalar`, which the LET-only detector never looked at.
    assert_prints("10 DATA 42\n20 READ ER\n30 PRINT ER\n", "42");
}

#[test]
fn read_into_el_is_a_user_variable() {
    assert_prints("10 DATA 77\n20 READ EL\n30 PRINT EL\n", "77");
}

#[test]
fn a_write_nested_in_an_if_body_still_counts() {
    // The detector has to keep descending into `THEN` bodies.
    assert_prints(
        "10 DATA 64,99\n20 READ A\n30 IF A=64 THEN READ EL\n40 PRINT EL\n",
        "99",
    );
}

#[test]
fn control_same_shape_with_an_ordinary_name() {
    // Identical to `for_loop_control_variable_named_el_is_a_user_variable` except for the name.
    assert_prints("10 FOR AB=1 TO 5\n20 NEXT AB\n30 PRINT AB\n", "6");
}

#[test]
fn no_write_still_gets_the_pseudo_var() {

    for p in PROFILES {
        let asm = build("10 PRINT ER\n20 PRINT EL\n", p).asm;
        assert!(
            asm.contains("LDY __TSB_ERR_CODE"),
            "{p:?}: read-only ER must stay the pseudo-var:\n{asm}"
        );
        assert!(
            asm.contains("LDA __TSB_RESUME_LINE+1"),
            "{p:?}: read-only EL must stay the pseudo-var:\n{asm}"
        );
    }
}

#[test]
fn a_subscripted_write_does_not_make_the_scalar_a_user_variable() {

    for p in PROFILES {
        let asm = build("10 DIM ER(3)\n20 ER(0)=5\n30 PRINT ER(0)\n40 PRINT ER\n", p).asm;
        assert!(
            asm.contains("LDY __TSB_ERR_CODE"),
            "{p:?}: an ER(n) store must leave scalar ER a pseudo-var:\n{asm}"
        );
    }
}

#[test]
fn a_subscripted_read_target_does_not_make_the_scalar_a_user_variable() {
    // Same distinction one level down: `READ ER(0)` carries a `ReadTarget::Array`, which names the array, not the scalar.
    for p in PROFILES {
        let asm = build(
            "10 DIM ER(3)\n20 DATA 5\n30 READ ER(0)\n40 PRINT ER(0)\n50 PRINT ER\n",
            p,
        )
        .asm;
        assert!(
            asm.contains("LDY __TSB_ERR_CODE"),
            "{p:?}: a READ into ER(n) must leave scalar ER a pseudo-var:\n{asm}"
        );
    }
}

#[test]
fn a_string_write_does_not_make_the_scalar_a_user_variable() {
    // `ER$` likewise.
    for p in PROFILES {
        let asm = build("10 ER$=\"X\"\n20 PRINT ER\n", p).asm;
        assert!(
            asm.contains("LDY __TSB_ERR_CODE"),
            "{p:?}: an ER$ store must leave scalar ER a pseudo-var:\n{asm}"
        );
    }
}
