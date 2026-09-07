use yabcompiler_core::{compile_with_options, tokenize_program, CompileOptions};

fn kernel(count: u8, destination: u16) -> Vec<u8> {
    let [lo, hi] = destination.to_le_bytes();
    vec![
        0xa9, 0, 0xaa, 0x9d, 0, 0xd4, 0x9d, lo, hi, 0xe8, 0xe0, count, 0xd0, 0xf5, 0x60,
    ]
}

fn reservations(bytes: &[u8], auto: bool) -> Vec<(u16, u16)> {
    let data = bytes
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let src = format!(
        "10 FORI=0TO{}:READA:POKE828+I,A:NEXT\n20 SYS828:END\n30 DATA{data}\n",
        bytes.len() - 1
    );
    compile_with_options(
        &tokenize_program(&src).unwrap(),
        CompileOptions {
            auto_reserve: auto,
            ..CompileOptions::default()
        },
    )
    .unwrap()
    .diagnostics
    .auto_reserved
}

fn covers(ranges: &[(u16, u16)], address: u16) -> bool {
    ranges
        .iter()
        .any(|&(lo, hi)| lo <= address && address <= hi)
}

#[test]
fn reserves_the_actual_clear_extent_and_ignores_sid() {
    for n in [1, 25, 255] {
        let bytes = kernel(n, 0xc100);
        let ranges = reservations(&bytes, true);
        // Execute the source machine code independently of the recognizer.
        let mut system = emu64::System::new();
        system.mem.ram[0xc0ff..=0xc200].fill(0x5a);
        system.mem.ram[828..828 + bytes.len()].copy_from_slice(&bytes);
        system.start_at(828);
        system.mem.ram[0x1fe] = 0xfd;
        system.mem.ram[0x1ff] = 0xff;
        system.cpu.regs.sp = 0xfd;
        for _ in 0..5000 {
            if system.cpu.regs.pc == 0xfffe || !system.step() {
                break;
            }
        }
        assert_eq!(system.cpu.regs.pc, 0xfffe);
        for address in 0xc0ff..=0xc200 {
            assert_eq!(
                covers(&ranges, address),
                system.mem.ram[address as usize] == 0,
                "N={n}, address={address:04x}"
            );
        }
        assert!(!covers(&ranges, 0xd400));
    }
}

#[test]
fn near_matches_and_out_of_window_spans_do_not_reserve() {
    let good = kernel(25, 0xc100);
    for (at, byte) in [
        (1, 1),
        (2, 0xa8),
        (9, 0xea),
        (10, 0xc0),
        (11, 0),
        (12, 0xf0),
        (13, 0xf4),
        (13, 0xf2),
    ] {
        let mut bad = good.clone();
        bad[at] = byte;
        assert!(
            !covers(&reservations(&bad, true), 0xc100),
            "mutation at {at}"
        );
    }
    assert!(!covers(&reservations(&good, false), 0xc100));
    assert!(covers(&reservations(&kernel(1, 0xcfff), true), 0xcfff));
    assert!(!covers(&reservations(&kernel(2, 0xcfff), true), 0xcfff));
    assert!(!covers(&reservations(&kernel(25, 0xfff0), true), 0x0800));
}

#[test]
fn the_retained_image_is_scanned_when_a_rom_call_prevents_whole_sys_analysis() {
    let mut original = tokenize_program("10 SYS4096:END\n").unwrap();
    original.resize((0x1000 - 0x0801) + 2, 0xaa);
    original.extend_from_slice(&[0x20, 0xd2, 0xff]);
    original.extend_from_slice(&kernel(25, 0xc100));
    original.extend_from_slice(&[0xaa; 16]);
    let compiled = compile_with_options(&original, CompileOptions::default()).unwrap();
    assert!(covers(&compiled.diagnostics.auto_reserved, 0xc100));
    assert!(covers(&compiled.diagnostics.auto_reserved, 0xc118));
    assert!(!covers(&compiled.diagnostics.auto_reserved, 0xc119));
}
