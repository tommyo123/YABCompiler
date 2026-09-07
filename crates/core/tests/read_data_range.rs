//! A `READ` target's `IntRange` is an *integrality* claim, so DATA that contains a fraction
//! must produce no range at all.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn compile_asm(src: &str) -> String {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(&prg, CompileOptions::default())
        .expect("compiles")
        .asm
}

fn output_at(src: &str, profile: Profile) -> String {
    let prg = tokenize_program(src).expect("tokenizes");
    let opts = CompileOptions {
        profile,
        ..Default::default()
    };
    let c = compile_with_options(&prg, opts).expect("compiles");
    let r = emu64::run_prg_to_end(&c.prg_bytes, common::MAX_INSNS).expect("runs");
    assert!(r.clean_exit, "must reach the final RTS:\n{src}");
    common::petscii_to_string(&r.output)
}

const FRACTIONAL: &str = "\
10 READ P,Q
20 PRINT INT(P)
30 IF P>2 THEN PRINT \"GT2\"
40 IF P<3 THEN PRINT \"LT3\"
50 PRINT P
60 PRINT INT(-P)
70 PRINT P-INT(P)
80 END
9000 DATA 2.7,1.6
";
#[test]
fn a_variable_read_from_fractional_data_keeps_its_fraction() {
    for profile in [Profile::Default, Profile::Size, Profile::Speed] {
        assert_eq!(
            output_at(FRACTIONAL, profile),
            " 2 \nGT2\nLT3\n 2.7 \n-3 \n .7 \n",
            "profile {profile:?}"
        );
    }
}
#[test]
fn int_is_not_elided_on_a_fractional_read_target() {
    let asm = compile_asm(FRACTIONAL);
    assert!(
        asm.contains("JSR $BCCC"),
        "INT() was elided on a value READ from fractional DATA:\n{asm}"
    );
}
#[test]
fn one_fractional_entry_is_enough_to_drop_the_range() {
    let src = "\
10 READ A,B
20 PRINT INT(A);INT(B)
30 END
9000 DATA 5,7.5
";
    assert!(compile_asm(src).contains("JSR $BCCC"));
    assert_eq!(output_at(src, Profile::Default), " 5  7 \n");
}
#[test]
fn integral_data_still_proves_integrality_and_elides_int() {
    let src = "\
10 READ A,B
20 PRINT INT(A);INT(B)
30 END
9000 DATA 5,7
";
    let asm = compile_asm(src);
    assert!(
        !asm.contains("JSR $BCCC"),
        "INT() should still be elided when DATA is all integral:\n{asm}"
    );
    assert_eq!(output_at(src, Profile::Default), " 5  7 \n");
}
#[test]
fn integral_and_fractional_pools_agree_on_behaviour() {
    let integral = "\
10 READ A
20 FOR I=1 TO 2:POKE A,I:A=A+1:NEXT
30 PRINT PEEK(830);PEEK(831)
40 END
9000 DATA 830
";
    let fractional = "\
10 READ A
20 FOR I=1 TO 2:POKE A,I:A=A+1:NEXT
30 PRINT PEEK(830);PEEK(831)
40 END
9000 DATA 830
9010 DATA 0.5
";
    assert!(
        compile_asm(integral).contains("2 bytes/entry"),
        "all-integral DATA should still use the compact i16 pool"
    );
    assert!(
        compile_asm(fractional).contains("5 bytes/entry"),
        "a fractional entry forces the MFLPT pool"
    );
    assert_eq!(output_at(integral, Profile::Default), " 1  2 \n");
    assert_eq!(output_at(fractional, Profile::Default), " 1  2 \n");
}
