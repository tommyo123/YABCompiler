use yabcompiler_core::compile::{LowCut, low_trim_facts};
use yabcompiler_core::{CompileOptions, Compiled, compile_with_options};

const LOAD: u16 = 0x0801;
/// `compile::INPLACE_HEAD` ; the `SYS` stub plus the `JMP` over the blob.
const HEAD: u16 = 15;

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

fn build_prg(lines: &[Line], tail: &[u8]) -> (Vec<u8>, u16) {
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
    let text_end = addr;
    out.extend_from_slice(tail);
    (out, text_end)
}

fn sys(n: u16) -> Vec<u8> {
    let mut v = vec![0x9E];
    v.extend_from_slice(n.to_string().as_bytes());
    v
}

fn filler_line(n: u16) -> Line {
    let mut b = vec![0x8Fu8];
    b.extend(std::iter::repeat_n(b'X', 64));
    line(n, &b)
}

fn program(n: u16, last: &[u8], pad: u16, ml: &[u8]) -> Prog {
    let mut entry = 0u16;
    for _ in 0..4 {
        let mut lines: Vec<Line> = (0..n).map(|i| filler_line(10 + i)).collect();
        let mut body = last.to_vec();
        if entry != 0 {
            if !body.is_empty() {
                body.push(b':');
            }
            body.extend_from_slice(&sys(entry));
        }
        lines.push(line(10 + n, &body));
        let mut tail = vec![0u8; usize::from(pad)];
        tail.extend_from_slice(ml);
        let (prg, text_end) = build_prg(&lines, &tail);
        let want = text_end + 2 + pad;
        if want == entry {
            return Prog {
                prg,
                entry,
                payload_at: text_end + 2,
                payload: {
                    let mut t = vec![0u8; usize::from(pad)];
                    t.extend_from_slice(ml);
                    t
                },
            };
        }
        entry = want;
    }
    panic!("layout did not settle");
}

struct Prog {
    prg: Vec<u8>,
    entry: u16,
    payload_at: u16,
    payload: Vec<u8>,
}

impl Prog {
    fn decision(&self) -> LowCut {
        low_trim_facts(&self.prg, &CompileOptions::default())
            .expect("materialises its image")
            .decision
    }

    fn cut(&self) -> u16 {
        match self.decision() {
            LowCut::Take(n) => LOAD + n as u16,
            LowCut::Refused(why) => panic!("expected a low trim, refused: {why}"),
        }
    }

    /// The default build ; low-trimmed whenever the guards allow it.
    fn build(&self) -> Compiled {
        compile_with_options(&self.prg, CompileOptions::default()).expect("builds")
    }

    fn control(&self) -> Compiled {
        let mut o = CompileOptions::default();
        o.reserved_ranges = vec![(LOAD + HEAD + 1, LOAD + HEAD + 3)];
        compile_with_options(&self.prg, o).expect("builds")
    }
}

fn at(c: &Compiled, addr: u16, len: usize) -> &[u8] {
    let off = 2 + usize::from(addr - LOAD);
    &c.prg_bytes[off..off + len]
}

/// The original file's bytes at `addr`.
fn orig(p: &Prog, addr: u16, len: usize) -> &[u8] {
    let off = 2 + usize::from(addr - LOAD);
    &p.prg[off..off + len]
}

fn ml_prints() -> Vec<u8> {
    vec![0xA9, 0x41, 0x20, 0xD2, 0xFF, 0x60]
}

/// Enough dead text to clear `MATERIALISE_MIN_SAVING` (2,048) plus a page of `MATERIALISE_SLACK` and the launch head, with room to spare.
const FILLERS: u16 = 60;

#[test]
fn the_control_really_is_the_untrimmed_build() {
    let p = program(FILLERS, b"", 0x40, &ml_prints());
    assert!(matches!(p.decision(), LowCut::Take(_)));
    let ctl = p.control();

    let body = &p.prg[2..];
    assert_eq!(
        at(&ctl, LOAD + HEAD, body.len() - usize::from(HEAD)),
        &body[usize::from(HEAD)..],
        "the control must embed the image whole",
    );

    assert!(
        ctl.diagnostics.effective_reserved.is_empty(),
        "the control's reservation must be clipped inside the in-place window, got {:?}",
        ctl.diagnostics.effective_reserved,
    );
}

#[test]
fn a_low_trim_drops_the_end_address_and_the_file() {
    let p = program(FILLERS, b"", 0x40, &ml_prints());
    let low = p.build();
    let ctl = p.control();
    assert!(
        low.diagnostics.end_address < ctl.diagnostics.end_address,
        "end address {:#06X} must drop below the untrimmed {:#06X} — a low trim that only \
         shrinks the file has not touched the address space",
        low.diagnostics.end_address,
        ctl.diagnostics.end_address,
    );
    assert!(
        low.prg_bytes.len() < ctl.prg_bytes.len(),
        "file {} must be smaller than the untrimmed {}",
        low.prg_bytes.len(),
        ctl.prg_bytes.len(),
    );
}

#[test]
fn the_payload_survives_byte_for_byte_at_its_own_address() {
    let p = program(FILLERS, b"", 0x40, &ml_prints());
    let low = p.build();
    assert_eq!(
        at(&low, p.payload_at, p.payload.len()),
        &p.payload[..],
        "the payload has to be exactly where the SYS aims",
    );
    // The tail is not merely present, it starts at the cut: everything from the cut up is the original image untouched.
    let cut = p.cut();
    let body = &p.prg[2..];
    let from = usize::from(cut - LOAD);
    assert_eq!(
        at(&low, cut, body.len() - from),
        &body[from..],
        "every byte from the cut upward is the original image",
    );

    assert_ne!(
        at(&low, LOAD + HEAD, from - usize::from(HEAD)),
        orig(&p, LOAD + HEAD, from - usize::from(HEAD)),
        "the build fell back to the untrimmed image; the assertions above prove nothing",
    );
}

#[test]
fn compiled_code_really_occupies_the_freed_range() {
    let p = program(FILLERS, b"", 0x40, &ml_prints());
    let low = p.build();
    let cut = p.cut();
    let freed = usize::from(cut - (LOAD + HEAD));
    assert!(freed >= 2048, "the fixture must clear the minimum saving");

    assert_ne!(
        at(&low, LOAD + HEAD, freed),
        orig(&p, LOAD + HEAD, freed),
        "the dead text must not still be embedded",
    );

    assert!(
        at(&low, LOAD + HEAD, freed).iter().any(|b| *b != 0),
        "the freed span must hold compiled code, not a hole",
    );
}

#[test]
fn code_larger_than_the_hole_is_bridged_over_the_payload() {

    let mut last: Vec<u8> = Vec::new();
    for i in 0..40 {
        if i > 0 {
            last.push(b':');
        }
        // PRINT"<40 chars>"
        last.push(0x99);
        last.push(b'"');
        last.extend(std::iter::repeat_n(b'A' + (i % 26) as u8, 40));
        last.push(b'"');
    }
    let p = program(FILLERS, &last, 0x40, &ml_prints());
    let low = p.build();
    let ctl = p.control();
    let cut = p.cut();
    assert!(
        low.diagnostics.end_address > p.payload_at,
        "this fixture is only interesting if the code runs past the payload",
    );
    assert!(low.diagnostics.end_address < ctl.diagnostics.end_address);
    // The payload is still verbatim, with compiled code on both sides.
    assert_eq!(at(&low, p.payload_at, p.payload.len()), &p.payload[..]);
    let freed = usize::from(cut - (LOAD + HEAD));
    assert!(at(&low, LOAD + HEAD, freed).iter().any(|b| *b != 0));
}

#[test]
fn an_entry_low_in_the_text_is_left_alone() {
    // The entry sits in the second line, so there is nothing dead underneath it and the mirror operation has no work to do.
    let mut lines: Vec<Line> = vec![line(10, &sys(LOAD + 0x30))];
    lines.extend((0..FILLERS).map(|i| filler_line(20 + i)));
    let mut tail = vec![0u8; 0x40];
    tail.extend_from_slice(&ml_prints());
    let (prg, _) = build_prg(&lines, &tail);
    let f = low_trim_facts(&prg, &CompileOptions::default()).expect("materialises");
    assert_eq!(
        f.decision.reason(),
        Some("slack"),
        "an entry a page above the load address leaves nothing under it to cut",
    );

    let built = compile_with_options(&prg, CompileOptions::default()).expect("builds");
    let body = &prg[2..];
    assert_eq!(
        at(&built, LOAD + HEAD, body.len() - usize::from(HEAD)),
        &body[usize::from(HEAD)..],
    );
}

#[test]
fn a_file_with_no_payload_is_left_to_the_prefix_trim() {
    // An image without appended data permits trimming unused trailing BASIC text.
    let mut lines: Vec<Line> = vec![line(10, &sys(LOAD + 0x30))];
    lines.extend((0..FILLERS).map(|i| filler_line(20 + i)));
    let (prg, _) = build_prg(&lines, &[]);
    let f = low_trim_facts(&prg, &CompileOptions::default()).expect("materialises");
    assert!(!f.has_appended_payload);
    assert_eq!(f.decision.reason(), Some("prefix-family"));
}

#[test]
fn a_program_that_walks_its_own_text_is_never_low_trimmed() {

    let last = vec![0x99, 0xC2, b'(', b'4', b'4', b')'];
    let p = program(FILLERS, &last, 0x40, &ml_prints());
    let f = low_trim_facts(&p.prg, &CompileOptions::default()).expect("materialises");
    assert!(f.reads_txttab, "the fixture must actually read TXTTAB");
    assert!(
        f.structural_dead_bytes >= 2048,
        "the fixture must have something to lose, got {}",
        f.structural_dead_bytes,
    );
    assert_eq!(f.decision.reason(), Some("txttab"));
}

#[test]
fn a_poke_into_the_dead_text_refuses_the_trim() {

    let target = LOAD + 0x200;
    let mut last = vec![0x97u8];
    last.extend_from_slice(target.to_string().as_bytes());
    last.push(b',');
    last.push(b'0');
    let poke_at = |target: u16| {
        let mut last = vec![0x97u8];
        last.extend_from_slice(target.to_string().as_bytes());
        last.push(b',');
        last.push(b'0');
        let p = program(FILLERS, &last, 0x40, &ml_prints());
        let f = low_trim_facts(&p.prg, &CompileOptions::default()).expect("materialises");
        assert!(
            f.peek_poke_ranges.iter().any(|(lo, _)| *lo == target),
            "the fixture must produce the range, got {:?}",
            f.peek_poke_ranges,
        );
        f.decision
    };

    let target = LOAD + 0x0C00;
    match poke_at(target) {
        LowCut::Take(n) => assert!(
            LOAD + n as u16 <= target,
            "cut {:#06X} must not rise above the POKE at {target:#06X}",
            LOAD + n as u16,
        ),
        LowCut::Refused(why) => panic!("a POKE with room underneath should still trim: {why}"),
    }

    assert_eq!(poke_at(LOAD + 0x0200).reason(), Some("min-saving"));
}

#[test]
fn a_poke_above_the_cut_is_not_a_reason_to_refuse() {

    let p0 = program(FILLERS, b"", 0x40, &ml_prints());
    let above = p0.payload_at + 2;
    let mut last = vec![0x97u8];
    last.extend_from_slice(above.to_string().as_bytes());
    last.push(b',');
    last.push(b'0');
    let p = program(FILLERS, &last, 0x40, &ml_prints());
    let f = low_trim_facts(&p.prg, &CompileOptions::default()).expect("materialises");
    assert!(
        f.peek_poke_ranges.iter().any(|(lo, _)| *lo >= p.payload_at),
        "the fixture must POKE inside the image above the cut, got {:?}",
        f.peek_poke_ranges,
    );
    assert!(
        matches!(f.decision, LowCut::Take(_)),
        "a POKE above the deleted span is not a reason: {:?}",
        f.decision,
    );
}

#[test]
fn ml_that_reads_below_the_cut_refuses_the_trim() {

    let p = program(FILLERS, b"", 0x40, &[0xAD, 0x50, 0x08, 0x60]);
    let f = low_trim_facts(&p.prg, &CompileOptions::default()).expect("materialises");
    assert_eq!(f.reachable_low, Some(0x0850));
    assert!(
        f.structural_dead_bytes >= 2048,
        "the fixture must have something to lose",
    );
    assert_eq!(f.decision.reason(), Some("scan-reaches-down"));
}

#[test]
fn ml_that_reads_above_the_cut_is_not_a_reason_to_refuse() {

    let p0 = program(FILLERS, b"", 0x40, &[0xAD, 0x00, 0x00, 0x60]);
    let hi = p0.payload_at;
    let ml = [0xAD, (hi & 0xFF) as u8, (hi >> 8) as u8, 0x60];
    let p = program(FILLERS, b"", 0x40, &ml);
    let f = low_trim_facts(&p.prg, &CompileOptions::default()).expect("materialises");
    assert_eq!(f.reachable_low, Some(p.entry.min(hi)));
    assert!(matches!(f.decision, LowCut::Take(_)), "{:?}", f.decision);
}

#[test]
fn a_bss_tail_below_the_payload_does_not_take_the_payload_with_it() {

    let last = vec![0x86, b'A', b'(', b'2', b'0', b'0', b')'];
    let p = program( 80, &last, 0x40, &ml_prints());
    let low = p.build();
    let cut = p.cut();
    let from = usize::from(cut - LOAD);

    let image_last = LOAD + (p.prg.len() - 2) as u16 - 1;
    assert_eq!(
        low.diagnostics.end_address, image_last,
        "fixture must have all its code below the payload",
    );
    assert_eq!(
        at(&low, p.payload_at, p.payload.len()),
        &p.payload[..],
        "the payload must survive the BSS trim",
    );
    assert_ne!(
        at(&low, LOAD + HEAD, from - usize::from(HEAD)),
        orig(&p, LOAD + HEAD, from - usize::from(HEAD)),
        "the build fell back to the untrimmed image; this proves nothing",
    );
}

#[test]
fn ml_the_scan_cannot_follow_refuses_the_trim() {
    // `LDA ($FB),Y : RTS`.
    let p = program(FILLERS, b"", 0x40, &[0xB1, 0xFB, 0x60]);
    let f = low_trim_facts(&p.prg, &CompileOptions::default()).expect("materialises");
    assert_eq!(f.reachable_low, None);
    assert!(
        f.structural_dead_bytes >= 2048,
        "the fixture must have something to lose",
    );
    assert_eq!(f.decision.reason(), Some("scan-refused"));
}

#[test]
fn a_broken_link_chain_never_cuts_above_the_break() {

    let mut p = program(FILLERS, b"", 0x40, &ml_prints());
    let whole = match p.decision() {
        LowCut::Take(n) => n,
        LowCut::Refused(why) => panic!("control must be trimmable, refused: {why}"),
    };
    // Walk to the 30th line start, then point its link backwards.
    let mut addr = LOAD;
    for _ in 0..30 {
        let off = 2 + usize::from(addr - LOAD);
        addr = u16::from_le_bytes([p.prg[off], p.prg[off + 1]]);
    }
    let break_at = addr;
    let off = 2 + usize::from(break_at - LOAD);
    p.prg[off] = (LOAD & 0xFF) as u8;
    p.prg[off + 1] = (LOAD >> 8) as u8;

    let f = low_trim_facts(&p.prg, &CompileOptions::default()).expect("materialises");
    match f.decision {
        LowCut::Refused(_) => {}
        LowCut::Take(n) => assert!(
            LOAD + n as u16 <= break_at,
            "cut {:#06X} is above the broken link at {break_at:#06X}",
            LOAD + n as u16,
        ),
    }

    assert!(
        f.decision.taken_or(0) < whole,
        "the break must reduce the cut from the control's {whole}",
    );
}

trait TakenOr {
    fn taken_or(self, d: usize) -> usize;
}
impl TakenOr for LowCut {
    fn taken_or(self, d: usize) -> usize {
        match self {
            LowCut::Take(n) => n,
            LowCut::Refused(_) => d,
        }
    }
}
