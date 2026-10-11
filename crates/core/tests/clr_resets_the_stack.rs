mod common;

#[test]
fn clr_drops_the_frames_of_abandoned_subroutines() {
    let src = "\
10 P=PEEK(830)+1:POKE 830,P:IF P>200 THEN PRINT \"OK\":END
20 GOSUB 30
30 CLR:GOTO 10
";
    let prg = common::compile_source(src).expect("compiles");
    assert_eq!(common::run_clean(&prg).expect("reaches a clean exit"), "OK\n");
}

#[test]
fn return_after_clr_has_no_gosub() {
    let src = "10 GOSUB 20\n20 CLR:RETURN\n";
    let prg = common::compile_source(src).expect("compiles");
    let r = emu64::run_prg_to_end(&prg, common::MAX_INSNS).expect("runs");
    assert_eq!((r.error_code, r.error_line), (Some(12), Some(20)));
}
