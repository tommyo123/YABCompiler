//! `emit_sys_param_build` renders each SYS parameter-text substitution by evaluating it into FAC1 and calling FOUT (`JSR $BDDD`).

mod common;

use common::{compile_source, run_clean};

use yabcompiler_core::{CompileOptions, compile_with_options, tokenize_program};

const WITNESS: &str = "10 FOR I=0 TO 18:READ D:POKE 828+I,D:NEXT\n\
                       20 POKE 2,20\n\
                       30 A=PEEK(2)/2\n\
                       40 SYS 828,A,A\n\
                       50 PRINT \"P1=\";PEEK(848)\n\
                       55 PRINT \"P2=\";PEEK(849)\n\
                       60 DATA 32,253,174,32,158,183,142,80,3\n\
                       70 DATA 32,253,174,32,158,183,142,81,3,96\n\
                       80 END\n";

const CONTROL: &str = "10 FOR I=0 TO 18:READ D:POKE 828+I,D:NEXT\n\
                       20 POKE 2,20\n\
                       30 A=PEEK(2)/2\n\
                       35 B=PEEK(2)/2\n\
                       40 SYS 828,A,B\n\
                       50 PRINT \"P1=\";PEEK(848)\n\
                       55 PRINT \"P2=\";PEEK(849)\n\
                       60 DATA 32,253,174,32,158,183,142,80,3\n\
                       70 DATA 32,253,174,32,158,183,142,81,3,96\n\
                       80 END\n";

#[test]
fn repeated_sys_param_renders_the_value_not_the_fout_residue() {
    let prg = compile_source(WITNESS).expect("compiles");

    let out = run_clean(&prg).unwrap_or_else(|| {
        panic!(
            "the interpreter runs this program to a clean exit. A stale FAC \
             cache makes the second substitution render FOUT's residue, the \
             ROM's GETBYT rejects it, and the image stops on an ?ILLEGAL \
             QUANTITY the interpreter never raises."
        )
    });
    assert!(
        out.contains("P1= 10"),
        "the first substitution is not the one under test and must be 10.\n\
         got: {out:?}"
    );
    assert!(
        out.contains("P2= 10"),
        "the second substitution must render `A` (10) again. A stale FAC \
         cache renders what FOUT left in FAC1 after printing the first one, \
         so GETBYT stores something else entirely.\ngot: {out:?}"
    );
}

#[test]
fn distinct_variables_of_equal_value_are_unaffected() {
    let prg = compile_source(CONTROL).expect("compiles");
    let out = run_clean(&prg).expect("runs to a clean exit");
    assert!(
        out.contains("P1= 10") && out.contains("P2= 10"),
        "the control must pass in BOTH arms — it never reuses a consumed \
         FAC, because the second substitution names a different variable. \
         A failure here means the witness is measuring the harness.\n\
         got: {out:?}"
    );
}

/// Structural guard on the producer.
#[test]
fn no_reuse_fac_follows_a_consumed_fout_inside_a_sys_param_build() {
    for (name, src) in [("witness", WITNESS), ("control", CONTROL)] {
        let asm = compile_with_options(
            &tokenize_program(src).expect("tokenizes"),
            CompileOptions::default(),
        )
        .expect("compiles")
        .asm;

        let mut in_build = false;
        // Set by `JSR __SYS_PB_VAL`, which is emitted immediately after the `JSR $BDDD` for a substitution.
        let mut consumed: Option<usize> = None;
        let mut windows = 0usize;

        for (i, line) in asm.lines().enumerate() {
            let t = line.trim();
            if t.starts_with("__SYS_PARAMS_END_") && t.ends_with(':') {
                in_build = true;
                consumed = None;
                windows += 1;
                continue;
            }
            if !in_build {
                continue;
            }
            if t == "LDA $7A" {
                in_build = false;
                consumed = None;
                continue;
            }
            if let Some(at) = consumed
                && t.contains("; reuse FAC")
            {
                panic!(
                    "{name}: `; reuse FAC` at line {} reuses a FAC that FOUT \
                     consumed at line {}.\nThe producer must invalidate the \
                     cache after `JSR $BDDD`.\n{asm}",
                    i + 1,
                    at + 1
                );
            }
            if t == "JSR __SYS_PB_VAL" {
                consumed = Some(i);
            }
        }

        assert_eq!(
            windows, 1,
            "{name}: expected exactly one SYS parameter build to inspect, \
             found {windows} — this guard has nothing to guard.\n{asm}"
        );
    }
}
