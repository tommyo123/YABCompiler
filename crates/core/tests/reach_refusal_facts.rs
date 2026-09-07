use yabcompiler_core::compile::{LowCut, low_trim_facts};
use yabcompiler_core::image_reach::RefusalCause;
use yabcompiler_core::{CompileOptions, compile_with_options};

const LOAD: u16 = 0x0801;
/// `compile::INPLACE_HEAD` ; the `SYS` stub plus the `JMP` over the blob.
const HEAD: usize = 15;
/// `compile::MATERIALISE_MIN_SAVING`.
const MIN_SAVING: usize = 2048;

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

const FILLERS: u16 = 60;

struct Prog {
    prg: Vec<u8>,
    entry: u16,
}

fn program(last: &[u8], pad: u16, ml: &[u8]) -> Prog {
    let mut entry = 0u16;
    for _ in 0..4 {
        let mut lines: Vec<Line> = (0..FILLERS).map(|i| filler_line(10 + i)).collect();
        let mut body = last.to_vec();
        if entry != 0 {
            if !body.is_empty() {
                body.push(b':');
            }
            body.extend_from_slice(&sys(entry));
        }
        lines.push(line(10 + FILLERS, &body));
        let mut tail = vec![0u8; usize::from(pad)];
        tail.extend_from_slice(ml);
        let (prg, text_end) = build_prg(&lines, &tail);
        let want = text_end + 2 + pad;
        if want == entry {
            return Prog { prg, entry };
        }
        entry = want;
    }
    panic!("layout did not settle");
}

fn prefix_program(ml: &[u8]) -> Vec<u8> {
    assert!(!ml.contains(&0), "a $00 would end the REM line");
    // link(2) + line number(2) + the REM token = 5 bytes before the code.
    let entry = LOAD + 5;
    let mut first = vec![0x8Fu8];
    first.extend_from_slice(ml);
    let mut lines = vec![line(10, &first)];
    lines.extend((0..FILLERS).map(|i| filler_line(20 + i)));
    lines.push(line(20 + FILLERS, &sys(entry)));
    build_prg(&lines, &[]).0
}

/// `LDA #$41 : JSR $FFD2 : RTS` ; nothing the walk cannot follow.
fn ml_clean() -> Vec<u8> {
    vec![0xA9, 0x41, 0x20, 0xD2, 0xFF, 0x60]
}

fn ml_indirect() -> Vec<u8> {
    vec![0xB1, 0xFB, 0x60]
}

fn ml_indirect_resolvable() -> Vec<u8> {
    vec![
        0xA9, 0x00, 0x85, 0xFB, 0xA9, 0x40, 0x85, 0xFC, 0xB1, 0xFB, 0x60,
    ]
}

fn facts(prg: &[u8]) -> yabcompiler_core::compile::LowTrimFacts {
    low_trim_facts(prg, &CompileOptions::default()).expect("materialises its image")
}

fn taken(c: LowCut) -> usize {
    match c {
        LowCut::Take(n) => n - HEAD,
        LowCut::Refused(_) => 0,
    }
}

#[test]
fn a_program_the_scan_can_follow_reports_no_refusals() {
    let p = program(b"", 0x40, &ml_clean());
    let f = facts(&p.prg);
    assert_eq!(f.refusals, vec![], "clean ML must produce no refusal");
    assert!(!f.scan_refused);
    assert_eq!(
        f.optimistic_span.map(|(lo, _)| lo),
        f.reachable_low,
        "the optimistic span must equal the real one where there is one",
    );
    assert!(matches!(f.decision, LowCut::Take(_)), "{:?}", f.decision);
    assert_eq!(
        f.decision, f.hypo_decision,
        "with nothing refused there is nothing to hypothesise",
    );
}

#[test]
fn a_scan_refusal_is_named_with_its_address_and_opcode() {
    let p = program(b"", 0x40, &ml_indirect());
    let f = facts(&p.prg);
    assert_eq!(f.decision.reason(), Some("scan-refused"));
    assert_eq!(f.refusals.len(), 1, "got {:?}", f.refusals);
    let r = f.refusals[0];
    assert_eq!(r.cause, RefusalCause::IndirectIndexedY);
    assert_eq!(r.cause.slug(), "indirect-y");

    assert_eq!(r.addr, p.entry);
    assert_eq!(r.opcode, 0xB1);
    assert_eq!(r.zp, Some(0xFB));
    assert_eq!(r.from_entries, 1);
    assert_eq!(u16::try_from(f.entries.len()).unwrap(), r.from_entries);
}

#[test]
fn a_scan_refusal_is_priced_by_the_builds_own_arithmetic() {
    let refused = facts(&program(b"", 0x40, &ml_indirect()).prg);
    let clean = facts(&program(b"", 0x40, &ml_clean()).prg);
    assert_eq!(refused.decision.reason(), Some("scan-refused"));

    assert_eq!(
        taken(refused.hypo_decision),
        taken(clean.decision),
        "the hypothetical must be the arithmetic the build would have run",
    );
    assert!(
        taken(refused.hypo_decision) >= MIN_SAVING,
        "the fixture must have something to recover, got {}",
        taken(refused.hypo_decision),
    );

    assert_eq!(refused.materialised_len, refused.body_len);
}

#[test]
fn a_refusal_behind_the_txttab_veto_is_not_credited_to_the_scan() {
    // `PRINT PEEK(44)` ; printed, not assigned: an assignment to an unused variable is dead-store-eliminated before the detector sees it.
    let last = vec![0x99, 0xC2, b'(', b'4', b'4', b')'];
    let f = facts(&program(&last, 0x40, &ml_indirect()).prg);
    assert!(f.reads_txttab);
    assert!(!f.refusals.is_empty(), "the ML must still be unfollowable");
    assert_eq!(f.decision.reason(), Some("txttab"));
    assert_eq!(
        f.hypo_decision.reason(),
        Some("txttab"),
        "converting the refusal would release nothing here",
    );
    assert_eq!(taken(f.hypo_decision), 0);
}

#[test]
fn a_refusal_behind_the_poke_veto_is_not_credited_to_the_scan() {
    let target = LOAD + 0x0200;
    let mut last = vec![0x97u8];
    last.extend_from_slice(target.to_string().as_bytes());
    last.extend_from_slice(b",0");
    let f = facts(&program(&last, 0x40, &ml_indirect()).prg);
    assert!(
        f.peek_poke_ranges.iter().any(|(lo, _)| *lo == target),
        "the fixture must produce the range, got {:?}",
        f.peek_poke_ranges,
    );
    assert_eq!(f.decision.reason(), Some("scan-refused"));
    assert_eq!(
        f.hypo_decision.reason(),
        Some("min-saving"),
        "with the scan answered, the POKE is what refuses — and it releases nothing",
    );
    assert_eq!(taken(f.hypo_decision), 0);
}

#[test]
fn a_pointer_the_walk_itself_built_is_reported_resolvable() {
    let f = facts(&program(b"", 0x40, &ml_indirect_resolvable()).prg);
    let r = f
        .refusals
        .iter()
        .find(|r| r.cause == RefusalCause::IndirectIndexedY)
        .expect("the indirect is still a refusal");
    assert!(r.zp_from_walk, "both pointer bytes come from visited LDA #");
    assert!(r.zp_linear);
}

#[test]
fn a_pointer_nothing_sets_is_not_reported_resolvable() {
    let f = facts(&program(b"", 0x40, &ml_indirect()).prg);
    let r = f.refusals[0];
    assert!(!r.zp_from_walk);
    assert!(!r.zp_linear);
}

#[test]
fn the_prefix_family_prices_its_refusal_too() {
    let prg = prefix_program(&ml_indirect());
    let f = facts(&prg);
    assert!(
        !f.has_appended_payload,
        "the prefix family carries nothing above the link chain",
    );
    assert!(f.scan_refused);
    assert_eq!(
        f.materialised_len, f.body_len,
        "the refusal is materialising the whole image today",
    );
    // The price: what the identical program with a followable routine gets.
    let clean = facts(&prefix_program(&ml_clean()));
    let took = clean.body_len - clean.materialised_len;
    assert!(took >= MIN_SAVING, "the control must really trim, got {took}");
    assert_eq!(f.hypo_prefix_saving, took);

    assert_eq!(f.hypo_decision.reason(), Some("prefix-family"));
}

#[test]
fn the_prefix_trim_prices_at_zero_when_it_already_happened() {
    let f = facts(&prefix_program(&ml_clean()));
    assert!(f.refusals.is_empty());
    assert!(f.materialised_len < f.body_len, "the trim really happened");
    assert!(f.hypo_prefix_saving > 0, "the arithmetic still runs…");

    assert_ne!(f.materialised_len, f.body_len);

    let c = compile_with_options(&f_prg_clean(), CompileOptions::default()).expect("builds");
    assert!(c.diagnostics.end_address > LOAD);
}

fn f_prg_clean() -> Vec<u8> {
    prefix_program(&ml_clean())
}

#[test]
fn txttab_uses_are_told_apart_by_shape() {

    let with = |last: &[u8]| {
        let mut lines: Vec<Line> = (0..4).map(|i| filler_line(10 + i)).collect();
        lines.push(line(90, last));
        facts(&build_prg(&lines, &[]).0)
    };

    // `PEEK(44)*256+12`, tokenised.
    const PTR: &[u8] = &[0xC2, b'(', b'4', b'4', b')', 0xAC, b'2', b'5', b'6', 0xAA, b'1', b'2'];
    let stmt = |tok: u8, tail: &[u8]| {
        let mut v = vec![tok];
        v.extend_from_slice(PTR);
        v.extend_from_slice(tail);
        v
    };

    // `SYS PEEK(44)*256+12` ; one address, not the whole text.
    let f = with(&stmt(0x9E, b""));
    assert!(f.reads_txttab);
    assert_eq!(f.txttab.sys_target, 1);
    assert!(f.txttab.only_sys_targets(), "got {:?}", f.txttab);

    // `PRINT PEEK(PEEK(44)*256+12)` ; a genuine read of its own text.
    let mut deref = vec![0x99u8, 0xC2, b'('];
    deref.extend_from_slice(PTR);
    deref.push(b')');
    let f = with(&deref);
    assert_eq!(f.txttab.deref_read, 1, "got {:?}", f.txttab);
    assert!(!f.txttab.only_sys_targets());

    // `POKE PEEK(44)*256+12,0` ; a genuine write to it.
    let f = with(&stmt(0x97, b",0"));
    assert_eq!(f.txttab.deref_write, 1, "got {:?}", f.txttab);
    assert!(!f.txttab.only_sys_targets());

    let f = with(&[0x99, 0xC2, b'(', b'4', b'4', b')']);
    assert_eq!(f.txttab.other, 1);
    assert!(!f.txttab.only_sys_targets());
}
