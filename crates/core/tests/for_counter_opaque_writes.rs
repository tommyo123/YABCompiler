mod common;

use yabcompiler_core::{CompileOptions, compile_with_options, tokenize_program};

fn build(src: &str) -> yabcompiler_core::Compiled {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(&prg, CompileOptions::default()).expect("compiles")
}

fn output(src: &str) -> String {
    let c = build(src);
    let r = emu64::run_prg_to_end(&c.prg_bytes, common::MAX_INSNS).expect("runs");
    assert!(r.clean_exit, "must reach END: {:?}", r.error_banner());
    common::petscii_to_string(&r.output).trim_end().to_string()
}

const ELSEWHERE: &str = "\
10 DEF FN A(X)=X*2
20 T=PEEK(2)+0.5
30 FOR T=1 TO 10:PRINT T;:NEXT
40 PRINT FN A(3)
50 IF PEEK(2)=7 THEN SYS 49152:PRINT USR(1)
60 END
";

#[test]
fn opaque_statements_the_loop_cannot_reach_keep_the_integer_counter() {
    let asm = build(ELSEWHERE).asm;
    let top = asm.find("__FOR_TOP_").expect("a loop head");
    let exit = top + asm[top..].find("__FOR_EXIT_").expect("a loop exit");
    assert!(
        !asm[top..exit].contains("JSR $B867") && !asm[top..exit].contains("JSR $B850"),
        "the FOR T loop must count in an integer:\n{}",
        &asm[top..exit]
    );
    assert_eq!(output(ELSEWHERE), " 1  2  3  4  5  6  7  8  9  10  6");
}

#[test]
fn an_fn_assigning_the_counter_from_inside_the_loop_keeps_basic_semantics() {
    assert_eq!(
        output(
            "10 DEF FN A(T)=T*10\n20 T=0\n30 FOR T=1 TO 3\n40 GOSUB 100\n50 NEXT\n\
             60 PRINT \"END\";T\n70 END\n100 PRINT FN A(7);T;\n110 RETURN\n"
        ),
        " 70  1  70  2  70  3 END 4"
    );
}

#[test]
fn a_clr_the_loop_can_reach_still_counts_as_a_write() {
    let c = build(
        "10 T=5\n20 FOR T=1 TO 3\n30 IF T=2 THEN 100\n40 NEXT\n50 END\n100 CLR\n110 GOTO 40\n",
    );
    let r = emu64::run_prg_to_end(&c.prg_bytes, common::MAX_INSNS).expect("runs");
    assert_eq!(r.error_banner().as_deref(), Some("?NEXT WITHOUT FOR  ERROR IN 40"));
}

#[test]
fn a_gosub_in_the_body_returns_to_its_own_call_not_to_every_return_site() {
    let src = "10 GOSUB 100:I=I+1:PRINT I;\n20 FOR I=1 TO 5:GOSUB 100:NEXT\n30 PRINT I:END\n\
               100 PRINT \"*\";\n110 IF PEEK(2)=7 THEN 100\n120 RETURN\n";
    let asm = build(src).asm;
    let top = asm.find("__FOR_TOP_").expect("a loop head");
    let exit = top + asm[top..].find("__FOR_EXIT_").expect("a loop exit");
    assert!(
        !asm[top..exit].contains("JSR $B867") && !asm[top..exit].contains("JSR $B850"),
        "the FOR I loop must count in an integer:\n{}",
        &asm[top..exit]
    );
    assert_eq!(output(src), "* 1 ***** 6");
}

#[test]
fn a_routine_that_writes_the_counter_still_counts() {
    assert_eq!(
        output("10 FOR I=1 TO 5:GOSUB 100:NEXT\n20 PRINT I:END\n100 PRINT I;:I=I+1:RETURN\n"),
        " 1  3  5  7"
    );
}
