mod common;

use yabcompiler_core::{
    CompileOptions, Dialect, Profile, compile_with_options, tokenize_program,
    tokenize_program_with,
};

fn compile(src: &str) -> yabcompiler_core::Compiled {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(&prg, CompileOptions::default()).unwrap_or_else(|e| panic!("compiles: {e}"))
}

fn compile_tsb(src: &str) -> yabcompiler_core::Compiled {
    let prg = tokenize_program_with(src, Dialect::Tsb).expect("tokenizes");
    let opts = CompileOptions {
        profile: Profile::Default,
        dialect: Dialect::Tsb,
        ..CompileOptions::default()
    };
    compile_with_options(&prg, opts).unwrap_or_else(|e| panic!("compiles: {e}"))
}

fn rom_error_banner(src: &str) -> Option<String> {
    let out = compile(src);
    let r = emu64::run_prg_to_end(&out.prg_bytes, common::MAX_INSNS).expect("runs");
    r.error_banner()
}

fn rom_error_banner_tsb(src: &str) -> Option<String> {
    let out = compile_tsb(src);
    let r = emu64::run_prg_to_end(&out.prg_bytes, common::MAX_INSNS).expect("runs");
    r.error_banner()
}

fn size(src: &str) -> usize {
    compile(src).machine_code.len()
}

#[test]
fn a_run_of_unerring_lines_still_reports_the_erring_one() {
    assert_eq!(
        rom_error_banner(
            "10 A%=1\n\
             20 A%=A%+1\n\
             30 A%=A%+2\n\
             40 POKE 1024,5\n\
             50 X=1/0\n"
        )
        .as_deref(),
        Some("?DIVISION BY ZERO  ERROR IN 50")
    );
}

/// Nested `FOR`s over integer counters.
#[test]
fn an_error_inside_nested_integer_for_loops_names_its_own_line() {
    assert_eq!(
        rom_error_banner(
            "10 A%=0\n\
             20 FOR I%=1 TO 3\n\
             30 FOR J%=1 TO 3\n\
             40 A%=A%+1\n\
             50 IF A%=5 THEN X=1/0\n\
             60 NEXT J%\n\
             70 NEXT I%\n"
        )
        .as_deref(),
        Some("?DIVISION BY ZERO  ERROR IN 50")
    );
}

/// Through a `GOSUB`, so the erring line is reached with a return address on the stack and the caller's line already overwritten.
#[test]
fn an_error_inside_a_gosub_names_the_subroutines_line() {
    assert_eq!(
        rom_error_banner(
            "10 GOSUB 100\n\
             20 END\n\
             100 A%=1\n\
             110 A%=A%+1\n\
             120 X=1/0\n"
        )
        .as_deref(),
        Some("?DIVISION BY ZERO  ERROR IN 120")
    );
}

#[test]
fn an_error_after_a_gosub_returns_names_the_callers_line() {
    assert_eq!(
        rom_error_banner(
            "10 A%=0\n\
             20 GOSUB 100\n\
             30 X=1/0\n\
             40 END\n\
             100 A%=A%+1\n\
             110 RETURN\n"
        )
        .as_deref(),
        Some("?DIVISION BY ZERO  ERROR IN 30")
    );
}

/// A computed `GOTO` reaches the runtime line table, which can `JMP` at any `L<n>:` in the program ; the successor set the analysis cannot name.
#[test]
fn an_error_after_a_computed_goto_names_the_landing_line() {
    assert_eq!(
        rom_error_banner_tsb(
            "5 DATA 301\n\
             10 READ X\n\
             20 CGOTO X\n\
             300 PRINT 1/1\n\
             301 X=1/0\n"
        )
        .as_deref(),
        Some("?DIVISION BY ZERO  ERROR IN 301")
    );
}

#[test]
fn a_body_starting_on_the_for_header_line_keeps_reporting_the_header() {
    assert_eq!(
        rom_error_banner(
            "10 FOR I=1 TO 2:IF I=2 THEN X=1/0\n\
             20 NEXT\n"
        )
        .as_deref(),
        Some("?DIVISION BY ZERO  ERROR IN 10")
    );
}

/// The 256-line boundary, where the high byte is not a whole-module invariant and both halves of the stamp are in play independently.
#[test]
fn an_error_across_the_high_byte_boundary_names_the_right_line() {
    assert_eq!(
        rom_error_banner(
            "10 GOTO 250\n\
             250 A%=1\n\
             255 A%=A%+1\n\
             300 A%=A%+1\n\
             310 X=1/0\n"
        )
        .as_deref(),
        Some("?DIVISION BY ZERO  ERROR IN 310")
    );
}

#[test]
fn the_high_byte_is_not_carried_past_the_line_that_owns_it() {
    assert_eq!(
        rom_error_banner(
            "10 GOTO 250\n\
             250 A%=1\n\
             255 X=1/0\n\
             300 A%=A%+1\n\
             310 PRINT A%\n"
        )
        .as_deref(),
        Some("?DIVISION BY ZERO  ERROR IN 255")
    );
}

#[test]
fn an_error_on_a_print_line_names_the_print_line() {
    assert_eq!(
        rom_error_banner(
            "10 A%=1\n\
             20 A%=A%+1\n\
             30 PRINT \"X\";ASC(\"\")\n"
        )
        .as_deref(),
        Some("?ILLEGAL QUANTITY  ERROR IN 30")
    );
}

#[test]
fn an_error_in_a_float_for_body_names_the_body_line() {
    assert_eq!(
        rom_error_banner(
            "10 Q=0\n\
             20 FOR I=1 TO 3\n\
             30 Q=Q+1\n\
             40 IF Q=2 THEN Z=1/0\n\
             50 NEXT I\n"
        )
        .as_deref(),
        Some("?DIVISION BY ZERO  ERROR IN 40")
    );
}

#[test]
fn peeking_curlin_reads_the_stamps_and_they_survive() {
    let src = "10 A%=1\n\
               20 A%=A%+1\n\
               30 PRINT PEEK(57);\n\
               40 A%=A%+1\n\
               50 PRINT PEEK(57)\n";
    let out = compile(src);
    let r = emu64::run_prg_to_end(&out.prg_bytes, common::MAX_INSNS).expect("runs");
    assert!(r.clean_exit, "must run to the end");
    assert_eq!(
        common::petscii_to_string(&r.output)
            .split_whitespace()
            .collect::<Vec<_>>(),
        vec!["30", "50"],
        "each PRINT must see its own line number"
    );
}

#[test]
fn an_unerring_integer_loop_pays_for_no_stamps() {
    let with_loop = size(
        "10 A%=1\n\
         20 FOR I%=1 TO 10\n\
         30 A%=A%+1\n\
         40 NEXT I%\n\
         50 PRINT A%\n\
         60 X=1/0\n",
    );

    let erring = size(
        "10 A%=1\n\
         20 FOR I%=1 TO 10\n\
         30 A%=A%+1:Z=Z/2\n\
         40 NEXT I%\n\
         50 PRINT A%\n\
         60 X=1/0\n",
    );
    assert!(
        erring > with_loop,
        "the erring body must pay for stamps the clean one does not: \
         clean={with_loop} erring={erring}"
    );
}

#[test]
fn a_program_that_cannot_raise_keeps_no_low_byte_stamp() {
    let asm = compile(
        "10 A%=1\n\
         20 A%=2\n\
         30 POKE 1024,5\n\
         40 POKE 1025,6\n\
         50 END\n",
    )
    .asm;
    assert!(
        !asm.contains("STA $39"),
        "nothing here can raise, so nothing may stamp $39:\n{asm}"
    );
}

#[test]
fn an_unprovable_percent_store_keeps_its_stamp() {
    let asm = compile(
        "10 A%=1\n\
         20 A%=A%+1\n\
         30 POKE 1024,5\n\
         50 END\n",
    )
    .asm;
    assert!(
        asm.contains("STA $39"),
        "a `%` store that can raise must stamp the line it is on:\n{asm}"
    );
}

#[test]
fn one_erring_statement_brings_its_own_stamp_back() {
    let asm = compile(
        "10 A%=1\n\
         20 A%=A%+1\n\
         30 POKE 1024,5\n\
         40 PRINT A%\n\
         50 END\n",
    )
    .asm;
    assert!(
        asm.contains("STA $39"),
        "line 40 PRINTs through $AB1E, which can reach $A437:\n{asm}"
    );

    assert!(
        asm.contains("LDA #$28"),
        "the surviving stamp must carry 40 ($28):\n{asm}"
    );
}

#[test]
fn a_goto_to_nowhere_still_names_its_own_line() {
    assert_eq!(
        rom_error_banner(
            "10 A%=1\n\
             20 A%=A%+1\n\
             30 GOTO 999\n"
        )
        .as_deref(),
        Some("?UNDEF'D STATEMENT  ERROR IN 30")
    );
}

#[test]
fn a_trapping_peek_names_its_own_line_in_integer_contexts() {
    for line in [20, 255, 256, 267] {
        for stmt in [
            "A=PEEK(-1)",
            "A%=PEEK(-32768)",
            "A%=1+PEEK(-1)",
            "IF PEEK(-1)=0 THEN GOTO 280",
            "A%=PEEK(65536)",
            "PRINT PEEK(-1)",
        ] {
            let src = format!("10 PRINT \"START\"\n{line} {stmt}\n280 END\n");
            let expected = format!("?ILLEGAL QUANTITY  ERROR IN {line}");
            assert_eq!(
                rom_error_banner(&src).as_deref(),
                Some(expected.as_str()),
                "{src}"
            );
        }
    }
}

#[test]
fn an_in_range_peek_still_completes_without_an_address_error() {
    let out = compile("10 A%=PEEK(1024)\n20 PRINT \"123\"\n30 END\n");
    let run = emu64::run_prg_to_end(&out.prg_bytes, common::MAX_INSNS).expect("runs");
    assert_eq!(run.error_banner(), None);
    assert!(run.clean_exit);
    assert_eq!(run.output, b"123\r");
}
