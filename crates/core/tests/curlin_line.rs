//! CURLIN ($39/$3A) line stamping.

mod common;

/// Compile, run, and return the decoded PRINT output.
fn run(src: &str) -> String {
    let prg = common::compile_source(src).expect("compiles");
    let r = emu64::run_prg_to_end(&prg, 5_000_000).expect("runs");
    assert!(r.clean_exit, "program must reach END:\n{src}");
    common::petscii_to_string(&r.output)
}
const READ_CURLIN: &str = "A=PEEK(57)+PEEK(58)*256:PRINT A";

#[test]
fn stamps_the_current_line_below_256() {
    let out = run(&format!("10 X=1\n20 {READ_CURLIN}\n30 END\n"));
    assert!(out.contains("20"), "line 20 must stamp itself:\n{out}");
}

#[test]
fn stamps_correctly_at_a_branch_target() {
    let out = run(&format!(
        "10 X=1\n20 GOTO 40\n30 PRINT \"SKIPPED\"\n40 {READ_CURLIN}\n50 END\n"
    ));
    assert!(out.contains("40"), "branch target must stamp itself:\n{out}");
    assert!(!out.contains("SKIPPED"), "GOTO must skip line 30:\n{out}");
}

#[test]
fn stamps_correctly_inside_a_for_body() {
    let out = run(&format!("10 FOR I=1 TO 2\n20 {READ_CURLIN}\n30 NEXT\n40 END\n"));
    assert!(out.contains("20"), "FOR-body line must stamp itself:\n{out}");
}

#[test]
fn stamps_a_high_byte_for_lines_above_255() {
    // Every line here is >= 256, so the invariant fires with hi = 1.
    let out = run(&format!("300 X=1\n310 {READ_CURLIN}\n320 END\n"));
    assert!(
        out.contains("310"),
        "line above 255 must stamp its high byte:\n{out}"
    );
}

#[test]
fn stamps_correctly_when_high_bytes_are_mixed() {
    // Lines straddle 255/256, so the whole-module invariant declines and the per-line cache
    // takes over.
    let out = run(&format!(
        "10 X=1\n20 GOTO 300\n30 END\n300 {READ_CURLIN}\n310 END\n"
    ));
    assert!(
        out.contains("300"),
        "mixed high bytes must still stamp correctly:\n{out}"
    );
}

#[test]
fn stamps_correctly_returning_to_a_low_line_after_a_high_one() {
    // The reverse crossing: a high line runs first, then control returns to a low line.
    let out = run(&format!(
        "10 GOTO 300\n20 {READ_CURLIN}\n30 END\n300 X=1\n310 GOTO 20\n"
    ));
    assert!(
        out.contains("20"),
        "returning to a low line must reset the high byte:\n{out}"
    );
}
