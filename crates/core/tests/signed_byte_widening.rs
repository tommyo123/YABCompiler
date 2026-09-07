mod common;

use yabcompiler_core::{CompileOptions, Compiled, Profile, compile_with_options, tokenize_program};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

fn build(src: &str, profile: Profile) -> Compiled {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(
        &prg,
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("must compile ({profile:?}): {e}\n{src}"))
}

#[track_caller]
fn check_out(src: &str, want: &str) {
    for profile in PROFILES {
        let c = build(src, profile);
        let r = emu64::run_prg_to_end(&c.prg_bytes, common::MAX_INSNS).expect("run");
        assert_eq!(
            r.error_banner(),
            None,
            "no error expected ({profile:?}):\n{src}"
        );
        assert_eq!(
            common::petscii_to_string(&r.output)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
            want,
            "({profile:?}):\n{src}"
        );
    }
}

const RHS_SIGNED: &str = "\
10 dim a(9)
20 gosub 100
30 for j=0 to 3
40 if a(j)=e then 60
50 print \"v\";j;
60 next j
70 print \"done\"
80 end
100 e=-9
105 if a(0)=1 then e=-8
110 for j=1 to 3
120 a(j)=e
130 next j
140 return
";

const LHS_SIGNED_POSITIVE_VALUE: &str = "\
10 dim a(9)
20 gosub 100
30 for j=0 to 3
40 if e=a(j) then 60
50 print \"v\";j;
60 next j
70 print \"done\"
80 end
100 e=5
105 if a(0)=1 then e=-9
110 for j=1 to 3
120 a(j)=e
130 next j
140 return
";

/// `E` is the AND operand against a two-byte negative value.
const BITWISE_SIGNED: &str = "\
10 gosub 100
20 q=x and e
30 print q
40 end
100 e=-9
105 if q=1 then e=-8
110 x=-256
115 if q=1 then x=-255
120 return
";

#[test]
fn compare_rhs_signed_byte_is_sign_extended() {

    check_out(RHS_SIGNED, "V 0 DONE");
}

#[test]
fn compare_lhs_signed_byte_does_not_clobber_the_borrow() {

    check_out(LHS_SIGNED_POSITIVE_VALUE, "V 0 DONE");
}

#[test]
fn bitwise_operand_signed_byte_is_sign_extended() {

    check_out(BITWISE_SIGNED, "-256");
}

#[test]
fn unary_minus_on_a_signed_byte_is_sign_extended() {
    let src = "\
10 gosub 100
20 q=-e
30 print q
40 end
100 e=-9
105 if q=1 then e=-8
120 return
";
    check_out(src, "9");
}

#[test]
fn signed_byte_storage_is_still_used() {
    for (name, src) in [
        ("compare", RHS_SIGNED),
        ("compare-lhs", LHS_SIGNED_POSITIVE_VALUE),
        ("bitwise", BITWISE_SIGNED),
    ] {
        for profile in PROFILES {
            let asm = build(src, profile).asm;
            assert!(
                asm.contains("__SXT_TMP_"),
                "{name} ({profile:?}) no longer sign-extends a one-byte \
                 signed variable — either byte storage was abandoned or \
                 the widening regressed to a zero-extension:\n{asm}"
            );
        }
    }
}

const RHS_UNSIGNED: &str = "\
10 dim a(9)
20 gosub 100
30 for j=0 to 3
40 if a(j)=e then 60
50 print \"v\";j;
60 next j
70 print \"done\"
80 end
100 e=9
105 if a(0)=1 then e=8
110 for j=1 to 3
120 a(j)=e
130 next j
140 return
";

/// `X AND E` with both operands non-negative.
const BITWISE_UNSIGNED: &str = "\
10 gosub 100
20 q=x and e
30 print q
40 end
100 e=12
105 if q=1 then e=10
110 x=260
115 if q=1 then x=261
120 return
";

#[test]
fn unsigned_byte_stays_cheap() {
    for (name, src) in [
        ("compare", RHS_UNSIGNED),
        ("bitwise", BITWISE_UNSIGNED),
    ] {
        for profile in PROFILES {
            let asm = build(src, profile).asm;
            assert!(
                !asm.contains("__SXT_TMP_"),
                "{name} ({profile:?}) sign-extends an UNSIGNED byte — the \
                 widening fix must be conditioned on the variable's range, \
                 not applied to every one-byte operand:\n{asm}"
            );
        }
    }
}

#[test]
fn unsigned_byte_compare_is_still_correct() {

    check_out(RHS_UNSIGNED, "V 0 DONE");
}

#[test]
fn unsigned_byte_bitwise_is_still_correct() {

    check_out(BITWISE_UNSIGNED, "4");
}

#[test]
fn two_byte_operands_are_untouched() {
    let src = "\
10 gosub 100
20 if x=y then print \"eq\";
30 if x<>y then print \"ne\";
40 print x and y
50 end
100 x=-256
105 if x=1 then x=-255
110 y=-256
115 if y=1 then y=-255
120 return
";
    for profile in PROFILES {
        let asm = build(src, profile).asm;
        assert!(
            !asm.contains("__SXT_TMP_"),
            "two-byte operands must not gain a sign-extend scratch \
             ({profile:?}):\n{asm}"
        );
    }
    check_out(src, "EQ-256");
}

#[test]
fn every_relational_operator_handles_a_signed_byte() {
    for (op, want) in [
        ("=", "TDONE"),
        ("<>", "DONE"),
        ("<", "DONE"),
        ("<=", "TDONE"),
        (">", "DONE"),
        (">=", "TDONE"),
    ] {
        let src = format!(
            "\
10 dim a(9)
20 gosub 100
30 if a(1){op}e then print \"t\";
40 print \"done\"
50 end
100 e=-9
105 if a(0)=1 then e=-8
110 a(1)=e
120 return
"
        );
        check_out(&src, want);
    }
}

#[test]
fn every_relational_operator_handles_a_signed_byte_on_the_left() {
    for (op, want) in [
        ("=", "TDONE"),
        ("<>", "DONE"),
        ("<", "DONE"),
        ("<=", "TDONE"),
        (">", "DONE"),
        (">=", "TDONE"),
    ] {
        let src = format!(
            "\
10 dim a(9)
20 gosub 100
30 if e{op}a(1) then print \"t\";
40 print \"done\"
50 end
100 e=5
105 if a(0)=1 then e=-9
110 a(1)=e
120 return
"
        );
        check_out(&src, want);
    }
}

#[test]
fn bitwise_or_with_a_signed_byte_is_sign_extended() {
    let src = "\
10 gosub 100
20 q=x or e
30 print q
40 end
100 e=-9
105 if q=1 then e=-8
110 x=-256
115 if q=1 then x=-255
120 return
";
    check_out(src, "-9");
}

#[test]
fn immediate_operands_are_unaffected() {
    let src = "\
10 gosub 100
20 if e=-9 then print \"eq\";
30 if e<>-9 then print \"ne\";
40 print e and -1
50 end
100 e=-9
105 if e=1 then e=-8
120 return
";
    check_out(src, "EQ-9");
}

#[test]
fn the_four_square_board_loop_does_not_raise() {
    let src = "\
10 dim a(99)
20 gosub 100
30 for j=0 to 20
40 if a(j)=e then 60
50 print chr$(a(j));
60 next j
70 print \"done\"
80 end
100 e=-9
105 if a(0)=1 then e=-8
110 n=asc(\".\")
120 for j=1 to 99
130 a(j)=e
140 z=j-10*int(j/10)
150 if z=0 or z=8 or z=9 then 170
160 a(j)=n
170 next j
180 return
";
    // A(0) is 0 -> CHR$(0), which prints nothing and is legal.
    check_out(src, "..............DONE");
}
