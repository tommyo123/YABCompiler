//! Accumulate bounded integer terms in 24 bits and store the float sum at loop exit.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

type Outcome = (Vec<u8>, Option<u8>, Option<u16>);

const BUDGET: u64 = 40_000_000;

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

fn sums_in_a_slot(src: &str) -> bool {
    compiled(src, Profile::Speed).1.contains("\n__I24_FAC:")
}

#[test]
fn a_loader_checksum_past_32767() {
    let mut src =
        String::from("10 FOR J=49152 TO 49551:READ A:POKE J,A:X=X+A:NEXT J\n20 PRINT X;J\n");
    for k in 0..25 {
        let items: Vec<String> = (0..16)
            .map(|i| ((k * 16 + i) * 37 % 256).to_string())
            .collect();
        src.push_str(&format!("{} DATA {}\n", 100 + k, items.join(",")));
    }
    same_as_the_rom(&src);
    assert!(sums_in_a_slot(&src));
    assert!(!compiled(&src, Profile::Default).1.contains("__I24_FAC:"));
}

#[test]
fn negative_values_and_subtraction() {
    let src = "\
10 X=-5000:FOR I=1 TO 300:X=X+200:NEXT:PRINT X
20 FOR I=1 TO 500:Y=Y-I:NEXT:PRINT Y;I
30 X=1000000:FOR I=1 TO 2:X=X-30000:NEXT:PRINT X
40 FOR I=1 TO 10:E=E-4000:NEXT:PRINT E;STR$(E);E/3
";
    same_as_the_rom(src);
    assert!(sums_in_a_slot(src));
}

#[test]
fn sums_under_if_and_several_terms() {
    let src = "\
10 FOR I=1 TO 1000:IF I AND 1 THEN Z=Z+I
20 NEXT:PRINT Z
30 FOR I=1 TO 300:S=S+I:T=T+2*I+1:NEXT:PRINT S;T
40 FOR I=1 TO 400:V=I+V:NEXT:PRINT V
50 FOR I=40000 TO 40100:W=W+PEEK(I)+1:NEXT:PRINT W
60 FOR I=1 TO 900:RESTORE:READ Q,Q6,Q7:CK=CK+Q6+Q7:NEXT:PRINT CK
70 DATA 12345,255,77
";
    same_as_the_rom(src);
    assert!(sums_in_a_slot(src));
}

#[test]
fn a_sum_read_inside_its_loop_stays_a_float() {
    let src = "\
10 FOR I=1 TO 300:U=U+I:PRINT U;:NEXT:PRINT
20 FOR I=1 TO 200:C=C+I:IF C>15000 THEN C=0
30 NEXT:PRINT C
40 FOR I=1 TO 300:A=A+I:B=A:NEXT:PRINT A;B
";
    same_as_the_rom(src);
    assert!(!sums_in_a_slot(src));
}

#[test]
fn a_jump_into_the_loop_keeps_the_float_sum() {
    let src = "\
10 GOTO 30
20 FOR I=1 TO 300:S=S+I
30 S=S+1000
40 NEXT:PRINT S
";
    same_as_the_rom(src);
}

#[test]
fn sums_with_ram_under_the_basic_rom() {
    let src = "10 FOR I=1 TO 500:S=S+I:NEXT:PRINT S\n";
    let want = interpreted(src);
    let prg = tokenize_program(src).expect("tokenizes");
    let c = compile_with_options(
        &prg,
        CompileOptions {
            profile: Profile::Speed,
            extraram: true,
            ..CompileOptions::default()
        },
    )
    .expect("compiles");
    assert!(c.asm.contains("\n__I24_FAC:"));
    let r = emu64::run_prg_to_end(&c.prg_bytes, BUDGET).expect("runs");
    assert_eq!(
        common::petscii_to_string(&r.output),
        common::petscii_to_string(&want.0)
    );
}
