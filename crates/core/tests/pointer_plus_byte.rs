//! Check POKE and PEEK through a zero-page pointer plus a byte offset.

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
    // A screen filled row by row, read back through the same addresses.
    "10 FOR Y=0 TO 24:FOR X=0 TO 39:POKE 1024+Y*40+X,(X+Y) AND 63:NEXT:NEXT\n\
     20 S=0:FOR Y=0 TO 24 STEP 3:FOR X=0 TO 39:S=S+PEEK(1024+Y*40+X):NEXT:NEXT:PRINT S\n",
    // A base variable and a byte counter; values that read the previous cell.
    "10 B=49152:FOR I=0 TO 255:POKE B+I,I:NEXT\n\
     20 FOR I=1 TO 255:POKE B+I,(PEEK(B+I-1)+PEEK(B+I)) AND 255:NEXT\n\
     30 FOR I=0 TO 255 STEP 17:PRINT PEEK(B+I);:NEXT\n",
    // The index changes inside the loop; the base moves between rows.
    "10 FOR R=0 TO 3:B=828+R*16:FOR J=0 TO 15:K=15-J:POKE B+K,J*R:NEXT:NEXT\n\
     20 FOR I=828 TO 891:PRINT PEEK(I);:NEXT\n",
    // Screen to colour RAM over a two-line body that reads before it writes.
    "10 FOR I=0 TO 999:POKE 1024+I,I AND 63:NEXT\n\
     20 FOR R=0 TO 24:FOR C=0 TO 39:V=PEEK(1024+R*40+C):IF V=32 THEN V=46\n\
     30 POKE 55296+R*40+C,V AND 15\n\
     40 NEXT:NEXT:S=0:FOR I=0 TO 999 STEP 7:S=S+(PEEK(55296+I) AND 15):NEXT:PRINT S\n",
];

#[test]
fn pointer_plus_byte_matches_the_interpreter() {
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

/// The row fill stores through its hoisted row address with the column in Y.
#[test]
fn a_row_fill_stores_through_its_row_pointer() {
    let (_, asm) = compiled(PROGRAMS[0], Profile::Default, false);
    assert!(asm.contains("    LDY VI_X\n    STA ("), "{asm}");
}

/// Hoist row addresses and remove line stamps before reads proven above zero page.
#[test]
fn a_reading_body_hoists_both_row_addresses() {
    let (_, asm) = compiled(PROGRAMS[3], Profile::Default, false);
    assert!(asm.matches("__LICM_").count() >= 2, "{asm}");
    assert!(asm.contains("),Y ; reads above zero page\n"), "{asm}");
    let read = asm.find("),Y ; reads above zero page").expect("the row read");
    let top = asm[..read].rfind("__FOR_TOP_").expect("the inner loop's top");
    assert!(!asm[top..read].contains("STA $39"), "{asm}");
}

/// Keep the column counter in Y and write the line stamp once at row exit.
#[test]
fn a_row_counts_in_y() {
    let (_, asm) = compiled(PROGRAMS[3], Profile::Default, false);
    let read = asm.find("),Y ; reads above zero page").expect("the row read");
    let close = read + asm[read..].find("    INY\n").expect("the counter steps in Y");
    let body = &asm[read..close];
    assert!(!body.contains("LDY VI_C") && !body.contains("STA $39"), "{asm}");
}
