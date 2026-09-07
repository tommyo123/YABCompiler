//! `compile::low_trim_facts` ; the measurement behind the low-trim study.

use yabcompiler_core::CompileOptions;
use yabcompiler_core::compile::{InplaceTrigger, low_trim_facts};

/// `LDA #$41 : JSR $FFD2 : RTS`.
fn ml() -> Vec<u8> {
    vec![0xA9, 0x41, 0x20, 0xD2, 0xFF, 0x60]
}

struct Line {
    number: u16,
    body: Vec<u8>,
}

fn line(number: u16, body: &[u8]) -> Line {
    Line { number, body: body.to_vec() }
}

const LOAD: u16 = 0x0801;

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
    // `text_end` is the address of that terminator pair.
    let text_end = addr;
    out.extend_from_slice(tail);
    (out, text_end)
}

fn sys(n: u16) -> Vec<u8> {
    let mut v = vec![0x9E];
    v.extend_from_slice(n.to_string().as_bytes());
    v
}

/// `REM` plus 64 bytes of filler ; 70 bytes of line, so a few dozen of them clear `MATERIALISE_MIN_SAVING` comfortably.
fn filler_line(n: u16) -> Line {
    let mut b = vec![0x8Fu8];
    b.extend(std::iter::repeat(b'X').take(64));
    line(n, &b)
}

/// `n` filler lines, then `last`, then a payload entered at `entry`.
fn program_with_payload(n: u16, last: Vec<u8>, entry_off: u16) -> (Vec<u8>, u16, u16) {
    let mut lines: Vec<Line> = (0..n).map(|i| filler_line(10 + i)).collect();
    lines.push(line(10 + n, &last));
    // Two passes: the entry address depends on `text_end`, which depends on the length of the line that names the entry.
    let (_, text_end) = build_prg(&lines, &[]);
    let entry = text_end + 2 + entry_off;
    let mut lines2: Vec<Line> = (0..n).map(|i| filler_line(10 + i)).collect();
    let mut body = last.clone();
    if body.is_empty() {
        body = sys(entry);
    } else {
        body.push(b':');
        body.extend_from_slice(&sys(entry));
    }
    lines2.push(line(10 + n, &body));
    let (_, te2) = build_prg(&lines2, &[]);
    let entry2 = te2 + 2 + entry_off;
    let mut lines3: Vec<Line> = (0..n).map(|i| filler_line(10 + i)).collect();
    let mut body3 = last;
    if body3.is_empty() {
        body3 = sys(entry2);
    } else {
        body3.push(b':');
        body3.extend_from_slice(&sys(entry2));
    }
    lines3.push(line(10 + n, &body3));
    let mut tail = vec![0u8; entry_off as usize];
    tail.extend_from_slice(&ml());
    let (prg, te3) = build_prg(&lines3, &tail);
    assert_eq!(te3, te2, "layout settled");
    (prg, te3, entry2)
}

fn facts(prg: &[u8]) -> yabcompiler_core::compile::LowTrimFacts {
    low_trim_facts(prg, &CompileOptions::default()).expect("materialises its image")
}

#[test]
fn dead_text_below_a_high_entry_is_measured() {
    let (prg, text_end, entry) = program_with_payload(60, Vec::new(), 0x40);
    let f = facts(&prg);
    assert!(f.has_appended_payload, "the payload is what makes this the mirror case");
    assert_eq!(f.trigger, Some(InplaceTrigger::SysIntoImage));
    assert_eq!(f.entries, vec![entry]);
    assert_eq!(f.text_end, text_end);
    // Today the whole image is embedded: no trimming happens at all.
    assert_eq!(f.materialised_len, f.body_len, "payload builds are exempt from the prefix trim");

    assert_eq!(f.raw_dead_bytes, usize::from(text_end - LOAD));

    let cut = f.guarded_cut.expect("a cut is available");
    assert!(cut > LOAD + 15, "cut {cut:#06x} must clear the launch head");
    assert!(cut + 0x0100 <= entry, "cut {cut:#06x} must leave the slack under {entry:#06x}");
    assert!(f.guarded_dead_bytes >= 2048, "a saving under the minimum is not taken");
    assert_eq!(f.guarded_dead_bytes, f.structural_dead_bytes, "no guard fires here");
    assert!(!f.reads_txttab);
}

#[test]
fn an_entry_low_in_the_text_leaves_nothing_to_trim() {
    let mut first = vec![0x8Fu8];
    first.extend_from_slice(&ml());
    let mut lines = vec![line(10, &first)];
    // The ML sits in line 10's body, at $0801 + 4 + 1.
    let entry = LOAD + 5;
    lines.push(line(20, &sys(entry)));
    for i in 0..60 {
        lines.push(filler_line(30 + i));
    }
    let (prg, _) = build_prg(&lines, &[]);
    let f = facts(&prg);
    assert_eq!(f.entries, vec![entry], "the low entry is still found");
    assert!(!f.has_appended_payload);
    assert_eq!(f.guarded_dead_bytes, 0, "nothing lies below an entry at {entry:#06x}");
    assert_eq!(f.structural_dead_bytes, 0);
}

/// The `PEEK`/`POKE` guard.
#[test]
fn a_poke_into_the_dead_text_pulls_the_cut_down() {
    let plain = program_with_payload(60, Vec::new(), 0x40);
    let baseline = facts(&plain.0);
    let target = LOAD + 0x0200;
    assert!(
        target < baseline.guarded_cut.expect("baseline cuts"),
        "the POKE has to land inside what the baseline would delete"
    );
    let mut poke = vec![0x97u8];
    poke.extend_from_slice(target.to_string().as_bytes());
    poke.push(b',');
    poke.push(b'0');
    let (prg, _, _) = program_with_payload(60, poke, 0x40);
    let f = facts(&prg);
    assert!(
        f.peek_poke_ranges.iter().any(|(lo, hi)| *lo <= target && target <= *hi),
        "the interval solver must see the literal write: {:?}",
        f.peek_poke_ranges
    );
    assert!(
        f.guarded_dead_bytes < f.structural_dead_bytes,
        "the guard must cost something: guarded {} structural {}",
        f.guarded_dead_bytes,
        f.structural_dead_bytes
    );
    if let Some(cut) = f.guarded_cut {
        assert!(cut <= target, "cut {cut:#06x} must not cross the write at {target:#06x}");
    }
}

#[test]
fn a_poke_above_the_cut_is_not_a_reason_to_refuse() {
    let plain = program_with_payload(60, Vec::new(), 0x40);
    let baseline = facts(&plain.0);
    let cut = baseline.guarded_cut.expect("baseline cuts");

    let target = cut + 0x20;
    let mut poke = vec![0x97u8];
    poke.extend_from_slice(target.to_string().as_bytes());
    poke.push(b',');
    poke.push(b'0');
    let (prg, _, _) = program_with_payload(60, poke, 0x40);
    let f = facts(&prg);
    assert!(
        f.guarded_dead_bytes > 0,
        "a write at {target:#06x}, above the cut, must not veto the trim"
    );
}

fn txttab_read() -> Vec<u8> {
    let mut b = b"A".to_vec();
    b.push(0xB2);
    b.push(0xC2);
    b.extend_from_slice(b"(43)");
    b.push(0xAA);
    b.extend_from_slice(b"256");
    b.push(0xAC);
    b.push(0xC2);
    b.extend_from_slice(b"(44)");
    b
}

#[test]
fn a_program_that_walks_its_own_text_is_never_trimmed() {
    let (prg, _, entry) = program_with_payload(60, txttab_read(), 0x40);
    let f = facts(&prg);
    assert!(f.reads_txttab, "the TXTTAB read must be detected");
    assert_eq!(f.entries, vec![entry], "and the program must still have a high entry");
    assert!(
        f.structural_dead_bytes >= 2048,
        "without the veto there would be {} bytes to take -- the veto has to be what removes them",
        f.structural_dead_bytes
    );
    assert_eq!(f.guarded_dead_bytes, 0, "a text-walker keeps every byte");
}

#[test]
fn a_variable_assigned_entry_is_resolved() {
    let (probe, text_end, _) = program_with_payload(60, Vec::new(), 0x40);
    let _ = probe;
    let entry = text_end + 2 + 0x40;
    let mut body = b"UP".to_vec();
    body.push(b'%');
    body.push(0xB2);
    body.extend_from_slice(entry.to_string().as_bytes());
    body.push(b':');
    body.push(0x9E);
    body.extend_from_slice(b"UP%");
    let mut lines: Vec<Line> = (0..60).map(|i| filler_line(10 + i)).collect();
    lines.push(line(70, &body));
    let mut tail = vec![0u8; 0x40];
    tail.extend_from_slice(&ml());
    let (prg, te) = build_prg(&lines, &tail);
    let f = facts(&prg);
    let real_entry = te + 2 + 0x40;
    // The operand text is a variable name; nothing in the file spells the address, yet the compiler resolves it.
    assert!(
        f.entries.contains(&entry) || f.entries.contains(&real_entry),
        "const-prop must resolve SYS UP%: entries {:?} wanted {entry:#06x} or {real_entry:#06x}",
        f.entries
    );
    assert!(f.raw_dead_bytes > 0);
}

#[test]
fn a_program_with_no_entry_is_not_a_candidate() {
    let mut lines = vec![line(10, b"A")];
    lines[0].body.push(0xB2);
    lines[0].body.extend_from_slice(b"1");
    for i in 0..60 {
        lines.push(filler_line(20 + i));
    }
    let (prg, _) = build_prg(&lines, &[]);
    assert!(
        low_trim_facts(&prg, &CompileOptions::default()).is_none(),
        "a program that never enters its own image is not an embed candidate"
    );
}
