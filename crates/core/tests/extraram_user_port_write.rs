mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn build(src: &str, extraram: bool) -> yabcompiler_core::Compiled {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(
        &prg,
        CompileOptions {
            profile: Profile::Default,
            extraram,
            force_extraram_off: !extraram,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compiles (extraram={extraram}): {e}\n{src}"))
}

/// Instruction lines, trimmed, with blanks and comments dropped ; so a "immediately precedes" claim is about code and not about layout.
fn code_lines(asm: &str) -> Vec<&str> {
    asm.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with(';'))
        .collect()
}

/// Indices of every `STA $0001` (the four-digit, user-`POKE` spelling).
fn user_port_stores(lines: &[&str]) -> Vec<usize> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, l)| **l == "STA $0001")
        .map(|(i, _)| i)
        .collect()
}

const MASK: &str = "AND #$FE";

#[test]
fn a_user_port_write_is_masked_under_extraram() {
    let asm = build("10 POKE 1,51\n20 PRINT\"OK\"\n", true).asm;
    let lines = code_lines(&asm);
    let at = user_port_stores(&lines);
    assert_eq!(at.len(), 1, "expected exactly one user port write:\n{asm}");
    assert_eq!(
        lines[at[0] - 1],
        MASK,
        "the store is not masked — LORAM would bank the BASIC ROM over the image"
    );
}

/// End to end: the byte that lands in `$01` has bit 0 clear.
#[test]
fn the_masked_value_is_what_reaches_the_port() {

    let src = "10 POKE 1,51\n20 A=PEEK(1)\n30 POKE 1,55\n40 PRINT A\n";
    let masked = build(src, true).prg_bytes;
    assert_eq!(
        common::run_clean(&masked).expect("extraram build runs"),
        " 50 \n",
        "$01 must read back with LORAM clear under extraram"
    );
    // The interpreter's own answer, and what the extraram-off layout still gives, is the unmasked 51.
    let plain = build(src, false).prg_bytes;
    assert_eq!(
        common::run_clean(&plain).expect("plain build runs"),
        " 51 \n",
        "without extraram nothing owns LORAM and the write is the user's"
    );
}

#[test]
fn every_user_port_write_in_a_program_is_masked() {
    let asm = build(
        "10 POKE 1,51\n20 FOR I=0 TO 3:POKE 32768+I,PEEK(53248+I):NEXT\n30 POKE 1,55\n",
        true,
    )
    .asm;
    let lines = code_lines(&asm);
    let at = user_port_stores(&lines);
    assert_eq!(at.len(), 2, "expected both port writes:\n{asm}");
    for i in at {
        assert_eq!(lines[i - 1], MASK, "an unmasked port write survived:\n{asm}");
    }
}

#[test]
fn a_user_port_write_costs_nothing_without_extraram() {
    let asm = build("10 POKE 1,51\n20 PRINT\"OK\"\n", false).asm;
    let lines = code_lines(&asm);
    let at = user_port_stores(&lines);
    assert_eq!(at.len(), 1, "expected exactly one user port write:\n{asm}");
    assert_ne!(
        lines[at[0] - 1],
        MASK,
        "the extraram-off layout must not pay for the mask"
    );
    assert_eq!(
        lines[at[0] - 1],
        "LDA #$33",
        "the literal path should still be a bare LDA/STA pair"
    );
}

#[test]
fn the_compilers_own_port_writes_are_never_masked() {
    let asm = build("10 X=PEEK(53248)\n20 PRINT X\n", true).asm;
    let lines = code_lines(&asm);
    let own: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| **l == "STA $01")
        .map(|(i, _)| i)
        .collect();
    assert!(
        !own.is_empty(),
        "fixture should contain compiler-owned port writes:\n{asm}"
    );
    for i in own {
        assert_ne!(
            lines[i - 1], MASK,
            "a compiler-owned port write was masked:\n{asm}"
        );
    }
}

#[test]
fn a_poke_to_a_neighbouring_address_is_never_masked() {
    let asm = build("10 POKE 0,47:POKE 2,51:POKE 53280,5\n", true).asm;
    assert!(
        !code_lines(&asm).contains(&MASK),
        "only the write to $01 may be masked:\n{asm}"
    );
}

#[test]
fn the_char_rom_idiom_still_runs_to_completion() {
    let src = "10 POKE 1,51\n\
               20 FOR I=0 TO 7:POKE 32768+I,PEEK(53248+I):NEXT\n\
               30 POKE 1,55\n\
               40 S=0:FOR I=0 TO 7:S=S+PEEK(32768+I):NEXT\n\
               50 PRINT\"S\";S\n";
    let with = common::run_clean(&build(src, true).prg_bytes).expect("extraram build runs");
    let without = common::run_clean(&build(src, false).prg_bytes).expect("plain build runs");
    assert_eq!(
        with, without,
        "masking LORAM changed what the program computes"
    );
    assert!(
        with.starts_with("S "),
        "the program must reach its own PRINT: {with:?}"
    );
}
