mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

fn build(src: &str, profile: Profile) -> (Vec<u8>, String) {
    let prg = tokenize_program(src).expect("tokenizes");
    let c = compile_with_options(
        &prg,
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}\n{src}"));
    (c.prg_bytes, c.asm)
}

/// Assert `src` runs clean and prints `out`, on every profile.
fn check_ok(src: &str, out: &str) {
    for profile in PROFILES {
        let (prg, _) = build(src, profile);
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

#[test]
fn each_read_publishes_the_line_its_item_came_from() {
    check_ok(
        "10 READ A:PRINT A;PEEK(63);PEEK(64)\n\
         20 READ A:PRINT A;PEEK(63);PEEK(64)\n\
         30 READ A:PRINT A;PEEK(63);PEEK(64)\n\
         100 DATA 1\n\
         110 DATA 2\n\
         120 DATA 3\n",
        " 1  100  0 \n 2  110  0 \n 3  120  0",
    );
}

/// A line with several items keeps naming that line for all of them, and only moves on when the cursor crosses into the next `DATA`.
#[test]
fn a_multi_item_line_is_named_for_every_one_of_its_items() {
    check_ok(
        "10 FOR I=1 TO 4:READ A:PRINT PEEK(63);:NEXT\n\
         40 DATA 1,2,3\n\
         50 DATA 4\n",
        " 40  40  40  50",
    );
}

#[test]
fn the_high_byte_is_published_too() {
    check_ok(
        "10 READ A:PRINT PEEK(63);PEEK(64)\n\
         30000 DATA 7\n",
        " 48  117",
    );
}

#[test]
fn restore_moves_the_published_line_backwards() {
    check_ok(
        "10 READ A:READ A:READ A:PRINT PEEK(63);\n\
         20 RESTORE:READ A:PRINT PEEK(63);\n\
         40 DATA 1,2\n\
         50 DATA 3\n",
        " 50  40",
    );
}

#[test]
fn nothing_is_published_before_the_first_read() {
    check_ok(
        "10 PRINT PEEK(63);PEEK(64);\n\
         20 READ A:PRINT PEEK(63)\n\
         40 DATA 9\n",
        " 0  0  40",
    );
}

#[test]
fn the_values_read_are_still_the_pool_entries() {
    check_ok(
        "10 FOR I=1 TO 6:READ A:PRINT A;:NEXT\n\
         40 DATA 11,22\n\
         50 DATA 33\n\
         60 DATA 44,55,66\n",
        " 11  22  33  44  55  66",
    );
}

/// String items and mixed pools go through a different consumer; the line has to be published for them too, and their contents must survive it.
#[test]
fn string_reads_publish_the_line_and_keep_their_text() {
    check_ok(
        "10 READ A$:PRINT A$;PEEK(63)\n\
         20 READ A$:PRINT A$;PEEK(63)\n\
         40 DATA HELLO\n\
         50 DATA WORLD\n",
        "HELLO 40 \nWORLD 50",
    );
}

#[test]
fn a_loop_that_terminates_on_the_data_line_number_terminates() {
    check_ok(
        "10 A=PEEK(63):IF A=90 THEN PRINT\"DONE\":END\n\
         20 READ B:PRINT A;B;\n\
         30 GOTO 10\n\
         70 DATA 1\n\
         80 DATA 2\n\
         90 DATA 3\n",
        " 0  1  70  2  80  3 DONE",
    );
}

#[test]
fn a_program_that_cannot_look_pays_nothing() {
    for profile in PROFILES {
        let (_, asm) = build(
            "10 FOR I=1 TO 3:READ A:PRINT A;:NEXT\n\
             40 DATA 1,2,3\n",
            profile,
        );
        assert!(
            !asm.contains("__DATA_SETLINE"),
            "untracked program emitted the DATLIN helper ({profile:?})"
        );
        assert!(
            !asm.contains("__DATA_LINETAB"),
            "untracked program emitted the DATLIN table ({profile:?})"
        );
    }
}

#[test]
fn a_peek_of_a_different_zero_page_byte_does_not_switch_it_on() {
    for profile in PROFILES {
        let (_, asm) = build(
            "10 READ A:PRINT PEEK(45);PEEK(65)\n\
             40 DATA 1\n",
            profile,
        );
        assert!(
            !asm.contains("__DATA_SETLINE"),
            "neighbouring PEEK switched DATLIN tracking on ({profile:?})"
        );
    }
}

#[test]
fn the_table_has_one_record_per_data_line_in_pool_order() {
    for profile in PROFILES {
        let (_, asm) = build(
            "10 READ A:PRINT PEEK(63)\n\
             40 DATA 1,2\n\
             50 DATA 3\n\
             60 DATA 4\n",
            profile,
        );
        assert!(asm.contains("__DATA_SETLINE"), "({profile:?})");
        let tab = asm
            .split("__DATA_LINETAB:")
            .nth(1)
            .unwrap_or_else(|| panic!("no DATLIN table ({profile:?})"));
        let words: Vec<&str> = tab
            .lines()
            .skip(1)
            .take_while(|l| l.trim_start().starts_with(".word"))
            .map(|l| l.trim())
            .collect();
        assert_eq!(
            words,
            vec![
                ".word __DATA_LINE_40, 40",
                ".word __DATA_LINE_50, 50",
                ".word __DATA_LINE_60, 60",
                ".word $0000",
            ],
            "({profile:?})"
        );
    }
}

#[test]
fn a_peek_with_no_data_pool_emits_no_table() {
    for profile in PROFILES {
        let (_, asm) = build("10 PRINT PEEK(63);PEEK(64)\n", profile);
        assert!(!asm.contains("__DATA_LINETAB"), "({profile:?})");
    }
}
