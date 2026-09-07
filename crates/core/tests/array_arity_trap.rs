mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];
const BAD_SUBSCRIPT: &str = "?BAD SUBSCRIPT  ERROR IN ";

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

fn check_ok(src: &str, out: &str) {
    for profile in PROFILES {
        let prg = build(src, profile);
        let r = emu64::run_prg_to_end(&prg, common::MAX_INSNS).expect("run");
        assert_eq!(r.error_banner(), None, "no error expected ({profile:?}):\n{src}");
        assert!(r.clean_exit, "clean exit expected ({profile:?}):\n{src}");
        assert_eq!(
            common::petscii_to_string(&r.output).trim_end(),
            out,
            "({profile:?}):\n{src}"
        );
    }
}

fn check_error(src: &str, out: &str, banner: &str) {
    for profile in PROFILES {
        let prg = build(src, profile);
        let r = emu64::run_prg_to_end(&prg, common::MAX_INSNS).expect("run");
        assert!(!r.clean_exit, "must not exit cleanly ({profile:?}):\n{src}");
        assert_eq!(
            common::petscii_to_string(&r.output).trim_end(),
            out,
            "output before the error ({profile:?}):\n{src}"
        );
        assert_eq!(r.error_banner().as_deref(), Some(banner), "({profile:?}):\n{src}");
    }
}

#[test]
fn too_many_subscripts_on_a_store() {
    check_error(
        "10 PRINT\"A\"\n20 DIM A(5)\n30 A(1,2)=3\n40 PRINT\"DONE\"",
        "A",
        &format!("{BAD_SUBSCRIPT}30"),
    );
}

#[test]
fn too_many_subscripts_on_a_read() {
    check_error(
        "10 PRINT\"A\"\n20 DIM A(5)\n30 PRINT A(1,2)\n40 PRINT\"DONE\"",
        "A",
        &format!("{BAD_SUBSCRIPT}30"),
    );
}

#[test]
fn too_few_subscripts() {
    check_error(
        "10 PRINT\"A\"\n20 DIM A(5,5)\n30 A(1)=3\n40 PRINT\"DONE\"",
        "A",
        &format!("{BAD_SUBSCRIPT}30"),
    );
}

#[test]
fn string_arrays_trap_the_same_way() {
    check_error(
        "10 PRINT\"A\"\n20 DIM A$(5)\n30 PRINT A$(1,2)\n40 PRINT\"DONE\"",
        "A",
        &format!("{BAD_SUBSCRIPT}30"),
    );
}

#[test]
fn an_unreachable_mismatch_costs_nothing() {
    check_ok(
        "10 DIM A(5)\n20 GOTO 40\n30 A(1,2)=3\n40 PRINT\"DONE\"",
        "DONE",
    );
}

#[test]
fn a_mismatch_on_a_branch_not_taken_costs_nothing() {
    check_ok(
        "10 DIM A(5)\n20 R=1\n30 IF R=59 THEN A(1,2)=3\n40 PRINT\"DONE\"",
        "DONE",
    );
}

#[test]
fn the_right_number_of_subscripts_still_works() {
    check_ok(
        "10 DIM A(5,5)\n20 A(2,3)=7\n30 PRINT A(2,3)\n40 DIM B(4)\n50 B(2)=9\n60 PRINT B(2)",
        // BASIC's leading sign space and trailing space on a numeric PRINT, which is what the ROM emits.
        " 7 \n 9",
    );
}

#[test]
fn a_runtime_dimensioned_array_traps_too() {
    check_error(
        "10 PRINT\"A\"\n20 N=5:DIM A(N)\n30 A(1,2)=3\n40 PRINT\"DONE\"",
        "A",
        &format!("{BAD_SUBSCRIPT}30"),
    );
    check_ok(
        "10 N=5:DIM A(N)\n20 GOTO 40\n30 A(1,2)=3\n40 PRINT\"DONE\"",
        "DONE",
    );
}
