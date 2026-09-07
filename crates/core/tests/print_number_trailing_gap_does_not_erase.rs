//! The gap BASIC prints after a number steps over the column ; it must never erase it.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const OVERPRINT: &str = concat!(
    "10 x=1\n",
    "15 print chr$(147);\n",
    "20 print \"#######################################\"\n",
    "30 print chr$(19);\n",
    "40 print x\n",
);

/// The 39 `#` of line 20.
const BAR: usize = 39;

#[rustfmt::skip]
const NATIVE_CHROUT: &[u8] = &[
    0x0d,
    0x93,
    0x23, 0x23, 0x23, 0x23, 0x23, 0x23, 0x23, 0x23, 0x23, 0x23, 0x23, 0x23, 0x23,
    0x23, 0x23, 0x23, 0x23, 0x23, 0x23, 0x23, 0x23, 0x23, 0x23, 0x23, 0x23, 0x23,
    0x23, 0x23, 0x23, 0x23, 0x23, 0x23, 0x23, 0x23, 0x23, 0x23, 0x23, 0x23, 0x23,
    0x0d,
    0x13,
    0x20, 0x31, 0x1d,
    0x0d,
];

const NATIVE_ROW0: &str = " 1#####################################";

fn basic_rom() -> Vec<u8> {
    std::fs::read(common::repo_path("crates/emu64/roms/basic.bin")).expect("BASIC ROM image")
}

/// One byte of the BASIC ROM, addressed the way the CPU sees it.
fn at(rom: &[u8], addr: u16) -> u8 {
    rom[usize::from(addr) - 0xA000]
}

/// What the interpreter emits after the digits of a number, taken from the interpreter.
fn rom_gap_bytes() -> (u8, u8) {
    let rom = basic_rom();
    assert_eq!(
        (at(&rom, 0xAB3B), at(&rom, 0xAB3C)),
        (0xA5, 0x13),
        "$AB3B no longer opens `LDA $13` — the gap routine has moved and \
         this oracle is reading the wrong code"
    );
    assert_eq!(at(&rom, 0xAB3D), 0xF0, "$AB3D is not the BEQ on CHANNL");
    let taken = (0xAB3Fi32 + i32::from(at(&rom, 0xAB3E) as i8)) as u16;
    assert_eq!(at(&rom, taken), 0xA9, "the BEQ target is not an `LDA #`");
    assert_eq!(at(&rom, 0xAB3F), 0xA9, "the fall-through is not an `LDA #`");
    (at(&rom, taken + 1), at(&rom, 0xAB40))
}

#[test]
fn the_interpreters_print_number_tail_ends_in_the_gap_routine() {
    let rom = basic_rom();
    let tail: Vec<u8> = (0xAABCu16..=0xAAC7).map(|a| at(&rom, a)).collect();
    assert_eq!(
        tail,
        vec![
            0x20, 0xDD, 0xBD,
            0x20, 0x87, 0xB4,
            0x20, 0x21, 0xAB,
            0x20, 0x3B, 0xAB, // JSR PRINT_GAP
        ],
        "BASIC's PRINT-a-number tail at $AABC is not the sequence this \
         file's oracle assumes"
    );
}

#[test]
fn the_roms_screen_gap_is_cursor_right_and_the_space_is_the_redirected_one() {
    let (screen, redirected) = rom_gap_bytes();
    assert_eq!(screen, 0x1D, "ROM's screen-channel gap byte");
    assert_eq!(redirected, 0x20, "ROM's redirected gap byte");
    assert_ne!(
        screen, 0x20,
        "a space erases the column that PRINT padding must preserve"
    );
}

fn chrout_of(src: &str, profile: Profile) -> Vec<u8> {
    let prg = tokenize_program(src).expect("tokenizes");
    let opts = CompileOptions {
        profile,
        ..CompileOptions::default()
    };
    let image = compile_with_options(&prg, opts)
        .unwrap_or_else(|e| panic!("compiles: {e}"))
        .prg_bytes;
    let r = emu64::run_prg_to_end(&image, common::MAX_INSNS).expect("runs");
    assert!(r.clean_exit, "{profile:?}: no clean exit");
    r.output
}

#[test]
fn numeric_overprint_matches_the_interpreter_byte_stream() {
    assert_eq!(
        OVERPRINT.lines().nth(2).unwrap().matches('#').count(),
        BAR,
        "line 20 must contain the expected bar length"
    );
    let (gap, _) = rom_gap_bytes();

    let mut want = vec![0x93];
    want.extend(std::iter::repeat_n(b'#', BAR));
    want.extend([0x0d, 0x13, 0x20, b'1', gap, 0x0d]);

    assert_eq!(
        want,
        NATIVE_CHROUT[1..],
        "the ROM-built expectation and the recorded native stream disagree"
    );

    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let out = chrout_of(OVERPRINT, profile);
        assert_eq!(
            out, want,
            "{profile:?}: issue #11 — the byte after the digit must be the \
             ROM's ${gap:02X}, not a space.\n  got  {out:02x?}\n  want {want:02x?}"
        );
    }
}

fn overprinted_row(stream: &[u8]) -> String {
    let mut screen = [[b' '; 40]; 25];
    let (mut row, mut col) = (0usize, 0usize);
    let advance = |row: &mut usize, col: &mut usize| {
        *col += 1;
        if *col == 40 {
            *col = 0;
            *row = (*row + 1).min(24);
        }
    };
    for &b in stream {
        match b {
            0x93 => {
                screen = [[b' '; 40]; 25];
                row = 0;
                col = 0;
            }
            0x13 => {
                row = 0;
                col = 0;
            }
            0x0d => {
                col = 0;
                row = (row + 1).min(24);
            }

            0x1d => advance(&mut row, &mut col),
            0x20..=0x5f => {
                screen[row][col] = b;
                advance(&mut row, &mut col);
            }
            other => panic!("byte ${other:02X} is not modelled by overprinted_row"),
        }
    }
    String::from_utf8(screen[0].to_vec())
        .expect("ascii row")
        .trim_end()
        .to_string()
}

/// Numeric PRINT padding preserves the character after the printed value.
#[test]
fn the_overprinted_row_keeps_the_character_the_gap_steps_over() {
    assert_eq!(
        overprinted_row(NATIVE_CHROUT),
        NATIVE_ROW0,
        "the screen model does not reproduce the interpreter's own \
         recorded row from the interpreter's own recorded stream — fix \
         the model before trusting it"
    );
    assert_eq!(
        NATIVE_ROW0.matches('#').count(),
        BAR - 2,
        "exactly two of the bar's characters are overwritten: the blank \
         standing in for the missing minus, and the digit"
    );

    let row = overprinted_row(&chrout_of(OVERPRINT, Profile::Default));
    assert_eq!(
        row, NATIVE_ROW0,
        "issue #11: the compiled program left `{row}` where BASIC V2 \
         leaves `{NATIVE_ROW0}`"
    );
}
