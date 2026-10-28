//! Runtime buffers occupy contiguous memory outside reserved ranges.

use asm6502::Assembler6502;
use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const SOURCE: &str = "10 INPUT A$,B:FOR I=1 TO 3:A$=A$+CHR$(48+I):NEXT\n\
    20 PRINT VAL(A$);B:PRINT A$:PRINT \"DONE\":END\n";

const BUFFERS: [(&str, u32); 4] = [
    ("__VAL_BUF", 256),
    ("__INPUT_BUF", 257),
    ("__FIELD_BUF", 81),
    ("__FETCH_BUF", 89),
];

fn sweep(profile: Profile, extraram: bool, from: u32, to: u32) -> usize {
    let input = tokenize_program(SOURCE).unwrap();
    let mut layouts = 0;
    for start in (from..to).step_by(16) {
        let reserved = (start as u16, (start + 15) as u16);
        let options = CompileOptions {
            profile,
            extraram,
            reserved_ranges: vec![reserved],
            ..CompileOptions::default()
        };
        let Ok(built) = compile_with_options(&input, options) else {
            continue;
        };
        let mut a = Assembler6502::new();
        for &(lo, hi) in &built.diagnostics.effective_reserved {
            a.add_reserved_range(lo, hi).unwrap();
        }
        a.assemble_bytes(&built.asm).unwrap();
        layouts += 1;
        for (name, len) in BUFFERS {
            let Some(at) = a.lookup(name).map(u32::from) else {
                continue;
            };
            for &(lo, hi) in &built.diagnostics.effective_reserved {
                assert!(
                    u32::from(hi) < at || u32::from(lo) >= at + len,
                    "{profile:?} extraram {extraram}: {name} ${at:04X}+{len} covers reserved ${lo:04X}-${hi:04X}"
                );
            }
        }
    }
    layouts
}

#[test]
fn input_and_val_buffers_stay_clear_of_a_reserved_range_anywhere_in_the_image() {
    let input = tokenize_program(SOURCE).unwrap();
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let plain = compile_with_options(
            &input,
            CompileOptions {
                profile,
                ..CompileOptions::default()
            },
        )
        .unwrap();
        let end = 0x080d + plain.machine_code.len() as u32;
        assert!(
            sweep(profile, false, 0x0880, end + 0x100) > 50,
            "{profile:?}"
        );
        assert!(
            sweep(profile, true, 0x0820, 0x0a00) > 10,
            "{profile:?} extraram"
        );
    }
}
