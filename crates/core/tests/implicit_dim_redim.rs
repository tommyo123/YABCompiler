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
    .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}\n{src}"))
    .prg_bytes
}

fn check_error(src: &str, out: &str, banner: &str) {
    for profile in PROFILES {
        let bytes = build(src, profile);
        let r = emu64::run_prg_to_end(&bytes, common::MAX_INSNS).expect("run");
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

fn check_ok(src: &str, out: &str) {
    for profile in PROFILES {
        let bytes = build(src, profile);
        let r = emu64::run_prg_to_end(&bytes, common::MAX_INSNS).expect("run");
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

/// Implicit array allocation prevents a later DIM of the same array.
#[test]
fn a_write_before_the_dim_makes_the_dim_a_redim() {
    check_error(
        "10 k(2)=5\n20 dim k(5)\n30 print k(2)\n",
        "",
        "?REDIM'D ARRAY  ERROR IN 20",
    );
}

/// A plain read creates the array just as a write does, so line 10 prints its ` 0` from the implicitly-created `K` and line 20 is still fatal.
#[test]
fn a_read_before_the_dim_creates_the_array_too() {
    check_error(
        "10 print k(2)\n20 dim k(5)\n30 print k(2)\n",
        " 0",
        "?REDIM'D ARRAY  ERROR IN 20",
    );
}

#[test]
fn string_arrays_behave_the_same_way() {
    check_error(
        "10 k$(2)=\"a\"\n20 dim k$(5)\n30 print k$(2)\n",
        "",
        "?REDIM'D ARRAY  ERROR IN 20",
    );
}

#[test]
fn a_multi_name_dim_traps_on_the_created_one() {
    check_error(
        "10 k(2)=5\n20 dim j(3),k(5)\n30 print k(2)\n",
        "",
        "?REDIM'D ARRAY  ERROR IN 20",
    );
}

#[test]
fn dim_of_an_already_dimmed_array_is_an_error() {
    check_error(
        "10 dim k(5)\n20 dim k(5)\n30 print k(2)\n",
        "",
        "?REDIM'D ARRAY  ERROR IN 20",
    );
}

#[test]
fn a_reference_between_two_dims_does_not_change_that() {
    check_error(
        "10 dim k(5)\n20 k(2)=7\n30 dim k(5)\n40 print k(2)\n",
        "",
        "?REDIM'D ARRAY  ERROR IN 30",
    );
}

/// The ordinary case, which is most `DIM`s in most programs.
#[test]
fn a_dim_before_the_first_reference_is_fine() {
    check_ok("10 dim k(5)\n20 k(2)=5\n30 print k(2)\n", " 5");
}

/// `CLR` resets ARYTAB, so the array stops existing and the `DIM` after it is the first one again.
#[test]
fn a_clr_between_the_reference_and_the_dim_clears_it() {
    check_ok("10 k(2)=5\n20 clr\n30 dim k(5)\n40 print k(2)\n", " 0");
}

/// Control never passes through the reference, so nothing created the array and the `DIM` is legal.
#[test]
fn a_reference_behind_a_goto_is_not_enough() {
    check_ok("10 goto 30\n20 k(2)=5\n30 dim k(5)\n40 print k(2)\n", " 0");
}

#[test]
fn a_conditional_reference_does_not_arm_the_trap() {
    check_ok("10 if 0 then k(2)=5\n20 dim k(5)\n30 print k(2)\n", " 0");
}

/// A `GOSUB`ed reference still counts when the call is inlined, which puts the subroutine body into the prologue.
#[test]
fn a_reference_reached_through_a_gosub_still_counts_when_it_is_inlined() {
    check_error(
        "10 gosub 100\n20 dim k(5)\n30 print k(2)\n40 end\n100 k(1)=1:return\n",
        "",
        "?REDIM'D ARRAY  ERROR IN 20",
    );
}

/// A `DIM` that something jumps back to still sees the reference before it.
#[test]
fn a_dim_that_something_jumps_back_to_still_traps() {
    check_error(
        "10 k(2)=5\n20 dim k(5)\n30 print k(2)\n40 if a=1 then 20\n",
        "",
        "?REDIM'D ARRAY  ERROR IN 20",
    );
}

/// The implicit bound is still 10 whether or not any of this fires: `K(10)` is in range after `K(2)=5` created the array, `K(11)` is not.
#[test]
fn the_implicit_bound_is_unchanged() {
    check_error(
        "10 k(2)=5\n20 print k(10)\n30 print k(11)\n",
        " 0",
        "?BAD SUBSCRIPT  ERROR IN 30",
    );
}
