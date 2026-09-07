//! A ROM address is not a ROM routine.

use emu64::System;

const BANK_BASIC_OUT: u8 = 0x36;

const BANK_KERNAL_OUT: u8 = 0x35;

const BANK_ALL_IN: u8 = 0x37;

const BASIC_READY: u16 = 0xA474;
const BASIC_ERROR: u16 = 0xA437;
const CHROUT: u16 = 0xFFD2;

fn poke(sys: &mut System, addr: u16, bytes: &[u8]) {
    let at = addr as usize;
    sys.mem.ram[at..at + bytes.len()].copy_from_slice(bytes);
}

fn run_from(sys: &mut System, entry: u16, budget: u32) -> u32 {
    sys.start_at(entry);
    for i in 0..budget {
        if !sys.step() {
            return i + 1;
        }
    }
    budget
}

#[test]
fn code_executing_at_a474_under_extraram_banking_is_not_ready() {
    let mut sys = System::new();
    sys.mem.ram[0x0001] = BANK_BASIC_OUT;

    poke(&mut sys, BASIC_READY, &[0xA9, 0x42, 0x20, 0xD2, 0xFF, 0x00]);

    run_from(&mut sys, BASIC_READY, 64);

    assert!(
        !sys.ready_reached,
        "$A474 with the BASIC ROM banked out is the program's own code, not READY"
    );
    assert_eq!(
        sys.output, b"\x42",
        "the RAM instruction at $A474 must actually execute"
    );
}

#[test]
fn code_executing_at_a437_under_extraram_banking_is_not_an_error() {
    let mut sys = System::new();
    sys.mem.ram[0x0001] = BANK_BASIC_OUT;

    poke(&mut sys, BASIC_ERROR, &[0xA2, 0x0B, 0xA9, 0x51, 0x20, 0xD2, 0xFF, 0x00]);

    run_from(&mut sys, BASIC_ERROR, 64);

    assert_eq!(
        sys.error_code, None,
        "$A437 with the BASIC ROM banked out did not raise a BASIC error"
    );
    assert_eq!(sys.output, b"\x51", "the RAM code at $A437 must execute");
}

#[test]
fn code_executing_at_ffd2_with_the_kernal_banked_out_is_not_chrout() {
    let mut sys = System::new();
    sys.mem.ram[0x0001] = BANK_KERNAL_OUT;

    poke(&mut sys, CHROUT, &[0xA9, 0x99, 0x8D, 0x00, 0x04, 0x00]);

    run_from(&mut sys, CHROUT, 64);

    assert!(
        sys.output.is_empty(),
        "nothing went through CHROUT: the KERNAL was not mapped, got {:?}",
        sys.output
    );
    assert_eq!(
        sys.mem.ram[0x0400], 0x99,
        "the RAM instruction at $FFD2 must execute"
    );
}

#[test]
fn reaching_ready_with_the_basic_rom_mapped_still_ends_the_run() {
    let mut sys = System::new();
    assert_eq!(sys.mem.ram[0x0001], BANK_ALL_IN, "cold-start banking");
    // Bytes underneath are irrelevant: with the ROM mapped they are not what executes.
    poke(&mut sys, BASIC_READY, &[0xA9, 0x42, 0x20, 0xD2, 0xFF, 0x00]);

    let executed = run_from(&mut sys, BASIC_READY, 64);

    assert!(sys.ready_reached, "$A474 with BASIC mapped IS the READY prompt");
    assert_eq!(executed, 1, "the run stops there");
    assert!(sys.output.is_empty(), "READY is a trap, not code");
}

#[test]
fn reaching_the_error_entry_with_the_basic_rom_mapped_still_records_the_error() {
    let mut sys = System::new();
    sys.mem.ram[0x39] = 0x0A;
    sys.mem.ram[0x3A] = 0x00;
    sys.start_at(BASIC_ERROR); // resets the registers, so set X after
    sys.cpu.regs.x = 0x0B;
    sys.step();

    assert_eq!(sys.error_code, Some(0x0B));
    assert_eq!(sys.error_line, Some(10));
}

#[test]
fn chrout_with_the_kernal_mapped_still_traps() {
    let mut sys = System::new();
    sys.start_at(CHROUT); // resets the registers, so set A after
    sys.cpu.regs.a = 0x41;
    sys.step();

    assert_eq!(sys.output, b"A");
}

#[test]
fn verifying_a_rom_address_is_rom() {
    let mut sys = System::new();
    for (ctrl, basic, kernal) in [
        (0x37u8, true, true),   // cold start
        (0x36, false, true),
        (0x35, false, false),
        (0x34, false, false),
        (0x30, false, false),
    ] {
        sys.mem.ram[0x0001] = ctrl;
        assert_eq!(
            sys.mem.basic_mapped(),
            basic,
            "basic_mapped for $01 = ${ctrl:02X}"
        );
        assert_eq!(
            sys.mem.kernal_mapped(),
            kernal,
            "kernal_mapped for $01 = ${ctrl:02X}"
        );
    }
}
