//! A DATA-fed loader loop reserves the run it writes, not one byte of it.

mod common;

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

/// Generate a DATA loader with a sentinel and backward branch.
fn sentinel_loader() -> String {
    let mut s = String::from(
        "10 T=0\n\
         20 READ A: IF A<>-1 THEN POKE 12800+T,A: T=T+1: GOTO 20\n\
         30 END\n",
    );
    // 40 items, then the sentinel.
    for line in 0..4 {
        s.push_str(&format!("{} DATA ", 100 + line));
        let items: Vec<String> = (0..10).map(|i| (i * 3 % 256).to_string()).collect();
        s.push_str(&items.join(","));
        s.push('\n');
    }
    s.push_str("200 DATA -1\n");
    s
}

#[test]
fn a_data_fed_loader_reserves_more_than_the_base_byte() {
    let reserved = reserved_of(&sentinel_loader());
    let base = 12800u16;
    let covering: Vec<_> = reserved
        .iter()
        .filter(|(s, e)| base >= *s && base <= *e)
        .collect();
    assert!(
        !covering.is_empty(),
        "the loader base $3200 is not reserved at all: {reserved:04X?}"
    );
    let widest = covering.iter().map(|(s, e)| e - s + 1).max().unwrap();
    assert!(
        widest > 1,
        "the loader base is reserved as a SINGLE BYTE — the assembler will \
         lay the image down at $3201, in the loader's path. reserved: {reserved:04X?}"
    );
}

#[test]
fn the_reservation_spans_the_whole_data_supply() {
    // 41 items (40 values + the -1 sentinel), so the loader can write at most 41 bytes from 12800.
    let reserved = reserved_of(&sentinel_loader());
    assert!(
        covers(&reserved, 12800, 12800 + 41),
        "reservation does not cover the run the loader can write: {reserved:04X?}"
    );
}

#[test]
fn the_image_does_not_start_inside_the_loader_run() {
    let src = sentinel_loader();
    let tok = tokenize_program(&src).unwrap();
    let built = compile_with_options(&tok, CompileOptions::default()).unwrap();

    let end = 0x0801u32 + built.prg_bytes.len() as u32 - 3;
    assert_ne!(
        end, 12801,
        "image ends exactly one byte past the loader base — the one-byte \
         reservation is back"
    );
}

#[test]
fn for_bounded_block_keeps_its_under_approximation() {
    let src = "\
10 N0=12288\n\
20 FOR X=1 TO 8\n\
30 FOR N=0 TO 62: READ Q: POKE N0+N,Q: NEXT N\n\
40 N0=N0+64: NEXT X\n\
50 END\n\
100 DATA 1,2,3,4,5,6,7,8,9,10\n";
    let reserved = reserved_of(src);
    assert!(
        covers(&reserved, 12288, 12288 + 62),
        "the FOR-bounded first block lost its reservation: {reserved:04X?}"
    );
}

/// The bound is the DATA supply, so a program with no DATA gets no loader reservation however walking its counters are.
#[test]
fn a_loader_with_no_data_reserves_nothing_extra() {
    let src = "\
10 T=0\n\
20 POKE 12800+T,255: T=T+1: IF T<10 THEN 20\n\
30 END\n";
    let reserved = reserved_of(src);
    let widest = reserved
        .iter()
        .filter(|(s, e)| 12800 >= *s && 12800 <= *e)
        .map(|(s, e)| e - s + 1)
        .max()
        .unwrap_or(0);
    assert!(
        widest <= 1,
        "reserved a DATA-sized run for a loop that reads no DATA: {reserved:04X?}"
    );
}

/// The poked value has to be a scalar the program `READ`s.
#[test]
fn a_computed_value_loader_does_not_borrow_the_data_bound() {
    let src = "\
10 T=0\n\
20 READ A\n\
30 POKE 12800+T,T*2: T=T+1: IF T<5 THEN 30\n\
40 END\n\
100 DATA 1,2,3,4,5,6,7,8,9,10\n";
    let reserved = reserved_of(src);
    let widest = reserved
        .iter()
        .filter(|(s, e)| 12800 >= *s && 12800 <= *e)
        .map(|(s, e)| e - s + 1)
        .max()
        .unwrap_or(0);
    assert!(
        widest <= 1,
        "a POKE of a computed value took the DATA bound: {reserved:04X?}"
    );
}

/// A program without the idiom compiles to exactly the bytes it did before.
#[test]
fn a_program_without_the_idiom_is_unchanged() {
    let src = "\
10 FOR I=0 TO 10: READ A: POKE 12800+I,A: NEXT\n\
20 PRINT \"DONE\"\n\
100 DATA 1,2,3,4,5,6,7,8,9,10,11\n";
    let reserved = reserved_of(src);

    assert!(
        covers(&reserved, 12800, 12810),
        "the FOR-bounded loader lost its exact range: {reserved:04X?}"
    );
    assert!(
        !reserved.iter().any(|(s, e)| *s <= 12800 && *e > 12800 + 11),
        "the exact FOR range was widened by the DATA bound: {reserved:04X?}"
    );
}
