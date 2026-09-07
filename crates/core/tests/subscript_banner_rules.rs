//! Which banner does a bad array subscript raise, and at which line?

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

fn build(src: &str, profile: Profile) -> yabcompiler_core::Compiled {
    let tok = tokenize_program(src).unwrap_or_else(|e| panic!("tokenize: {e:?}"));
    compile_with_options(
        &tok,
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compile ({profile:?}): {e}"))
}

/// Require exactly `banner` at every profile.
#[track_caller]
fn assert_banner(src: &str, banner: &str) {
    for profile in PROFILES {
        let c = build(src, profile);
        let r = emu64::run_prg_to_end(&c.prg_bytes, common::MAX_INSNS).expect("emu64 runs");
        let out = common::petscii_to_string(&r.output);
        assert_eq!(
            r.error_banner().as_deref(),
            Some(banner),
            "{profile:?}: {src:?}\n  expected {banner:?}, output was {out:?}"
        );
    }
}

/// Require a clean run at every profile.
#[track_caller]
fn assert_no_banner(src: &str) {
    for profile in PROFILES {
        let c = build(src, profile);
        let r = emu64::run_prg_to_end(&c.prg_bytes, common::MAX_INSNS).expect("emu64 runs");
        let out = common::petscii_to_string(&r.output);
        assert_eq!(
            r.error_banner(),
            None,
            "{profile:?}: {src:?} must run clean, got {:?} / {out:?}",
            r.error_banner()
        );
    }
}

#[test]
fn subscript_32767_is_bad_subscript_not_illegal_quantity() {
    assert_banner(
        "10 DIM Q(10)\n20 READ A%,B%\n30 Q(A%+B%)=1\n40 PRINT \"AFTER\"\n50 DATA 20000,12767\n",
        "?BAD SUBSCRIPT  ERROR IN 30",
    );
}

#[test]
fn subscript_32768_is_illegal_quantity_not_bad_subscript() {
    assert_banner(
        "10 DIM Q(10)\n20 READ A%,B%\n30 Q(A%+B%)=1\n40 PRINT \"AFTER\"\n50 DATA 20000,12768\n",
        "?ILLEGAL QUANTITY  ERROR IN 30",
    );
}

#[test]
fn negative_subscript_is_illegal_quantity() {
    assert_banner(
        "10 DIM Q(10)\n20 READ A%\n30 Q(A%)=1\n40 PRINT \"AFTER\"\n50 DATA -1\n",
        "?ILLEGAL QUANTITY  ERROR IN 30",
    );
}

#[test]
fn fac_route_rejects_70000() {
    assert_banner(
        "10 DIM Q(10)\n20 READ X\n30 Q(X)=1\n40 PRINT \"AFTER\"\n50 DATA 70000\n",
        "?ILLEGAL QUANTITY  ERROR IN 30",
    );
}

#[test]
fn fac_route_rejects_40000() {
    assert_banner(
        "10 DIM Q(10)\n20 READ X\n30 Q(X)=1\n40 PRINT \"AFTER\"\n50 DATA 40000\n",
        "?ILLEGAL QUANTITY  ERROR IN 30",
    );
}

#[test]
fn runtime_dim_bound_outside_the_window_is_illegal_quantity() {
    assert_banner(
        "10 READ I\n20 DIM Q(I)\n30 PRINT \"AFTER\"\n40 DATA 40000\n",
        "?ILLEGAL QUANTITY  ERROR IN 20",
    );
}

#[test]
fn wrong_number_of_subscripts_is_bad_subscript() {
    assert_banner(
        "10 DIM Q(5)\n20 Q(1,2)=3\n30 PRINT \"AFTER\"\n",
        "?BAD SUBSCRIPT  ERROR IN 20",
    );
}

#[test]
fn index_past_the_dim_is_bad_subscript() {
    assert_banner(
        "10 DIM Q(5)\n20 READ I\n30 Q(I)=1\n40 PRINT \"AFTER\"\n50 DATA 7\n",
        "?BAD SUBSCRIPT  ERROR IN 30",
    );
}

#[test]
fn auto_dimmed_array_past_bound_10_is_bad_subscript() {
    assert_banner(
        "10 READ I\n20 A(I)=1\n30 PRINT \"AFTER\"\n40 DATA 11\n",
        "?BAD SUBSCRIPT  ERROR IN 20",
    );
}

#[test]
fn fractional_subscript_truncates_and_stays_in_bounds() {
    assert_no_banner("10 DIM Q(1)\n20 READ X\n30 Q(X)=7\n40 PRINT Q(1);Q(0)\n50 DATA 1.9\n");
}

#[test]
fn literal_index_past_the_dim_survives_a_clr() {
    assert_no_banner("10 DIM K(3)\n20 K(1)=5\n30 CLR\n40 PRINT K(9)\n50 PRINT \"AFTER\"\n");
}

#[test]
fn redim_larger_after_clr_still_runs() {
    assert_no_banner(
        "10 DIM K(3)\n20 K(1)=5\n30 CLR\n40 DIM K(20)\n50 READ I\n60 K(I)=7\n\
         70 PRINT K(20)\n80 PRINT \"AFTER\"\n90 DATA 20\n",
    );
}
