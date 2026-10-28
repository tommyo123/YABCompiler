//! Constant bases inside POKE and PEEK address sums.

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
fn a_character_copy_with_the_base_in_a_variable() {
    let src = "\
10 V=49152:FOR I=0 TO 31:POKE V+I,I*3:NEXT
20 FOR I=0 TO 3:FOR J=0 TO 7:POKE 14336+I*8+J,PEEK(V+I*8+J):NEXT J,I
30 S=0:FOR I=0 TO 31:S=S+PEEK(14336+I):NEXT:PRINT S
";
    same_as_the_rom(src);
    let (_, asm) = compiled(src, Profile::Default);
    assert!(
        !asm.contains("JSR $BA28"),
        "no FMULT for the address:\n{asm}"
    );
}

#[test]
fn literal_terms_anywhere_in_the_sum() {
    same_as_the_rom(
        "\
10 FOR I=0 TO 9:POKE 40000+I-3+I,I+1:NEXT:PRINT PEEK(39997);PEEK(40015)
20 W=49152:FOR I=0 TO 9:POKE W+I*2+1,I:NEXT:PRINT PEEK(49153);PEEK(49171)
",
    );
}

#[test]
fn an_address_past_65535_is_still_illegal_quantity() {
    same_as_the_rom("10 FOR I=0 TO 3:X=PEEK(65530+I*3):NEXT\n");
    same_as_the_rom("10 V=65000:FOR I=0 TO 3:POKE V+I*200+1,0:NEXT\n");
}
