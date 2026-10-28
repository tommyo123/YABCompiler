mod common;

use yabcompiler_core::{CompileOptions, compile_with_options, tokenize_program};

fn asm_of(src: &str) -> String {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(&prg, CompileOptions::default())
        .expect("compiles")
        .asm
}

fn run(src: &str) -> emu64::RunToEnd {
    let prg = common::compile_source(src).expect("compiles");
    emu64::run_prg_to_end(&prg, 60_000_000).expect("runs")
}

fn text(src: &str) -> String {
    let r = run(src);
    assert!(r.clean_exit, "must reach END: {src}");
    common::petscii_to_string(&r.output).trim_end().to_string()
}

fn insns(src: &str) -> u64 {
    let r = run(src);
    assert!(r.clean_exit, "must reach END: {src}");
    r.instructions
}

/// `A$ = A$ + "X"` repeated `n` times, printing the result so the run cannot be optimised into nothing and the transcript carries the whole string.
fn grow(n: usize) -> String {
    format!("10 A$=\"\"\n20 FOR I=1 TO {n}\n30 A$=A$+\"X\"\n40 NEXT I\n50 PRINT A$\n60 PRINT LEN(A$)\n70 END\n")
}

#[test]
fn concat_helper_compares_lhs_against_the_destination() {
    let asm = asm_of(&grow(10));
    let body = asm
        .split("__STR_CONCAT:")
        .nth(1)
        .expect("__STR_CONCAT is emitted");
    let head = body.split("__CONCAT_AFTER_LHS:").next().unwrap();
    for want in [
        "LDA $03",
        "CMP $FD",
        "BNE __CONCAT_COPY_NEEDED",
        "LDA $04",
        "CMP $FE",
        "BEQ __CONCAT_AFTER_LHS",
        "__CONCAT_COPY_NEEDED:",
    ] {
        assert!(
            head.contains(want),
            "missing {want:?} before the lhs copy loop:\n{head}"
        );
    }
    // Ordering: the guard must precede the loop it guards, and the loop must still be there for the non-aliasing case.
    let guard = head.find("BNE __CONCAT_COPY_NEEDED").unwrap();
    let loop_top = head.find("__CONCAT_COPY_LHS:").expect("copy loop kept");
    assert!(guard < loop_top, "guard is not before the loop:\n{head}");
}

#[test]
fn a_module_that_never_self_appends_does_not_carry_the_guard() {
    // The operands have to be built at run time: a concat of two literals is folded at compile time and emits no helper at all.
    let asm = asm_of(
        "10 FOR I=1 TO 3\n20 A$=CHR$(64+I)\n30 B$=CHR$(70+I)\n40 C$=A$+B$\n\
         50 PRINT C$\n60 NEXT I\n70 END\n",
    );
    assert!(asm.contains("__STR_CONCAT:"), "the helper is still emitted");
    assert!(
        !asm.contains("__CONCAT_COPY_NEEDED"),
        "guard emitted into a module that cannot use it:\n{asm}"
    );
    assert!(
        asm.contains("__CONCAT_COPY_LHS:"),
        "the copy loop is what such a module relies on"
    );
    // A prepend is not a customer either: the destination is a fresh chunk and the left operand is the literal.
    let asm = asm_of("10 A$=\"Z\"\n20 FOR I=1 TO 3\n30 A$=\"X\"+A$\n40 NEXT I\n50 PRINT A$\n60 END\n");
    assert!(
        !asm.contains("__CONCAT_COPY_NEEDED"),
        "prepend-only module carries the guard:\n{asm}"
    );
}

#[test]
fn self_append_builds_the_whole_string() {
    let out = text(&grow(100));
    let want = format!("{}\n 100", "X".repeat(100));
    assert_eq!(out, want, "self-append lost or reordered characters");
}

#[test]
fn self_append_of_a_varying_tail_is_exact() {
    let src = "10 A$=\"\"\n20 FOR I=1 TO 9\n30 A$=A$+CHR$(48+I)+\"-\"\n40 NEXT I\n\
               50 PRINT A$\n60 PRINT LEN(A$)\n70 END\n";
    assert_eq!(text(src), "1-2-3-4-5-6-7-8-9-\n 18");
}

/// Interleaving reads of the string with the appends: the value has to be right during the loop, not only at the end.
#[test]
fn self_append_is_readable_between_appends() {
    let src = "10 A$=\"\"\n20 FOR I=1 TO 5\n30 A$=A$+\"AB\"\n\
               40 PRINT LEN(A$);LEFT$(A$,2);RIGHT$(A$,1)\n50 NEXT I\n60 END\n";
    assert_eq!(
        text(src),
        " 2 ABB\n 4 ABB\n 6 ABB\n 8 ABB\n 10 ABB",
        "the appended string is wrong while it is being built"
    );
}

const PRELUDE: &str = "1 A$=\"\"\n2 FOR Z=1 TO 4\n3 A$=A$+\"Z\"\n4 NEXT Z\n5 PRINT A$\n";

#[test]
fn prepend_still_copies_the_left_operand() {
    let src = format!(
        "{PRELUDE}10 P$=\"\"\n20 FOR I=1 TO 5\n30 P$=\"X\"+P$+\"Y\"\n\
         40 NEXT I\n50 PRINT P$\n60 PRINT LEN(P$)\n70 END\n"
    );
    assert!(asm_of(&src).contains("__CONCAT_COPY_NEEDED"), "guard absent");
    assert_eq!(text(&src), "ZZZZ\nXXXXXYYYYY\n 10");
}

/// Two different variables: the destination is a new chunk and both operands must be copied into it.
#[test]
fn non_self_concat_copies_both_operands() {
    let src = format!(
        "{PRELUDE}10 B$=\"AB\"+A$\n20 C$=\"DE\"+A$\n30 D$=B$+C$\n\
         40 PRINT D$;LEN(D$)\n50 PRINT B$;C$\n60 END\n"
    );
    assert!(asm_of(&src).contains("__CONCAT_COPY_NEEDED"), "guard absent");
    assert_eq!(text(&src), "ZZZZ\nABZZZZDEZZZZ 12 \nABZZZZDEZZZZ");
}

/// `B$ = B$ + B$` ; the operand appears on both sides, so the rewind is refused and the destination is a fresh chunk.
#[test]
fn self_doubling_is_not_treated_as_an_in_place_append() {
    let src = format!(
        "{PRELUDE}10 B$=\"AB\"\n20 FOR I=1 TO 4\n30 B$=B$+B$\n40 NEXT I\n\
         50 PRINT B$\n60 PRINT LEN(B$)\n70 END\n"
    );
    assert!(asm_of(&src).contains("__CONCAT_COPY_NEEDED"), "guard absent");
    assert_eq!(text(&src), format!("ZZZZ\n{}\n 32", "AB".repeat(16)));
}

#[test]
fn string_array_self_append_still_copies() {
    let src = format!(
        "{PRELUDE}10 DIM Q$(3)\n20 Q$(1)=\"P\"\n30 Q$(2)=\"Q\"\n\
         40 FOR I=1 TO 3\n50 Q$(1)=Q$(1)+\"1\"\n60 Q$(2)=Q$(2)+\"2\"\n70 NEXT I\n\
         80 PRINT Q$(1);Q$(2)\n90 PRINT LEN(Q$(1));LEN(Q$(2))\n100 END\n"
    );
    assert!(asm_of(&src).contains("__CONCAT_COPY_NEEDED"), "guard absent");
    assert_eq!(text(&src), "ZZZZ\nP111Q222\n 4  4");
}

#[test]
fn temporary_concat_is_unaffected() {
    let src = format!(
        "{PRELUDE}10 B$=\"AB\"+A$\n20 C$=\"CD\"+A$\n\
         30 IF B$+C$=\"ABZZZZCDZZZZ\" THEN PRINT \"OK\"\n\
         40 PRINT B$+C$+B$\n50 PRINT B$;C$\n60 END\n"
    );
    assert!(asm_of(&src).contains("__CONCAT_COPY_NEEDED"), "guard absent");
    assert_eq!(
        text(&src),
        "ZZZZ\nOK\nABZZZZCDZZZZABZZZZ\nABZZZZCDZZZZ"
    );
}

#[test]
fn appending_is_no_longer_quadratic() {
    let bench = |n: usize| {
        format!("10 A$=\"\"\n20 FOR I=1 TO {n}\n30 A$=A$+\"X\"\n40 NEXT I\n50 PRINT LEN(A$)\n60 END\n")
    };
    let n100 = insns(&bench(100));
    let n200 = insns(&bench(200));
    let ratio = n200 as f64 / n100 as f64;
    assert!(
        ratio < 2.4,
        "appending is still superlinear: {n100} insns at n=100, {n200} at n=200 (ratio {ratio:.2})"
    );

    assert!(
        n200 > n100,
        "the n=200 bench did less work than the n=100 one ({n200} vs {n100})"
    );
}

#[test]
fn the_guard_costs_the_non_appending_shape_almost_nothing() {
    let prepend = |n: usize| {
        format!("10 A$=\"\"\n20 FOR I=1 TO {n}\n30 A$=\"X\"+A$\n40 NEXT I\n50 PRINT LEN(A$)\n60 END\n")
    };
    let n100 = insns(&prepend(100));

    assert!(
        n100 < 39_000,
        "prepend got materially more expensive: {n100} instructions"
    );
}

#[test]
fn appending_allocates_exactly_what_it_did_before() {
    let src = "10 A$=\"\"\n20 F0=FRE(0)\n30 FOR I=1 TO 10\n40 A$=A$+\"X\"\n50 NEXT I\n\
               60 PRINT FRE(0)-F0\n70 PRINT LEN(A$)\n80 END\n";

    assert_eq!(text(src), "-13 \n 10");
}

#[test]
fn the_helper_still_assembles_to_the_emitted_machine_code() {
    let prg = tokenize_program(&grow(20)).expect("tokenizes");
    let c = compile_with_options(&prg, CompileOptions::default()).expect("compiles");
    assert!(
        c.asm.contains("__CONCAT_COPY_NEEDED"),
        "guard not present in the listing"
    );
    assert!(!c.machine_code.is_empty(), "no machine code");
}

#[test]
fn an_array_element_copy_survives_the_next_append() {
    assert_eq!(
        text(
            "10 DIM Z$(3):X$=\"A\"\n\
             20 FOR I=0 TO 3:Z$(I)=X$:X$=X$+\"B\":NEXT\n\
             30 FOR I=0 TO 3:PRINT LEN(Z$(I));Z$(I):NEXT\n\
             40 END\n"
        ),
        " 1 A\n 2 AB\n 3 ABB\n 4 ABBB"
    );
    assert_eq!(
        text("10 DIM Y$(2):X$=\"A\":X$=X$+\"B\":Y$(1)=X$+\"Q\":X$=X$+\"C\"\n20 PRINT X$;\" \";Y$(1)\n30 END\n"),
        "ABC ABQ"
    );
}

#[test]
fn appending_still_happens_in_place_after_an_array_store() {
    let base = insns(&grow(200));
    let with_store = insns(
        "10 DIM Z$(1):A$=\"\"\n20 FOR I=1 TO 200\n30 A$=A$+\"X\"\n40 NEXT I\n45 Z$(1)=A$\n50 PRINT A$\n60 PRINT LEN(A$)\n70 END\n",
    );
    assert!(
        with_store < base * 11 / 10,
        "one array store must not make the appends quadratic: {base} vs {with_store}"
    );
}

fn speed_run(src: &str) -> (String, u64, String) {
    let prg = tokenize_program(src).expect("tokenizes");
    let c = compile_with_options(
        &prg,
        CompileOptions {
            profile: yabcompiler_core::Profile::Speed,
            ..CompileOptions::default()
        },
    )
    .expect("compiles");
    let r = emu64::run_prg_to_end(&c.prg_bytes, 60_000_000).expect("runs");
    assert!(r.clean_exit, "must reach END: {src}");
    let rom = emu64::run_basic_to_end(&prg, 400_000_000).expect("runs in the interpreter");
    assert_eq!(
        common::petscii_to_string(&r.output),
        common::petscii_to_string(&rom.output),
        "{src}"
    );
    (
        common::petscii_to_string(&r.output).trim_end().to_string(),
        r.instructions,
        c.asm,
    )
}

/// Under Speed an element whose chunk is the last one stored grows where it is.
#[test]
fn speed_appends_to_an_array_element_in_place() {
    let (out, _, asm) = speed_run(
        "10 DIM C$(3):FOR I=1 TO 90:C$(2)=C$(2)+\"X\":NEXT:PRINT LEN(C$(2))\n\
         20 FOR I=1 TO 2:C$(1)=C$(1)+C$(2):C$(3)=C$(3)+\"Y\":NEXT:PRINT LEN(C$(1));C$(3)\n\
         30 C$(2)=\"Q\":C$(2)=C$(2)+\"R\":PRINT C$(2);LEN(C$(1))\n",
    );
    assert!(asm.contains("JSR __ARR_APPEND_PRE"), "{asm}");
    assert_eq!(out, " 90 \n 180 YY\nQR 180");
    let bench =
        |n: usize| format!("10 DIM C$(3):FOR I=1 TO {n}:C$(2)=C$(2)+\"X\":NEXT:PRINT LEN(C$(2))\n");
    let (_, n100, _) = speed_run(&bench(100));
    let (_, n200, _) = speed_run(&bench(200));
    let ratio = n200 as f64 / n100 as f64;
    assert!(
        ratio < 2.4,
        "array appends are still superlinear: {n100} insns at n=100, {n200} at n=200"
    );
}
