//! INT preserves FAC, rounding, and shifted scratch bytes across all exponents.

mod common;

use asm6502::Assembler6502;
use yabcompiler_core::codegen::FAST_INT;
use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

/// FAC, its rounding byte and the fill byte QINT shifts in.
const CELLS: [u8; 8] = [0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x68, 0x70];

/// Run `trials` FAC states through ROM INT and `__INT`; "OK" or "X" and the failing trial.
fn run(trials: u16) -> String {
    let n = CELLS.len();
    let snap: String = CELLS
        .iter()
        .enumerate()
        .map(|(i, c)| format!("    LDA ${c:02X}\n    STA IN+{i}\n"))
        .collect();
    let unsnap: String = CELLS
        .iter()
        .enumerate()
        .map(|(i, c)| format!("    LDA IN+{i}\n    STA ${c:02X}\n"))
        .collect();
    let save: String = CELLS
        .iter()
        .enumerate()
        .map(|(i, c)| format!("    LDA ${c:02X}\n    STA WANT+{i}\n"))
        .collect();
    let compare: String = CELLS
        .iter()
        .enumerate()
        .map(|(i, c)| format!("    LDA ${c:02X}\n    CMP WANT+{i}\n    BNE FAIL\n"))
        .collect();
    let src = format!(
        "*=$C000
    LDA #$A5
    STA S0
    LDA #$5A
    STA S1
    LDA #${lo:02X}
    STA N0
    LDA #${hi:02X}
    STA N1
LOOP:
    JSR SETUP
{snap}    JSR $BCCC
{save}{unsnap}    JSR __INT
{compare}    LDA N0
    BNE DEC0
    DEC N1
DEC0:
    DEC N0
    LDA N0
    ORA N1
    BNE LOOP
    LDA #$4F
    JSR $FFD2
    LDA #$4B
    JMP $FFD2
FAIL:
    LDA #$58
    JSR $FFD2
    LDA N1
    JSR HEX
    LDA N0
HEX:
    PHA
    LSR
    LSR
    LSR
    LSR
    JSR NIB
    PLA
    AND #$0F
NIB:
    CMP #$0A
    BCC DIG
    ADC #$06
DIG:
    ADC #$30
    JMP $FFD2
RB:
    LDY #$08
    LDA S0
RB1:
    ASL
    ROL S1
    BCC RB2
    EOR #$2D
RB2:
    DEY
    BNE RB1
    STA S0
    RTS
SETUP:
    LDA N0
    STA $61
    JSR RB
    ORA #$80
    STA $62
    JSR RB
    STA $63
    JSR RB
    STA $64
    JSR RB
    STA $65
    JSR RB
    STA $70
    JSR RB
    STA $66
    AND #$03
    BEQ SIGN_SET
    AND #$01
    BEQ SIGN_POS
    LDA #$FF
SIGN_POS:
    STA $66
SIGN_SET:
    LDA #$00
    STA $68
    JSR RB
    AND #$07
    BNE BITS_SET
    JSR RB
    STA $68
BITS_SET:
    RTS
{FAST_INT}S0:
    .byte 0
S1:
    .byte 0
N0:
    .byte 0
N1:
    .byte 0
IN:
    .byte {zeros}
WANT:
    .byte {zeros}
",
        zeros = vec!["0"; n].join(","),
        lo = trials & 0xFF,
        hi = trials >> 8,
    );
    let code = Assembler6502::new()
        .assemble_bytes(&src)
        .expect("assembles");
    let mut prg = vec![0x00, 0xC0];
    prg.extend_from_slice(&code);
    let r = emu64::run_prg_to_end(&prg, 400_000_000).expect("runs");
    assert!(r.clean_exit, "the check ran to its end");
    String::from_utf8_lossy(&r.output).into_owned()
}

#[test]
fn every_exponent_matches_the_rom() {
    assert_eq!(run(16384), "OK");
}

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

fn compiled(src: &str, profile: Profile) -> (Outcome, String) {
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
        (
            common::petscii_to_string(&r.output),
            r.error_code,
            r.error_line,
        ),
        c.asm,
    )
}

#[test]
fn compiled_programs_match_the_interpreter() {
    for src in [
        "10 X=RND(-7):FOR I=1 TO 200:A=INT(RND(1)*6)+1:B=INT(-RND(1)*6):PRINT A;B;:NEXT\n",
        "10 FOR I=-40 TO 40:X=I/7:PRINT INT(X);INT(X*1E6);INT(X/1E6);INT(2^I);:NEXT\n",
        "10 READ X:IF X=-1 THEN END\n20 PRINT INT(X);INT(X*3.3):GOTO 10\n\
         30 DATA 0,.5,1,1.5,255.99,256,32767.5,65535.9,2147483647,2147483648,1E9,1E10,-1\n",
    ] {
        let want = interpreted(src);
        let (got, asm) = compiled(src, Profile::Speed);
        assert_eq!(got, want, "{src}");
        assert!(asm.contains("JSR __INT"), "{asm}");
        for profile in [Profile::Default, Profile::Size] {
            let (got, asm) = compiled(src, profile);
            assert_eq!(got, want, "{profile:?}:\n{src}");
            assert!(!asm.contains("__INT_MASK"), "{profile:?}:\n{asm}");
        }
    }
}
