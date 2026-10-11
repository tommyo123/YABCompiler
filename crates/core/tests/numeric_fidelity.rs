//! Numeric results must be the ROM's results.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const VARIANTS: [(Profile, bool); 6] = [
    (Profile::Default, false),
    (Profile::Default, true),
    (Profile::Speed, false),
    (Profile::Speed, true),
    (Profile::Size, false),
    (Profile::Size, true),
];

fn compile_variant(src: &str, profile: Profile, extraram: bool) -> yabcompiler_core::Compiled {
    let prg = tokenize_program(src).expect("tokenizes");
    let opts = CompileOptions {
        profile,
        extraram,
        force_extraram_off: !extraram,
        ..CompileOptions::default()
    };
    compile_with_options(&prg, opts)
        .unwrap_or_else(|e| panic!("compiles ({profile:?}, extraram={extraram}): {e}\n{src}"))
}
fn check_output(src: &str, expected: &str) {
    for (profile, extraram) in VARIANTS {
        let bytes = compile_variant(src, profile, extraram).prg_bytes;
        let got = common::run_clean(&bytes)
            .unwrap_or_else(|| panic!("runs to a clean exit ({profile:?}, {extraram}):\n{src}"));
        assert_eq!(
            got, expected,
            "{profile:?} extraram={extraram} mismatch for:\n{src}"
        );
    }
}
fn nums(fields: &[&str]) -> String {
    fields.iter().map(|f| format!("{f} \n")).collect()
}

/// Assert a substring is present (or absent) in the emitted assembly of every shipped variant.
fn check_asm(src: &str, needle: &str, want: bool) {
    for (profile, extraram) in VARIANTS {
        let asm = compile_variant(src, profile, extraram).asm;
        assert_eq!(
            asm.contains(needle),
            want,
            "{profile:?} extraram={extraram}: expected contains({needle:?})={want} for:\n{src}"
        );
    }
}

/// Under `--extraram` the BASIC ROM is banked out and every ROM call is routed through a
/// jump-table stub, so a raw `JSR $xxxx` only appears in the non-extraram layouts.
fn check_rom_call(src: &str, addr: &str, want: bool) {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let asm = compile_variant(src, profile, false).asm;
        let stand_in = (addr == "$B7F7").then_some("JSR __FACWORD");
        assert_eq!(
            asm.contains(&format!("JSR {addr}")) || stand_in.is_some_and(|s| asm.contains(s)),
            want,
            "{profile:?}: expected a JSR {addr} = {want} in:\n{src}"
        );
    }
}

#[test]
fn sqr_of_a_runtime_value_matches_the_rom() {
    // The Newton build printed 2.00060976 / 5.00008043 / 4.00121951 for rows 1-3 and 5.00008043
    // for the Pythagorean idiom.
    check_output(
        "10 X=4:PRINT SQR(X)\n20 X=25:PRINT SQR(X)\n30 X=16:PRINT SQR(X)\n\
         40 X=2:PRINT SQR(X)\n50 X1=3:Y1=4:PRINT SQR(X1*X1+Y1*Y1)\n\
         60 X=1000000:PRINT SQR(X)\n70 X=.25:PRINT SQR(X)\n80 END\n",
        &nums(&[" 2", " 5", " 4", " 1.41421356", " 5", " 1000", " .5"]),
    );
}

/// ROM SQR is `EXP(.5*LOG(x))` and is not correctly rounded: `SQR(9)` is one ULP above 3.
#[test]
fn sqr_reproduces_the_roms_one_ulp_error() {
    check_output(
        "10 PRINT SQR(9)-3\n20 X=9:PRINT SQR(X)-3\n30 END\n",
        &nums(&[" 9.31322575E-10", " 9.31322575E-10"]),
    );
}

#[test]
fn sqr_calls_rom_bf71_and_the_newton_helper_is_gone() {
    let src = "10 X=2:PRINT SQR(X)\n20 END\n";
    check_rom_call(src, "$BF71", true);
    check_asm(src, "FAST_SQRT", false);
    check_asm(src, "__SQRT_N", false);
}

#[test]
fn cos_of_zero_is_not_exactly_one() {
    check_output(
        "10 PRINT INT(COS(0)*1000)\n20 PRINT INT(SIN(0)*1000)\n30 PRINT COS(0)\n40 END\n",
        &nums(&[" 999", " 0", " 1"]),
    );
}

#[test]
fn literal_transcendentals_reach_their_rom_entry() {
    // If any of these folded, the ROM call would not be emitted at all.
    for (src, addr) in [
        ("10 A=SIN(1)\n20 END\n", "$E26B"),
        ("10 A=COS(1)\n20 END\n", "$E264"),
        ("10 A=TAN(1)\n20 END\n", "$E2B4"),
        ("10 A=ATN(1)\n20 END\n", "$E30E"),
        ("10 A=LOG(2)\n20 END\n", "$B9EA"),
        ("10 A=EXP(1)\n20 END\n", "$BFED"),
        ("10 A=SQR(2)\n20 END\n", "$BF71"),
    ] {
        check_rom_call(src, addr, true);
    }
}

/// `EXP(1000)` is ?OVERFLOW and `TAN(pi/2)` is ?DIVISION BY ZERO on a real C64.
#[test]
fn trapping_transcendentals_are_left_to_the_rom() {
    check_rom_call("10 PRINT EXP(1000)\n20 END\n", "$BFED", true);
    check_rom_call("10 PRINT TAN(1.5707963267949)\n20 END\n", "$E2B4", true);
    check_asm(
        "10 A=VAL(\"1E40\")\n20 PRINT A\n30 END\n",
        "__VAL_HELPER",
        true,
    );
}

/// The other half of the contract: refusing to fold everything would also pass every
/// correctness test above.
#[test]
fn abs_int_sgn_still_fold() {
    let src = "10 PRINT ABS(-5)+INT(3.7)+SGN(-2)\n20 END\n";
    check_output(src, &nums(&[" 7"]));
    check_rom_call(src, "$BC58", false); // ABS
    check_rom_call(src, "$BCCC", false); // INT
    check_rom_call(src, "$BC39", false); // SGN
}

/// `10 A%=40000 : 20 PRINT A%` stops with ?ILLEGAL QUANTITY IN 10 on a real C64.
#[test]
fn out_of_range_int_store_is_not_constant_folded() {
    let src = "10 A%=40000\n20 PRINT A%\n30 END\n";
    check_asm(src, "JSR __AYINT", true);
    // The store must survive as a real conversion, so the program cannot reach its END: the ROM
    // raises inside AYINT.
    for (profile, extraram) in VARIANTS {
        let bytes = compile_variant(src, profile, extraram).prg_bytes;
        assert!(
            common::run_clean(&bytes).is_none(),
            "{profile:?} extraram={extraram}: A%=40000 must not complete"
        );
    }
}
#[test]
fn dead_out_of_range_int_store_survives_dead_store_elim() {
    check_asm("10 A%=40000\n20 END\n", "JSR __AYINT", true);
    check_asm("10 X=40000:A%=X\n20 END\n", "JSR __AYINT", true);
}

#[test]
fn ayint_narrowing_floors_and_is_reachable_from_every_context() {
    check_output(
        "10 A%=-1.5:PRINT A%\n20 A%=-7/2:PRINT A%\n30 A%=1.7:PRINT A%\n\
         40 X=-1.5:PRINT X AND -1\n50 PRINT 32767.5 AND -1\n60 END\n",
        &nums(&["-2", "-4", " 1", "-2", " 32767"]),
    );
}

/// `NOT` narrows through AYINT exactly like AND/OR do, so it floors.
#[test]
fn not_folds_with_ayint_floor_semantics() {
    check_output(
        "10 PRINT NOT -1.5\n20 PRINT NOT 1.7\n30 PRINT NOT 0\n40 END\n",
        &nums(&[" 1", "-2", "-1"]),
    );
}
#[test]
fn in_range_narrowing_keeps_the_facword_fast_path() {
    let src = "10 PRINT PEEK(53280) AND 15\n20 END\n";
    check_rom_call(src, "$B7F7", true);
    check_asm(src, "JSR __AYINT", false);
}
#[test]
fn dead_in_range_int_store_is_still_eliminated() {
    check_asm("10 A%=5\n20 END\n", "VI_A", false);
}

#[test]
fn power_of_a_literal_is_not_folded() {
    check_output(
        "10 PRINT 7^2\n20 PRINT 3^4\n30 PRINT 10^-10\n40 PRINT 2^10\n\
         50 PRINT 10^3\n60 PRINT 2^-2\n70 END\n",
        &nums(&[
            " 49.0000001",
            " 81.0000001",
            " 9.99999998E-11",
            " 1024",
            " 1000",
            " .25",
        ]),
    );
}
#[test]
fn power_strength_reduction_is_gone() {
    let src = "10 A=7\n20 PRINT A^2\n30 PRINT A^3\n40 PRINT A^4\n50 END\n";
    check_output(src, &nums(&[" 49.0000001", " 343", " 2401"]));
    check_rom_call(src, "$BF7B", true);
}
#[test]
fn exact_power_identities_still_fold() {
    let src = "10 PRINT 5^0;0^2;0^0;2^0\n20 END\n";
    check_output(src, " 1  0  1  1 \n");
    check_rom_call(src, "$BF7B", false);
}

/// The RHS of `^` is a whole `^` chain, not a bare atom, once a sign prefix opens FRMEVL's
/// `$7D` frame.
#[test]
fn unary_sign_after_caret_binds_the_whole_power_chain() {
    check_output(
        "10 PRINT 2^-1^3\n20 PRINT 2^-3^2\n30 PRINT 2^3^-1^2\n\
         40 PRINT 2^--1^2\n50 PRINT 4^-1^1\n60 END\n",
        &nums(&[" .5", " 1.95312499E-03", " .125", " 2", " .25"]),
    );
}
#[test]
fn unary_plus_after_caret_is_accepted_but_opens_no_frame() {
    check_output(
        "10 PRINT 2^+2\n20 PRINT 2^+1^3\n30 PRINT 2^-1^3\n40 END\n",
        &nums(&[" 4", " 8", " .5"]),
    );
}

/// The other side of the precedence: the `$7D` frame is *above* `*` (`$7B`), so the minus must
/// not swallow the multiply.
#[test]
fn unary_minus_after_caret_does_not_swallow_lower_precedence_operators() {
    check_output(
        "10 PRINT 2^-1*3\n20 PRINT 2^-1+1\n30 PRINT 2^3^2\n40 PRINT -2^2\n50 END\n",
        &nums(&[" 1.5", " 1.5", " 64", "-4"]),
    );
}

/// The ROM packs (and therefore rounds) the LEFT operand and keeps the right one in FAC with
/// its rounding byte live, so `a*b` and `b*a` can differ.
#[test]
fn commutative_operands_keep_the_rom_order() {
    check_output(
        "10 PRINT INT(LOG(EXP(1))*1000)\n20 PRINT INT(1000*LOG(EXP(1)))\n\
         30 A=LOG(EXP(1)):PRINT INT(A*1000)\n40 END\n",
        &nums(&[" 1000", " 999", " 1000"]),
    );
}
#[test]
fn constant_mem_operand_fast_path_still_fires_for_plain_loads() {
    let src = "10 DIM A(20)\n20 FOR I=0 TO 20:A(I)=I/3:NEXT I\n\
               30 FOR I=0 TO 20\n40 P=1.5*A(I)\n50 Q=A(I)+2.5\n\
               60 R=2.5+(P*Q)\n70 S=1.5*(P+Q)\n80 PRINT P;Q;R;S\n\
               90 NEXT I\n99 END\n";
    let asm = compile_variant(src, Profile::Default, false).asm;
    let stubs = asm
        .lines()
        .filter(|l| {
            l.ends_with(':')
                && ["__FADD_F", "__FSUB_F", "__FMUL_F", "__FDIV_F"]
                    .iter()
                    .any(|p| l.starts_with(p))
        })
        .count();
    assert!(
        stubs >= 2,
        "constant-memory-operand fast path stopped firing: {stubs} stubs\n{asm}"
    );
}
#[test]
fn computed_lhs_is_packed_to_a_temp_before_a_constant_operand() {
    let src = "10 PRINT INT(LOG(EXP(1))*1000)\n20 END\n";
    check_asm(src, "__ST__T0", true);
}
