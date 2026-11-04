//! The embedded FMULT gives the ROM's FAC and rounding byte, squares included.

use asm6502::Assembler6502;
use yabcompiler_core::fast_float::{Form, fmult_helper, table_area};

/// FAC and its rounding byte.
const CELLS: [u8; 7] = [0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x70];

/// Compare FAC products with ROM FMULT; report the failing trial.
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
    JSR __FM_BUILD
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
{snap}    LDA #<MEM
    LDY #>MEM
    JSR $BA28
{save}{unsnap}    LDA #<MEM
    LDY #>MEM
    JSR __FMULT
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
; Exponents $60-$9F keep every product inside the float range.
SETUP:
    JSR RB
    AND #$3F
    ORA #$60
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
    ORA #$60
    STA MEM
    JSR RB
    STA MEM+1
    JSR RB
    STA MEM+2
    JSR RB
    STA MEM+3
    JSR RB
    STA MEM+4
    ; Every other product squares the FAC mantissa, sign and exponent free.
    LDA N0
    LSR
    BCC ZEROS
    LDA $62
    AND #$7F
    EOR MEM+1
    AND #$7F
    EOR MEM+1
    STA MEM+1
    LDA $63
    STA MEM+2
    LDA $64
    STA MEM+3
    LDA $65
    STA MEM+4
ZEROS:
    ; Now and then a zero byte on either side.
    JSR RB
    AND #$0F
    BNE SET_DONE
    JSR RB
    AND #$03
    TAX
    LDA #$00
    STA MEM+1,X
    CPX #$00
    BEQ SET_DONE
    STA $62,X
SET_DONE:
    RTS
{fmult}{tables}S0:
    .byte 0
S1:
    .byte 0
N0:
    .byte 0
N1:
    .byte 0
MEM:
    .byte 0,0,0,0,0
IN:
    .byte {zeros}
WANT:
    .byte {zeros}
",
        fmult = fmult_helper(form),
        tables = table_area(),
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
fn unrolled_products_match_the_rom() {
    assert_eq!(run(Form::Fast, 12000), "OK");
}

#[test]
fn looped_products_match_the_rom() {
    assert_eq!(run(Form::Compact, 6000), "OK");
}
