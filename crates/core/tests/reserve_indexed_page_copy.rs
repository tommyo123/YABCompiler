//! A SYS page-copy destination belongs to the program even when no BASIC POKE names it.
use yabcompiler_core::{CompileOptions, compile_with_options, tokenize_program};

fn kernel(index_x: bool, destinations: &[u16]) -> Vec<u8> {
    let mut bytes = vec![if index_x { 0xa2 } else { 0xa0 }, 0];
    for (i, dest) in destinations.iter().enumerate() {
        let source = 0x2000 + i as u16 * 256;
        bytes.push(if index_x { 0xbd } else { 0xb9 });
        bytes.extend(source.to_le_bytes());
        bytes.push(if index_x { 0x9d } else { 0x99 });
        bytes.extend(dest.to_le_bytes());
    }
    bytes.extend([if index_x { 0xe8 } else { 0xc8 }, 0xd0]);
    bytes.push((2isize - (bytes.len() + 1) as isize) as i8 as u8);
    bytes.push(0x60);
    bytes
}

fn reservations(bytes: &[u8], auto: bool) -> Vec<(u16, u16)> {
    let data = bytes
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let src = format!(
        "10 FORI=0TO{}:READA:POKE832+I,A:NEXT\n20 SYS832:END\n30 DATA{data}\n",
        bytes.len() - 1
    );
    compile_with_options(
        &tokenize_program(&src).unwrap(),
        CompileOptions {
            auto_reserve: auto,
            ..Default::default()
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
fn independently_executed_copies_match_every_reserved_destination_byte() {
    for index_x in [false, true] {
        for destinations in [vec![0xc080], (0..8).map(|i| 0xc000 + i * 256).collect()] {
            let bytes = kernel(index_x, &destinations);
            let ranges = reservations(&bytes, true);
            let mut system = emu64::System::new();
            system.mem.ram[0x2000..0x2800].fill(0x37);
            system.mem.ram[0xbfff..=0xc800].fill(0x5a);
            system.mem.ram[832..832 + bytes.len()].copy_from_slice(&bytes);
            system.start_at(832);
            system.mem.ram[0x1fe] = 0xfd;
            system.mem.ram[0x1ff] = 0xff;
            system.cpu.regs.sp = 0xfd;
            for _ in 0..6000 {
                if system.cpu.regs.pc == 0xfffe || !system.step() {
                    break;
                }
            }
            assert_eq!(system.cpu.regs.pc, 0xfffe);
            for address in 0xbfff..=0xc800 {
                assert_eq!(
                    covers(&ranges, address),
                    system.mem.ram[address as usize] == 0x37,
                    "index_x={index_x}, address={address:04x}"
                );
            }
        }
    }
}

#[test]
fn index_control_branch_target_and_complete_window_are_required() {
    let good = kernel(false, &[0xc000]);
    for (at, byte) in [
        (1, 1),
        (2, 0xbd),
        (5, 0x9d),
        (8, 0xe8),
        (9, 0xf0),
        (10, 0xf8),
        (10, 0xfc),
    ] {
        let mut bad = good.clone();
        bad[at] = byte;
        assert!(!covers(&reservations(&bad, true), 0xc000), "mutation {at}");
    }
    assert!(!covers(&reservations(&good, false), 0xc000));
    assert!(covers(
        &reservations(&kernel(false, &[0xcf00]), true),
        0xcfff
    ));
    assert!(!covers(
        &reservations(&kernel(false, &[0xcf01]), true),
        0xcf01
    ));
    assert!(!covers(
        &reservations(&kernel(false, &[0xff80]), true),
        0x0900
    ));
    let mixed = reservations(&kernel(false, &[0x0400, 0xc000, 0xd800]), true);
    assert!(covers(&mixed, 0xc000));
    assert!(!covers(&mixed, 0xd800));
}

#[test]
fn retained_machine_code_is_scanned_even_after_an_opaque_rom_call() {
    let mut original = tokenize_program("10 SYS4096:END\n").unwrap();
    original.resize((0x1000 - 0x0801) + 2, 0xaa);
    original.extend([0x20, 0xd2, 0xff]);
    original.extend(kernel(false, &[0xc000, 0xc100]));
    original.extend([0xaa; 16]);
    let compiled = compile_with_options(&original, CompileOptions::default()).unwrap();
    assert!(covers(&compiled.diagnostics.auto_reserved, 0xc000));
    assert!(covers(&compiled.diagnostics.auto_reserved, 0xc1ff));
    assert!(!covers(&compiled.diagnostics.auto_reserved, 0xc200));
}
