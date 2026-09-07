mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn run(source: &str, expected: &[&str]) {
    for profile in [Profile::Default, Profile::Size, Profile::Speed] {
        for banked in [false, true] {
            let built = compile_with_options(
                &tokenize_program(source).unwrap(),
                CompileOptions {
                    profile,
                    extraram: banked,
                    force_extraram_off: !banked,
                    ..CompileOptions::default()
                },
            )
            .unwrap();
            let result = emu64::run_prg_to_end(&built.prg_bytes, common::MAX_INSNS).unwrap();
            assert!(result.clean_exit, "{profile:?}/{banked}");
            assert_eq!(result.error_banner(), None, "{profile:?}/{banked}");
            let text = common::petscii_to_string(&result.output);
            assert_eq!(
                text.split_whitespace().collect::<Vec<_>>(),
                expected,
                "{profile:?}/{banked}"
            );
            // Plain output retains BSS.
            assert_eq!(built.asm.contains("__BSS_START:"), !banked);
            assert!(built.asm.contains("__DATA_BAD:"));
            for label in ["__DATA_BAD:", "__DATA_LINETAB:"] {
                if let Some(table) = built.asm.split(label).nth(1) {
                    let mut terminated = false;
                    for line in table.lines() {
                        if line.trim() == ".word $0000" {
                            terminated = true;
                            break;
                        }
                        assert!(
                            !line.contains("__BSS_START:") && !line.contains("peephole-factored"),
                            "{profile:?}/{banked}: split {label}"
                        );
                    }
                    assert!(
                        terminated,
                        "{profile:?}/{banked}: missing {label} terminator"
                    );
                }
            }
        }
    }
}

#[test]
fn string_then_numeric_reads_cross_the_rejection_table_terminator() {
    run(
        "10 DIM A$(40),H(40)\n20 FOR I=1 TO 2:READ A$(I):NEXT\n30 FOR I=1 TO 2:READ H(I):NEXT\n40 PRINT A$(1);\" \";A$(2);H(1);H(2)\n50 A=.5:FOR J=1 TO 3:A=1+A:NEXT:PRINT A\n100 DATA ONE,TWO\n200 DATA 7,8\n",
        &["ONE", "TWO", "7", "8", "3.5"],
    );
}

#[test]
fn published_data_lines_keep_their_own_terminator_with_zero_arrays() {
    run(
        "10 DIM A$(40),H(40)\n20 READ A$(1):PRINT PEEK(63)+256*PEEK(64)\n30 READ H(1):PRINT PEEK(63)+256*PEEK(64);H(1)\n40 RESTORE:READ A$(2):PRINT PEEK(63)+256*PEEK(64);A$(2)\n100 DATA ONE\n60000 DATA 7\n",
        &["100", "60000", "7", "100", "ONE"],
    );
}
