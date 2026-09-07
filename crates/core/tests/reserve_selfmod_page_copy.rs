use yabcompiler_core::{CompileOptions, Compiled, compile_with_options};

const LOAD: u16 = 0x0801;

/// `SYS <n>`, tokenised.
fn sys(n: u16) -> Vec<u8> {
    let mut v = vec![0x9E];
    v.extend_from_slice(n.to_string().as_bytes());
    v
}

/// One BASIC line `10 SYS <entry>`, then `$AA` filler up to `payload_at`, then `payload`, then filler out to `end_at`.
fn prg_padded(entry: u16, payload_at: u16, payload: &[u8], end_at: u16) -> Vec<u8> {
    let body = sys(entry);
    let mut out = LOAD.to_le_bytes().to_vec();
    let next = LOAD + 5 + body.len() as u16;
    out.extend_from_slice(&next.to_le_bytes());
    out.extend_from_slice(&10u16.to_le_bytes());
    out.extend_from_slice(&body);
    out.push(0);
    out.extend_from_slice(&[0, 0]); // end of the link chain
    let want = (payload_at - LOAD) as usize + 2;
    assert!(out.len() <= want, "payload address is inside the BASIC text");
    while out.len() < want {
        out.push(0xAA);
    }
    out.extend_from_slice(payload);
    while out.len() < (end_at - LOAD) as usize + 2 {
        out.push(0xAA);
    }
    out
}

/// The common case: a little filler past the payload so the image does not end exactly on it.
fn prg_with_payload(entry: u16, payload_at: u16, payload: &[u8]) -> Vec<u8> {
    let end = payload_at + payload.len() as u16 + 0x40;
    prg_padded(entry, payload_at, payload, end)
}

/// Parameters of a runtime-modified page-copy loop.
struct Kernel {
    at: u16,
    src: u16,
    dst: u16,
    pages: u8,
    /// `0xEE` (up) or `0xCE` (down) ; emitted for both steps.
    step: u8,
    /// Emit the destination step first.
    dst_step_first: bool,
    /// Displacement of the closing `BNE`.
    back: i8,
}

impl Kernel {
    fn new(at: u16, src: u16, dst: u16, pages: u8) -> Self {
        Kernel {
            at,
            src,
            dst,
            pages,
            step: 0xEE,
            dst_step_first: false,
            back: -18,
        }
    }
    fn bytes(&self) -> Vec<u8> {

        let head = self.at + 4;
        let [sl, sh] = self.src.to_le_bytes();
        let [dl, dh] = self.dst.to_le_bytes();
        let src_hi_at = (head + 2).to_le_bytes();
        let dst_hi_at = (head + 5).to_le_bytes();
        let (first, second) = if self.dst_step_first {
            (dst_hi_at, src_hi_at)
        } else {
            (src_hi_at, dst_hi_at)
        };
        let mut v = vec![0xA2, self.pages, 0xA0, 0x00];
        v.extend_from_slice(&[0xB9, sl, sh]);
        v.extend_from_slice(&[0x99, dl, dh]);
        v.extend_from_slice(&[0xC8, 0xD0, 0xF7]);
        v.extend_from_slice(&[self.step, first[0], first[1]]);
        v.extend_from_slice(&[self.step, second[0], second[1]]);
        v.extend_from_slice(&[0xCA, 0xD0, self.back as u8]);
        v.push(0x60);
        v
    }
}

/// Copy eight pages from $1300 to $2800 using a routine at $12E7.
fn eight_page_copy() -> Kernel {
    Kernel::new(0x12E7, 0x1300, 0x2800, 8)
}

fn reserved_for(k: &Kernel) -> Vec<(u16, u16)> {
    reserved_for_prg(&prg_with_payload(k.at, k.at, &k.bytes()))
}

fn reserved_for_prg(prg: &[u8]) -> Vec<(u16, u16)> {
    compile_with_options(prg, CompileOptions::default())
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
fn two_kilobyte_copy_reserves_its_whole_destination() {
    let reserved = reserved_for(&eight_page_copy());
    assert!(
        covers(&reserved, 0x2800, 0x2FFF),
        "SYS 4839 copies eight pages to $2800; every byte of $2800-$2FFF \
         must be reserved. reserved: {reserved:04X?}"
    );
}

#[test]
fn the_page_count_comes_from_the_ldx_immediate() {
    let three = reserved_for(&Kernel::new(0x12E7, 0x1300, 0x2800, 3));
    assert!(
        covers(&three, 0x2800, 0x2AFF),
        "three pages is $2800-$2AFF; reserved: {three:04X?}"
    );
    assert!(
        !touched(&three, 0x2B00),
        "three pages must not reserve a fourth; reserved: {three:04X?}"
    );
}

#[test]
fn the_descending_form_walks_the_destination_down() {
    let mut k = Kernel::new(0x12E7, 0x1300, 0x2800, 4);
    k.step = 0xCE;
    let reserved = reserved_for(&k);
    assert!(
        covers(&reserved, 0x2500, 0x28FF),
        "a descending four-page copy whose pointer starts at $2800 fills \
         $2500-$28FF; reserved: {reserved:04X?}"
    );
}

/// Accept either order of the source and destination pointer increments.
#[test]
fn the_two_steps_may_be_written_in_either_order() {
    let mut k = eight_page_copy();
    k.dst_step_first = true;
    assert!(
        covers(&reserved_for(&k), 0x2800, 0x2FFF),
        "INCing the destination operand first is the same loop"
    );
}

#[test]
fn compiled_code_is_pushed_clear_of_the_destination() {
    let k = eight_page_copy();
    let with = compile_ok(&prg_padded(k.at, k.at, &k.bytes(), 0x27E0));
    assert!(
        covers(&with.diagnostics.effective_reserved, 0x2800, 0x2FFF),
        "the reservation must reach the assembler, not just the \
         diagnostics: {:04X?}",
        with.diagnostics.effective_reserved
    );
    assert!(
        with.diagnostics.end_address > 0x2FFF,
        "code must be pushed above the copy destination, but it ends at \
         ${:04X}",
        with.diagnostics.end_address
    );

    let without = compile_ok(&prg_padded(
        k.at,
        k.at,
        &[0xA9, 0x08, 0x20, 0xE4, 0xFF, 0x60],
        0x27E0,
    ));
    assert!(
        without.diagnostics.end_address <= 0x2FFF,
        "without the copy loop the code must stay where it was; it ends \
         at ${:04X}",
        without.diagnostics.end_address
    );
}

#[test]
fn two_instructions_are_not_a_page_copy() {
    // `LDA $1300,Y : STA $2800,Y : RTS` ; the copy's two instructions with no loop around them at all.
    let ml = [0xB9, 0x00, 0x13, 0x99, 0x00, 0x28, 0x60];
    let reserved = reserved_for_prg(&prg_with_payload(0x12E7, 0x12E7, &ml));
    assert!(
        covers(&reserved, 0x2800, 0x28FF),
        "the single page the store itself reaches is the older rule's, \
         and must survive; reserved: {reserved:04X?}"
    );
    assert!(
        !touched(&reserved, 0x2900),
        "with no loop there is no second page; reserved: {reserved:04X?}"
    );
}

/// The whole kernel, byte for byte, in a program that never `SYS`es into its image.
#[test]
fn bytes_that_are_never_executed_reserve_nothing() {
    let k = eight_page_copy();
    // Same payload, but the SYS goes to the cassette buffer, so no image is materialised.
    let prg = prg_with_payload(828, k.at, &k.bytes());
    let reserved = reserved_for_prg(&prg);
    assert!(
        !touched(&reserved, 0x2800) && !touched(&reserved, 0x2FFF),
        "an un-materialised image is data, not code; \
         reserved: {reserved:04X?}"
    );
}

#[test]
fn inc_operands_that_miss_the_high_bytes_are_declined() {
    let k = eight_page_copy();
    let mut ml = k.bytes();
    // The two step operands are at offsets 13..15 and 16..18 of `ml` (four bytes of LDX/LDY, then the 9-byte head).
    let low = (k.at + 4 + 1).to_le_bytes();
    ml[14] = low[0];
    ml[15] = low[1];
    let reserved = reserved_for_prg(&prg_with_payload(k.at, k.at, &ml));
    assert!(
        !touched(&reserved, 0x2FFF),
        "a step that does not walk the operand high byte is a different \
         loop and must be declined; reserved: {reserved:04X?}"
    );
}

/// One `INC` and one `DEC` is not a page copy.
#[test]
fn a_mismatched_step_pair_is_declined() {
    let k = eight_page_copy();
    let mut ml = k.bytes();
    ml[16] = 0xCE;
    let reserved = reserved_for_prg(&prg_with_payload(k.at, k.at, &ml));
    assert!(
        !touched(&reserved, 0x2FFF),
        "INC one pointer, DEC the other is not a copy; \
         reserved: {reserved:04X?}"
    );
}

/// The outer branch has to close the loop back onto the copy head.
#[test]
fn an_outer_branch_that_does_not_close_the_loop_is_declined() {
    let mut k = eight_page_copy();
    k.back = -60;
    let reserved = reserved_for(&k);
    assert!(
        !touched(&reserved, 0x2FFF),
        "the page count only bounds a loop that repeats the copy; \
         reserved: {reserved:04X?}"
    );
}

/// No literal `LDX`, no page count, no reservation. (`LDX $1234,Y` in place of `LDX #n`.)
#[test]
fn a_page_count_that_is_not_a_literal_is_declined() {
    let k = eight_page_copy();
    let mut ml = k.bytes();
    ml.splice(0..2, [0xBE, 0x34, 0x12]); // LDX $1234,Y The head has moved one byte up, so re-aim the step operands.
    let head = k.at + 5;
    let s = (head + 2).to_le_bytes();
    let d = (head + 5).to_le_bytes();
    ml[15] = s[0];
    ml[16] = s[1];
    ml[18] = d[0];
    ml[19] = d[1];
    let reserved = reserved_for_prg(&prg_with_payload(k.at, k.at, &ml));
    assert!(
        !touched(&reserved, 0x2FFF),
        "a trip count that is not derivable is unbounded, and an \
         unbounded reservation is the failure this avoids; \
         reserved: {reserved:04X?}"
    );
}

#[test]
fn a_destination_running_past_the_window_is_declined_not_clamped() {
    let reserved = reserved_for(&Kernel::new(0x12E7, 0x1300, 0xCE00, 8));
    assert!(
        !touched(&reserved, 0xCF00),
        "eight pages from $CE00 runs to $D5FF, past $CFFF; the range must \
         be dropped whole rather than clamped; reserved: {reserved:04X?}"
    );
}

#[test]
fn no_auto_reserve_switches_it_off() {
    let k = eight_page_copy();
    let prg = prg_with_payload(k.at, k.at, &k.bytes());
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
