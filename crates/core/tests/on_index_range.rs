//! `ON <expr> GOTO/GOSUB` must range-check its selector.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

fn build(src: &str, profile: Profile) -> yabcompiler_core::Compiled {
    let tok = tokenize_program(src).unwrap_or_else(|e| panic!("tokenize: {e:?}"));
    compile_with_options(
        &tok,
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compile ({profile:?}): {e}"))
}

/// Run at every profile and require the ROM's error banner.
#[track_caller]
fn assert_illegal_quantity(src: &str, line: u16) {
    for profile in PROFILES {
        let c = build(src, profile);
        let r = emu64::run_prg_to_end(&c.prg_bytes, common::MAX_INSNS).expect("emu64 runs");
        let want = format!("?ILLEGAL QUANTITY  ERROR IN {line}");
        assert_eq!(
            r.error_banner().as_deref(),
            Some(want.as_str()),
            "{profile:?}: {src:?} must raise {want:?}, got banner {:?} output {:?}",
            r.error_banner(),
            common::petscii_to_string(&r.output)
        );
        assert!(
            !common::petscii_to_string(&r.output).contains('A')
                && !common::petscii_to_string(&r.output).contains('B'),
            "{profile:?}: must not reach a branch target before erroring: {:?}",
            common::petscii_to_string(&r.output)
        );
    }
}

/// Run at every profile and require a clean exit printing `want`.
#[track_caller]
fn assert_prints(src: &str, want: &str) {
    for profile in PROFILES {
        let c = build(src, profile);
        let r = emu64::run_prg_to_end(&c.prg_bytes, common::MAX_INSNS).expect("emu64 runs");
        let got = common::petscii_to_string(&r.output);
        assert!(
            r.clean_exit,
            "{profile:?}: {src:?} must run clean, banner {:?} output {got:?}",
            r.error_banner()
        );
        assert_eq!(got.trim(), want, "{profile:?}: {src:?} must print {want:?}");
    }
}

const TARGETS: &str = "30 print \"a\":end\n40 print \"b\":end\n";
const TARGETS_FALL: &str = "20 print \"fall\":end\n30 print \"a\":end\n40 print \"b\":end\n";

// negative: out-of-range selectors must raise

#[test]
fn a_runtime_selector_of_258_is_illegal_quantity() {
    assert_illegal_quantity(&format!("10 q=258\n20 on q goto 30,40\n{TARGETS}"), 20);
}

#[test]
fn a_runtime_selector_of_256_is_illegal_quantity() {
    assert_illegal_quantity(&format!("10 q=256\n20 on q goto 30,40\n{TARGETS}"), 20);
}

#[test]
fn a_literal_selector_of_256_is_illegal_quantity() {
    assert_illegal_quantity(&format!("10 on 256 goto 30,40\n{TARGETS}"), 10);
}

#[test]
fn a_negative_selector_is_illegal_quantity() {
    assert_illegal_quantity(&format!("10 on -1 goto 30,40\n{TARGETS}"), 10);
}

#[test]
fn a_negative_fractional_selector_is_illegal_quantity() {
    assert_illegal_quantity(&format!("10 on -0.5 goto 30,40\n{TARGETS}"), 10);
}

#[test]
fn a_selector_above_the_signed_range_is_illegal_quantity() {
    assert_illegal_quantity(&format!("10 on 40000 goto 30,40\n{TARGETS}"), 10);
}

#[test]
fn on_gosub_range_checks_the_same_way() {
    assert_illegal_quantity(
        "10 on 256 gosub 30,40\n20 print \"fall\":end\n30 print \"a\":return\n40 print \"b\":return\n",
        10,
    );
}

#[test]
fn a_failed_on_gosub_pushes_no_frame() {
    let c = build("10 on 256 gosub 30\n30 return\n", Profile::Default);
    let head = c.asm.split("; --- helper subroutines ---").next().unwrap();
    let chk = head.find("__FAC_BYTE").expect("range check is emitted");
    let push = head.find("__FOR_STACK_PUSH_MARKER");
    assert!(
        push.is_none_or(|p| p > chk),
        "the marker push must sit after the range check:\n{head}"
    );
}

/// 0 and "past the end of the list" are both plain fall-through on the ROM.
#[test]
fn index_zero_falls_through() {
    assert_prints(&format!("10 on 0 goto 30,40\n{TARGETS_FALL}"), "FALL");
}

#[test]
fn an_index_past_the_last_target_falls_through() {
    assert_prints(&format!("10 on 5 goto 30,40\n{TARGETS_FALL}"), "FALL");
}

#[test]
fn index_255_falls_through() {
    assert_prints(&format!("10 on 255 goto 30,40\n{TARGETS_FALL}"), "FALL");
}

#[test]
fn a_fractional_selector_floors_towards_its_target() {
    assert_prints(&format!("10 on 1.7 goto 30,40\n{TARGETS_FALL}"), "A");
}

#[test]
fn ordinary_selectors_still_branch() {
    assert_prints(&format!("10 on 1 goto 30,40\n{TARGETS_FALL}"), "A");
    assert_prints(&format!("10 on 2 goto 30,40\n{TARGETS_FALL}"), "B");
    assert_prints(&format!("10 q=2\n20 on q goto 30,40\n{TARGETS_FALL}"), "B");
}

#[test]
fn a_u8_proven_selector_skips_the_range_check() {
    for src in [
        "10 x=1\n20 on x goto 30,40\n30 print \"a\":end\n40 print \"b\":end\n",
        "10 x=peek(1024) and 3\n20 on x goto 30,40\n30 print \"a\":end\n40 print \"b\":end\n",
    ] {
        let c = build(src, Profile::Default);
        assert!(
            c.asm.contains("__FAC_BYTE_OK"),
            "a u8-proven selector must use the no-check helper:\n{src}\n{}",
            c.asm
        );
        assert!(
            !c.asm.contains("__FAC_BYTE:"),
            "…and must not pull in the checking helper:\n{src}\n{}",
            c.asm
        );
    }
}

#[test]
fn an_unproven_selector_keeps_the_range_check() {
    let src = "10 input x\n20 on x goto 30,40\n30 print \"a\":end\n40 print \"b\":end\n";
    let c = build(src, Profile::Default);
    assert!(
        c.asm.contains("__FAC_BYTE:"),
        "an unproven selector must keep the range check:\n{}",
        c.asm
    );
}
