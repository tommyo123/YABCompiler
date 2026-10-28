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
