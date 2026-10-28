//! Concatenating constants past 255 characters raises ?STRING TOO LONG, as in the ROM.

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

fn check(src: &str) -> Outcome {
    let want = interpreted(src);
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        assert_eq!(compiled(src, profile), want, "{profile:?}:\n{src}");
    }
    want
}

const STRING_TOO_LONG: u8 = 23;

/// `A$="<n X>"` on line 10, then `tail` on line 20.
fn program(n: usize, tail: &str) -> String {
    format!("10 A$=\"{}\"\n20 {tail}\n", "X".repeat(n))
}

#[test]
fn a_constant_concat_past_255_fails() {
    for tail in [
        "PRINT LEN(A$+A$+A$+A$)",
        "B$=A$+A$+A$+A$:PRINT \"DONE\"",
        "PRINT A$+A$+A$+A$",
        "B$=A$+A$+A$+A$:PRINT B$",
    ] {
        let got = check(&program(70, tail));
        assert_eq!((got.1, got.2), (Some(STRING_TOO_LONG), Some(20)), "{tail}");
    }
}

#[test]
fn a_constant_concat_of_exactly_255_still_works() {
    let got = check(&program(85, "B$=A$+A$+A$:PRINT LEN(B$);RIGHT$(B$,3)"));
    assert_eq!(got, (" 255 XXX\n".to_string(), None, None));
}
