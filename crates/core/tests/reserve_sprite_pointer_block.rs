//! `POKE 2040, block` declares 64 bytes of RAM as sprite data, and the program will fill them whether or not the fill is analysable.

use yabcompiler_core::{CompileOptions, compile_with_options, tokenize_program};

fn reserved_of(src: &str) -> Vec<(u16, u16)> {
    let tok = tokenize_program(src).unwrap_or_else(|e| panic!("tokenize: {e:?}"));
    compile_with_options(&tok, CompileOptions::default())
        .unwrap_or_else(|e| panic!("compile: {e}"))
        .diagnostics
        .auto_reserved
}

fn covers(reserved: &[(u16, u16)], lo: u16, hi: u16) -> bool {
    (lo..=hi).all(|a| reserved.iter().any(|(s, e)| a >= *s && a <= *e))
}

fn touched(reserved: &[(u16, u16)], addr: u16) -> bool {
    reserved.iter().any(|(s, e)| addr >= *s && addr <= *e)
}

#[test]
fn a_literal_sprite_pointer_reserves_its_block() {
    let r = reserved_of("10 POKE2040,236\n20 END\n");
    assert!(
        covers(&r, 0x3B00, 0x3B3F),
        "POKE 2040,236 must reserve $3B00-$3B3F, got {r:02X?}"
    );
    assert!(
        !touched(&r, 0x3B40),
        "the block is 64 bytes and stops there, got {r:02X?}"
    );
}

#[test]
fn every_one_of_the_eight_cells_is_a_pointer() {
    for (i, cell) in (2040u32..=2047).enumerate() {
        let block = 200 + i as u32;
        let base = (block * 64) as u16;
        let r = reserved_of(&format!("10 POKE{cell},{block}\n20 END\n"));
        assert!(
            covers(&r, base, base + 63),
            "POKE {cell},{block} must reserve ${base:04X}, got {r:02X?}"
        );
    }
}

#[test]
fn the_loop_spelling_reserves_the_whole_run() {
    let r = reserved_of("10 FORI=0TO3:POKE2040+I,250+I:NEXT\n20 END\n");
    assert!(
        covers(&r, 0x3E80, 0x3F7F),
        "blocks 250..253 are $3E80-$3F7F, got {r:02X?}"
    );
}

/// Combine character-window reservations without leaving gaps.
#[test]
fn both_pointer_forms_reserve_the_complete_window() {
    let mut src = String::new();
    for (n, b) in (236u32..=249).enumerate() {
        src.push_str(&format!("{} POKE2040,{b}\n", 10 + n * 10));
    }
    src.push_str("500 FORI=0TO3:POKE2040+I,250+I:NEXT\n");
    src.push_str("510 POKE2043,254\n520 POKE2042,255\n530 END\n");
    let r = reserved_of(&src);
    assert!(
        covers(&r, 0x3B00, 0x3FFF),
        "pointer writes must cover $3B00-$3FFF, got {r:02X?}"
    );
}

#[test]
fn the_cassette_buffer_blocks_reserve_nothing() {
    for block in [11u32, 13, 14, 15] {
        let r = reserved_of(&format!("10 POKE2040,{block}\n20 END\n"));
        let base = (block * 64) as u16;
        assert!(
            !touched(&r, base),
            "block {block} is ${base:04X}, below the auto-reserve window; got {r:02X?}"
        );
    }
}

#[test]
fn the_neighbouring_addresses_are_not_pointer_cells() {
    for addr in [2039u32, 2048, 2032, 1016] {
        let r = reserved_of(&format!("10 POKE{addr},236\n20 END\n"));
        assert!(
            !touched(&r, 0x3B00),
            "${addr:04X} is not a sprite pointer, got {r:02X?}"
        );
    }
}

#[test]
fn an_unknowable_block_reserves_nothing() {
    let r = reserved_of("10 INPUTB\n20 POKE2040,B\n30 END\n");
    let sprite_area = (0x0900u16..=0x3FFF).filter(|a| touched(&r, *a)).count();
    assert_eq!(
        sprite_area, 0,
        "a POKE 2040,<unknown> must reserve no sprite block, got {r:02X?}"
    );
}

#[test]
fn an_out_of_range_block_interval_is_dropped_not_clamped() {
    let r = reserved_of("10 FORI=0TO4000:POKE2040,I:NEXT\n20 END\n");
    assert!(
        !covers(&r, 0x0900, 0x3FFF),
        "an impossible interval must not reserve the whole sprite area, got {r:02X?}"
    );
}

#[test]
fn a_moved_vic_bank_refuses() {
    let r = reserved_of("10 POKE56576,2\n20 POKE2040,236\n30 END\n");
    assert!(
        !touched(&r, 0x3B00),
        "bank 2 makes $07F8 not the pointer table; reserve nothing, got {r:02X?}"
    );
}

#[test]
fn a_program_with_no_sprite_pointer_reserves_no_block() {
    let r = reserved_of("10 FORI=0TO9:PRINTI:NEXT\n20 END\n");
    assert!(
        r.is_empty() || !(0x0900u16..=0x3FFF).any(|a| touched(&r, a)),
        "no pointer write, no sprite reservation, got {r:02X?}"
    );
}

#[test]
fn the_older_reserve_rules_still_fire() {
    let charset = reserved_of("10 POKE53272,28\n20 POKE2040,236\n30 END\n");
    assert!(
        covers(&charset, 0x3000, 0x37FF),
        "the $D018 charset window must survive, got {charset:02X?}"
    );
    let screen = reserved_of("10 POKE648,132\n20 POKE2040,236\n30 END\n");
    assert!(
        covers(&screen, 0x8400, 0x87E7),
        "the $0288 screen window must survive, got {screen:02X?}"
    );
}

#[test]
fn the_reserved_block_really_is_a_hole_in_the_image() {

    let mut src = String::from("10 POKE2040,132\n");
    for n in 0..1400 {
        src.push_str(&format!("{} PRINT\"AAAAAAAAAAAAAAAAAAAAAAAA\"\n", 100 + n));
    }
    src.push_str("60000 END\n");
    let tok = tokenize_program(&src).unwrap();
    let out = compile_with_options(&tok, CompileOptions::default()).unwrap();
    let load = u16::from_le_bytes([out.prg_bytes[0], out.prg_bytes[1]]);
    let end = load as usize + out.prg_bytes.len() - 2;
    assert!(
        end > 0x2140,
        "the fixture must reach past the reserved block; ended at ${end:04X}"
    );
    assert!(
        covers(&out.diagnostics.auto_reserved, 0x2100, 0x213F),
        "the block must be reserved for this test to mean anything"
    );

    let lo = 0x2100usize - load as usize + 2;
    let hi = 0x213Fusize - load as usize + 2;
    assert!(
        out.prg_bytes[lo..=hi].iter().all(|b| *b == 0),
        "the reserved block holds emitted bytes: {:02X?}",
        &out.prg_bytes[lo..=hi]
    );
}
