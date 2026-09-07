mod common;

use yabcompiler_core::{CompileOptions, compile_with_options, tokenize_program};

/// Six bytes of position-independent 6502: `LDA #'A' / JSR CHROUT / RTS`.
const ML_PRINTS_A: &str = "CHR$(169)+CHR$(65)+CHR$(32)+CHR$(210)+CHR$(255)+CHR$(96)";

fn compile(src: &str) -> (String, Vec<u8>) {
    let prg = tokenize_program(src).expect("tokenize");
    let c = compile_with_options(&prg, CompileOptions::default()).expect("compile");
    (c.asm, c.prg_bytes)
}

/// Every instruction in the generated assembly that writes the FRETOP pair.
fn fretop_stores(asm: &str) -> Vec<String> {
    asm.lines()
        .map(str::trim)
        .filter(|l| {
            (l.starts_with("STA ") || l.starts_with("STX ") || l.starts_with("STY "))
                && (l.ends_with("$33") || l.ends_with("$34"))
        })
        .map(str::to_string)
        .collect()
}

/// How many instructions in the program body write the low byte of a string variable's cell ; one per surviving `LET` to that variable.
fn var_cell_writes(asm: &str, label: &str) -> usize {
    let sta = format!("STA {label}");
    let stx = format!("STX {label}");
    asm.lines()
        .map(str::trim)
        .take_while(|l| !l.starts_with("; --- helper subroutines"))
        .filter(|l| *l == sta || *l == stx)
        .count()
}

#[test]
fn the_sys_reaches_machine_code_carried_in_a_string() {
    let src = format!(
        "10 ML$={ML_PRINTS_A}\n\
         20 ML$=ML$\n\
         30 SYS(PEEK(51)+256*PEEK(52))\n\
         40 PRINT\"Z\"\n"
    );
    let (asm, prg) = compile(&src);
    assert_eq!(
        fretop_stores(&asm).len(),
        2,
        "expected one lo + one hi store at the self-assignment, got {:?}",
        fretop_stores(&asm)
    );
    let out = common::run_clean(&prg).expect("image must reach a clean exit, not a JAM");
    assert_eq!(
        out, "AZ\n",
        "the `A` is printed by the six bytes carried in ML$; without it the SYS \
         never reached the string"
    );
}

#[test]
fn without_the_self_assignment_the_same_program_does_not_reach_the_string() {
    let src = format!(
        "10 ML$={ML_PRINTS_A}\n\
         30 SYS(PEEK(51)+256*PEEK(52))\n\
         40 PRINT\"Z\"\n"
    );
    let (asm, prg) = compile(&src);
    assert!(
        fretop_stores(&asm).is_empty(),
        "nothing asked for FRETOP to move, so nothing may write it: {:?}",
        fretop_stores(&asm)
    );
    assert_ne!(
        common::run_clean(&prg).as_deref(),
        Some("AZ\n"),
        "the ML must not be reachable without the self-assignment"
    );
}

#[test]
fn fretop_is_untouched_without_a_self_assignment() {
    let src = "10 A$=\"ABC\"+\"DEF\"\n\
               20 B$=A$\n\
               30 PRINT PEEK(51)+256*PEEK(52)\n";
    let (asm, prg) = compile(src);
    assert!(
        fretop_stores(&asm).is_empty(),
        "a plain string assignment must not move FRETOP: {:?}",
        fretop_stores(&asm)
    );
    let out = common::run_clean(&prg).expect("runs");
    assert_eq!(
        out.trim(),
        "40960",
        "the boot value of MEMSIZ is what the machine hands these programs"
    );
}

/// The rule is self-assignment, not "any assignment whose right-hand side is a variable".
#[test]
fn a_plain_copy_between_two_variables_is_not_a_self_assignment() {
    let src = "10 A$=\"ABC\"\n\
               20 B$=A$\n\
               30 PRINT PEEK(51)+256*PEEK(52)\n";
    let (asm, _) = compile(src);
    assert!(
        fretop_stores(&asm).is_empty(),
        "B$=A$ is not A$=A$: {:?}",
        fretop_stores(&asm)
    );
}

/// Size: a self-assignment in a program that cannot observe FRETOP is still a dead store and must still be eliminated.
#[test]
fn a_self_assignment_is_still_dropped_when_fretop_is_never_read() {
    let unobserved = "10 A$=\"ABC\"+\"DEF\"\n\
                      20 A$=A$\n\
                      30 PRINT\"Z\"\n";
    let observed = "10 A$=\"ABC\"+\"DEF\"\n\
                    20 A$=A$\n\
                    30 PRINT PEEK(51)+256*PEEK(52)\n";

    let (asm_unobserved, _) = compile(unobserved);
    assert!(
        fretop_stores(&asm_unobserved).is_empty(),
        "no PEEK(51)/PEEK(52) in this program, so no store: {:?}",
        fretop_stores(&asm_unobserved)
    );
    assert_eq!(
        var_cell_writes(&asm_unobserved, "VS_A"),
        1,
        "`A$=A$` must still be dropped as a dead store when nothing can \
         observe FRETOP\n{asm_unobserved}"
    );

    let (asm_observed, _) = compile(observed);
    assert_eq!(
        var_cell_writes(&asm_observed, "VS_A"),
        2,
        "with FRETOP readable the self-assignment must survive to make its \
         store\n{asm_observed}"
    );
    assert_eq!(fretop_stores(&asm_observed).len(), 2);
}

#[test]
fn reading_only_the_high_byte_still_arms_the_gate() {
    let src = format!(
        "10 ML$={ML_PRINTS_A}\n\
         20 ML$=ML$\n\
         30 PRINT PEEK(52)\n"
    );
    let (asm, _) = compile(&src);
    assert_eq!(
        fretop_stores(&asm).len(),
        2,
        "PEEK(52) alone must arm the gate: {:?}",
        fretop_stores(&asm)
    );
}

/// The stored address is the characters, not the chunk header.
#[test]
fn the_stored_address_names_the_characters_not_the_length_byte() {
    let src = format!(
        "10 ML$={ML_PRINTS_A}\n\
         20 ML$=ML$\n\
         30 A=PEEK(51)+256*PEEK(52)\n\
         40 SYS A\n\
         50 PRINT LEN(ML$)\n"
    );
    let (_, prg) = compile(&src);
    let out = common::run_clean(&prg).expect("image must reach a clean exit");
    assert_eq!(
        out, "A 6 \n",
        "`A` from the routine, then the length of the string it lives in \
         (BASIC prints a leading sign blank and a trailing space)"
    );
}
