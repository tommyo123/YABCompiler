//! `LOAD` / `VERIFY` must raise the BASIC error the KERNAL reported.

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

/// The instruction that follows `after` must be the carry test.
fn carry_is_tested_after(asm: &str, after: &str) -> bool {
    let mut lines = asm.lines().map(str::trim);
    while let Some(l) = lines.next() {
        if l == after {
            return lines.next().is_some_and(|n| n.starts_with("BCC "));
        }
    }
    false
}

/// LIST ends execution before later statements.
const LOAD_SRC: &str = "10 LOAD\"RELOCATE EZLIST\",8\n20 PRINT\"AFTER\"\n";
const VERIFY_SRC: &str = "10 VERIFY\"RELOCATE EZLIST\",8\n20 PRINT\"AFTER\"\n";

#[test]
fn load_checks_the_kernal_carry_at_every_profile() {
    for profile in PROFILES {
        let asm = asm_of(LOAD_SRC, profile);
        assert!(
            carry_is_tested_after(&asm, "JSR $FFD5"),
            "{profile:?}: the KERNAL LOAD status must be tested:\n{asm}"
        );
    }
}

#[test]
fn verify_checks_the_kernal_carry_at_every_profile() {
    for profile in PROFILES {
        let asm = asm_of(VERIFY_SRC, profile);
        assert!(
            carry_is_tested_after(&asm, "JSR $FFD5"),
            "{profile:?}: the KERNAL VERIFY status must be tested:\n{asm}"
        );
    }
}

#[test]
fn the_failure_arm_reproduces_the_e0f9_dispatch() {
    for profile in PROFILES {
        let asm = asm_of(LOAD_SRC, profile);
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
fn a_successful_load_still_falls_through_to_the_next_statement() {
    for profile in PROFILES {
        let asm = asm_of(LOAD_SRC, profile);
        let bcc = asm
            .lines()
            .map(str::trim)
            .filter(|l| l.starts_with("BCC __LOAD_OK_"))
            .count();
        assert_eq!(
            bcc, 1,
            "{profile:?}: exactly one LOAD carry branch expected:\n{asm}"
        );
        let ok_label = asm
            .lines()
            .map(str::trim)
            .find(|l| l.starts_with("__LOAD_OK_") && l.ends_with(':'))
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

const LOAD_LAST_SRC: &str = "10 LOAD\"RELOCATE EZLIST\",8\n";
const VERIFY_LAST_SRC: &str = "10 VERIFY\"RELOCATE EZLIST\",8\n";

#[test]
fn never_tail_jumps_into_the_kernal() {
    for profile in PROFILES {
        for src in [LOAD_LAST_SRC, VERIFY_LAST_SRC] {
            let asm = asm_of(src, profile);
            assert!(
                !asm.contains("JMP $FFD5"),
                "{profile:?}: the call must not be a tail jump:\n{asm}"
            );

            assert!(
                asm.contains("JSR $FFD5"),
                "{profile:?}: the KERNAL call itself must survive:\n{asm}"
            );
        }
    }
}

#[test]
fn a_program_without_load_pays_nothing() {
    let asm = asm_of("10 PRINT\"HI\"\n", Profile::Default);
    assert!(
        !asm.contains("__LOAD_OK_"),
        "no LOAD, no carry check:\n{asm}"
    );
    assert!(!asm.contains("JSR $FFD5"), "no LOAD, no KERNAL call:\n{asm}");
}
