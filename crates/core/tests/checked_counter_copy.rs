//! Checked 16-bit copies of float counters and their float fallbacks.

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

fn check(src: &str) -> String {
    let want = interpreted(src);
    let mut speed = String::new();
    for profile in [Profile::Speed, Profile::Default, Profile::Size] {
        for extraram in [false, true] {
            let (got, asm) = compiled(src, profile, extraram);
            assert_eq!(got, want, "{profile:?} extraram {extraram}:\n{src}");
            if profile == Profile::Speed && !extraram {
                speed = asm;
            }
        }
    }
    speed
}

#[test]
fn counters_cross_the_edges_of_their_copies() {
    for (start, step) in [
        ("65530", "+1"),
        ("65533", "+7"),
        ("2", "-1"),
        ("32760", "+3"),
        ("-32765", "-7"),
        ("0", "+1000"),
        ("-3", "+1"),
    ] {
        // P is an address, so its copy is unsigned; S is a plain counter with a signed copy.
        let asm = check(&format!(
            "10 P={start}:S={start}:G=0\n\
             20 P=P{step}:S=S{step}:G=G+1:IF G<12 THEN 20\n\
             30 PRINT P;S;P/3;S-1\n\
             40 IF P>=0 AND P<=65535 THEN POKE 828,P AND 255:PRINT PEEK(828)\n\
             50 IF P<1 THEN END\n\
             60 POKE P-1,PEEK(P-1):PRINT \"OK\"\n"
        ));
        assert!(asm.contains("__CS_SLOW_"), "{asm}");
    }
}

#[test]
fn a_counter_stays_exact_through_compares_and_addresses() {
    let asm = check(
        "10 AD=828:N=0\n\
         20 POKE AD,N:AD=AD+1:N=N+3:IF AD<892 THEN 20\n\
         30 S=0:AD=828\n\
         40 S=S+PEEK(AD):AD=AD+1:IF AD<>892 THEN 40\n\
         50 PRINT S;AD;N\n\
         60 Z=0\n\
         70 Z=Z+1:IF Z<1500 THEN 70\n\
         80 PRINT Z;Z*2;Z/7\n",
    );
    assert!(asm.contains("JSR __CS_ST_"), "{asm}");
}

#[test]
fn fractions_reads_and_gets_hand_back_to_float() {
    for value in [
        "7", "1.5", "-1", "32768", "65535", "70000", "-32769", ".25", "1E9",
    ] {
        check(&format!(
            "10 READ C:G=0\n\
             20 C=C+1:D=C:E=C+100:G=G+1:IF G<5 THEN 20\n\
             30 PRINT C;D;E\n\
             40 C=7:G=0\n\
             50 C=C-2:G=G+1:IF G<4 THEN 50\n\
             60 PRINT C:GET C:PRINT C\n\
             70 C=C+1:IF C<3 THEN 70\n\
             80 PRINT C:DATA {value}\n"
        ));
    }
}

#[test]
fn clr_and_run_restart_the_copies_at_zero() {
    check(
        "10 IF PEEK(828)=7 THEN 40\n\
         20 C=5\n\
         30 C=C+1:IF C<9 THEN 30\n\
         35 POKE 828,7:PRINT C:RUN\n\
         40 C=C+1:IF C<3 THEN 40\n\
         50 PRINT C:C=40:CLR\n\
         60 C=C+1:IF C<2 THEN 60\n\
         70 PRINT C:POKE 828,0\n",
    );
}

#[test]
fn for_loops_over_a_counter_leave_its_copy_stale() {
    let asm = check(
        "10 I=0\n\
         20 I=I+1:IF I<5 THEN 20\n\
         30 PRINT I\n\
         40 FOR I=1 TO 6:GOSUB 200:NEXT I:PRINT I\n\
         45 FOR I=1 TO 3 STEP 2:GOSUB 210:NEXT:PRINT I\n\
         50 FOR I=1 TO 4\n\
         60 IF I=2 THEN 100\n\
         70 NEXT I:GOTO 110\n\
         100 I=7:GOTO 70\n\
         110 PRINT I\n\
         120 I=I+1:IF I<12 THEN 120\n\
         130 PRINT I:FOR I=65530 TO 65535:NEXT:PRINT I\n\
         140 I=I+1:IF I<65540 THEN 140\n\
         150 PRINT I:END\n\
         200 I=I+1:RETURN\n\
         210 I=2:RETURN\n",
    );
    assert!(asm.contains("__CS_SLOW_"), "{asm}");
}
