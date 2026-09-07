

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

/// `check_stops_with`, plus the exact `?<MSG> ERROR IN <line>` banner the ROM printed.
fn check_stops_with_banner(src: &str, expected: &str, banner: &str) {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        for extraram in [false, true] {
            let (bytes, _) = build(src, profile, extraram);
            let r = emu64::run_prg_to_end(&bytes, 6_000_000).expect("runs");
            let got = common::petscii_to_string(&r.output);
            assert_eq!(
                got, expected,
                "{profile:?} extraram={extraram} output mismatch for:\n{src}"
            );
            assert_eq!(
                r.error_banner().as_deref(),
                Some(banner),
                "{profile:?} extraram={extraram} wrong BASIC error for:\n{src}"
            );
        }
    }
}
fn after_jsr(asm: &str, target: u16) -> String {
    let needle = format!("    JSR L{target}\n");
    let rest = asm
        .split_once(&needle)
        .unwrap_or_else(|| panic!("no `JSR L{target}` in:\n{asm}"))
        .1;
    let end = rest
        .lines()
        .position(|l| l.ends_with(':') && !l.starts_with(' '))
        .unwrap_or(rest.lines().count());
    rest.lines().take(end).collect::<Vec<_>>().join("\n")
}
fn curlin_stamp(line: u16) -> String {
    format!("LDA #${:02X}\n    STA $39", line & 0xFF)
}

// The GOSUB marker is a barrier even when the jump looks benign.

#[test]
fn a_gosub_to_the_loops_own_next_is_still_a_barrier() {
    let src = "10 FOR I=1 TO 2\n\
               20 GOSUB 100\n\
               30 PRINT\"z\":END\n\
               100 NEXT:RETURN\n";
    check_stops_with(src, "");
    assert!(
        asm_of(src).contains("__FOR_STACK"),
        "the barrier has to be representable at all"
    );
}
#[test]
fn the_barrier_reaches_the_whole_subroutine_not_just_its_first_line() {
    check_stops_with(
        "10 FOR I=1 TO 3\n\
         20 GOSUB 100\n\
         30 PRINT\"a\"\n\
         40 END\n\
         100 PRINT I;\n\
         110 NEXT\n\
         120 RETURN\n",
        " 1 ",
    );
}

/// `ON n GOSUB` pushes the same marker.
#[test]
fn on_gosub_is_a_barrier_too() {
    check_stops_with(
        "10 FOR I=1 TO 3\n\
         20 X=1\n\
         30 ON X GOSUB 100\n\
         40 PRINT\"a\"\n\
         50 END\n\
         100 PRINT I;\n\
         110 NEXT\n\
         120 RETURN\n",
        " 1 ",
    );
}
#[test]
fn a_subroutine_local_loop_keeps_the_static_fast_path() {
    let src = "10 FOR K=1 TO 3\n\
               20 GOSUB 100\n\
               30 PRINT\"k\";K;\n\
               40 NEXT K\n\
               50 PRINT\"d\";K\n\
               60 END\n\
               100 FOR Z=1 TO 2:NEXT:RETURN\n";
    check_all_variants(src, "K 1 K 2 K 3 D 4 \n");
    let asm = asm_of(src);
    assert!(
        !asm.contains("__FOR_STACK"),
        "neither loop crosses a barrier; both must stay tier 0:\n{asm}"
    );
}
#[test]
fn a_gosub_out_of_a_loop_body_costs_the_loop_nothing() {
    let src = "10 FOR I=1 TO 3\n\
               20 GOSUB 100\n\
               30 PRINT\"k\";I;\n\
               40 NEXT I\n\
               50 PRINT\"d\";I\n\
               60 END\n\
               100 PRINT\"s\";:RETURN\n";
    check_all_variants(src, "SK 1 SK 2 SK 3 D 4 \n");
    assert!(
        !asm_of(src).contains("__FOR_STACK"),
        "a GOSUB that returns crosses nothing"
    );
}

// A body re-entered from below its own NEXT.

#[test]
fn an_exhausted_loop_re_entered_below_its_next_dispatches() {
    check_stops_with(
        "1 GOTO 30\n\
         20 GOTO 40\n\
         30 FOR I=1 TO 3:FOR J=1 TO 3\n\
         40 C=C+1:IF C>9 THEN PRINT\"z\":END\n\
         42 PRINT I;J;:GOTO 50\n\
         50 NEXT I:GOTO 20\n",
        " 1  1  2  1  3  1  4  1 ",
    );
}
#[test]
fn the_re_entry_hole_reached_through_on_goto() {
    check_stops_with(
        "1 GOTO 30\n\
         20 GOTO 40\n\
         30 FOR I=1 TO 3:FOR J=1 TO 3\n\
         40 C=C+1:IF C>9 THEN PRINT\"z\":END\n\
         42 PRINT I;J;:ON I-3*INT(I/3)+1 GOTO 50,52\n\
         50 NEXT I:GOTO 20\n\
         52 NEXT I:GOTO 20\n",
        " 1  1  2  1  3  1  4  1 ",
    );
}
#[test]
fn leaving_a_body_does_not_by_itself_orphan_its_next() {
    let src = "10 GOTO 50\n\
               20 NEXT J:GOTO 200\n\
               30 END\n\
               50 FOR J=1 TO 3\n\
               60 FOR W=1 TO 4\n\
               70 PRINT W;\n\
               80 IF W=2 THEN 100\n\
               90 NEXT W\n\
               100 PRINT\"/\";\n\
               110 GOTO 20\n\
               200 PRINT\"done\"\n";
    let asm = asm_of(src);
    assert_eq!(
        asm.lines()
            .filter(|l| l.trim() == "STA __FOR_STACK,X")
            .count(),
        2,
        "both loops framed:\n{asm}"
    );
    assert_eq!(
        asm.lines()
            .filter(|l| l.starts_with("__FOR_NEXT_HANDLER_"))
            .count(),
        1,
        "only the dispatched loop gets a handler:\n{asm}"
    );
}

// Frame accounting when a NEXT walks past a framed loop.

#[test]
fn an_abandoned_frame_is_dropped_before_the_loop_tail() {
    check_stops_with(
        "10 FOR I=0 TO 3\n\
         20 FOR J=1 TO 4\n\
         30 FOR K=0 TO 3\n\
         40 PRINT I;J;K;\n\
         50 NEXT I\n\
         60 NEXT K\n\
         70 PRINT\"e\"\n",
        " 0  1  0  1  1  0  2  1  0  3  1  0 ",
    );
}
#[test]
fn end_inside_an_on_gosub_subroutine_really_ends() {
    let src = "10 ON 1 GOSUB 100\n\
               20 PRINT\"no\"\n\
               30 END\n\
               100 PRINT\"x\":END\n";
    check_all_variants(src, "X\n");
    assert!(
        asm_of(src).contains("STX __SAVED_SP"),
        "an ON..GOSUB program has to snapshot the stack pointer"
    );
}
#[test]
fn a_tail_gosub_keeps_its_marker_when_the_subroutine_has_a_next() {
    check_stops_with(
        "10 FOR I=1 TO 2:GOSUB 100\n\
         20 PRINT\"a\";I:END\n\
         100 FOR J=1 TO 2:GOSUB 200:RETURN\n\
         200 NEXT:RETURN\n",
        "",
    );
}
#[test]
fn a_tail_gosub_without_a_next_still_folds_to_a_jump() {
    let src = "10 FOR I=1 TO 3\n\
               20 GOSUB 100\n\
               30 NEXT I\n\
               40 PRINT\"d\";I:END\n\
               100 PRINT\"s\";:GOSUB 200:RETURN\n\
               200 PRINT\"t\";:RETURN\n";
    check_all_variants(src, "STSTSTD 4 \n");
    let asm = asm_of(src);
    assert_eq!(
        asm.lines().filter(|l| l.trim() == "JSR L200").count(),
        0,
        "the tail GOSUB 200 should have folded to a jump:\n{asm}"
    );
}

// A returning GOSUB leaves the subroutine's line number in CURLIN.

#[test]
fn a_next_after_a_returning_gosub_is_blamed_on_the_calling_line() {
    check_stops_with_banner(
        "10 GOSUB 100:NEXT\n\
         20 END\n\
         100 FOR I=1 TO 3:RETURN\n",
        "",
        "?NEXT WITHOUT FOR  ERROR IN 10",
    );
}
#[test]
fn the_calling_line_is_restored_before_the_rest_of_the_line_runs() {
    check_stops_with_banner(
        "10 GOSUB 100:PRINT\"back\":NEXT:END\n\
         100 FOR I=1 TO 3:RETURN\n",
        "BACK\n",
        "?NEXT WITHOUT FOR  ERROR IN 10",
    );
}

/// `ON n GOSUB` returns to its join label the same way.
#[test]
fn on_gosub_also_restores_the_calling_line() {
    check_stops_with_banner(
        "10 X=1\n\
         20 ON X GOSUB 100:NEXT\n\
         30 END\n\
         100 FOR I=1 TO 3:RETURN\n",
        "",
        "?NEXT WITHOUT FOR  ERROR IN 20",
    );
}

/// Read back through `PEEK(57)+PEEK(58)*256`, which is what a BASIC program itself would see.
#[test]
fn curlin_after_a_gosub_reads_back_as_the_calling_line() {
    check_all_variants(
        "10 GOSUB 100:A=PEEK(57)+PEEK(58)*256:PRINT A\n\
         20 END\n\
         100 X=1:RETURN\n",
        " 10 \n",
    );
}
#[test]
fn curlin_high_byte_is_restored_across_a_gosub_to_a_high_line() {
    check_all_variants(
        "10 GOSUB 300:A=PEEK(57)+PEEK(58)*256:PRINT A\n\
         20 END\n\
         300 X=1:RETURN\n",
        " 10 \n",
    );
}
#[test]
fn a_gosub_whose_line_cannot_raise_again_pays_nothing_for_the_restore() {
    let asm = asm_of(
        "10 GOSUB 100:END\n\
         100 PRINT I;:FOR I=1 TO 2:NEXT:RETURN\n",
    );
    let tail = after_jsr(&asm, 100);
    assert!(
        !tail.contains("STA $39"),
        "only `END` follows the call, so no re-stamp is owed:\n{tail}"
    );
}
#[test]
fn a_gosub_followed_by_more_of_its_line_does_emit_the_restore() {
    let asm = asm_of(
        "10 GOSUB 100:NEXT\n\
         20 END\n\
         100 FOR I=1 TO 3:RETURN\n",
    );
    let tail = after_jsr(&asm, 100);
    assert!(
        tail.contains(&curlin_stamp(10)),
        "line 10 must re-stamp itself after the call:\n{tail}"
    );
}

// CLR inside a subroutine kills the frame its own RETURN needs.

#[test]
fn clr_in_a_subroutine_makes_its_own_return_fail() {
    check_stops_with_banner(
        "10 FOR I=1 TO 3\n\
         20 GOSUB 100\n\
         30 NEXT\n\
         40 END\n\
         100 CLR:RETURN\n",
        "",
        "?RETURN WITHOUT GOSUB  ERROR IN 100",
    );
}
#[test]
fn clr_in_a_subroutine_discards_the_callers_for_frames_too() {
    check_stops_with_banner(
        "10 FOR I=1 TO 2\n\
         20 FOR J=1 TO 2\n\
         30 PRINT\"a\";I;J\n\
         40 GOSUB 100\n\
         50 NEXT J,I\n\
         60 END\n\
         100 CLR:RETURN\n",
        "A 1  1 \n",
        "?RETURN WITHOUT GOSUB  ERROR IN 100",
    );
}
#[test]
fn a_subroutine_without_clr_is_still_inlined() {
    let src = "10 FOR I=1 TO 3\n\
               20 GOSUB 100\n\
               30 NEXT\n\
               40 PRINT\"d\";I:END\n\
               100 X=X+1:RETURN\n";
    check_all_variants(src, "D 4 \n");
    let asm = asm_of(src);
    assert_eq!(
        asm.lines().filter(|l| l.trim() == "JSR L100").count(),
        0,
        "a CLR-free single-use body must still inline:\n{asm}"
    );
}

// A *bare* NEXT takes the top of the stack, not its lexical partner.

#[test]
fn a_bare_next_reached_from_a_deeper_body_advances_the_inner_loop() {
    check_all_variants(
        "10 FOR I=0 TO 3\n\
         20 FOR J=1 TO 4\n\
         30 FOR K=0 TO 3\n\
         40 C=C+1:PRINT C;\n\
         50 ON (C AND 3)+1 GOTO 60,70,80,90\n\
         60 NEXT\n\
         70 NEXT\n\
         80 NEXT\n\
         90 PRINT\"e\";I;J;K\n",
        " 1  2  3 E 0  1  2 \n",
    );
}
#[test]
fn bare_nexts_in_a_three_deep_on_goto_all_bind_the_innermost_loop() {
    check_all_variants(
        "10 FOR I=1 TO 2\n\
         20 FOR J=1 TO 2\n\
         30 FOR K=1 TO 2\n\
         40 C=C+1:ON (C AND 3)+1 GOTO 50,60,70,80\n\
         50 NEXT\n\
         60 NEXT\n\
         70 NEXT\n\
         80 PRINT\"e\";I;J;K\n",
        "E 1  1  3 \n",
    );
}
#[test]
fn a_bare_next_reached_from_its_own_body_keeps_the_static_fast_path() {
    let src = "10 FOR I=1 TO 2\n\
               20 FOR X=1 TO 3\n\
               30 PRINT X;\n\
               40 IF X<=1 THEN 60\n\
               50 PRINT\"-\";\n\
               60 NEXT\n\
               70 NEXT I\n\
               80 PRINT\"e\";I;X\n";
    check_all_variants(src, " 1  2 - 3 - 1  2 - 3 -E 3  4 \n");
    assert!(
        !asm_of(src).contains("__FOR_STACK"),
        "the innermost loop's own back-jump must stay tier 0"
    );
}
#[test]
fn a_named_next_reached_from_a_deeper_body_keeps_the_static_fast_path() {
    let src = "10 FOR I=1 TO 3\n\
               20 FOR J=1 TO 3\n\
               30 IF J=2 THEN 50\n\
               40 NEXT J\n\
               50 NEXT I\n\
               60 PRINT\"e\";I;J\n";
    check_all_variants(src, "E 4  2 \n");
    assert!(
        !asm_of(src).contains("__FOR_STACK"),
        "a named NEXT resolves by search; nothing here needs a frame"
    );
}
