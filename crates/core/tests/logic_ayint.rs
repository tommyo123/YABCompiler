//! `AND` / `OR` / `EXOR`: AYINT narrowing of the operands, and the order the operands are
//! evaluated in.

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

/// Assert on all six variants whether the bitwise lowering survived into the emitted code.
fn check_simplified(src: &str, expect_simplified: bool) {
    for (profile, extraram) in VARIANTS {
        let asm = compile_variant(src, profile, extraram).asm;
        assert_eq!(
            !asm.contains("__LOGIC_LO"),
            expect_simplified,
            "{profile:?} extraram={extraram}: expected simplified={expect_simplified} for:\n{src}"
        );
    }
}

/// A trap the operand order can be read off, named by how its call site looks in the emitted
/// assembly.
#[derive(Clone, Copy)]
struct Trap {
    /// Call site without extraram.
    rom: &'static str,
    jt: &'static str,
}

impl Trap {
    fn site(&self, extraram: bool) -> &'static str {
        if extraram && !self.jt.is_empty() {
            self.jt
        } else {
            self.rom
        }
    }
}
const FDIV: Trap = Trap {
    rom: "JSR $BB0F",
    jt: "JSR __JT_FDIV",
};
const FN_LOG: Trap = Trap {
    rom: "JSR $B9EA",
    jt: "JSR __JT_FN_LOG",
};
const BAD_SUBSCRIPT: Trap = Trap {
    rom: "JMP __BAD_SUBSCRIPT",
    jt: "",
};
const BOUNDS_GUARD: Trap = Trap {
    rom: "JSR __BNDS",
    jt: "",
};
fn check_emit_order(src: &str, first: Trap, second: Trap) {
    for (profile, extraram) in VARIANTS {
        let asm = compile_variant(src, profile, extraram).asm;
        let (f, s) = (first.site(extraram), second.site(extraram));
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
#[test]
fn identity_shapes_still_narrow_a_fractional_operand() {
    check_output(
        "10 READ X\n20 PRINT -1 AND X\n30 PRINT X AND -1\n\
         40 PRINT X OR 0\n50 PRINT 0 OR X\n60 PRINT X\n70 DATA 3.5\n80 END\n",
        &nums(&[" 3", " 3", " 3", " 3", " 3.5"]),
    );
}

/// AYINT floors, so a negative fraction goes *away* from zero: -3.5 narrows to -4, not -3.
#[test]
fn identity_shapes_floor_a_negative_operand() {
    check_output(
        "10 READ X\n20 PRINT -1 AND X\n30 PRINT X OR 0\n40 PRINT X\n50 DATA -3.5\n60 END\n",
        &nums(&["-4", "-4", "-3.5"]),
    );
}
#[test]
fn exor_with_zero_still_narrows() {
    check_output(
        "10 READ X\n20 PRINT EXOR(X,0)\n30 PRINT EXOR(0,X)\n40 DATA 3.5\n50 END\n",
        &nums(&[" 3", " 3"]),
    );
}
#[test]
fn computed_fractional_operand_still_narrows() {
    check_output(
        "10 READ N\n20 X=N/4\n30 PRINT -1 AND X\n40 PRINT X\n50 DATA 14\n60 END\n",
        &nums(&[" 3", " 3.5"]),
    );
}
#[test]
fn literal_operands_fold_with_the_narrowing_applied() {
    check_output(
        "10 PRINT -1.5 AND 3\n20 PRINT 3 AND -1.5\n30 PRINT -1 AND 3.5\n\
         40 PRINT 3.5 OR 0\n50 END\n",
        &nums(&[" 2", " 2", " 3", " 3"]),
    );
}

/// A `%` scalar lives in a signed 16-bit slot, so AYINT is the identity on it and the rewrite
/// is sound.
#[test]
fn int_scalar_operand_still_simplifies() {
    check_simplified("10 X%=PEEK(53280)\n20 PRINT -1 AND X%\n30 END\n", true);
    check_simplified("10 X%=PEEK(53280)\n20 PRINT X% OR 0\n30 END\n", true);
    check_simplified("10 X%=PEEK(53280)\n20 PRINT EXOR(X%,0)\n30 END\n", true);
}

/// `%` array elements are stored the same way.
#[test]
fn int_array_element_operand_still_simplifies() {
    check_simplified(
        "10 DIM Q%(3)\n20 Q%(1)=PEEK(53280)\n30 PRINT -1 AND Q%(1)\n40 END\n",
        true,
    );
}

/// A comparison yields 0 or -1, which is narrow by construction.
#[test]
fn comparison_operand_still_simplifies() {
    check_simplified("10 A=PEEK(53280)\n20 PRINT (A=99) AND -1\n30 END\n", true);
    check_simplified("10 A$=\"X\"\n20 PRINT (A$=\"Y\") OR 0\n30 END\n", true);
}
#[test]
fn already_bitwise_operand_still_simplifies() {
    check_simplified("10 A=PEEK(53280)\n20 PRINT NOT(A) OR 0\n30 END\n", true);
}

/// `PEEK`, `LEN` and `ASC` are byte-valued.
#[test]
fn byte_valued_operands_still_simplify() {
    check_simplified("10 PRINT PEEK(53280) OR 0\n20 END\n", true);
    check_simplified("10 A$=\"ABCD\"\n20 PRINT LEN(A$) AND -1\n30 END\n", true);
    check_simplified("10 INPUT A$\n20 PRINT ASC(A$) OR 0\n30 END\n", true);
}

/// An integer literal in range is narrow; the folder normally gets there first, but the
/// predicate has to agree with it.
#[test]
fn in_range_integer_literal_operand_still_simplifies() {
    check_simplified("10 PRINT -1 AND 30000\n20 END\n", true);
    check_simplified("10 PRINT -32768 OR 0\n20 END\n", true);
}

/// A float scalar can hold a fraction, so the AND has to stay.
#[test]
fn float_scalar_operand_is_not_simplified() {
    check_simplified("10 X=PEEK(53280)/4\n20 PRINT -1 AND X\n30 END\n", false);
    check_simplified("10 X=PEEK(53280)/4\n20 PRINT X OR 0\n30 END\n", false);
    check_simplified("10 X=PEEK(53280)/4\n20 PRINT EXOR(X,0)\n30 END\n", false);
}

/// Float array elements likewise.
#[test]
fn float_array_element_operand_is_not_simplified() {
    check_simplified(
        "10 DIM Q(3)\n20 Q(1)=PEEK(53280)/4\n30 PRINT -1 AND Q(1)\n40 END\n",
        false,
    );
}

/// Arithmetic results are not integral in general.
#[test]
fn arithmetic_operand_is_not_simplified() {
    check_simplified("10 A=PEEK(53280)\n20 PRINT (A/4) OR 0\n30 END\n", false);
}
#[test]
fn out_of_range_literal_operand_is_not_simplified() {
    check_simplified("10 PRINT -1 AND 40000\n20 END\n", false);
    check_simplified("10 PRINT 40000 OR 0\n20 END\n", false);
}

/// `TI` is a reserved pseudo-variable, not a `%` slot: it counts jiffies well past 32767, so it
/// must not be treated as narrow.
#[test]
fn ti_is_not_treated_as_narrow() {
    check_simplified("10 PRINT TI OR 0\n20 END\n", false);
}

/// The `IF <a> AND <b> THEN` fast path.
#[test]
fn if_and_evaluates_the_left_operand_first() {
    check_emit_order(
        "10 IF (1/0)=1 AND LOG(0)=1 THEN PRINT \"Y\"\n20 PRINT \"Z\"\n30 END\n",
        FDIV,
        FN_LOG,
    );
}

/// The mirror image: swapping the operands must swap which trap fires.
#[test]
fn if_and_with_operands_swapped_swaps_the_order() {
    check_emit_order(
        "10 IF LOG(0)=1 AND (1/0)=1 THEN PRINT \"Y\"\n20 PRINT \"Z\"\n30 END\n",
        FN_LOG,
        FDIV,
    );
}
#[test]
fn if_or_evaluates_the_left_operand_first() {
    check_emit_order(
        "10 IF (1/0)=1 OR LOG(0)=1 THEN PRINT \"Y\"\n20 PRINT \"Z\"\n30 END\n",
        FDIV,
        FN_LOG,
    );
}
#[test]
fn value_context_and_evaluates_the_left_operand_first() {
    check_emit_order(
        "10 A=((1/0)=1) AND (LOG(0)=1)\n20 PRINT A\n30 END\n",
        FDIV,
        FN_LOG,
    );
}

#[test]
fn value_context_with_operands_swapped_swaps_the_order() {
    check_emit_order(
        "10 A=((LOG(0)=1) AND ((1/0)=1))\n20 PRINT A\n30 END\n",
        FN_LOG,
        FDIV,
    );
}

/// An array bounds check is a trap too.
#[test]
fn left_operand_bounds_check_runs_before_the_right_operand() {
    check_emit_order(
        "5 READ I\n10 DIM Q(2)\n20 IF Q(I)=1 AND LOG(0)=1 THEN PRINT \"Y\"\n\
         30 PRINT \"Z\"\n40 DATA 9\n50 END\n",
        BOUNDS_GUARD,
        FN_LOG,
    );
}
#[test]
fn a_provably_bad_subscript_deletes_the_right_operand_outright() {
    let src = "10 DIM Q(2)\n20 IF Q(9)=1 AND LOG(0)=1 THEN PRINT \"Y\"\n\
               30 PRINT \"Z\"\n40 END\n";
    for (profile, extraram) in VARIANTS {
        let asm = compile_variant(src, profile, extraram).asm;
        let lines: Vec<&str> =
            asm.lines().map(|l| l.split(';').next().unwrap().trim()).filter(|l| !l.is_empty()).collect();
        let runs_in = lines.iter().position(|l| *l == "__BAD_SUBSCRIPT:").is_some_and(|i| {
            i > 0 && !["RTS", "RTI"].contains(&lines[i - 1]) && !lines[i - 1].starts_with("JMP ")
        });
        assert!(
            asm.contains(BAD_SUBSCRIPT.site(extraram)) || runs_in,
            "{profile:?} extraram={extraram}: the trap itself must stay:\n{asm}"
        );
        assert!(
            !asm.contains(FN_LOG.site(extraram)),
            "{profile:?} extraram={extraram}: LOG is behind an unconditional \
             trap and must not be emitted:\n{asm}"
        );
    }
}
#[test]
fn nested_and_keeps_both_stashes() {
    check_output(
        "10 READ A,B,C\n20 PRINT (A AND B) AND C\n30 PRINT A AND (B AND C)\n\
         40 PRINT A AND B AND C\n50 DATA 15,12,10\n60 END\n",
        &nums(&[" 8", " 8", " 8"]),
    );
}

/// `OR` nesting, and a mixed `AND`/`OR` tree so the two mnemonics share the depth counter. 1 OR
/// 2 OR 4 is 7; (1 OR 2) AND 6 is 2.
#[test]
fn nested_or_and_mixed_trees_keep_both_stashes() {
    check_output(
        "10 READ A,B,C\n20 PRINT (A OR B) OR C\n30 PRINT A OR (B OR C)\n\
         40 PRINT (A OR B) AND 6\n50 PRINT A OR (B AND C)\n\
         60 DATA 1,2,4\n70 END\n",
        &nums(&[" 7", " 7", " 2", " 1"]),
    );
}

/// Three levels deep, to prove the depth counter is a counter and not a single spare slot.
#[test]
fn three_level_nesting_keeps_every_stash() {
    check_output(
        "10 READ A,B,C,D\n20 PRINT A AND (B AND (C AND D))\n\
         30 PRINT 255 AND (A AND (B AND (C AND D)))\n\
         40 DATA 15,14,12,8\n50 END\n",
        &nums(&[" 8", " 8"]),
    );
}

/// Nesting inside an `IF` condition takes the separate fast path in
/// `emit_branch_skip_on_false`, which keeps its own copy of the stash sequence.
#[test]
fn nested_bitwise_inside_an_if_condition() {
    check_output(
        "10 READ A,B,C\n20 IF A AND (B AND C) THEN PRINT \"Y\"\n\
         30 IF A AND (B AND 0) THEN PRINT \"N\"\n40 PRINT \"Z\"\n\
         50 DATA 15,12,10\n60 END\n",
        "Y\nZ\n",
    );
}
