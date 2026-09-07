mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

/// Compile at `profile` (with extraram forced on or off) and return the image plus its assembly
/// listing.
fn build(src: &str, profile: Profile, extraram: bool) -> (Vec<u8>, String) {
    let prg = tokenize_program(src).expect("tokenizes");
    let opts = CompileOptions {
        profile,
        extraram,
        force_extraram_off: !extraram,
        ..CompileOptions::default()
    };
    let c = compile_with_options(&prg, opts)
        .unwrap_or_else(|e| panic!("compiles ({profile:?}, extraram={extraram}): {e}"));
    (c.prg_bytes, c.asm)
}

fn run_variant(src: &str, profile: Profile, extraram: bool) -> String {
    let (bytes, _) = build(src, profile, extraram);
    common::run_clean(&bytes).unwrap_or_else(|| {
        panic!("runs to a clean exit ({profile:?}, extraram={extraram}):\n{src}")
    })
}

fn check_all_variants(src: &str, expected: &str) {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        for extraram in [false, true] {
            let got = run_variant(src, profile, extraram);
            assert_eq!(
                got, expected,
                "{profile:?} extraram={extraram} mismatch for:\n{src}"
            );
        }
    }
}

/// Default-profile assembly, for the structural assertions.
fn asm_of(src: &str) -> String {
    build(src, Profile::Default, false).1
}

/// The decisive case, and the one that needs no emulator to condemn: two reads of `J` in
/// adjacent statements, with no assignment between them, disagreed.
#[test]
fn adjacent_reads_of_one_variable_agree() {
    let src = "10 FOR J=7 TO 9\n\
               20 GOTO 100\n\
               30 NEXT J\n\
               100 J=3\n\
               110 PRINT \"A\";J\n\
               115 PRINT \"B\";J+0\n\
               116 IF J=3 THEN PRINT \"EQ3\"\n\
               117 IF J=7 THEN PRINT \"EQ7\"\n\
               120 END\n";
    check_all_variants(src, "A 3 \nB 3 \nEQ3\n");
}

/// Write from outside the loop's textual body, then branch back in.
#[test]
fn write_from_outside_the_body_moves_the_counter() {
    let src = "10 FOR I=1 TO 6\n\
               20 GOTO 100\n\
               30 PRINT I;\n\
               40 NEXT I\n\
               50 PRINT \"F\";I\n\
               60 END\n\
               100 I=I+2\n\
               110 GOTO 30\n";
    check_all_variants(src, " 3  6 F 7 \n");
}

#[test]
fn write_from_outside_the_body_moves_a_16bit_counter() {
    let src = "10 FOR I=1 TO 100\n\
               20 GOTO 100\n\
               30 NEXT I\n\
               40 PRINT \"E\";I\n\
               50 END\n\
               100 I=I+50\n\
               110 GOTO 30\n";
    check_all_variants(src, "E 103 \n");
}

/// Read from outside the textual body, reached by a branch out of it and back.
#[test]
fn read_from_outside_the_body_sees_the_live_counter() {
    let src = "10 FOR I=0 TO 5\n\
               20 IF I=2 THEN 100\n\
               30 PRINT I;\n\
               40 NEXT I\n\
               50 PRINT \"=\";I\n\
               60 END\n\
               100 I=4\n\
               105 PRINT \"W\";I;\n\
               110 GOTO 30\n";
    check_all_variants(src, " 0  1 W 4  4  5 = 6 \n");
}

/// A `NEXT I` that closes the outer loop abandons the inner `J` frame.
#[test]
fn inner_loop_abandoned_by_outer_next_keeps_its_start_value() {
    let src = "10 FOR I=1 TO 2\n\
               20 FOR J=5 TO 9\n\
               30 NEXT I\n\
               40 PRINT \"J\";J\n\
               50 END\n";
    check_all_variants(src, "J 5 \n");
}

#[test]
fn abandoned_counter_cascades_into_a_later_loop_bound() {
    let src = "10 FOR C=5 TO 9\n\
               20 FOR A=1 TO 2\n\
               30 NEXT A\n\
               40 FOR B=1 TO C\n\
               50 PRINT \"B\";B;\n\
               60 NEXT B\n\
               70 PRINT \"C=\";C\n\
               80 END\n";
    check_all_variants(src, "B 1 B 2 B 3 B 4 B 5 C= 5 \n");
}

/// A negative-range counter.
#[test]
fn negative_for_range_compares_as_signed() {
    let src = "10 FOR I=-2 TO 2\n\
               20 IF I<0 THEN PRINT I;\"NEG\":GOTO 50\n\
               30 IF I=0 THEN PRINT I;\"ZERO\":GOTO 50\n\
               40 PRINT I;\"POS\"\n\
               50 NEXT\n\
               60 END\n";
    check_all_variants(src, "-2 NEG\n-1 NEG\n 0 ZERO\n 1 POS\n 2 POS\n");
}

/// `NEXT A` abandons the open `C` frame, so no exit block is ever emitted for it.
#[test]
fn abandoned_for_still_assigns_its_start_value() {
    let src = "10 FOR C=5 TO 9\n\
               20 FOR A=1 TO 2\n\
               30 NEXT A\n\
               40 PRINT \"C=\";C\n\
               50 END\n";
    check_all_variants(src, "C= 5 \n");
}

/// The dead-store hazard: the passes are entitled to delete `C=99` because the FOR header
/// redefines C.
#[test]
fn for_header_store_keeps_the_preceding_store_deletable_and_correct() {
    let src = "5 C=99\n\
               10 FOR C=5 TO 9\n\
               20 GOTO 40\n\
               30 NEXT C\n\
               40 PRINT \"C=\";C\n\
               50 END\n";
    check_all_variants(src, "C= 5 \n");
}

#[test]
fn for_left_open_at_program_end_still_assigns() {
    let src = "10 FOR C=5 TO 9\n\
               20 PRINT \"C=\";C\n";
    check_all_variants(src, "C= 5 \n");
}

#[test]
fn byte_range_loop_still_uses_the_one_byte_specialisation() {
    let asm = asm_of("10 FOR I=0 TO 200\n20 POKE 1024+I,65\n30 NEXT I\n40 END\n");
    assert!(
        !asm.contains("FU_") && !asm.contains("FI_"),
        "an Integer-kind counter must alias its variable, not allocate a \
         private slot:\n{asm}"
    );
    assert!(
        asm.contains("INC VI_I") || asm.contains("INX"),
        "the one-byte FOR specialisation must still fire (INC/INX on the \
         counter):\n{asm}"
    );
    assert!(
        !asm.contains("FE_0"),
        "no float end-value slot — the loop must not fall back to \
         float-FOR:\n{asm}"
    );
}

#[test]
fn word_range_loop_still_uses_the_int_specialisation() {
    let asm = asm_of("10 FOR I=1 TO 1000\n20 A=A+1\n30 NEXT I\n40 PRINT A\n50 END\n");
    assert!(
        !asm.contains("FI_") && !asm.contains("FU_"),
        "an Integer-kind counter must alias its variable:\n{asm}"
    );
    assert!(
        asm.contains("__FOR_DIFF_LO"),
        "the int-FOR signed compare must still fire:\n{asm}"
    );
    assert!(
        asm.contains("INC VI_I"),
        "the int-FOR increment must still be an inline INC chain:\n{asm}"
    );
}

#[test]
fn index_register_promotion_still_fires() {
    let asm = asm_of("10 F=1\n20 FOR I=1 TO 100\n30 NEXT I\n40 PRINT F\n50 END\n");
    assert!(
        asm.contains("loop index -> X"),
        "the peephole must still promote the loop counter to X:\n{asm}"
    );
}

#[test]
fn float_kind_counter_keeps_its_private_int_slot() {
    // `I` is used as both a scalar and an array subscript name, which disqualifies it from
    // int-promotion, so it keeps 5-byte MFLPT storage that no raw counter can alias.
    let asm = asm_of("10 DIM I(3)\n20 FOR I=1 TO 10\n30 I(1)=1\n40 NEXT I\n50 PRINT I\n60 END\n");
    assert!(
        asm.contains("V_I"),
        "expected a Float-kind loop variable in this shape:\n{asm}"
    );
    assert!(
        asm.contains("FU_") || asm.contains("FI_"),
        "a Float-kind counter has no aliasable slot, so the int/u8 \
         specialisation must still allocate one rather than dropping to \
         FAC arithmetic:\n{asm}"
    );
}

/// Nested loops on distinct Integer counters must both alias, and each counter must be a
/// distinct address.
#[test]
fn nested_loops_alias_distinct_variables() {
    let src = "10 FOR I=1 TO 3\n\
               20 FOR J=1 TO 3\n\
               30 PRINT I;J;\n\
               40 NEXT J\n\
               50 NEXT I\n\
               60 PRINT \"F\";I;J\n\
               70 END\n";
    let asm = asm_of(src);
    assert!(
        !asm.contains("FU_") && !asm.contains("FI_"),
        "both Integer counters must alias their variables:\n{asm}"
    );
    check_all_variants(
        src,
        " 1  1  1  2  1  3  2  1  2  2  2  3  3  1  3  2  3  3 F 4  4 \n",
    );
}

#[test]
fn sequential_loops_on_one_variable_share_one_slot() {
    let src = "10 FOR I=1 TO 3\n\
               20 NEXT I\n\
               30 PRINT \"A\";I\n\
               40 FOR I=1 TO 5\n\
               50 NEXT I\n\
               60 PRINT \"B\";I\n\
               70 END\n";
    let asm = asm_of(src);
    assert!(
        !asm.contains("FU_") && !asm.contains("FI_"),
        "both loops on I must use I's own slot:\n{asm}"
    );
    check_all_variants(src, "A 4 \nB 6 \n");
}
