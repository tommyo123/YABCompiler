//! Check power-of-two multiplication and rounding against the interpreter.

mod common;

use asm6502::Assembler6502;
use yabcompiler_core::codegen::FMULT_P2;
use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

/// FAC and its rounding byte.
const CELLS: [u8; 7] = [0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x70];

/// Compare multiplication by 2^k for k in -64..63; report the failing trial.
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
{snap}    LDA #<K
    LDY #>K
    JSR $BA28
{save}{unsnap}    LDA #<K
    LDY #>K
    JSR __FMULT_P2
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
; 2^k for k in -64..63, and a FAC exponent that keeps the product from
; overflowing (FMULT would stop with an error) but reaches underflow.
SETUP:
    JSR RB
    AND #$7F
    CLC
    ADC #$41
    STA K
    JSR RB
    BNE EXP_NZ
    LDA #$01
EXP_NZ:
    STA $61
    CLC
    ADC K
    BCC EXP_OK
    CMP #$7F
    BCC EXP_OK
    LDA #$80
    STA $61
EXP_OK:
    JSR RB
    ORA #$80
    STA $62
    JSR RB
    STA $63
    AND #$03
    BNE HI_SET
    STA $63
HI_SET:
    JSR RB
    STA $64
    AND #$03
    BNE MID_SET
    STA $64
MID_SET:
    JSR RB
    STA $65
    AND #$03
    BNE LO_SET
    STA $65
LO_SET:
    JSR RB
    STA $70
    AND #$07
    BNE RND_SET
    STA $70
RND_SET:
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
{FMULT_P2}K:
    .byte $81,$00,$00,$00,$00
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
    let r = emu64::run_prg_to_end(&prg, 1_500_000_000).expect("runs");
    assert!(r.clean_exit, "the check ran to its end");
    String::from_utf8_lossy(&r.output).into_owned()
}

#[test]
fn power_of_two_products_match_rom_fmult() {
    assert_eq!(run(30000), "OK");
}

type Outcome = (String, Option<u8>, Option<u16>);

fn interpreted(src: &str) -> Outcome {
    let prg = tokenize_program(src).expect("tokenizes");
    let r = emu64::run_basic_to_end(&prg, 300_000_000).expect("runs in the interpreter");
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
    let r = emu64::run_prg_to_end(&c.prg_bytes, 300_000_000).expect("runs");
    (
        (
            common::petscii_to_string(&r.output),
            r.error_code,
            r.error_line,
        ),
        c.asm,
    )
}

/// Print the sign, mantissa halves and exponent without losing bits.
const EXACT: &str = "1000 IF V=0 THEN PRINT \"0 \";:RETURN\n\
                     1010 S=SGN(V):V=ABS(V):EX=0\n\
                     1020 IF V>=1 THEN V=V/2:EX=EX+1:GOTO 1020\n\
                     1030 IF V<.5 THEN V=V*2:EX=EX-1:GOTO 1030\n\
                     1040 H=INT(V*65536):L=(V*65536-H)*65536:PRINT S*H;L;EX;:RETURN\n";

#[test]
fn power_of_two_products_match_the_interpreter() {
    for body in [
        // Test both operand orders, unrounded sums, zero-byte mantissas and quotients.
        "10 FOR I=0 TO 7:X=(1+I/2147483648)*(1+I):Y=X/3+I*1E-9:Z=-X*7\n\
         20 A=2*X:B=X*2:C=.5*Y:D=Y*.25:E=8*(X+Y):F=(X+Y)*8:G=4*(Z-Y)\n\
         30 P=(X+Y)/2:Q=(Z*Y)/16:R=Y/4096:T=1024*(Y*Y)\n\
         40 V=A:GOSUB1000:V=B:GOSUB1000:V=C:GOSUB1000:V=D:GOSUB1000:V=E:GOSUB1000\
         :V=F:GOSUB1000:V=G:GOSUB1000:V=P:GOSUB1000:V=Q:GOSUB1000:V=R:GOSUB1000\
         :V=T:GOSUB1000:PRINT\n\
         50 NEXT:END\n",
        // Products and quotients near the bottom of the range.
        "10 X=3E-37:FOR I=1 TO 6:A=X/8:B=.125*X:C=X*2^-4:D=2^-5*(X+X)\
         :V=A:GOSUB1000:V=B:GOSUB1000:V=C:GOSUB1000:V=D:GOSUB1000:PRINT:X=X/3:NEXT\n\
         20 END\n",
    ] {
        let src = format!("{body}{EXACT}");
        let want = interpreted(&src);
        assert_eq!(want.1, None, "the program itself runs clean:\n{src}");
        for (profile, fast_float) in [
            (Profile::Default, false),
            (Profile::Speed, false),
            (Profile::Size, false),
            (Profile::Default, true),
            (Profile::Speed, true),
        ] {
            let (got, asm) = compiled(&src, profile, fast_float);
            assert_eq!(got, want, "{profile:?} fast float {fast_float}:\n{src}");
            for helper in ["__FMULT_P2:", "__FMUL2K:"] {
                assert_eq!(
                    asm.contains(helper),
                    profile != Profile::Size,
                    "{profile:?}: {helper} only outside the size profile"
                );
            }
        }
    }
}

/// Doubling past the top of the range stops with the interpreter's error.
#[test]
fn power_of_two_overflow_matches_the_interpreter() {
    for src in [
        "10 X=1.6E38:PRINT 1:Y=2*(X+1):PRINT Y\n",
        "10 X=1.6E38:PRINT 1:Y=X*4:PRINT Y\n",
        "10 X=1.6E38:PRINT 1:Y=(X+1)*2:PRINT Y\n",
    ] {
        let want = interpreted(src);
        assert!(want.1.is_some(), "the interpreter stops:\n{src}");
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
