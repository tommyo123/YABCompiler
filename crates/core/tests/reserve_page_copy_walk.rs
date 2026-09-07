use yabcompiler_core::{CompileOptions, compile_with_options};

const STUB: u16 = 0x0F00;
const KERNEL: u16 = 0x0F70;

/// Print a control byte before entering the copy loop.
fn stub_to(target: u16) -> Vec<u8> {
    let [lo, hi] = target.to_le_bytes();
    vec![0xA9, 0x08, 0x20, 0xD2, 0xFF, 0x4C, lo, hi]
}

fn image(ml: &[u8]) -> Vec<u8> {
    let mut blob = stub_to(KERNEL);
    blob.resize((KERNEL - STUB) as usize, 0xAA);
    blob.extend(ml);
    image_with_ml(&blob, STUB, STUB)
}

fn image_no_stub(ml: &[u8]) -> Vec<u8> {
    let [lo, hi] = KERNEL.to_le_bytes();
    let mut blob = vec![0x4C, lo, hi];
    blob.resize((KERNEL - STUB) as usize, 0xAA);
    blob.extend(ml);
    image_with_ml(&blob, STUB, STUB)
}

fn image_with_ml(ml: &[u8], ml_addr: u16, entry: u16) -> Vec<u8> {
    const LOAD: u16 = 0x0801;
    let mut text: Vec<u8> = Vec::new();
    // 10 SYS<entry>
    let sys = format!("{entry}");
    let mut line10: Vec<u8> = vec![0x9E]; // SYS token
    line10.extend(sys.bytes());

    let line20: Vec<u8> = vec![0x80]; // END token
    let l10_at = LOAD;
    let l10_len = 4 + line10.len() + 1;
    let l20_at = l10_at + l10_len as u16;
    let l20_len = 4 + line20.len() + 1;
    let end_at = l20_at + l20_len as u16;

    text.extend(l20_at.to_le_bytes());
    text.extend(10u16.to_le_bytes());
    text.extend(&line10);
    text.push(0);
    text.extend((end_at + 2).to_le_bytes());
    text.extend(20u16.to_le_bytes());
    text.extend(&line20);
    text.push(0);
    text.extend([0, 0]); // end of program

    let mut body = text;
    let ml_off = (ml_addr - LOAD) as usize;
    assert!(ml_off >= body.len(), "ML would overlap the BASIC text");
    body.resize(ml_off, 0xAA);
    body.extend(ml);

    let mut prg = LOAD.to_le_bytes().to_vec();
    prg.extend(body);
    prg
}

fn reserved_of_prg(prg: &[u8]) -> Vec<(u16, u16)> {
    compile_with_options(prg, CompileOptions::default())
        .unwrap_or_else(|e| panic!("compile: {e}"))
        .diagnostics
        .auto_reserved
}

fn reserved_of_source(src: &str) -> Vec<(u16, u16)> {
    let tok = yabcompiler_core::tokenize_program(src).unwrap_or_else(|e| panic!("tokenize: {e:?}"));
    reserved_of_prg(&tok)
}

fn covers(reserved: &[(u16, u16)], lo: u16, hi: u16) -> bool {
    (lo..=hi).all(|a| reserved.iter().any(|(s, e)| a >= *s && a <= *e))
}

fn touched(reserved: &[(u16, u16)], addr: u16) -> bool {
    reserved.iter().any(|(s, e)| addr >= *s && addr <= *e)
}

fn descending_copy_kernel(src_hi: u8, dst_hi: u8, lim: u8) -> Vec<u8> {
    vec![
        0xA0, 0x00,
        0x84, 0xFC,
        0x84, 0xFE,
        0xA9, src_hi, 0x85, 0xFD, // LDA #src / STA $FD
        0xA9, dst_hi, 0x85, 0xFF, // LDA #dst / STA $FF
        0xB1, 0xFC,
        0x91, 0xFE,
        0xC8, 0xD0, 0xF9,
        0xC6, 0xFD,
        0xC6, 0xFF,
        0xA5, 0xFF, 0xC9, lim, 0xD0, 0xEF, // LDA $FF / CMP #lim / BNE
        0x60,
    ]
}

#[test]
fn descending_copy_reserves_the_complete_destination() {
    let prg = image(&descending_copy_kernel(0x43, 0x63, 0x2F));
    let r = reserved_of_prg(&prg);
    assert!(
        covers(&r, 0x3000, 0x63FF),
        "the 52-page descending copy must reserve $3000-$63FF, got {r:02X?}"
    );
}

#[test]
fn the_descending_walk_stops_one_page_short_of_its_limit() {
    let r = reserved_of_prg(&image(&descending_copy_kernel(0x43, 0x63, 0x2F)));
    assert!(touched(&r, 0x3000), "the last page written must be reserved");
    assert!(
        !touched(&r, 0x2FFF),
        "the page the loop stops ON is never written and must not be reserved: {r:02X?}"
    );
}

/// The sharpest of the set.
#[test]
fn a_counted_descending_walk_and_a_compared_one_agree_exactly() {
    let compared = reserved_of_prg(&image(&descending_copy_kernel(0x43, 0x63, 0x2F)));
    let counted = reserved_of_prg(&image(&[
            0xA0, 0x00, 0x84, 0xFC, 0x84, 0xFE,
            0xA9, 0x43, 0x85, 0xFD,
            0xA9, 0x63, 0x85, 0xFF,
            0xA2, 52,
            0xB1, 0xFC, 0x91, 0xFE, 0xC8, 0xD0, 0xF9,
            0xC6, 0xFD, 0xC6, 0xFF,
            0xCA, 0xD0, 0xF3,
            0x60,
        ]));
    assert_eq!(
        compared, counted,
        "the two termination forms must name the same destination extent"
    );
}

#[test]
fn a_descending_copy_resolves_from_a_data_pool_too() {
    let bytes = descending_copy_kernel(0x43, 0x63, 0x2F)
        .iter()
        .map(|b| b.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let r = reserved_of_source(&format!(
        "10 FORI=828TO{}:READQ:POKEI,Q:NEXT\n20 SYS828\n30 DATA{bytes}\n",
        828 + descending_copy_kernel(0x43, 0x63, 0x2F).len() - 1
    ));
    assert!(
        covers(&r, 0x3000, 0x63FF),
        "a descending kernel in DATA must reserve $3000-$63FF, got {r:02X?}"
    );
}

#[test]
fn an_ascending_copy_in_the_image_is_reserved() {
    let r = reserved_of_prg(&image(&[
            0xA9, 0x00, 0x85, 0xFB, 0xA9, 0xD0, 0x85, 0xFC,
            0xA9, 0x00, 0x85, 0xFD, 0xA9, 0x30, 0x85, 0xFE,
            0xA2, 0x10,
            0xA0, 0x00,
            0xB1, 0xFB, 0x91, 0xFD, 0xC8, 0xD0, 0xF9,
            0xE6, 0xFC, 0xE6, 0xFE, 0xCA, 0xD0, 0xF3, 0x60,
        ]));
    assert!(
        covers(&r, 0x3000, 0x3FFF),
        "16 ascending pages from $3000 must reserve $3000-$3FFF, got {r:02X?}"
    );
}

#[test]
fn the_chrout_stub_is_what_silences_the_write_extent_analysis() {
    let ml = [0x8D, 0x00, 0x50, 0x60];
    assert!(
        touched(&reserved_of_prg(&image_no_stub(&ml)), 0x5000),
        "without the stub, write_extent must resolve this store"
    );
    assert!(
        !touched(&reserved_of_prg(&image(&ml)), 0x5000),
        "with the stub, write_extent must refuse the whole image"
    );
}

#[test]
fn the_ascending_data_pool_extent_is_unchanged() {
    let r = reserved_of_source(
        "10 FORI=1TO35:READQ:NEXT\n\
         20 END\n\
         30 DATA169,,133,251,169,208,133,252,169,,133,253,169,136,133,254,162,16,160\n\
         40 DATA,177,251,145,253,200,208,249,230,252,230,254,202,208,240,96\n",
    );
    assert!(
        covers(&r, 0x8800, 0x97FF),
        "the 16-page ascending copy to $8800 must still reserve $8800-$97FF, got {r:02X?}"
    );
    assert!(
        !touched(&r, 0x9800),
        "and must not reserve a seventeenth page: {r:02X?}"
    );
}

/// One pointer up and one down is not a page copy.
#[test]
fn a_kernel_that_steps_its_pointers_in_opposite_directions_reserves_nothing() {
    let mut ml = descending_copy_kernel(0x43, 0x63, 0x2F);

    let i = ml.iter().position(|&b| b == 0xC6).unwrap();
    ml[i] = 0xE6;
    let r = reserved_of_prg(&image(&ml));
    assert!(
        !touched(&r, 0x3000) && !touched(&r, 0x6300),
        "a mixed-direction kernel must reserve nothing, got {r:02X?}"
    );
}

/// The two steps have to be exactly the two pointer high bytes.
#[test]
fn steps_that_are_not_the_two_high_bytes_reserve_nothing() {
    let mut ml = descending_copy_kernel(0x43, 0x63, 0x2F);
    // `DEC $FD` -> `DEC $F0`, which is neither $FC+1 nor $FE+1.
    let i = ml.windows(2).position(|w| w == [0xC6, 0xFD]).unwrap();
    ml[i + 1] = 0xF0;
    let r = reserved_of_prg(&image(&ml));
    assert!(
        !touched(&r, 0x3000) && !touched(&r, 0x6300),
        "reserved {r:02X?}"
    );
}

#[test]
fn a_kernel_with_a_single_pointer_reserves_nothing() {
    let ml = vec![
        0xA0, 0x00, 0x84, 0xFE, 0xA9, 0x63, 0x85, 0xFF,
        0xB1, 0xFE, 0x91, 0xFE, 0xC8, 0xD0, 0xF9, // both through $FE
        0xC6, 0xFF, 0xC6, 0xFF, 0xA5, 0xFF, 0xC9, 0x2F, 0xD0, 0xEF, 0x60,
    ];
    let r = reserved_of_prg(&image(&ml));
    assert!(!touched(&r, 0x3000), "reserved {r:02X?}");
}

/// No immediate loads before the loop means no destination.
#[test]
fn an_unresolvable_destination_pointer_reserves_nothing() {
    // The `A9 63 / 85 FF` pair that sets the destination high byte is replaced by two `NOP`s, so `$FF` is never known.
    let mut ml = descending_copy_kernel(0x43, 0x63, 0x2F);
    let i = ml.windows(4).position(|w| w == [0xA9, 0x63, 0x85, 0xFF]).unwrap();
    ml[i..i + 4].copy_from_slice(&[0xEA, 0xEA, 0xEA, 0xEA]);
    let r = reserved_of_prg(&image(&ml));
    assert!(
        !touched(&r, 0x3000) && !touched(&r, 0x6300),
        "reserved {r:02X?}"
    );
}

/// A compare against a byte the loop does not walk says nothing about the trip count.
#[test]
fn a_compare_naming_neither_pointer_reserves_nothing() {
    let mut ml = descending_copy_kernel(0x43, 0x63, 0x2F);
    // `LDA $FF / CMP #$2F` -> `LDA $90 / CMP #$2F`.
    let i = ml.windows(3).position(|w| w == [0xA5, 0xFF, 0xC9]).unwrap();
    ml[i + 1] = 0x90;
    let r = reserved_of_prg(&image(&ml));
    assert!(!touched(&r, 0x3000), "reserved {r:02X?}");
}

#[test]
fn a_descending_walk_whose_limit_is_above_its_start_reserves_nothing() {
    // Starts at $30, walks down, compares against $63 ; never equal.
    let r = reserved_of_prg(&image(&descending_copy_kernel(0x43, 0x30, 0x63)));
    assert!(
        !touched(&r, 0x3000) && !touched(&r, 0x6300),
        "reserved {r:02X?}"
    );
}

/// A page count of zero is not "no pages", it is a loop the tracker misread ; `DEX` from 0 runs 256 times.
#[test]
fn a_zero_page_count_reserves_nothing() {
    let r = reserved_of_prg(&image(&[
            0xA9, 0x00, 0x85, 0xFB, 0xA9, 0xD0, 0x85, 0xFC,
            0xA9, 0x00, 0x85, 0xFD, 0xA9, 0x30, 0x85, 0xFE,
            0xA2, 0x00,
            0xA0, 0x00, 0xB1, 0xFB, 0x91, 0xFD, 0xC8, 0xD0, 0xF9,
            0xE6, 0xFC, 0xE6, 0xFE, 0xCA, 0xD0, 0xF3, 0x60,
        ]));
    assert!(!touched(&r, 0x3000), "reserved {r:02X?}");
}

#[test]
fn a_destination_outside_the_window_is_declined_not_clamped() {

    let r = reserved_of_prg(&image(&descending_copy_kernel(0x43, 0xF0, 0xDF)));
    assert!(
        !touched(&r, 0xCFFF) && !touched(&r, 0xE000),
        "an out-of-window copy must be declined whole, got {r:02X?}"
    );
}

#[test]
fn an_image_with_no_kernel_reserves_nothing_from_this_rule() {
    // A routine that stores, branches and returns, but never copies.
    let r = reserved_of_prg(&image(&[
            0xA9, 0x05, 0x8D, 0x20, 0xD0,
            0xA2, 0x10, 0xCA, 0xD0, 0xFD,
            0x60,
        ]));
    assert!(
        !r.iter().any(|(s, e)| *e - *s > 0x00FF),
        "no multi-page reservation may appear for a program with no copy loop: {r:02X?}"
    );
}
