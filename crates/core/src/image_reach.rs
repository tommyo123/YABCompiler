//! Find bytes reachable by machine-code calls into the original program image.
//! Unresolved instructions or addresses require keeping the full image.

use std::collections::BTreeSet;

/// Addressing mode, which is all the scan needs from an opcode: the instruction's length and where its operand points.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    /// Implied / accumulator ; one byte, no operand.
    Imp,

    Imm,
    Zp,
    ZpX,
    ZpY,
    /// `($nn,X)` / `($nn),Y` ; the pointer lives in zero page and its contents are not knowable here.
    Izx,
    Izy,
    /// Branch ; two bytes, target is PC-relative.
    Rel,
    /// Absolute, absolute indexed ; three bytes, operand is an address.
    Abs,
    AbX,
    AbY,

    Ind,

    Illegal,
}

impl Mode {
    fn len(self) -> u16 {
        match self {
            Mode::Imp => 1,
            Mode::Imm | Mode::Zp | Mode::ZpX | Mode::ZpY | Mode::Izx | Mode::Izy | Mode::Rel => 2,
            Mode::Abs | Mode::AbX | Mode::AbY | Mode::Ind => 3,
            Mode::Illegal => 0,
        }
    }
}

/// What the instruction does to control flow.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Flow {
    /// Falls through to the next instruction.
    Next,
    /// `JMP $nnnn` ; continues at the operand, never falls through.
    Jump,
    /// `JSR $nnnn` ; continues at the operand and falls through.
    Call,
    /// Branch ; both the target and the fall-through are live.
    Branch,
    /// `RTS` / `RTI` ; the path ends.
    Stop,
    /// `BRK` ; a program that BRKs inside its own image is doing something this scan has no model for.
    Refuse,
}

/// `(mode, flow)` for one opcode.
fn decode(op: u8) -> (Mode, Flow) {
    use Flow::*;
    use Mode::*;
    match op {
        0x00 => (Imp, Refuse),
        0x20 => (Abs, Call),
        0x4C => (Abs, Jump),
        0x6C => (Ind, Refuse),
        0x40 | 0x60 => (Imp, Stop),
        0x10 | 0x30 | 0x50 | 0x70 | 0x90 | 0xB0 | 0xD0 | 0xF0 => (Rel, Branch),

        // implied / accumulator
        0x08 | 0x18 | 0x28 | 0x38 | 0x48 | 0x58 | 0x68 | 0x78 | 0x88 | 0x8A | 0x98 | 0x9A
        | 0xA8 | 0xAA | 0xB8 | 0xBA | 0xC8 | 0xCA | 0xD8 | 0xE8 | 0xEA | 0xF8 => (Imp, Next),
        0x0A | 0x2A | 0x4A | 0x6A => (Imp, Next), // ASL/ROL/LSR/ROR A
        0x09 | 0x29 | 0x49 | 0x69 | 0xA0 | 0xA2 | 0xA9 | 0xC0 | 0xC9 | 0xE0 | 0xE9 => (Imm, Next),

        // zero page
        0x05 | 0x06 | 0x24 | 0x25 | 0x26 | 0x45 | 0x46 | 0x65 | 0x66 | 0x84 | 0x85 | 0x86
        | 0xA4 | 0xA5 | 0xA6 | 0xC4 | 0xC5 | 0xC6 | 0xE4 | 0xE5 | 0xE6 => (Zp, Next),
        // zero page,X
        0x15 | 0x16 | 0x35 | 0x36 | 0x55 | 0x56 | 0x75 | 0x76 | 0x94 | 0x95 | 0xB4 | 0xB5
        | 0xD5 | 0xD6 | 0xF5 | 0xF6 => (ZpX, Next),
        // zero page,Y
        0x96 | 0xB6 => (ZpY, Next),

        0x01 | 0x21 | 0x41 | 0x61 | 0x81 | 0xA1 | 0xC1 | 0xE1 => (Izx, Next),
        0x11 | 0x31 | 0x51 | 0x71 | 0x91 | 0xB1 | 0xD1 | 0xF1 => (Izy, Next),

        // absolute
        0x0D | 0x0E | 0x2C | 0x2D | 0x2E | 0x4D | 0x4E | 0x6D | 0x6E | 0x8C | 0x8D | 0x8E
        | 0xAC | 0xAD | 0xAE | 0xCC | 0xCD | 0xCE | 0xEC | 0xED | 0xEE => (Abs, Next),
        // absolute,X
        0x1D | 0x1E | 0x3D | 0x3E | 0x5D | 0x5E | 0x7D | 0x7E | 0x9D | 0xBC | 0xBD | 0xDD
        | 0xDE | 0xFD | 0xFE => (AbX, Next),
        // absolute,Y.
        0x19 | 0x39 | 0x59 | 0x79 | 0x99 | 0xB9 | 0xBE | 0xD9 | 0xF9 => (AbY, Next),

        _ => (Illegal, Refuse),
    }
}

/// Instructions the scan will decode before giving up.
const BUDGET: usize = 4096;

const INDEX_SLACK: u16 = 0xFF;

/// The highest image address reachable from `entries`, or `None` when the scan met something it cannot account for.
pub(crate) fn reachable_end(body: &[u8], load: u16, entries: &[u16]) -> Option<u16> {
    reachable_span(body, load, entries).map(|(_, high)| high)
}

/// The lowest and highest image address reachable from `entries`, or `None` when the scan met something it cannot account for.
pub(crate) fn reachable_span(body: &[u8], load: u16, entries: &[u16]) -> Option<(u16, u16)> {
    reachable_span_strict(body, load, entries)
}

/// The walk as it has always been: any indirect operand is a refusal.
fn reachable_span_strict(body: &[u8], load: u16, entries: &[u16]) -> Option<(u16, u16)> {
    if body.is_empty() || entries.is_empty() {
        return None;
    }
    let last = u16::try_from(load as usize + body.len() - 1).ok()?;
    let inside = |a: u16| a >= load && a <= last;
    let at = |a: u16| body[(a - load) as usize];

    let mut seen: BTreeSet<u16> = BTreeSet::new();
    let mut work: Vec<u16> = Vec::new();
    let mut high = load;
    let mut low = last;
    for e in entries {
        if !inside(*e) {

            return None;
        }
        work.push(*e);
        high = high.max(*e);
        low = low.min(*e);
    }

    let mut budget = BUDGET;
    while let Some(mut pc) = work.pop() {
        loop {
            if !inside(pc) {
                // Execution left the image.
                break;
            }
            if !seen.insert(pc) {
                break; // already walked from here
            }
            budget = budget.checked_sub(1)?;
            let (mode, flow) = decode(at(pc));

            if mode == Mode::Illegal
                || flow == Flow::Refuse
                || matches!(mode, Mode::Izx | Mode::Izy)
            {
                return None;
            }
            let len = mode.len();
            // The instruction's own bytes have to survive.
            let end = pc.checked_add(len - 1)?;
            if !inside(end) {
                // The last instruction runs off the end of the image: the scan has lost the thread.
                return None;
            }
            high = high.max(end);
            // The instruction's own first byte is the floor it contributes; `end` is the ceiling.
            low = low.min(pc);

            // Operand that names an address.
            match mode {
                Mode::Abs | Mode::AbX | Mode::AbY => {
                    let target = u16::from_le_bytes([at(pc + 1), at(pc + 2)]);
                    let reach = match mode {
                        Mode::Abs => Some(target),
                        // A run-time index can carry the access a full page past the base.
                        _ => Some(target.saturating_add(INDEX_SLACK)),
                    };
                    if let Some(r) = reach {
                        if inside(target) {
                            high = high.max(r.min(last));
                            // An index is added, so whichever mode this is, the base is the lowest byte the access can name.
                            low = low.min(target);
                        }
                    }
                }
                _ => {}
            }

            let fallthrough = |pc: u16, len: u16| -> Option<u16> {
                let next = pc.checked_add(len)?;
                inside(next).then_some(next)
            };
            match flow {
                Flow::Stop => break,
                Flow::Jump => {
                    pc = u16::from_le_bytes([at(pc + 1), at(pc + 2)]);
                    continue;
                }
                Flow::Call => {
                    let target = u16::from_le_bytes([at(pc + 1), at(pc + 2)]);
                    if inside(target) {
                        work.push(target);
                    }
                    pc = fallthrough(pc, len)?;
                    continue;
                }
                Flow::Branch => {
                    let next = fallthrough(pc, len)?;
                    let off = at(pc + 1) as i8 as i16;
                    let target = (next as i32 + off as i32) as u16;
                    if inside(target) {
                        work.push(target);
                    }
                    pc = next;
                    continue;
                }
                Flow::Next => {
                    pc = fallthrough(pc, len)?;
                    continue;
                }
                Flow::Refuse => return None,
            }
        }
    }
    Some((low.min(high), high))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefusalCause {

    IllegalOpcode,

    Brk,

    JmpIndirect,
    /// `($nn),Y` ; the pointer is in zero page.
    IndirectIndexedY,
    /// `($nn,X)` ; X selects which pointer at run time.
    IndirectIndexedX,
    /// The instruction's own operand bytes run past the last byte of the image.
    OperandOffEnd,
    /// Fall-through leaves the image: the routine continues into memory the image does not define.
    RanOffEnd,
    /// [`BUDGET`] instructions decoded ; the scan has misread something as code.
    Budget,
    /// An entry point outside `[load, last]`.
    EntryOutside,
}

impl RefusalCause {

    pub fn slug(self) -> &'static str {
        match self {
            RefusalCause::IllegalOpcode => "illegal-opcode",
            RefusalCause::Brk => "brk",
            RefusalCause::JmpIndirect => "jmp-indirect",
            RefusalCause::IndirectIndexedY => "indirect-y",
            RefusalCause::IndirectIndexedX => "indirect-x",
            RefusalCause::OperandOffEnd => "operand-off-end",
            RefusalCause::RanOffEnd => "ran-off-end",
            RefusalCause::Budget => "budget",
            RefusalCause::EntryOutside => "entry-outside",
        }
    }
}

/// One instruction [`reachable_span`] cannot account for, named.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReachRefusal {
    /// Address of the offending instruction, or of the offending entry point for [`RefusalCause::EntryOutside`].
    pub addr: u16,
    /// Its first byte.
    pub opcode: u8,
    pub cause: RefusalCause,
    /// The zero-page pointer the operand names, for the two indirect causes only.
    pub zp: Option<u8>,

    pub zp_linear: bool,
    /// The tight resolvability test.
    pub zp_from_walk: bool,
    /// How many of the entry points reach this instruction.
    pub from_entries: u16,
}

/// What the walk found, refusals and all.
#[derive(Clone, Debug)]
pub struct ReachDiag {
    /// Every refusal site, in ascending address order.
    pub refusals: Vec<ReachRefusal>,
    /// Optimistic lowest and highest addresses named ; see the type doc.
    pub low: u16,
    pub high: u16,
    /// Instruction starts the walk decoded.
    pub instructions: usize,
}

/// One entry point's worth of walking, ignoring refusals where that is defined.
#[allow(clippy::type_complexity)]
fn walk_recording(
    body: &[u8],
    load: u16,
    last: u16,
    entries: &[u16],
    seen: &mut BTreeSet<u16>,
    sink: &mut Vec<(u16, u8, RefusalCause, Option<u8>)>,
) -> (u16, u16) {
    let inside = |a: u16| a >= load && a <= last;
    let at = |a: u16| body[(a - load) as usize];
    let mut high = load;
    let mut low = last;
    let mut work: Vec<u16> = Vec::new();
    for e in entries {
        if !inside(*e) {
            sink.push((*e, 0, RefusalCause::EntryOutside, None));
            continue;
        }
        work.push(*e);
        high = high.max(*e);
        low = low.min(*e);
    }
    let mut budget = BUDGET;
    'outer: while let Some(mut pc) = work.pop() {
        loop {
            if !inside(pc) || !seen.insert(pc) {
                break;
            }
            match budget.checked_sub(1) {
                Some(b) => budget = b,
                None => {
                    sink.push((pc, at(pc), RefusalCause::Budget, None));
                    break 'outer;
                }
            }
            let op = at(pc);
            let (mode, flow) = decode(op);
            if mode == Mode::Illegal {
                sink.push((pc, op, RefusalCause::IllegalOpcode, None));
                break;
            }
            let len = mode.len();
            let Some(end) = pc.checked_add(len - 1) else {
                sink.push((pc, op, RefusalCause::OperandOffEnd, None));
                break;
            };
            if !inside(end) {
                sink.push((pc, op, RefusalCause::OperandOffEnd, None));
                break;
            }
            high = high.max(end);
            low = low.min(pc);
            if let Mode::Abs | Mode::AbX | Mode::AbY = mode {
                let target = u16::from_le_bytes([at(pc + 1), at(pc + 2)]);
                if inside(target) {
                    let reach = match mode {
                        Mode::Abs => target,
                        _ => target.saturating_add(INDEX_SLACK),
                    };
                    high = high.max(reach.min(last));
                    low = low.min(target);
                }
            }

            match mode {
                Mode::Izy => {
                    sink.push((pc, op, RefusalCause::IndirectIndexedY, Some(at(pc + 1))))
                }
                Mode::Izx => {
                    sink.push((pc, op, RefusalCause::IndirectIndexedX, Some(at(pc + 1))))
                }
                _ => {}
            }
            let fallthrough = |pc: u16, len: u16| -> Option<u16> {
                let next = pc.checked_add(len)?;
                inside(next).then_some(next)
            };
            match flow {
                Flow::Stop => break,
                Flow::Refuse => {

                    let cause = if op == 0x00 {
                        RefusalCause::Brk
                    } else {
                        RefusalCause::JmpIndirect
                    };
                    sink.push((pc, op, cause, None));
                    break;
                }
                Flow::Jump => {
                    pc = u16::from_le_bytes([at(pc + 1), at(pc + 2)]);
                }
                Flow::Call => {
                    let target = u16::from_le_bytes([at(pc + 1), at(pc + 2)]);
                    if inside(target) {
                        work.push(target);
                    }
                    match fallthrough(pc, len) {
                        Some(n) => pc = n,
                        None => {
                            sink.push((pc, op, RefusalCause::RanOffEnd, None));
                            break;
                        }
                    }
                }
                Flow::Branch => {
                    let Some(next) = fallthrough(pc, len) else {
                        sink.push((pc, op, RefusalCause::RanOffEnd, None));
                        break;
                    };
                    let off = at(pc + 1) as i8 as i16;
                    let target = (next as i32 + off as i32) as u16;
                    if inside(target) {
                        work.push(target);
                    }
                    pc = next;
                }
                Flow::Next => match fallthrough(pc, len) {
                    Some(n) => pc = n,
                    None => {
                        sink.push((pc, op, RefusalCause::RanOffEnd, None));
                        break;
                    }
                },
            }
        }
    }
    (low.min(high), high)
}

/// Is `zd` written from an immediate by an instruction the walk decoded, unambiguously?
fn zp_resolved_by_walk(body: &[u8], load: u16, seen: &BTreeSet<u16>, zd: u8) -> bool {
    let at = |a: u16| body.get((a.wrapping_sub(load)) as usize).copied();
    let (mut a, mut x, mut y) = (None, None, None);

    let mut val: Option<Option<u8>> = None;
    for pc in seen {
        let Some(op) = at(*pc) else { continue };
        let arg = at(pc.wrapping_add(1));
        match op {
            0xA9 => a = arg,
            0xA2 => x = arg,
            0xA0 => y = arg,
            0x85 | 0x86 | 0x84 if arg == Some(zd) => {
                let src = match op {
                    0x85 => a,
                    0x86 => x,
                    _ => y,
                };
                val = Some(match val {
                    None => src,
                    Some(prev) if prev == src => src,
                    Some(_) => None,
                });
            }
            _ => {}
        }
    }
    matches!(val, Some(Some(_)))
}

/// Walk the image from `entries` and report every refusal with its weight, instead of stopping at the first one.
pub(crate) fn diagnose(body: &[u8], load: u16, entries: &[u16]) -> Option<ReachDiag> {
    if body.is_empty() || entries.is_empty() {
        return None;
    }
    let last = u16::try_from(load as usize + body.len() - 1).ok()?;

    let mut seen: BTreeSet<u16> = BTreeSet::new();
    let mut sink: Vec<(u16, u8, RefusalCause, Option<u8>)> = Vec::new();
    let (low, high) = walk_recording(body, load, last, entries, &mut seen, &mut sink);

    // Which entries reach each site.
    let mut per_entry: Vec<BTreeSet<u16>> = Vec::with_capacity(entries.len());
    for e in entries {
        let mut s = BTreeSet::new();
        let mut k: Vec<(u16, u8, RefusalCause, Option<u8>)> = Vec::new();
        walk_recording(body, load, last, std::slice::from_ref(e), &mut s, &mut k);
        per_entry.push(k.into_iter().map(|(a, ..)| a).collect());
    }

    let mut refusals: Vec<ReachRefusal> = Vec::new();
    let mut done: BTreeSet<(u16, u8)> = BTreeSet::new();
    for (addr, opcode, cause, zp) in sink {
        // One site can be pushed twice ; a `Budget` on an address that also carried an indirect operand.
        if !done.insert((addr, cause as u8)) {
            continue;
        }
        let (zp_linear, zp_from_walk) = match (cause, zp) {
            // `($nn,X)` selects the pointer with a run-time index, so there is no single zero-page address to resolve.
            (RefusalCause::IndirectIndexedY, Some(zd)) => {
                let hi = zd.checked_add(1);
                let lin = addr.checked_sub(load).is_some_and(|off| {
                    let st = track_immediates(&body[..off as usize]);
                    hi.is_some_and(|h| st.zp.contains_key(&zd) && st.zp.contains_key(&h))
                });
                let wk = hi.is_some_and(|h| {
                    zp_resolved_by_walk(body, load, &seen, zd)
                        && zp_resolved_by_walk(body, load, &seen, h)
                });
                (lin, wk)
            }
            _ => (false, false),
        };
        let from_entries =
            u16::try_from(per_entry.iter().filter(|s| s.contains(&addr)).count()).unwrap_or(u16::MAX);
        refusals.push(ReachRefusal {
            addr,
            opcode,
            cause,
            zp,
            zp_linear,
            zp_from_walk,
            from_entries,
        });
    }
    refusals.sort_by_key(|r| (r.addr, r.cause as u8));
    Some(ReachDiag {
        refusals,
        low,
        high,
        instructions: seen.len(),
    })
}

#[derive(Default)]
pub(crate) struct ImmState {
    pub(crate) a: Option<u8>,
    pub(crate) x: Option<u8>,
    pub(crate) y: Option<u8>,
    pub(crate) zp: std::collections::HashMap<u8, u8>,
}

pub(crate) fn track_immediates(code: &[u8]) -> ImmState {
    let mut st = ImmState::default();
    let mut i = 0usize;
    while i + 1 < code.len() {
        let (op, arg) = (code[i], code[i + 1]);
        match op {
            0xA9 => st.a = Some(arg),
            0xA2 => st.x = Some(arg),
            0xA0 => st.y = Some(arg),
            0x85 => {
                if let Some(v) = st.a {
                    st.zp.insert(arg, v);
                }
            }
            0x86 => {
                if let Some(v) = st.x {
                    st.zp.insert(arg, v);
                }
            }
            0x84 => {
                if let Some(v) = st.y {
                    st.zp.insert(arg, v);
                }
            }
            _ => {
                i += 1;
                continue;
            }
        }
        i += 2;
    }
    st
}

/// What one instruction writes to memory.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Wr {
    /// Writes nothing outside zero page and the stack.
    None,
    /// One byte, at the absolute operand.
    One,
    /// `abs,X` / `abs,Y`: a run-time index carries the store up to a full page past the base.
    Page,
    /// `STA (zp),Y` ; the operand names the pointer, not the target.
    IndY,

    IndX,
}

/// Which opcodes store, and how wide.
fn writes(op: u8) -> Wr {
    match op {
        // STA/STX/STY absolute, and the read-modify-writes.
        0x8D | 0x8E | 0x8C | 0x0E | 0x2E | 0x4E | 0x6E | 0xCE | 0xEE => Wr::One,
        // STA abs,X / abs,Y and the indexed read-modify-writes.
        0x9D | 0x99 | 0x1E | 0x3E | 0x5E | 0x7E | 0xDE | 0xFE => Wr::Page,
        0x91 => Wr::IndY,
        0x81 => Wr::IndX,
        _ => Wr::None,
    }
}

/// ROM entry points a routine may call without the scan losing its grip on what gets written.
const ROM_NO_USER_RAM_WRITE: [u16; 2] = [0xFFE1, 0xFFE4];

pub(crate) fn write_extent(body: &[u8], load: u16, entries: &[u16]) -> Option<Vec<(u16, u16)>> {
    let seen = reachable_instructions(body, load, entries)?;
    let at = |a: u16| body[(a - load) as usize];
    let word_at = |a: u16| u16::from_le_bytes([at(a + 1), at(a + 2)]);
    let mut out: Vec<(u16, u16)> = Vec::new();
    for pc in &seen {
        let op = at(*pc);
        match writes(op) {
            Wr::None => {}
            Wr::One => {
                let t = word_at(*pc);
                out.push((t, t));
            }
            Wr::Page => {
                let t = word_at(*pc);
                out.push((t, t.saturating_add(0xFF)));
            }
            Wr::IndX => return None,
            Wr::IndY => out.push(indy_store_span(body, load, *pc, at(*pc + 1), &seen)?),
        }
    }
    Some(out)
}

/// The instruction start addresses a `SYS` into the image can reach, or `None` when the walk lost the thread.
fn reachable_instructions(body: &[u8], load: u16, entries: &[u16]) -> Option<BTreeSet<u16>> {
    if body.is_empty() || entries.is_empty() {
        return None;
    }
    let last = u16::try_from(load as usize + body.len() - 1).ok()?;
    let inside = |a: u16| a >= load && a <= last;
    let at = |a: u16| body[(a - load) as usize];

    let mut seen: BTreeSet<u16> = BTreeSet::new();
    let mut work: Vec<u16> = Vec::new();
    for e in entries {
        if !inside(*e) {
            return None;
        }
        work.push(*e);
    }
    let mut budget = BUDGET;
    while let Some(mut pc) = work.pop() {
        loop {
            if !inside(pc) || !seen.insert(pc) {
                break;
            }
            budget = budget.checked_sub(1)?;
            let (mode, flow) = decode(at(pc));
            if mode == Mode::Illegal || flow == Flow::Refuse {
                return None;
            }
            let len = mode.len();
            let end = pc.checked_add(len - 1)?;
            if !inside(end) {
                return None;
            }
            let fallthrough = |pc: u16, len: u16| -> Option<u16> {
                let next = pc.checked_add(len)?;
                inside(next).then_some(next)
            };
            // Control leaving the image is where the two analyses part company hardest.
            let leaving = |t: u16| -> bool { !inside(t) && !ROM_NO_USER_RAM_WRITE.contains(&t) };
            match flow {
                Flow::Stop => break,
                Flow::Jump => {
                    let t = word_at_in(body, load, pc);
                    if leaving(t) {
                        return None;
                    }
                    if !inside(t) {
                        break;
                    }
                    pc = t;
                }
                Flow::Call => {
                    let t = word_at_in(body, load, pc);
                    if leaving(t) {
                        return None;
                    }
                    if inside(t) {
                        work.push(t);
                    }
                    pc = fallthrough(pc, len)?;
                }
                Flow::Branch => {
                    let next = fallthrough(pc, len)?;
                    let off = at(pc + 1) as i8 as i16;
                    let target = (next as i32 + off as i32) as u16;
                    // `reachable_end` may ignore a branch that leaves the image: it reaches none of the program's own bytes.
                    if !inside(target) {
                        return None;
                    }
                    work.push(target);
                    pc = next;
                }
                Flow::Next => pc = fallthrough(pc, len)?,
                Flow::Refuse => return None,
            }
        }
    }
    Some(seen)
}

fn word_at_in(body: &[u8], load: u16, pc: u16) -> u16 {
    let o = (pc - load) as usize;
    u16::from_le_bytes([body[o + 1], body[o + 2]])
}

/// The span a `STA (zd),Y` at `site` can cover.
fn indy_store_span(
    body: &[u8],
    load: u16,
    site: u16,
    zd: u8,
    seen: &BTreeSet<u16>,
) -> Option<(u16, u16)> {

    let at = |a: u16| body.get((a.wrapping_sub(load)) as usize).copied();
    let st = track_immediates(&body[..(site - load) as usize]);
    let zh = zd.checked_add(1)?;
    let p0 = u16::from_le_bytes([*st.zp.get(&zd)?, *st.zp.get(&zh)?]);
    // Is the pointer's high byte walked, and which way?
    let steps =
        |op: u8| seen.iter().any(|a| at(*a) == Some(op) && at(a.wrapping_add(1)) == Some(zh));
    let (up, down) = (steps(0xE6), steps(0xC6));
    let hi0 = (p0 >> 8) as u8;
    let (lo_hi, hi_hi) = match (up, down) {

        (false, false) => (hi0, hi0),
        // Walked both ways at once.
        (true, true) => return None,
        (true, false) => (hi0, page_limit(body, load, seen, zh, st.x, hi0, true)?),
        (false, true) => (page_limit(body, load, seen, zh, st.x, hi0, false)?, hi0),
    };
    if lo_hi > hi_hi {
        return None;
    }
    let keep_lo = (p0 & 0x00FF) as u8;
    let lowest = u16::from_le_bytes([keep_lo, lo_hi]);
    let highest = u16::from_le_bytes([keep_lo, hi_hi]);
    Some((lowest, highest.checked_add(0xFF)?))
}

fn page_limit(
    body: &[u8],
    load: u16,
    seen: &BTreeSet<u16>,
    zh: u8,
    x: Option<u8>,
    hi0: u8,
    up: bool,
) -> Option<u8> {
    let at = |a: u16| body.get((a.wrapping_sub(load)) as usize).copied();
    // `LDA zh` immediately followed by `CMP #imm`, both decoded by the walk.
    let mut lim: Option<u8> = None;
    for a in seen {
        if at(*a) != Some(0xA5) || at(a.wrapping_add(1)) != Some(zh) {
            continue;
        }
        let cmp = a.wrapping_add(2);
        if !seen.contains(&cmp) || at(cmp) != Some(0xC9) {
            continue;
        }
        let Some(v) = at(cmp.wrapping_add(1)) else {
            continue;
        };
        if lim.is_some_and(|prev| prev != v) {
            return None;
        }
        lim = Some(v);
    }
    if let Some(v) = lim {
        // The step happens before the test, so the loop leaves the high byte AT `lim` having last written the page one before it.
        return if up {
            v.checked_sub(1)
        } else {
            v.checked_add(1)
        };
    }

    let n = x?;
    if n == 0 || !seen.iter().any(|a| at(*a) == Some(0xCA)) {
        return None;
    }
    if up {
        hi0.checked_add(n - 1)
    } else {
        hi0.checked_sub(n - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOAD: u16 = 0x0801;

    fn img(bytes: &[u8]) -> Vec<u8> {
        let mut v = vec![0u8; 0x400];
        v[..bytes.len()].copy_from_slice(bytes);
        v
    }

    #[test]
    fn a_straight_line_routine_ends_at_its_rts() {
        // SEI / LDA #$55 / STA $90 / CLI / RTS  = 8 bytes at $0801
        let body = img(&[0x78, 0xA9, 0x55, 0x85, 0x90, 0x58, 0x60]);
        assert_eq!(reachable_end(&body, LOAD, &[LOAD]), Some(LOAD + 6));
    }

    #[test]
    fn a_branch_reaches_both_arms() {

        let mut b = vec![0u8; 0x20];
        b[0] = 0xD0;
        b[1] = 0x04;
        b[2] = 0x60;
        b[6] = 0x60;
        let body = img(&b);
        assert_eq!(reachable_end(&body, LOAD, &[LOAD]), Some(LOAD + 6));
    }

    #[test]
    fn a_jsr_is_followed_and_the_caller_continues() {
        // $0801 JSR $0810 ; $0804 RTS ; $0810 NOP NOP RTS
        let mut b = vec![0u8; 0x40];
        b[0] = 0x20;
        b[1] = 0x10;
        b[2] = 0x08;
        b[3] = 0x60;
        b[0x0F] = 0xEA;
        b[0x10] = 0xEA;
        b[0x11] = 0x60;
        let body = img(&b);
        assert_eq!(reachable_end(&body, LOAD, &[LOAD]), Some(LOAD + 0x11));
    }

    #[test]
    fn a_call_out_of_the_image_is_not_a_refusal() {
        // JSR $FFD2 / RTS
        let body = img(&[0x20, 0xD2, 0xFF, 0x60]);
        assert_eq!(reachable_end(&body, LOAD, &[LOAD]), Some(LOAD + 3));
    }

    #[test]
    fn an_absolute_read_of_the_image_extends_the_range() {
        // LDA $0900 / RTS  ; the table at $0900 has to survive
        let body = img(&[0xAD, 0x00, 0x09, 0x60]);
        assert_eq!(reachable_end(&body, LOAD, &[LOAD]), Some(0x0900));
    }

    #[test]
    fn an_indexed_read_of_the_image_reserves_a_whole_page() {

        let body = img(&[0xBD, 0x00, 0x09, 0x60]);
        assert_eq!(reachable_end(&body, LOAD, &[LOAD]), Some(0x09FF));
    }

    #[test]
    fn an_absolute_reference_outside_the_image_is_ignored() {
        // LDA $D012 / RTS ; a VIC read says nothing about the image
        let body = img(&[0xAD, 0x12, 0xD0, 0x60]);
        assert_eq!(reachable_end(&body, LOAD, &[LOAD]), Some(LOAD + 3));
    }

    #[test]
    fn an_indirect_jump_refuses() {

        let body = img(&[0x6C, 0x00, 0x09]);
        assert_eq!(reachable_end(&body, LOAD, &[LOAD]), None);
    }

    #[test]
    fn an_indirect_indexed_load_refuses() {
        // LDA ($FB),Y / RTS ; the pointer is a run-time value
        let body = img(&[0xB1, 0xFB, 0x60]);
        assert_eq!(reachable_end(&body, LOAD, &[LOAD]), None);
    }

    #[test]
    fn an_undocumented_opcode_refuses() {

        let body = img(&[0x02, 0x60]);
        assert_eq!(reachable_end(&body, LOAD, &[LOAD]), None);
    }

    #[test]
    fn a_brk_refuses() {
        let body = img(&[0x00]);
        assert_eq!(reachable_end(&body, LOAD, &[LOAD]), None);

        let body = img(&[0xA9, 0x00, 0xF0, 0x02, 0xEA, 0x00, 0x60]);
        assert_eq!(reachable_end(&body, LOAD, &[LOAD]), None);

        let body = img(&[0xA9, 0x00, 0xF0, 0x02, 0xEA, 0xEA, 0x60]);
        assert_eq!(reachable_end(&body, LOAD, &[LOAD]), Some(LOAD + 6));
    }

    #[test]
    fn the_stable_undocumented_opcodes_are_still_refused() {
        // Instruction lengths and addressing modes for every opcode.
        const MODELLED: [u8; 81] = [
            0x03, 0x04, 0x07, 0x0C, 0x0F, 0x13, 0x14, 0x17, 0x1A, 0x1B, 0x1C, 0x1F, 0x23, 0x27,
            0x2F, 0x33, 0x34, 0x37, 0x3A, 0x3B, 0x3C, 0x3F, 0x43, 0x44, 0x47, 0x4F, 0x53, 0x54,
            0x57, 0x5A, 0x5B, 0x5C, 0x5F, 0x63, 0x64, 0x67, 0x6F, 0x73, 0x74, 0x77, 0x7A, 0x7B,
            0x7C, 0x7F, 0x80, 0x82, 0x83, 0x87, 0x89, 0x8F, 0x97, 0xA3, 0xA7, 0xAB, 0xAF, 0xB3,
            0xB7, 0xBF, 0xC2, 0xC3, 0xC7, 0xCF, 0xD3, 0xD4, 0xD7, 0xDA, 0xDB, 0xDC, 0xDF, 0xE2,
            0xE3, 0xE7, 0xEB, 0xEF, 0xF3, 0xF4, 0xF7, 0xFA, 0xFB, 0xFC, 0xFF,
        ];
        for op in MODELLED {
            assert_eq!(
                decode(op).0,
                Mode::Illegal,
                "${op:02X} is decoded; see the survey above `decode` before landing that"
            );
        }

        let documented = (0u16..=0xFF).filter(|o| decode(*o as u8).0 != Mode::Illegal).count();
        assert_eq!(documented, 151, "the documented NMOS opcode count moved");
    }

    #[test]
    fn the_opcodes_this_tree_cannot_verify_stay_refused() {
        const UNVERIFIABLE: [u8; 12] = [
            0x0B, 0x2B, 0x4B, 0x6B, 0x8B, 0x93, 0x9B, 0x9C, 0x9E, 0x9F, 0xBB, 0xCB,
        ];
        for op in UNVERIFIABLE {
            assert_eq!(decode(op).0, Mode::Illegal, "${op:02X}");
        }

        for op in [0x02, 0x12, 0x22, 0x32, 0x42, 0x52, 0x62, 0x72, 0x92, 0xB2, 0xD2, 0xF2] {
            let (mode, flow) = decode(op);
            assert_eq!(mode, Mode::Illegal, "${op:02X} JAM");
            assert_eq!(flow, Flow::Refuse, "${op:02X} JAM");
        }

        assert_ne!(decode(0xEA).0, Mode::Illegal, "NOP");
        assert_ne!(decode(0x8D).0, Mode::Illegal, "STA abs");
    }

    /// [`decode`] and [`writes`] must be extended together.
    #[test]
    fn writes_and_decode_stay_coupled() {

        const STORES: [(u8, Wr); 43] = [
            // STA/STX/STY abs, and the documented read-modify-writes.
            (0x8D, Wr::One),
            (0x8E, Wr::One),
            (0x8C, Wr::One),
            (0x0E, Wr::One),
            (0x2E, Wr::One),
            (0x4E, Wr::One),
            (0x6E, Wr::One),
            (0xCE, Wr::One),
            (0xEE, Wr::One),
            // abs,X / abs,Y.
            (0x9D, Wr::Page),
            (0x99, Wr::Page),
            (0x1E, Wr::Page),
            (0x3E, Wr::Page),
            (0x5E, Wr::Page),
            (0x7E, Wr::Page),
            (0xDE, Wr::Page),
            (0xFE, Wr::Page),
            // The two indirect stores.
            (0x91, Wr::IndY),
            (0x81, Wr::IndX),
            // SLO/RLA/SRE/RRA/DCP/ISC abs, and SAX abs.
            (0x0F, Wr::One),
            (0x2F, Wr::One),
            (0x4F, Wr::One),
            (0x6F, Wr::One),
            (0x8F, Wr::One),
            (0xCF, Wr::One),
            (0xEF, Wr::One),

            (0x1F, Wr::Page),
            (0x3F, Wr::Page),
            (0x5F, Wr::Page),
            (0x7F, Wr::Page),
            (0xDF, Wr::Page),
            (0xFF, Wr::Page),

            (0x1B, Wr::Page),
            (0x3B, Wr::Page),
            (0x5B, Wr::Page),
            (0x7B, Wr::Page),
            (0xDB, Wr::Page),
            (0xFB, Wr::Page),

            (0x03, Wr::IndX),
            (0x23, Wr::IndX),
            (0x43, Wr::IndX),
            (0x63, Wr::IndX),
            (0x83, Wr::IndX),
        ];

        const MORE: [(u8, Wr); 8] = [
            (0xC3, Wr::IndX),
            (0xE3, Wr::IndX),
            (0x13, Wr::IndY),
            (0x33, Wr::IndY),
            (0x53, Wr::IndY),
            (0x73, Wr::IndY),
            (0xD3, Wr::IndY),
            (0xF3, Wr::IndY),
        ];
        let expect = |op: u8| -> Wr {
            STORES
                .iter()
                .chain(MORE.iter())
                .find(|(o, _)| *o == op)
                .map_or(Wr::None, |(_, w)| *w)
        };

        let mut checked = 0usize;
        let mut stores_checked = 0usize;
        for op in 0u16..=0xFF {
            let op = op as u8;
            // An opcode `decode` refuses never reaches `writes`, because `reachable_instructions` has already returned `None`.
            if decode(op).0 == Mode::Illegal {
                continue;
            }
            checked += 1;
            let want = expect(op);
            if want != Wr::None {
                stores_checked += 1;
            }
            assert_eq!(
                writes(op),
                want,
                "${op:02X}: `decode` accepts it and `writes` disagrees with \
                 the probe. If this is a new decoding, teach `writes` in the \
                 same change."
            );
        }

        assert_eq!(checked, 151, "the documented NMOS opcode count moved");
        assert_eq!(stores_checked, 19, "the documented 16-bit stores moved");

        let undecoded = STORES
            .iter()
            .chain(MORE.iter())
            .filter(|(o, _)| decode(*o).0 == Mode::Illegal)
            .count();
        assert_eq!(undecoded, 32, "an undocumented store became decodable");
    }

    #[test]
    fn a_routine_that_runs_off_the_end_refuses() {
        // A body of pure NOPs: the walk falls off the top with no RTS.
        let body = vec![0xEA; 0x20];
        assert_eq!(reachable_end(&body, LOAD, &[LOAD]), None);
    }

    #[test]
    fn a_branch_that_leaves_the_image_is_not_followed_but_is_not_a_refusal() {

        let body = img(&[0xD0, 0xFD, 0x60]);
        assert_eq!(reachable_end(&body, LOAD, &[LOAD]), Some(LOAD + 2));
    }

    #[test]
    fn a_jump_out_of_the_image_ends_the_path_without_refusing() {
        // JMP $E37B ; hand control to BASIC.
        let body = img(&[0x4C, 0x7B, 0xE3]);
        assert_eq!(reachable_end(&body, LOAD, &[LOAD]), Some(LOAD + 2));
    }

    #[test]
    fn a_tight_loop_terminates() {
        // $0801 JMP $0801 ; visited-set stops it, and there is no RTS, so the answer is just the three bytes of the JMP.
        let body = img(&[0x4C, 0x01, 0x08]);
        assert_eq!(reachable_end(&body, LOAD, &[LOAD]), Some(LOAD + 2));
    }

    #[test]
    fn two_entries_are_both_covered() {
        // $0801 RTS ; $0820 NOP RTS
        let mut b = vec![0u8; 0x40];
        b[0] = 0x60;
        b[0x1F] = 0xEA;
        b[0x20] = 0x60;
        let body = img(&b);
        assert_eq!(
            reachable_end(&body, LOAD, &[LOAD, LOAD + 0x1F]),
            Some(LOAD + 0x20)
        );
    }

    #[test]
    fn an_entry_outside_the_image_refuses() {
        let body = img(&[0x60]);
        assert_eq!(reachable_end(&body, LOAD, &[0xC000]), None);
    }

    #[test]
    fn cmp_absolute_y_is_a_documented_instruction() {
        // CMP $0A00,Y / RTS
        let body = img(&[0xD9, 0x00, 0x0A, 0x60]);
        assert_eq!(
            reachable_span_strict(&body, LOAD, &[LOAD]),
            Some((LOAD, 0x0AFF))
        );
        assert_eq!(reachable_end(&body, LOAD, &[LOAD]), Some(0x0AFF));
    }

    #[test]
    fn sbc_absolute_y_is_a_documented_instruction() {
        // SBC $0A00,Y / RTS
        let body = img(&[0xF9, 0x00, 0x0A, 0x60]);
        assert_eq!(
            reachable_span_strict(&body, LOAD, &[LOAD]),
            Some((LOAD, 0x0AFF))
        );
    }

    #[test]
    fn the_neighbours_of_the_two_fixed_opcodes_are_still_illegal() {
        assert_eq!(reachable_end(&img(&[0xDA, 0x60]), LOAD, &[LOAD]), None);
        assert_eq!(reachable_end(&img(&[0xFA, 0x60]), LOAD, &[LOAD]), None);
    }

    fn diag(body: &[u8], entries: &[u16]) -> ReachDiag {
        diagnose(body, LOAD, entries).expect("there is something to walk")
    }

    #[test]
    fn a_clean_routine_has_no_refusals_and_agrees_with_the_shipping_scan() {
        // LDA $0900,X / JSR $0820 / RTS ; $0820 STA $0A00 / RTS
        let mut b = vec![0u8; 0x400];
        b[0..3].copy_from_slice(&[0xBD, 0x00, 0x09]);
        b[3..6].copy_from_slice(&[0x20, 0x20, 0x08]);
        b[6] = 0x60;
        b[0x1F..0x22].copy_from_slice(&[0x8D, 0x00, 0x0A]);
        b[0x22] = 0x60;
        let d = diag(&b, &[LOAD]);
        assert_eq!(d.refusals, vec![]);
        assert_eq!(
            (d.low, d.high),
            reachable_span(&b, LOAD, &[LOAD]).expect("clean code")
        );

        assert_eq!((d.low, d.high), (LOAD, 0x0A00));
        assert_eq!(d.instructions, 5);
    }

    #[test]
    fn the_optimistic_low_agrees_with_the_shipping_scan() {

        let mut b = vec![0u8; 0x400];
        b[0x1FF..0x202].copy_from_slice(&[0xAD, 0x50, 0x08]);
        b[0x202] = 0x60;
        let d = diag(&b, &[0x0A00]);
        assert_eq!(d.refusals, vec![]);
        assert_eq!(
            (d.low, d.high),
            reachable_span(&b, LOAD, &[0x0A00]).expect("clean code")
        );
        assert_eq!(d.low, 0x0850);
    }

    #[test]
    fn an_illegal_opcode_is_named_with_its_address() {

        let d = diag(&img(&[0xEA, 0x02, 0x60]), &[LOAD]);
        assert_eq!(d.refusals.len(), 1);
        let r = d.refusals[0];
        assert_eq!(r.cause, RefusalCause::IllegalOpcode);
        assert_eq!((r.addr, r.opcode), (LOAD + 1, 0x02));
        assert_eq!(r.from_entries, 1);

        assert_eq!(reachable_end(&img(&[0xEA, 0x02, 0x60]), LOAD, &[LOAD]), None);
    }

    #[test]
    fn brk_and_an_indirect_jump_are_told_apart() {
        assert_eq!(diag(&img(&[0x00]), &[LOAD]).refusals[0].cause, RefusalCause::Brk);
        assert_eq!(
            diag(&img(&[0x6C, 0x00, 0x09]), &[LOAD]).refusals[0].cause,
            RefusalCause::JmpIndirect
        );
    }

    #[test]
    fn a_routine_that_runs_off_the_end_is_named_at_its_last_instruction() {
        let body = vec![0xEA; 0x20];
        let d = diag(&body, &[LOAD]);
        assert_eq!(d.refusals.len(), 1);
        assert_eq!(d.refusals[0].cause, RefusalCause::RanOffEnd);
        assert_eq!(d.refusals[0].addr, LOAD + 0x1F);
    }

    #[test]
    fn an_indirect_is_stepped_over_and_the_rest_of_the_routine_still_counts() {
        // LDA ($FB),Y / STA $0A00 / RTS
        let d = diag(&img(&[0xB1, 0xFB, 0x8D, 0x00, 0x0A, 0x60]), &[LOAD]);
        assert_eq!(d.refusals.len(), 1);
        assert_eq!(d.refusals[0].cause, RefusalCause::IndirectIndexedY);
        assert_eq!(d.refusals[0].zp, Some(0xFB));
        assert_eq!(d.high, 0x0A00, "the store past the indirect was counted");
        assert_eq!(d.instructions, 3);
    }

    #[test]
    fn an_indirect_through_a_pointer_the_walk_built_is_resolvable() {
        let body = img(&[
            0xA9, 0x00, 0x85, 0xFB, // LDA #$00 / STA $FB
            0xA9, 0x40, 0x85, 0xFC, // LDA #$40 / STA $FC
            0xB1, 0xFB,
            0x60,
        ]);
        let r = diag(&body, &[LOAD]).refusals[0];
        assert_eq!(r.cause, RefusalCause::IndirectIndexedY);
        assert!(r.zp_from_walk, "both halves come from visited immediates");
        assert!(r.zp_linear);
    }

    #[test]
    fn an_indirect_through_an_unset_pointer_is_not_resolvable() {
        let r = diag(&img(&[0xB1, 0xFB, 0x60]), &[LOAD]).refusals[0];
        assert!(!r.zp_from_walk);
        assert!(!r.zp_linear);
    }

    #[test]
    fn a_pointer_forged_out_of_bytes_the_walk_never_decoded_resolves_only_loosely() {
        let body = img(&[
            0x4C, 0x10, 0x08,
            0xA9, 0x40, 0x85, 0xFC, 0xA9, 0x00, 0x85, 0xFB,
            0xEA, 0xEA, 0xEA, 0xEA,
            0xB1, 0xFB,
            0x60,
        ]);
        let r = diag(&body, &[LOAD]).refusals[0];
        assert!(r.zp_linear, "the blind scan is fooled by the operand bytes");
        assert!(
            !r.zp_from_walk,
            "no VISITED instruction stores to the pointer"
        );
    }

    /// `($nn,X)` picks the pointer at run time, so it is reported unresolvable whatever zero page holds.
    #[test]
    fn an_indirect_x_is_never_resolvable() {
        let body = img(&[
            0xA9, 0x00, 0x85, 0xFB, 0xA9, 0x40, 0x85, 0xFC,
            0xA1, 0xFB,
            0x60,
        ]);
        let r = diag(&body, &[LOAD]).refusals[0];
        assert_eq!(r.cause, RefusalCause::IndirectIndexedX);
        assert_eq!(r.zp, Some(0xFB));
        assert!(!r.zp_from_walk);
        assert!(!r.zp_linear);
    }

    #[test]
    fn a_pointer_stored_twice_with_different_values_is_not_resolvable() {
        let body = img(&[
            0xA9, 0x00, 0x85, 0xFB, 0xA9, 0x40, 0x85, 0xFC,
            0xA9, 0x50, 0x85, 0xFC,
            0xB1, 0xFB, 0x60,
        ]);
        let r = diag(&body, &[LOAD]).refusals[0];
        assert!(!r.zp_from_walk);
    }

    #[test]
    fn a_refusal_behind_one_entry_of_two_is_counted_once() {
        let mut b = vec![0u8; 0x40];
        b[0] = 0x60; // $0801 RTS ; clean entry
        b[0x10] = 0x02;
        let d = diag(&img(&b), &[LOAD, LOAD + 0x10]);
        assert_eq!(d.refusals.len(), 1);
        assert_eq!(d.refusals[0].from_entries, 1);
    }

    #[test]
    fn a_refusal_both_entries_reach_is_counted_twice() {
        // $0801 JMP $0820 ; $0810 JMP $0820 ; $0820 JAM
        let mut b = vec![0u8; 0x40];
        b[0..3].copy_from_slice(&[0x4C, 0x20, 0x08]);
        b[0x0F..0x12].copy_from_slice(&[0x4C, 0x20, 0x08]);
        b[0x1F] = 0x02;
        let d = diag(&img(&b), &[LOAD, LOAD + 0x0F]);
        assert_eq!(d.refusals.len(), 1);
        assert_eq!(d.refusals[0].from_entries, 2);
    }

    #[test]
    fn an_entry_outside_the_image_is_named_as_such() {
        let d = diag(&img(&[0x60]), &[0xC000]);
        assert_eq!(d.refusals.len(), 1);
        assert_eq!(d.refusals[0].cause, RefusalCause::EntryOutside);
        assert_eq!(d.refusals[0].addr, 0xC000);
    }

    /// A `NOP` sled long enough to exhaust [`BUDGET`].
    #[test]
    fn exhausting_the_budget_is_its_own_cause() {

        let body = vec![0xEA; BUDGET + 0x100];
        let d = diag(&body, &[LOAD]);
        assert!(d.refusals.iter().any(|r| r.cause == RefusalCause::Budget));
        // The budget is checked after the address is marked visited, in both walks, so the last instruction decoded is the one the budget refuses on.
        assert_eq!(d.instructions, BUDGET + 1);
        assert_eq!(reachable_end(&body, LOAD, &[LOAD]), None);
    }

    #[test]
    fn diagnose_refuses_exactly_when_the_shipping_scan_does() {
        let cases: Vec<(Vec<u8>, Vec<u16>)> = vec![
            (img(&[0x78, 0xA9, 0x55, 0x85, 0x90, 0x58, 0x60]), vec![LOAD]),
            (img(&[0x20, 0xD2, 0xFF, 0x60]), vec![LOAD]),
            (img(&[0xAD, 0x00, 0x09, 0x60]), vec![LOAD]),
            (img(&[0xBD, 0x00, 0x09, 0x60]), vec![LOAD]),
            (img(&[0xAD, 0x12, 0xD0, 0x60]), vec![LOAD]),
            (img(&[0xD0, 0xFD, 0x60]), vec![LOAD]),
            (img(&[0x4C, 0x7B, 0xE3]), vec![LOAD]),
            (img(&[0x4C, 0x01, 0x08]), vec![LOAD]),

            (img(&[0x6C, 0x00, 0x09]), vec![LOAD]),
            (img(&[0xB1, 0xFB, 0x60]), vec![LOAD]),
            (img(&[0x81, 0xFB, 0x60]), vec![LOAD]),
            (img(&[0x02, 0x60]), vec![LOAD]),
            (img(&[0x00]), vec![LOAD]),
            (vec![0xEA; 0x20], vec![LOAD]),
            (img(&[0x60]), vec![0xC000]),
        ];
        for (body, entries) in cases {
            let scan = reachable_span(&body, LOAD, &entries);
            let d = diagnose(&body, LOAD, &entries).expect("something to walk");
            assert_eq!(
                scan.is_none(),
                !d.refusals.is_empty(),
                "disagreement at {:02X?} entries {entries:04X?}: scan={scan:?} refusals={:?}",
                &body[..body.len().min(8)],
                d.refusals
            );
            if let Some(span) = scan {
                assert_eq!(span, (d.low, d.high), "span drift on an accepted fixture");
            }
        }
    }

    fn extent(body: &[u8]) -> Option<Vec<(u16, u16)>> {
        write_extent(body, LOAD, &[LOAD])
    }

    #[test]
    fn an_absolute_store_is_one_byte() {
        // LDA #$00 / STA $4000 / RTS
        assert_eq!(
            extent(&img(&[0xA9, 0x00, 0x8D, 0x00, 0x40, 0x60])),
            Some(vec![(0x4000, 0x4000)])
        );
    }

    #[test]
    fn an_indexed_store_covers_a_page() {
        // STA $4000,X / RTS ; X is a run-time value.
        assert_eq!(
            extent(&img(&[0x9D, 0x00, 0x40, 0x60])),
            Some(vec![(0x4000, 0x40FF)])
        );
    }

    #[test]
    fn a_read_modify_write_is_a_store() {

        assert_eq!(
            extent(&img(&[0xEE, 0x00, 0x40, 0x60])),
            Some(vec![(0x4000, 0x4000)])
        );
    }

    #[test]
    fn a_routine_that_writes_nothing_yields_an_empty_extent() {
        // LDA $4000 / CMP #$05 / BNE +0 / RTS ; reads only.
        assert_eq!(
            extent(&img(&[0xAD, 0x00, 0x40, 0xC9, 0x05, 0xD0, 0x00, 0x60])),
            Some(vec![])
        );
    }

    #[test]
    fn zero_page_stores_are_not_reserved() {
        // LDA #$FF / STA $FB / STX $FC / RTS
        assert_eq!(
            extent(&img(&[0xA9, 0xFF, 0x85, 0xFB, 0x86, 0xFC, 0x60])),
            Some(vec![])
        );
    }

    /// Follow a machine-code routine embedded in BASIC text.
    #[test]
    fn descending_page_copy_keeps_all_reachable_bytes() {
        let body = img(&[
            0xA9, 0xFF,
            0x85, 0xFD, // STA $FD      dest lo
            0x85, 0xFB, // STA $FB      src  lo
            0xA9, 0x3F,
            0x85, 0xFE, // STA $FE      dest hi  -> pointer = $3FFF
            0xA9, 0xDF,
            0x85, 0xFC, // STA $FC      src  hi
            0xA4, 0x10,
            0xB1, 0xFB,
            0x91, 0xFD,
            0x88,
            0xD0, 0xF9,
            0xC6, 0xFE,
            0xC6, 0xFC,
            0xA5, 0xFE,
            0xC9, 0x2E, // CMP #$2E     stop when the high byte hits $2E
            0xD0, 0xED,
            0xA9, 0x0E, 0x8D, 0x28, 0x03,
            0xA9, 0x08, 0x8D, 0x29, 0x03,
            0x60,
        ]);
        let mut got = extent(&body).expect("the routine is fully accountable");
        got.sort_unstable();
        assert_eq!(got, vec![(0x0328, 0x0328), (0x0329, 0x0329), (0x2FFF, 0x40FE)]);
    }

    /// The ascending sibling: a page count in `X` and a `DEX`, which is the canonical charset copy `note_ml_copy_destination` matches in the DATA pool.
    #[test]
    fn an_ascending_page_copy_counted_in_x() {
        let body = img(&[
            0xA9, 0x00,
            0x85, 0xFD,
            0xA9, 0x30,
            0x85, 0xFE, // STA $FE      pointer = $3000
            0xA2, 0x10, // LDX #$10     sixteen pages
            0xA0, 0x00,
            0x91, 0xFD,
            0xC8,
            0xD0, 0xFB,
            0xE6, 0xFE,
            0xCA,
            0xD0, 0xF6,
            0x60,
        ]);
        assert_eq!(extent(&body), Some(vec![(0x3000, 0x3FFF)]));
    }

    /// A pointer that is never walked fills one pointer-width run.
    #[test]
    fn a_fixed_pointer_writes_one_run_from_the_pointer() {
        // pointer = $4080, STA ($FD),Y once, RTS.
        let body = img(&[
            0xA9, 0x80, 0x85, 0xFD, 0xA9, 0x40, 0x85, 0xFE, 0x91, 0xFD, 0x60,
        ]);
        assert_eq!(extent(&body), Some(vec![(0x4080, 0x417F)]));
    }

    #[test]
    fn an_unresolvable_pointer_refuses() {
        // STA ($FB),Y with nothing having set $FB/$FC.
        assert_eq!(extent(&img(&[0x91, 0xFB, 0x60])), None);
    }

    #[test]
    fn a_store_through_a_run_time_pointer_refuses() {
        // STA ($FB,X) ; X selects which pointer, so not even the pointer is knowable.
        let body = img(&[0xA9, 0x00, 0x85, 0xFB, 0xA9, 0x40, 0x85, 0xFC, 0x81, 0xFB, 0x60]);
        assert_eq!(extent(&body), None);
    }

    #[test]
    fn an_unbounded_page_walk_refuses() {

        let body = img(&[
            0xA9, 0x00, 0x85, 0xFD, 0xA9, 0x30, 0x85, 0xFE,
            0x91, 0xFD,
            0xE6, 0xFE,
            0xD0, 0xFA,
            0x60,
        ]);
        assert_eq!(extent(&body), None);
    }

    #[test]
    fn a_pointer_walked_both_ways_refuses() {
        let body = img(&[
            0xA9, 0x00, 0x85, 0xFD, 0xA9, 0x30, 0x85, 0xFE, 0x91, 0xFD,
            0xE6, 0xFE,
            0xC6, 0xFE,
            0xA5, 0xFE, 0xC9, 0x40, 0xD0, 0xF4, 0x60,
        ]);
        assert_eq!(extent(&body), None);
    }

    #[test]
    fn a_call_to_chrout_refuses() {
        // JSR $FFD2 writes the screen, wherever the program has put it.
        assert_eq!(extent(&img(&[0x20, 0xD2, 0xFF, 0x60])), None);
    }

    #[test]
    fn a_call_to_getin_is_not_a_refusal() {
        // JSR $FFE4 / STA $0429 / RTS
        assert_eq!(
            extent(&img(&[0x20, 0xE4, 0xFF, 0x8D, 0x29, 0x04, 0x60])),
            Some(vec![(0x0429, 0x0429)])
        );
    }

    #[test]
    fn reachable_end_still_refuses_on_indirect_indexed() {
        assert_eq!(reachable_end(&img(&[0xB1, 0xFB, 0x60]), LOAD, &[LOAD]), None);
        assert_eq!(reachable_end(&img(&[0x81, 0xFB, 0x60]), LOAD, &[LOAD]), None);
    }

    #[test]
    fn a_branch_that_leaves_the_image_refuses() {

        assert_eq!(extent(&img(&[0xD0, 0xFD, 0x60])), None);

        assert_eq!(
            reachable_end(&img(&[0xD0, 0xFD, 0x60]), LOAD, &[LOAD]),
            Some(LOAD + 2)
        );
    }

    #[test]
    fn one_bad_entry_refuses_the_whole_analysis() {
        let mut b = vec![0u8; 0x40];
        b[0..4].copy_from_slice(&[0x8D, 0x00, 0x40, 0x60]);
        b[0x10..0x13].copy_from_slice(&[0x20, 0xD2, 0xFF]);
        b[0x13] = 0x60;
        let body = img(&b);
        assert_eq!(write_extent(&body, LOAD, &[LOAD]), Some(vec![(0x4000, 0x4000)]));
        assert_eq!(write_extent(&body, LOAD, &[LOAD, LOAD + 0x10]), None);
    }
}
