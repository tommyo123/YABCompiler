//! `POKE 648, page` moves the text screen, and the ROM writes there whether or not the program does.

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
fn poke_648_reserves_the_thousand_bytes_it_names() {
    let r = reserved_of("10 POKE648,132\n20 PRINT\"HI\"\n30 END\n");
    assert!(
        covers(&r, 0x8400, 0x87E7),
        "POKE 648,132 must reserve the whole screen $8400-$87E7, got {r:02X?}"
    );
    // Exactly 1000 bytes, not a page-rounded 1024: $87E8 is not screen.
    assert!(
        !touched(&r, 0x87E8) || r.iter().any(|(s, _)| *s > 0x87E8),
        "reservation should stop at $87E7, got {r:02X?}"
    );
}

#[test]
fn planet_probe_screen_covers_the_byte_that_was_clobbered() {
    let r = reserved_of("171 POKE56576,5:POKE648,132\n190 PRINT\"{clr}\"\n200 END\n");
    assert!(
        touched(&r, 0x8495),
        "$8495 is where Planet Probe's __STR_PRINT_I was overwritten; \
         it must be inside a reserved range, got {r:02X?}"
    );
}

#[test]
fn the_page_operand_is_what_selects_the_window() {
    // Repeated writes through one pointer do not identify a copy destination.
    let r = reserved_of("10 POKE648,68\n20 END\n");
    assert!(
        covers(&r, 0x4400, 0x47E7),
        "POKE 648,68 must reserve $4400-$47E7, got {r:02X?}"
    );
    assert!(
        !touched(&r, 0x8400),
        "POKE 648,68 must not reserve some other program's screen, got {r:02X?}"
    );
}

#[test]
fn the_stock_screen_page_reserves_nothing() {
    let r = reserved_of("10 POKE648,4\n20 PRINT\"HI\"\n30 END\n");
    assert!(
        !touched(&r, 0x0400) && !touched(&r, 0x07E7),
        "POKE 648,4 is the default screen and must reserve nothing, got {r:02X?}"
    );
}

#[test]
fn neighbouring_addresses_are_not_the_screen_pointer() {
    for (addr, page) in [(647u16, 132u16), (649, 132)] {
        let r = reserved_of(&format!("10 POKE{addr},{page}\n20 END\n"));
        assert!(
            !touched(&r, 0x8400),
            "POKE {addr},{page} is not $0288 and must reserve no screen, got {r:02X?}"
        );
    }
}

#[test]
fn a_non_literal_page_reserves_nothing() {
    let r = reserved_of("10 INPUTP\n20 POKE648,P\n30 END\n");
    assert!(
        r.iter().all(|(s, e)| *e < 0x4000 || *s > 0xC000),
        "POKE 648,<variable> must not invent a screen window, got {r:02X?}"
    );
}

#[test]
fn a_fully_known_expression_still_names_the_page() {
    let r = reserved_of("10 POKE648,128OR4\n20 END\n");
    assert!(
        covers(&r, 0x8400, 0x87E7),
        "POKE 648,128 OR 4 is $84 with every bit known, got {r:02X?}"
    );
}

#[test]
fn the_charset_window_still_reserves() {

    let r = reserved_of("10 POKE53272,28\n20 END\n");
    assert!(
        covers(&r, 0x3000, 0x37FF),
        "POKE 53272,28 must still reserve the charset at $3000-$37FF, got {r:02X?}"
    );
}

#[test]
fn a_program_that_moves_nothing_reserves_nothing() {
    let r = reserved_of("10 PRINT\"HI\"\n20 FORI=1TO10:PRINTI:NEXT\n30 END\n");
    assert!(
        r.is_empty(),
        "a program with no POKE should auto-reserve nothing, got {r:02X?}"
    );
}
