//! RND preserves results, seed state, pointers, and registers.

mod common;

use asm6502::Assembler6502;
use yabcompiler_core::codegen::{FAST_RND, FAST_RND_ARG};
use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

/// What RND's caller can see afterwards.
const CELLS: [u8; 16] = [
    0x0D, 0x22, 0x23, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x68, 0x70, 0x8B, 0x8C, 0x8D, 0x8E, 0x8F,
];

/// Cells SETUP fills with noise before each trial.
const NOISE: [u8; 22] = [
    0x22, 0x23, 0x26, 0x27, 0x28, 0x29, 0x56, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x69, 0x6A, 0x6B,
    0x6C, 0x6D, 0x6E, 0x6F, 0x70, 0x0D,
];

/// Run `trials` seeds through `rom` and `fast`; "OK" or "X" and the failing trial number.
fn run(rom: &str, fast: &str, trials: u16) -> String {
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
    let noise: String = NOISE
        .iter()
        .map(|c| format!("    JSR RB\n    STA ${c:02X}\n"))
        .collect();
    let mant: String = [0x8D, 0x8E, 0x8F]
        .iter()
        .map(|c| {
            format!(
                "    JSR RB\n    STA ${c:02X}\n    JSR RB\n    AND #$03\n    BNE M{c:02X}\n\
                 \x20   STA ${c:02X}\nM{c:02X}:\n"
            )
        })
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
{snap}    JSR ROM
    LDA RA
    STA WANT+{n}
    LDA RX
    STA WANT+{n}+1
    LDA RY
    STA WANT+{n}+2
    LDA RP
    STA WANT+{n}+3
{save}{unsnap}    JSR FAST
{compare}    LDA RA
    CMP WANT+{n}
    BNE FAIL
    LDA RX
    CMP WANT+{n}+1
    BNE FAIL
    LDA RY
    CMP WANT+{n}+2
    BNE FAIL
    LDA RP
    CMP WANT+{n}+3
    BNE FAIL
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
{noise}    JSR RB
    AND #$01
    BEQ KEEP
    JSR RB
    AND #$07
    BNE EXP_NEAR
    JSR RB
    CMP #$E8
    BCC EXP_SET
    LDA #$E7
    BNE EXP_SET
EXP_NEAR:
    JSR RB
    AND #$1F
    CLC
    ADC #$6C
EXP_SET:
    STA $8B
    JSR RB
    STA $8C
    JSR RB
    AND #$07
    BEQ SIGN_KEPT
    LDA $8C
    AND #$7F
    STA $8C
SIGN_KEPT:
{mant}KEEP:
    LDA #$00
    STA $68
    JSR RB
    AND #$0F
    BNE BITS_SET
    JSR RB
    STA $68
BITS_SET:
    JSR RB
    AND #$07
    BNE ARG_SET
    STA $61
ARG_SET:
    RTS
ROM:
{rom}    PHP
    STA RA
    STX RX
    STY RY
    PLA
    STA RP
    RTS
FAST:
{fast}    PHP
    STA RA
    STX RX
    STY RY
    PLA
    STA RP
    RTS
{FAST_RND_ARG}{FAST_RND}S0:
    .byte 0
S1:
    .byte 0
N0:
    .byte 0
N1:
    .byte 0
RA:
    .byte 0
RX:
    .byte 0
RY:
    .byte 0
RP:
    .byte 0
IN:
    .byte {zeros}
WANT:
    .byte {zeros},0,0,0,0
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
fn a_positive_argument_matches_the_rom() {
    let rom = "    LDA #$81\n    STA $61\n    LDA #$00\n    STA $66\n    JSR $E097\n";
    assert_eq!(run(rom, "    JSR __RND_POS\n", 6000), "OK");
}

#[test]
fn any_argument_matches_the_rom() {
    assert_eq!(run("    JSR $E097\n", "    JSR __RND\n", 3000), "OK");
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
fn compiled_sequences_match_the_interpreter() {
    for src in [
        "10 X=RND(-7):FOR I=1 TO 300:PRINT RND(1);:NEXT\n",
        "10 X=RND(-3.5):FOR I=-5 TO 5:PRINT RND(I);RND(I/2);RND(.5);:NEXT\n",
        "10 FOR I=1 TO 40:POKE 139,96+I:POKE 140,I*3:POKE 141,0:POKE 142,I AND 1:POKE 143,I\n\
         20 PRINT RND(1);PEEK(139);PEEK(140):NEXT\n",
        "10 X=RND(-2):FOR I=1 TO 20:POKE 104,I:PRINT RND(1);:NEXT\n",
        "10 X=RND(-1):FOR I=1 TO 200:A=INT(RND(1)*6)+1:S=S+A:NEXT:PRINT S;RND(1)\n",
    ] {
        let want = interpreted(src);
        let (got, asm) = compiled(src, Profile::Speed);
        assert_eq!(got, want, "{src}");
        assert!(asm.contains("JSR __RND"), "{asm}");
        for profile in [Profile::Default, Profile::Size] {
            let (got, asm) = compiled(src, profile);
            assert_eq!(got, want, "{profile:?}:\n{src}");
            assert!(!asm.contains("__RND_POS"), "{profile:?}:\n{asm}");
        }
    }
}

#[test]
fn seeding_calls_keep_the_rom_entry() {
    let (_, asm) = compiled("10 X=RND(-1):Y=RND(0):PRINT X\n", Profile::Speed);
    assert!(!asm.contains("__RND"), "{asm}");
}
