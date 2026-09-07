

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

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

fn asm_of(src: &str) -> String {
    build(src, Profile::Default, false).1
}
fn check_all_variants(src: &str, expected: &str) {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        for extraram in [false, true] {
            let (bytes, _) = build(src, profile, extraram);
            let got = common::run_clean(&bytes).unwrap_or_else(|| {
                panic!("runs to a clean exit ({profile:?}, extraram={extraram}):\n{src}")
            });
            assert_eq!(
                got, expected,
                "{profile:?} extraram={extraram} mismatch for:\n{src}"
            );
        }
    }
}
fn check_stops_with(src: &str, expected: &str) {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        for extraram in [false, true] {
            let (bytes, _) = build(src, profile, extraram);
            let r = emu64::run_prg_to_end(&bytes, 6_000_000).expect("runs");
            let got = common::petscii_to_string(&r.output);
            assert_eq!(
                got, expected,
                "{profile:?} extraram={extraram} output mismatch for:\n{src}"
            );
            assert!(
                !r.clean_exit,
                "{profile:?} extraram={extraram}: the BASIC error must abort the \
                 program, not let it end normally:\n{src}"
            );
        }
    }
}
#[test]
fn a_branch_over_the_next_that_would_have_popped_the_survivor() {
    check_all_variants(
        "10 FOR I=1 TO 2\n\
         20 FOR I=1 TO 2\n\
         30 PRINT\"a\";I\n\
         40 IF I=2 THEN 70\n\
         50 NEXT\n\
         70 NEXT I\n\
         80 PRINT\"c\";I\n",
        "A 1 \nA 2 \nC 3 \n",
    );
}

/// The recorded shape, with `ON I GOTO` in place of the `IF`.
#[test]
fn the_same_shape_reached_through_on_goto() {
    check_all_variants(
        "10 FOR I=1 TO 2\n\
         20 FOR I=1 TO 2\n\
         30 PRINT\"a\";I\n\
         40 ON I GOTO 50,70\n\
         50 NEXT\n\
         70 NEXT I\n\
         80 PRINT\"c\";I\n",
        "A 1 \nA 2 \nC 3 \n",
    );
}
#[test]
fn the_named_next_now_reaches_the_dispatcher_the_bare_one_always_did() {
    let bare = "10 FOR I=1 TO 2\n\
                20 FOR I=1 TO 2\n\
                30 PRINT\"a\";I\n\
                40 IF I=2 THEN 70\n\
                50 NEXT\n\
                70 NEXT\n\
                80 PRINT\"c\";I\n";
    let named = bare.replace("70 NEXT\n", "70 NEXT I\n");
    check_all_variants(bare, "A 1 \nA 2 \nC 3 \n");
    for (what, src) in [("bare", bare), ("named", named.as_str())] {
        let asm = asm_of(src);
        assert!(
            asm.contains("__FOR_STACK"),
            "the {what} NEXT must resolve against the runtime FOR stack:\n{asm}"
        );
        assert!(
            asm.contains("__FOR_RT_SRCH_") || asm.contains("__FOR_DISP_VEC"),
            "…through the dispatcher, not an inline tail ({what}):\n{asm}"
        );
    }
}

// The reopened frame is the only one that goes.

/// The minimal case.
#[test]
fn reopening_the_inner_loop_leaves_the_outer_one_closeable() {
    let src = "10 FOR I=1 TO 2\n\
               20 FOR J=1 TO 2\n\
               30 FOR J=1 TO 1\n\
               40 NEXT\n\
               50 NEXT\n\
               60 PRINT I;J\n";
    check_all_variants(src, " 3  2 \n");
}
#[test]
fn reopening_the_inner_loop_keeps_both_tails_inline() {
    let asm = asm_of(
        "10 FOR I=1 TO 2\n\
         20 FOR J=1 TO 2\n\
         30 FOR J=1 TO 1\n\
         40 NEXT\n\
         50 NEXT\n\
         60 PRINT I;J\n",
    );
    assert_eq!(
        asm.matches("JMP __FOR_TOP").count(),
        2,
        "the reopened inner loop and the outer loop each keep a \
         compile-time back edge:\n{asm}"
    );
    assert!(
        !asm.contains("__FOR_STACK"),
        "nothing here needs the runtime FOR stack:\n{asm}"
    );
}

/// Three deep, reopening the *middle* variable.
#[test]
fn reopening_a_middle_frame_discards_what_was_opened_inside_it() {
    check_all_variants(
        "10 FOR I=1 TO 2\n\
         20 FOR J=1 TO 2\n\
         30 FOR K=1 TO 2\n\
         40 FOR J=1 TO 1\n\
         50 PRINT\"p\";I;J;K\n\
         60 NEXT\n\
         70 PRINT\"q\";I;J;K\n\
         80 NEXT\n\
         90 PRINT\"r\";I;J;K\n",
        "P 1  1  1 \nQ 1  2  1 \nP 2  1  1 \nQ 2  2  1 \nR 3  2  1 \n",
    );
}

// The discard has to come off the *runtime* stack too.

/// `30 FOR J` discards the frame `20 FOR J` pushed.
#[test]
fn a_discarded_frame_releases_its_runtime_slot() {
    check_stops_with(
        "10 FOR I=1 TO 2\n\
         20 FOR J=1 TO 2\n\
         30 FOR J=1 TO 2\n\
         40 PRINT\"a\";I;J;J\n\
         50 FOR J=1 TO 1\n\
         60 NEXT\n\
         70 NEXT I\n\
         80 NEXT\n\
         90 PRINT\"z\";I;J;J\n\
         95 END\n\
         200 PRINT\"s\";I:RETURN\n",
        "A 1  1  1 \nA 2  1  1 \n",
    );
}
#[test]
fn a_discarded_frame_releases_its_runtime_slot_with_a_third_variable() {
    check_stops_with(
        "10 FOR I=1 TO 2\n\
         20 FOR J=1 TO 2\n\
         30 FOR K=1 TO 2\n\
         40 PRINT\"a\";I;J;K\n\
         50 FOR J=1 TO 1\n\
         60 NEXT\n\
         70 NEXT I\n\
         80 NEXT\n\
         90 PRINT\"z\";I;J;K\n\
         95 END\n\
         200 PRINT\"s\";I:RETURN\n",
        "A 1  1  1 \nA 2  1  1 \n",
    );
}
#[test]
fn a_discarded_frame_releases_its_runtime_slot_across_a_goto() {
    check_stops_with(
        "10 FOR I=1 TO 2\n\
         20 FOR J=1 TO 2\n\
         30 FOR J=1 TO 2\n\
         40 PRINT\"a\";I;J;J\n\
         50 GOTO 70\n\
         60 NEXT\n\
         70 NEXT I\n\
         80 NEXT\n\
         90 PRINT\"z\";I;J;J\n\
         95 END\n\
         200 PRINT\"s\";I:RETURN\n",
        "A 1  1  1 \nA 2  1  1 \n",
    );
}

/// `50 NEXT I` walks past the `J` frame `40 FOR J` installed over the discarded one.
#[test]
fn next_var_past_a_reopened_frame_releases_every_slot_it_walks() {
    check_stops_with(
        "10 FOR I=1 TO 2\n\
         20 FOR J=1 TO 2\n\
         30 PRINT\"a\";I;J\n\
         40 FOR J=1 TO 1\n\
         50 NEXT I\n\
         60 PRINT\"b\";I;J\n\
         70 NEXT\n\
         80 PRINT\"c\";I;J\n\
         90 END\n\
         200 PRINT\"s\";I;J\n\
         210 RETURN\n",
        "A 1  1 \nA 2  1 \nB 3  1 \n",
    );
}

// A `NEXT V` that walks past a *conditionally* opened frame.

/// The only non-terminating program in the family.
#[test]
fn next_var_past_a_conditional_frame_terminates() {
    check_stops_with(
        "10 FOR I=1 TO 2\n\
         20 FOR J=1 TO 2\n\
         30 PRINT\"a\";I;J\n\
         40 IF I=1 THEN FOR J=5 TO 6\n\
         50 NEXT I\n\
         60 PRINT\"b\";I;J\n\
         70 NEXT\n\
         80 PRINT\"c\";I;J\n\
         90 END\n\
         200 PRINT\"s\";I;J\n\
         210 RETURN\n",
        "A 1  1 \nA 2  1 \nB 3  1 \n",
    );
}
#[test]
fn next_var_past_a_conditional_frame_dispatches() {
    let src = "10 FOR I=1 TO 2\n\
               20 IF I=1 THEN FOR J=1 TO 2\n\
               30 NEXT I\n\
               40 PRINT I\n";
    check_all_variants(src, " 3 \n");
    assert!(
        asm_of(src).contains("__FOR_STACK"),
        "a frame that exists only on the taken path has to be \
         represented at run time"
    );
}
#[test]
fn next_var_past_an_unconditional_frame_keeps_the_static_fast_path() {
    let src = "10 FOR I=1 TO 2\n\
               20 FOR J=1 TO 2\n\
               30 NEXT I\n\
               40 PRINT I\n";
    check_all_variants(src, " 3 \n");
    let asm = asm_of(src);
    assert!(
        !asm.contains("__FOR_STACK"),
        "no runtime FOR stack for a wholly static nest:\n{asm}"
    );
    assert_eq!(
        asm.matches("JMP __FOR_TOP").count(),
        1,
        "`NEXT I` keeps its inline back edge:\n{asm}"
    );
}

// Cost counterweights for the runtime-slot release.

#[test]
fn a_reopen_with_no_framed_loops_stays_free() {
    let src = "10 FOR I=0 TO 3\n\
               20 FOR I=1 TO 2\n\
               30 PRINT I;\n\
               40 NEXT:NEXT\n";
    check_stops_with(src, " 1  2 ");
    let asm = asm_of(src);
    assert!(
        !asm.contains("__FOR_STACK"),
        "a reopen is not by itself a reason to pay for the runtime \
         stack:\n{asm}"
    );
    assert!(
        !asm.contains("DEC __FOR_STACK_PTR"),
        "no slot was allocated, so none is released:\n{asm}"
    );
}
#[test]
fn a_searching_push_does_its_own_truncation() {
    let src = "10 GOTO 40\n\
               20 NEXT:GOTO 90\n\
               40 FOR I=1 TO 2\n\
               50 FOR I=1 TO 3\n\
               60 PRINT\"a\";I\n\
               70 GOTO 20\n\
               90 PRINT\"z\";I\n";
    check_all_variants(src, "A 1 \nA 2 \nA 3 \nZ 4 \n");
    let asm = asm_of(src);
    assert!(
        asm.contains("__FOR_PUSH_SRCH_"),
        "the reopened loop's push has to run the FNDFOR search:\n{asm}"
    );
    // The searching push is `LDX __FOR_STACK_PTR / BEQ __FOR_PUSH_n`; a compile-time `DEC` for
    // the same discard would sit immediately in front of it.
    for (i, line) in asm.lines().enumerate() {
        if line.trim_start().starts_with("LDX __FOR_STACK_PTR")
            && asm
                .lines()
                .nth(i + 1)
                .is_some_and(|n| n.trim_start().starts_with("BEQ __FOR_PUSH_"))
        {
            let prev = asm.lines().nth(i.wrapping_sub(1)).unwrap_or("");
            assert!(
                !prev.trim_start().starts_with("DEC __FOR_STACK_PTR"),
                "the search already truncates; the extra DEC would \
                 double-count:\n{asm}"
            );
        }
    }
}
