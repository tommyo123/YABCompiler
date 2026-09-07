//! `CONT` ; the one direct-mode keyword whose run-time behaviour splits in two, and only one half is reproducible.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];
const SYNTAX: &str = "?SYNTAX  ERROR IN ";

fn build(src: &str, profile: Profile) -> Vec<u8> {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(
        &prg,
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("must compile ({profile:?}): {e}\n{src}"))
    .prg_bytes
}

#[test]
fn cont_followed_by_anything_is_a_runtime_syntax_error() {
    let src = "10 PRINT\"A\":CONT\"XYZ\":PRINT\"C\"\n20 PRINT\"B\"";
    for profile in PROFILES {
        let prg = build(src, profile);
        let r = emu64::run_prg_to_end(&prg, common::MAX_INSNS).expect("run");
        assert!(!r.clean_exit, "must not exit cleanly ({profile:?})");
        // `A` printed, then the trap.
        assert_eq!(
            common::petscii_to_string(&r.output).trim_end(),
            "A",
            "({profile:?})"
        );
        assert_eq!(
            r.error_banner().as_deref(),
            Some(format!("{SYNTAX}10").as_str()),
            "({profile:?})"
        );
    }
}

#[test]
fn an_unreached_cont_costs_nothing() {
    let src = "10 R=1\n20 IF R=59 THEN CONT\"NO PICKAXE\"\n30 PRINT\"DONE\"";
    for profile in PROFILES {
        let prg = build(src, profile);
        let r = emu64::run_prg_to_end(&prg, common::MAX_INSNS).expect("run");
        assert_eq!(r.error_banner(), None, "({profile:?})");
        assert!(r.clean_exit, "({profile:?})");
        assert_eq!(
            common::petscii_to_string(&r.output).trim_end(),
            "DONE",
            "({profile:?})"
        );
    }
}

#[test]
fn the_taken_branch_of_the_foolsgol_shape_raises() {
    let src = "10 R=59\n20 IF R=59 THEN CONT\"NO PICKAXE\"\n30 PRINT\"DONE\"";
    let prg = build(src, Profile::Default);
    let r = emu64::run_prg_to_end(&prg, common::MAX_INSNS).expect("run");
    assert!(!r.clean_exit);
    assert_eq!(common::petscii_to_string(&r.output).trim_end(), "");
    assert_eq!(
        r.error_banner().as_deref(),
        Some(format!("{SYNTAX}20").as_str())
    );
}

#[test]
fn a_reached_bare_cont_stops_the_program_dead() {
    for (src, expected) in [
        ("10 PRINT\"A\":CONT\n20 PRINT\"B\"", "A"),
        ("10 PRINT\"A\":CONT :PRINT\"C\"\n20 PRINT\"B\"", "A"),
        ("10 PRINT\"A\"\n20 CONT\n30 PRINT\"B\"", "A"),
        (
            "10 PRINT\"A\":FORI=1TO3:PRINTI;:NEXT:CONT\n20 PRINT\"B\"",
            "A\n 1  2  3",
        ),
        (
            "10 PRINT\"A\":GOSUB100\n20 PRINT\"B\":END\n100 CONT\n110 RETURN",
            "A",
        ),
    ] {
        for profile in PROFILES {
            let prg = build(src, profile);
            let r = emu64::run_prg_to_end(&prg, common::MAX_INSNS).expect("run");
            assert_eq!(
                common::petscii_to_string(&r.output).trim_end(),
                expected,
                "({profile:?}) {src}"
            );
            // Not an error ; the ROM prints nothing at all here, which is exactly what distinguishes it from `STOP`.
            assert_eq!(r.error_banner(), None, "({profile:?}) {src}");
            assert!(
                !r.clean_exit,
                "a reached bare CONT must not return to READY ({profile:?}): {src}"
            );
        }
    }
}

#[test]
fn stop_is_still_distinguishable_from_a_bare_cont() {
    let prg = build("10 PRINT\"A\":STOP\n20 PRINT\"B\"", Profile::Default);
    let r = emu64::run_prg_to_end(&prg, common::MAX_INSNS).expect("run");
    let out = common::petscii_to_string(&r.output);
    assert!(out.contains('A'), "{out:?}");
    assert!(out.contains("BREAK IN 10"), "{out:?}");
}

#[test]
fn an_unreached_bare_cont_costs_nothing() {
    for src in [
        "10 PRINT\"A\"\n20 END\n30 CONT",
        "10 R=1\n20 IF R=59 THEN CONT\n30 PRINT\"A\"",
    ] {
        for profile in PROFILES {
            let prg = build(src, profile);
            let r = emu64::run_prg_to_end(&prg, common::MAX_INSNS).expect("run");
            assert!(r.clean_exit, "({profile:?}) {src}");
            assert_eq!(r.error_banner(), None, "({profile:?}) {src}");
            assert_eq!(
                common::petscii_to_string(&r.output).trim_end(),
                "A",
                "({profile:?}) {src}"
            );
        }
    }
}

#[test]
fn a_bare_cont_does_not_cost_the_rest_of_the_program() {
    let src = "10 PRINT\"A\"\n20 GOSUB 100\n30 PRINT\"C\"\n40 END\n100 PRINT\"B\"\n110 RETURN\n3017 CONT";
    let prg = tokenize_program(src).expect("tokenizes");
    let out = compile_with_options(&prg, CompileOptions::default())
        .expect("an unreached CONT must not refuse the build");
    let r = emu64::run_prg_to_end(&out.prg_bytes, common::MAX_INSNS).expect("run");
    assert!(r.clean_exit);
    assert_eq!(common::petscii_to_string(&r.output).trim_end(), "A\nB\nC");
}
