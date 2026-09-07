//! DATA error and line boundaries survive ordinary extraram data hoisting.
mod common;
use yabcompiler_core::{compile_with_options, tokenize_program, CompileOptions, Profile};

fn check(source: &str, output: &str, error: Option<&str>) {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        for banked in [false, true] {
            let built = compile_with_options(&tokenize_program(source).unwrap(), CompileOptions {
                profile, extraram: banked, force_extraram_off: !banked,
                ..CompileOptions::default()
            }).unwrap();
            let result = emu64::run_prg_to_end(&built.prg_bytes, common::MAX_INSNS).unwrap();
            assert_eq!(common::petscii_to_string(&result.output).trim_end(), output,
                "{profile:?}/{banked}: {source}");
            assert_eq!(result.error_banner().as_deref(), error,
                "{profile:?}/{banked}: {source}");
            if banked && built.asm.contains("__DATA_BAD:") {
                let lines: Vec<_> = built.asm.lines().collect();
                let start = lines.iter().position(|line| line.trim() == "__DATA:").unwrap();
                let end = lines.iter().position(|line| line.trim() == "__DATA_END:").unwrap();
                for (i, line) in lines.iter().enumerate() {
                    if line.starts_with("__DBAD_") || line.starts_with("__DATA_LINE_") {
                        assert!(start < i && i < end, "stranded DATA anchor: {line}");
                    }
                }
            }
        }
    }
}

#[test]
fn first_middle_and_last_rejected_items_keep_their_data_line() {
    for (source, output, error) in [
        ("10 READ A:PRINT A\n100 DATA ONE\n", "", "?SYNTAX  ERROR IN 100"),
        ("10 READ A$:READ B:PRINT B\n100 DATA \"ONE\"\n200 DATA \"TWO\"\n", "", "?SYNTAX  ERROR IN 200"),
        ("10 READ A$:READ B:PRINT B\n100 DATA ONE\n200 DATA 1X\n300 DATA 7\n", "", "?SYNTAX  ERROR IN 200"),
        ("10 READ A$:READ B:PRINT B\n100 DATA ONE\n200 DATA \"7\"\n", "", "?SYNTAX  ERROR IN 200"),
        ("10 DIM A(2):READ A$:READ A(1)\n100 DATA ONE\n60000 DATA TWO\n", "", "?SYNTAX  ERROR IN 60000"),
        ("10 DIM A%(2):READ A$:READ A%(1)\n100 DATA ONE\n256 DATA TWO\n", "", "?SYNTAX  ERROR IN 256"),
        ("10 READ A$:PRINT A$:READ B\n100 DATA ONE\n", "ONE", "?OUT OF DATA  ERROR IN 10"),
    ] { check(source, output, Some(error)); }
}

#[test]
fn accepted_gaps_string_aliases_restore_and_datlin_are_preserved() {
    check("10 READ A$,B$,C:PRINT A$;B$;C\n20 RESTORE:READ D$:PRINT D$\n100 DATA ONE,TWO,7\n", "ONETWO 7 \nONE", None);
    check("10 READ A$:PRINT PEEK(63)+256*PEEK(64)\n20 READ B$:PRINT PEEK(63)+256*PEEK(64)\n30 READ C:PRINT PEEK(63)+256*PEEK(64);C\n100 DATA \"ONE\"\n200 DATA \"TWO\"\n300 DATA 7\n", " 100 \n 200 \n 300  7", None);
    check("10 READ A$,B,C$,D:PRINT A$;B;C$;D\n100 DATA ONE,7,TWO,8\n", "ONE 7 TWO 8", None);
}

#[test]
fn rejection_table_and_data_pool_can_cross_pages() {
    let mut source = "10 FOR I=1 TO 69:READ A$:NEXT\n20 READ B:PRINT B\n".to_string();
    for i in 0..70 {
        source.push_str(&format!("{} DATA \"LONG ITEM {}\"\n", 100+i*5, i));
    }
    check(&source, "", Some("?SYNTAX  ERROR IN 445"));
}
