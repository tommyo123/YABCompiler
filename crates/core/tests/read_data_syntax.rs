mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

struct Built {
    prg_bytes: Vec<u8>,
    asm: String,
}

fn build(src: &str, profile: Profile) -> Built {
    let prg = tokenize_program(src).expect("tokenizes");
    let c = compile_with_options(
        &prg,
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}\n{src}"));
    Built {
        prg_bytes: c.prg_bytes,
        asm: common::expand_call_entries(&c.asm),
    }
}

/// Assert `src` prints `out` and then dies with `banner`, on every profile.
fn check_error(src: &str, out: &str, banner: &str) {
    for profile in PROFILES {
        let built = build(src, profile);
        let r = emu64::run_prg_to_end(&built.prg_bytes, common::MAX_INSNS).expect("run");
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

/// Assert `src` runs to a clean exit printing `out`, on every profile.
fn check_ok(src: &str, out: &str) {
    for profile in PROFILES {
        let built = build(src, profile);
        let r = emu64::run_prg_to_end(&built.prg_bytes, common::MAX_INSNS).expect("run");
        assert_eq!(
            r.error_banner(),
            None,
            "no error expected ({profile:?}):\n{src}"
        );
        assert!(r.clean_exit, "clean exit expected ({profile:?}):\n{src}");
        assert_eq!(
            common::petscii_to_string(&r.output).trim_end(),
            out,
            "({profile:?}):\n{src}"
        );
    }
}

#[test]
fn read_of_a_non_numeric_data_item_is_syntax_error_in_the_data_line() {
    check_error(
        "10 read a\n20 print a\n100 data zz\n",
        "",
        "?SYNTAX  ERROR IN 100",
    );
}

#[test]
fn the_cited_line_follows_the_data_statement() {
    check_error(
        "10 read a\n20 print a\n30 data zz\n",
        "",
        "?SYNTAX  ERROR IN 30",
    );
}

/// Reject malformed numeric DATA when READ reaches it.
#[test]
fn a_quoted_item_with_trailing_letters_is_syntax_error() {
    check_error("10 read a:print a\n20 data \"5x\"\n", "", "?SYNTAX  ERROR IN 20");
}

#[test]
fn a_quoted_item_is_refused_even_when_it_is_a_clean_number() {
    check_error("10 read a:print a\n20 data \"5\"\n", "", "?SYNTAX  ERROR IN 20");
}

#[test]
fn items_fin_consumes_whole_still_read_as_numbers() {

    check_ok("10 read a:print a\n20 data 1e3\n", " 1000");
    check_ok("10 read a:print a\n20 data 007\n", " 7");
    check_ok("10 read a:print a\n20 data .5\n", " .5");
    // CHRGET skips spaces, so this is twelve, not one.
    check_ok("10 read a:print a\n20 data 1 2\n", " 12");
    check_ok("10 read a:print a\n20 data 5   \n", " 5");
    check_ok("10 read a:print a\n20 data - 5\n", "-5");
    // An exponent may be empty or sign-only.
    check_ok("10 read a:print a\n20 data 1e\n", " 1");
    check_ok("10 read a:print a\n20 data 1e+\n", " 1");
    // Sign alone, point alone, and nothing at all are all zero.
    check_ok("10 read a:print a\n20 data +\n", " 0");
    check_ok("10 read a:print a\n20 data .\n", " 0");
    check_ok("10 read a:print a\n20 data\n", " 0");
}

#[test]
fn items_fin_cannot_finish_are_refused() {
    // A second decimal point is not consumable, so something is left over and the terminator test fails.
    check_error("10 read a:print a\n20 data ..\n", "", "?SYNTAX  ERROR IN 20");
    check_error("10 read a:print a\n20 data 1.2.3\n", "", "?SYNTAX  ERROR IN 20");
    check_error("10 read a:print a\n20 data 1x\n", "", "?SYNTAX  ERROR IN 20");
    // Spaces are skipped, so this is not "5 then a terminator" ; it is 5 followed by an X that nothing can eat.
    check_error("10 read a:print a\n20 data 5 x\n", "", "?SYNTAX  ERROR IN 20");
}

/// The refusal is per item, not per pool: everything before the bad one reads normally and prints, and only then does the program stop.
#[test]
fn items_before_the_bad_one_still_read() {
    check_error(
        "10 for i=1 to 3:read a:print a:next\n20 data 1,2,zz\n",
        " 1 \n 2",
        "?SYNTAX  ERROR IN 20",
    );
}

#[test]
fn the_blamed_line_is_the_one_the_failing_item_came_from() {
    check_error(
        "10 read b:print b\n20 read c:print c\n30 data 1\n40 data zz\n",
        " 1",
        "?SYNTAX  ERROR IN 40",
    );
}

#[test]
fn integer_and_array_read_targets_validate_too() {
    check_error("10 read a%:print a%\n20 data zz\n", "", "?SYNTAX  ERROR IN 20");
    check_error(
        "10 dim q(3):read q(1):print q(1)\n20 data zz\n",
        "",
        "?SYNTAX  ERROR IN 20",
    );
}

#[test]
fn a_string_read_of_the_same_item_is_unaffected() {
    check_ok("10 read a$:print a$\n20 data zz\n", "ZZ");
    check_ok(
        "10 read a$:print a$\n20 restore\n30 read a$:print a$\n40 data zz\n",
        "ZZ\nZZ",
    );

    check_ok("10 read a$:read b:print a$;b\n20 data zz,7\n", "ZZ 7");
}

#[test]
fn a_pool_no_numeric_read_touches_emits_no_check() {
    let asm = build("10 read a$:print a$\n20 data zz\n", Profile::Default).asm;
    assert!(
        !asm.contains("__READ_NUM") && !asm.contains("__DATA_BAD"),
        "string-only READ must not pull in the item check:\n{asm}"
    );
}

/// The whole-program fast path: every entry parses, so the numeric `READ` calls `__VAL_HELPER` directly, exactly as before, and no table exists.
#[test]
fn a_clean_ascii_pool_still_calls_val_helper_directly() {

    let asm = build(
        "10 read b:read c:read a$:print b;c;a$\n20 data 1e3,007,5\n",
        Profile::Default,
    )
    .asm;
    assert!(
        asm.contains("JSR __VAL_HELPER"),
        "clean pool must reach VAL in one call:\n{asm}"
    );
    assert!(
        !asm.contains("__READ_NUM") && !asm.contains("__DATA_BAD"),
        "clean pool must not emit the item check:\n{asm}"
    );
    check_ok("10 read b:read c:read a$:print b;c;a$\n20 data 1e3,007,5\n", " 1000  7 5");
}

#[test]
fn a_binary_pool_still_bypasses_the_parser_entirely() {
    let asm = build("10 read a:read b:print a;b\n20 data 3,4\n", Profile::Default).asm;
    assert!(
        !asm.contains("__VAL_HELPER") && !asm.contains("__READ_NUM"),
        "binary DATA pool must not touch the parser:\n{asm}"
    );
    check_ok("10 read a:read b:print a;b\n20 data 3,4\n", " 3  4");
}

/// The check is a table walk, so it has to survive a pool with more than one refusable entry ; and reach the second one with the right line.
#[test]
fn several_refusable_entries_each_carry_their_own_line() {
    let src = "10 read a$:print a$\n\
               20 read b$:print b$\n\
               30 read c$:print c$\n\
               40 read d\n\
               50 data zz\n\
               60 data yy\n\
               70 data xx\n\
               80 data ww\n";
    check_error(src, "ZZ\nYY\nXX", "?SYNTAX  ERROR IN 80");
}

#[test]
fn adjacent_refusable_entries_collapse_into_one_range() {
    let asm = build(
        "10 read a$:read b$:read n:print a$;b$;n\n\
         20 data aa,bb,cc,dd,ee,ff,gg,hh,ii,jj,kk,ll\n\
         30 data 7\n",
        Profile::Default,
    )
    .asm;
    let records = asm.matches("    .word __DBAD_S").count();
    assert_eq!(records, 1, "twelve adjacent items are one range:\n{asm}");
}

#[test]
fn the_table_walk_does_not_disturb_the_staged_entry_pointer() {
    let asm = build("10 read a:print a\n20 data zz\n", Profile::Default).asm;
    let helper = asm
        .split_once("__READ_NUM:")
        .expect("helper exists")
        .1
        .split_once("__VAL_HELPER:")
        .expect("helper ends at VAL")
        .0;
    // Address operands only ; `LDY #$04` is an immediate index into a table record, not a zero-page cell.
    let addresses: Vec<&str> = helper
        .split('$')
        .skip(1)
        .filter(|_| true)
        .zip(helper.match_indices('$').map(|(i, _)| i))
        .filter(|(_, i)| *i == 0 || helper.as_bytes()[i - 1] != b'#')
        .map(|(rest, _)| &rest[..rest.len().min(2)])
        .collect();
    assert!(
        !addresses.contains(&"03") && !addresses.contains(&"04"),
        "the walk must not touch STR_OP_LHS: {addresses:?}\n{helper}"
    );
    assert!(
        addresses.contains(&"05") && addresses.contains(&"06"),
        "the walk should be using STR_OP_RHS as its cursor: {addresses:?}\n{helper}"
    );
}

#[test]
fn running_off_the_pool_is_still_out_of_data() {
    check_error(
        "10 read a$:print a$\n20 read b\n30 data zz\n",
        "ZZ",
        "?OUT OF DATA  ERROR IN 20",
    );
}
