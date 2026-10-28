//! Embedded float routines with matching FAC and rounding-byte results.
//! Addition aligns bytes; multiplication builds quarter-square tables on demand.

use std::fmt::Write as _;

/// Start of the table area: the last thing in the image.
pub const TABLE_LABEL: &str = "__FF_TBL";
/// End of the table area.
pub const TABLE_END_LABEL: &str = "__FF_TBL_END";
/// Six table pages and the slack to align them.
const TABLE_BYTES: usize = 6 * 256 + 255;
/// FMULT calls that go to the ROM before the tables are built.
const WARMUP_CALLS: u8 = 48;

/// Comment on zero-page indexed instructions whose index stays inside zero page.
pub const BOUNDED_INDEX: &str = "zp index";

struct Lines(String);

impl Lines {
    fn push(&mut self, text: &str) {
        for line in text.trim_matches('\n').lines() {
            self.0.push_str(line);
            if zero_page_indexed(line) {
                self.0.push_str(" ; ");
                self.0.push_str(BOUNDED_INDEX);
            }
            self.0.push('\n');
        }
    }
}

/// `MNE $nn,X`: zero page with an index.
fn zero_page_indexed(line: &str) -> bool {
    let Some((_, operand)) = line.trim().split_once(' ') else {
        return false;
    };
    let Some(base) = operand.strip_suffix(",X") else {
        return false;
    };
    base.len() == 3 && base.starts_with('$')
}

/// Unrolled routines, or loops about a third of the size and a little slower.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Form {
    Fast,
    Compact,
}

/// CONUPK ($BA8C) in line: the memory operand at A/Y into ARG, ending with `LDA $61`.
const CONUPK: &str = "
    STA $22
    STY $23
    LDY #$04
    LDA ($22),Y
    STA $6D
    DEY
    LDA ($22),Y
    STA $6C
    DEY
    LDA ($22),Y
    STA $6B
    DEY
    LDA ($22),Y
    STA $6E
    EOR $66
    STA $6F
    LDA $6E
    ORA #$80
    STA $6A
    DEY
    LDA ($22),Y
    STA $69
    LDA $61
";

/// Shift a 40-bit operand right by A; return its rounding byte with carry clear.
fn shift_right(out: &mut Lines, tag: &str, o: [u8; 4]) {
    let p = format!("__FA_{tag}");
    let [o0, o1, o2, o3] = o;
    out.push(&format!(
        "
    CMP #$08
    BCC {p}B
    CMP #$28
    BCS {p}Z
    TAX
    AND #$07
    STA $5F
    TXA
    LSR
    LSR
    LSR
    CMP #$02
    BCC {p}Q1
    BEQ {p}Q2
    CMP #$04
    BCC {p}Q3
    LDA ${o0:02X}
    STA $70
    LDA #$00
    STA ${o0:02X}
    STA ${o1:02X}
    STA ${o2:02X}
    STA ${o3:02X}
    BEQ {p}QD
{p}Q3:
    LDA ${o1:02X}
    STA $70
    LDA ${o0:02X}
    STA ${o3:02X}
    LDA #$00
    STA ${o0:02X}
    STA ${o1:02X}
    STA ${o2:02X}
    BEQ {p}QD
{p}Q2:
    LDA ${o2:02X}
    STA $70
    LDA ${o1:02X}
    STA ${o3:02X}
    LDA ${o0:02X}
    STA ${o2:02X}
    LDA #$00
    STA ${o0:02X}
    STA ${o1:02X}
    BEQ {p}QD
{p}Q1:
    LDA ${o3:02X}
    STA $70
    LDA ${o2:02X}
    STA ${o3:02X}
    LDA ${o1:02X}
    STA ${o2:02X}
    LDA ${o0:02X}
    STA ${o1:02X}
    LDA #$00
    STA ${o0:02X}
{p}QD:
    LDX $5F
    LDA $70
    JMP {p}X
{p}Z:
    LDA #$00
    STA ${o0:02X}
    STA ${o1:02X}
    STA ${o2:02X}
    STA ${o3:02X}
    CLC
    JMP {p}D
{p}B:
    TAX
    LDA $70
{p}X:
    CPX #$06
    BCS {p}L
    CPX #$00
    BEQ {p}C
{p}R:
    LSR ${o0:02X}
    ROR ${o1:02X}
    ROR ${o2:02X}
    ROR ${o3:02X}
    ROR
    DEX
    BNE {p}R
{p}C:
    CLC
    JMP {p}D
{p}L:
    LDY #$00
    STY $57
    CPX #$07
    BEQ {p}L1
    ASL
    ROL ${o3:02X}
    ROL ${o2:02X}
    ROL ${o1:02X}
    ROL ${o0:02X}
    ROL $57
{p}L1:
    ASL
    ROL ${o3:02X}
    ROL ${o2:02X}
    ROL ${o1:02X}
    ROL ${o0:02X}
    ROL $57
    LDX ${o3:02X}
    LDA ${o2:02X}
    STA ${o3:02X}
    LDA ${o1:02X}
    STA ${o2:02X}
    LDA ${o0:02X}
    STA ${o1:02X}
    LDA $57
    STA ${o0:02X}
    TXA
    CLC
{p}D:
"
    ));
}

/// Add aligned operands and normalize borrowed differences through ROM.
const FADD_ADD: &str = "
__FA_ADD:
    ADC $56
    STA $70
    LDA $65
    ADC $6D
    STA $65
    LDA $64
    ADC $6C
    STA $64
    LDA $63
    ADC $6B
    STA $63
    LDA $62
    ADC $6A
    STA $62
    BCS __FA_UP
__FA_RTS:
    RTS
__FA_UP:
    INC $61
    BEQ __FA_OVF
    ROR $62
    ROR $63
    ROR $64
    ROR $65
    ROR $70
    RTS
__FA_OVF:
    JMP $B97E
";

/// Handle zero and equal-exponent operands before aligning ARG with FAC.
const FADD_ENTRY: &str = "
__FADDT:
    BNE __FA_GO
    JMP $BBFC
__FA_GO:
    LDX $70
    STX $56
    LDA $69
    BEQ __FA_RTS
    SEC
    SBC $61
    BEQ __FA_EQ
    BCS __FA_FS
    EOR #$FF
    ADC #$01
    LDY #$00
    STY $70
";

/// FSUBT: negate FAC and add.
const FSUBT: &str = "
__FSUBT:
    LDA $66
    EOR #$FF
    STA $66
    EOR $6E
    STA $6F
    LDA $61
    JMP __FADDT
";

/// FADD ($B867), FADDT ($B86A), FSUB ($B850) and FSUBT ($B853).
pub fn fadd_helper(form: Form) -> String {
    let mut out = Lines(String::new());
    match form {
        Form::Fast => {
            out.push("__FSUB:");
            out.push(CONUPK);
            out.push(FSUBT);
            out.push("__FADD:");
            out.push(CONUPK);
            out.push(FADD_ENTRY);
            shift_right(&mut out, "A", [0x6A, 0x6B, 0x6C, 0x6D]);
            out.push(
                "
    BIT $6F
    BMI __FA_SA",
            );
            out.push(FADD_ADD);
            out.push(
                "
__FA_EQ:
    BIT $6F
    BPL __FA_ADD
__FA_SA:
    SEC
    EOR #$FF
    ADC $56
    STA $70
    LDA $65
    SBC $6D
    STA $65
    LDA $64
    SBC $6C
    STA $64
    LDA $63
    SBC $6B
    STA $63
    LDA $62
    SBC $6A
    STA $62
    JMP $B8D2
__FA_FS:
    LDY $69
    STY $61
    LDY $6E
    STY $66
    LDY #$00
    STY $56",
            );
            shift_right(&mut out, "F", [0x62, 0x63, 0x64, 0x65]);
            out.push(
                "
    BIT $6F
    BPL __FA_ADD
    SEC
    EOR #$FF
    ADC $56
    STA $70
    LDA $6D
    SBC $65
    STA $65
    LDA $6C
    SBC $64
    STA $64
    LDA $6B
    SBC $63
    STA $63
    LDA $6A
    SBC $62
    STA $62
    JMP $B8D2",
            );
        }
        Form::Compact => {
            out.push(
                "
__FSUB:
    JSR $BA8C",
            );
            out.push(FSUBT);
            out.push(
                "
__FADD:
    JSR $BA8C",
            );
            out.push(FADD_ENTRY);
            out.push(
                "
    LDX #$69
    JSR __FA_SHIFT
    LDY #$61
    BIT $6F
    BMI __FA_SUB",
            );
            out.push(FADD_ADD);
            out.push(
                "
__FA_EQ:
    LDX #$69
    LDY #$61
    BIT $6F
    BPL __FA_ADD
__FA_SUB:
    SEC
    EOR #$FF
    ADC $56
    STA $70
    LDA $0004,Y
    SBC $04,X
    STA $65
    LDA $0003,Y
    SBC $03,X
    STA $64
    LDA $0002,Y
    SBC $02,X
    STA $63
    LDA $0001,Y
    SBC $01,X
    STA $62
    JMP $B8D2
__FA_FS:
    LDY $69
    STY $61
    LDY $6E
    STY $66
    LDY #$00
    STY $56
    LDX #$61
    JSR __FA_SHIFT
    LDY #$69
    BIT $6F
    BPL __FA_ADD
    BMI __FA_SUB",
            );
            out.push(SHIFT_INDEXED);
        }
    }
    out.0
}

/// The compact form's shift: the operand whose exponent is at X, with $70, right by A.
const SHIFT_INDEXED: &str = "
__FA_SHIFT:
    CMP #$08
    BCC __FA_SB
    CMP #$28
    BCS __FA_SZ
    TAY
    AND #$07
    STA $5F
    TYA
    LSR
    LSR
    LSR
    TAY
__FA_SQ:
    LDA $04,X
    STA $70
    LDA $03,X
    STA $04,X
    LDA $02,X
    STA $03,X
    LDA $01,X
    STA $02,X
    LDA #$00
    STA $01,X
    DEY
    BNE __FA_SQ
    LDY $5F
    LDA $70
    JMP __FA_SX
__FA_SZ:
    LDA #$00
    STA $01,X
    STA $02,X
    STA $03,X
    STA $04,X
    CLC
    RTS
__FA_SB:
    TAY
    LDA $70
__FA_SX:
    CPY #$06
    BCS __FA_SL
    CPY #$00
    BEQ __FA_SC
__FA_SR:
    LSR $01,X
    ROR $02,X
    ROR $03,X
    ROR $04,X
    ROR
    DEY
    BNE __FA_SR
__FA_SC:
    CLC
    RTS
__FA_SL:
    STY $5F
    LDY #$00
    STY $57
    LDY $5F
    CPY #$07
    BEQ __FA_SL1
    ASL
    ROL $04,X
    ROL $03,X
    ROL $02,X
    ROL $01,X
    ROL $57
__FA_SL1:
    ASL
    ROL $04,X
    ROL $03,X
    ROL $02,X
    ROL $01,X
    ROL $57
    LDY $04,X
    LDA $03,X
    STA $04,X
    LDA $02,X
    STA $03,X
    LDA $01,X
    STA $02,X
    LDA $57
    STA $01,X
    TYA
    CLC
    RTS
";

/// Emit restoring division; use ROM exponent and rounding operations.
pub fn fdiv_helper(form: Form) -> String {
    let mut out = Lines(String::new());
    out.push("__FDIV:");
    out.push(if form == Form::Fast {
        CONUPK
    } else {
        "    JSR $BA8C"
    });
    out.push(
        "
__FDIVT:
    BNE __FD_GO
    JMP $BB8A
__FD_GO:
    JSR $BC1B
    LDA #$00
    SEC
    SBC $61
    STA $61
    JSR $BAB7
    INC $61
    BNE __FD_X
    JMP $BADF
__FD_X:
    LDX #$FC
    LDA #$01
__FD_CMP:
    LDY $6A
    CPY $62
    BNE __FD_DEC
    LDY $6B
    CPY $63
    BNE __FD_DEC
    LDY $6C
    CPY $64
    BNE __FD_DEC
    LDY $6D
    CPY $65
__FD_DEC:
    BCC __FD_ZERO
__FD_ONE:
    ROL
    BCS __FD_ONEB
__FD_SUB:
    TAY
    SEC
    LDA $6D
    SBC $65
    STA $6D
    LDA $6C
    SBC $64
    STA $6C
    LDA $6B
    SBC $63
    STA $6B
    LDA $6A
    SBC $62
    STA $6A
    TYA
__FD_SHIFT:
    ASL $6D
    ROL $6C
    ROL $6B
    ROL $6A
    BCS __FD_ONE
    BMI __FD_CMP
__FD_ZERO:
    ROL
    BCC __FD_SHIFT
    INX
    STA $29,X
    BEQ __FD_Z40
    BPL __FD_DONE
    LDA #$01
    BNE __FD_SHIFT
__FD_Z40:
    LDA #$40
    BNE __FD_SHIFT
__FD_ONEB:
    INX
    STA $29,X
    BEQ __FD_O40
    BPL __FD_DONE
    LDA #$01
    BNE __FD_SUB
__FD_O40:
    LDA #$40
    BNE __FD_SUB
__FD_DONE:
    ASL
    ASL
    ASL
    ASL
    ASL
    ASL
    STA $70
    JMP $BB8F",
    );
    out.0
}

/// Product-column accumulator byte for unrolled or compact multiplication.
fn column(form: Form, c: usize) -> u8 {
    match form {
        Form::Fast => 0x55 + c as u8,
        Form::Compact => 0x5F - c as u8,
    }
}

/// Emit table-based multiplication with ROM fallbacks for ambiguous low-product carries.
pub fn fmult_helper(form: Form) -> String {
    // Multiplicand bytes low to high.
    const A: [u8; 4] = [0x6D, 0x6C, 0x6B, 0x6A];
    let col = |c| column(form, c);
    let mut out = Lines(String::new());
    // The warm-up jumps become `BIT`, which changes Z, so the T entry tests FAC again.
    out.push(
        "
__FMULTT:
__FM_W2:
    JMP __FM_WARM2
    LDA $61
    BNE __FM_GO
    RTS
__FMULT:
__FM_W1:
    JMP __FM_WARM1",
    );
    out.push(if form == Form::Fast {
        CONUPK
    } else {
        "    JSR $BA8C"
    });
    out.push(
        "
    BNE __FM_GO
    RTS
__FM_GO:
    LDA $64
    BNE __FM_OK
    LDA $63
    BEQ __FM_Q2
    LDA $65
    BNE __FM_OK
    LDA $70
    BEQ __FM_OK
    JMP $BA30
__FM_Q2:
    LDA $70
    ORA $65
    BEQ __FM_OK
__FM_ROM:
    JMP $BA30
__FM_OK:
    LDA $69
    BEQ __FM_ZERO
    CLC
    ADC $61
    BCC __FM_NC
    BMI __FM_OVF
    CLC
    BCC __FM_E
__FM_NC:
    BPL __FM_ZERO
__FM_E:
    ADC #$80
    BEQ __FM_ROM
    STA $5E
__FM_PL0:
    LDA #$00
    STA $23
__FM_PH0:
    LDA #$00
    STA $25
__FM_PL1:
    LDA #$00
    STA $27
__FM_PH1:
    LDA #$00
    STA $29
    LDA #$00
    CLC",
    );
    for c in 2..=8 {
        let _ = writeln!(out.0, "    STA ${:02X}", col(c));
    }
    // Keep carry clear between products so subtraction and addition restore the exact product.
    const ROW_SETUP: &str = "
    STA $22
    STA $24
    EOR #$FF
    CLC
    ADC #$01
    STA $26
    STA $28";
    match form {
        Form::Fast => {
            // Multiplier bytes low to high: the rounding byte first.
            const M: [u8; 5] = [0x70, 0x65, 0x64, 0x63, 0x62];
            let mut label = 0;
            let mut next = || {
                label += 1;
                format!("__FM_L{label}")
            };
            for (i, m) in M.iter().enumerate() {
                let skip_row = next();
                out.push(&format!(
                    "
    LDA ${m:02X}
    BEQ {skip_row}"
                ));
                out.push(ROW_SETUP);
                for (j, a) in A.iter().enumerate() {
                    let c = i + j;
                    if c < 2 {
                        continue;
                    }
                    let skip = next();
                    let (lo, hi) = (col(c), col(c + 1));
                    out.push(&format!(
                        "
    LDY ${a:02X}
    BEQ {skip}
    LDA ($22),Y
    SBC ($26),Y
    TAX
    LDA ($24),Y
    SBC ($28),Y
    TAY
    TXA
    ADC ${lo:02X}
    STA ${lo:02X}
    TYA
    ADC ${hi:02X}
    STA ${hi:02X}"
                    ));
                    if c + 2 <= 8 {
                        let _ = writeln!(out.0, "    BCC {skip}");
                        let _ = writeln!(out.0, "    INC ${:02X}", col(c + 2));
                        if c + 3 <= 8 {
                            let carried = next();
                            let _ = writeln!(out.0, "    BNE {carried}");
                            let _ = writeln!(out.0, "    JSR __FM_RIP{}", c + 3);
                            let _ = writeln!(out.0, "{carried}:");
                        }
                    }
                    let _ = writeln!(out.0, "    CLC");
                    let _ = writeln!(out.0, "{skip}:");
                }
                let _ = writeln!(out.0, "{skip_row}:");
            }
        }
        Form::Compact => {
            // Map multiplier rows and ARG bytes to their product accumulator columns.
            out.push(
                "
    LDA $70
    STA $66
    LDX #$04
__FM_ROW:
    LDA $62,X
    BEQ __FM_NEXT",
            );
            out.push(ROW_SETUP);
            out.push(
                "
    CPX #$03
    BCS __FM_J1",
            );
            for (j, a) in A.iter().enumerate() {
                if j == 1 {
                    out.push(
                        "
__FM_J1:
    CPX #$04
    BCS __FM_J1S",
                    );
                }
                if j == 2 {
                    out.push("__FM_J2:");
                }
                let (lo, hi, carry) = (col(4 + j), col(5 + j), col(6 + j));
                out.push(&format!(
                    "
    LDY ${a:02X}
    BEQ __FM_S{j}
    LDA ($22),Y
    SBC ($26),Y
    STA $56
    LDA ($24),Y
    SBC ($28),Y
    TAY
    LDA $56
    ADC ${lo:02X},X
    STA ${lo:02X},X
    TYA
    ADC ${hi:02X},X
    STA ${hi:02X},X
    BCC __FM_S{j}
    INC ${carry:02X},X
    BNE __FM_C{j}
    TXA
    CLC
    ADC #${deeper:02X}
    JSR __FM_DEEP
__FM_C{j}:
    CLC
__FM_S{j}:",
                    deeper = carry - 1,
                ));
            }
            out.push(
                "
__FM_NEXT:
    DEX
    BPL __FM_ROW
    JMP __FM_SUM
__FM_J1S:
    CLC
    BCC __FM_J2
__FM_DEEP:
    STX $5F
    TAX
__FM_DEEPL:
    INC $00,X
    BNE __FM_DEEPX
    DEX
    CPX #$56
    BNE __FM_DEEPL
__FM_DEEPX:
    LDX $5F
    RTS
__FM_SUM:",
            );
        }
    }
    out.push(&format!(
        "
    LDA ${c3:02X}
    CMP #$FE
    BCS __FM_ROM
    LDA ${c4:02X}
    STA $70
    LDA ${c5:02X}
    STA $65
    LDA ${c6:02X}
    STA $64
    LDA ${c7:02X}
    STA $63
    LDA ${c8:02X}
    STA $62
    LDA $6F
    STA $66
    LDA $5E
    LDX $62
    BMI __FM_ST
    ASL $70
    ROL $65
    ROL $64
    ROL $63
    ROL $62
    SEC
    SBC #$01
    BEQ __FM_ZERO
    CLC
__FM_ST:
    STA $61
    RTS
__FM_ZERO:
    LDA #$00
    STA $61
    STA $66
    RTS
__FM_OVF:
    JMP $B97E",
        c3 = col(3),
        c4 = col(4),
        c5 = col(5),
        c6 = col(6),
        c7 = col(7),
        c8 = col(8),
    ));
    if form == Form::Fast {
        out.push(&format!(
            "
__FM_RIP5:
    INC ${c5:02X}
    BNE __FM_RIPX
__FM_RIP6:
    INC ${c6:02X}
    BNE __FM_RIPX
__FM_RIP7:
    INC ${c7:02X}
    BNE __FM_RIPX
__FM_RIP8:
    INC ${c8:02X}
__FM_RIPX:
    RTS",
            c5 = col(5),
            c6 = col(6),
            c7 = col(7),
            c8 = col(8),
        ));
    }
    out.push(&format!(
        "
__FM_WARM1:
    DEC __FM_CNT
    BEQ __FM_INIT1
    JMP $BA28
__FM_INIT1:
    PHA
    TYA
    PHA
    JSR __FM_BUILD
    PLA
    TAY
    PLA
    JMP __FMULT
__FM_WARM2:
    DEC __FM_CNT
    BEQ __FM_INIT2
    LDA $61
    JMP $BA2B
__FM_INIT2:
    JSR __FM_BUILD
    LDA $61
    JMP __FMULTT
__FM_CNT:
    .byte ${WARMUP_CALLS:02X}
{BUILD}",
        BUILD = table_builder()
    ));
    out.0
}

/// Build quarter-square tables, patch their addresses, and disable warm-up jumps.
fn table_builder() -> String {
    format!(
        "
__FM_BUILD:
    LDA #<{TABLE_LABEL}
    CMP #$01
    LDA #>{TABLE_LABEL}
    ADC #$01
    STA __FM_PL0+1
    STA __FM_B1+2
    TAX
    DEX
    STX __FM_PL1+1
    STX __FM_B6+2
    INX
    INX
    STX __FM_B3+2
    INX
    INX
    STX __FM_PH0+1
    STX __FM_B2+2
    INX
    STX __FM_B4+2
    DEX
    DEX
    STX __FM_PH1+1
    STX __FM_B8+2
    LDA #$00
    STA $22
    STA $23
    TAX
    TAY
__FM_BU1:
    LDA $22
__FM_B1:
    STA $FF00,X
__FM_B6:
    STA $FF00,Y
    LDA $23
__FM_B2:
    STA $FF00,X
__FM_B8:
    STA $FF00,Y
    TXA
    LSR
    ADC $22
    STA $22
    BCC __FM_BU2
    INC $23
__FM_BU2:
    DEY
    INX
    BNE __FM_BU1
__FM_BU3:
    LDA $22
__FM_B3:
    STA $FF00,X
    LDA $23
__FM_B4:
    STA $FF00,X
    TXA
    LSR
    ORA #$80
    ADC $22
    STA $22
    BCC __FM_BU4
    INC $23
__FM_BU4:
    INX
    CPX #$FF
    BNE __FM_BU3
    LDA #$2C
    STA __FM_W1
    STA __FM_W2
    RTS"
    )
}

/// Whether table storage is contiguous, unreserved, and below BASIC ROM.
pub(crate) fn table_area_is_clear(start: u16, end: u16, reserved: &[(u16, u16)]) -> bool {
    usize::from(end.wrapping_sub(start)) == TABLE_BYTES
        && end > start
        && end <= 0xA000
        && reserved.iter().all(|&(lo, hi)| hi < start || lo >= end)
}

/// The table area: the last bytes of the image, left out of the file and of the startup clear.
pub fn table_area() -> String {
    let mut s = String::new();
    let _ = writeln!(s, "{TABLE_LABEL}:");
    let row = vec!["$00"; 16].join(",");
    for _ in 0..TABLE_BYTES / 16 {
        let _ = writeln!(s, "    .byte {row}");
    }
    let rest = TABLE_BYTES % 16;
    if rest > 0 {
        let _ = writeln!(s, "    .byte {}", vec!["$00"; rest].join(","));
    }
    let _ = writeln!(s, "{TABLE_END_LABEL}:");
    s
}

/// Put the table area at the end of the image, ahead of the heap if there is one.
pub(crate) fn append_table_area(asm: &str) -> String {
    let heap = "__HEAP_BOTTOM:";
    let mut out = String::with_capacity(asm.len() + TABLE_BYTES * 5);
    let mut had_heap = false;
    for line in asm.lines() {
        if line.trim() == heap {
            had_heap = true;
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(&table_area());
    if had_heap {
        out.push_str(heap);
        out.push('\n');
    }
    out
}

/// Emit power functions through local float routines, preserving required scratch state.
const FX_SQR_PWR: &str = "
__FX_SQR:
    JSR $BC0C
    LDA #$11
    LDY #$BF
    JSR $BBA2
__FX_PWR:
    BEQ __FX_EXP
    LDA $69
    BNE __FX_PW1
    JMP $B8F9
__FX_PW1:
    LDX #$4E
    LDY #$00
    JSR $BBD4
    LDA $6E
    BPL __FX_PW2
    JSR $BCCC ; ROM INT: sets $07
    LDA #$4E
    LDY #$00
    JSR $BC5B
    BNE __FX_PW2
    TYA
    LDY $07
__FX_PW2:
    JSR $BBFE
    TYA
    PHA
    JSR __FX_LOG
    LDA #$4E
    LDY #$00
    JSR $BA28
    JSR __FX_EXP
    PLA
    LSR
    BCC __FX_PW3
    LDA $61
    BEQ __FX_PW3
    LDA $66
    EOR #$FF
    STA $66
__FX_PW3:
    RTS
";

/// LOG ($B9EA).
const FX_LOG: &str = "
__FX_LOG:
    JSR $BC2B
    BEQ __FX_LGE
    BPL __FX_LG1
__FX_LGE:
    JMP $B248
__FX_LG1:
    LDA $61
    SBC #$7F
    PHA
    LDA #$80
    STA $61
    LDA #$D6
    LDY #$B9
    JSR $B867
    LDA #$DB
    LDY #$B9
    JSR $BB0F
    LDA #$BC
    LDY #$B9
    JSR $B850
    LDA #$C1
    LDY #$B9
    JSR __FX_POLYX
    LDA #$E0
    LDY #$B9
    JSR $B867
    PLA
    PHA
    JSR $BC0C
    PLA
    JSR $BC3C
    LDA $6E
    EOR $66
    STA $6F
    LDX $61
    JSR $B86A
    LDA #$E5
    LDY #$B9
    JMP $BA28
";

/// EXP ($BFED).
const FX_EXP: &str = "
__FX_EXP:
    LDA #$BF
    LDY #$BF
    JSR $BA28
    LDA $70
    ADC #$50
    BCC __FX_EX1
    JSR $BC23
__FX_EX1:
    STA $56
    JSR $BC0F
    LDA $61
    CMP #$88
    BCC __FX_EX3
__FX_EX2:
    JSR $BAD4
__FX_EX3:
    JSR $BCCC ; ROM INT: sets $07
    LDA $07
    CLC
    ADC #$81
    BEQ __FX_EX2
    SEC
    SBC #$01
    PHA
    LDX #$05
__FX_EX4:
    LDA $69,X
    LDY $61,X
    STA $61,X
    STY $69,X
    DEX
    BPL __FX_EX4
    LDA $56
    STA $70
    JSR $B853
    JSR $BFB4
    LDA #$C4
    LDY #$BF
    JSR __FX_POLY
    LDA #$00
    STA $6F
    PLA
    JSR $BAB9
    RTS
";

/// The polynomial evaluators ($E043, $E059) the power and trig families share.
const POLYNOMIALS: &str = "
__FX_POLYX:
    STA $71
    STY $72
    LDA $70
    BPL __FX_RT1
    JSR $BC1B
__FX_RT1:
    LDA $65
    STA __FX_T1+4
    LDA $64
    STA __FX_T1+3
    LDA $63
    STA __FX_T1+2
    LDA $66
    ORA #$7F
    AND $62
    STA __FX_T1+1
    LDY #$00
    STY $70
    LDA $61
    STA __FX_T1
    LDA #<__FX_T1
    LDY #>__FX_T1
    JSR $BA28
    JSR __FX_PL1
    LDA #<__FX_T1
    LDY #>__FX_T1
    JMP $BA28
__FX_POLY:
    STA $71
    STY $72
__FX_PL1:
    LDA $70
    BPL __FX_RT2
    JSR $BC1B
__FX_RT2:
    LDA $65
    STA __FX_T2+4
    LDA $64
    STA __FX_T2+3
    LDA $63
    STA __FX_T2+2
    LDA $66
    ORA #$7F
    AND $62
    STA __FX_T2+1
    LDY #$00
    STY $70
    LDA $61
    STA __FX_T2
    LDA ($71),Y
    STA $67
    LDY $71
    INY
    TYA
    BNE __FX_PL2
    INC $72
__FX_PL2:
    STA $71
    LDY $72
__FX_PL3:
    JSR $BA28
    LDA $71
    LDY $72
    CLC
    ADC #$05
    BCC __FX_PL4
    INY
__FX_PL4:
    STA $71
    STY $72
    JSR $B867
    LDA #<__FX_T2
    LDY #>__FX_T2
    DEC $67
    BNE __FX_PL3
    RTS
__FX_T1:
    .byte $00,$00,$00,$00,$00
__FX_T2:
    .byte $00,$00,$00,$00,$00
";

/// Emit trigonometric functions using shared polynomial storage.
const FX_SIN_COS: &str = "
__FX_COS:
    LDA #$E0
    LDY #$E2
    JSR $B867
__FX_SIN:
    JSR $BC0C
    LDA #$E5
    LDY #$E2
    LDX $6E
    STX $6F
    JSR $BBA2
    JSR $BB12
    JSR $BC0C
    JSR $BCCC ; ROM INT: sets $07
    LDA #$00
    STA $6F
    JSR $B853
    LDA #$EA
    LDY #$E2
    JSR $B850
    LDA $66
    PHA
    BPL __FX_SN1
    LDA #$11
    LDY #$BF
    JSR $B867
    LDA $66
    BMI __FX_SN2
    LDA $12
    EOR #$FF
    STA $12
__FX_SN1:
    JSR $BFB4
__FX_SN2:
    LDA #$EA
    LDY #$E2
    JSR $B867
    PLA
    BPL __FX_SN3
    JSR $BFB4
__FX_SN3:
    LDA #$EF
    LDY #$E2
    JMP __FX_POLYX
";

/// TAN ($E2B4).
const FX_TAN: &str = "
__FX_TAN:
    LDX #<__FX_T1
    LDY #>__FX_T1
    JSR $BBD4
    LDA #$00
    STA $12
    JSR __FX_SIN
    LDX #$4E
    LDY #$00
    JSR $BBD4
    LDA #<__FX_T1
    LDY #>__FX_T1
    JSR $BBA2
    LDA #$00
    STA $66
    LDA $12
    JSR __FX_TN1
    LDA #$4E
    LDY #$00
    JMP $BB0F
__FX_TN1:
    PHA
    JMP __FX_SN1
";

/// ATN ($E30E).
const FX_ATN: &str = "
__FX_ATN:
    LDA $66
    PHA
    BPL __FX_AT1
    JSR $BFB4
__FX_AT1:
    LDA $61
    PHA
    CMP #$81
    BCC __FX_AT2
    LDA #$BC
    LDY #$B9
    JSR $BB0F
__FX_AT2:
    LDA #$3E
    LDY #$E3
    JSR __FX_POLYX
    PLA
    CMP #$81
    BCC __FX_AT3
    LDA #$E0
    LDY #$E2
    JSR $B850
__FX_AT3:
    PLA
    BPL __FX_AT4
    JMP $BFB4
__FX_AT4:
    RTS
";

/// Return the power, trigonometric, and polynomial routines.
pub fn transcendentals() -> String {
    TRANSCENDENTALS.iter().map(|t| t.text).collect()
}

/// One transcendental routine: its entries, its text, and the pieces it calls.
struct Transcendental {
    entries: &'static [(&'static str, &'static str)],
    text: &'static str,
    calls: &'static [usize],
}

/// SQR and `^` call LOG and EXP; everything but `^` calls the polynomials, which come last.
const TRANSCENDENTALS: [Transcendental; 7] = [
    Transcendental {
        entries: &[("$BF71", "__FX_SQR"), ("$BF7B", "__FX_PWR")],
        text: FX_SQR_PWR,
        calls: &[1, 2],
    },
    Transcendental {
        entries: &[("$B9EA", "__FX_LOG")],
        text: FX_LOG,
        calls: &[6],
    },
    Transcendental {
        entries: &[("$BFED", "__FX_EXP")],
        text: FX_EXP,
        calls: &[6],
    },
    Transcendental {
        entries: &[("$E26B", "__FX_SIN"), ("$E264", "__FX_COS")],
        text: FX_SIN_COS,
        calls: &[6],
    },
    Transcendental {
        entries: &[("$E2B4", "__FX_TAN")],
        text: FX_TAN,
        calls: &[3],
    },
    Transcendental {
        entries: &[("$E30E", "__FX_ATN")],
        text: FX_ATN,
        calls: &[6],
    },
    Transcendental {
        entries: &[],
        text: POLYNOMIALS,
        calls: &[],
    },
];

/// Route the transcendentals the program calls, with only the pieces they need.
fn route_transcendentals(asm: &str) -> Option<String> {
    let called = |addr: &str| {
        asm.lines().any(|l| {
            let t = l.trim();
            t.strip_prefix("JSR ").or_else(|| t.strip_prefix("JMP ")) == Some(addr)
        })
    };
    let mut needed = [false; TRANSCENDENTALS.len()];
    let mut work: Vec<usize> = (0..TRANSCENDENTALS.len())
        .filter(|&i| TRANSCENDENTALS[i].entries.iter().any(|(addr, _)| called(addr)))
        .collect();
    while let Some(i) = work.pop() {
        if !needed[i] {
            needed[i] = true;
            work.extend_from_slice(TRANSCENDENTALS[i].calls);
        }
    }
    let renames: Vec<(&str, &str)> = TRANSCENDENTALS
        .iter()
        .flat_map(|t| t.entries.iter().copied())
        .collect();
    let text: String = TRANSCENDENTALS
        .iter()
        .zip(needed)
        .filter(|(_, n)| *n)
        .map(|(t, _)| t.text)
        .collect();
    crate::codegen::route_rom_calls(asm, &renames, &block(&text))
}

/// Marks around a routine that builds without extraram may drop.
pub(crate) const BLOCK_OPEN: &str = "; fast float block";
pub(crate) const BLOCK_CLOSE: &str = "; fast float block end";

fn block(text: &str) -> String {
    let mut out = Lines(format!("{BLOCK_OPEN}\n"));
    out.push(text);
    out.0.push_str(BLOCK_CLOSE);
    out.0.push('\n');
    out.0
}

/// Calls the extraram fallback sends back to the ROM.
const UNROUTED: [(&str, &str); 12] = [
    ("__FDIV", "$BB0F"),
    ("__FDIVT", "$BB12"),
    ("__FMULT", "$BA28"),
    ("__FMULTT", "$BA2B"),
    ("__FX_SQR", "$BF71"),
    ("__FX_PWR", "$BF7B"),
    ("__FX_LOG", "$B9EA"),
    ("__FX_EXP", "$BFED"),
    ("__FX_SIN", "$E26B"),
    ("__FX_COS", "$E264"),
    ("__FX_TAN", "$E2B4"),
    ("__FX_ATN", "$E30E"),
];

/// Send float calls to the routines above.
pub(crate) fn route(asm: &str, form: Form) -> String {
    let inlined = inline_moves(asm, form);
    let asm = &inlined;
    let transcendentals = route_transcendentals(asm);
    let asm = transcendentals.as_deref().unwrap_or(asm);
    let fadd = [
        ("$B867", "__FADD"),
        ("$B86A", "__FADDT"),
        ("$B850", "__FSUB"),
        ("$B853", "__FSUBT"),
    ];
    let asm = crate::codegen::route_rom_calls(asm, &fadd, &fadd_helper(form))
        .unwrap_or_else(|| asm.to_string());
    let fdiv = [("$BB0F", "__FDIV"), ("$BB12", "__FDIVT")];
    let asm =
        crate::codegen::route_rom_calls(&asm, &fdiv, &block(&fdiv_helper(form))).unwrap_or(asm);
    let fmult = [("$BA28", "__FMULT"), ("$BA2B", "__FMULTT")];
    match crate::codegen::route_rom_calls(&asm, &fmult, &block(&fmult_helper(form))) {
        Some(routed) => append_table_area(&routed),
        None => asm,
    }
}

/// Route multiplication and power functions to ROM and remove their table storage.
pub(crate) fn without_tables(asm: &str) -> Option<String> {
    let open = format!("{TABLE_LABEL}:");
    let close = format!("{TABLE_END_LABEL}:");
    if !asm
        .lines()
        .any(|l| l.trim() == open || l.trim() == BLOCK_OPEN)
    {
        return None;
    }
    let mut out = String::with_capacity(asm.len());
    let mut skipping = false;
    for line in asm.lines() {
        let t = line.trim();
        if t == open || t == BLOCK_OPEN {
            skipping = true;
            continue;
        }
        if t == close || t == BLOCK_CLOSE {
            skipping = false;
            continue;
        }
        if skipping {
            continue;
        }
        let rom = t.split_once(' ').and_then(|(mnem, op)| {
            let (_, addr) = UNROUTED.iter().find(|(label, _)| *label == op)?;
            matches!(mnem, "JSR" | "JMP").then(|| format!("    {mnem} {addr}"))
        });
        match rom {
            Some(r) => out.push_str(&r),
            None => out.push_str(line),
        }
        out.push('\n');
    }
    Some(out)
}

/// Inline float loads and stores at constant addresses, preserving rounding and registers.
pub(crate) fn inline_moves(asm: &str, form: Form) -> String {
    let lines: Vec<&str> = asm.lines().collect();
    let code = |i: usize| lines.get(i).map_or("", |l| l.split(';').next().unwrap_or("").trim());
    let mut out = String::with_capacity(asm.len() + asm.len() / 4);
    let mut rounds = 0;
    let mut i = 0;
    while i < lines.len() {
        let (a, b, c) = (code(i), code(i + 1), code(i + 2));
        let tail = matches!(c, "JMP $BBA2" | "JMP $BBD4");
        let wanted = tail || (form == Form::Fast && matches!(c, "JSR $BBA2" | "JSR $BBD4"));
        let label = b.strip_prefix("LDY #>").filter(|l| !l.is_empty());
        let load =
            label.filter(|l| wanted && c.ends_with("$BBA2") && a.strip_prefix("LDA #<") == Some(l));
        let store =
            label.filter(|l| wanted && c.ends_with("$BBD4") && a.strip_prefix("LDX #<") == Some(l));
        if let Some(l) = load {
            let _ = write!(
                out,
                "    LDA {l}+4\n    STA $65\n    LDA {l}+3\n    STA $64\n    LDA {l}+2\n    STA $63\n    \
                 LDA {l}+1\n    STA $66\n    ORA #$80\n    STA $62\n    LDY #$00\n    STY $70\n    \
                 LDA {l}\n    STA $61\n"
            );
        } else if let Some(l) = store {
            rounds += 1;
            let _ = write!(
                out,
                "    LDA $70\n    BPL __FF_MR{rounds}\n    JSR $BC1B\n__FF_MR{rounds}:\n    LDA $65\n    \
                 STA {l}+4\n    LDA $64\n    STA {l}+3\n    LDA $63\n    STA {l}+2\n    LDA $66\n    \
                 ORA #$7F\n    AND $62\n    STA {l}+1\n    LDY #$00\n    STY $70\n    LDA $61\n    \
                 STA {l}\n"
            );
        } else {
            out.push_str(lines[i]);
            out.push('\n');
            i += 1;
            continue;
        }
        if tail {
            out.push_str("    RTS\n");
        }
        i += 3;
    }
    out
}
