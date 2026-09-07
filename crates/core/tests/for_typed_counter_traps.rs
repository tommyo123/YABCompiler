mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

fn run(src: &str, profile: Profile) -> (String, bool, Option<String>) {
    let prg = tokenize_program(src).expect("tokenize");
    let c = compile_with_options(
        &prg,
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .expect("must build: the ROM runs this program");
    let r = emu64::run_prg_to_end(&c.prg_bytes, 20_000_000).expect("runs");
    (
        common::petscii_to_string(&r.output),
        r.clean_exit,
        r.error_banner(),
    )
}

/// Assert the exact banner and that everything printed before the trap still ran.
fn assert_traps(src: &str, banner: &str, before: &str) {
    for p in PROFILES {
        let (out, clean, got) = run(src, p);
        assert!(!clean, "{p:?}: must not finish cleanly, got {out:?}");
        assert_eq!(got.as_deref(), Some(banner), "{p:?}: wrong banner");
        assert_eq!(
            out.trim_end(),
            before,
            "{p:?}: output before the trap must survive"
        );
    }
}

/// Assert the program runs to a clean stop printing exactly `want`.
fn assert_clean(src: &str, want: &str) {
    for p in PROFILES {
        let (out, clean, banner) = run(src, p);
        assert!(clean, "{p:?}: must finish cleanly, got {out:?} {banner:?}");
        assert_eq!(banner, None, "{p:?}: must not raise");
        assert_eq!(out.trim_end(), want, "{p:?}");
    }
}

#[test]
fn for_string_counter_traps_type_mismatch_where_it_runs() {
    assert_traps(
        "10 print\"a\"\n20 for a$=1 to 2\n30 next\n40 print\"b\"\n",
        "?TYPE MISMATCH  ERROR IN 20",
        "A",
    );
}

#[test]
fn for_string_counter_with_string_bounds_is_also_type_mismatch() {
    assert_traps(
        "10 print\"a\"\n20 for a$=\"a\" to \"b\"\n30 next\n40 print\"b\"\n",
        "?TYPE MISMATCH  ERROR IN 20",
        "A",
    );
}

/// Space-transparent spelling.
#[test]
fn for_string_counter_is_space_transparent() {
    assert_traps(
        "10 print\"a\"\n20 for a $=1 to 2\n30 next\n40 print\"b\"\n",
        "?TYPE MISMATCH  ERROR IN 20",
        "A",
    );
}

/// The rest of the line dies with the statement, as it does on the ROM.
#[test]
fn for_string_counter_kills_the_rest_of_its_line() {
    assert_traps(
        "10 print\"a\"\n20 for a$=1 to 2:print\"x\"\n30 next\n",
        "?TYPE MISMATCH  ERROR IN 20",
        "A",
    );
}

#[test]
fn an_unreached_string_counter_costs_nothing() {
    assert_clean("10 print\"a\"\n20 end\n30 for a$=1 to 2\n40 next\n", "A");
}

#[test]
fn the_string_counter_residual_is_the_start_expressions_own_error() {
    let (_, clean, banner) = run(
        "10 print\"a\"\n20 for a$=1/0 to 2\n30 next\n",
        Profile::Default,
    );
    assert!(!clean);
    assert_eq!(
        banner.as_deref(),
        Some("?TYPE MISMATCH  ERROR IN 20"),
        "the ROM says ?DIVISION BY ZERO  ERROR IN 20 here; this pins OUR \
         current answer so the gap stays measured"
    );
}

#[test]
fn the_integer_counter_extension_is_still_on_in_both_spellings() {
    assert_clean(
        "10 for i%=1 to 3\n20 print i%;:next\n30 print\"e\"\n",
        " 1  2  3 E",
    );
    assert_clean(
        "10 for i %=1 to 3\n20 print i%;:next\n30 print\"e\"\n",
        " 1  2  3 E",
    );
}

#[test]
fn a_float_counter_still_loops() {
    assert_clean(
        "10 for h=0 to 2\n20 print h;\n30 next\n40 print\"z\"\n",
        " 0  1  2 Z",
    );
    assert_clean(
        "10 for h =0 to 2\n20 print h;\n30 next\n40 print\"z\"\n",
        " 0  1  2 Z",
    );
}

/// An integer or string variable is only rejected as the counter.
#[test]
fn integer_and_string_variables_still_work_off_the_counter() {
    assert_clean(
        "10 h%=2:a$=\"z\"\n20 for i=0 to h%\n30 print i;\n40 next\n50 print a$\n",
        " 0  1  2 Z",
    );
}

#[test]
fn a_typed_next_variable_is_next_without_for_not_syntax() {
    for src in [
        "10 print\"a\"\n20 for h=0 to 2\n30 next h%\n40 print\"b\"\n",
        "10 print\"a\"\n20 for h=0 to 2\n30 next h$\n40 print\"b\"\n",
    ] {
        assert_traps(src, "?NEXT WITHOUT FOR  ERROR IN 30", "A");
    }
}
