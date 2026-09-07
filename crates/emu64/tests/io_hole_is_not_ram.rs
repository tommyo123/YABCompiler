use emu64::System;

const BANK_ALL_IN: u8 = 0x37;

const BANK_BASIC_OUT: u8 = 0x36;

const BANK_KERNAL_OUT: u8 = 0x35;

/// The store the KERNAL's load loop performs, as this emulator models it: an ordinary CPU write under whatever banking is current.
fn kernal_load_store(sys: &mut System, addr: u16, value: u8) {
    sys.mem.write_byte(addr, value);
}

#[test]
fn a_load_store_into_the_io_area_never_reaches_the_ram_underneath() {
    let mut sys = System::new();
    sys.mem.ram[0x0001] = BANK_ALL_IN;

    for addr in [0xD011u16, 0xD418, 0xD800, 0xDD00] {
        sys.mem.ram[addr as usize] = 0x00;
        kernal_load_store(&mut sys, addr, 0xA5);
        assert_eq!(
            sys.mem.ram[addr as usize], 0x00,
            "${addr:04X}: a LOAD store with $01=$37 must not reach RAM"
        );
        assert_eq!(
            sys.mem.io[(addr - 0xD000) as usize],
            0xA5,
            "${addr:04X}: it went to the register instead"
        );
    }
}

#[test]
fn the_loaded_bytes_are_unreadable_afterwards_whichever_bank_the_program_runs_in() {
    // The program then runs at $01=$36 (extraram) and reads back not its own data but whatever the register returns.
    let mut sys = System::new();
    sys.mem.ram[0x0001] = BANK_ALL_IN;
    sys.mem.ram[0xD200] = 0xEE; // what the program wanted to find there
    kernal_load_store(&mut sys, 0xD200, 0xA5);

    for bank in [BANK_ALL_IN, BANK_BASIC_OUT, BANK_KERNAL_OUT] {
        sys.mem.ram[0x0001] = bank;
        assert_ne!(
            sys.mem.read_byte(0xD200),
            0xEE,
            "$01=${bank:02X} still shows the register, not the RAM"
        );
    }
}

#[test]
fn a_load_store_into_the_basic_and_kernal_windows_does_reach_ram() {

    let mut sys = System::new();
    sys.mem.ram[0x0001] = BANK_ALL_IN;

    for addr in [0xA000u16, 0xBFFF, 0xE000, 0xFFF9] {
        kernal_load_store(&mut sys, addr, 0x5A);
        assert_eq!(
            sys.mem.ram[addr as usize], 0x5A,
            "${addr:04X}: a write under ROM lands in the RAM underneath"
        );
    }

    sys.mem.ram[0x0001] = BANK_KERNAL_OUT;
    assert_eq!(sys.mem.read_byte(0xA000), 0x5A);
    assert_eq!(sys.mem.read_byte(0xE000), 0x5A);
    assert_eq!(sys.mem.read_byte(0xFFF9), 0x5A);
}

#[test]
fn banking_the_kernal_out_keeps_the_io_block_mapped() {

    let mut sys = System::new();
    sys.mem.ram[0x0001] = BANK_KERNAL_OUT;
    sys.mem.io[0x020] = 0x0B;
    sys.mem.ram[0xD020] = 0xFF;
    assert_eq!(
        sys.mem.read_byte(0xD020),
        0x0B,
        "$D020 is still the VIC border register at $01=$35"
    );
}

#[test]
fn load_prg_in_this_emulator_bypasses_banking_and_fills_the_io_hole_with_ram() {

    let mut sys = System::new();
    sys.mem.ram[0x0001] = BANK_ALL_IN;
    // A .prg loading at $CFFE whose body runs four bytes into the register block.
    let prg = [0xFE, 0xCF, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66];
    sys.mem.load_prg(&prg).expect("loads");

    assert_eq!(
        &sys.mem.ram[0xD000..0xD004],
        &[0x33, 0x44, 0x55, 0x66],
        "load_prg put the body in RAM under the I/O block — on a real \
         C64 these four bytes would have gone to $D000-$D003 (sprite 0/1 \
         X/Y) and been lost"
    );
    assert_eq!(
        sys.mem.io[0x000], 0x00,
        "and no register was touched, which is the opposite of the hardware"
    );

    let mut real = System::new();
    real.mem.ram[0x0001] = BANK_ALL_IN;
    kernal_load_store(&mut real, 0xD000, 0x33);
    assert_eq!(real.mem.ram[0xD000], 0x00);
    assert_eq!(real.mem.io[0x000], 0x33);
}
