//! The prologue clears decimal mode, and this is what keeps it there.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn asm_at(src: &str, profile: Profile) -> String {
    let prg = tokenize_program(src).expect("tokenizes");
    let opts = CompileOptions {
        profile,
        ..CompileOptions::default()
    };
    compile_with_options(&prg, opts).expect("compiles").asm
}

/// Instruction mnemonics in emission order, labels and directives dropped.
fn first_instruction(asm: &str) -> String {
    for raw in asm.lines() {
        let l = match raw.find(" ;") {
            Some(i) => raw[..i].trim(),
            None => raw.trim(),
        };
        if l.is_empty() || l.starts_with(';') || l.starts_with('.') || l.starts_with("*=") {
            continue;
        }
        // `NAME = $XX` zero-page equates emit no bytes.
        if let Some(eq) = l.find('=') {
            let lhs = l[..eq].trim();
            if !lhs.is_empty()
                && !lhs.contains(char::is_whitespace)
                && lhs.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                continue;
            }
        }
        if l.ends_with(':') && !l.contains(char::is_whitespace) {
            continue;
        }
        return l.to_string();
    }
    String::new()
}

/// The minimal case: nothing but a PRINT, so no ZP promotions and no prologue setup to hide
/// behind.
#[test]
fn cld_is_the_very_first_instruction() {
    let asm = asm_at("10 PRINT 1\n", Profile::Default);
    assert_eq!(
        first_instruction(&asm),
        "CLD",
        "decimal mode must be cleared before anything can add:\n{asm}"
    );
}

/// A program that does claim zero page.
#[test]
fn cld_precedes_the_prologue_of_a_zero_page_heavy_program() {
    let src = "\
10 DIM A%(50)
20 FOR I=0 TO 50:A%(I)=50-I:NEXT
30 FOR J=0 TO 49
40 IF A%(J)<=A%(J+1) THEN 60
50 T%=A%(J):A%(J)=A%(J+1):A%(J+1)=T%
60 NEXT J
70 PRINT A%(0)
80 END
";
    let asm = asm_at(src, Profile::Default);
    assert!(
        asm.contains(" = $"),
        "this program is supposed to promote to zero page:\n{asm}"
    );
    assert_eq!(
        first_instruction(&asm),
        "CLD",
        "equates emit no bytes, so CLD is still first:\n{asm}"
    );
}
#[test]
fn cld_survives_the_peephole_at_every_profile() {
    for profile in [Profile::Default, Profile::Size, Profile::Speed] {
        let asm = asm_at("10 A=1+2:PRINT A\n", profile);
        assert_eq!(
            first_instruction(&asm),
            "CLD",
            "CLD dropped at {profile:?}:\n{asm}"
        );
    }
}
#[test]
fn cld_is_emitted_exactly_once() {
    let src = "\
10 FOR I=1 TO 3
20 A=A+I
30 NEXT I
40 PRINT A
50 END
";
    let asm = asm_at(src, Profile::Default);
    let n = asm
        .lines()
        .filter(|l| l.trim().split(" ;").next().unwrap_or("").trim() == "CLD")
        .count();
    assert_eq!(n, 1, "expected a single prologue CLD, found {n}:\n{asm}");
}
#[test]
fn arithmetic_is_binary_end_to_end() {
    let src = "\
10 A=0
20 FOR I=1 TO 9
30 A=A+9
40 NEXT I
50 PRINT A
60 END
";
    let prg = tokenize_program(src).expect("tokenizes");
    let compiled = compile_with_options(&prg, CompileOptions::default()).expect("compiles");
    let r = emu64::run_prg_to_end(&compiled.prg_bytes, 20_000_000).expect("runs");
    assert!(r.clean_exit, "must reach END");
    let out = common::petscii_to_string(&r.output);
    assert!(out.contains("81"), "9*9 must be 81 in binary, got {out:?}");
}
