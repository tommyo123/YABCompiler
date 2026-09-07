

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

/// Compile at `profile` (with extraram forced on or off), run to a clean exit and return the
/// decoded PRINT output.
fn run_variant(src: &str, profile: Profile, extraram: bool) -> String {
    let prg = tokenize_program(src).expect("tokenizes");
    let opts = CompileOptions {
        profile,
        extraram,
        force_extraram_off: !extraram,
        ..CompileOptions::default()
    };
    let bytes = compile_with_options(&prg, opts)
        .unwrap_or_else(|e| panic!("compiles ({profile:?}, extraram={extraram}): {e}"))
        .prg_bytes;
    common::run_clean(&bytes)
        .unwrap_or_else(|| panic!("runs to a clean exit ({profile:?}, extraram={extraram}):\n{src}"))
}

/// Build the expected CHROUT text for a sequence of `PRINT <number>` statements.
fn nums(fields: &[&str]) -> String {
    fields.iter().map(|f| format!("{f} \n")).collect()
}
fn check_all_variants(src: &str, expected: &str) {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        for extraram in [false, true] {
            let got = run_variant(src, profile, extraram);
            assert_eq!(
                got, expected,
                "{profile:?} extraram={extraram} mismatch for:\n{src}"
            );
        }
    }
}
#[test]
fn two_int_stores_of_float_literals_floor() {
    check_all_variants(
        "10 A%=-1.5:PRINT A%\n20 A%=1.5:PRINT A%\n30 END\n",
        &nums(&["-2", " 1"]),
    );
}
#[test]
fn one_int_store_of_a_float_literal_floors() {
    check_all_variants("10 A%=-1.5:PRINT A%\n20 END\n", &nums(&["-2"]));
}

/// Both the store and the use inside a single BASIC line, twice over.
#[test]
fn two_store_use_pairs_on_one_line_floor() {
    check_all_variants(
        "10 A%=-1.5:PRINT A%:A%=1.5:PRINT A%\n20 END\n",
        &nums(&["-2", " 1"]),
    );
}

/// Three pairs on one line, plus a third distinct value.
#[test]
fn three_store_use_pairs_on_one_line_floor() {
    check_all_variants(
        "10 A%=-1.5:PRINT A%:A%=1.5:PRINT A%:A%=-2.7:PRINT A%\n20 END\n",
        &nums(&["-2", " 1", "-3"]),
    );
}

/// More than two statements per line, with the stores and the uses interleaved across several
/// `%` variables.
#[test]
fn interleaved_int_stores_across_many_statements_floor() {
    check_all_variants(
        "10 A%=-1.5:B%=2.5:PRINT A%:PRINT B%:C%=-0.5:PRINT C%\n\
         20 A%=3.9:B%=-3.9:PRINT A%:PRINT B%:C%=0.5:PRINT C%\n\
         30 END\n",
        &nums(&["-2", " 2", "-1", " 3", "-4", " 0"]),
    );
}
#[test]
fn narrowed_int_value_feeds_arithmetic() {
    check_all_variants(
        "10 A%=-1.5:PRINT A%*2:PRINT A%+1:PRINT -A%\n\
         20 A%=1.5:PRINT A%*2:PRINT A%+1:PRINT -A%\n\
         30 END\n",
        &nums(&["-4", "-1", " 2", " 2", " 2", "-1"]),
    );
}

/// The RHS need not be a literal: a constant-folded division has to floor the same way.
#[test]
fn division_into_an_int_var_floors() {
    check_all_variants(
        "10 A%=-3/2:PRINT A%\n20 A%=-7/2:PRINT A%\n30 A%=7/2:PRINT A%\n40 END\n",
        &nums(&["-2", "-4", " 3"]),
    );
}
#[test]
fn int_store_from_a_float_var_floors() {
    check_all_variants(
        "10 X=-1.5:A%=X:PRINT A%\n20 X=2.7:A%=X:PRINT A%\n30 END\n",
        &nums(&["-2", " 2"]),
    );
}
#[test]
fn int_array_element_stores_floor() {
    check_all_variants(
        "10 DIM A%(3)\n\
         20 A%(0)=-1.5:PRINT A%(0):A%(1)=1.5:PRINT A%(1)\n\
         30 A%(2)=-2.7:PRINT A%(2):A%(3)=2.7:PRINT A%(3)\n\
         40 END\n",
        &nums(&["-2", " 1", "-3", " 2"]),
    );
}

/// Narrowing across an `IF..THEN` boundary, in both directions: the store outside and the use
/// inside the THEN body, then the store inside the THEN body.
#[test]
fn int_narrowing_survives_if_then() {
    check_all_variants(
        "10 A%=-1.5:IF A%<0 THEN PRINT A%\n\
         20 A%=1.5:IF A%>0 THEN PRINT A%\n\
         30 IF 1 THEN A%=-2.7:PRINT A%\n\
         40 END\n",
        &nums(&["-2", " 1", "-3"]),
    );
}
#[test]
fn narrowed_int_value_copies_to_other_vars() {
    check_all_variants(
        "10 A%=-1.5:B%=A%:X=A%:PRINT B%:PRINT X\n\
         20 A%=1.5:B%=A%:X=A%:PRINT B%:PRINT X\n\
         30 END\n",
        &nums(&["-2", "-2", " 1", " 1"]),
    );
}

/// Fractions with magnitude below 1: floor sends -0.5 to -1 while truncation would leave 0.
#[test]
fn sub_unit_fractions_floor_not_truncate() {
    check_all_variants(
        "10 A%=-0.5:PRINT A%\n20 A%=0.5:PRINT A%\n30 A%=-.0001:PRINT A%\n40 END\n",
        &nums(&["-1", " 0", "-1"]),
    );
}
#[test]
fn narrowed_int_value_feeds_for_bounds() {
    check_all_variants(
        "10 A%=-1.5:FOR I=A% TO A%+2:PRINT I:NEXT\n\
         20 A%=1.5:FOR I=A% TO A%+1:PRINT I:NEXT\n\
         30 END\n",
        &nums(&["-2", "-1", " 0", " 1", " 2"]),
    );
}
#[test]
fn float_var_line_local_forwarding_is_unaffected() {
    check_all_variants(
        "10 X=1.5:PRINT X:X=2.5:PRINT X\n20 X=-1.5:PRINT X:X=-2.5:PRINT X\n30 END\n",
        &nums(&[" 1.5", " 2.5", "-1.5", "-2.5"]),
    );
}
#[test]
fn string_var_line_local_forwarding_is_unaffected() {
    check_all_variants(
        "10 A$=\"HI\":PRINT A$:A$=\"YO\":PRINT A$\n\
         20 A$=\"ZA\":PRINT A$:A$=\"QQ\":PRINT A$\n\
         30 END\n",
        "HI\nYO\nZA\nQQ\n",
    );
}
