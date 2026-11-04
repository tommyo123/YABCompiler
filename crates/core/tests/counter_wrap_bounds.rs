//! Check bounds for counters reset by equality tests.
//! Reject bounds invalidated by skipped limits, other writes or alternate paths.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

type Outcome = (String, Option<u8>, Option<u16>);

fn interpreted(src: &str) -> Outcome {
    let prg = tokenize_program(src).expect("tokenizes");
    let r = emu64::run_basic_to_end(&prg, 400_000_000).expect("runs in the interpreter");
    (common::petscii_to_string(&r.output), r.error_code, r.error_line)
}

fn compiled(src: &str, profile: Profile, fast_float: bool) -> (Outcome, String) {
    let prg = tokenize_program(src).expect("tokenizes");
    let c = compile_with_options(&prg, CompileOptions { profile, fast_float, ..CompileOptions::default() })
        .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}\n{src}"));
    let r = emu64::run_prg_to_end(&c.prg_bytes, 400_000_000).expect("runs");
    ((common::petscii_to_string(&r.output), r.error_code, r.error_line), c.asm)
}

fn same_as_the_rom(src: &str) -> String {
    let want = interpreted(src);
    let mut default_asm = String::new();
    for (profile, fast_float) in [
        (Profile::Default, false),
        (Profile::Speed, false),
        (Profile::Size, false),
        (Profile::Default, true),
        (Profile::Speed, true),
    ] {
        let (got, asm) = compiled(src, profile, fast_float);
        assert_eq!(got, want, "{profile:?} fast float {fast_float}:\n{src}");
        if profile == Profile::Default && !fast_float {
            default_asm = asm;
        }
    }
    default_asm
}

/// Counted up and reset, counted down and reloaded: both stay one byte.
#[test]
fn reset_counters_take_one_byte() {
    let asm = same_as_the_rom(
        "10 FOR I=1 TO 1000:C=C+1:IF C=10 THEN C=0:S=S+1\n20 NEXT:PRINT C;S\n",
    );
    assert!(asm.contains("    INC VI_C\n    LDA VI_C\n"), "{asm}");
    assert!(!asm.contains("VI_C+1"), "{asm}");
    let asm = same_as_the_rom(
        "10 T=50:FOR I=1 TO 777:T=T-1:IF T=0 THEN T=50:N=N+1\n20 NEXT:PRINT T;N\n",
    );
    assert!(!asm.contains("VI_T+1"), "{asm}");
    same_as_the_rom("10 FOR I=1 TO 300:F=F+1:IF F<>8 THEN 30\n20 F=0:PRINT I;\n30 NEXT:PRINT F\n");
}

/// Reject bounds when counters exceed byte or integer ranges.
#[test]
fn a_counter_that_passes_its_test_keeps_its_range() {
    // Steps by 2 over an odd limit.
    same_as_the_rom("10 FOR I=1 TO 20000:C=C+2:IF C=9 THEN C=0\n20 NEXT:PRINT C\n");
    // Another write pushes it past the limit.
    same_as_the_rom(
        "10 FOR I=1 TO 600:C=C+1:IF C=10 THEN C=0\n20 IF I=5 THEN C=C+20\n30 NEXT:PRINT C\n",
    );
    // Starts above the limit.
    same_as_the_rom("10 C=300:FOR I=1 TO 500:C=C+1:IF C=10 THEN C=0\n20 NEXT:PRINT C\n");
    // Counts down past its reload test.
    same_as_the_rom("10 T=5:FOR I=1 TO 40000:T=T-3:IF T=0 THEN T=50\n20 NEXT:PRINT T\n");
    // A GOTO loop that leaves through the test.
    same_as_the_rom("10 C=C+1:IF C<>200 THEN 10\n20 PRINT C:C=C+1:IF C=300 THEN END\n30 GOTO 20\n");
}
