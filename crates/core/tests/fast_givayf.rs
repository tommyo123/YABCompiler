use asm6502::Assembler6502;
use yabcompiler_core::codegen::{FAST_BYTE_GIVAYF, FAST_GIVAYF};

const CELLS: [u8; 8] = [0x0D, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x70];

fn run(check: &str, helper: &str) -> String {
    let poison: String = CELLS.iter().map(|c| format!("    STA ${c:02X}\n")).collect();
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
        "*=$C000\n    LDA #$00\n    STA HI\n    STA LO\n{check}\
         OK:\n    LDA #$4F\n    JSR $FFD2\n    LDA #$4B\n    JMP $FFD2\n\
         FAIL:\n    LDA #$58\n    JSR $FFD2\n    LDA HI\n    JSR HEX\n    LDA LO\n\
         HEX:\n    PHA\n    LSR\n    LSR\n    LSR\n    LSR\n    JSR NIB\n    PLA\n    AND #$0F\n\
         NIB:\n    CMP #$0A\n    BCC DIG\n    ADC #$06\nDIG:\n    ADC #$30\n    JMP $FFD2\n\
         POISON:\n    LDA #$A5\n{poison}    RTS\n\
         SAVE:\n{save}    RTS\n\
         COMPARE:\n{compare}    RTS\n\
         {helper}\
         HI:\n    .byte 0\nLO:\n    .byte 0\nWANT:\n    .byte 0,0,0,0,0,0,0,0\n"
    );
    let code = Assembler6502::new().assemble_bytes(&src).expect("assembles");
    let mut prg = vec![0x00, 0xC0];
    prg.extend_from_slice(&code);
    let r = emu64::run_prg_to_end(&prg, 200_000_000).expect("runs");
    assert!(r.clean_exit, "the check ran to its end");
    String::from_utf8_lossy(&r.output).into_owned()
}

const GIVAYF_CHECK: &str = "LOOP:\n    JSR POISON\n    LDA HI\n    LDY LO\n    JSR $B391\n    JSR SAVE\n\
    \x20   JSR POISON\n    LDA HI\n    LDY LO\n    JSR __GIVAYF\n    JSR COMPARE\n\
    \x20   INC LO\n    BNE LOOP\n    INC HI\n    BNE LOOP\n";

const BYTEFAC_CHECK: &str = "LOOP:\n    JSR POISON\n    LDY LO\n    JSR $B3A2\n    JSR SAVE\n\
    \x20   JSR POISON\n    LDY LO\n    JSR __BYTEFAC\n    JSR COMPARE\n\
    \x20   INC LO\n    BNE LOOP\n";

#[test]
fn both_versions_match_the_rom_for_every_16_bit_input() {
    for helper in [FAST_GIVAYF, FAST_BYTE_GIVAYF] {
        assert_eq!(run(GIVAYF_CHECK, helper), "OK");
    }
}

#[test]
fn both_versions_match_the_rom_for_every_byte() {
    for helper in [FAST_GIVAYF, FAST_BYTE_GIVAYF] {
        assert_eq!(run(BYTEFAC_CHECK, helper), "OK");
    }
}
