mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn program(r: u16) -> String {
    let vars: Vec<String> = (0..60).map(|i| format!("{}{}", (b'A' + i / 10) as char, i % 10)).collect();
    let set = |range: std::ops::Range<usize>| {
        range.map(|i| format!("{}={i}", vars[i])).collect::<Vec<_>>().join(":")
    };
    let strings: Vec<String> = (0..20).map(|i| format!("{}$=\"S{i}\"", vars[i])).collect();
    let sum = |range: std::ops::Range<usize>, suffix: &str| {
        range.map(|i| format!("{}{suffix}", vars[i])).collect::<Vec<_>>().join("+")
    };
    format!(
        "5 READ R\n10 {}\n11 {}\n12 {}\n\
         20 FOR I=0 TO 15:POKE R+I,I+100:NEXT\n30 CLR:RESTORE:READ R\n\
         40 FOR I=0 TO 15:PRINT PEEK(R+I);:NEXT\n50 PRINT {}\n51 PRINT {}\n52 PRINT {}\n\
         60 DATA {r}\n",
        set(0..30),
        set(30..60),
        strings.join(":"),
        sum(0..30, ""),
        sum(30..60, ""),
        sum(0..20, "$"),
    )
}

fn compile(src: &str, reserved: Vec<(u16, u16)>) -> Vec<u8> {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(
        &prg,
        CompileOptions {
            profile: Profile::Default,
            auto_reserve: false,
            reserved_ranges: reserved,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compiles: {e}"))
    .prg_bytes
}

#[test]
fn clr_leaves_a_reserved_range_inside_the_variable_block_alone() {
    let image = compile(&program(0xC000), Vec::new());
    let end = u16::from_le_bytes([image[0], image[1]]) + (image.len() - 2) as u16;
    let from = (end - 0x240).max(0x0E00) & !0x0F;
    for r in (from..end + 0x20).step_by(32) {
        let src = program(r);
        let prg = tokenize_program(&src).expect("tokenizes");
        let rom = emu64::run_basic_to_end(&prg, 20_000_000).expect("runs in the interpreter");
        let compiled = compile(&src, vec![(r, r + 15)]);
        let got = emu64::run_prg_to_end(&compiled, 20_000_000).expect("runs");
        assert_eq!(
            (common::petscii_to_string(&got.output), got.error_code),
            (common::petscii_to_string(&rom.output), rom.error_code),
            "reserved ${r:04X}-${:04X}",
            r + 15
        );
    }
}
