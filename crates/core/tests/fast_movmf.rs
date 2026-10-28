//! MOVMF preserves stored bytes, FAC, registers, and flags.

mod common;

use asm6502::Assembler6502;
use yabcompiler_core::codegen::FAST_MOVMF;
use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

/// What the caller can see afterwards, besides MEM and the registers.
const CELLS: [u8; 9] = [0x22, 0x23, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x70];

/// Compare random FAC stores and report the first failing trial.
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
        .chain((0..5).map(|k| format!("    LDA MEM+{k}\n    STA WANT+{}\n", n + 4 + k)))
        .collect();
    let compare: String = CELLS
        .iter()
        .enumerate()
        .map(|(i, c)| format!("    LDA ${c:02X}\n    CMP WANT+{i}\n    BNE FAIL\n"))
        .chain((0..5).map(|k| {
            format!(
                "    LDA MEM+{k}\n    CMP WANT+{}\n    BNE FAIL\n",
                n + 4 + k
            )
        }))
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
{snap}    JSR NOISE
    JSR ROM
    LDA RA
    STA WANT+{n}
    LDA RX
    STA WANT+{n}+1
    LDA RY
    STA WANT+{n}+2
    LDA RP
    STA WANT+{n}+3
{save}{unsnap}    JSR NOISE
    JSR FAST
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
    JSR RB
    STA $22
    JSR RB
    STA $23
    JSR RB
    CMP #$FF
    BCC FEXP
    LDA #$FE
FEXP:
    STA $61
    JSR RB
    AND #$07
    BNE FAC_SET
    STA $61
FAC_SET:
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
    STA $70
    RTS
NOISE:
    JSR RB
    STA MEM
    JSR RB
    STA MEM+1
    JSR RB
    STA MEM+2
    JSR RB
    STA MEM+3
    JSR RB
    STA MEM+4
    JSR RB
    LSR
    PHP
    PLA
    STA CIN
    RTS
ROM:
    LDA CIN
    PHA
    LDX #<MEM
    LDY #>MEM
    PLP
    JSR $BBD4
    JMP KEEP
FAST:
    LDA CIN
    PHA
    LDX #<MEM
    LDY #>MEM
    PLP
    JSR __MOVMF
KEEP:
    PHP
    STA RA
    STX RX
    STY RY
    PLA
    AND #$82
    STA RP
    RTS
{FAST_MOVMF}S0:
    .byte 0
S1:
    .byte 0
N0:
    .byte 0
N1:
    .byte 0
CIN:
    .byte 0
RA:
    .byte 0
RX:
    .byte 0
RY:
    .byte 0
RP:
    .byte 0
MEM:
    .byte 0,0,0,0,0
IN:
    .byte {zeros}
WANT:
    .byte {zeros},0,0,0,0,0,0,0,0,0
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
fn movmf_matches_the_rom() {
    assert_eq!(run(30000), "OK");
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

fn compiled(src: &str, profile: Profile, extraram: bool) -> (Outcome, String) {
    let prg = tokenize_program(src).expect("tokenizes");
    let c = compile_with_options(
        &prg,
        CompileOptions {
            profile,
            extraram,
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
        "10 FOR I=1 TO 200:READ A:S=S+A/7:T=T*1.01+A:U=-S/T:PRINT S;T;U;:NEXT\n\
         20 DATA 5,200,13,0,77,1E6,-4,.001,32767,9\n30 RESTORE:IF S<1E5 THEN 10\n",
        "10 X=1:FOR I=1 TO 140:X=X*1.9:Y=X-1:Z=1/X:PRINT X;Y;Z;:NEXT\n",
        "10 READ A:FOR I=1 TO 99:B=A/I:C=B*I:D=C-A:PRINT B;D;:NEXT\n20 DATA 2.71828\n",
    ] {
        let want = interpreted(src);
        for extraram in [false, true] {
            let (got, asm) = compiled(src, Profile::Speed, extraram);
            assert_eq!(got, want, "extraram {extraram}:\n{src}");
            assert!(asm.contains("__MM_ROM"), "{asm}");
        }
        for profile in [Profile::Default, Profile::Size] {
            let (got, asm) = compiled(src, profile, false);
            assert_eq!(got, want, "{profile:?}:\n{src}");
            assert!(!asm.contains("__MM_ROM"), "{profile:?}:\n{asm}");
        }
    }
}
