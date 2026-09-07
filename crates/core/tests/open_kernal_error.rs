//! `OPEN` must raise the BASIC error the KERNAL reported.

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

fn asm_of(src: &str, profile: Profile) -> String {
    let prg = tokenize_program(src).expect("tokenize");
    compile_with_options(
        &prg,
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .expect("compiles")
    .asm
}

/// The instruction that follows `JSR $FFC0` must be the carry test.
fn carry_is_tested_after(asm: &str, after: &str) -> bool {
    let mut lines = asm.lines().map(str::trim);
    while let Some(l) = lines.next() {
        if l == after {
            return lines.next().is_some_and(|n| n.starts_with("BCC "));
        }
    }
    false
}

#[test]
fn open_checks_the_kernal_carry_at_every_profile() {
    for profile in PROFILES {
        let asm = asm_of("10 OPEN 2,8,2,\"0:X,S,R\"\n20 PRINT\"AFTER\"\n", profile);
        assert!(
            carry_is_tested_after(&asm, "JSR $FFC0"),
            "{profile:?}: the KERNAL OPEN status must be tested:\n{asm}"
        );

        assert!(
            asm.contains("    TAX\n"),
            "{profile:?}: the status must move to .X:\n{asm}"
        );
        assert!(
            asm.contains("LDX #$1E"),
            "{profile:?}: a zero status is BREAK, the `$E107` arm:\n{asm}"
        );
        assert!(
            asm.contains("JMP $A437"),
            "{profile:?}: the error must go to BASIC's ERROR routine:\n{asm}"
        );
    }
}

#[test]
fn a_successful_open_still_falls_through_to_the_next_statement() {

    for profile in PROFILES {
        let asm = asm_of("10 OPEN 2,8,2,\"0:X,S,R\"\n20 PRINT\"AFTER\"\n", profile);
        let bcc = asm
            .lines()
            .map(str::trim)
            .filter(|l| l.starts_with("BCC __OPEN_OK_"))
            .count();
        assert_eq!(
            bcc, 1,
            "{profile:?}: exactly one OPEN carry branch expected:\n{asm}"
        );
        let ok_label = asm
            .lines()
            .map(str::trim)
            .find(|l| l.starts_with("__OPEN_OK_") && l.ends_with(':'))
            .unwrap_or_else(|| panic!("{profile:?}: the success label must exist:\n{asm}"));

        let label_at = asm.find(ok_label).expect("label present");
        let after_at = asm.find("AFTER").or_else(|| asm.find("$41,$46,$54"));
        if let Some(after_at) = after_at {
            assert!(
                label_at < after_at,
                "{profile:?}: the success path must continue into line 20:\n{asm}"
            );
        }
    }
}

#[test]
fn a_program_without_open_pays_nothing() {

    let asm = asm_of("10 PRINT\"HI\"\n", Profile::Default);
    assert!(
        !asm.contains("__OPEN_OK_"),
        "no OPEN, no carry check:\n{asm}"
    );
}
