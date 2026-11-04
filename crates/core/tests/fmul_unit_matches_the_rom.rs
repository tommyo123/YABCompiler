//! Check float multiplication by integer flags -1, 0 and 1 against ROM FMULT.

mod common;

use asm6502::Assembler6502;
use yabcompiler_core::codegen::FMUL_UNIT;
use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

/// FAC and its rounding byte.
const CELLS: [u8; 7] = [0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x70];

/// Compare multiplication by -1, 0 and 1 with ROM FMULT; report the failing trial.
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
{snap}    LDX #$02
FLAG:
    STX K
{unsnap}    LDA KLO,X
    LDY #>K0
    JSR $BA28
{save}{unsnap}    LDX K
    LDA KA,X
    JSR __FMUL_UNIT
{compare}    LDX K
    DEX
    BPL FLAG
    LDA N0
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
    LDA K
    JSR HEX
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
    CMP #$FF
    BNE EXP_OK
    LDA #$FE
EXP_OK:
    STA $61
    JSR RB
    ORA #$80
    STA $62
    JSR RB
    STA $63
    JSR RB
    STA $64
    AND #$03
    BNE MID_SET
    STA $64
MID_SET:
    JSR RB
    STA $65
    JSR RB
    STA $70
    JSR RB
    STA $66
    AND #$01
    BEQ SIGN_SET
    LDA #$FF
    STA $66
SIGN_SET:
    LDA #$00
    STA $68
    RTS
{FMUL_UNIT}K0:
    .byte $00,$00,$00,$00,$00
K1:
    .byte $81,$00,$00,$00,$00
KM1:
    .byte $81,$80,$00,$00,$00
KLO:
    .byte <KM1,<K0,<K1
KA:
    .byte $FF,$00,$01
K:
    .byte 0
S0:
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
    assert!(code.len() < 0x1000, "the harness stays inside $C000-$CFFF");
    let mut prg = vec![0x00, 0xC0];
    prg.extend_from_slice(&code);
    let r = emu64::run_prg_to_end(&prg, 800_000_000).expect("runs");
    assert!(r.clean_exit, "the check ran to its end");
    String::from_utf8_lossy(&r.output).into_owned()
}

#[test]
fn every_flag_matches_rom_fmult() {
    assert_eq!(run(12000), "OK");
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

#[test]
fn flag_products_match_the_interpreter() {
    for src in [
        // 0/1 flags times sums, products and their unrounded results.
        "10 FOR I=-3 TO 3:FOR J=-2 TO 2\n\
         20 Z=-((I>J)AND(J<1)):M=(I=J):X=I/7+J*1E-3:Y=1.6E38-J*J*1E37\n\
         30 A=Z*X:B=M*X:C=Z*(X*X+.1):D=M*(X-1E-9):E=Z*(Y/9):F=M*0:G=Z*X+M*I\n\
         40 PRINT A;B;C;D;E;F;G\n\
         50 NEXT:NEXT\n",
        // Times 1 with exponent $FF overflows, as FMULT does.
        "10 FOR J=-2 TO 2:Z=-(J>0):Y=1.6E38-J*J*1E37:PRINT Z*Y:NEXT\n",
        // A FAC whose $63/$64 are zero takes FMULT's own path.
        "10 FOR I=0 TO 3:Z=-(I>1):X=3+I*2^-30:Y=(3+I/2^25)*7:PRINT Z*X;Z*Y;Z*(X+Y):NEXT\n",
    ] {
        let want = interpreted(src);
        for (profile, fast_float) in [
            (Profile::Default, false),
            (Profile::Speed, false),
            (Profile::Size, false),
            (Profile::Default, true),
            (Profile::Speed, true),
        ] {
            let (got, asm) = compiled(src, profile, fast_float);
            assert_eq!(got, want, "{profile:?} fast float {fast_float}:\n{src}");
            assert!(asm.contains("__FMUL_UNIT:"), "{profile:?}:\n{asm}");
        }
    }
}
