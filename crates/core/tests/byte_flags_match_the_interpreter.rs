//! Check byte bitwise operations against interpreter results.

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

fn compiled(src: &str, profile: Profile, fast_float: bool) -> (Outcome, String) {
    let prg = tokenize_program(src).expect("tokenizes");
    let c = compile_with_options(
        &prg,
        CompileOptions {
            profile,
            fast_float,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}\n{src}"));
    let r = emu64::run_prg_to_end(&c.prg_bytes, 200_000_000).expect("runs");
    (
        (
            common::petscii_to_string(&r.output),
            r.error_code,
            r.error_line,
        ),
        c.asm,
    )
}

const PROGRAMS: &[&str] = &[
    // -1/0 flags, a byte 0..195, their ANDs, ORs and NOTs, tested and stored.
    "10 FOR I=0 TO 15:A=-(I AND 1):B=((I AND 2)>0):D=I*13:E=I AND 7\n\
     20 IF A AND B THEN PRINT \"AB\";\n\
     30 IF A OR B THEN PRINT \"A|B\";\n\
     40 IF NOT A THEN PRINT \"NA\";\n\
     50 IF A THEN PRINT \"A\";\n\
     60 IF D AND A THEN PRINT \"DA\";\n\
     70 IF D OR A THEN PRINT \"D|A\";\n\
     80 IF NOT D THEN PRINT \"ND\";\n\
     90 IF NOT (D OR A) THEN PRINT \"N(D|A)\";\n\
     100 F=NOT A:G=A AND B:H=NOT(A OR B):PRINT F;G;H;\n\
     110 IF E=3 AND A THEN PRINT \"E3A\";\n\
     120 IF (I>5) OR NOT B THEN PRINT \"X\";\n\
     130 IF (D AND 128) OR (A AND B) THEN PRINT \"Y\";\n\
     140 IF NOT(A AND B) AND NOT(NOT A) THEN PRINT \"Z\";\n\
     150 PRINT:NEXT\n",
    // Set flags in subroutines and read them after RETURN.
    "10 FOR T=1 TO 12:GOSUB 100:F4=NOT F0:IF F1 AND F2 THEN PRINT \"BOTH\";:GOTO 50\n\
     20 IF F1 THEN PRINT \"ONE\";:GOTO 50\n\
     30 IF NOT F2 THEN PRINT \"NONE\";:GOTO 50\n\
     40 PRINT \"TWO\";\n\
     50 PRINT F0;F1;F2;F4:NEXT:END\n\
     100 F0=0:F1=T*7>40:F2=T/3=INT(T/3):IF T>10 THEN F0=-1\n\
     110 RETURN\n",
    // Masks of 16-bit integers and of PEEKs; a mask past the low byte.
    "10 POKE 828,37:FOR I=0 TO 900 STEP 37\n\
     20 IF I AND 1 THEN PRINT \"O\";\n\
     30 IF I AND 256 THEN PRINT \"H\";\n\
     40 IF I AND 768 THEN PRINT \"M\";\n\
     50 X=I AND 255:PRINT X;:IF NOT(I AND 4) THEN PRINT \"N\";\n\
     60 IF (I AND 6) OR (PEEK(828) AND 8) THEN PRINT \"Q\";\n\
     70 IF PEEK(828) AND I THEN PRINT \"P\";\n\
     80 IF I THEN PRINT \"T\";\n\
     90 PRINT:NEXT\n",
    // Joystick-style masks compared with a byte: equal, unequal and ordered.
    "10 FOR I=0 TO 300 STEP 7:J=I AND 255:P=PEEK(828)\n\
     20 IF (J AND 1)=0 THEN PRINT \"E\";\n\
     30 IF (J AND 16)<>0 THEN PRINT \"F\";\n\
     40 IF (J AND 12)=12 THEN PRINT \"T\";\n\
     50 IF (J AND 7)<3 THEN PRINT \"L\";\n\
     60 IF 4<=(J AND 6) THEN PRINT \"G\";\n\
     70 IF (J AND 255)>254 THEN PRINT \"X\";\n\
     80 IF (P OR 2)=2 THEN PRINT \"P\";\n\
     90 IF (I AND 3)=3 THEN PRINT \"I\";\n\
     100 PRINT:NEXT\n",
    // A counter crossing zero keeps its sign byte separate.
    "10 X=-1\n\
     20 X=X+1:IF X THEN PRINT X;\n\
     30 IF X AND 1 THEN PRINT \"O\";\n\
     40 IF X<200 THEN 20\n\
     50 PRINT:END\n",
];

#[test]
fn byte_flags_match_the_interpreter() {
    for src in PROGRAMS {
        let want = interpreted(src);
        assert_eq!(want.1, None, "the program itself runs clean:\n{src}");
        for (profile, fast_float) in [
            (Profile::Default, false),
            (Profile::Speed, false),
            (Profile::Size, false),
            (Profile::Default, true),
            (Profile::Speed, true),
        ] {
            let (got, _) = compiled(src, profile, fast_float);
            assert_eq!(got, want, "{profile:?} fast float {fast_float}:\n{src}");
        }
    }
}

/// Compute boolean flags in one byte without FAC conversion.
#[test]
fn flag_tests_stay_one_byte_wide() {
    let (_, asm) = compiled(PROGRAMS[1], Profile::Default, false);
    assert!(!asm.contains("__SXT_PRE_DONE"), "{asm}");
    assert!(!asm.contains("AYINT"), "{asm}");
}

/// `(J AND 1)=0` on a byte: the mask and the compare stay one byte wide.
#[test]
fn a_masked_byte_compares_one_byte_wide() {
    let (_, asm) = compiled(
        "10 FOR N=1 TO 50:J=PEEK(56320):IF (J AND 16)=0 THEN Y=Y+1\n20 NEXT:PRINT Y\n",
        Profile::Default,
        false,
    );
    assert!(asm.contains("    AND #$10\n    BNE "), "{asm}");
    assert!(!asm.contains("T0+1"), "{asm}");
}
