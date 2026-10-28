//! Omit runtime checks and line tracking for valid programs.

mod common;

use yabcompiler_core::{
    CompileOptions, Dialect, Profile, compile_with_options, tokenize_program, tokenize_program_with,
};

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

fn build(src: &str, profile: Profile, unchecked: bool) -> yabcompiler_core::Compiled {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(
        &prg,
        CompileOptions {
            profile,
            unchecked,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}\n{src}"))
}

fn run(c: &yabcompiler_core::Compiled) -> Outcome {
    let r = emu64::run_prg_to_end(&c.prg_bytes, 200_000_000).expect("runs");
    (
        common::petscii_to_string(&r.output),
        r.error_code,
        r.error_line,
    )
}

fn unchecked_asm(src: &str) -> String {
    common::expand_call_entries(&build(src, Profile::Default, true).asm)
}

/// Arrays, GOSUB, FOR, READ, POKE/PEEK, strings and `%` arithmetic, all inside their limits.
const CLEAN: &[&str] = &[
    "10 DIM A(20),B%(5,5):FOR I=0 TO 20:A(I)=I*I:NEXT\n\
     20 FOR I=0 TO 5:FOR J=0 TO 5:B%(I,J)=I*J-7:NEXT:NEXT\n\
     30 S=0:FOR I=0 TO 20:S=S+A(I):NEXT:PRINT S;B%(5,5);B%(0,3)\n",
    "10 GOSUB 100:GOSUB 100:PRINT N:END\n\
     100 N=N+1:IF N<5 THEN GOSUB 100\n\
     110 RETURN\n",
    "10 READ A,B$,C%:PRINT A;B$;C%:RESTORE:READ X:PRINT X\n\
     20 DATA 1.5,HELLO,-7\n",
    "10 POKE 828,200:A=PEEK(828)+55:POKE 829,A AND 255:PRINT PEEK(829);CHR$(65+PEEK(828)-200)\n",
    "10 A$=\"\":FOR I=1 TO 40:A$=A$+CHR$(64+(I AND 15)):NEXT\n\
     20 PRINT LEN(A$);LEFT$(A$,3);MID$(A$,5,4);RIGHT$(A$,2);ASC(A$)\n",
    "10 A%=32000:B%=700:C%=A%+B%-1000:D%=-A%-B%+999:PRINT C%;D%\n\
     20 X=3:IF X>2 AND A%>0 THEN PRINT \"Y\"\n\
     30 DIM C(3):I=2:IF I<=3 AND C(I)=0 THEN PRINT \"Z\"\n",
    "10 ON 2 GOSUB 100,200:FOR I=1 TO 3:ON I GOTO 30,40,50\n\
     30 PRINT \"A\";:GOTO 60\n\
     40 PRINT \"B\";:GOTO 60\n\
     50 PRINT \"C\";\n\
     60 NEXT:END\n\
     100 PRINT 1:RETURN\n\
     200 PRINT 2:RETURN\n",
];

#[test]
fn a_program_without_errors_runs_as_in_the_rom() {
    for src in CLEAN {
        let want = interpreted(src);
        assert_eq!(
            want.1, None,
            "the fixture must run cleanly in the ROM:\n{src}"
        );
        for profile in [Profile::Default, Profile::Speed, Profile::Size] {
            assert_eq!(run(&build(src, profile, true)), want, "{profile:?}:\n{src}");
        }
    }
}

#[test]
fn checks_and_line_stamps_are_gone() {
    let asm = unchecked_asm(
        "10 DIM A(20):INPUT N:A(N)=1:GOSUB 100:READ X:POKE N,X\n\
         20 END\n\
         100 PRINT A(N+1):RETURN\n\
         110 DATA 5\n",
    );
    for gone in [
        "STA $39",
        "STA $3A",
        "__BNDS",
        "__GOSUB_CHK",
        "__RET_CHK",
        "__DATA_CHK",
    ] {
        assert!(!asm.contains(gone), "{gone} is left:\n{asm}");
    }
}

/// STOP is not an error: "BREAK IN <line>" is the program's own output.
#[test]
fn stop_still_names_its_line() {
    let src = "10 FOR I=1 TO 2:PRINT I:NEXT\n20 A=1\n30 IF A THEN STOP\n40 PRINT \"NO\"\n";
    let want = interpreted(src);
    assert!(want.0.ends_with("BREAK IN 30"), "{:?}", want.0);
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        assert_eq!(run(&build(src, profile, true)), want, "{profile:?}");
    }
}

/// The ROM stops with ?ILLEGAL QUANTITY; unchecked integer arithmetic wraps.
#[test]
fn integer_overflow_wraps() {
    let src = "10 READ A%,B%:A%=A%+B%:PRINT A%\n20 DATA 32767,1\n";
    assert_eq!(interpreted(src).1, Some(14));
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let got = run(&build(src, profile, true));
        assert_eq!(got, ("-32768 \n".to_string(), None, None), "{profile:?}");
    }
}

/// A program that traps its errors keeps every check: the error is part of what it does.
#[test]
fn error_trapping_keeps_the_checks() {
    let src = "10 ON ERROR GOTO 300\n\
               20 DIM A(5):I=9\n\
               30 A(I)=1\n\
               40 END\n\
               300 PRINT \"TRAPPED\":END\n";
    let prg = tokenize_program_with(src, Dialect::Tsb).expect("tokenizes");
    let asm = |unchecked| {
        compile_with_options(
            &prg,
            CompileOptions {
                dialect: Dialect::Tsb,
                unchecked,
                ..CompileOptions::default()
            },
        )
        .unwrap_or_else(|e| panic!("compiles: {e}"))
        .asm
    };
    assert_eq!(asm(true), asm(false));
}
