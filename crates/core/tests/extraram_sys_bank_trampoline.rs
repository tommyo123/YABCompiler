//! A `SYS` under `--extraram` may not switch the memory bank at its own call site.

mod common;

use yabcompiler_core::{CompileOptions, Compiled, compile_with_options, tokenize_program};

fn build_with(src: &str, extraram: bool) -> Compiled {
    let prg = tokenize_program(src).expect("tokenize");
    compile_with_options(
        &prg,
        CompileOptions {
            extraram,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("must compile: {e}\n{src}"))
}

/// A program with a runtime string heap (so MEMSIZ is relocated ; the `ceiling = Some` arm) and a `SYS`.
fn heap_sys_program(sys_lines: &str) -> String {
    format!(
        "10 N=50\n20 DIM A(N)\n30 A(7)=1234\n\
         40 B$=CHR$(72)+CHR$(69)+CHR$(65)+CHR$(80)\n{sys_lines}50 PRINT A(7);B$\n60 END\n"
    )
}

fn user_code(asm: &str) -> &str {
    let at = asm
        .find("__EXRAM_CODE_START:")
        .expect("an extraram build has a code-start marker");
    &asm[at..]
}

fn preamble(asm: &str) -> &str {
    let at = asm.find("__EXRAM_CODE_START:").unwrap_or(asm.len());
    &asm[..at]
}

#[test]
fn no_bank_switch_survives_in_user_code_with_memsiz_relocated() {
    let asm = build_with(&heap_sys_program("45 SYS 58732\n"), true).asm;
    assert!(
        asm.contains("__EXRAM_MEMSIZ_SAVE"),
        "the fixture is not relocating MEMSIZ, so it tests the wrong arm"
    );
    let code = user_code(&asm);
    for banned in ["INC $01", "DEC $01"] {
        assert!(
            !code.lines().any(|l| l.trim() == banned),
            "a bare `{banned}` survived in user code — it can land above $A000, \
             where the next fetch is ROM:\n{}",
            code.lines()
                .filter(|l| l.trim() == banned)
                .take(3)
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    let win = &asm[asm.find("__EXRAM_SYS_IN:\n").expect("bank-in wrapper")..];
    let win = &win[..win.find("__EXRAM_SYS_OUT:").unwrap_or(win.len())];
    assert!(
        !win.contains("INC $01"),
        "__EXRAM_SYS_IN still banks ROM in before returning:\n{win}"
    );
}

#[test]
fn no_bank_switch_survives_in_user_code_without_memsiz_relocation() {
    // No runtime heap, so `ceiling` is `None`.
    let asm = build_with("10 FOR I=1 TO 3\n20 SYS 58732\n30 NEXT\n", true).asm;
    assert!(
        !asm.contains("__EXRAM_MEMSIZ_SAVE"),
        "the fixture relocates MEMSIZ, so it tests the wrong arm"
    );
    let code = user_code(&asm);
    for banned in ["INC $01", "DEC $01"] {
        assert!(
            !code.lines().any(|l| l.trim() == banned),
            "a bare `{banned}` survived in user code:\n{code}"
        );
    }
    assert!(
        code.contains("    JSR __EXRAM_SYSJT_0\n"),
        "the SYS was not routed through a trampoline:\n{code}"
    );

    assert!(!code.contains("__EXRAM_SYS_IN"));
}

#[test]
fn the_trampoline_brackets_the_call_and_lives_in_the_preamble() {
    let asm = build_with(&heap_sys_program("45 SYS 58732\n"), true).asm;
    let pre = preamble(&asm);
    assert!(
        pre.contains("__EXRAM_SYSJT_0:\n    INC $01\n    JSR $E56C\n    DEC $01\n    RTS\n"),
        "the trampoline is not in the preamble or has the wrong shape:\n{pre}"
    );
}

/// One trampoline per distinct call, not per site.
#[test]
fn sites_calling_the_same_address_share_one_trampoline() {
    let asm = build_with(
        &heap_sys_program("41 SYS 58732\n42 SYS 58732\n43 SYS 58732\n45 SYS 58732\n"),
        true,
    )
    .asm;
    let sites = asm
        .lines()
        .filter(|l| l.trim() == "JSR __EXRAM_SYSJT_0")
        .count();
    assert_eq!(sites, 4, "expected four call sites, got {sites}");
    assert!(
        !asm.contains("__EXRAM_SYSJT_1:"),
        "four SYSes to one address made more than one trampoline:\n{}",
        preamble(&asm)
    );
}

/// Two different targets get two trampolines, and each keeps its own address.
#[test]
fn distinct_targets_get_distinct_trampolines() {
    let asm = build_with(&heap_sys_program("44 SYS 58732\n45 SYS 59626\n"), true).asm;
    let pre = preamble(&asm);
    assert!(pre.contains("__EXRAM_SYSJT_0:\n    INC $01\n    JSR $E56C\n"), "{pre}");
    assert!(pre.contains("__EXRAM_SYSJT_1:\n    INC $01\n    JSR $E8EA\n"), "{pre}");
}

#[test]
fn computed_sys_sites_share_one_patched_trampoline() {

    let asm = build_with(
        &heap_sys_program("43 SA=58000+PEEK(53280)\n44 SYS SA\n45 SYS SA+2\n"),
        true,
    )
    .asm;
    let pre = preamble(&asm);
    assert!(
        pre.contains(
            "__EXRAM_SYSJT_IND:\n    INC $01\n__EXRAM_SYS_PATCHED:\n    JSR $FFFF\n    DEC $01\n    RTS\n"
        ),
        "the shared computed-SYS trampoline is missing or misshapen:\n{pre}"
    );
    let code = user_code(&asm);
    assert_eq!(
        code.lines()
            .filter(|l| l.trim() == "JSR __EXRAM_SYSJT_IND")
            .count(),
        2,
        "both computed sites should call the shared trampoline:\n{code}"
    );

    assert_eq!(
        code.lines()
            .filter(|l| l.trim() == "STA __EXRAM_SYS_PATCHED+1")
            .count(),
        2,
        "a computed site is still patching its own vanished JSR:\n{code}"
    );
    assert!(
        !code.contains("STA __SYS_"),
        "a self-modify still names a per-site SYS label:\n{code}"
    );

    assert_eq!(
        asm.lines().filter(|l| l.trim() == "JSR $FFFF").count(),
        1,
        "more than one patched SYS JSR survived:\n{asm}"
    );
}

#[test]
fn the_jsr_pair_factorer_does_not_swallow_the_sys_call() {
    let src = heap_sys_program("41 SYS 58732\n42 SYS 58732\n43 SYS 58732\n44 SYS 58732\n");
    for extraram in [false, true] {
        let asm = build_with(&src, extraram).asm;

        assert!(
            !asm.contains("    JSR __SYS_REGS_IN\n    JMP $E56C\n"),
            "PH110 factored the SYS call into a helper (extraram={extraram}):\n{}",
            asm.lines()
                .skip_while(|l| !l.contains("__JSR2_"))
                .take(8)
                .collect::<Vec<_>>()
                .join("\n")
        );

        let want = if extraram {
            "    JSR __SYS_REGS_IN\n    JSR __EXRAM_SYS_IN\n    JSR __EXRAM_SYSJT_0\n"
        } else {
            "    JSR __SYS_REGS_IN\n    ; YAB_SYS_WRAP_IN\n    JSR $E56C\n"
        };
        assert!(
            asm.contains(want),
            "the SYS site lost its shape (extraram={extraram}); wanted:\n{want}"
        );
    }
}

#[test]
fn the_jsr_pair_factorer_still_factors_ordinary_pairs() {

    let mut src = String::from("10 N=9:DIM A(N)\n");
    for (i, ln) in (20..).step_by(10).zip(0..8) {
        src.push_str(&format!("{i} A({ln})=A({ln})+1.5\n"));
    }
    src.push_str("200 PRINT A(0);A(1);A(2);A(3)\n");
    let asm = build_with(&src, false).asm;
    assert!(
        asm.lines().any(|l| l.starts_with("__JSR2_")),
        "no JSR pair was factored at all — the barrier is over-firing, or this \
         fixture no longer produces recurring pairs:\n{asm}"
    );
}

#[test]
fn a_default_build_has_no_trampoline_and_no_bank_switch() {
    let asm = build_with(&heap_sys_program("45 SYS 58732\n"), false).asm;
    assert!(!asm.contains("__EXRAM_"), "extraram machinery in a default build");
    assert!(asm.contains("    JSR $E56C\n"), "the SYS call went missing:\n{asm}");
    assert!(
        !asm.lines().any(|l| l.trim() == "INC $01"),
        "a default build has no bank switch to make"
    );
}

#[test]
fn the_program_still_runs_and_prints() {
    for extraram in [false, true] {
        let built = build_with(&heap_sys_program("45 SYS 58732\n"), extraram);
        let out = common::run_clean(&built.prg_bytes)
            .unwrap_or_else(|| panic!("extraram={extraram}: did not reach a clean exit"));
        assert!(
            out.contains("1234") && out.contains("HEAP"),
            "extraram={extraram}: expected ` 1234 HEAP`, got {out:?}"
        );
    }
}
