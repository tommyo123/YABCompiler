//! Guarded integer loops and float fallbacks produce matching output.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

type Outcome = (String, Option<u8>, Option<u16>);

fn interpreted(src: &str) -> Outcome {
    let prg = tokenize_program(src).expect("tokenizes");
    let r = emu64::run_basic_to_end(&prg, 200_000_000).expect("runs in the interpreter");
    (
        common::petscii_to_string(&r.output),
        r.error_code,
        r.error_line,
    )
}

fn compiled(src: &str, profile: Profile, extraram: bool) -> (Outcome, String) {
    let prg = tokenize_program(src).expect("tokenizes");
    let c = compile_with_options(
        &prg,
        CompileOptions {
            profile,
            extraram,
            force_extraram_off: !extraram,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}\n{src}"));
    let r = emu64::run_prg_to_end(&c.prg_bytes, 200_000_000).expect("runs");
    (
        (
            common::petscii_to_string(&r.output),
            r.error_code,
            r.error_line,
        ),
        c.asm,
    )
}

/// Every profile matches the interpreter; the speed profile guards a loop when `guarded` says so.
fn check(src: &str, guarded: Option<bool>) {
    let want = interpreted(src);
    for profile in [Profile::Speed, Profile::Default, Profile::Size] {
        for extraram in [false, true] {
            let (got, asm) = compiled(src, profile, extraram);
            assert_eq!(got, want, "{profile:?}, extraram={extraram}:\n{src}");
            if profile == Profile::Speed
                && let Some(guarded) = guarded
            {
                assert_eq!(
                    asm.contains("__GIF_"),
                    guarded,
                    "guard in speed, extraram={extraram}:\n{src}"
                );
            }
        }
    }
}

#[test]
fn ends_the_guard_takes_and_turns_away() {
    check(
        "10 FOR N=-3 TO 7:E=N*1.5:GOSUB 100:NEXT:END\n\
         100 FOR I=1 TO E:PRINT I;:NEXT:PRINT \"/\"I:RETURN\n",
        Some(true),
    );
    check(
        "10 E=32765:GOSUB 100:E=32766:GOSUB 100:E=32767:GOSUB 100:E=40000:GOSUB 100:END\n\
         100 FOR I=32760 TO E:NEXT:PRINT I:RETURN\n",
        Some(true),
    );
    check(
        "10 E=-1:GOSUB 100:E=-32768:GOSUB 100:E=-40000:GOSUB 100:END\n\
         100 FOR I=1 TO E:NEXT:PRINT I:RETURN\n",
        Some(false),
    );
}

#[test]
fn starts_that_are_fractional_negative_or_large_take_the_original_loop() {
    check(
        "10 FOR K=1 TO 6:READ A:GOSUB 100:NEXT:END\n\
         20 DATA 2,2.5,-2,0,-40000,32766\n\
         100 FOR I=A TO A+2.5:PRINT I;:NEXT:PRINT \"/\"I:RETURN\n",
        Some(true),
    );
}

#[test]
fn downward_loops_use_the_ceiling_of_the_end() {
    check(
        "10 FOR K=1 TO 5:READ E:GOSUB 100:NEXT:END\n\
         20 DATA 1,1.5,-1.5,0,9\n\
         100 FOR I=8 TO E STEP -3:PRINT I;:NEXT:PRINT \"/\"I:RETURN\n",
        Some(true),
    );
    check(
        "10 FOR K=1 TO 4:READ A,E:GOSUB 100:NEXT:END\n\
         20 DATA 5,1,5,1.5,2.5,0,-3,-9\n\
         100 FOR I=A TO E STEP -1:PRINT I;:NEXT:PRINT \"/\"I:RETURN\n",
        Some(true),
    );
}

#[test]
fn the_end_is_evaluated_once_after_the_start_is_stored() {
    // The end reads the counter, an FN reads it and RND would move on if evaluated twice.
    check(
        "10 READ J:I=J:FOR I=J TO I+3:PRINT I;:NEXT:PRINT I\n20 DATA 5,.5\n",
        Some(true),
    );
    check(
        "10 DEF FN F(X)=I*2:FOR K=1 TO 2:FOR I=K TO FN F(0):PRINT I;:NEXT:PRINT:NEXT\n",
        None,
    );
    check(
        "10 X=RND(-7):FOR K=1 TO 3:GOSUB 100:NEXT:PRINT RND(1):END\n\
         100 FOR I=1 TO RND(1)*10:NEXT:PRINT I;:RETURN\n",
        Some(true),
    );
}

#[test]
fn bodies_and_tails_run_the_same_in_both_loops() {
    check(
        "10 FOR K=1 TO 2:READ N:GOSUB 100:NEXT:END\n\
         20 DATA 3,1,2,3,4.5,1,2,3,4\n\
         100 FOR I=1 TO N:READ D:S=S+D:POKE 1024+I,D:NEXT:PRINT S;PEEK(1027):RETURN\n",
        Some(true),
    );
    check(
        "10 FOR K=1 TO 2:N=K*2.2:GOSUB 100:NEXT:END\n\
         100 FOR I=1 TO N:PRINT I;:NEXT:IF I>3 THEN PRINT \"BIG\":RETURN\n\
         110 PRINT \"SMALL\":RETURN\n",
        Some(true),
    );
}

#[test]
fn errors_report_the_same_line() {
    check("10 N=0:FOR I=1 TO 10/N:NEXT\n", Some(true));
    check(
        "10 READ N:FOR I=1 TO N:POKE 1024+I,I*100:NEXT\n20 DATA 5,.5\n",
        Some(true),
    );
    check(
        "10 N=4.5:GOSUB 100:N=300:GOSUB 100:END\n\
         100 FOR I=1 TO N:POKE 1024,I:NEXT:PRINT I:RETURN\n",
        Some(true),
    );
}

#[test]
fn loops_that_share_their_line_with_other_loops() {
    // An outer NEXT after the guarded loop, a NEXT closing both, an inner loop, a later loop.
    check(
        "10 FOR K=1 TO 3:READ N:FOR I=1 TO N:PRINT I;:NEXT:PRINT:NEXT\n20 DATA 2,3.5,-1\n",
        Some(true),
    );
    check(
        "10 FOR K=1 TO 3:READ N:FOR I=1 TO N:PRINT I;:NEXT I,K:PRINT K\n20 DATA 2,3.5,-1\n",
        Some(true),
    );
    check(
        "10 READ N:FOR I=1 TO N:FOR J=1 TO 2:PRINT I*J;:NEXT J,I:PRINT I;J\n20 DATA 3.5,1\n",
        Some(true),
    );
    check(
        "10 READ N:FOR Z=1 TO N:NEXT Z:PRINT Z:FOR Z=1 TO 20:NEXT Z:PRINT Z\n20 DATA 7.5,1\n",
        Some(true),
    );
    check(
        "10 FOR K=1 TO 3\n20 READ N:FOR I=1 TO N:PRINT I;:NEXT:NEXT:PRINT K\n30 DATA 2,3.5,-1\n",
        Some(true),
    );
}

#[test]
fn one_loop_per_line_takes_the_guard() {
    check(
        "10 READ N,M:FOR I=1 TO N:FOR J=1 TO M:PRINT I*J;:NEXT:PRINT:NEXT:PRINT I;J\n\
         20 DATA 2.5,3.5\n",
        Some(true),
    );
    check(
        "10 READ N,M:FOR I=1 TO N:PRINT I;:NEXT:FOR J=1 TO M:PRINT J;:NEXT:PRINT I;J\n\
         20 DATA 2.5,3.5\n",
        Some(true),
    );
}
