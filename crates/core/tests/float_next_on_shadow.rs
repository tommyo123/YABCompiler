//! NEXT steps and compares integer copies before updating float counters.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

type Outcome = (Vec<u8>, Option<u8>, Option<u16>);

const BUDGET: u64 = 20_000_000;

fn interpreted(src: &str) -> Outcome {
    let prg = tokenize_program(src).expect("tokenizes");
    let r = emu64::run_basic_to_end(&prg, BUDGET).expect("runs in the interpreter");
    assert!(r.ready_reached, "the interpreter reaches READY:\n{src}");
    (r.output, r.error_code, r.error_line)
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
    let r = emu64::run_prg_to_end(&c.prg_bytes, BUDGET).expect("runs");
    assert!(
        r.clean_exit || r.error_code.is_some(),
        "ends ({profile:?}):\n{src}"
    );
    ((r.output, r.error_code, r.error_line), c.asm)
}

fn same_as_the_rom(src: &str) {
    let want = interpreted(src);
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let (got, _) = compiled(src, profile);
        assert_eq!(
            (common::petscii_to_string(&got.0), got.1, got.2),
            (common::petscii_to_string(&want.0), want.1, want.2),
            "{profile:?}:\n{src}"
        );
    }
}

#[test]
fn search_loops_with_a_signed_shadow() {
    let src = "\
10 FOR X=1144 TO 1823:POKE X,32:NEXT:FOR K=1 TO 40:X=INT(K*16.7)+1144:POKE X,1:NEXT
20 C=0:FOR X=1144 TO 1823:IF PEEK(X)<>1 THEN NEXT X
30 IF PEEK(X)=1 THEN POKE X,3:C=C+1:NEXT X
40 PRINT X;C
50 FOR X=1823 TO 1144 STEP -1:IF PEEK(X)<>3 THEN NEXT X
60 PRINT X:FOR X=1144 TO 1823 STEP 7:IF PEEK(X)<>3 THEN NEXT
70 PRINT X;PEEK(1144)
";
    same_as_the_rom(src);
    assert!(compiled(src, Profile::Default).1.contains("__FOR_SGN_"));
}

#[test]
fn search_loops_past_32767() {
    let src = "\
10 FOR X=49152 TO 49400:POKE X,0:NEXT:FOR K=1 TO 9:X=INT(K*26.3)+49152:POKE X,1:NEXT
20 C=0:FOR X=49152 TO 49400:IF PEEK(X)<>1 THEN NEXT X
30 IF PEEK(X)=1 THEN POKE X,3:C=C+1:NEXT X
40 PRINT X;C:FOR X=49152 TO 49400 STEP 3:IF PEEK(X)<>3 THEN NEXT X
50 PRINT X
";
    same_as_the_rom(src);
    assert!(compiled(src, Profile::Default).1.contains("BCC __FOR_EXIT"));
}

#[test]
fn frames_reached_through_gosub() {
    same_as_the_rom(
        "\
10 FOR X=1024 TO 1030:IF PEEK(X)<>99 THEN NEXT X:GOTO 40
20 PRINT \"F\";X
30 NEXT X
40 PRINT X
50 FOR X=1024 TO 1026:FOR Y=1 TO 2:GOSUB 100:NEXT Y:IF PEEK(X)<>99 THEN NEXT X
60 PRINT X;Y:END
100 FOR X=1500 TO 1502:IF PEEK(X)<>99 THEN NEXT X
110 RETURN
",
    );
}
