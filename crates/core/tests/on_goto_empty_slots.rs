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

#[track_caller]
fn check_ok(src: &str, out: &str) {
    for profile in PROFILES {
        let prg = build(src, profile);
        let r = emu64::run_prg_to_end(&prg, common::MAX_INSNS).expect("run");
        assert_eq!(
            r.error_banner(),
            None,
            "no error expected ({profile:?}):\n{src}"
        );
        assert!(r.clean_exit, "clean exit expected ({profile:?}):\n{src}");
        assert_eq!(
            common::petscii_to_string(&r.output).trim(),
            out,
            "({profile:?}):\n{src}"
        );
    }
}

#[track_caller]
fn check_error(src: &str, out: &str, banner: &str) {
    for profile in PROFILES {
        let prg = build(src, profile);
        let r = emu64::run_prg_to_end(&prg, common::MAX_INSNS).expect("run");
        assert!(!r.clean_exit, "must not exit cleanly ({profile:?}):\n{src}");
        assert_eq!(
            common::petscii_to_string(&r.output).trim(),
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
fn the_rom_probe_program_reproduces() {
    check_error(
        "10 on 1 goto 100\n\
         100 print\"a1\"\n\
         110 on 3 goto 200,,210\n\
         210 print\"a3\"\n\
         220 goto 300:junk here\n\
         300 print\"a4\"\n\
         310 on 7 gosub 400,400,400,\n\
         320 print\"a5\"\n\
         330 on 2 goto 500,,510\n\
         340 print\"notreached\"\n\
         400 return\n\
         500 print\"w1\":end\n\
         510 print\"w2\":end\n",
        "A1\nA3\nA4\nA5",
        "?UNDEF'D STATEMENT  ERROR IN 330",
    );
}

#[test]
fn an_empty_slot_occupies_a_position() {
    // Index 3 must reach the third entry, not the second.
    check_ok(
        "10 on 3 goto 100,,120\n20 print\"fall\"\n30 end\n\
         100 print\"one\":end\n120 print\"three\":end\n",
        "THREE",
    );
    // Index 1 still reaches the first entry.
    check_ok(
        "10 on 1 goto 100,,120\n20 print\"fall\"\n30 end\n\
         100 print\"one\":end\n120 print\"three\":end\n",
        "ONE",
    );
}

#[test]
fn several_empty_slots_in_a_row_each_count() {
    check_ok(
        "10 on 4 goto 100,,,130\n20 print\"fall\"\n30 end\n\
         100 print\"one\":end\n130 print\"four\":end\n",
        "FOUR",
    );
}

#[test]
fn a_leading_empty_slot_counts() {
    check_ok(
        "10 on 2 goto ,110\n20 print\"fall\"\n30 end\n110 print\"two\":end\n",
        "TWO",
    );
}

/// Same rule for `GOSUB`, and the return lands after the `ON`.
#[test]
fn empty_slots_work_for_on_gosub() {
    check_ok(
        "10 on 3 gosub 100,,120\n20 print\"back\":end\n\
         100 print\"one\":return\n120 print\"three\":return\n",
        "THREE\nBACK",
    );
}

#[test]
fn selecting_an_empty_slot_is_undefd_statement() {
    check_error(
        "10 on 2 goto 100,,120\n20 print\"fall\"\n30 end\n\
         100 print\"one\":end\n120 print\"three\":end\n",
        "",
        "?UNDEF'D STATEMENT  ERROR IN 10",
    );
}

#[test]
fn selecting_an_empty_slot_is_undefd_statement_for_gosub_too() {
    check_error(
        "10 on 2 gosub 100,,120\n20 print\"back\":end\n\
         100 print\"one\":return\n120 print\"three\":return\n",
        "",
        "?UNDEF'D STATEMENT  ERROR IN 10",
    );
}

/// A list with no numbers at all is a one-entry list holding line 0: index 1 raises, index 0 falls through.
#[test]
fn an_entirely_empty_list_is_one_slot_of_line_zero() {
    check_error(
        "10 on 1 goto\n20 print\"fall\"\n",
        "",
        "?UNDEF'D STATEMENT  ERROR IN 10",
    );
    check_ok("10 on 0 goto\n20 print\"fall\"\n", "FALL");
}

#[test]
fn an_index_past_a_trailing_comma_list_falls_through() {
    check_ok(
        "10 on 7 gosub 100,100,100,\n20 print\"fall\":end\n100 print\"hit\":return\n",
        "FALL",
    );
}

#[test]
fn a_trailing_comma_adds_exactly_one_slot() {

    check_error(
        "10 on 4 goto 100,100,100,\n20 print\"fall\"\n30 end\n100 print\"hit\":end\n",
        "",
        "?UNDEF'D STATEMENT  ERROR IN 10",
    );
    check_ok(
        "10 on 5 goto 100,100,100,\n20 print\"fall\":end\n100 print\"hit\":end\n",
        "FALL",
    );
}

#[test]
fn a_trailing_comma_does_not_eat_the_next_statement() {
    check_ok(
        "10 on 9 goto 100,:print\"same line\"\n20 print\"next\":end\n100 print\"hit\":end\n",
        "SAME LINE\nNEXT",
    );
}

#[test]
fn junk_after_an_unconditional_goto_never_executes() {
    check_ok("10 goto 30:junk here\n20 print\"skipped\"\n30 print\"ok\"\n", "OK");
}

#[test]
fn a_well_formed_list_still_works() {
    for (idx, want) in [(1, "ONE"), (2, "TWO"), (3, "THREE")] {
        check_ok(
            &format!(
                "10 on {idx} goto 100,110,120\n20 print\"fall\"\n30 end\n\
                 100 print\"one\":end\n110 print\"two\":end\n120 print\"three\":end\n"
            ),
            want,
        );
    }
    check_ok(
        "10 on 0 goto 100,110,120\n20 print\"fall\":end\n\
         100 print\"one\":end\n110 print\"two\":end\n120 print\"three\":end\n",
        "FALL",
    );
    check_ok(
        "10 on 4 goto 100,110,120\n20 print\"fall\":end\n\
         100 print\"one\":end\n110 print\"two\":end\n120 print\"three\":end\n",
        "FALL",
    );
}

#[test]
fn spaces_do_not_change_the_slot_count() {
    check_ok(
        "10 on 3 goto 100 , , 120\n20 print\"fall\"\n30 end\n\
         100 print\"one\":end\n120 print\"three\":end\n",
        "THREE",
    );
}
