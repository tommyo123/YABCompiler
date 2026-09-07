//! A ROM float op through `emit_op_with_temp` overwrites FAC1, so every "FAC1 already holds <expr>" fact the emitter carries dies with it.

mod common;

use common::{compile_source, run_clean};

use yabcompiler_core::{CompileOptions, compile_with_options, tokenize_program};

/// `N = 2` and `F(2,0) = 2`, so the left compare's difference is exactly 0.
const SILENT: &str = "10 DIM F(3,2)\n\
                      20 POKE 2,4\n\
                      30 N=PEEK(2)/2\n\
                      40 F(0,1)=0.5\n\
                      50 F(2,0)=2\n\
                      60 F(2,1)=1\n\
                      70 FF=1\n\
                      80 IF F(N,0)<>N OR F(N,1)<>1 THEN FF=0\n\
                      90 PRINT \"FF=\";FF\n";

const NEGATIVE: &str = "10 DIM F(3,2)\n\
                        20 POKE 2,4\n\
                        30 N=PEEK(2)/2\n\
                        40 F(0,1)=0.5\n\
                        50 F(2,0)=1\n\
                        60 F(2,1)=1\n\
                        70 FF=1\n\
                        80 IF F(N,0)<>N OR F(N,1)<>1 THEN FF=0\n\
                        90 PRINT \"FF=\";FF\n";

/// The second consumer, and no array in sight.
const SHORT_CIRCUIT: &str = "10 POKE 2,12\n\
                             20 B=PEEK(2)/4\n\
                             30 X=PEEK(2)/6\n\
                             40 R=9\n\
                             50 IF X*2<>B AND 1<>B THEN R=1\n\
                             60 PRINT \"R=\";R\n";

#[test]
fn silent_zero_difference_does_not_reindex_the_array() {
    let prg = compile_source(SILENT).expect("compiles");
    let out = run_clean(&prg).expect("runs to a clean exit");
    assert!(
        out.contains("FF= 1"),
        "the interpreter prints `FF= 1`; a stale FAC cache indexes F(0,1) \
         instead of F(2,1) and prints `FF= 0` with no error at all.\n\
         got: {out:?}"
    );
}

#[test]
fn negative_difference_does_not_raise_a_spurious_illegal_quantity() {
    let prg = compile_source(NEGATIVE).expect("compiles");
    let out = run_clean(&prg).expect("runs to a clean exit");
    assert!(
        !out.contains("ILLEGAL QUANTITY"),
        "the interpreter runs this clean; a stale FAC cache feeds the \
         negative difference to GETADR.\ngot: {out:?}"
    );
    assert!(
        out.contains("FF= 0"),
        "the interpreter prints `FF= 0`.\ngot: {out:?}"
    );
}

#[test]
fn short_circuit_and_arm_does_not_reuse_a_consumed_fac() {
    let prg = compile_source(SHORT_CIRCUIT).expect("compiles");
    let out = run_clean(&prg).expect("runs to a clean exit");
    assert!(
        out.contains("R= 1"),
        "the interpreter prints `R= 1`; a stale FAC cache makes the right \
         compare read the left compare's difference and prints `R= 9`.\n\
         This shape reaches the producer through the short-circuit arm, \
         NOT through `emit_logic_byte` — a fix in that consumer alone \
         cannot reach it.\ngot: {out:?}"
    );
}

#[test]
fn no_reuse_fac_directly_follows_a_float_op_through_a_temp() {
    for (name, src) in [
        ("silent", SILENT),
        ("negative", NEGATIVE),
        ("short_circuit", SHORT_CIRCUIT),
    ] {
        let asm = compile_with_options(
            &tokenize_program(src).expect("tokenizes"),
            CompileOptions::default(),
        )
        .expect("compiles")
        .asm;

        let mut open: Option<(usize, &str)> = None;
        for (i, line) in asm.lines().enumerate() {
            let t = line.trim();
            if let Some((op_line, op)) = open
                && t.contains("; reuse FAC")
            {
                panic!(
                    "{name}: `; reuse FAC` at line {} reuses a FAC that \
                     `{op}` at line {} has already overwritten.\n\
                     The producer must invalidate the cache.\n{asm}",
                    i + 1,
                    op_line + 1
                );
            }
            if t.is_empty() || t.starts_with(';') {
                continue;
            }
            let is_temp_op = ["__FADD__T0", "__FSUB__T0", "__FMULT__T0", "__FDIV__T0"]
                .iter()
                .any(|s| t == format!("JSR {s}"));
            if is_temp_op {
                open = Some((i, t));
            } else if t.starts_with("LDA $") || t.starts_with("B") {
                // Flag read at $61/$66 and conditional branch on it: this is exactly the compare tail, and it leaves FAC1 alone.
            } else {
                open = None;
            }
        }
    }
}
