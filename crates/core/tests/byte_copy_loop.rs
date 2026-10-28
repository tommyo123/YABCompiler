//! Consecutive byte copies lowered from FOR loops.

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
fn a_copy_across_pages_with_different_low_bytes() {
    let src = "\
10 FOR I=0 TO 300:POKE 49152+I,I AND 255:NEXT
20 FOR N=0 TO 300:POKE N+40000,PEEK(N+49152):NEXT N
30 PRINT N;PEEK(40000);PEEK(40300);PEEK(40255);PEEK(40256)
";
    same_as_the_rom(src);
    assert!(compiled(src, Profile::Default).1.contains("STA $FF00,Y"));
}

#[test]
fn an_overlapping_copy_runs_in_loop_order() {
    same_as_the_rom(
        "\
10 POKE 49152,7:FOR N=49152 TO 49160:POKE N+1,PEEK(N):NEXT
20 PRINT N;PEEK(49152);PEEK(49161)
",
    );
}

#[test]
fn a_start_past_the_end_copies_once() {
    same_as_the_rom(
        "10 POKE 49157,9:FOR N=5 TO 3:POKE N+40000,PEEK(N+49152):NEXT:PRINT N;PEEK(40005)\n",
    );
}

#[test]
fn negative_offsets_and_the_top_of_memory() {
    same_as_the_rom(
        "\
10 FOR N=40100 TO 40110:POKE N-100,PEEK(N):NEXT:PRINT PEEK(40000);N
20 FOR N=65530 TO 65535:POKE N,PEEK(N-30000):NEXT:PRINT N
",
    );
}

#[test]
fn the_same_offset_in_both_pages_shares_one_index() {
    let src = "\
10 FOR I=0 TO 599:POKE 49152+I,(I*7) AND 255:NEXT
20 FOR N=0 TO 511:POKE N+50176,PEEK(N+49152):NEXT
30 FOR N=37 TO 300:POKE N+50688,PEEK(N+49152):NEXT
40 FOR N=10 TO 20:POKE N+51200,PEEK(N+49152):NEXT
50 FOR N=0 TO 299:POKE N+51456,PEEK(N+49152):NEXT
60 S=0:FOR J=0 TO 1583:S=S+PEEK(50176+J)*((J AND 3)+1):NEXT:PRINT S;N
";
    same_as_the_rom(src);
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let asm = compiled(src, profile).1;
        assert!(asm.contains("LDA $FF00,Y"), "{profile:?}:\n{asm}");
        assert!(
            asm.contains("CPY #$15"),
            "{profile:?}: the line 40 copy stays in one page"
        );
    }
}

#[test]
fn the_poke_and_next_on_the_next_line() {
    let src = "\
10 FOR I=0 TO 600:POKE 49152+I,I AND 255:NEXT
20 FOR X=0 TO 511
30 POKE 51200+X,PEEK(49152+X):NEXT
40 PRINT X;PEEK(51200);PEEK(51711);PEEK(51712)
";
    same_as_the_rom(src);
    assert!(compiled(src, Profile::Default).1.contains("__CPS_"));
}

#[test]
fn a_jump_to_the_poke_line_keeps_the_loop() {
    let src = "\
10 FOR I=0 TO 9:POKE 49152+I,I:NEXT:X=3:GOTO 30
20 FOR X=0 TO 5
30 POKE 51200+X,PEEK(49152+X):NEXT
40 PRINT X;PEEK(51203);PEEK(51205)
";
    same_as_the_rom(src);
    assert!(!compiled(src, Profile::Default).1.contains("__CPS_"));
}
