//! `$D018` is relative to the VIC bank, and so is character ROM.

use yabcompiler_core::{CompileOptions, compile_with_options, tokenize_program};

fn reserved_of(src: &str) -> Vec<(u16, u16)> {
    let tok = tokenize_program(src).unwrap_or_else(|e| panic!("tokenize: {e:?}"));
    compile_with_options(&tok, CompileOptions::default())
        .unwrap_or_else(|e| panic!("compile: {e}"))
        .diagnostics
        .auto_reserved
}

/// Does some reserved range cover every byte of `lo..=hi`?
fn covers(reserved: &[(u16, u16)], lo: u16, hi: u16) -> bool {
    (lo..=hi).all(|a| reserved.iter().any(|(s, e)| a >= *s && a <= *e))
}

fn touched(reserved: &[(u16, u16)], addr: u16) -> bool {
    reserved.iter().any(|(s, e)| addr >= *s && addr <= *e)
}

#[test]
fn bank_one_shifts_the_charset_window_up_by_sixteen_k() {
    let r = reserved_of("10 POKE56576,2\n20 POKE53272,28\n30 END\n");
    assert!(
        covers(&r, 0x7000, 0x77FF),
        "bank $4000 + bits 110 is $7000-$77FF, got {r:02X?}"
    );
    assert!(
        !touched(&r, 0x3000),
        "$3000 is the bank-0 answer and must not be reserved here, got {r:02X?}"
    );
}

#[test]
fn the_bank_bits_are_inverted() {
    // %01 -> bank 2 -> $8000. bits 3:1 of 30 = 111 -> +$3800.
    let r2 = reserved_of("10 POKE56576,5\n20 POKE53272,30\n30 END\n");
    assert!(
        covers(&r2, 0xB800, 0xBFFF),
        "POKE 56576,5 is bank 2 ($8000); 30 names +$3800, so $B800-$BFFF, got {r2:02X?}"
    );

    let r1 = reserved_of("10 POKE56576,2\n20 POKE53272,30\n30 END\n");
    assert!(
        covers(&r1, 0x7800, 0x7FFF),
        "POKE 56576,2 is bank 1 ($4000), so $7800-$7FFF, got {r1:02X?}"
    );
}

#[test]
fn the_read_modify_write_spelling_resolves_the_bank() {
    let r = reserved_of("10 POKE56576,(PEEK(56576)AND252)OR2\n20 POKE53272,28\n30 END\n");
    assert!(
        covers(&r, 0x7000, 0x77FF),
        "AND 252 OR 2 pins bits 0:1 to %10 = bank 1, so $7000-$77FF, got {r:02X?}"
    );
}

/// Bank 1, bits `010`.
#[test]
fn bits_010_are_ram_in_bank_one_and_must_be_reserved() {
    let r = reserved_of("10 POKE56576,2\n20 POKE53272,20\n30 END\n");
    assert!(
        covers(&r, 0x5000, 0x57FF),
        "bank $4000 + bits 010 is $5000-$57FF of plain RAM, got {r:02X?}"
    );
}

#[test]
fn setting_the_bank_bits_to_outputs_does_not_move_the_bank() {
    let r = reserved_of("10 POKE56578,PEEK(56578)OR3\n20 POKE53272,28\n30 END\n");
    assert!(
        covers(&r, 0x3000, 0x37FF),
        "POKE 56578,...OR3 leaves the bank at $0000, so $3000-$37FF, got {r:02X?}"
    );
}

#[test]
fn a_bank_write_naming_the_stock_bank_still_reserves_the_stock_window() {
    let r = reserved_of("10 POKE56576,PEEK(56576)OR3\n20 POKE53272,28\n30 END\n");
    assert!(
        covers(&r, 0x3000, 0x37FF),
        "bits 0:1 = %11 is bank 0, so $3000-$37FF, got {r:02X?}"
    );
}

#[test]
fn the_stock_bank_reserves_exactly_what_it_always_did() {
    let r = reserved_of("10 POKE53272,28\n20 END\n");
    assert_eq!(
        r,
        vec![(0x3000, 0x37FF)],
        "POKE 53272,28 with no bank move must still be exactly $3000-$37FF"
    );
}

#[test]
fn the_rom_charset_is_still_discarded_in_bank_zero() {
    for v in [20u32, 22] {

        let r = reserved_of(&format!("10 POKE53272,{v}\n20 END\n"));
        assert!(
            !touched(&r, 0x1000) && !touched(&r, 0x1800),
            "POKE 53272,{v} in bank 0 is a ROM charset and must reserve nothing, got {r:02X?}"
        );
    }
}

#[test]
fn the_planet_probe_charset_is_rom_in_bank_2() {
    let r = reserved_of("171 POKE56576,5:POKE648,132\n190 POKE53272,21\n200 END\n");
    assert!(
        !touched(&r, 0x9000) && !touched(&r, 0x97FF),
        "bank 2 sees character ROM at $9000; nothing there is ours, got {r:02X?}"
    );

    assert!(
        covers(&r, 0x8400, 0x87E7),
        "POKE 648,132 must still reserve the screen at $8400-$87E7, got {r:02X?}"
    );
}

#[test]
fn bank_two_still_reserves_its_non_rom_windows() {
    // bits 3:1 of 28 = 110 -> +$3000 -> $B000, plain RAM in bank 2.
    let r = reserved_of("10 POKE56576,5\n20 POKE53272,28\n30 END\n");
    assert!(
        covers(&r, 0xB000, 0xB7FF),
        "bank $8000 + bits 110 is $B000-$B7FF and is RAM, got {r:02X?}"
    );
}

#[test]
fn an_unreadable_bank_operand_reserves_nothing() {
    let r = reserved_of("10 INPUTX\n20 POKE56576,X\n30 POKE53272,28\n40 END\n");
    assert!(
        r.iter().all(|(s, e)| *e < 0x3000 || *s > 0x37FF),
        "an unknown bank must not fall back to the bank-0 window, got {r:02X?}"
    );
}

#[test]
fn two_different_banks_refuse() {
    let r = reserved_of("10 POKE56576,2\n20 POKE53272,28\n30 POKE56576,3\n40 END\n");
    assert!(
        !touched(&r, 0x7000) && !touched(&r, 0x3000),
        "a program naming two banks must reserve neither window, got {r:02X?}"
    );
}

#[test]
fn releasing_the_bank_bits_refuses() {
    let r = reserved_of("10 POKE56576,2\n20 POKE56578,PEEK(56578)AND252\n30 POKE53272,28\n40 END\n");
    assert!(
        !touched(&r, 0x7000),
        "with the bank bits made inputs the bank is not $4000, got {r:02X?}"
    );
}

#[test]
fn bitmap_mode_still_vetoes_in_every_bank() {
    for bank in ["", "10 POKE56576,2\n"] {
        let src = format!("{bank}15 POKE53265,59\n20 POKE53272,28\n30 END\n");
        let r = reserved_of(&src);
        assert!(
            !touched(&r, 0x3000) && !touched(&r, 0x7000),
            "bitmap mode must reserve no charset window, got {r:02X?} for {src:?}"
        );
    }
}

#[test]
fn a_program_that_touches_none_of_this_reserves_nothing() {
    let r = reserved_of("10 PRINT\"HI\"\n20 FORI=1TO10:PRINTI:NEXT\n30 END\n");
    assert!(
        r.is_empty(),
        "a program with no POKE should auto-reserve nothing, got {r:02X?}"
    );
}

#[test]
fn a_window_above_the_auto_reserve_fence_is_dropped() {
    let r = reserved_of("10 POKE56576,4\n20 POKE53272,28\n30 END\n");
    assert!(
        r.is_empty(),
        "bank $C000 + $3000 is $F000, outside the fence; nothing to reserve, got {r:02X?}"
    );

    let r2 = reserved_of("10 POKE56576,4\n20 POKE53272,2\n30 END\n");
    assert!(
        covers(&r2, 0xC800, 0xCFFF),
        "bank $C000 + bits 001 is $C800-$CFFF, inside the fence, got {r2:02X?}"
    );
}
