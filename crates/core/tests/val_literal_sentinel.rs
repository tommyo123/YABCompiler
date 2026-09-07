mod common;
use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn run_again(m: &mut emu64::System, entry: u16) {
    m.output.clear();
    m.error_output.clear();
    m.error_code = None;
    m.error_line = None;
    m.ready_reached = false;
    m.start_at(entry);
    m.cpu.regs.sp = 0xfd;
    m.mem.ram[0x1fe] = 0xfd;
    m.mem.ram[0x1ff] = 0xff;
    for _ in 0..1_000_000 {
        if m.cpu.regs.pc == 0xfffe || !m.step() {
            return;
        }
    }
    panic!("program did not terminate");
}

#[test]
fn fatal_literal_val_does_not_erase_the_next_print_on_rerun() {
    let source = "10 IF PEEK(2)=1 THEN100\n20 POKE2,1:A$=\"1\"+CHR$(69)+\"39\":PRINT VAL(A$)\n30 END\n100 PRINT VAL(\"7\")\n110 PRINT\"DONE\":END\n";
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        for extra in [false, true] {
            let built = compile_with_options(
                &tokenize_program(source).unwrap(),
                CompileOptions {
                    profile,
                    extraram: extra,
                    force_extraram_off: !extra,
                    ..Default::default()
                },
            )
            .unwrap();
            let mut m = emu64::System::new();
            let entry = m.load_prg(&built.prg_bytes).unwrap();
            m.mem.ram[2] = 0;
            run_again(&mut m, entry);
            assert_eq!((m.error_code, m.error_line), (Some(15), Some(20)));
            run_again(&mut m, entry);
            assert_eq!(m.error_code, None);
            assert_eq!(
                common::petscii_to_string(&m.output)
                    .split_whitespace()
                    .collect::<Vec<_>>(),
                ["7", "DONE"],
                "{profile:?}/{extra}"
            );
        }
    }
}

#[test]
fn a_shared_literal_has_one_private_sentinel_without_changing_its_length() {
    let source =
        "10 A$=\"1\"+CHR$(69)+\"39\":PRINT A$;\n20 PRINT VAL(A$)\n30 PRINT VAL(A$)\n40 END\n";
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let built = compile_with_options(
            &tokenize_program(source).unwrap(),
            CompileOptions {
                profile,
                ..Default::default()
            },
        )
        .unwrap();
        let mut a = asm6502::Assembler6502::new();
        let image = a.assemble_bytes(&built.asm).unwrap();
        let literal: Vec<_> = a
            .symbols()
            .iter()
            .filter_map(|(name, addr)| {
                if !name.starts_with('S') || !name[1..].bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                let at = usize::from(*addr) - 0x080d;
                (image.get(at..at + 5) == Some(&[4, 49, 69, 51, 57])).then_some(at)
            })
            .collect();
        assert_eq!(
            literal.len(),
            1,
            "VAL/PRINT reads must share the interned payload"
        );
        assert_eq!(
            image[literal[0] + 5],
            0,
            "VAL's prospective terminator is private"
        );
    }
}

#[test]
fn ordinary_constant_val_results_still_parse_and_leave_prints_intact() {
    let source = "10 PRINT VAL(\"25\");VAL(\"\");VAL(\"1.5X\")\n20 PRINT\"DONE\"\n";
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let built = compile_with_options(
            &tokenize_program(source).unwrap(),
            CompileOptions {
                profile,
                ..Default::default()
            },
        )
        .unwrap();
        let r = emu64::run_prg_to_end(&built.prg_bytes, 1_000_000).unwrap();
        assert!(r.clean_exit);
        assert_eq!(
            common::petscii_to_string(&r.output)
                .split_whitespace()
                .collect::<Vec<_>>(),
            ["25", "0", "1.5", "DONE"]
        );
    }
}

#[test]
fn a_reservation_cannot_detach_the_literal_sentinel() {
    let source = "10 PRINT VAL(\"1e39\")\n20 PRINT\"DONE\":END\n";
    let input = tokenize_program(source).unwrap();
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        for extra in [false, true] {
            let opts = CompileOptions {
                profile,
                extraram: extra,
                force_extraram_off: !extra,
                ..Default::default()
            };
            let before = compile_with_options(&input, opts.clone()).unwrap();
            let label = before
                .asm
                .lines()
                .collect::<Vec<_>>()
                .windows(2)
                .find_map(|pair| {
                    (pair[1] == "; VAL literal sentinel")
                        .then(|| pair[0].trim_end_matches(':').to_owned())
                })
                .unwrap();
            let mut a = asm6502::Assembler6502::new();
            a.assemble_bytes(&before.asm).unwrap();
            let start = a.lookup(&label).unwrap();
            // A separate five-byte literal plus guard allows a JMP right where VAL writes NUL.
            let reserved = (start + 8, start + 40);
            let built = compile_with_options(
                &input,
                CompileOptions {
                    reserved_ranges: vec![reserved],
                    ..opts
                },
            )
            .unwrap();
            let mut a = asm6502::Assembler6502::new();
            a.add_reserved_range(reserved.0, reserved.1).unwrap();
            let image = a.assemble_bytes(&built.asm).unwrap();
            let start = usize::from(a.lookup(&label).unwrap()) - 0x080d;
            assert_eq!(
                &image[start..start + 6],
                &[4, 49, 69, 51, 57, 0],
                "{profile:?}/{extra}"
            );
        }
    }
}
