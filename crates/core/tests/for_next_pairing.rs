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

/// Run without requiring a clean exit: `(output, clean_exit)`.
fn run_raw(src: &str, profile: Profile, extraram: bool) -> (String, bool) {
    let (bytes, _) = build(src, profile, extraram);
    let r = emu64::run_prg_to_end(&bytes, 4_000_000).expect("runs");
    (common::petscii_to_string(&r.output), r.clean_exit)
}

/// Assert `src` stops (does not run away) having printed exactly `expected`, on all six
/// variants.
fn check_stops_with(src: &str, expected: &str) {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        for extraram in [false, true] {
            let (got, clean) = run_raw(src, profile, extraram);
            assert_eq!(
                got, expected,
                "{profile:?} extraram={extraram} output mismatch for:\n{src}"
            );
            assert!(
                !clean,
                "{profile:?} extraram={extraram}: ?NEXT WITHOUT FOR must abort the \
                 program, not let it end normally:\n{src}"
            );
        }
    }
}

/// Default-profile assembly, for the structural assertions.
fn asm_of(src: &str) -> String {
    build(src, Profile::Default, false).1
}

/// The headline case.
#[test]
fn for_under_then_owns_the_following_next() {
    let src = "10 FOR I=1 TO 4\n\
               20 IF I=2 THEN FOR K=1 TO 3\n\
               30 NEXT\n\
               40 PRINT I;K\n";
    check_all_variants(src, " 2  4 \n");
}

#[test]
fn for_under_then_body_checksum_matches_basic() {
    let src = "5 N=0:S=0\n\
               10 FOR I=1 TO 4\n\
               20 IF I=2 THEN FOR K=1 TO 3\n\
               30 N=N+1:S=S+I*10+K:PRINT\"T\";I;K;N;S:NEXT\n\
               40 PRINT\"E\";I;K;N;S\n";
    check_all_variants(
        src,
        "T 1  0  1  10 \n\
         T 2  1  2  31 \n\
         T 2  2  3  53 \n\
         T 2  3  4  76 \n\
         E 2  4  4  76 \n",
    );
}

#[test]
fn for_under_never_taken_then_leaves_the_outer_loop_alone() {
    let src = "10 FOR I=1 TO 4\n\
               20 IF I=9 THEN FOR K=1 TO 3\n\
               30 NEXT\n\
               40 PRINT I;K\n";
    check_all_variants(src, " 5  0 \n");
}

/// Condition always true: the K loop is opened on the very first pass, so I never advances past
/// 1.
#[test]
fn for_under_always_taken_then_captures_the_next_immediately() {
    let src = "10 FOR I=1 TO 4\n\
               20 IF I>0 THEN FOR K=1 TO 3\n\
               30 NEXT\n\
               40 PRINT I;K\n";
    check_all_variants(src, " 1  4 \n");
}

#[test]
fn for_under_then_at_top_level_taken() {
    let src = "10 C=0:K=0\n\
               20 IF K=0 THEN FOR I=1 TO 3\n\
               30 C=C+1\n\
               40 NEXT\n\
               50 PRINT C;I\n";
    check_all_variants(src, " 3  4 \n");
}

#[test]
fn for_under_skipped_then_leaves_a_next_without_for() {
    let src = "10 C=0:K=1\n\
               20 IF K=0 THEN FOR I=1 TO 3\n\
               30 C=C+1:PRINT\"C\";C\n\
               40 NEXT\n\
               50 PRINT C;I\n";
    check_stops_with(src, "C 1 \n");
}

#[test]
fn conditional_frame_closes_and_abandons_the_outer_loop() {
    let src = "10 S=0\n\
               20 FOR I=1 TO 3\n\
               30 IF I=1 THEN FOR K=1 TO 2\n\
               40 S=S+1\n\
               50 NEXT\n\
               60 PRINT S;I;K\n";
    check_all_variants(src, " 2  1  3 \n");
}

#[test]
fn one_next_binds_to_different_loops_on_different_iterations() {
    let src = "10 S=0\n\
               20 FOR I=1 TO 5\n\
               30 IF I=3 THEN FOR K=1 TO 2\n\
               40 S=S+1\n\
               50 NEXT\n\
               60 PRINT S;I;K\n";
    check_all_variants(src, " 4  3  3 \n");
}

/// Two conditional frames stacked on one iteration.
#[test]
fn two_conditional_frames_stack() {
    let src = "10 S=0\n\
               20 FOR I=1 TO 4\n\
               30 IF I=2 THEN FOR K=1 TO 2\n\
               35 IF I=2 THEN FOR L=1 TO 2\n\
               40 S=S+1\n\
               50 NEXT\n\
               60 PRINT S;I;K;L\n";
    check_all_variants(src, " 3  2  1  3 \n");
}

#[test]
fn for_under_else_arm_owns_the_following_next() {
    let src = "10 FOR I=1 TO 4\n\
               20 IF I<>2 THEN A=A+1:GOTO 30\n\
               25 FOR K=1 TO 3\n\
               30 NEXT\n\
               40 PRINT I;K;A\n";
    check_all_variants(src, " 2  4  1 \n");
}

#[test]
fn conditional_frame_accumulator_matches_basic() {
    let src = "10 S=0\n\
               20 FOR I=1 TO 4\n\
               30 IF I=2 THEN FOR K=1 TO 3\n\
               40 S=S+I*100+K\n\
               50 NEXT\n\
               60 PRINT\"S\";S\n\
               70 PRINT\"I\";I;\"K\";K\n";
    check_all_variants(src, "S 706 \nI 2 K 4 \n");
}

#[test]
fn conditional_for_closed_by_a_goto_reached_orphan_next() {
    let src = "5 P=0\n\
               10 GOTO 30\n\
               20 NEXT:GOTO 50\n\
               30 FOR I=1 TO 3\n\
               40 IF I=2 THEN FOR K=1 TO 2\n\
               50 P=P+1:PRINT\"P\";P;I;K\n\
               60 GOTO 20\n";
    check_stops_with(
        src,
        "P 1  1  0 \n\
         P 2  2  1 \n\
         P 3  2  2 \n\
         P 4  2  3 \n\
         P 5  3  3 \n\
         P 6  4  3 \n",
    );
}

#[test]
fn goto_reached_orphan_next_without_a_conditional_frame() {
    let src = "5 P=0\n\
               10 GOTO 30\n\
               20 NEXT:GOTO 50\n\
               30 FOR I=1 TO 3\n\
               40 IF I=9 THEN FOR K=1 TO 2\n\
               50 P=P+1:PRINT\"P\";P;I;K\n\
               60 GOTO 20\n";
    check_stops_with(
        src,
        "P 1  1  0 \n\
         P 2  2  0 \n\
         P 3  3  0 \n\
         P 4  4  0 \n",
    );
}

#[test]
fn conditional_for_gets_a_dispatch_handler() {
    let asm = asm_of(
        "5 P=0\n\
         10 GOTO 30\n\
         20 NEXT:GOTO 50\n\
         30 FOR I=1 TO 3\n\
         40 IF I=2 THEN FOR K=1 TO 2\n\
         50 P=P+1:PRINT\"P\";P;I;K\n\
         60 GOTO 20\n",
    );
    assert!(
        asm.contains("__FOR_NEXT_HANDLER_1") && asm.contains("__FOR_NEXT_HANDLER_2"),
        "both the enclosing loop and the conditional one need a runtime \
         handler, or the dispatcher resolves the conditional loop's NEXT \
         against its enclosing frame:\n{asm}"
    );
}

/// FNDFOR: `20 FOR I` removes the record `10 FOR I` pushed, so after line 30 closes the inner
/// loop the stack is empty and line 40 is a real `?NEXT WITHOUT FOR ERROR IN 40`.
#[test]
fn refor_same_var_then_extra_next_is_an_error() {
    let src = "10 FOR I=1 TO 3\n\
               20 FOR I=1 TO 2\n\
               30 PRINT\"X\";I\n\
               40 NEXT\n\
               50 NEXT\n\
               60 PRINT\"E\";I\n";
    check_stops_with(src, "X 1 \nX 2 \n");
}

#[test]
fn refor_same_var_bare() {
    let src = "10 FOR I=1 TO 3\n\
               20 FOR I=1 TO 2\n\
               30 NEXT\n\
               40 NEXT\n\
               50 PRINT\"E\";I\n";
    check_stops_with(src, "");
}

/// A plain unmatched `NEXT` with no loop anywhere.
#[test]
fn bare_next_with_no_for_at_all_is_an_error() {
    let src = "10 PRINT\"A\"\n\
               20 NEXT\n\
               30 PRINT\"B\"\n";
    check_stops_with(src, "A\n");
}

/// One extra `NEXT` after a well-formed loop.
#[test]
fn extra_next_after_a_closed_loop_is_an_error() {
    let src = "10 FOR I=1 TO 2\n\
               20 NEXT\n\
               30 PRINT\"D\";I\n\
               40 NEXT\n\
               50 PRINT\"E\"\n";
    check_stops_with(src, "D 3 \n");
}

/// Structural: the underflowing `NEXT` must route to the trap, and must not re-emit a retired
/// loop's tail.
#[test]
fn unmatched_next_emits_the_trap_not_a_resurrected_tail() {
    let asm = asm_of(
        "10 FOR I=1 TO 3\n\
         20 FOR I=1 TO 2\n\
         30 NEXT\n\
         40 NEXT\n\
         50 PRINT\"E\";I\n",
    );
    assert!(
        asm.contains("__NEXT_NO_FOR"),
        "the second NEXT underflows the compile-time stack after FNDFOR \
         discarded the outer frame — it must become ?NEXT WITHOUT FOR:\n{asm}"
    );
}

#[test]
fn shared_tail_next_idiom_still_pairs() {
    let src = "10 FOR I=1 TO 5\n\
               20 IF I<3 THEN NEXT\n\
               30 PRINT\"D\";I\n\
               40 END\n";
    check_all_variants(src, "D 3 \n");
}

/// An ordinary static nest must stay entirely inline: no runtime FOR stack, no dispatcher, no
/// handler table.
#[test]
fn ordinary_nest_stays_on_the_compile_time_stack() {
    let asm = asm_of(
        "10 FOR I=1 TO 10\n\
         20 FOR J=1 TO 10\n\
         30 A=A+1\n\
         40 NEXT J\n\
         50 NEXT I\n\
         60 PRINT A\n\
         70 END\n",
    );
    assert!(
        !asm.contains("__FOR_STACK"),
        "a statically paired nest must not touch the runtime FOR stack:\n{asm}"
    );
    assert!(
        !asm.contains("__FOR_HANDLER_TABLE"),
        "a statically paired nest needs no dispatch table:\n{asm}"
    );
    assert!(
        !asm.contains("__NEXT_NO_FOR"),
        "a statically paired nest cannot reach ?NEXT WITHOUT FOR:\n{asm}"
    );
}

#[test]
fn if_inside_a_loop_body_does_not_force_the_runtime_stack() {
    let asm = asm_of(
        "10 FOR I=1 TO 10\n\
         20 IF I=5 THEN A=A+1\n\
         30 IF I>7 THEN PRINT\"H\";I\n\
         40 NEXT I\n\
         50 PRINT A\n\
         60 END\n",
    );
    assert!(
        !asm.contains("__FOR_STACK"),
        "an IF with no FOR in it must not push the loop onto the runtime \
         stack:\n{asm}"
    );
}

/// A conditional FOR that the branch also *closes* stays static: the frame does not survive the
/// branch, so both models agree.
#[test]
fn for_and_next_both_inside_one_then_stay_static() {
    let src = "10 FOR I=1 TO 3\n\
               20 IF I=2 THEN FOR K=1 TO 3:S=S+K:NEXT K\n\
               30 NEXT I\n\
               40 PRINT S;I;K\n\
               50 END\n";
    let asm = asm_of(src);
    assert!(
        !asm.contains("__FOR_STACK"),
        "a FOR opened and closed inside one THEN body leaves the stack \
         balanced and must stay on the compile-time path:\n{asm}"
    );
    check_all_variants(src, " 6  4  4 \n");
}

#[test]
fn conditional_frame_still_uses_the_byte_counter_specialisation() {
    let asm = asm_of(
        "10 FOR I=1 TO 4\n\
         20 IF I=2 THEN FOR K=1 TO 3\n\
         30 NEXT\n\
         40 PRINT I;K\n",
    );
    assert!(
        !asm.contains("__FOR_DIFF_LO") || asm.contains("INC"),
        "the conditional loop must still use an integer counter, not FAC \
         arithmetic:\n{asm}"
    );
    assert!(
        !asm.contains("JSR $B8"),
        "no float add/compare should be needed for two byte-range \
         loops:\n{asm}"
    );
}
