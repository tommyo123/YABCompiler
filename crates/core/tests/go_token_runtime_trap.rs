//! `GO`, and the truncated `GOTO` / `GOSUB` tails, behave as the ROM does.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

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

/// Compiles, prints `out`, exits clean with no banner ; on every profile.
fn check_ok(src: &str, out: &str) {
    for profile in PROFILES {
        let prg = build(src, profile);
        let r = emu64::run_prg_to_end(&prg, common::MAX_INSNS).expect("run");
        assert_eq!(r.error_banner(), None, "({profile:?}):\n{src}");
        assert!(r.clean_exit, "clean exit expected ({profile:?}):\n{src}");
        assert_eq!(
            common::petscii_to_string(&r.output).trim_end(),
            out,
            "({profile:?}):\n{src}"
        );
    }
}

/// Compiles, prints `out`, then dies with `banner` ; on every profile.
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
        assert_eq!(
            r.error_banner().as_deref(),
            Some(banner),
            "({profile:?}):\n{src}"
        );
    }
}

#[test]
fn reached_bare_go_raises_syntax_error() {
    check_error(
        "10 print\"a\"\n20 go\n30 print\"b\"\n",
        "A",
        "?SYNTAX  ERROR IN 20",
    );
}

#[test]
fn reached_bare_go_before_a_colon_stops_the_whole_line() {
    check_error(
        "10 print\"a\"\n20 go:print\"c\"\n30 print\"b\"\n",
        "A",
        "?SYNTAX  ERROR IN 20",
    );
}

#[test]
fn reached_truncated_got_raises_syntax_error() {
    check_error(
        "10 print\"a\"\n20 got\n30 print\"b\"\n",
        "A",
        "?SYNTAX  ERROR IN 20",
    );
}

#[test]
fn reached_truncated_gos_raises_syntax_error() {
    check_error(
        "10 print\"a\"\n20 gos\n30 print\"b\"\n",
        "A",
        "?SYNTAX  ERROR IN 20",
    );
}

#[test]
fn reached_bare_goto_raises_undefd_statement() {
    check_error(
        "10 print\"a\"\n20 goto\n30 print\"b\"\n",
        "A",
        "?UNDEF'D STATEMENT  ERROR IN 20",
    );
}

#[test]
fn reached_bare_gosub_raises_undefd_statement_not_syntax() {
    check_error(
        "10 print\"a\"\n20 gosub\n30 print\"b\"\n",
        "A",
        "?UNDEF'D STATEMENT  ERROR IN 20",
    );
}

#[test]
fn bare_goto_is_goto_line_zero_not_a_trap() {
    check_ok("0 print\"z\":end\n10 goto\n", "Z");
}

#[test]
fn bare_gosub_is_gosub_line_zero_not_a_trap() {
    let src = "0 goto 10\n10 print\"a\"\n20 gosub\n30 print\"b\"\n";
    for profile in PROFILES {
        let prg = build(src, profile);
        let r = emu64::run_prg_to_end(&prg, common::MAX_INSNS).expect("run");
        let banner = r.error_banner().unwrap_or_default();
        assert!(
            banner.starts_with("?OUT OF MEMORY"),
            "({profile:?}) expected stack exhaustion from GOSUB 0, got {banner:?}:\n{src}"
        );
    }
}

#[test]
fn go_to_with_a_target_still_runs() {
    check_ok(
        "10 print\"a\"\n20 go to 40\n30 print\"skip\"\n40 print\"b\"\n",
        "A\nB",
    );
}

#[test]
fn goto_with_a_target_still_runs() {
    check_ok(
        "10 print\"a\"\n20 goto 40\n30 print\"skip\"\n40 print\"b\"\n",
        "A\nB",
    );
}

#[test]
fn gosub_with_a_target_still_runs() {
    check_ok(
        "10 print\"a\"\n20 gosub 500\n30 print\"b\"\n40 end\n500 print\"s\":return\n",
        "A\nS\nB",
    );
}

#[test]
fn go_sub_spelled_with_a_space_is_a_syntax_error() {
    check_error(
        "10 print\"a\"\n20 go sub 500\n30 print\"b\"\n40 end\n500 print\"s\":return\n",
        "A",
        "?SYNTAX  ERROR IN 20",
    );
}

#[test]
fn got_with_a_target_is_a_syntax_error() {
    check_error(
        "10 print\"a\"\n20 got40\n30 print\"skip\"\n40 print\"b\"\n",
        "A",
        "?SYNTAX  ERROR IN 20",
    );
}

/// Likewise `GOS500` -> `$CB` `'S'` `'5'` `'0'` `'0'`.
#[test]
fn gos_with_a_target_is_a_syntax_error() {
    check_error(
        "10 print\"a\"\n20 gos500\n30 print\"b\"\n40 end\n500 print\"s\":return\n",
        "A",
        "?SYNTAX  ERROR IN 20",
    );
}

#[test]
fn the_correctly_spelled_forms_still_run() {
    check_ok(
        "10 print\"a\"\n20 goto40\n30 print\"skip\"\n40 print\"b\"\n",
        "A\nB",
    );
    check_ok(
        "10 print\"a\"\n20 gosub500\n30 print\"b\"\n40 end\n500 print\"s\":return\n",
        "A\nS\nB",
    );
    check_ok(
        "10 print\"a\"\n20 go to40\n30 print\"skip\"\n40 print\"b\"\n",
        "A\nB",
    );
}

/// `GO TO` with the target missing is `GOTO 0`, exactly like bare `GOTO`.
#[test]
fn bare_go_to_is_goto_line_zero() {
    check_error(
        "10 print\"a\"\n20 go to\n30 print\"b\"\n",
        "A",
        "?UNDEF'D STATEMENT  ERROR IN 20",
    );
    // and it finds a line 0 when there is one
    check_ok("0 print\"z\":end\n10 go to\n", "Z");
}

#[test]
fn unreached_go_shapes_cost_nothing() {
    check_ok("10 print\"a\"\n20 end\n30 go\n", "A");
    check_ok("10 print\"a\"\n20 end\n30 got\n", "A");
    check_ok("10 print\"a\"\n20 end\n30 gos\n", "A");
    check_ok("10 print\"a\"\n20 end\n30 godub500\n", "A");
}

#[test]
fn a_false_if_branch_containing_a_bad_go_costs_nothing() {
    check_ok("10 if 0 then go\n20 print\"b\"\n", "B");
    check_ok("10 if 0 then godub500\n20 print\"b\"\n", "B");
}

#[test]
fn a_true_if_branch_containing_a_bad_go_raises() {
    check_error("10 if 1 then go\n20 print\"b\"\n", "", "?SYNTAX  ERROR IN 10");
}

#[test]
fn reached_then_goto_with_no_target_raises_undefd_statement() {
    check_error(
        "10 print\"a\"\n20 if 1 then goto\n30 print\"b\"\n",
        "A",
        "?UNDEF'D STATEMENT  ERROR IN 20",
    );
}

#[test]
fn then_gosub_and_on_goto_with_no_target_also_raise_undefd_statement() {
    check_error(
        "10 print\"a\"\n20 if 1 then gosub\n30 print\"b\"\n",
        "A",
        "?UNDEF'D STATEMENT  ERROR IN 20",
    );
    check_error(
        "10 print\"a\"\n20 on 1 goto\n30 print\"b\"\n",
        "A",
        "?UNDEF'D STATEMENT  ERROR IN 20",
    );
}

#[test]
fn then_goto_counterweights() {
    check_ok("10 print\"a\"\n20 if 0 then goto\n30 print\"b\"\n", "A\nB");
    check_ok(
        "10 print\"a\"\n20 if 1 then goto 40\n30 print\"skip\"\n40 print\"b\"\n",
        "A\nB",
    );
    check_ok(
        "10 print\"a\"\n20 on 1 goto 40\n30 print\"skip\"\n40 print\"b\"\n",
        "A\nB",
    );
}

#[test]
fn then_goto_with_no_target_finds_line_zero() {
    check_ok("0 print\"z\":end\n10 if 1 then goto\n", "Z");
}

#[test]
fn the_starlight_zone_line_302_shape() {
    check_error(
        "10 print\"a\"\n20 godub500\n30 print\"b\"\n500 return\n",
        "A",
        "?SYNTAX  ERROR IN 20",
    );
    check_ok(
        "10 print\"a\"\n20 gosub500\n30 print\"b\"\n40 end\n500 print\"s\":return\n",
        "A\nS\nB",
    );
}
