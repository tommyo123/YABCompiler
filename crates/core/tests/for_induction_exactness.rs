//! The FOR strength-reduction guard tested the wrong quantity.

mod common;

use yabcompiler_core::opt_model::{
    CostModel, INDUCTION_MIN_TRAILING_ZEROS, mflpt_mantissa_trailing_zeros,
};
use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn build(src: &str, profile: Profile) -> Vec<u8> {
    let prg = tokenize_program(src).expect("tokenizes");
    let opts = CompileOptions {
        profile,
        force_extraram_off: true,
        ..CompileOptions::default()
    };
    compile_with_options(&prg, opts)
        .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}"))
        .prg_bytes
}

fn asm_of(src: &str) -> String {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(&prg, CompileOptions::default())
        .expect("compiles")
        .asm
}

fn check_prints(src: &str, expected: &str) {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let bytes = build(src, profile);
        let got = common::run_clean(&bytes)
            .unwrap_or_else(|| panic!("runs to a clean exit ({profile:?}):\n{src}"));
        assert_eq!(got, expected, "{profile:?} mismatch for:\n{src}");
    }
}

/// How many induction slots the image allocates.
fn induction_slots(asm: &str) -> usize {
    asm.lines()
        .filter(|l| {
            let l = l.trim_end();
            l.starts_with("FB_") && l.ends_with(':')
        })
        .count()
}

#[test]
fn inexact_increment_does_not_get_an_induction_slot() {
    let src = "10 for i=1 to 400:x=.1*i:next i\n20 print x\n";
    check_prints(src, " 40 \n");
    assert_eq!(
        induction_slots(&asm_of(src)),
        0,
        "an increment that cannot accumulate exactly must not be \
         strength-reduced:\n{}",
        asm_of(src)
    );
}

/// `.7` is the second `tz = 0` carrier from the causal validation.
#[test]
fn inexact_increment_point_seven_does_not_get_a_slot() {
    let src = "10 for i=1 to 400:x=.7*i:next i\n20 print x\n";
    check_prints(src, " 280 \n");
    assert_eq!(induction_slots(&asm_of(src)), 0);
}

#[test]
fn exact_multiplier_with_inexact_step_does_not_get_a_slot() {
    let src = "10 k=3\n20 for i=1 to 100 step .1:x=k*i:next\n30 print x\n";
    check_prints(src, " 299.999989 \n");
    assert_eq!(
        induction_slots(&asm_of(src)),
        0,
        "K alone is exact here; the guard must judge K*STEP"
    );
}

#[test]
fn unknown_step_does_not_get_an_induction_slot() {
    let src = "10 read s\n\
               20 for i=1 to 100 step s\n\
               30 x=3*i\n\
               40 next\n\
               50 print x\n\
               60 data .1\n";
    check_prints(src, " 299.999989 \n");
    assert_eq!(
        induction_slots(&asm_of(src)),
        0,
        "an unknown STEP is an unknown increment and must be declined"
    );
}

#[test]
fn still_reduces_a_non_integer_but_stable_increment() {
    let src = "10 for i=1 to 400:x=.5*i:next i\n20 print x\n";
    check_prints(src, " 200 \n");
    assert_eq!(
        induction_slots(&asm_of(src)),
        1,
        "a stable non-integer increment must keep its slot"
    );
}

#[test]
fn still_reduces_a_quarter_increment() {
    let src = "10 for i=1 to 400:x=.25*i:next i\n20 print x\n";
    check_prints(src, " 100 \n");
    assert_eq!(induction_slots(&asm_of(src)), 1);
}

/// The integer control.
#[test]
fn still_reduces_an_integer_increment() {
    let src = "10 for i=1 to 400:x=3*i:next i\n20 print x\n";
    check_prints(src, " 1200 \n");
    assert_eq!(induction_slots(&asm_of(src)), 1);
}

#[test]
fn still_reduces_when_only_the_product_is_stable() {
    let src = "10 for i=0 to 30 step 10:x=.1*i:next i\n20 print x\n";
    check_prints(src, " 3 \n");
    assert_eq!(induction_slots(&asm_of(src)), 1);
}

#[test]
fn trailing_zeros_match_the_emitted_pool_constants() {
    // $7D,$4C,$CC,$CC,$CD -- 0.1, mantissa 0xCCCCCCCD
    assert_eq!(mflpt_mantissa_trailing_zeros(0.1), Some(0));
    // $7F,$19,$99,$99,$9A -- 0.3, mantissa 0x9999999A
    assert_eq!(mflpt_mantissa_trailing_zeros(0.3), Some(1));
    // $82,$40,$00,$00,$00 -- 3.0, mantissa 0xC0000000
    assert_eq!(mflpt_mantissa_trailing_zeros(3.0), Some(30));

    assert_eq!(mflpt_mantissa_trailing_zeros(0.5), Some(31));
    assert_eq!(mflpt_mantissa_trailing_zeros(0.25), Some(31));
    assert_eq!(mflpt_mantissa_trailing_zeros(-0.5), Some(31));

    assert_eq!(mflpt_mantissa_trailing_zeros(1099511627777.0), Some(31));

    assert_eq!(mflpt_mantissa_trailing_zeros(4294967295.0), Some(0));

    // Outside the format entirely.
    assert_eq!(mflpt_mantissa_trailing_zeros(0.0), None);
    assert_eq!(mflpt_mantissa_trailing_zeros(f64::NAN), None);
    assert_eq!(mflpt_mantissa_trailing_zeros(f64::INFINITY), None);
    assert_eq!(mflpt_mantissa_trailing_zeros(1.0e300), None);
}

#[test]
fn the_headroom_boundary_is_pinned_by_value() {
    let just_enough = 2147549184.0_f64;
    let one_short = 2147516416.0_f64;
    assert_eq!(mflpt_mantissa_trailing_zeros(just_enough), Some(16));
    assert_eq!(mflpt_mantissa_trailing_zeros(one_short), Some(15));

    let cm = CostModel::default();
    assert!(cm.loop_induction_worth_it(just_enough, 1.0));
    assert!(!cm.loop_induction_worth_it(one_short, 1.0));

    assert_eq!(INDUCTION_MIN_TRAILING_ZEROS, 16);
}

#[test]
fn an_inexact_step_is_refused_even_when_the_product_is_exact() {
    let cm = CostModel::default();

    assert_eq!(0.1_f64 * 10.0, 1.0, "premise: the product really is 1.0");
    assert_eq!(10.0_f64 * 0.1, 1.0, "premise: and in the other order too");

    assert!(
        !cm.loop_induction_worth_it(10.0, 0.1),
        "STEP .1 drifts the ROM's counter; the exact product hides it"
    );
    assert!(
        !cm.loop_induction_worth_it(3.0, 0.1),
        "same shape with a product of .3"
    );

    assert!(
        cm.loop_induction_worth_it(0.1, 10.0),
        "STEP 10 is exact and the product is 1.0"
    );

    assert!(cm.loop_induction_worth_it(3.0, 0.0), "zero STEP");
}

/// The guard's decision table, stated as values.
#[test]
fn guard_judges_the_product_of_k_and_step() {
    let cm = CostModel::default();

    // Refused: the product cannot accumulate.
    assert!(!cm.loop_induction_worth_it(0.1, 1.0), ".1*1");
    assert!(!cm.loop_induction_worth_it(0.7, 1.0), ".7*1");
    assert!(!cm.loop_induction_worth_it(3.0, 0.1), "3*.1 = .3");
    assert!(!cm.loop_induction_worth_it(0.015, 1.0), "Rox 64");
    assert!(!cm.loop_induction_worth_it(39.9, 1.0), "Crazy Joe");
    assert!(!cm.loop_induction_worth_it(2.891, 1.0), "Fallschirmspringen");
    assert!(!cm.loop_induction_worth_it(1.8, -1.0), "Schiffe versenken");
    assert!(!cm.loop_induction_worth_it(0.4, 1.0), "Karzalean Attack");

    // Kept: the product is stable.
    assert!(cm.loop_induction_worth_it(3.0, 1.0), "3*1");
    assert!(cm.loop_induction_worth_it(0.5, 1.0), ".5*1");
    assert!(cm.loop_induction_worth_it(0.25, 2.0), ".25*2 = .5");
    assert!(cm.loop_induction_worth_it(7.5, 1.0), "7.5*1");
    assert!(cm.loop_induction_worth_it(0.1, 10.0), ".1*10 = 1");
    assert!(cm.loop_induction_worth_it(3.0, -1.0), "negative step");

    assert!(!cm.loop_induction_worth_it(0.0, 1.0), "K=0");
    assert!(!cm.loop_induction_worth_it(1.0, 1.0), "K=1");
    assert!(!cm.loop_induction_worth_it(-1.0, 1.0), "K=-1");
    assert!(!cm.loop_induction_worth_it(4.0, 1.0), "power of two");
    assert!(!cm.loop_induction_worth_it(1024.0, 1.0), "power of two");

    assert!(!cm.loop_induction_worth_it(f64::NAN, 1.0));
    assert!(!cm.loop_induction_worth_it(3.0, f64::NAN));
    assert!(!cm.loop_induction_worth_it(f64::INFINITY, 1.0));
}
