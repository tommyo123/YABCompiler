//! Global constants must respect BASIC's initial values across control flow.
mod common;
use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn check(source: &str, expected: &str) {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        for extraram in [false, true] {
            let c = compile_with_options(
                &tokenize_program(source).unwrap(),
                CompileOptions {
                    profile,
                    extraram,
                    ..CompileOptions::default()
                },
            )
            .unwrap();
            let r = emu64::run_prg_to_end(&c.prg_bytes, 20_000_000).unwrap();
            assert!(
                r.clean_exit,
                "{profile:?}/{extraram}: {:?}",
                r.error_banner()
            );
            assert_eq!(
                common::petscii_to_string(&r.output)
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
                expected,
                "{profile:?}/{extraram}\n{source}"
            );
        }
    }
}

#[test]
fn a_subroutine_can_read_a_for_limit_before_its_only_assignment() {
    check(
        "10 GOSUB 100:PRINT K\n20 XX=300:GOSUB 100:PRINT K\n30 END\n100 FOR K=1 TO XX:NEXT:RETURN\n",
        "2 301",
    );
}

#[test]
fn forward_call_reads_initial_numeric_and_string_values() {
    check(
        "10 GOSUB 100\n20 A=7:A$=\"SET\":GOSUB 100\n30 END\n100 PRINT A;LEN(A$):RETURN\n",
        "0 0 7 3",
    );
}

#[test]
fn transitive_fn_reads_are_charged_to_an_early_call() {
    check(
        "10 DEF FN F(X)=X+A\n20 DEF FN G(Y)=FN F(Y)+LEN(A$)\n30 GOSUB 100\n40 A=7:A$=\"SET\":GOSUB 100\n50 END\n100 PRINT FN G(1):RETURN\n",
        "1 11",
    );
}

#[test]
fn same_line_read_before_assignment_uses_initial_value() {
    check(
        "10 PRINT A;LEN(A$):A=7:A$=\"SET\":PRINT A;LEN(A$)\n",
        "0 0 7 3",
    );
}

#[test]
fn a_jump_can_skip_an_earlier_assignment() {
    check("10 GOTO 30\n20 A=7:A$=\"SET\"\n30 PRINT A;LEN(A$)\n", "0 0");
}

#[test]
fn multiple_nexts_can_reach_a_read_before_a_later_assignment() {
    check(
        "10 GOTO 100\n20 NEXT I:GOTO 200\n30 XX=3\n100 FOR I=1 TO 2\n110 FOR K=1 TO XX:NEXT K:PRINT K\n120 IF I=1 THEN 20\n130 NEXT I\n200 END\n",
        "2 2",
    );
}

#[test]
fn constants_initialized_before_all_reads_still_fold() {
    let source = "10 XX=300:A$=\"SET\"\n20 GOSUB 100:PRINT K\n30 END\n100 FOR K=1 TO XX:NEXT:PRINT LEN(A$):RETURN\n";
    let c = compile_with_options(
        &tokenize_program(source).unwrap(),
        CompileOptions::default(),
    )
    .unwrap();
    assert!(!c.asm.contains("V_XX"));
    assert!(!c.asm.contains("VS_A"));
    check(source, "3 301");
}
