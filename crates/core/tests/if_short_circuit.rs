//! `IF <a> AND <b> THEN` / `IF <a> OR <b> THEN` must not skip `<b>`.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

/// Compile `src` at `profile` (with extraram forced on or off) and return the final
/// post-peephole assembly.
fn asm_variant(src: &str, profile: Profile, extraram: bool) -> String {
    let prg = tokenize_program(src).expect("tokenizes");
    let opts = CompileOptions {
        profile,
        extraram,
        force_extraram_off: !extraram,
        ..CompileOptions::default()
    };
    compile_with_options(&prg, opts)
        .unwrap_or_else(|e| panic!("compiles ({profile:?}, extraram={extraram}): {e}\n{src}"))
        .asm
}

/// Which of the three `AND`/`OR` lowerings the emitted code used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lowering {
    /// Branch chain.
    ShortCircuit,
    /// Byte boolean operations.
    Byte,
    /// The int16 `__AYINT` combine through `__LOGIC_LO`/`__LOGIC_HI`.
    Int,
}

/// Read the lowering off the emitted code.
fn lowering_of(asm: &str) -> Lowering {
    match (asm.contains("__LOGIC_LO"), asm.contains("__LOGIC_B")) {
        (false, false) => Lowering::ShortCircuit,
        (true, false) => Lowering::Int,
        (false, true) => Lowering::Byte,
        (true, true) => panic!(
            "fixture emitted both stash cells, so the lowering it is being \
             scored on is ambiguous — split it into one condition per test:\n{asm}"
        ),
    }
}
fn check_all_variants(src: &str, expect: Lowering, also_contains: &[&str]) {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        for extraram in [false, true] {
            let asm = asm_variant(src, profile, extraram);
            assert_eq!(
                lowering_of(&asm),
                expect,
                "{profile:?} extraram={extraram}: wrong lowering for:\n{src}"
            );
            for needle in also_contains {
                assert!(
                    asm.contains(needle),
                    "{profile:?} extraram={extraram}: {needle:?} missing from the emitted code \
                     for:\n{src}"
                );
            }
        }
    }
}
const FDIV: &str = "JSR $BB0F";

// Must NOT short-circuit: the skipped operand is observable.

#[test]
fn trapping_rhs_of_and_is_still_evaluated() {
    check_all_variants(
        "10 A=PEEK(53280)\n20 IF A=99 AND (1/0)=1 THEN PRINT \"Y\"\n30 PRINT \"Z\"\n40 END\n",
        Lowering::Byte,
        &[FDIV],
    );
}
#[test]
fn trapping_rhs_of_or_is_still_evaluated() {
    check_all_variants(
        "10 A=PEEK(53280)\n20 IF A=99 OR (1/0)=1 THEN PRINT \"Y\"\n30 PRINT \"Z\"\n40 END\n",
        Lowering::Byte,
        &[FDIV],
    );
}
#[test]
fn folded_false_lhs_still_emits_a_trapping_rhs() {
    check_all_variants(
        "10 A=0\n20 IF A=1 AND (1/0)=1 THEN PRINT \"Y\"\n30 PRINT \"Z\"\n40 END\n",
        Lowering::Byte,
        &[FDIV],
    );
}
#[test]
fn folded_true_lhs_of_or_still_emits_a_trapping_rhs() {
    check_all_variants(
        "10 A=1\n20 IF A=1 OR (1/0)=1 THEN PRINT \"Y\"\n30 PRINT \"Z\"\n40 END\n",
        Lowering::Byte,
        &[FDIV],
    );
}
#[test]
fn impure_rnd_rhs_is_still_evaluated() {
    check_all_variants(
        "10 A=PEEK(53280)\n20 IF A=99 AND RND(1)<2 THEN PRINT \"Y\"\n\
         30 PRINT INT(RND(1)*1000)\n40 END\n",
        Lowering::Byte,
        &[],
    );
}

/// A user function body is an arbitrary expression the compiler does not look inside, so
/// calling one is opaque.
#[test]
fn def_fn_rhs_is_still_evaluated() {
    check_all_variants(
        "10 DEF FN F(Q)=1/Q\n20 A=PEEK(53280)\n\
         30 IF A=99 AND FN F(0)>0 THEN PRINT \"Y\"\n40 PRINT \"Z\"\n50 END\n",
        Lowering::Byte,
        &[FDIV],
    );
}
#[test]
fn io_space_peek_rhs_is_still_evaluated() {
    check_all_variants(
        "10 A=PEEK(53280)\n20 IF A=99 AND PEEK(53279)=1 THEN PRINT \"Y\"\n30 PRINT \"Z\"\n40 END\n",
        Lowering::Byte,
        &[],
    );
}
#[test]
fn trapping_leaf_nested_in_the_rhs_disqualifies_it() {
    check_all_variants(
        "10 A=PEEK(53280)\n20 IF A=99 AND (B=1 OR (1/0)=1) THEN PRINT \"Y\"\n\
         30 PRINT \"Z\"\n40 END\n",
        Lowering::Byte,
        &[FDIV],
    );
}

/// The bread-and-butter shape.
#[test]
fn plain_and_of_two_comparisons_still_short_circuits() {
    check_all_variants(
        "10 A=PEEK(53280)\n20 IF A=99 AND B=1 THEN PRINT \"Y\"\n30 PRINT \"Z\"\n40 END\n",
        Lowering::ShortCircuit,
        &[],
    );
}

#[test]
fn plain_or_of_two_comparisons_still_short_circuits() {
    check_all_variants(
        "10 A=PEEK(53280)\n20 IF A=99 OR B=1 THEN PRINT \"Y\"\n30 PRINT \"Z\"\n40 END\n",
        Lowering::ShortCircuit,
        &[],
    );
}

/// `AND` is left-associative, so a three-term chain puts a bare comparison on each rhs and both
/// levels stay eligible.
#[test]
fn three_term_and_chain_still_short_circuits() {
    check_all_variants(
        "10 A=PEEK(53280)\n20 IF A=99 AND B=1 AND C=2 THEN PRINT \"Y\"\n\
         30 PRINT \"Z\"\n40 END\n",
        Lowering::ShortCircuit,
        &[],
    );
}

/// A parenthesised sub-condition on the rhs.
#[test]
fn nested_bool_rhs_still_short_circuits() {
    check_all_variants(
        "10 A=PEEK(53280)\n20 IF A=99 AND (B=1 OR C=2) THEN PRINT \"Y\"\n\
         30 PRINT \"Z\"\n40 END\n",
        Lowering::ShortCircuit,
        &[],
    );
}
#[test]
fn not_of_a_comparison_on_the_rhs_still_short_circuits() {
    check_all_variants(
        "10 A=PEEK(53280)\n20 IF A=99 AND NOT (B=1) THEN PRINT \"Y\"\n30 PRINT \"Z\"\n40 END\n",
        Lowering::ShortCircuit,
        &[],
    );
}
#[test]
fn ram_peek_rhs_still_short_circuits() {
    check_all_variants(
        "10 A=PEEK(53280)\n20 IF A=99 AND PEEK(1024)=5 THEN PRINT \"Y\"\n30 PRINT \"Z\"\n40 END\n",
        Lowering::ShortCircuit,
        &[],
    );
}
#[test]
fn string_compare_rhs_still_short_circuits() {
    check_all_variants(
        "10 B$=\"N\"\n20 INPUT C$\n30 IF C$=\"Y\" AND B$=\"N\" THEN PRINT \"YES\"\n\
         40 PRINT \"Z\"\n50 END\n",
        Lowering::ShortCircuit,
        &[],
    );
}
#[test]
fn substring_rhs_is_still_evaluated() {
    check_all_variants(
        "10 INPUT B$\n20 INPUT C$\n30 IF C$=\"Y\" AND MID$(B$,1,1)=\"N\" THEN PRINT \"YES\"\n\
         40 PRINT \"Z\"\n50 END\n",
        Lowering::Int,
        &[],
    );
}
#[test]
fn constant_arithmetic_rhs_still_short_circuits() {
    check_all_variants(
        "10 A=PEEK(53280)\n20 IF A=99 AND B=2*3 THEN PRINT \"Y\"\n30 PRINT \"Z\"\n40 END\n",
        Lowering::ShortCircuit,
        &[],
    );
}
