//! Check FADD and FSUB results, including the rounding byte, against the ROM.

use asm6502::Assembler6502;
use yabcompiler_core::fast_float::{Form, fadd_helper};

/// FAC and its rounding byte.
const CELLS: [u8; 7] = [0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x70];

/// Compare FAC addition and subtraction with the ROM; report the failing trial.
fn run(form: Form, trials: u16) -> String {
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
        "*=$7000
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
{snap}    LDA #$00
    STA OP
    LDA #<MEM
    LDY #>MEM
    JSR $B867
{save}{unsnap}    LDA #<MEM
    LDY #>MEM
    JSR __FADD
{compare}{unsnap}    LDA #$01
    STA OP
    LDA #<MEM
    LDY #>MEM
    JSR $B850
{save}{unsnap}    LDA #<MEM
    LDY #>MEM
    JSR __FSUB
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
    JSR HEX
    LDA OP
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
; Exponents $40-$BF, the operand within 40 of FAC: every shift distance,
; cancellation, and no overflow.
SETUP:
    JSR RB
    AND #$7F
    ORA #$40
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
    STA $66
    JSR RB
    AND #$03
    BEQ R70
    LDA #$00
    BEQ R70S
R70:
    JSR RB
R70S:
    STA $70
    LDA #$00
    STA $68
    JSR RB
    AND #$3F
    CMP #$29
    BCC GAP
    AND #$07
GAP:
    STA GAPV
    JSR RB
    AND #$01
    BEQ GAPDN
    LDA $61
    CLC
    ADC GAPV
    JMP GAPST
GAPDN:
    LDA $61
    SEC
    SBC GAPV
GAPST:
    STA MEM
    JSR RB
    STA MEM+1
    JSR RB
    STA MEM+2
    JSR RB
    STA MEM+3
    JSR RB
    STA MEM+4
    ; Now and then the operand's top bytes copy FAC's, for deep cancellation.
    JSR RB
    AND #$03
    BNE SET_DONE
    LDA $62
    AND #$7F
    EOR MEM+1
    AND #$7F
    EOR MEM+1
    STA MEM+1
    LDA $63
    STA MEM+2
    JSR RB
    AND #$01
    BEQ SET_DONE
    LDA $64
    STA MEM+3
SET_DONE:
    RTS
{fadd}S0:
    .byte 0
S1:
    .byte 0
N0:
    .byte 0
N1:
    .byte 0
OP:
    .byte 0
GAPV:
    .byte 0
MEM:
    .byte 0,0,0,0,0
IN:
    .byte {zeros}
WANT:
    .byte {zeros}
",
        fadd = fadd_helper(form),
        zeros = vec!["0"; n].join(","),
        lo = trials & 0xFF,
        hi = trials >> 8,
    );
    let code = Assembler6502::new()
        .assemble_bytes(&src)
        .expect("assembles");
    assert!(0x7000 + code.len() < 0xA000, "the harness stays below BASIC ROM");
    let mut prg = vec![0x00, 0x70];
    prg.extend_from_slice(&code);
    let r = emu64::run_prg_to_end(&prg, 1_500_000_000).expect("runs");
    assert!(r.clean_exit, "the check ran to its end");
    String::from_utf8_lossy(&r.output).into_owned()
}

#[test]
fn unrolled_sums_and_differences_match_the_rom() {
    assert_eq!(run(Form::Fast, 20000), "OK");
}

#[test]
fn looped_sums_and_differences_match_the_rom() {
    assert_eq!(run(Form::Compact, 10000), "OK");
}
