//! Address sums combine cached float bases with integer FOR counters.

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
fn a_loader_loop_uses_the_cached_base() {
    let asm = check(
        "10 READ L:L=L/2*2:IF L<0 THEN PRINT S:END\n\
         20 FOR I=0 TO 7:READ A:POKE L+I,A:S=S+PEEK(I+L):NEXT:GOTO 10\n\
         30 DATA 828,1,2,3,4,5,6,7,8,900,9,8,7,6,5,4,3,2,836,10,20,30,40,50,60,70,80,-1\n",
    );
    assert!(asm.contains("JSR __ACI_0"), "{asm}");
}

/// Fills the cassette buffer, so every byte a test reads there is known.
const FILL: &str = "1 FOR K=828 TO 1019:POKE K,K AND 255:NEXT\n";

#[test]
fn bases_that_are_not_whole_or_overflow_keep_the_rom_result() {
    // In the filled buffer, and in the KERNAL ROM up to and past 65535.
    for base in [
        "828", "828.5", "828.999", "830.25", "900.0001", "57344.5", "60000", "65530", "65528",
        "65535", "-1",
    ] {
        check(&format!(
            "{FILL}10 L={base}:L=L/2*2\n20 FOR I=0 TO 7:A=PEEK(L+I):PRINT A;:NEXT:PRINT I\n"
        ));
    }
    // POKE only where the bytes are free.
    for base in ["828.5", "828.999", "830.25", "827"] {
        check(&format!(
            "{FILL}10 L={base}:L=L/2*2\n20 FOR I=1 TO 9 STEP 2:POKE L+I,I:NEXT:PRINT I;PEEK(830);PEEK(836)\n"
        ));
    }
}

#[test]
fn a_sum_that_rounds_up_reads_where_the_rom_does() {
    // The rounded sum needs 33 mantissa bits and differs from the integer-base sum.
    check(
        "10 FOR K=32760 TO 32775:POKE K,K-32700:NEXT:READ D:L=32768-1/D\n\
         20 FOR I=0 TO 3:PRINT PEEK(L+I);:NEXT:PRINT\n30 DATA 131072\n",
    );
}

#[test]
fn a_base_that_changes_between_loops_is_read_again() {
    check(
        "10 FOR K=0 TO 5:L=820+K*1.5:FOR I=0 TO 3:POKE I+L,K+I:NEXT:NEXT\n\
         20 FOR I=820 TO 835:PRINT PEEK(I);:NEXT\n",
    );
    check(&format!(
        "{FILL}10 L=1000/2*2-100:FOR I=0 TO 90:S=S+PEEK(L+I):NEXT:PRINT S;I\n\
         20 L=6000/7:FOR I=0 TO 150 STEP 7:S=S+PEEK(L+I):NEXT:PRINT S;I\n"
    ));
}
