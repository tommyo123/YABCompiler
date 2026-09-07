mod common;

use yabcompiler_core::{
    CompileOptions, Dialect, Profile, compile_with_options, tokenize_program_with,
};

fn check(source: &str, expected: &str) {
    let prg = tokenize_program_with(source, Dialect::Tsb).unwrap();
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let compiled = compile_with_options(
            &prg,
            CompileOptions {
                profile,
                dialect: Dialect::Tsb,
                ..CompileOptions::default()
            },
        )
        .unwrap();
        let result = emu64::run_prg_to_end(&compiled.prg_bytes, 2_000_000).unwrap();
        let output = common::petscii_to_string(&result.output);
        assert_eq!(
            result.error_code,
            None,
            "{profile:?}: {:?}",
            result.error_banner()
        );
        assert!(result.clean_exit, "{profile:?}: did not finish: {output:?}");
        assert_eq!(
            output.split_whitespace().collect::<Vec<_>>().join(" "),
            expected,
            "{profile:?}"
        );
    }
}

#[test]
fn handler_changes_same_line_numeric_and_string_values() {
    check(
        "10 ON KEY \"Q\" GOSUB 100\n20 A=1:A$=\"OLD\":POKE631,209:POKE198,1:PRINT A;A$\n30 END\n100 A=2:A$=\"NEW\":RETURN\n",
        "2 NEW",
    );
}

#[test]
fn handler_observes_store_overwritten_by_next_statement() {
    check(
        "10 ON KEY \"Q\" GOSUB100\n20 A=1:POKE631,209:POKE198,1:A=2:PRINT A\n30 END\n100 PRINT A:RETURN\n",
        "1 2",
    );
}

#[test]
fn handler_can_move_for_counter_past_limit() {
    check(
        "10 ON KEY \"Q\" GOSUB100\n20 FOR I=1 TO2\n30 POKE631,209:POKE198,1:PRINT I\n40 NEXT I\n50 PRINT I:END\n100 I=5:RETURN\n",
        "5 6",
    );
}

#[test]
fn handler_can_change_fill_value_between_iterations() {
    check(
        "10 ON KEY \"Q\" GOSUB100\n20 V=1:FOR I=1024 TO1026\n30 POKE631,209:POKE198,1:POKE I,V\n40 NEXT I\n50 PRINT PEEK(1024);PEEK(1025);PEEK(1026):END\n100 V=V+1:RETURN\n",
        "2 3 4",
    );
}

#[test]
fn callback_target_inside_proc_survives_named_call_inlining() {
    check(
        "10 ON KEY \"Q\" GOSUB100\n20 EXEC GREET\n30 A=0:POKE631,209:POKE198,1:PRINT A\n40 END\n100 PROC GREET\n110 A=A+1\n120 END PROC\n",
        "1",
    );
}

#[test]
fn callback_invalidates_byte_range_before_poke() {
    let source = "10 ON KEY \"Q\" GOSUB100\n20 A=1:POKE631,209:POKE198,1:POKE53280,A\n30 PRINT \"BAD\":END\n100 A=256:RETURN\n";
    let prg = tokenize_program_with(source, Dialect::Tsb).unwrap();
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let compiled = compile_with_options(
            &prg,
            CompileOptions {
                profile,
                dialect: Dialect::Tsb,
                ..CompileOptions::default()
            },
        )
        .unwrap();
        let result = emu64::run_prg_to_end(&compiled.prg_bytes, 2_000_000).unwrap();
        let output = common::petscii_to_string(&result.output);
        assert_eq!(
            result.error_code,
            Some(14),
            "{profile:?}: {:?}",
            result.error_banner()
        );
        assert_eq!(
            result.error_line,
            Some(20),
            "{profile:?}: {:?}",
            result.error_banner()
        );
        assert!(!output.contains("BAD"), "{profile:?}: {output:?}");
    }
}

#[test]
fn proc_callback_keeps_local_variables_separate() {
    check(
        "10 ON KEY \"Q\" GOSUB100\n20 A=7:POKE631,209:POKE198,1:PRINT A\n30 END\n100 PROC GREET\n110 LOCAL A\n120 A=3:PRINT A\n130 END PROC\n",
        "3 7",
    );
}
