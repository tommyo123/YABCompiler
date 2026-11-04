//! Dead byte-counter stores across shared slots and control-flow boundaries.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

type Outcome = (String, Option<u8>, Option<u16>);

fn interpreted(src: &str) -> Outcome {
    let prg = tokenize_program(src).expect("tokenizes");
    let r = emu64::run_basic_to_end(&prg, 100_000_000).expect("runs in the interpreter");
    (
        common::petscii_to_string(&r.output),
        r.error_code,
        r.error_line,
    )
}

fn compiled(src: &str, profile: Profile) -> (Outcome, String) {
    let prg = tokenize_program(src).expect("tokenizes");
    let c = compile_with_options(
        &prg,
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}\n{src}"));
    let r = emu64::run_prg_to_end(&c.prg_bytes, 100_000_000).expect("runs");
    (
        (
            common::petscii_to_string(&r.output),
            r.error_code,
            r.error_line,
        ),
        c.asm,
    )
}

fn check(src: &str) -> Vec<String> {
    let want = interpreted(src);
    [Profile::Default, Profile::Speed, Profile::Size]
        .into_iter()
        .map(|profile| {
            let (got, asm) = compiled(src, profile);
            assert_eq!(got, want, "{profile:?}:\n{src}");
            asm
        })
        .collect()
}

#[test]
fn a_delay_loop_keeps_no_counter_in_memory() {
    let src = "10 PRINT \"Q\";:FOR A=1 TO 50:NEXT:PRINT \"X\";:FOR B=1 TO 9:NEXT\n\
               20 GET K$:IF K$=\"\" THEN C=C+1:IF C<3 THEN 10\n30 A=A/3:PRINT A;B\n";
    for asm in check(src) {
        let top = asm.find("__FOR_TOP_0:").expect("first loop");
        let exit = asm.find("__FOR_EXIT_0:").expect("first loop exit");
        assert!(!asm[top..exit].contains("FU_"), "{}", &asm[top..exit]);
    }
}

#[test]
fn loops_sharing_a_cell_read_their_own_counter() {
    check("10 FOR I=1 TO 3:FOR J=1 TO 4:POKE 828+J,I:NEXT:PRINT PEEK(829+I);:NEXT\n20 PRINT I;J\n");
    check("10 FOR I=1 TO 5:NEXT:FOR J=2 TO 7:POKE 828,J:NEXT:PRINT PEEK(828);I;J\n");
    check(
        "10 FOR I=1 TO 9:IF I=4 THEN 30\n20 NEXT\n30 PRINT I:FOR J=0 TO 2:POKE 828+J,I*J:NEXT:PRINT PEEK(830)\n",
    );
}

#[test]
fn subroutines_and_early_exits_keep_the_counter() {
    check("10 FOR I=1 TO 5:GOSUB 100:NEXT:PRINT I:END\n100 POKE 828,I:PRINT PEEK(828);:RETURN\n");
    check(
        "10 GOSUB 100:PRINT I:FOR J=1 TO 3:GOSUB 100:NEXT:PRINT J:END\n100 FOR I=1 TO 4:NEXT:RETURN\n",
    );
    check("10 FOR I=1 TO 9:POKE 828,I:IF PEEK(828)=6 THEN GOTO 30\n20 NEXT\n30 PRINT I\n");
    check("10 FOR I=1 TO 3:FOR J=1 TO 3:IF J=2 THEN NEXT I\n20 NEXT J:PRINT I;J\n");
}

#[test]
fn a_run_time_next_finds_its_loop() {
    check(
        "10 FOR I=1 TO 3:GOSUB 50:NEXT:PRINT \"E\";I:END\n50 FOR J=1 TO 2:PRINT I;J;:NEXT:RETURN\n",
    );
    check(
        "10 FOR I=1 TO 4:ON I GOTO 20,30,20,30\n20 PRINT \"A\";I;:NEXT:GOTO 40\n30 PRINT \"B\";I;:NEXT\n40 PRINT I\n",
    );
}

#[test]
fn variables_read_after_a_return_keep_their_stores() {
    // Set in a subroutine, read by the caller after RETURN.
    check("10 FOR I=1 TO 3:GOSUB 100:PRINT X;:NEXT:END\n100 X=I*2:RETURN\n");
    // Set before a GOSUB and read only inside it, then overwritten.
    check("10 X=5:GOSUB 100:X=7:PRINT X:END\n100 PRINT X;:RETURN\n");
    // ON GOSUB targets read the value.
    check(
        "10 FOR I=1 TO 3:X=I+10:ON I GOSUB 100,110,120:NEXT:END\n\
         100 PRINT X;:RETURN\n110 PRINT X+1;:RETURN\n120 PRINT X+2;:RETURN\n",
    );
    // A value stored in a nested subroutine and read after both return.
    check("10 GOSUB 100:PRINT X:END\n100 X=3:GOSUB 200:RETURN\n200 PRINT X;:X=4:RETURN\n");
    // END inside a subroutine after a store.
    check("10 X=1:GOSUB 100:PRINT X:END\n100 X=2:IF X=2 THEN PRINT X;:END\n110 RETURN\n");
}

/// A scan loop's index lives in X and is never stored.
#[test]
fn a_scan_index_stays_in_x() {
    let src = "10 FOR I=0 TO 9:POKE 828+I,(I*7) AND 15:NEXT\n\
               20 M=10:S=1:W=828\n\
               30 M=M-1:IF M=0 THEN 80\n\
               35 IF S=0 THEN 80\n\
               40 S=0:X=-1\n\
               50 X=X+1:IF X=M THEN 30\n\
               60 C=PEEK(W+X+1):D=PEEK(W+X):IF C<D THEN POKE W+X+1,D:POKE W+X,C:S=1\n\
               70 GOTO 50\n\
               80 FOR I=0 TO 9:PRINT PEEK(828+I);:NEXT\n";
    let asms = check(src);
    let asm = &asms[0];
    assert!(
        !asm.contains("STX VI_X") && !asm.contains("    LDA #$FF\n    STA VI_X\n"),
        "the index is not stored:\n{asm}"
    );
    // Compare D using its original screen cell.
    assert!(asm.contains("CMP $033C,X"), "{asm}");
}

/// A value read back after its source cell changed keeps its variable.
#[test]
fn a_changed_source_is_not_read_again() {
    check(
        "10 FOR I=0 TO 5:POKE 828+I,I*3:NEXT:X=-1\n\
         20 X=X+1:C=PEEK(828+X):POKE 828+X,9:IF C=6 THEN PRINT \"SIX\";\n\
         30 D=PEEK(829+X):POKE 829+X,C:IF D>C THEN PRINT D;\n\
         40 IF X<4 THEN 20\n\
         50 FOR I=0 TO 5:PRINT PEEK(828+I);:NEXT\n",
    );
}

/// Retain stores used by DEF FN bodies entered through a pushed address.
#[test]
fn a_function_body_reads_its_free_variables() {
    check("10 GOTO 50\n20 Q=3:PRINT FNK(5);\n30 Q=10:PRINT FNK(5)\n40 END\n50 DEF FNK(X)=X*Q\n60 GOTO 20\n");
    check("10 DEF FNC(X)=X+I\n20 FOR I=1 TO 3:PRINT FNC(10);:NEXT\n30 I=7:PRINT FNC(1)\n");
}

/// Keep single-block loop counters in X and preserve their exit values.
#[test]
fn simple_loops_count_in_x() {
    let src = "10 FOR I=0 TO 39:POKE 1064+I,I:NEXT\n\
               20 A=0:FOR I=1 TO 5:A=A+I:NEXT:PRINT A;I\n\
               30 FOR J=0 TO 99:POKE 828+J,J AND 7:NEXT:PRINT J;PEEK(830);PEEK(1070)\n";
    let asms = check(src);
    assert!(asms[0].contains("    INX\n    CPX #$28\n"), "{}", asms[0]);
}
