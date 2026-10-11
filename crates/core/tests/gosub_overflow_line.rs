mod common;

const OUT_OF_MEMORY: u8 = 16;

fn error_at(src: &str) -> (Option<u8>, Option<u16>) {
    let prg = common::compile_source(src).expect("compiles");
    let r = emu64::run_prg_to_end(&prg, common::MAX_INSNS).expect("runs");
    (r.error_code, r.error_line)
}

#[test]
fn a_gosub_that_overflows_names_its_line() {
    let src = "\
10 FOR I=1 TO 1:NEXT
20 GOSUB 100:GOSUB 200
100 PRINT \"A\";:RETURN
200 GOTO 20
";
    assert_eq!(error_at(src), (Some(OUT_OF_MEMORY), Some(20)));
}

#[test]
fn a_gosub_after_a_plain_assignment_names_its_line() {
    let src = "\
10 FOR I=1 TO 1:NEXT
20 X=38:PRINT \"B\";:GOSUB 100:GOSUB 200
100 PRINT \"A\";:RETURN
200 PRINT X;:GOTO 20
";
    assert_eq!(error_at(src), (Some(OUT_OF_MEMORY), Some(20)));
}
