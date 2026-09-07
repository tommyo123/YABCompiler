//! `--reserved` over a program that hands its own image back verbatim.

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

fn build_prg(lines: &[Line]) -> Vec<u8> {
    let load = 0x0801u16;
    let mut addr = load;
    let mut starts = Vec::new();
    for l in lines {
        starts.push(addr);
        addr += 5 + l.body.len() as u16;
    }
    let mut out = load.to_le_bytes().to_vec();
    for (i, l) in lines.iter().enumerate() {
        let next = starts.get(i + 1).copied().unwrap_or(addr);
        out.extend_from_slice(&next.to_le_bytes());
        out.extend_from_slice(&l.number.to_le_bytes());
        out.extend_from_slice(&l.body);
        out.push(0);
    }
    out.extend_from_slice(&[0, 0]);
    out
}

/// `10 REM<RTS>` / `20 SYS 2054` / `30 PRINT"OK"` plus padding, so the image runs well past `$0900`.
fn sys_into_image_program() -> Vec<u8> {
    let mut lines = vec![
        line(10, b"\x8f\x60"),
        line(20, b"\x9e2054"),
        line(30, b"\x99\"OK\""),
    ];
    // Padding REMs.
    for i in 0..9u16 {
        lines.push(line(
            100 + i,
            b"\x8fPADDINGPADDINGPADDINGPADDINGPADDINGPADDINGPADDINGPADDINGPADD",
        ));
    }
    build_prg(&lines)
}

fn no_window_program() -> Vec<u8> {
    let mut lines = Vec::new();
    for i in 0..13u16 {
        lines.push(line(10 + i, b"\x99\"OK\""));
    }
    build_prg(&lines)
}

fn range_inside_code(c: &Compiled) -> (u16, u16) {
    let d = &c.diagnostics;
    let code_lo = match d.original_image {
        Some((_, hi)) => hi + 8,
        // Stock builds put code at $080D, past the `0 SYS2061` stub.
        None => d.start_address + 0x18,
    };
    let range = (code_lo, code_lo + 0x10);
    assert!(
        range.1 + 8 < d.end_address,
        "fixture too small: reserving ${:04X}-${:04X} in an image ending at ${:04X} would \
         not make the assembler bridge anything",
        range.0,
        range.1,
        d.end_address
    );
    range
}

fn compile_reserving(prg: &[u8], ranges: &[(u16, u16)]) -> Compiled {
    let mut o = CompileOptions::default();
    o.reserved_ranges = ranges.to_vec();
    compile_with_options(prg, o).expect("compiles")
}

fn window(c: &Compiled) -> (u16, u16) {
    c.diagnostics
        .original_image
        .expect("this program must take the in-place layout, or the test proves nothing")
}

fn petscii(bytes: &[u8]) -> String {
    let mut s = String::new();
    for &b in bytes {
        match b {
            0x0d => s.push('\n'),
            0x20..=0x5a => s.push(b as char),
            0xc1..=0xda => s.push((b - 0x80) as char),
            _ => {}
        }
    }
    s
}

fn run(c: &Compiled) -> String {
    let r = emu64::run_prg_to_end(&c.prg_bytes, 40_000_000).expect("runs");
    assert!(r.clean_exit, "program must reach its final RTS");
    petscii(&r.output)
}

/// The range every "inside" test reserves, and the one every "outside" test reserves.
const INSIDE: (u16, u16) = (0x0900, 0x0910);

#[test]
fn a_reservation_inside_the_window_is_dropped_not_refused() {
    let prg = sys_into_image_program();
    let c = compile_reserving(&prg, &[INSIDE]);
    let (lo, hi) = window(&c);
    assert!(
        lo <= INSIDE.0 && INSIDE.1 <= hi,
        "the fixture must actually put ${:04X}-${:04X} inside ${lo:04X}-${hi:04X}",
        INSIDE.0,
        INSIDE.1
    );
    assert_eq!(
        c.diagnostics.manual_reserved_clipped,
        vec![INSIDE],
        "the drop has to be reported, not silent"
    );
    assert!(
        c.diagnostics.manual_reserved.is_empty(),
        "…and nothing of it may reach the assembler"
    );
    assert!(
        !c.diagnostics.effective_reserved.iter().any(|&(s, e)| {
            // No effective range may overlap the request.
            s <= INSIDE.1 && INSIDE.0 <= e
        }),
        "effective_reserved was {:?}",
        c.diagnostics.effective_reserved
    );
}

#[test]
fn dropping_it_leaves_the_image_byte_for_byte_unreserved() {

    let prg = sys_into_image_program();
    let plain = compile_reserving(&prg, &[]);
    let asked = compile_reserving(&prg, &[INSIDE]);
    assert_eq!(
        plain.prg_bytes, asked.prg_bytes,
        "a dropped reservation must not move a single byte"
    );
}

#[test]
fn the_dropped_build_still_runs_on_the_rom() {
    // ROM adjudication.
    let prg = sys_into_image_program();
    let c = compile_reserving(&prg, &[INSIDE]);
    let out = run(&c);
    let got: Vec<&str> = out.split_whitespace().collect();
    assert_eq!(got, vec!["OK"], "full output was {out:?}");
}

#[test]
fn a_reservation_above_the_window_is_still_honoured() {

    let prg = sys_into_image_program();
    let plain = compile_reserving(&prg, &[]);
    let outside = range_inside_code(&plain);
    let hi = window(&plain).1;
    assert!(outside.0 > hi + 4, "the fixture must clear the bridge cushion");
    let asked = compile_reserving(&prg, &[outside]);
    assert!(
        asked.diagnostics.manual_reserved_clipped.is_empty(),
        "nothing was inside the window, so nothing may be reported dropped"
    );
    assert_eq!(asked.diagnostics.manual_reserved, vec![outside]);
    assert!(
        asked.diagnostics.effective_reserved.contains(&outside),
        "effective_reserved was {:?}",
        asked.diagnostics.effective_reserved
    );
    assert_ne!(
        plain.prg_bytes, asked.prg_bytes,
        "an honoured reservation has to change the layout"
    );

    let out = run(&asked);
    assert_eq!(out.split_whitespace().collect::<Vec<_>>(), vec!["OK"]);
}

#[test]
fn an_honoured_reservation_really_holds_no_code() {

    let prg = sys_into_image_program();
    let plain = compile_reserving(&prg, &[]);
    let outside = range_inside_code(&plain);
    let asked = compile_reserving(&prg, &[outside]);
    // The file's first body byte sits at the load address, so an address maps to an offset by subtracting it.
    let base = asked.diagnostics.start_address as usize;
    let body = &asked.prg_bytes[2..];
    let from = outside.0 as usize - base;
    let to = outside.1 as usize - base;
    assert!(
        to < body.len(),
        "the fixture must reserve inside the image, not past its end"
    );

    assert!(
        body[from..=to].iter().all(|&b| b == 0x00),
        "reserved bytes ${:04X}-${:04X} were {:02X?}",
        outside.0,
        outside.1,
        &body[from..=to]
    );

    let plain_body = &plain.prg_bytes[2..];
    assert!(
        plain_body[from..=to].iter().any(|&b| b != 0x00),
        "the unreserved image is already blank there; the test proves nothing"
    );
}

#[test]
fn a_reservation_straddling_the_top_keeps_its_upper_half() {
    // Clip, not discard.
    let prg = sys_into_image_program();
    let plain = compile_reserving(&prg, &[]);
    let (_, hi) = window(&plain);
    let straddle = (hi - 0x10, hi + 0x80);
    let asked = compile_reserving(&prg, &[straddle]);
    // BRIDGE_ROOM is 4, so the kept part resumes at hi + 4 and the reported loss runs to hi + 3.
    assert_eq!(
        asked.diagnostics.manual_reserved_clipped,
        vec![(straddle.0, hi + 3)]
    );
    assert_eq!(asked.diagnostics.manual_reserved, vec![(hi + 4, straddle.1)]);
    assert_ne!(
        plain.prg_bytes, asked.prg_bytes,
        "the surviving half still has to be honoured"
    );
    let out = run(&asked);
    assert_eq!(out.split_whitespace().collect::<Vec<_>>(), vec!["OK"]);
}

#[test]
fn a_program_without_a_window_never_clips_anything() {

    let prg = no_window_program();
    let plain = compile_reserving(&prg, &[]);
    assert!(
        plain.diagnostics.original_image.is_none(),
        "the fixture must NOT be materialised in place, or it proves nothing"
    );
    let asked_for = range_inside_code(&plain);
    let asked = compile_reserving(&prg, &[asked_for]);
    assert!(
        asked.diagnostics.manual_reserved_clipped.is_empty(),
        "no window means nothing to clip against"
    );
    assert_eq!(asked.diagnostics.manual_reserved, vec![asked_for]);
    assert!(asked.diagnostics.effective_reserved.contains(&asked_for));
    assert_ne!(plain.prg_bytes, asked.prg_bytes);
}

#[test]
fn an_auto_discovered_range_inside_the_window_is_dropped_too() {

    let mut lines = vec![
        line(10, b"\x8f\x60"),
        line(20, b"\x9e2054"),
        // POKE 2400,0 -- POKE is $97.
        line(30, b"\x972400,0"),
        line(40, b"\x99\"OK\""),
    ];
    for i in 0..9u16 {
        lines.push(line(
            100 + i,
            b"\x8fPADDINGPADDINGPADDINGPADDINGPADDINGPADDINGPADDINGPADDINGPADD",
        ));
    }
    let c = compile_reserving(&build_prg(&lines), &[]);
    let (lo, hi) = window(&c);
    assert!(
        lo <= 2400 && 2400 <= hi,
        "the POKE address must be inside ${lo:04X}-${hi:04X} or the test proves nothing"
    );
    assert!(
        !c.diagnostics
            .effective_reserved
            .iter()
            .any(|&(s, e)| s <= 2400 && 2400 <= e),
        "auto-reserve leaked $0960 into the blob: {:?}",
        c.diagnostics.effective_reserved
    );
    assert!(
        c.diagnostics.manual_reserved_clipped.is_empty(),
        "nothing was asked for manually, so nothing may be reported dropped"
    );
}
