use asm6502::Assembler6502;
use yabcompiler_core::codegen::FAST_FACWORD;

fn run(target: &str, state: [u8; 7]) -> (Vec<u8>, Option<u8>) {
    let cells = [0x61u8, 0x62, 0x63, 0x64, 0x65, 0x66, 0x70];
    let set: String = cells
        .iter()
        .zip(state)
        .map(|(c, v)| format!("    LDA #${v:02X}\n    STA ${c:02X}\n"))
        .collect();
    let show: String = ["$02", "$03", "$04", "$14", "$15", "$62", "$63", "$64", "$65", "$66"]
        .iter()
        .map(|c| format!("    LDA {c}\n    JSR $FFD2\n"))
        .collect();
    let src = format!(
        "*=$C000\n{set}    JSR {target}\n    PHP\n    STA $02\n    STY $03\n    PLA\n    AND #$83\n    STA $04\n\
         {show}    RTS\n{FAST_FACWORD}"
    );
    let code = Assembler6502::new().assemble_bytes(&src).expect("assembles");
    let mut prg = vec![0x00, 0xC0];
    prg.extend_from_slice(&code);
    let r = emu64::run_prg_to_end(&prg, 1_000_000).expect("runs");
    (r.output, r.error_code)
}

#[test]
fn matches_the_rom_for_every_exponent() {
    let mantissas: [[u8; 4]; 5] = [
        [0x80, 0x00, 0x00, 0x00],
        [0x80, 0x00, 0x00, 0x01],
        [0xC0, 0x01, 0x80, 0x00],
        [0xA5, 0x5A, 0xC3, 0x3C],
        [0xFF, 0xFF, 0xFF, 0xFF],
    ];
    let mut checked = 0;
    for exp in 0..=255u8 {
        for sign in [0x00u8, 0xFF] {
            for m in mantissas {
                for round in [0x00u8, 0xFF] {
                    let state = [exp, m[0], m[1], m[2], m[3], sign, round];
                    assert_eq!(run("__FACWORD", state), run("$B7F7", state), "FAC {state:02X?}");
                    checked += 1;
                }
            }
        }
    }
    assert_eq!(checked, 256 * 2 * 5 * 2);
}
