//! A string store nobody reads still raises the error the ROM raises for it.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

type Outcome = (String, Option<u8>, Option<u16>);

fn interpreted(src: &str) -> Outcome {
    let prg = tokenize_program(src).expect("tokenizes");
    let r = emu64::run_basic_to_end(&prg, 200_000_000).expect("runs in the interpreter");
    (
        common::petscii_to_string(&r.output),
        r.error_code,
        r.error_line,
    )
}

fn compiled(src: &str, profile: Profile) -> Outcome {
    let prg = tokenize_program(src).expect("tokenizes");
    let c = compile_with_options(
        &prg,
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}\n{src}"));
    let r = emu64::run_prg_to_end(&c.prg_bytes, 200_000_000).expect("runs");
    (
        common::petscii_to_string(&r.output),
        r.error_code,
        r.error_line,
    )
}

/// Every profile matches the ROM, and the ROM stops with `code` in `line`.
fn check(src: &str, code: u8, line: u16) {
    let want = interpreted(src);
    assert_eq!(
        (want.1, want.2),
        (Some(code), Some(line)),
        "the fixture must make the ROM fail:\n{src}"
    );
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        assert_eq!(compiled(src, profile), want, "{profile:?}:\n{src}");
    }
}

const STRING_TOO_LONG: u8 = 23;
const ILLEGAL_QUANTITY: u8 = 14;
const OVERFLOW: u8 = 15;

#[test]
fn a_concat_past_255_characters_fails_where_the_rom_does() {
    check(
        "10 FOR I=1 TO 255:A$=A$+\"X\":NEXT\n\
         20 C$=A$+\"Y\":PRINT \"DONE\"\n",
        STRING_TOO_LONG,
        20,
    );
}

/// An unread subroutine result raises an error before the caller string exceeds its limit.
#[test]
fn a_subroutine_whose_result_nobody_reads_fails_first() {
    check(
        "10 PRINT \"U1DB .,,DGGFG*CJFIH*. EBGGCFHEFBI--BE\",\n\
         90 FOR I=1 TO 11:B$=B$+CHR$(65+I):NEXT\n\
         150 FOR I=1 TO 9:B$=B$+CHR$(65+I):NEXT\n\
         180 IF LEN(B$)>60 THEN B$=\"\"\n\
         270 FOR I=1 TO 7:B$=B$+CHR$(65+I):NEXT\n\
         290 FOR I=1 TO 12:B$=B$+CHR$(65+I):NEXT\n\
         330 A$=B$+A$\n\
         370 GOSUB 950\n\
         380 IF LEN(B$)>60 THEN B$=\"\"\n\
         410 B$=B$+CHR$(65+I)\n\
         510 IF C<16 THEN 10\n\
         950 C$=A$+\"U55AH\":RETURN\n",
        STRING_TOO_LONG,
        950,
    );
}

#[test]
fn byte_arguments_out_of_range_fail_where_the_rom_does() {
    for (stmt, data) in [
        ("C$=CHR$(N)", "300"),
        ("C$=LEFT$(A$,N)", "-1"),
        ("C$=RIGHT$(A$,N)", "256"),
        ("C$=MID$(A$,N)", "0"),
        ("C$=MID$(A$,1,N)", "-2"),
    ] {
        check(
            &format!("10 A$=\"ABC\":READ N:{stmt}:PRINT \"DONE\"\n20 DATA {data}\n"),
            ILLEGAL_QUANTITY,
            10,
        );
    }
}

#[test]
fn a_str_argument_that_overflows_fails_where_the_rom_does() {
    check(
        "10 READ X:C$=STR$(X*X):PRINT \"DONE\"\n20 DATA 1E30\n",
        OVERFLOW,
        10,
    );
}
