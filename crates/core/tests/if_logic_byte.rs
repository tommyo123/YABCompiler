

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

/// The byte lowering's stash.
fn takes_byte_path(asm: &str) -> bool {
    asm.contains("STA __LOGIC_B")
}

/// The int16 lowering's stash.
fn takes_int_path(asm: &str) -> bool {
    asm.contains("STA __LOGIC_LO")
}

fn assert_path(src: &str, byte: bool, int: bool) {
    for (profile, extraram) in VARIANTS {
        let asm = compile_variant(src, profile, extraram).asm;
        assert_eq!(
            takes_byte_path(&asm),
            byte,
            "{profile:?} extraram={extraram}: expected byte-path={byte} for:\n{src}"
        );
        assert_eq!(
            takes_int_path(&asm),
            int,
            "{profile:?} extraram={extraram}: expected int-path={int} for:\n{src}"
        );
    }
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

/// The canonical target: a scalar compare AND an array compare.
#[test]
fn scalar_and_array_compare_takes_the_byte_path() {
    assert_path(
        "10 DIM C(40)\n20 L=PEEK(53280)\n30 IF L=34 AND C(31)=2 THEN POKE 1024,1\n40 END\n",
        true,
        false,
    );
}

/// `OR` shares the lowering.
#[test]
fn or_of_two_barred_comparisons_takes_the_byte_path() {
    assert_path(
        "10 DIM C(40)\n20 L=PEEK(53280)\n30 IF L=34 OR C(31)=2 THEN POKE 1024,1\n40 END\n",
        true,
        false,
    );
}

/// An impure operand (`RND` advances the seed) is barred from short-circuiting for a completely
/// different reason and lands on the same path.
#[test]
fn impure_operand_takes_the_byte_path() {
    assert_path(
        "10 A=PEEK(53280)\n20 IF A=99 AND RND(1)<2 THEN POKE 1024,1\n30 END\n",
        true,
        false,
    );
}

/// A read-clear I/O register is impure the same way.
#[test]
fn io_peek_operand_takes_the_byte_path() {
    assert_path(
        "10 A=PEEK(53280)\n20 IF A=99 AND PEEK(53279)=1 THEN POKE 1024,1\n30 END\n",
        true,
        false,
    );
}
#[test]
fn value_context_takes_the_byte_path() {
    assert_path(
        "10 DIM C(4)\n20 L=PEEK(53280)\n30 X=(L=34) AND (C(1)=2)\n40 PRINT X\n50 END\n",
        true,
        false,
    );
}
#[test]
fn exor_of_two_comparisons_takes_the_byte_path() {
    assert_path(
        "10 A=PEEK(53280)\n20 B=PEEK(53281)\n30 IF EXOR(A=1,B=2) THEN POKE 1024,1\n40 END\n",
        true,
        false,
    );
}
#[test]
fn not_of_a_comparison_stays_in_the_byte_domain() {
    let asm =
        compile_variant("10 DIM C(4)\n20 A=PEEK(53280)\n30 IF NOT(A=1) AND C(1)=2 THEN POKE 1024,1\n40 END\n",
        Profile::Default, false).asm;
    assert!(takes_byte_path(&asm), "byte path must fire:\n{asm}");
    assert!(
        asm.contains("EOR #$FF"),
        "NOT must lower to a byte complement:\n{asm}"
    );
}
#[test]
fn the_branch_context_condition_makes_no_float() {
    let src = "10 DIM C(40)\n20 L=PEEK(53280)\n30 IF L=34 AND C(31)=2 THEN POKE 1024,1\n40 END\n";
    for (profile, extraram) in VARIANTS {
        let asm = compile_variant(src, profile, extraram).asm;
        assert!(takes_byte_path(&asm), "{profile:?}: byte path must fire");
        for gone in ["JSR __AYINT", "JSR __BOOL_TO_FAC"] {
            assert!(
                !asm.contains(gone),
                "{profile:?} extraram={extraram}: {gone} must be gone from:\n{asm}"
            );
        }
    }
}

/// `IF PEEK(addr) AND 16 THEN` is a bit *mask*, not a logical AND.
#[test]
fn a_bit_mask_is_not_lowered_in_the_byte_domain() {
    for (profile, extraram) in VARIANTS {
        let asm = compile_variant(
            "10 IF PEEK(56320) AND 16 THEN POKE 1024,1\n20 END\n",
            profile,
            extraram,
        )
        .asm;
        assert!(
            !takes_byte_path(&asm),
            "{profile:?} extraram={extraram}: a mask must not take the byte path:\n{asm}"
        );
    }
}

/// A float variable is not -1/0 either.
#[test]
fn a_float_operand_is_not_lowered_in_the_byte_domain() {
    assert_path(
        "10 A=PEEK(53280)/4\n20 B=PEEK(53281)/4\n30 X=A AND B\n40 PRINT X\n50 END\n",
        false,
        true,
    );
}

/// A condition that allocates from the string heap keeps the int16 lowering.
#[test]
fn a_heap_allocating_condition_keeps_the_int_lowering() {
    assert_path(
        "10 INPUT B$\n20 INPUT C$\n30 IF C$=\"y\" AND MID$(B$,1,1)=\"n\" THEN POKE 1024,1\n40 END\n",
        false,
        true,
    );
}
#[test]
fn a_non_allocating_string_compare_is_eligible() {
    assert_path(
        "10 INPUT B$\n20 IF B$=\"y\" AND ASC(B$)=1 THEN POKE 1024,1\n30 END\n",
        true,
        false,
    );
}
#[test]
fn a_short_circuitable_condition_still_short_circuits() {
    assert_path(
        "10 A=PEEK(53280)\n20 IF A=99 AND B=1 THEN POKE 1024,1\n30 END\n",
        false,
        false,
    );
}

/// Branch context, 2x2.
#[test]
fn and_or_not_truth_table_in_a_branch_context() {
    check_output(
        "10 DIM Q(1)\n20 Q(0)=0:Q(1)=1\n30 FOR A=0 TO 1\n40 FOR B=0 TO 1\n50 T=0\n\
         60 IF A=1 AND Q(B)=1 THEN T=T+1\n70 IF A=1 OR Q(B)=1 THEN T=T+2\n\
         80 IF NOT(A=1) AND Q(B)=1 THEN T=T+4\n90 PRINT T\n100 NEXT B\n110 NEXT A\n120 END\n",
        &nums(&[" 0", " 6", " 2", " 3"]),
    );
}

/// Value context, 2x2, printing the -1/0 values themselves.
#[test]
fn and_or_not_truth_table_in_a_value_context() {
    check_output(
        "10 DIM Q(1)\n20 Q(0)=0:Q(1)=1\n30 FOR A=0 TO 1\n40 FOR B=0 TO 1\n\
         50 X=(A=1) AND (Q(B)=1)\n60 Y=(A=1) OR (Q(B)=1)\n70 Z=NOT(A=1) AND (Q(B)=1)\n\
         80 PRINT X;Y;Z\n90 NEXT B\n100 NEXT A\n110 END\n",
        " 0  0  0 \n 0 -1 -1 \n 0 -1  0 \n-1 -1  0 \n",
    );
}
#[test]
fn three_term_chains_and_nested_subconditions() {
    check_output(
        "10 DIM Q(1)\n20 Q(0)=0:Q(1)=1\n30 FOR A=0 TO 1\n40 FOR B=0 TO 1\n50 FOR C=0 TO 1\n\
         60 T=0\n70 IF A=1 AND Q(B)=1 AND Q(C)=1 THEN T=T+1\n\
         80 IF A=1 AND (Q(B)=1 OR Q(C)=1) THEN T=T+2\n\
         90 IF (A=1 OR Q(B)=1) AND Q(C)=1 THEN T=T+4\n100 PRINT T\n\
         110 NEXT C\n120 NEXT B\n130 NEXT A\n140 END\n",
        &nums(&[" 0", " 0", " 0", " 4", " 0", " 6", " 2", " 7"]),
    );
}
#[test]
fn exor_truth_table() {
    check_output(
        "10 DIM Q(1)\n20 Q(0)=0:Q(1)=1\n30 FOR A=0 TO 1\n40 FOR B=0 TO 1\n\
         50 PRINT EXOR(A=1,Q(B)=1)\n60 NEXT B\n70 NEXT A\n80 END\n",
        &nums(&[" 0", "-1", "-1", " 0"]),
    );
}

/// The stash is live across the right operand's evaluation, so a bitwise operator *inside* that
/// operand must take its own cell.
#[test]
fn right_nested_chains_keep_every_stash() {
    let src = "10 DIM Q(1)\n20 Q(0)=0:Q(1)=1\n30 FOR A=0 TO 1\n40 FOR B=0 TO 1\n\
               50 FOR C=0 TO 1\n60 FOR D=0 TO 1\n70 T=0\n\
               80 IF Q(A)=1 AND (Q(B)=1 AND (Q(C)=1 AND Q(D)=1)) THEN T=1\n\
               90 PRINT T\n100 NEXT D\n110 NEXT C\n120 NEXT B\n130 NEXT A\n140 END\n";
    check_output(
        src,
        &nums(&[
            " 0", " 0", " 0", " 0", " 0", " 0", " 0", " 0", " 0", " 0", " 0", " 0", " 0", " 0",
            " 0", " 1",
        ]),
    );
    let asm = compile_variant(src, Profile::Default, false).asm;
    for cell in ["__LOGIC_B:", "__LOGIC_B1:", "__LOGIC_B2:"] {
        assert!(asm.contains(cell), "{cell} must be allocated:\n{asm}");
    }
}

/// A byte-path condition with an int16 bitwise operator nested inside one of its operands.
#[test]
fn an_int_mask_nested_inside_a_byte_path_condition() {
    let src = "10 DIM Q(1)\n20 Q(0)=0:Q(1)=1\n30 READ M\n40 T=0\n\
               50 IF Q(1)=1 AND (M AND 10)=12 THEN T=T+1\n\
               60 IF Q(1)=1 OR (M AND 10)=12 THEN T=T+2\n\
               70 PRINT T\n80 DATA 12\n90 END\n";
    check_output(src, &nums(&[" 2"]));
    let asm = compile_variant(src, Profile::Default, false).asm;
    assert!(takes_byte_path(&asm), "outer must be byte:\n{asm}");
    assert!(takes_int_path(&asm), "inner mask must stay int:\n{asm}");
}

/// FDIV is the only route to ?DIVISION BY ZERO; the ROM `LOG` is the only route to `LOG(0)`'s
/// ?ILLEGAL QUANTITY.
fn check_order(src: &str, first: (&str, &str), second: (&str, &str)) {
    for (profile, extraram) in VARIANTS {
        let asm = compile_variant(src, profile, extraram).asm;
        assert!(takes_byte_path(&asm), "{profile:?}: byte path must fire");
        let f = if extraram { first.1 } else { first.0 };
        let s = if extraram { second.1 } else { second.0 };
        let a = asm
            .find(f)
            .unwrap_or_else(|| panic!("{profile:?} extraram={extraram}: {f} missing:\n{src}"));
        let b = asm
            .find(s)
            .unwrap_or_else(|| panic!("{profile:?} extraram={extraram}: {s} missing:\n{src}"));
        assert!(
            a < b,
            "{profile:?} extraram={extraram}: {f} must precede {s} for:\n{src}"
        );
    }
}

const FDIV: (&str, &str) = ("JSR $BB0F", "JSR __JT_FDIV");
const FN_LOG: (&str, &str) = ("JSR $B9EA", "JSR __JT_FN_LOG");

#[test]
fn the_byte_path_evaluates_the_left_operand_first() {
    check_order(
        "10 IF (1/0)=1 AND LOG(0)=1 THEN POKE 1024,1\n20 END\n",
        FDIV,
        FN_LOG,
    );
}

/// The mirror image: swapping the operands swaps which trap is reached first.
#[test]
fn swapping_the_operands_swaps_the_order() {
    check_order(
        "10 IF LOG(0)=1 AND (1/0)=1 THEN POKE 1024,1\n20 END\n",
        FN_LOG,
        FDIV,
    );
}
#[test]
fn the_value_context_evaluates_the_left_operand_first() {
    check_order(
        "10 X=((1/0)=1) AND (LOG(0)=1)\n20 PRINT X\n30 END\n",
        FDIV,
        FN_LOG,
    );
}
#[test]
fn a_statically_false_lhs_still_emits_the_right_operand() {
    for (profile, extraram) in VARIANTS {
        let asm = compile_variant(
            "10 A=0\n20 IF A=1 AND (1/0)=1 THEN POKE 1024,1\n30 END\n",
            profile,
            extraram,
        )
        .asm;
        assert!(takes_byte_path(&asm), "{profile:?}: byte path must fire");
        let fdiv = if extraram { FDIV.1 } else { FDIV.0 };
        assert!(
            asm.contains(fdiv),
            "{profile:?} extraram={extraram}: the division must survive:\n{asm}"
        );
    }
}
