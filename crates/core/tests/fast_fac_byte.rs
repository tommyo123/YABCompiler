mod common;

use asm6502::Assembler6502;
use yabcompiler_core::codegen::FAST_FAC_BYTE;
use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn same_as_the_rom(src: &str) {
    let prg = tokenize_program(src).expect("tokenizes");
    let r = emu64::run_basic_to_end(&prg, 20_000_000).expect("runs in the interpreter");
    let want = (common::petscii_to_string(&r.output), r.error_code, r.error_line);
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let c = compile_with_options(
            &prg,
            CompileOptions {
                profile,
                ..CompileOptions::default()
            },
        )
        .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}\n{src}"));
        let r = emu64::run_prg_to_end(&c.prg_bytes, 20_000_000).expect("runs");
        assert_eq!(
            (common::petscii_to_string(&r.output), r.error_code, r.error_line),
            want,
            "{profile:?}:\n{src}"
        );
    }
}

const WITH_FACWORD: &str = "__FAC_BYTE:\n    JSR $B7F7\n    LDA $15\n    BNE __FAC_BYTE_BAD\n\
    \x20   LDA $14\n    RTS\n__FAC_BYTE_BAD:\n    LDX #$0E\n    JMP $A437\n";

fn run(helper: &str, state: [u8; 7]) -> (Vec<u8>, Option<u8>) {
    let cells = [0x61u8, 0x62, 0x63, 0x64, 0x65, 0x66, 0x70];
    let set: String = cells
        .iter()
        .zip(state)
        .map(|(c, v)| format!("    LDA #${v:02X}\n    STA ${c:02X}\n"))
        .collect();
    let src = format!("*=$C000\n{set}    JSR __FAC_BYTE\n    JSR $FFD2\n    RTS\n{helper}");
    let code = Assembler6502::new().assemble_bytes(&src).expect("assembles");
    let mut prg = vec![0x00, 0xC0];
    prg.extend_from_slice(&code);
    let r = emu64::run_prg_to_end(&prg, 1_000_000).expect("runs");
    (r.output, r.error_code)
}

#[test]
fn matches_the_facword_version_for_every_exponent() {
    let fast = format!("__FAC_BYTE:\n{FAST_FAC_BYTE}    LDX #$0E\n    JMP $A437\n");
    let mantissas: [[u8; 4]; 5] = [
        [0x80, 0x00, 0x00, 0x00],
        [0x80, 0x00, 0x00, 0x01],
        [0xC0, 0x01, 0x00, 0x00],
        [0xA5, 0x5A, 0xC3, 0x3C],
        [0xFF, 0xFF, 0xFF, 0xFF],
    ];
    let mut checked = 0;
    for exp in 0..=255u8 {
        for sign in [0x00u8, 0xFF] {
            for m in mantissas {
                for round in [0x00u8, 0xFF] {
                    let state = [exp, m[0], m[1], m[2], m[3], sign, round];
                    let want = run(WITH_FACWORD, state);
                    assert_eq!(run(&fast, state), want, "FAC {state:02X?}");
                    checked += 1;
                }
            }
        }
    }
    assert_eq!(checked, 256 * 2 * 5 * 2);
}

#[test]
fn poke_and_chr_take_the_truncated_byte() {
    same_as_the_rom(
        "10 FOR I=1 TO 19:READ V:POKE 830,V:PRINT PEEK(830);ASC(CHR$(V)+\"X\");:NEXT\n\
         20 DATA 0,.001,.5,.999,1,1.5,2,3.75,4,15.9,16,63,64,127.5,128,200.25,254.999,255,255.99\n",
    );
}

#[test]
fn a_poke_of_256_is_an_illegal_quantity() {
    same_as_the_rom("10 READ V:POKE 830,V:PRINT PEEK(830)\n20 READ V:POKE 830,V\n30 DATA 255,256\n");
}

#[test]
fn a_poke_of_a_negative_fraction_is_an_illegal_quantity() {
    same_as_the_rom("10 READ V:POKE 830,V:PRINT PEEK(830)\n20 READ V:POKE 830,V\n30 DATA 7,-.5\n");
}

#[test]
fn a_huge_value_is_an_illegal_quantity() {
    same_as_the_rom("10 V=1E6:PRINT \"A\";:POKE 830,V:PRINT \"B\"\n");
}
