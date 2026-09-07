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

/// Run at every profile and require exactly the ROM's banner, at `line`.
#[track_caller]
fn assert_banner(src: &str, banner: &str, line: u16, must_not_contain: &str) {
    for profile in PROFILES {
        let c = build(src, profile);
        let r = emu64::run_prg_to_end(&c.prg_bytes, common::MAX_INSNS).expect("emu64 runs");
        let out = common::petscii_to_string(&r.output);
        let want = format!("?{banner}  ERROR IN {line}");
        assert_eq!(
            r.error_banner().as_deref(),
            Some(want.as_str()),
            "{profile:?}: {src:?}\n  must raise {want:?}, got {:?} with output {out:?}",
            r.error_banner()
        );
        if !must_not_contain.is_empty() {
            assert!(
                !out.contains(must_not_contain),
                "{profile:?}: {src:?}\n  raised {want:?} but kept running -- output {out:?} \
                 contains {must_not_contain:?}"
            );
        }
    }
}

/// Run at every profile and require a clean exit whose output is exactly `want`.
#[track_caller]
fn assert_prints(src: &str, want: &str) {
    for profile in PROFILES {
        let c = build(src, profile);
        let r = emu64::run_prg_to_end(&c.prg_bytes, common::MAX_INSNS).expect("emu64 runs");
        let out = common::petscii_to_string(&r.output);
        assert_eq!(
            r.error_banner(),
            None,
            "{profile:?}: {src:?} must NOT raise an error, got {:?} / {out:?}",
            r.error_banner()
        );
        assert_eq!(out, want, "{profile:?}: {src:?}");
    }
}

#[test]
fn a_string_comparison_is_minus_one_in_an_expression_and_through_a_variable() {
    assert_prints(
        "10 A$=\"A\"\n20 PRINT (A$=\"A\")+2\n30 X=(A$=\"A\")\n40 PRINT X+2\n",
        " 1 \n 1 \n",
    );
}

#[test]
fn a_string_comparison_prints_minus_one_and_zero() {
    assert_prints(
        "10 A$=\"A\"\n20 PRINT (A$=\"A\")\n30 PRINT (A$=\"B\")\n",
        "-1 \n 0 \n",
    );
}

#[test]
fn division_by_zero_names_the_dividing_line_not_the_assigning_line() {
    assert_banner("10 Z=0\n100 X=1/Z\n", "DIVISION BY ZERO", 100, "");
}

#[test]
fn poke_with_a_value_of_256_is_illegal_quantity_and_stops() {
    assert_banner("10 POKE 1024,256\n20 PRINT \"REACHED\"\n", "ILLEGAL QUANTITY", 10, "REACHED");
}

#[test]
fn an_integer_assignment_of_32768_is_illegal_quantity() {
    assert_banner("10 A%=32768\n20 PRINT A%\n", "ILLEGAL QUANTITY", 10, "");
}

#[test]
fn the_integer_boundaries_themselves_still_store() {
    assert_prints("10 A%=32767\n20 B%=-32768\n30 PRINT A%;B%\n", " 32767 -32768 \n");
}

#[test]
fn a_computed_subscript_past_the_dim_is_bad_subscript_and_stops() {
    assert_banner(
        "10 I=2:DIM A(1):A(I)=5\n20 PRINT \"REACHED\"\n",
        "BAD SUBSCRIPT",
        10,
        "REACHED",
    );
}

#[test]
fn the_last_in_range_subscript_still_stores() {
    assert_prints("10 I=1:DIM A(1):A(I)=5\n20 PRINT A(1)\n", " 5 \n");
}
