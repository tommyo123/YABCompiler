//! An empty `DATA` field is a zero, and the ML scanners have to see it.

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

fn loader(zero: &str, dest_hi: u8) -> String {
    format!(
        "10 FORI=1TO35:READQ:NEXT\n\
         20 END\n\
         30 DATA169,{zero},133,251,169,208,133,252,169,{zero},133,253,169,{dest_hi},133,254,162,16,160\n\
         40 DATA{zero},177,251,145,253,200,208,249,230,252,230,254,202,208,240,96\n"
    )
}

#[test]
fn an_empty_data_field_does_not_break_the_ml_run() {
    let r = reserved_of(&loader("", 0x88));
    assert!(
        covers(&r, 0x8800, 0x97FF),
        "$8800 + 16 pages is $8800-$97FF, got {r:02X?}"
    );
}

#[test]
fn an_empty_field_and_an_explicit_zero_agree_exactly() {
    let empty = reserved_of(&loader("", 0x88));
    let zero = reserved_of(&loader("0", 0x88));
    assert_eq!(
        empty, zero,
        "`DATA 169,,133` and `DATA 169,0,133` are the same bytes"
    );
}

#[test]
fn the_destination_comes_out_of_the_data() {
    let r = reserved_of(&loader("", 0x50));
    assert!(
        covers(&r, 0x5000, 0x5FFF),
        "destination byte $50 means $5000-$5FFF, got {r:02X?}"
    );
    assert!(
        !touched(&r, 0x8800),
        "the $8800 answer belongs to the other program, got {r:02X?}"
    );
}

#[test]
fn a_quoted_string_still_breaks_the_run() {
    let src = loader("", 0x88).replace("133,254,162", "133,254,\"X\",162");
    let r = reserved_of(&src);
    assert!(
        !touched(&r, 0x8800),
        "a string between the pointer set-up and the kernel breaks the blob, got {r:02X?}"
    );
}

#[test]
fn an_unquoted_word_still_breaks_the_run() {
    let src = loader("", 0x88).replace("133,254,162", "133,254,END,162");
    let r = reserved_of(&src);
    assert!(
        !touched(&r, 0x8800),
        "an unquoted word is not a byte and must break the blob, got {r:02X?}"
    );
}

#[test]
fn a_negative_sentinel_still_breaks_the_run() {
    let src = loader("", 0x88).replace("133,254,162", "133,254,-1,162");
    let r = reserved_of(&src);
    assert!(
        !touched(&r, 0x8800),
        "a -1 sentinel must still break the blob, got {r:02X?}"
    );
}

#[test]
fn a_non_byte_number_still_breaks_the_run() {
    for item in ["300", "1.5"] {
        let src = loader("", 0x88).replace("133,254,162", &format!("133,254,{item},162"));
        let r = reserved_of(&src);
        assert!(
            !touched(&r, 0x8800),
            "{item} is not a byte and must break the blob, got {r:02X?}"
        );
    }
}

#[test]
fn a_pool_of_empty_fields_reserves_nothing() {
    let r = reserved_of("10 FORI=1TO8:READQ:NEXT\n20 END\n30 DATA,,,,,,,\n");
    assert!(
        r.is_empty(),
        "eight zeroes are not a copy kernel, got {r:02X?}"
    );
}

#[test]
fn the_item_count_is_unchanged_by_the_spelling() {
    let a = reserved_of("10 T=0\n20 READA:POKE12288+T,A:T=T+1:IFT<8THEN20\n30 END\n40 DATA0,0,0,0,0,0,0,0\n");
    let b = reserved_of("10 T=0\n20 READA:POKE12288+T,A:T=T+1:IFT<8THEN20\n30 END\n40 DATA,,,,,,,\n");
    assert_eq!(
        a, b,
        "eight empty fields bound the loader exactly as eight zeroes do"
    );
    assert!(
        covers(&a, 0x3000, 0x3007),
        "eight DATA items bound the loader to $3000-$3007, got {a:02X?}"
    );
}

#[test]
fn a_program_with_no_data_reserves_nothing() {
    let r = reserved_of("10 PRINT\"HI\"\n20 FORI=1TO10:PRINTI:NEXT\n30 END\n");
    assert!(r.is_empty(), "no DATA, no reservation, got {r:02X?}");
}
