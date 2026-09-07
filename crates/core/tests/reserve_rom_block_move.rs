//! Machine code inside a program's own image can aim BASIC's block move somewhere the BASIC text never names.

use yabcompiler_core::{CompileOptions, Compiled, compile_with_options};

struct Line {
    number: u16,
    body: Vec<u8>,
}

fn line(number: u16, body: &[u8]) -> Line {
    Line {
        number,
        body: body.to_vec(),
    }
}

fn build_prg(lines: &[Line], pad_to: u16) -> Vec<u8> {
    const LOAD: u16 = 0x0801;
    let mut addr = LOAD;
    let mut starts = Vec::new();
    for l in lines {
        starts.push(addr);
        addr += 5 + l.body.len() as u16;
    }
    let mut out = LOAD.to_le_bytes().to_vec();
    for (i, l) in lines.iter().enumerate() {
        let next = starts.get(i + 1).copied().unwrap_or(addr);
        out.extend_from_slice(&next.to_le_bytes());
        out.extend_from_slice(&l.number.to_le_bytes());
        out.extend_from_slice(&l.body);
        out.push(0);
    }
    out.extend_from_slice(&[0, 0]);
    // `$AA` filler, not `$00`: a run of zeroes is a plausible end-of-image and this block has to look like payload.
    while (out.len() as u16) < pad_to {
        out.push(0xAA);
    }
    out
}

/// `SYS <n>`, tokenised.
fn sys(n: u16) -> Vec<u8> {
    let mut v = vec![0x9E];
    v.extend_from_slice(n.to_string().as_bytes());
    v
}

/// A `REM` line carrying `ml` verbatim.
fn rem(ml: &[u8]) -> Vec<u8> {
    let mut v = vec![0x8F];
    v.extend_from_slice(ml);
    v
}

/// Generate a ROM block-move routine with configurable pointer pairs.
fn set_pair(zp: u8, value: u16) -> Vec<u8> {
    let [lo, hi] = value.to_le_bytes();
    vec![0xA9, lo, 0xA0, hi, 0x85, zp, 0x84, zp + 1]
}

fn bltu_ml(src_end: u16, dest_end: u16, entry: u8) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&set_pair(0x5A, src_end));
    v.extend_from_slice(&set_pair(0x58, dest_end));
    v.extend_from_slice(&[0x20, entry, 0xA3]);
    v.push(0x60);
    v
}

/// Place machine code in a REM statement and enter it through SYS.
fn reserved_for_rem_ml(ml: &[u8], pad_to: u16) -> Vec<(u16, u16)> {
    // Line 0 is `REM<ml>`: 4 bytes of header + 1 for the `REM` token, so `ml` begins at $0806.
    let prg = build_prg(&[line(0, &rem(ml)), line(3, &sys(0x0806))], pad_to);
    compile_with_options(&prg, CompileOptions::default())
        .expect("compiles")
        .diagnostics
        .auto_reserved
}

fn covers(reserved: &[(u16, u16)], lo: u16, hi: u16) -> bool {
    (lo..=hi).all(|a| reserved.iter().any(|(s, e)| a >= *s && a <= *e))
}

fn touched(reserved: &[(u16, u16)], addr: u16) -> bool {
    reserved.iter().any(|(s, e)| addr >= *s && addr <= *e)
}

fn compile_ok(prg: &[u8]) -> Compiled {
    compile_with_options(prg, CompileOptions::default()).expect("compiles")
}

#[test]
fn an_upward_rom_block_move_reserves_up_to_its_destination_end() {
    let reserved = reserved_for_rem_ml(&bltu_ml(0x433B, 0x63CB, 0xBF), 0x1000);
    assert!(
        covers(&reserved, 0x2090, 0x63CB),
        "BLTU moves a block ending at $433B so that it ends at $63CB; the \
         written region can reach down to $63CB-$433B = $2090 and nothing \
         reserved it. reserved: {reserved:04X?}"
    );
}

#[test]
fn the_last_write_of_the_destination_pointer_wins() {
    // The later store determines the block-move source pointer.
    let mut ml = set_pair(0x58, 0x0F70);
    ml.extend_from_slice(&bltu_ml(0x433B, 0x63CB, 0xBF));
    let reserved = reserved_for_rem_ml(&ml, 0x1000);
    assert!(
        touched(&reserved, 0x63CB),
        "the second $58/$59 store is the live one; reserved: {reserved:04X?}"
    );
    assert!(
        !touched(&reserved, 0x0F70),
        "the overwritten first store must not create a reservation of its \
         own; reserved: {reserved:04X?}"
    );
}

#[test]
fn the_checked_entry_at_a3b8_counts_too() {
    let reserved = reserved_for_rem_ml(&bltu_ml(0x433B, 0x63CB, 0xB8), 0x1000);
    assert!(
        touched(&reserved, 0x63CB),
        "JSR $A3B8 falls into $A3BF and moves the same block; \
         reserved: {reserved:04X?}"
    );
}

#[test]
fn compiled_code_ends_up_above_the_destination() {
    let prg = build_prg(
        &[line(0, &rem(&bltu_ml(0x433B, 0x63CB, 0xBF))), line(3, &sys(0x0806))],
        0x4342 - 0x0801,
    );
    let c = compile_ok(&prg);
    let end = 0x0801 + c.prg_bytes.len() as u32 - 2;
    assert!(
        end > 0x63CB,
        "the move writes everything up to $63CB; compiled code must not \
         live under it, but the image ends at ${end:04X}"
    );
}

#[test]
fn bytes_that_are_never_executed_reserve_nothing() {
    // Same REM, but the SYS goes to the cassette buffer instead of into the text, so no image is materialised.
    let prg = build_prg(
        &[
            line(0, &rem(&bltu_ml(0x433B, 0x63CB, 0xBF))),
            line(3, &sys(828)),
        ],
        0x1000,
    );
    let reserved = compile_with_options(&prg, CompileOptions::default())
        .expect("compiles")
        .diagnostics
        .auto_reserved;
    assert!(
        !touched(&reserved, 0x63CB) && !touched(&reserved, 0x2090),
        "an un-materialised image is data; `20 BF A3` in it is a byte \
         coincidence and must not reserve 17 KB. reserved: {reserved:04X?}"
    );
}

#[test]
fn a_downward_move_reserves_nothing() {
    let reserved = reserved_for_rem_ml(&bltu_ml(0x63CB, 0x433B, 0xBF), 0x1000);
    assert!(
        !touched(&reserved, 0x433B) && !touched(&reserved, 0x63CB),
        "destEnd <= srcEnd is a compaction, not a relocation; \
         reserved: {reserved:04X?}"
    );
}

/// If the pointers are not both statically known the honest answer is "no idea", not a guessed 40 KB span.
#[test]
fn a_call_with_unknown_pointers_reserves_nothing() {
    // Set the source end, but load the destination pointer from memory (`AD lo hi` = LDA abs) so no immediate is tracked.
    let mut ml = set_pair(0x5A, 0x433B);
    ml.extend_from_slice(&[0xAD, 0x00, 0x30, 0x85, 0x58, 0xAD, 0x01, 0x30, 0x85, 0x59]);
    ml.extend_from_slice(&[0x20, 0xBF, 0xA3, 0x60]);
    let reserved = reserved_for_rem_ml(&ml, 0x1000);
    assert!(
        !touched(&reserved, 0x63CB),
        "operands that are not immediates must be declined, not guessed; \
         reserved: {reserved:04X?}"
    );
}

#[test]
fn a_destination_past_the_window_is_declined() {
    let reserved = reserved_for_rem_ml(&bltu_ml(0x1000, 0xDBE8, 0xBF), 0x1000);
    assert!(
        !touched(&reserved, 0xCFFF),
        "a move ending at $DBE8 is past $CFFF and must not be honoured; \
         reserved: {reserved:04X?}"
    );
}

/// A program with no `BLTU` at all must be untouched ; the trivially necessary control, and the one that catches a matcher that fires on any `JSR`.
#[test]
fn a_program_without_a_block_move_is_unchanged() {
    // Printing a control byte does not perform a block move.
    let reserved = reserved_for_rem_ml(&[0xA9, 0x08, 0x20, 0xD2, 0xFF, 0x60], 0x1000);
    assert!(
        !touched(&reserved, 0x63CB) && !touched(&reserved, 0x2090),
        "no BLTU, no reservation; reserved: {reserved:04X?}"
    );
}

#[test]
fn no_auto_reserve_switches_it_off() {
    let prg = build_prg(
        &[line(0, &rem(&bltu_ml(0x433B, 0x63CB, 0xBF))), line(3, &sys(0x0806))],
        0x1000,
    );
    let reserved = compile_with_options(
        &prg,
        CompileOptions {
            auto_reserve: false,
            ..CompileOptions::default()
        },
    )
    .expect("compiles")
    .diagnostics
    .auto_reserved;
    assert!(
        reserved.is_empty(),
        "--no-auto-reserve must suppress this too; reserved: {reserved:04X?}"
    );
}

#[test]
fn a_reservation_the_code_never_reaches_costs_no_bytes() {
    let mk = |ml: &[u8]| {
        build_prg(
            &[line(0, &rem(ml)), line(3, &sys(0x0806))],
            0x4000 - 0x0801,
        )
    };

    let with = compile_ok(&mk(&bltu_ml(0x1000, 0xB000, 0xBF)));
    let without = compile_ok(&mk(&[0xA9, 0x08, 0x20, 0xD2, 0xFF, 0x60]));

    let reserved = compile_with_options(&mk(&bltu_ml(0x1000, 0xB000, 0xBF)), CompileOptions::default())
        .expect("compiles")
        .diagnostics
        .auto_reserved;
    assert!(
        touched(&reserved, 0xB000),
        "the reservation should still be derived; reserved: {reserved:04X?}"
    );
    let delta = with.prg_bytes.len() as i64 - without.prg_bytes.len() as i64;
    assert!(
        delta.abs() < 0x400,
        "a reservation the code never reaches must not inflate the image; \
         with={} without={} delta={delta}",
        with.prg_bytes.len(),
        without.prg_bytes.len()
    );
}
