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

/// Generate a program that installs and calls machine code in the cassette buffer.
fn loader(bytes: &[u8]) -> String {
    let items: Vec<String> = bytes.iter().map(|b| b.to_string()).collect();
    let mut s = String::from("160 POKE56,48:CLR\n170 FORI=828TO898:READA:POKEI,A:NEXT\n");
    // Ten items per DATA line, as the original is typed.
    for (n, chunk) in items.chunks(10).enumerate() {
        s.push_str(&format!("{} DATA {}\n", 180 + n, chunk.join(",")));
    }
    s.push_str("900 SYS828\n910 END\n");
    s
}

/// Copy 16 character-ROM pages to $3000.
const CHAR_ROM_COPY: [u8; 71] = [
    173, 14, 220, 41, 254, 141, 14, 220, 165, 1, 41, 251, 133, 1, 169, 0, 133, 251, 133, 253, 169,
    208, 133, 252, 169, 48, 133, 254, 162, 16, 160, 0, 177, 251, 145, 253, 200, 208, 249, 230, 252,
    230, 254, 202, 208, 240, 165, 1, 9, 4, 133, 1, 173, 14, 220, 9, 1, 141, 14, 220, 173, 24, 208,
    41, 240, 9, 12, 141, 24, 208, 96,
];

/// Copy 32 character-ROM pages with pointer low bytes initialized by STY.
const MISSION: [u8; 53] = [
    120, 165, 1, 72, 41, 251, 133, 1, 169, 208, 133, 3, 169, 48, 133, 5, 160, 0, 132, 2, 132, 4,
    162, 32, 177, 2, 145, 4, 200, 208, 249, 230, 3, 230, 5, 202, 208, 242, 104, 133, 1, 173, 24,
    208, 41, 241, 9, 12, 141, 24, 208, 88, 96,
];

#[test]
fn a_four_kilobyte_charset_copy_reserves_its_destination() {
    let reserved = reserved_of(&loader(&CHAR_ROM_COPY));
    assert!(
        covers(&reserved, 0x3000, 0x3FFF),
        "the copy fills $3000-$3FFF and nothing reserves it; our code lands \
         in the charset. reserved: {reserved:04X?}"
    );
}

#[test]
fn the_page_count_sets_the_length_eight_kilobytes_not_four() {
    // `LDX #$20` is 32 pages.
    let reserved = reserved_of(&loader(&MISSION));
    assert!(
        covers(&reserved, 0x3000, 0x4FFF),
        "an `LDX #$20` copy fills 8 KB from $3000; reserved: {reserved:04X?}"
    );
}

#[test]
fn pointer_low_bytes_may_be_zeroed_with_sty() {

    let reserved = reserved_of(&loader(&MISSION));
    assert!(
        touched(&reserved, 0x3000),
        "destination base $3000 was initialised via `LDY #0 / STY $04` and \
         went unseen; reserved: {reserved:04X?}"
    );
}

#[test]
fn a_data_pool_with_no_copy_kernel_reserves_nothing_at_the_destination() {
    // Identical DATA except the four kernel bytes `177,251,145,253` (`LDA ($FB),Y / STA ($FD),Y`) are replaced by NOPs.
    let mut bytes = CHAR_ROM_COPY;
    for b in &mut bytes[32..36] {
        *b = 234;
    }
    let reserved = reserved_of(&loader(&bytes));
    assert!(
        !touched(&reserved, 0x3000),
        "reserved $3000 for a program with no copy loop — the pass is \
         matching the immediates, not the block move: {reserved:04X?}"
    );
}

#[test]
fn an_inc_pair_that_does_not_advance_the_pointers_is_not_a_block_move() {
    // `230,254` (`INC $FE`, the destination high byte) becomes `230,200` (`INC $C8`).
    let mut bytes = CHAR_ROM_COPY;
    bytes[42] = 200;
    let reserved = reserved_of(&loader(&bytes));
    assert!(
        !touched(&reserved, 0x3400),
        "a loop whose INC pair does not advance both pointers was still \
         read as a multi-page copy: {reserved:04X?}"
    );
}

#[test]
fn a_destination_below_the_auto_reserve_window_is_declined() {

    let mut bytes = CHAR_ROM_COPY;
    bytes[25] = 4;
    let reserved = reserved_of(&loader(&bytes));
    assert!(
        !touched(&reserved, 0x0400),
        "reserved below the auto-reserve window: {reserved:04X?}"
    );
}

#[test]
fn the_reservation_does_not_swallow_the_address_space() {
    // A 4 KB copy must reserve 4 KB.
    let reserved = reserved_of(&loader(&CHAR_ROM_COPY));
    assert!(
        !touched(&reserved, 0x8000),
        "a $3000 charset copy reserved memory at $8000: {reserved:04X?}"
    );
    let widest = reserved.iter().map(|(s, e)| *e as u32 - *s as u32 + 1).max();
    assert!(
        widest.is_some_and(|w| w <= 0x2000),
        "widest reservation is implausible for a 4 KB copy: {reserved:04X?}"
    );
}

#[test]
fn the_loader_extent_itself_is_still_reserved() {

    let items: Vec<String> = MISSION.iter().map(|b| b.to_string()).collect();
    let mut src = String::from("19 FORI=51200TO51252\n20 READX:POKEI,X:NEXTI\n");
    for (n, chunk) in items.chunks(10).enumerate() {
        src.push_str(&format!("{} DATA {}\n", 22 + n, chunk.join(",")));
    }
    src.push_str("40 SYS51200\n50 END\n");
    let reserved = reserved_of(&src);
    assert!(
        covers(&reserved, 0xC800, 0xC834),
        "the loader's own landing area $C800-$C834 stopped being reserved: \
         {reserved:04X?}"
    );

    assert!(
        covers(&reserved, 0x3000, 0x4FFF),
        "destination not reserved in the $C800 loader shape: {reserved:04X?}"
    );
}
