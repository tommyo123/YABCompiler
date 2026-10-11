//! Assembly-level peephole optimisation pass.

use std::collections::BTreeMap;

use crate::{codegen::Profile, runtime as rt};

mod structured;
mod zp_stores;
mod short_pairs;

#[derive(Debug, Clone)]
pub enum Item {
    /// `*=$080D` style origin or other top-level pseudo.
    Origin(String),
    /// A standalone label line `Lxx:` or `__HELPER:`.
    Label(String),
    /// An indented instruction line.
    Insn(Insn),
    /// `.byte`, `.word`, etc. Treated as opaque data — barrier.
    Directive(String),
    /// `; ...` comment line (full line; in-line comments live on Insn).
    Comment(String),
    /// Blank line.
    Blank,
}

#[derive(Debug, Clone)]
pub struct Insn {
    pub mnem: String,
    pub operand: Option<String>,
    /// Trailing `; ...` comment, without the leading `;`.
    pub comment: Option<String>,
}

impl Insn {
    fn render(&self, out: &mut String) {
        out.push_str("    ");
        out.push_str(&self.mnem);
        if let Some(op) = &self.operand {
            out.push(' ');
            out.push_str(op);
        }
        if let Some(c) = &self.comment {
            out.push_str(" ; ");
            out.push_str(c);
        }
        out.push('\n');
    }
}

// ----- Parser ---------------------------------------------------------------

pub fn parse(asm: &str) -> Vec<Item> {
    let mut out = Vec::with_capacity(asm.lines().count());
    for raw in asm.lines() {
        out.push(parse_line(raw));
    }
    out
}

fn parse_line(raw: &str) -> Item {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Item::Blank;
    }
    if trimmed.starts_with(';') {
        return Item::Comment(raw.to_string());
    }
    if trimmed.starts_with("*=") {
        return Item::Origin(raw.to_string());
    }
    if trimmed.starts_with('.') {
        return Item::Directive(raw.to_string());
    }
    // Constant assignment: `LABEL = $XX` style (DASM zero-page
    // promotion declarations). The first whitespace-delimited word
    // is the label, followed by `=` and an expression. Treat as a
    // directive so the renderer preserves it verbatim — instructions
    // get a 4-space indent that breaks DASM's parsing of these.
    if let Some(eq_pos) = trimmed.find('=') {
        let lhs = trimmed[..eq_pos].trim_end();
        if !lhs.is_empty()
            && !lhs.contains(char::is_whitespace)
            && lhs.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return Item::Directive(raw.to_string());
        }
    }
    // Label line: ends with ':' and has no whitespace before the
    // colon (otherwise it's an indented instruction with a colon in
    // a comment).
    if let Some(stripped) = trimmed.strip_suffix(':') {
        if !stripped.contains(char::is_whitespace) && !stripped.is_empty() {
            return Item::Label(stripped.to_string());
        }
    }
    // Instruction: split off any trailing ` ; comment`. We only
    // consider `;` as comment when preceded by whitespace, since
    // operands like `LDA #$3B` never contain semicolons.
    let (body, comment) = match trimmed.find(" ;") {
        Some(idx) => (
            trimmed[..idx].trim_end().to_string(),
            Some(trimmed[idx + 2..].trim().to_string()),
        ),
        None => (trimmed.to_string(), None),
    };
    let (mnem, operand) = match body.find(char::is_whitespace) {
        Some(idx) => (
            body[..idx].to_string(),
            Some(body[idx..].trim().to_string()),
        ),
        None => (body, None),
    };
    Item::Insn(Insn {
        mnem,
        operand,
        comment,
    })
}

// ----- Renderer -------------------------------------------------------------

pub fn render(items: &[Item]) -> String {
    let mut out = String::new();
    for it in items {
        match it {
            Item::Origin(s) | Item::Comment(s) | Item::Directive(s) => {
                out.push_str(s);
                out.push('\n');
            }
            Item::Label(name) => {
                out.push_str(name);
                out.push_str(":\n");
            }
            Item::Insn(i) => i.render(&mut out),
            Item::Blank => out.push('\n'),
        }
    }
    out
}

// ----- Helpers shared across rules -----------------------------------------

/// Step forward from `i` skipping blanks and full-line comments,
/// returning the index of the next "structural" item (label, insn,
/// directive) or `None` if we run off the end.
fn next_struct(items: &[Item], mut i: usize) -> Option<usize> {
    i += 1;
    while i < items.len() {
        match &items[i] {
            Item::Blank | Item::Comment(_) => i += 1,
            _ => return Some(i),
        }
    }
    None
}

/// The two comments `codegen::emit_sys` leaves around a `SYS`'s own `JSR`.
const SYS_WRAP_MARKERS: [&str; 2] = ["; YAB_SYS_WRAP_IN", "; YAB_SYS_WRAP_OUT"];

fn is_sys_wrap_marker(it: &Item) -> bool {
    matches!(it, Item::Comment(c) if SYS_WRAP_MARKERS.contains(&c.trim()))
}

/// Whether a SYS bank-switch marker separates two instructions.
fn crosses_sys_wrap(items: &[Item], i: usize, j: usize) -> bool {
    items[i + 1..j].iter().any(is_sys_wrap_marker)
}

fn prev_struct(items: &[Item], i: usize) -> Option<usize> {
    if i == 0 {
        return None;
    }
    let mut j = i - 1;
    loop {
        match &items[j] {
            Item::Blank | Item::Comment(_) => {
                if j == 0 {
                    return None;
                }
                j -= 1;
            }
            _ => return Some(j),
        }
    }
}

/// Predicate: does this mnemonic always end the basic block?
fn is_uncond_terminator(mnem: &str) -> bool {
    matches!(mnem, "JMP" | "RTS" | "RTI" | "BRK")
}

/// Predicate: is this a conditional branch?
fn is_cond_branch(mnem: &str) -> bool {
    matches!(
        mnem,
        "BEQ" | "BNE" | "BCC" | "BCS" | "BMI" | "BPL" | "BVC" | "BVS"
    )
}

/// True iff `target` is a label name (vs. an absolute `$XXXX`
/// literal). 6502 conditional branches are 8-bit PC-relative —
/// they can only reach labels in the same translation unit, never
/// distant ROM addresses. Used by PH004/PH005 to refuse to rewrite
/// a branch's target to an absolute address.
fn is_label_target(target: &str) -> bool {
    !target.starts_with('$')
        && !target.starts_with('#')
        && !target.starts_with('(')
        && target
            .chars()
            .next()
            .map(|c| c.is_ascii_alphabetic() || c == '_')
            .unwrap_or(false)
}

/// The `NAME = $XX` zero-page equates codegen emits at the top of the stream (see the
/// "zero-page promotions" block in `codegen.rs`).
#[derive(Debug, Default)]
struct ZpEquates {
    /// Equate names resolved to zero-page addresses.
    map: std::collections::HashMap<String, u16>,
}

impl ZpEquates {
    /// Collect every `NAME = <value>` directive whose value lands in zero page.
    fn scan(items: &[Item]) -> Self {
        let mut map = std::collections::HashMap::new();
        for it in items {
            let Item::Directive(raw) = it else { continue };
            let trimmed = raw.trim();
            let Some(eq) = trimmed.find('=') else { continue };
            let lhs = trimmed[..eq].trim();

            if lhs.is_empty()
                || lhs.contains(char::is_whitespace)
                || !lhs.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                continue;
            }
            let rhs = trimmed[eq + 1..].trim();
            // Ignore a trailing comment.
            let rhs = rhs.split(';').next().unwrap_or("").trim();
            if let Some(v) = parse_plain_addr(rhs)
                && v < 0x0100
            {
                map.insert(lhs.to_string(), v);
            }
        }
        ZpEquates { map }
    }

    /// True when `operand` names a zero-page equate and therefore assembles to the 2-byte form.
    fn is_zp_operand(&self, op: &str) -> bool {
        if self.map.is_empty() {
            return false;
        }
        let op = op.trim();
        if op.starts_with('#') || op.starts_with('$') || op.starts_with('(') || op.contains(',') {
            return false;
        }
        match op.find(['+', '-']) {
            None => self.map.contains_key(op),
            Some(p) => {
                let base = op[..p].trim();
                let Some(&addr) = self.map.get(base) else {
                    return false;
                };
                let Some(off) = parse_plain_addr(op[p + 1..].trim()) else {
                    return false;
                };
                let resolved = if op.as_bytes()[p] == b'+' {
                    addr.checked_add(off)
                } else {
                    addr.checked_sub(off)
                };
                resolved.is_some_and(|v| v < 0x0100)
            }
        }
    }
}

/// Parse a bare `$XX`/`$XXXX` hex or decimal literal.
fn parse_plain_addr(s: &str) -> Option<u16> {
    match s.strip_prefix('$') {
        Some(hex) if !hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit()) => {
            u16::from_str_radix(hex, 16).ok()
        }
        Some(_) => None,
        None if !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()) => s.parse::<u16>().ok(),
        None => None,
    }
}

/// Rough byte-cost estimate for an instruction. Used by PH005 to
/// guard against rewrites that would push a conditional branch out
/// of 8-bit reach, where the assembler's long-branch fix-up would
/// expand the code by more bytes than the rewrite saves.
fn approx_insn_bytes(insn: &Insn, zp: &ZpEquates) -> usize {
    match insn.mnem.as_str() {
        // Implied: 1 byte.
        "RTS" | "RTI" | "BRK" | "NOP" | "TAX" | "TAY" | "TXA" | "TYA" | "TSX" | "TXS" | "PHA"
        | "PLA" | "PHP" | "PLP" | "INX" | "INY" | "DEX" | "DEY" | "CLC" | "SEC" | "CLI" | "SEI"
        | "CLD" | "SED" | "CLV" => 1,
        // Branches: 2 bytes.
        "BEQ" | "BNE" | "BCC" | "BCS" | "BMI" | "BPL" | "BVC" | "BVS" => 2,
        // JSR/JMP: 3 bytes (absolute).
        "JSR" | "JMP" => 3,
        // Everything else (loads/stores/arith) — guess based on operand.
        _ => match insn.operand.as_deref() {
            None => 1,
            Some(op) if op.starts_with('#') => 2, // immediate
            Some(op) if op.starts_with("$") && op.len() <= 3 => 2, // zp ($XX)
            Some(op) if op.starts_with("(") => 2, // indirect
// A named operand that resolves into zero page assembles to the 2-byte form, not
// the 3-byte absolute one.
            Some(op) if zp.is_zp_operand(op) => 2,
            Some(_) => 3, // absolute
        },
    }
}

/// Search forward and backward from `start_idx` for `target_label`.
/// Returns the absolute byte distance if found within `max_bytes`,
/// or None otherwise. Used by PH005 to make sure the inverted
/// branch is still in 8-bit reach.
fn label_within_branch_range(
    items: &[Item],
    start_idx: usize,
    target_label: &str,
    max_bytes: usize,
    zp: &ZpEquates,
) -> bool {
    // Forward.
    let mut bytes = 0usize;
    for it in &items[start_idx..] {
        match it {
            Item::Label(name) if name == target_label => return true,
            Item::Insn(insn) => {
                bytes += approx_insn_bytes(insn, zp);
                if bytes > max_bytes {
                    break;
                }
            }
            Item::Directive(_) => break,
            _ => {}
        }
    }
    // Backward.
    bytes = 0;
    for it in items[..start_idx].iter().rev() {
        match it {
            Item::Label(name) if name == target_label => return true,
            Item::Insn(insn) => {
                bytes += approx_insn_bytes(insn, zp);
                if bytes > max_bytes {
                    return false;
                }
            }
            Item::Directive(_) => return false,
            _ => {}
        }
    }
    false
}

/// Invert a branch over JMP and let assembly check its final reach.
fn ph329_branch_straight_to_the_jmp_target(items: &mut Vec<Item>) -> bool {
    let labels: std::collections::HashSet<String> = items
        .iter()
        .filter_map(|it| match it {
            Item::Label(name) => Some(name.clone()),
            _ => None,
        })
        .collect();
    let mut rewrites: Vec<(usize, &'static str, String, usize)> = Vec::new();
    for i in 0..items.len() {
        let Item::Insn(branch) = &items[i] else {
            continue;
        };
        let Some(inverted) = invert_branch(&branch.mnem) else {
            continue;
        };
        let Some(over) = branch.operand.as_deref() else {
            continue;
        };
        let Some(j) = next_struct(items, i) else {
            continue;
        };
        let Item::Insn(jmp) = &items[j] else {
            continue;
        };
        let Some(target) = jmp.operand.as_deref() else {
            continue;
        };
        if jmp.mnem != "JMP" || !is_label_target(target) || !labels.contains(target) {
            continue;
        }
        let mut k = j;
        let mut lands_after_jmp = false;
        while let Some(n) = next_struct(items, k) {
            let Item::Label(name) = &items[n] else {
                break;
            };
            if name == over {
                lands_after_jmp = true;
                break;
            }
            k = n;
        }
        if lands_after_jmp {
            rewrites.push((i, inverted, target.to_string(), j));
        }
    }
    for &(i, inverted, ref target, j) in rewrites.iter().rev() {
        items.remove(j);
        if let Item::Insn(branch) = &mut items[i] {
            branch.mnem = inverted.to_string();
            branch.operand = Some(target.clone());
        }
    }
    !rewrites.is_empty()
}

/// Remove zero additions or subtractions when carry and flags permit it.
fn ph330_drop_add_of_zero_with_known_carry(items: &mut Vec<Item>) -> bool {
    if items
        .iter()
        .any(|it| matches!(it, Item::Insn(i) if i.mnem == "PHP"))
    {
        return false;
    }
    let candidate = |insn: &Insn| {
        matches!(insn.mnem.as_str(), "ADC" | "SBC")
            && insn.operand.as_deref().map(str::trim) == Some("#$00")
    };
    if !items
        .iter()
        .any(|it| matches!(it, Item::Insn(i) if candidate(i)))
    {
        return false;
    }
    // Neither reads nor writes C.
    const CARRY_NEUTRAL: [&str; 22] = [
        "LDA", "LDX", "LDY", "STA", "STX", "STY", "TAX", "TAY", "TXA", "TYA", "AND", "ORA",
        "EOR", "INC", "DEC", "INX", "INY", "DEX", "DEY", "BIT", "NOP", "CLV",
    ];
    let flags = analyze_flag_liveness_with_config(items, &FlagLivenessConfig::default());
    let mut to_drop = Vec::new();
    for (idx, it) in items.iter().enumerate() {
        let Item::Insn(insn) = it else { continue };
        if !candidate(insn) {
            continue;
        }
        let wants = if insn.mnem == "ADC" { "CLC" } else { "SEC" };
        let mut carry_known = false;
        let mut k = idx;
        while let Some(p) = prev_struct(items, k) {
            let Item::Insn(prev) = &items[p] else {
                break;
            };
            if prev.mnem == wants {
                carry_known = true;
                break;
            }
            if !CARRY_NEUTRAL.contains(&prev.mnem.as_str()) {
                break;
            }
            k = p;
        }
        if !carry_known
            || flags.is_live_out(idx, Flag::N)
            || flags.is_live_out(idx, Flag::Z)
            || flags.is_live_out(idx, Flag::V)
        {
            continue;
        }
        to_drop.push(idx);
    }
    for &idx in to_drop.iter().rev() {
        items.remove(idx);
    }
    !to_drop.is_empty()
}

/// Give shared zero page cells one name.
fn canonicalize_zp_aliases(items: &mut [Item]) -> bool {
    use std::collections::HashMap;
    let kind = |name: &str| name.find('_').map(|i| name[..=i].to_string());
    let mut first: HashMap<String, String> = HashMap::new();
    let mut mixed: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut alias: HashMap<String, String> = HashMap::new();
    let mut equates: Vec<(String, String)> = Vec::new();
    for it in items.iter() {
        let Item::Directive(raw) = it else { continue };
        let Some((name, value)) = raw.split_once('=') else { continue };
        let (name, value) = (name.trim(), value.trim());
        if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            || !value.starts_with('$')
            || value.len() > 3
        {
            continue;
        }
        equates.push((name.to_string(), value.to_string()));
    }
    for (name, addr) in &equates {
        match first.get(addr) {
            None => {
                first.insert(addr.clone(), name.clone());
            }
            Some(f) => {
                if kind(f) != kind(name) {
                    mixed.insert(addr.clone());
                }
            }
        }
    }
    for (name, addr) in &equates {
        if mixed.contains(addr) {
            continue;
        }
        let f = &first[addr];
        if f != name {
            alias.insert(name.clone(), f.clone());
        }
    }
    if alias.is_empty() {
        return false;
    }
    let mut changed = false;
    for it in items.iter_mut() {
        let Item::Insn(insn) = it else { continue };
        let Some(op) = insn.operand.as_mut() else { continue };
        let mut out = String::with_capacity(op.len());
        let bytes = op.as_bytes();
        let mut i = 0;
        let mut touched = false;
        while i < bytes.len() {
            let c = bytes[i] as char;
            if (c.is_ascii_alphabetic() || c == '_') && (i == 0 || bytes[i - 1] != b'$') {
                let start = i;
                while i < bytes.len() && ((bytes[i] as char).is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                let tok = &op[start..i];
                match alias.get(tok) {
                    Some(canon) => {
                        out.push_str(canon);
                        touched = true;
                    }
                    None => out.push_str(tok),
                }
            } else {
                out.push(c);
                i += 1;
            }
        }
        if touched {
            *op = out;
            changed = true;
        }
    }
    changed
}

/// Invert a conditional branch mnemonic. Used by PH005.
fn invert_branch(mnem: &str) -> Option<&'static str> {
    Some(match mnem {
        "BEQ" => "BNE",
        "BNE" => "BEQ",
        "BCC" => "BCS",
        "BCS" => "BCC",
        "BMI" => "BPL",
        "BPL" => "BMI",
        "BVC" => "BVS",
        "BVS" => "BVC",
        _ => return None,
    })
}

// ----- Register liveness ---------------------------------------------------

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Reg {
    A,
    X,
    Y,
}

#[allow(dead_code)]
impl Reg {
    fn bit(self) -> u8 {
        match self {
            Reg::A => 0b001,
            Reg::X => 0b010,
            Reg::Y => 0b100,
        }
    }
}

/// Bitset for the 6502 general registers we track. Flags and stack
/// state are intentionally out of scope; this is meant as a stable
/// foundation for register-aware codegen/peephole decisions.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RegSet(u8);

#[allow(dead_code)]
impl RegSet {
    pub const EMPTY: Self = Self(0);
    pub const A: Self = Self(0b001);
    pub const X: Self = Self(0b010);
    pub const Y: Self = Self(0b100);
    pub const ALL: Self = Self(0b111);

    pub fn from_regs(regs: &[Reg]) -> Self {
        let mut out = Self::EMPTY;
        for &reg in regs {
            out.insert(reg);
        }
        out
    }

    pub fn contains(self, reg: Reg) -> bool {
        self.0 & reg.bit() != 0
    }

    pub fn insert(&mut self, reg: Reg) {
        self.0 |= reg.bit();
    }

    pub fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub fn difference(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegEffect {
    pub uses: RegSet,
    pub defs: RegSet,
}

#[allow(dead_code)]
impl RegEffect {
    pub const NONE: Self = Self {
        uses: RegSet::EMPTY,
        defs: RegSet::EMPTY,
    };
    pub const CONSERVATIVE_CALL: Self = Self {
        uses: RegSet::ALL,
        defs: RegSet::ALL,
    };

    pub fn new(uses: RegSet, defs: RegSet) -> Self {
        Self { uses, defs }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct RegisterLivenessConfig {
    /// Effect used for JSR targets without a specific contract. The
    /// default is deliberately conservative: calls may consume and
    /// clobber A/X/Y.
    pub default_jsr: RegEffect,
    /// Optional per-target JSR contracts. Use labels as they appear in
    /// operands, e.g. "__TAB" or "$FFD2".
    pub known_jsr: std::collections::HashMap<String, RegEffect>,
    /// Prefix-keyed JSR contracts. Useful for families of generated
    /// helpers (e.g. `__CHROUT_<imm>`) that share a calling
    /// convention. Checked AFTER `known_jsr` so an exact match wins.
    pub known_jsr_prefix: Vec<(String, RegEffect)>,
    /// Registers consumed by a tail JMP to an external/unknown target.
    pub external_jmp_uses: RegSet,
    /// Registers considered observable at RTS/RTI/BRK. Conservative
    /// default keeps helper return values alive until a caller contract
    /// can narrow it.
    pub return_live_out: RegSet,
}

impl Default for RegisterLivenessConfig {
    fn default() -> Self {
        Self {
            default_jsr: RegEffect::CONSERVATIVE_CALL,
            known_jsr: std::collections::HashMap::new(),
            known_jsr_prefix: Vec::new(),
            external_jmp_uses: RegSet::ALL,
            return_live_out: RegSet::ALL,
        }
    }
}

#[allow(dead_code)]
impl RegisterLivenessConfig {
    pub fn with_jsr_effect(mut self, target: impl Into<String>, effect: RegEffect) -> Self {
        self.known_jsr.insert(target.into(), effect);
        self
    }

    pub fn with_jsr_prefix(mut self, prefix: impl Into<String>, effect: RegEffect) -> Self {
        self.known_jsr_prefix.push((prefix.into(), effect));
        self
    }

    fn jsr_effect(&self, target: &str) -> Option<RegEffect> {
        if let Some(eff) = self.known_jsr.get(target).copied() {
            return Some(eff);
        }
        for (prefix, eff) in &self.known_jsr_prefix {
            if target.starts_with(prefix.as_str()) {
                return Some(*eff);
            }
        }
        None
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct RegisterLiveness {
    pub live_in: Vec<RegSet>,
    pub live_out: Vec<RegSet>,
}

#[allow(dead_code)]
impl RegisterLiveness {
    pub fn live_in_at(&self, idx: usize) -> RegSet {
        self.live_in.get(idx).copied().unwrap_or(RegSet::EMPTY)
    }

    pub fn live_out_at(&self, idx: usize) -> RegSet {
        self.live_out.get(idx).copied().unwrap_or(RegSet::EMPTY)
    }

    pub fn is_live_in(&self, idx: usize, reg: Reg) -> bool {
        self.live_in_at(idx).contains(reg)
    }

    pub fn is_live_out(&self, idx: usize, reg: Reg) -> bool {
        self.live_out_at(idx).contains(reg)
    }
}

#[allow(dead_code)]
pub fn analyze_register_liveness(items: &[Item]) -> RegisterLiveness {
    analyze_register_liveness_with_config(items, &RegisterLivenessConfig::default())
}

#[allow(dead_code)]
pub fn analyze_register_liveness_with_config(
    items: &[Item],
    config: &RegisterLivenessConfig,
) -> RegisterLiveness {
    let labels = register_liveness_label_map(items);
    let mut live_in = vec![RegSet::EMPTY; items.len()];
    let mut live_out = vec![RegSet::EMPTY; items.len()];

    loop {
        let mut changed = false;
        for idx in (0..items.len()).rev() {
            let mut new_out = RegSet::EMPTY;
            let successors = register_liveness_successors(items, idx, &labels);
            if successors.is_empty() {
                new_out = register_liveness_terminal_out(&items[idx], config);
            } else {
                for succ in successors {
                    new_out = new_out.union(live_in[succ]);
                }
            }

            let effect = register_liveness_item_effect(&items[idx], &labels, config);
            let new_in = effect.uses.union(new_out.difference(effect.defs));
            if new_in != live_in[idx] || new_out != live_out[idx] {
                live_in[idx] = new_in;
                live_out[idx] = new_out;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    RegisterLiveness { live_in, live_out }
}

#[allow(dead_code)]
pub fn insn_register_effect(insn: &Insn, config: &RegisterLivenessConfig) -> RegEffect {
    insn_register_effect_inner(insn, false, config)
}

#[allow(dead_code)]
fn register_liveness_label_map(items: &[Item]) -> std::collections::HashMap<String, usize> {
    let mut labels = std::collections::HashMap::new();
    for (idx, item) in items.iter().enumerate() {
        if let Item::Label(name) = item {
            labels.insert(name.clone(), idx);
        }
    }
    labels
}

#[allow(dead_code)]
fn register_liveness_successors(
    items: &[Item],
    idx: usize,
    labels: &std::collections::HashMap<String, usize>,
) -> Vec<usize> {
    let next = || (idx + 1 < items.len()).then_some(idx + 1);
    match &items[idx] {
        Item::Insn(insn) if insn.mnem == "JMP" => {
            if let Some(target) = insn
                .operand
                .as_deref()
                .and_then(|op| labels.get(op).copied())
            {
                vec![target]
            } else {
                Vec::new()
            }
        }
        Item::Insn(insn) if is_uncond_terminator(&insn.mnem) => Vec::new(),
        Item::Insn(insn) if is_cond_branch(&insn.mnem) => {
            let mut out = Vec::new();
            if let Some(n) = next() {
                out.push(n);
            }
            if let Some(target) = insn
                .operand
                .as_deref()
                .and_then(|op| labels.get(op).copied())
            {
                if !out.contains(&target) {
                    out.push(target);
                }
            }
            out
        }
        Item::Directive(_) => Vec::new(),
        _ => next().into_iter().collect(),
    }
}

#[allow(dead_code)]
fn register_liveness_terminal_out(item: &Item, config: &RegisterLivenessConfig) -> RegSet {
    match item {
        Item::Insn(insn) if matches!(insn.mnem.as_str(), "RTS" | "RTI" | "BRK") => {
            config.return_live_out
        }
        _ => RegSet::EMPTY,
    }
}

#[allow(dead_code)]
fn register_liveness_item_effect(
    item: &Item,
    labels: &std::collections::HashMap<String, usize>,
    config: &RegisterLivenessConfig,
) -> RegEffect {
    let Item::Insn(insn) = item else {
        return RegEffect::NONE;
    };
    let internal_jmp = insn.mnem == "JMP"
        && insn
            .operand
            .as_deref()
            .is_some_and(|op| labels.contains_key(op));
    // A `JMP` to a non-label target that has a known call effect is a
    // tail call (e.g. a helper ending in `JMP <ROM-float-op>`): its
    // register effect is the callee's, not the conservative
    // "external jump reads everything". Only fires when `known_jsr` is
    // populated (precise-effect configs); the default config leaves it
    // empty, so existing peephole behaviour is unchanged.
    if insn.mnem == "JMP"
        && !internal_jmp
        && let Some(op) = insn.operand.as_deref()
        && let Some(eff) = config.known_jsr.get(op)
    {
        return *eff;
    }
    let mut effect = insn_register_effect_inner(insn, internal_jmp, config);
    // Conditional branches whose target isn't a known internal label
    // (literal `$XXXX`, externally-declared label) lose the target
    // arm of the CFG inside `register_liveness_successors`. Pull in
    // `external_jmp_uses` here so the over-approximation stays
    // sound: callers must never observe a value as dead when an
    // unanalysed target could read it.
    if is_cond_branch(&insn.mnem)
        && insn
            .operand
            .as_deref()
            .is_none_or(|op| !labels.contains_key(op))
    {
        effect.uses = effect.uses.union(config.external_jmp_uses);
    }
    effect
}

#[allow(dead_code)]
fn insn_register_effect_inner(
    insn: &Insn,
    internal_jmp: bool,
    config: &RegisterLivenessConfig,
) -> RegEffect {
    let mut uses = operand_index_regs(insn.operand.as_deref());
    let mut defs = RegSet::EMPTY;

    match insn.mnem.as_str() {
        "LDA" => defs = defs.union(RegSet::A),
        "LDX" => defs = defs.union(RegSet::X),
        "LDY" => defs = defs.union(RegSet::Y),
        "STA" => uses = uses.union(RegSet::A),
        "STX" => uses = uses.union(RegSet::X),
        "STY" => uses = uses.union(RegSet::Y),
        "TAX" => {
            uses = uses.union(RegSet::A);
            defs = defs.union(RegSet::X);
        }
        "TAY" => {
            uses = uses.union(RegSet::A);
            defs = defs.union(RegSet::Y);
        }
        "TXA" => {
            uses = uses.union(RegSet::X);
            defs = defs.union(RegSet::A);
        }
        "TYA" => {
            uses = uses.union(RegSet::Y);
            defs = defs.union(RegSet::A);
        }
        "TSX" => defs = defs.union(RegSet::X),
        "TXS" => uses = uses.union(RegSet::X),
        "PHA" => uses = uses.union(RegSet::A),
        "PLA" => defs = defs.union(RegSet::A),
        "INX" | "DEX" => {
            uses = uses.union(RegSet::X);
            defs = defs.union(RegSet::X);
        }
        "INY" | "DEY" => {
            uses = uses.union(RegSet::Y);
            defs = defs.union(RegSet::Y);
        }
        "ADC" | "SBC" | "AND" | "ORA" | "EOR" => {
            uses = uses.union(RegSet::A);
            defs = defs.union(RegSet::A);
        }
        "CMP" => uses = uses.union(RegSet::A),
        "CPX" => uses = uses.union(RegSet::X),
        "CPY" => uses = uses.union(RegSet::Y),
        "BIT" => uses = uses.union(RegSet::A),
        "ASL" | "LSR" | "ROL" | "ROR" => {
            if insn
                .operand
                .as_deref()
                .map_or(true, |op| op.eq_ignore_ascii_case("A"))
            {
                uses = uses.union(RegSet::A);
                defs = defs.union(RegSet::A);
            }
        }
        "JSR" => {
            return insn
                .operand
                .as_deref()
                .and_then(|target| config.jsr_effect(target))
                .unwrap_or(config.default_jsr);
        }
        "JMP" if !internal_jmp => uses = uses.union(config.external_jmp_uses),
        _ => {}
    }

    RegEffect { uses, defs }
}

#[allow(dead_code)]
fn operand_index_regs(operand: Option<&str>) -> RegSet {
    let Some(op) = operand else {
        return RegSet::EMPTY;
    };
    let upper = op.trim().to_ascii_uppercase();
    let mut out = RegSet::EMPTY;
    if upper.ends_with(",X") || upper.contains(",X)") {
        out = out.union(RegSet::X);
    }
    if upper.ends_with(",Y") || upper.contains("),Y") {
        out = out.union(RegSet::Y);
    }
    out
}

// ----- Forward static ASM facts --------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct ImmFact {
    raw: String,
    byte: Option<u8>,
}

impl ImmFact {
    fn from_operand(op: &str) -> Option<Self> {
        let raw = op.trim();
        if !raw.starts_with('#') {
            return None;
        }
        let body = raw[1..].trim();
        Some(Self {
            raw: raw.to_string(),
            byte: parse_static_imm_byte(body),
        })
    }

    fn same_value(&self, other: &Self) -> bool {
        self.raw == other.raw
            || self
                .byte
                .zip(other.byte)
                .is_some_and(|(lhs, rhs)| lhs == rhs)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct StaticRegFact {
    imm: Option<ImmFact>,
    mem: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StaticAsmFacts {
    regs: [StaticRegFact; 3],
    flag_c: Option<bool>,
    flag_z: Option<bool>,
    flag_n: Option<bool>,
    /// Known immediate values held in memory locations we can name.
    /// Populated by `STA/STX/STY <stable mem>` whenever the storer's
    /// register has a known imm. Invalidated by anything that writes
    /// the same address through an opaque path or by a JSR/JMP/label
    /// barrier (via `reset`).
    mem_imm: BTreeMap<String, u8>,
    /// Shared zero-page equates used to invalidate aliased memory facts.
    zp_equ: std::rc::Rc<std::collections::HashMap<String, u16>>,
    /// Registers whose values match the current N and Z flags.
    nz_regs: RegSet,
    /// Decimal mode does not set N and Z from the stored ADC result.
    adc_sets_nz: bool,
}

impl Default for StaticAsmFacts {
    fn default() -> Self {
        Self {
            regs: [
                StaticRegFact::default(),
                StaticRegFact::default(),
                StaticRegFact::default(),
            ],
            flag_c: None,
            flag_z: None,
            flag_n: None,
            mem_imm: BTreeMap::new(),
            zp_equ: std::rc::Rc::default(),
            nz_regs: RegSet::EMPTY,
            adc_sets_nz: true,
        }
    }
}

impl StaticAsmFacts {
    fn reset(&mut self) {
        let zp_equ = std::rc::Rc::clone(&self.zp_equ);
        let adc_sets_nz = self.adc_sets_nz;
        *self = Self::default();
        self.zp_equ = zp_equ;
        self.adc_sets_nz = adc_sets_nz;
    }

    fn reg(&self, reg: Reg) -> &StaticRegFact {
        &self.regs[reg_idx(reg)]
    }

    fn reg_mut(&mut self, reg: Reg) -> &mut StaticRegFact {
        &mut self.regs[reg_idx(reg)]
    }

    fn clear_reg(&mut self, reg: Reg) {
        *self.reg_mut(reg) = StaticRegFact::default();
    }

    fn clear_nz(&mut self) {
        self.flag_z = None;
        self.flag_n = None;
    }

    fn clear_nzc(&mut self) {
        self.flag_c = None;
        self.clear_nz();
    }

    fn clear_all_mem_origins(&mut self) {
        for fact in &mut self.regs {
            fact.mem = None;
        }
    }

    fn clear_all_mem_imms(&mut self) {
        self.mem_imm.clear();
    }

    /// Whether both fact keys resolve to the same byte.
    fn same_cell(&self, a: &str, b: &str) -> bool {
        a == b
            || fact_key_addr(&self.zp_equ, a)
                .is_some_and(|x| fact_key_addr(&self.zp_equ, b) == Some(x))
    }

    fn invalidate_mem_origin(&mut self, mem: &str) {
        let addr = fact_key_addr(&self.zp_equ, mem);
        let zp_equ = std::rc::Rc::clone(&self.zp_equ);
        let hits = |key: &str| key == mem || addr.is_some_and(|a| fact_key_addr(&zp_equ, key) == Some(a));
        for fact in &mut self.regs {
            if fact.mem.as_deref().is_some_and(hits) {
                fact.mem = None;
            }
        }
        self.mem_imm.retain(|k, _| !hits(k));
    }

    fn record_mem_imm(&mut self, mem: &str, value: u8) {
        self.mem_imm.insert(mem.to_string(), value);
    }

    fn known_mem_imm(&self, mem: &str) -> Option<u8> {
        if let Some(v) = self.mem_imm.get(mem) {
            return Some(*v);
        }
        let addr = fact_key_addr(&self.zp_equ, mem)?;
        self.mem_imm
            .iter()
            .find(|(k, _)| fact_key_addr(&self.zp_equ, k) == Some(addr))
            .map(|(_, v)| *v)
    }

    fn set_nz_from_imm(&mut self, imm: &ImmFact) {
        if let Some(byte) = imm.byte {
            self.flag_z = Some(byte == 0);
            self.flag_n = Some(byte & 0x80 != 0);
        } else {
            self.clear_nz();
        }
    }

    fn load_reg(&mut self, reg: Reg, operand: Option<&str>) {
        if let Some(op) = operand
            && let Some(imm) = ImmFact::from_operand(op)
        {
            self.reg_mut(reg).imm = Some(imm.clone());
            self.reg_mut(reg).mem = None;
            self.set_nz_from_imm(&imm);
            return;
        }

        if let Some(op) = operand
            && let Some(mem) = static_fact_mem_operand(op)
        {
            // If we know the memory's value, propagate it as an
            // immediate fact too — this lets a downstream LDA/CMP
            // see a concrete byte instead of just "same address".
            let known = self.known_mem_imm(&mem);
            self.reg_mut(reg).imm = known.map(|byte| ImmFact {
                raw: format!("#${byte:02X}"),
                byte: Some(byte),
            });
            self.reg_mut(reg).mem = Some(mem);
            if let Some(byte) = known {
                self.flag_z = Some(byte == 0);
                self.flag_n = Some(byte & 0x80 != 0);
            } else {
                self.clear_nz();
            }
            return;
        }

        self.clear_reg(reg);
        self.clear_nz();
    }

    fn store_reg(&mut self, reg: Reg, operand: Option<&str>) {
        let Some(op) = operand else {
            self.clear_all_mem_origins();
            self.clear_all_mem_imms();
            return;
        };
        if let Some(mem) = static_fact_mem_operand(op) {
            self.invalidate_mem_origin(&mem);
            // After storing reg → mem, if we knew reg's immediate
            // value, mem now holds the same byte. Other registers
            // that already happened to hold that imm pick up an
            // alias to this mem too — letting a later `LDX mem` get
            // rewritten to TXA/TYA when convenient.
            let stored_imm = self.reg(reg).imm.as_ref().and_then(|i| i.byte);
            if let Some(byte) = stored_imm {
                self.record_mem_imm(&mem, byte);
                for r in [Reg::A, Reg::X, Reg::Y] {
                    if self.reg(r).imm.as_ref().and_then(|i| i.byte) == Some(byte) {
                        self.reg_mut(r).mem = Some(mem.clone());
                    }
                }
            }
            self.reg_mut(reg).mem = Some(mem);
        } else if store_window_misses_zero_page(op) {
            // A literal absolute(-indexed) store that provably can't reach zero page: facts
            // about proven-ZP cells survive.
            self.invalidate_non_zp_mem_facts();
        } else {
            self.clear_all_mem_origins();
            self.clear_all_mem_imms();
        }
    }

    /// Discard memory facts outside zero page.
    fn invalidate_non_zp_mem_facts(&mut self) {
        let zp_equ = std::rc::Rc::clone(&self.zp_equ);
        for fact in &mut self.regs {
            if let Some(m) = fact.mem.as_deref()
                && !fact_key_is_zp(&zp_equ, m)
            {
                fact.mem = None;
            }
        }
        self.mem_imm.retain(|k, _| fact_key_is_zp(&zp_equ, k));
    }

    fn transfer(&mut self, src: Reg, dest: Reg) {
        let src_fact = self.reg(src).clone();
        *self.reg_mut(dest) = src_fact.clone();
        if let Some(imm) = &src_fact.imm {
            self.set_nz_from_imm(imm);
        } else {
            self.clear_nz();
        }
    }

    fn invalidate_memory_write_operand(&mut self, operand: Option<&str>) {
        let Some(op) = operand else {
            self.clear_all_mem_origins();
            self.clear_all_mem_imms();
            return;
        };
        if op.trim().eq_ignore_ascii_case("A") {
            return;
        }
        if let Some(mem) = static_fact_mem_operand(op) {
            // invalidate_mem_origin already drops the mem_imm entry
            // for this address, so no extra call needed here.
            self.invalidate_mem_origin(&mem);
        } else {
            self.clear_all_mem_origins();
            self.clear_all_mem_imms();
        }
    }

    /// Apply INX/INY/DEX/DEY to a known-immediate register fact.
    /// Falls back to "unknown" when the source value isn't tracked.
    fn bump_reg(&mut self, reg: Reg, delta: i8) {
        let known = self.reg(reg).imm.as_ref().and_then(|i| i.byte);
        match known {
            Some(byte) => {
                let new_byte = byte.wrapping_add(delta as u8);
                let imm = ImmFact {
                    raw: format!("#${new_byte:02X}"),
                    byte: Some(new_byte),
                };
                self.reg_mut(reg).imm = Some(imm.clone());
                // The increment invalidates any "this register
                // mirrors memory X" alias — the new byte may not
                // match what memory X currently holds.
                self.reg_mut(reg).mem = None;
                self.set_nz_from_imm(&imm);
            }
            None => {
                self.clear_reg(reg);
                self.clear_nz();
            }
        }
    }

    /// Constant-fold AND/ORA/EOR with an immediate operand into the
    /// current A.imm fact. Anything else (memory operand, unknown
    /// A) collapses to the conservative "A unknown, N/Z unknown".
    fn alu_a_imm(&mut self, insn: &Insn) {
        let folded = (|| {
            let op = insn.operand.as_deref()?;
            let rhs = ImmFact::from_operand(op)?.byte?;
            let lhs = self.reg(Reg::A).imm.as_ref()?.byte?;
            let result = match insn.mnem.as_str() {
                "AND" => lhs & rhs,
                "ORA" => lhs | rhs,
                "EOR" => lhs ^ rhs,
                _ => return None,
            };
            Some(result)
        })();
        match folded {
            Some(byte) => {
                let imm = ImmFact {
                    raw: format!("#${byte:02X}"),
                    byte: Some(byte),
                };
                self.reg_mut(Reg::A).imm = Some(imm.clone());
                self.reg_mut(Reg::A).mem = None;
                self.set_nz_from_imm(&imm);
            }
            None => {
                self.clear_reg(Reg::A);
                self.clear_nz();
            }
        }
    }

    /// Constant-fold CMP/CPX/CPY with an immediate when the
    /// register is known. Comparison sets:
    ///   Z = (reg == imm)
    ///   C = (reg >= imm)
    ///   N = bit 7 of (reg - imm)
    /// When both operands aren't known, we fall back to clear_nzc.
    fn cmp_imm(&mut self, reg: Reg, insn: &Insn) {
        let folded = (|| {
            let op = insn.operand.as_deref()?;
            let rhs = ImmFact::from_operand(op)?.byte?;
            let lhs = self.reg(reg).imm.as_ref()?.byte?;
            Some((lhs, rhs))
        })();
        match folded {
            Some((lhs, rhs)) => {
                let diff = lhs.wrapping_sub(rhs);
                self.flag_z = Some(lhs == rhs);
                self.flag_c = Some(lhs >= rhs);
                self.flag_n = Some(diff & 0x80 != 0);
            }
            None => self.clear_nzc(),
        }
    }

    fn apply_unknown_effect(&mut self, insn: &Insn) {
        let reg_config = RegisterLivenessConfig::default();
        let reg_effect = insn_register_effect_inner(insn, false, &reg_config);
        for reg in [Reg::A, Reg::X, Reg::Y] {
            if reg_effect.defs.contains(reg) {
                self.clear_reg(reg);
            }
        }

        let flag_config = FlagLivenessConfig::default();
        let flag_effect = insn_flag_effect_inner(insn, false, &flag_config);
        if flag_effect.defs.contains(Flag::C) {
            self.flag_c = None;
        }
        if flag_effect.defs.contains(Flag::Z) {
            self.flag_z = None;
        }
        if flag_effect.defs.contains(Flag::N) {
            self.flag_n = None;
        }
        // Unknown opcodes might write to memory we don't recognise
        // (this catches typed pseudo-mnems and 65C02/65816 ops the
        // analyser doesn't model). Drop every memory-origin fact so
        // a later load doesn't pretend the cached register still
        // mirrors that address.
        self.clear_all_mem_origins();
        self.clear_all_mem_imms();
    }

    fn update_item(&mut self, item: &Item) {
        match item {
            Item::Label(_) | Item::Directive(_) | Item::Origin(_) => self.reset(),
            Item::Blank | Item::Comment(_) => {}
            Item::Insn(insn) => self.update_insn(insn),
        }
    }

    fn update_insn(&mut self, insn: &Insn) {
        let nz_before = self.nz_regs;
        self.update_insn_values(insn);
        self.nz_regs = nz_regs_after(insn, nz_before, self.adc_sets_nz);
    }

    fn update_insn_values(&mut self, insn: &Insn) {
        match insn.mnem.as_str() {
            "LDA" => self.load_reg(Reg::A, insn.operand.as_deref()),
            "LDX" => self.load_reg(Reg::X, insn.operand.as_deref()),
            "LDY" => self.load_reg(Reg::Y, insn.operand.as_deref()),
            "STA" => self.store_reg(Reg::A, insn.operand.as_deref()),
            "STX" => self.store_reg(Reg::X, insn.operand.as_deref()),
            "STY" => self.store_reg(Reg::Y, insn.operand.as_deref()),
            "TAX" => self.transfer(Reg::A, Reg::X),
            "TAY" => self.transfer(Reg::A, Reg::Y),
            "TXA" => self.transfer(Reg::X, Reg::A),
            "TYA" => self.transfer(Reg::Y, Reg::A),
            "TSX" => {
                self.clear_reg(Reg::X);
                self.clear_nz();
            }
            "TXS" | "PHA" | "PHP" => {}
            // PLP loads ALL flags from the byte popped off the stack —
            // C, Z, N (and V/I/D, which we don't track). We can't
            // predict what was pushed, so all tracked flags become
            // unknown. Treating it as a no-op (the previous version
            // did) would let the optimizer drop a CLC/SEC after a
            // PHP/PLP pair even though the carry coming out of PLP
            // is whatever the caller had.
            "PLP" => self.clear_nzc(),
            "PLA" => {
                self.clear_reg(Reg::A);
                self.clear_nz();
            }
            "INX" => self.bump_reg(Reg::X, 1),
            "DEX" => self.bump_reg(Reg::X, -1),
            "INY" => self.bump_reg(Reg::Y, 1),
            "DEY" => self.bump_reg(Reg::Y, -1),
            "AND" | "ORA" | "EOR" => self.alu_a_imm(insn),
            "ADC" | "SBC" => {
                // ADC/SBC depend on the carry flag. We could fold
                // this when both A.imm and C are known, but the
                // payoff is small and easy to get wrong. Stay
                // conservative for now and just clear A + N/Z/C.
                self.clear_reg(Reg::A);
                self.clear_nzc();
            }
            "CMP" => self.cmp_imm(Reg::A, insn),
            "CPX" => self.cmp_imm(Reg::X, insn),
            "CPY" => self.cmp_imm(Reg::Y, insn),
            "BIT" => self.clear_nz(),
            "ASL" | "LSR" | "ROL" | "ROR" => {
                if insn
                    .operand
                    .as_deref()
                    .map_or(true, |op| op.trim().eq_ignore_ascii_case("A"))
                {
                    self.clear_reg(Reg::A);
                } else {
                    self.invalidate_memory_write_operand(insn.operand.as_deref());
                }
                self.clear_nzc();
            }
            "INC" | "DEC" => {
                self.invalidate_memory_write_operand(insn.operand.as_deref());
                self.clear_nz();
            }
            "CLC" => self.flag_c = Some(false),
            "SEC" => self.flag_c = Some(true),
            "CLV" => {}
            "JSR" | "JMP" | "RTS" | "RTI" | "BRK" => self.reset(),
            // Conditional branches change PC but touch no registers,
            // no memory, and (BIT aside) don't clear any flags we
            // track. `apply_unknown_effect` would conservatively
            // wipe every memory-origin fact — that severs the
            // X-mem cache across `BCS`/`BEQ` etc. in tight inner
            // loops (bubble sort: `LDX VI_X / abs,X ops / CMP /
            // BCS / abs,X ops` reloaded VI_X redundantly because of
            // this). Branches are a pure no-op for the fact set.
            m if is_cond_branch(m) => {}
            _ => self.apply_unknown_effect(insn),
        }
    }
}

/// Track registers whose values match N and Z after this instruction.
fn nz_regs_after(insn: &Insn, before: RegSet, adc_sets_nz: bool) -> RegSet {
    let on_a = insn
        .operand
        .as_deref()
        .is_none_or(|op| op.trim().eq_ignore_ascii_case("A"));
    match insn.mnem.as_str() {
        "LDA" | "PLA" | "AND" | "ORA" | "EOR" => RegSet::A,
        "ADC" | "SBC" if adc_sets_nz => RegSet::A,
        "ASL" | "LSR" | "ROL" | "ROR" if on_a => RegSet::A,
        "LDX" | "TSX" | "INX" | "DEX" => RegSet::X,
        "LDY" | "INY" | "DEY" => RegSet::Y,
        "TAX" | "TXA" => RegSet::A.union(RegSet::X),
        "TAY" | "TYA" => RegSet::A.union(RegSet::Y),
        "STA" | "STX" | "STY" | "CLC" | "SEC" | "CLV" | "CLD" | "SED" | "CLI" | "SEI"
        | "NOP" | "PHA" | "PHP" | "TXS" => before,
        m if is_cond_branch(m) => before,
        _ => RegSet::EMPTY,
    }
}

fn reg_idx(reg: Reg) -> usize {
    match reg {
        Reg::A => 0,
        Reg::X => 1,
        Reg::Y => 2,
    }
}

fn parse_static_imm_byte(body: &str) -> Option<u8> {
    let body = body.trim();
    if let Some(rest) = body.strip_prefix('<') {
        return parse_u16_literal(rest).map(|v| (v & 0x00ff) as u8);
    }
    if let Some(rest) = body.strip_prefix('>') {
        return parse_u16_literal(rest).map(|v| ((v >> 8) & 0x00ff) as u8);
    }
    parse_u16_literal(body).and_then(|v| (v <= 0x00ff).then_some(v as u8))
}

fn parse_u16_literal(s: &str) -> Option<u16> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix('$') {
        u16::from_str_radix(hex, 16).ok()
    } else if let Some(bin) = s.strip_prefix('%') {
        u16::from_str_radix(bin, 2).ok()
    } else {
        s.parse::<u16>().ok()
    }
}

fn static_fact_mem_operand(op: &str) -> Option<String> {
    let trimmed = op.trim();
    if trimmed.starts_with('#') || trimmed.eq_ignore_ascii_case("A") {
        return None;
    }
    if is_label_or_zp_safe(trimmed) {
        Some(trimmed.to_string())
    } else {
        None
    }
}

/// Does this store operand name a literal absolute(-indexed) window that provably stays out of
/// zero page?
fn store_window_misses_zero_page(op: &str) -> bool {
    let (base, indexed) = match op.split_once(',') {
        Some((b, idx)) => {
            let idx = idx.trim();
            if idx != "X" && idx != "Y" {
                return false;
            }
            (b.trim(), true)
        }
        None => (op.trim(), false),
    };
    let Some(hex) = base.strip_prefix('$') else {
        return false;
    };
    if hex.len() != 4 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return false;
    }
    let Ok(lo) = u16::from_str_radix(hex, 16) else {
        return false;
    };
    lo >= 0x0100 && (!indexed || lo <= 0xFF00)
}

/// Is `key` (a `static_fact_mem_operand` string) provably a zero-page cell?
fn fact_key_is_zp(zp_equ: &std::collections::HashMap<String, u16>, key: &str) -> bool {
    fact_key_addr(zp_equ, key).is_some()
}

/// Resolve a fact key to a zero page address.
fn fact_key_addr(zp_equ: &std::collections::HashMap<String, u16>, key: &str) -> Option<u16> {
    let (base, off) = match key.split_once('+') {
        Some((b, o)) => (b.trim(), i32::from(o.trim().parse::<u16>().ok()?)),
        None => match key.split_once('-') {
            Some((b, o)) => (b.trim(), -i32::from(o.trim().parse::<u16>().ok()?)),
            None => (key, 0),
        },
    };
    let val = if let Some(hex) = base.strip_prefix('$') {
        if hex.len() == 2 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            u16::from_str_radix(hex, 16).ok()
        } else {
            None
        }
    } else {
        zp_equ.get(base).copied()
    };
    let addr = i32::from(val?) + off;
    (0..0x0100).contains(&addr).then_some(addr as u16)
}

fn analyze_static_asm_facts(items: &[Item]) -> Vec<StaticAsmFacts> {
    let mut current = StaticAsmFacts::default();
    current.adc_sets_nz = !items
        .iter()
        .any(|it| matches!(it, Item::Insn(i) if i.mnem == "SED"));
    current.zp_equ = std::rc::Rc::new(
        collect_equ_values(items)
            .into_iter()
            .filter(|(_, v)| *v < 0x0100)
            .collect(),
    );
    let entered_by_one_branch = labels_entered_by_one_forward_branch(items);
    let mut facts: Vec<StaticAsmFacts> = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        facts.push(current.clone());
        current.update_item(item);
        if let Some(&branch) = entered_by_one_branch.get(&i) {
            let mut inherited = facts[branch].clone();
            inherited.update_item(&items[branch]);
            current = inherited;
        }
    }
    facts
}

/// Find labels reached only by one forward conditional branch.
fn labels_entered_by_one_forward_branch(
    items: &[Item],
) -> std::collections::HashMap<usize, usize> {
    let refs = label_ref_counts(items);
    let mut branch_to: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for (j, it) in items.iter().enumerate() {
        if let Item::Insn(insn) = it
            && is_cond_branch(&insn.mnem)
            && let Some(op) = insn.operand.as_deref()
        {
            branch_to.insert(op.trim(), j);
        }
    }
    let mut out = std::collections::HashMap::new();
    for (i, it) in items.iter().enumerate() {
        let Item::Label(name) = it else { continue };
        if refs.get(name.as_str()).copied() != Some(1) {
            continue;
        }
        let Some(&branch) = branch_to.get(name.as_str()) else {
            continue;
        };
        if branch >= i {
            continue;
        }
        let falls_in = prev_struct(items, i).is_none_or(|p| {
            !matches!(&items[p], Item::Insn(insn) if is_uncond_terminator(&insn.mnem))
        });
        let shares_address = next_struct(items, i)
            .is_some_and(|n| matches!(items[n], Item::Label(_)));
        if falls_in || shares_address {
            continue;
        }
        out.insert(i, branch);
    }
    out
}

// ----- Flag liveness --------------------------------------------------------

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Flag {
    Z,
    N,
    C,
    V,
}

#[allow(dead_code)]
impl Flag {
    fn bit(self) -> u8 {
        match self {
            Flag::Z => 0b0001,
            Flag::N => 0b0010,
            Flag::C => 0b0100,
            Flag::V => 0b1000,
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FlagSet(u8);

#[allow(dead_code)]
impl FlagSet {
    pub const EMPTY: Self = Self(0);
    pub const Z: Self = Self(0b0001);
    pub const N: Self = Self(0b0010);
    pub const C: Self = Self(0b0100);
    pub const V: Self = Self(0b1000);
    pub const NZ: Self = Self(0b0011);
    pub const NZC: Self = Self(0b0111);
    pub const ALL: Self = Self(0b1111);

    pub fn from_flags(flags: &[Flag]) -> Self {
        let mut out = Self::EMPTY;
        for &flag in flags {
            out.insert(flag);
        }
        out
    }

    pub fn contains(self, flag: Flag) -> bool {
        self.0 & flag.bit() != 0
    }

    pub fn insert(&mut self, flag: Flag) {
        self.0 |= flag.bit();
    }

    pub fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub fn difference(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlagEffect {
    pub uses: FlagSet,
    pub defs: FlagSet,
}

#[allow(dead_code)]
impl FlagEffect {
    pub const NONE: Self = Self {
        uses: FlagSet::EMPTY,
        defs: FlagSet::EMPTY,
    };
    pub const CONSERVATIVE_CALL: Self = Self {
        uses: FlagSet::ALL,
        defs: FlagSet::ALL,
    };

    pub fn new(uses: FlagSet, defs: FlagSet) -> Self {
        Self { uses, defs }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct FlagLivenessConfig {
    pub default_jsr: FlagEffect,
    pub known_jsr: std::collections::HashMap<String, FlagEffect>,
    pub known_jsr_prefix: Vec<(String, FlagEffect)>,
    pub external_jmp_uses: FlagSet,
    pub return_live_out: FlagSet,
}

impl Default for FlagLivenessConfig {
    fn default() -> Self {
        Self {
            default_jsr: FlagEffect::CONSERVATIVE_CALL,
            known_jsr: std::collections::HashMap::new(),
            known_jsr_prefix: Vec::new(),
            external_jmp_uses: FlagSet::ALL,
            return_live_out: FlagSet::ALL,
        }
    }
}

#[allow(dead_code)]
impl FlagLivenessConfig {
    pub fn with_jsr_effect(mut self, target: impl Into<String>, effect: FlagEffect) -> Self {
        self.known_jsr.insert(target.into(), effect);
        self
    }

    pub fn with_jsr_prefix(mut self, prefix: impl Into<String>, effect: FlagEffect) -> Self {
        self.known_jsr_prefix.push((prefix.into(), effect));
        self
    }

    fn jsr_effect(&self, target: &str) -> Option<FlagEffect> {
        if let Some(eff) = self.known_jsr.get(target).copied() {
            return Some(eff);
        }
        for (prefix, eff) in &self.known_jsr_prefix {
            if target.starts_with(prefix.as_str()) {
                return Some(*eff);
            }
        }
        None
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct FlagLiveness {
    pub live_in: Vec<FlagSet>,
    pub live_out: Vec<FlagSet>,
}

#[allow(dead_code)]
impl FlagLiveness {
    pub fn live_in_at(&self, idx: usize) -> FlagSet {
        self.live_in.get(idx).copied().unwrap_or(FlagSet::EMPTY)
    }

    pub fn live_out_at(&self, idx: usize) -> FlagSet {
        self.live_out.get(idx).copied().unwrap_or(FlagSet::EMPTY)
    }

    pub fn is_live_in(&self, idx: usize, flag: Flag) -> bool {
        self.live_in_at(idx).contains(flag)
    }

    pub fn is_live_out(&self, idx: usize, flag: Flag) -> bool {
        self.live_out_at(idx).contains(flag)
    }
}

#[allow(dead_code)]
pub fn analyze_flag_liveness(items: &[Item]) -> FlagLiveness {
    analyze_flag_liveness_with_config(items, &FlagLivenessConfig::default())
}

#[allow(dead_code)]
pub fn analyze_flag_liveness_with_config(
    items: &[Item],
    config: &FlagLivenessConfig,
) -> FlagLiveness {
    let labels = register_liveness_label_map(items);
    let mut live_in = vec![FlagSet::EMPTY; items.len()];
    let mut live_out = vec![FlagSet::EMPTY; items.len()];

    loop {
        let mut changed = false;
        for idx in (0..items.len()).rev() {
            let successors = register_liveness_successors(items, idx, &labels);
            let mut new_out = FlagSet::EMPTY;
            if successors.is_empty() {
                new_out = flag_liveness_terminal_out(&items[idx], config);
            } else {
                for succ in successors {
                    new_out = new_out.union(live_in[succ]);
                }
            }

            let effect = flag_liveness_item_effect(&items[idx], &labels, config);
            let new_in = effect.uses.union(new_out.difference(effect.defs));
            if new_in != live_in[idx] || new_out != live_out[idx] {
                live_in[idx] = new_in;
                live_out[idx] = new_out;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    FlagLiveness { live_in, live_out }
}

#[allow(dead_code)]
pub fn insn_flag_effect(insn: &Insn, config: &FlagLivenessConfig) -> FlagEffect {
    insn_flag_effect_inner(insn, false, config)
}

fn flag_liveness_terminal_out(item: &Item, config: &FlagLivenessConfig) -> FlagSet {
    match item {
        Item::Insn(insn) if matches!(insn.mnem.as_str(), "RTS" | "RTI" | "BRK") => {
            config.return_live_out
        }
        _ => FlagSet::EMPTY,
    }
}

fn flag_liveness_item_effect(
    item: &Item,
    labels: &std::collections::HashMap<String, usize>,
    config: &FlagLivenessConfig,
) -> FlagEffect {
    let Item::Insn(insn) = item else {
        return FlagEffect::NONE;
    };
    let internal_jmp = insn.mnem == "JMP"
        && insn
            .operand
            .as_deref()
            .is_some_and(|op| labels.contains_key(op));
    let mut effect = insn_flag_effect_inner(insn, internal_jmp, config);
    if is_cond_branch(&insn.mnem)
        && insn
            .operand
            .as_deref()
            .is_none_or(|op| !labels.contains_key(op))
    {
        effect.uses = effect.uses.union(config.external_jmp_uses);
    }
    effect
}

fn insn_flag_effect_inner(
    insn: &Insn,
    internal_jmp: bool,
    config: &FlagLivenessConfig,
) -> FlagEffect {
    let mut uses = FlagSet::EMPTY;
    let mut defs = FlagSet::EMPTY;

    match insn.mnem.as_str() {
        "BEQ" | "BNE" => uses = uses.union(FlagSet::Z),
        "BMI" | "BPL" => uses = uses.union(FlagSet::N),
        "BCC" | "BCS" => uses = uses.union(FlagSet::C),
        "BVC" | "BVS" => uses = uses.union(FlagSet::V),
        "LDA" | "LDX" | "LDY" | "TAX" | "TAY" | "TXA" | "TYA" | "TSX" | "PLA" | "INX" | "DEX"
        | "INY" | "DEY" | "AND" | "ORA" | "EOR" => {
            defs = defs.union(FlagSet::NZ);
        }
        "ADC" | "SBC" => {
            uses = uses.union(FlagSet::C);
            defs = defs.union(FlagSet::ALL);
        }
        "CMP" | "CPX" | "CPY" => defs = defs.union(FlagSet::NZC),
        "BIT" => defs = defs.union(FlagSet::NZ.union(FlagSet::V)),
        "ASL" | "LSR" => defs = defs.union(FlagSet::NZC),
        "ROL" | "ROR" => {
            uses = uses.union(FlagSet::C);
            defs = defs.union(FlagSet::NZC);
        }
        "CLC" | "SEC" => defs = defs.union(FlagSet::C),
        "CLV" => defs = defs.union(FlagSet::V),
        "JSR" => {
            return insn
                .operand
                .as_deref()
                .and_then(|target| config.jsr_effect(target))
                .unwrap_or(config.default_jsr);
        }
        "JMP" if !internal_jmp => uses = uses.union(config.external_jmp_uses),
        _ => {}
    }

    FlagEffect { uses, defs }
}

// ----- FAC liveness ---------------------------------------------------------

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FacEffect {
    pub uses: bool,
    pub defs: bool,
}

#[allow(dead_code)]
impl FacEffect {
    pub const NONE: Self = Self {
        uses: false,
        defs: false,
    };
    pub const CONSERVATIVE_CALL: Self = Self {
        uses: true,
        defs: true,
    };

    pub fn new(uses: bool, defs: bool) -> Self {
        Self { uses, defs }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct FacLivenessConfig {
    pub default_jsr: FacEffect,
    pub known_jsr: std::collections::HashMap<String, FacEffect>,
    pub known_jsr_prefix: Vec<(String, FacEffect)>,
    pub external_jmp_uses: bool,
    pub return_live_out: bool,
}

impl Default for FacLivenessConfig {
    fn default() -> Self {
        Self {
            default_jsr: FacEffect::CONSERVATIVE_CALL,
            known_jsr: std::collections::HashMap::new(),
            known_jsr_prefix: Vec::new(),
            external_jmp_uses: true,
            return_live_out: true,
        }
    }
}

#[allow(dead_code)]
impl FacLivenessConfig {
    pub fn with_jsr_effect(mut self, target: impl Into<String>, effect: FacEffect) -> Self {
        self.known_jsr.insert(target.into(), effect);
        self
    }

    pub fn with_jsr_prefix(mut self, prefix: impl Into<String>, effect: FacEffect) -> Self {
        self.known_jsr_prefix.push((prefix.into(), effect));
        self
    }

    fn jsr_effect(&self, target: &str) -> Option<FacEffect> {
        if let Some(eff) = self.known_jsr.get(target).copied() {
            return Some(eff);
        }
        for (prefix, eff) in &self.known_jsr_prefix {
            if target.starts_with(prefix.as_str()) {
                return Some(*eff);
            }
        }
        None
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct FacLiveness {
    pub live_in: Vec<bool>,
    pub live_out: Vec<bool>,
}

#[allow(dead_code)]
impl FacLiveness {
    pub fn is_live_in(&self, idx: usize) -> bool {
        self.live_in.get(idx).copied().unwrap_or(false)
    }

    pub fn is_live_out(&self, idx: usize) -> bool {
        self.live_out.get(idx).copied().unwrap_or(false)
    }
}

#[allow(dead_code)]
pub fn analyze_fac_liveness(items: &[Item]) -> FacLiveness {
    analyze_fac_liveness_with_config(items, &fac_liveness_config_with_helpers())
}

#[allow(dead_code)]
pub fn analyze_fac_liveness_with_config(items: &[Item], config: &FacLivenessConfig) -> FacLiveness {
    let labels = register_liveness_label_map(items);
    let mut live_in = vec![false; items.len()];
    let mut live_out = vec![false; items.len()];

    loop {
        let mut changed = false;
        for idx in (0..items.len()).rev() {
            let successors = register_liveness_successors(items, idx, &labels);
            let mut new_out = false;
            if successors.is_empty() {
                new_out = fac_liveness_terminal_out(&items[idx], config);
            } else {
                for succ in successors {
                    new_out |= live_in[succ];
                }
            }

            let effect = fac_liveness_item_effect(&items[idx], &labels, config);
            let new_in = effect.uses || (new_out && !effect.defs);
            if new_in != live_in[idx] || new_out != live_out[idx] {
                live_in[idx] = new_in;
                live_out[idx] = new_out;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    FacLiveness { live_in, live_out }
}

fn fac_liveness_terminal_out(item: &Item, config: &FacLivenessConfig) -> bool {
    match item {
        Item::Insn(insn) if matches!(insn.mnem.as_str(), "RTS" | "RTI" | "BRK") => {
            config.return_live_out
        }
        // End-of-stream non-terminator (typically only test snippets;
        // production asm always ends with RTS). Treat FAC as live to
        // avoid PH150 dropping the last FAC-defining JSR in a snippet
        // where the unit test trims off the consumer for brevity.
        _ => true,
    }
}

fn fac_liveness_item_effect(
    item: &Item,
    labels: &std::collections::HashMap<String, usize>,
    config: &FacLivenessConfig,
) -> FacEffect {
    let Item::Insn(insn) = item else {
        return FacEffect::NONE;
    };
    let internal_jmp = insn.mnem == "JMP"
        && insn
            .operand
            .as_deref()
            .is_some_and(|op| labels.contains_key(op));
    let mut effect = insn_fac_effect_inner(insn, internal_jmp, config);
    if is_cond_branch(&insn.mnem)
        && insn
            .operand
            .as_deref()
            .is_none_or(|op| !labels.contains_key(op))
    {
        effect.uses |= config.external_jmp_uses;
    }
    effect
}

fn insn_fac_effect_inner(insn: &Insn, internal_jmp: bool, config: &FacLivenessConfig) -> FacEffect {
    let mut uses = false;
    let mut defs = false;
    let refs_fac = insn.operand.as_deref().is_some_and(operand_refs_fac_zp);

    match insn.mnem.as_str() {
        "LDA" | "LDX" | "LDY" | "ADC" | "SBC" | "AND" | "ORA" | "EOR" | "CMP" | "CPX" | "CPY"
        | "BIT" => {
            uses |= refs_fac;
        }
        "STA" | "STX" | "STY" => {
            defs |= refs_fac;
        }
        "INC" | "DEC" | "ASL" | "LSR" | "ROL" | "ROR" => {
            if refs_fac
                && !insn
                    .operand
                    .as_deref()
                    .is_some_and(|op| op.eq_ignore_ascii_case("A"))
            {
                uses = true;
                defs = true;
            }
        }
        "JSR" => {
            return insn
                .operand
                .as_deref()
                .and_then(|target| config.jsr_effect(target))
                .unwrap_or(config.default_jsr);
        }
        "JMP" if !internal_jmp => uses |= config.external_jmp_uses,
        _ => {}
    }

    FacEffect { uses, defs }
}

fn operand_refs_fac_zp(operand: &str) -> bool {
    let op = operand.trim();
    if op.starts_with('#') {
        return false;
    }
    let op = op.trim_start_matches('(');
    let Some(rest) = op.strip_prefix('$') else {
        return false;
    };
    let hex: String = rest.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
    if hex.is_empty() {
        return false;
    }
    u16::from_str_radix(&hex, 16)
        .ok()
        .is_some_and(|addr| (0x61..=0x66).contains(&addr))
}

fn fac_liveness_config_with_helpers() -> FacLivenessConfig {
    let use_only = FacEffect::new(true, false);
    let def_only = FacEffect::new(false, true);
    let use_def = FacEffect::new(true, true);

    FacLivenessConfig::default()
        .with_jsr_effect("$BBA2", def_only) // MOVFM: memory -> FAC
        .with_jsr_effect("$BBD4", use_only) // MOVMF: FAC -> memory
        .with_jsr_effect("$B391", def_only) // GIVAYF
        .with_jsr_effect("$B3A2", def_only)
        .with_jsr_effect("$B7F7", use_def)
        .with_jsr_effect("$BC0C", use_only) // FACARG
        .with_jsr_effect("$B867", use_def) // FADD
        .with_jsr_effect("$B850", use_def) // FSUB
        .with_jsr_effect("$BA28", use_def) // FMULT
        .with_jsr_effect("$BB0F", use_def) // FDIV
        .with_jsr_effect("$BF7B", use_def) // FPWRT
        .with_jsr_effect("$BC58", use_def) // ABS
        .with_jsr_effect("$BCCC", use_def) // INT
        .with_jsr_effect("$BC39", use_def) // SGN
        .with_jsr_effect("$BF71", use_def) // SQR
        .with_jsr_effect("$E26B", use_def) // SIN
        .with_jsr_effect("$E264", use_def) // COS
        .with_jsr_effect("$E2B4", use_def) // TAN
        .with_jsr_effect("$E30E", use_def) // ATN
        .with_jsr_effect("$B9EA", use_def) // LOG
        .with_jsr_effect("$BFED", use_def) // EXP
        .with_jsr_effect("$E097", use_def) // RND
        .with_jsr_effect("$BDDD", use_def) // FOUT
        .with_jsr_effect("__PRINT_FAC", use_def)
        .with_jsr_effect("__BOOL_TO_FAC", def_only)

        .with_jsr_effect("__FAC_BYTE", use_def)
        .with_jsr_effect("__FAC_BYTE_OK", use_def)
        .with_jsr_effect("$B1BF", use_def) // AYINT (falls into QINT)
        .with_jsr_effect("__AYINT", use_def)
        .with_jsr_effect("__FAC_TO_INT16", use_def)
        .with_jsr_effect("__FAC_TO_INT16_NOTRAP", use_def)
        // DIV16: I/O is via LINNUM and __DIV16_*; FAC is untouched.
        // Marking it neither-uses-nor-defs lets PH's FAC-liveness
        // see DIV16 as transparent across MOVMF/MOVFM windows.
        .with_jsr_effect("__DIV16", FacEffect::new(false, false))
        .with_jsr_prefix("__LD_BYTE_FAC", def_only)
        .with_jsr_prefix("__LDV_", def_only)
        .with_jsr_prefix("__READ_", def_only)
}

/// Remove loads whose register value and flags are unused.
fn ph017_drop_dead_load(items: &mut Vec<Item>) -> bool {
    let config = liveness_config_with_helpers();
    let live = analyze_register_liveness_with_config(items, &config);
    let smc = smc_patched_labels(items);
    let mut to_drop = Vec::new();
    for (idx, it) in items.iter().enumerate() {
        let Item::Insn(insn) = it else { continue };
        let dest = match insn.mnem.as_str() {
            "LDA" => Reg::A,
            "LDX" => Reg::X,
            "LDY" => Reg::Y,
            _ => continue,
        };
        if live.is_live_out(idx, dest) {
            continue;
        }
        if insn_is_smc_target(items, idx, &smc) {
            continue;
        }
        let Some(op) = insn.operand.as_deref() else {
            continue;
        };
        if op.starts_with('(') {
            continue;
        }
        if let Some(addr) = parse_hex_addr(op)
            && (0xD000..=0xDFFF).contains(&addr)
        {
            continue;
        }
        // Local redefinition gate: the next structural insn must
        // unconditionally redefine `dest` before any read.
        let Some(j) = next_struct(items, idx) else {
            continue;
        };
        let Some(Item::Insn(next)) = items.get(j) else {
            continue;
        };
        if is_cond_branch(&next.mnem) {
            continue;
        }
        let next_effect = insn_register_effect_inner(next, false, &config);
        if !next_effect.defs.contains(dest) || next_effect.uses.contains(dest) {
            continue;
        }
        to_drop.push(idx);
    }
    if to_drop.is_empty() {
        return false;
    }
    for idx in to_drop.into_iter().rev() {
        items.remove(idx);
    }
    true
}

/// Parse `$DEAD` / `$beef` / `$D` (decimal-style with `$` prefix) to
/// the numeric address it refers to. Returns `None` for immediate
/// operands (`#$XX`), labels, indexed forms, or anything that
/// doesn't look like a bare address literal.
fn parse_hex_addr(operand: &str) -> Option<u16> {
    let op = operand.trim();
    if !op.starts_with('$') {
        return None;
    }
    // Must be a bare hex literal — refuse `$XX,Y` etc. since the
    // effective address depends on the runtime index.
    let body = &op[1..];
    let body = body.split(',').next()?;
    u16::from_str_radix(body, 16).ok()
}

/// Drop immediate loads only when the register and N/Z are dead.
fn ph317_drop_dead_load_via_flag_liveness(items: &mut Vec<Item>) -> bool {
    // PHP reads flags through the stack, so disable this pass.
    if items
        .iter()
        .any(|it| matches!(it, Item::Insn(i) if i.mnem == "PHP"))
    {
        return false;
    }
    // Skip liveness analysis when there are no immediate loads.
    if !items.iter().any(|it| {
        matches!(it, Item::Insn(i)
            if matches!(i.mnem.as_str(), "LDA" | "LDX" | "LDY")
                && i.operand.as_deref().is_some_and(|op| op.starts_with('#')))
    }) {
        return false;
    }
    let reg_config = liveness_config_with_helpers();
    let regs = analyze_register_liveness_with_config(items, &reg_config);
    let flags = analyze_flag_liveness_with_config(items, &FlagLivenessConfig::default());
    let smc = smc_patched_labels(items);
    let mut to_drop = Vec::new();
    for (idx, it) in items.iter().enumerate() {
        let Item::Insn(insn) = it else { continue };
        let dest = match insn.mnem.as_str() {
            "LDA" => Reg::A,
            "LDX" => Reg::X,
            "LDY" => Reg::Y,
            _ => continue,
        };
        // The loaded register must be dead.
        if regs.is_live_out(idx, dest) {
            continue;
        }
        // Both flags set by the load must be dead.
        if flags.is_live_out(idx, Flag::N) || flags.is_live_out(idx, Flag::Z) {
            continue;
        }
        // Require an instruction after the load to avoid layout barriers.
        let Some(j) = next_struct(items, idx) else {
            continue;
        };
        if !matches!(items[j], Item::Insn(_)) {
            continue;
        }
        if insn_is_smc_target(items, idx, &smc) {
            continue;
        }
        let Some(op) = insn.operand.as_deref() else {
            continue;
        };
        if !op.starts_with('#') {
            continue;
        }
        if op.starts_with('(') {
            continue;
        }
        if let Some(addr) = parse_hex_addr(op)
            && (0xD000..=0xDFFF).contains(&addr)
        {
            continue;
        }
        to_drop.push(idx);
    }
    if to_drop.is_empty() {
        return false;
    }
    for idx in to_drop.into_iter().rev() {
        items.remove(idx);
    }
    true
}

/// Drop register transfers only when the destination and flags are dead.
fn ph015_drop_dead_transfer(items: &mut Vec<Item>) -> bool {
    let config = liveness_config_with_helpers();
    let live = analyze_register_liveness_with_config(items, &config);
    let smc = smc_patched_labels(items);
    let facts = analyze_static_asm_facts(items);
    let labels = collect_label_index(items);
    let contracts = conservative_return_flag_config();
    let mut to_drop = Vec::new();
    for (idx, it) in items.iter().enumerate() {
        let Item::Insn(insn) = it else { continue };
        let (src, dest) = match insn.mnem.as_str() {
            "TAX" => (Reg::A, Reg::X),
            "TAY" => (Reg::A, Reg::Y),
            "TXA" => (Reg::X, Reg::A),
            "TYA" => (Reg::Y, Reg::A),
            _ => continue,
        };
        if live.is_live_out(idx, dest) {
            continue;
        }

        if insn_is_smc_target(items, idx, &smc) {
            continue;
        }
        if !facts[idx].nz_regs.contains(src)
            && !nz_dead_after(items, idx, &labels, &smc, &contracts, 3)
        {
            continue;
        }
        to_drop.push(idx);
    }
    if to_drop.is_empty() {
        return false;
    }
    for idx in to_drop.into_iter().rev() {
        items.remove(idx);
    }
    true
}

/// PH019 — `LDA op / TAX|TAY` → `LDX|LDY op` when `.A` is dead.
///
/// Saves 1 byte per match (drops the 1-byte transfer). Flags after
/// the rewrite are set by `LDX|LDY op` based on the same loaded
/// value, so any downstream branch that read the LDA's flags
/// continues to read equivalent flags.
///
/// Addressing-mode constraints follow the 6502's asymmetry:
///   * `LDX` accepts: `#imm`, zp, zp,Y, abs, abs,Y. **Not** zp,X /
///     abs,X / (zp,X) / (zp),Y.
///   * `LDY` accepts: `#imm`, zp, zp,X, abs, abs,X. **Not** zp,Y /
///     abs,Y / (zp,X) / (zp),Y.
///
/// We refuse any addressing mode the destination instruction can't
/// represent.
fn ph019_lda_to_index_load(items: &mut Vec<Item>) -> bool {
    let config = liveness_config_with_helpers();
    let live = analyze_register_liveness_with_config(items, &config);

    // Plan the rewrites first — applying them in-place would
    // invalidate the live-info indices.
    enum Plan {
        ToLdx,
        ToLdy,
    }
    let mut plans: Vec<(usize, usize, Plan, String)> = Vec::new();
    let mut i = 0;
    while i < items.len() {
        let Some(Item::Insn(insn)) = items.get(i) else {
            i += 1;
            continue;
        };
        if insn.mnem != "LDA" {
            i += 1;
            continue;
        }
        let Some(op) = insn.operand.clone() else {
            i += 1;
            continue;
        };
        let Some(j) = next_struct(items, i) else {
            i += 1;
            continue;
        };
        let Some(Item::Insn(next)) = items.get(j) else {
            i += 1;
            continue;
        };
        let plan = match next.mnem.as_str() {
            "TAX" if ldx_accepts(&op) => Plan::ToLdx,
            "TAY" if ldy_accepts(&op) => Plan::ToLdy,
            _ => {
                i += 1;
                continue;
            }
        };
        // `.A` must be dead at the live-out of the TAX/TAY for the
        // rewrite to be sound: nothing downstream may read the
        // value that LDA placed in A.
        if live.is_live_out(j, Reg::A) {
            i += 1;
            continue;
        }
        plans.push((i, j, plan, op));
        i = j + 1;
    }
    if plans.is_empty() {
        return false;
    }
    // Reverse so removals don't disturb earlier indices.
    plans.sort_by(|a, b| b.0.cmp(&a.0));
    for (lda_idx, transfer_idx, plan, op) in plans {
        let new_mnem = match plan {
            Plan::ToLdx => "LDX",
            Plan::ToLdy => "LDY",
        };
        if let Some(Item::Insn(insn)) = items.get_mut(lda_idx) {
            insn.mnem = new_mnem.to_string();
            insn.operand = Some(op);
        }
        items.remove(transfer_idx);
    }
    true
}

/// True iff the operand's addressing mode is one `LDX` accepts.
fn ldx_accepts(op: &str) -> bool {
    let upper = op.trim().to_ascii_uppercase();
    if upper.starts_with("#") {
        return true;
    }
    if upper.starts_with("(") {
        return false;
    } // indirect
// LDX has zp, zp,Y, abs, abs,Y (no zp,X / abs,X).
    if upper.ends_with(",X") {
        return false;
    }
    true
}

/// True iff the operand's addressing mode is one `LDY` accepts.
fn ldy_accepts(op: &str) -> bool {
    let upper = op.trim().to_ascii_uppercase();
    if upper.starts_with("#") {
        return true;
    }
    if upper.starts_with("(") {
        return false;
    }
    // LDY has zp, zp,X, abs, abs,X (no zp,Y / abs,Y).
    if upper.ends_with(",Y") {
        return false;
    }
    true
}

/// Builds a `RegisterLivenessConfig` populated with the contracts of
/// the helpers we emit ourselves. Any helper not listed here falls
/// back to `CONSERVATIVE_CALL` (defs=ALL, uses=ALL), which is always
/// sound — it just leaves fewer optimisation opportunities on the
/// table.
///
/// Contracts must be SOUND: a contract that under-states what a
/// helper clobbers will let liveness drop a value the helper still
/// needs. The fallback is conservative; we only register a tighter
/// contract when we're sure of the helper's preservation guarantees.
fn liveness_config_with_helpers() -> RegisterLivenessConfig {
    // CHROUT (KERNAL $FFD2) consumes .A and preserves .X and .Y.
    // Documented in the C64 Programmer's Reference Guide. This
    // contract is what `__STR_PRINT`'s `LDA / JSR / INY / DEX / BNE`
    // loop relies on — model it conservatively (uses=A, defs=A) so
    // PH015 can't drop the TAX that initialises the loop counter.
    let chrout = RegEffect::new(RegSet::A, RegSet::A);
    // `__CHROUT_<imm>` helpers are `LDA #imm / JMP $FFD2` — they
    // load .A from the immediate and tail-call CHROUT. The caller
    // doesn't need .A to come in (the helper sources it), and on
    // return only .A has been clobbered. uses=∅, defs=A.
    let chrout_imm = RegEffect::new(RegSet::EMPTY, RegSet::A);

    let print_gap = RegEffect::new(RegSet::EMPTY, RegSet::A);
    // GETIN (KERNAL $FFE4) returns the next queued key in .A.

    let getin = RegEffect::new(RegSet::EMPTY, RegSet::A);
    let no_reg_inputs = RegEffect::new(RegSet::EMPTY, RegSet::ALL);

    // BASIC FAC helper conventions: most take an operand pointer in
    // .A,.Y (or .X,.Y for MOVMF, .Y alone for BYTEFAC) and clobber
    // all three registers internally. Modelling the precise USE
    // pattern lets backward-liveness see that the JSR doesn't depend
    // on, say, .X — without that, every JSR site looks like a
    // conservative full-register-use barrier and rules like PH150
    // (drop dead-FAC-def JSRs) never fire because some upstream .X
    // looks falsely live across the call.
    //
    // Sources:
    //   * C64 Programmer's Reference Guide, Appendix B.
    //   * Runtime helper register behavior verified in `runtime.rs`.
    let mem_to_fac = RegEffect::new(RegSet::A.union(RegSet::Y), RegSet::ALL);
    let fac_to_mem = RegEffect::new(RegSet::X.union(RegSet::Y), RegSet::ALL);
    let byte_to_fac = RegEffect::new(RegSet::Y, RegSet::ALL);
    let givayf = RegEffect::new(RegSet::A.union(RegSet::Y), RegSet::ALL);
    let fac_only = no_reg_inputs;

    RegisterLivenessConfig::default()
        .with_jsr_effect("$FFD2", chrout)
        .with_jsr_effect("$AB3B", print_gap) // PRINT's channel-aware gap column
        .with_jsr_effect("$FFE4", getin)
        // FAC ↔ memory
        .with_jsr_effect("$BBA2", mem_to_fac) // MOVFM
        .with_jsr_effect("$BBD4", fac_to_mem) // MOVMF
// Float arithmetic with memory operand
        .with_jsr_effect("$B867", mem_to_fac) // FADD
        .with_jsr_effect("$B850", mem_to_fac) // FSUB
        .with_jsr_effect("$BA28", mem_to_fac) // FMULT
        .with_jsr_effect("$BB0F", mem_to_fac) // FDIV
        .with_jsr_effect("$BF7B", fac_only) // FPWRT (operates on FAC/ARG)
        .with_jsr_effect("$BC0C", fac_only) // FACARG (FAC -> ARG)
// Float ↔ integer
        .with_jsr_effect("$B391", givayf) // GIVAYF
        .with_jsr_effect("$B3A2", byte_to_fac) // BYTEFAC
        .with_jsr_effect("$B7F7", fac_only) // FACWORD
        .with_jsr_effect("$B1BF", fac_only) // AYINT
// Numeric one-arg helpers (FAC → FAC); operand is FAC, no
// register inputs.
        .with_jsr_effect("$BC58", fac_only) // ABS
        .with_jsr_effect("$BCCC", fac_only) // INT
        .with_jsr_effect("$BC39", fac_only) // SGN
        .with_jsr_effect("$BF71", fac_only) // SQR
        .with_jsr_effect("$E26B", fac_only) // SIN
        .with_jsr_effect("$E264", fac_only) // COS
        .with_jsr_effect("$E2B4", fac_only) // TAN
        .with_jsr_effect("$E30E", fac_only) // ATN
        .with_jsr_effect("$B9EA", fac_only) // LOG
        .with_jsr_effect("$BFED", fac_only) // EXP
        .with_jsr_effect("$E097", fac_only) // RND
        .with_jsr_effect("$BDDD", fac_only) // FOUT
// Compiler-emitted helpers
        .with_jsr_effect("__PRINT_FAC", no_reg_inputs)
        .with_jsr_effect("__FAC_BYTE", no_reg_inputs)
        .with_jsr_effect("__FAC_BYTE_OK", no_reg_inputs)
        .with_jsr_effect("__AYINT", no_reg_inputs)
        .with_jsr_effect("__FAC_TO_INT16", no_reg_inputs)
        .with_jsr_effect("__FAC_TO_INT16_NOTRAP", no_reg_inputs)
        // DIV16: takes inputs from LINNUM/__DIV16_DEN (memory),
        // clobbers all three registers internally. No reg inputs.
        .with_jsr_effect("__DIV16", no_reg_inputs)
        // Per-imm CHROUT helpers — covers `__CHROUT_0D`,
        // `__CHROUT_20`, etc.
        .with_jsr_prefix("__CHROUT_", chrout_imm)
        // PH106 supplies A=0, but the caller still supplies Y.
        .with_jsr_prefix("__LD_BYTE_FAC", byte_to_fac)
        .with_jsr_prefix("__LDV_", no_reg_inputs)
}

fn peephole_flag_liveness_config() -> FlagLivenessConfig {
    with_known_flag_contracts(FlagLivenessConfig {
        return_live_out: FlagSet::EMPTY,
        ..FlagLivenessConfig::default()
    })
}

fn conservative_return_flag_config() -> FlagLivenessConfig {
    with_known_flag_contracts(FlagLivenessConfig::default())
}

/// Flag contracts for the ROM entry points and compiler-emitted helpers that provably consume
/// no incoming flag and clobber all four.
fn with_known_flag_contracts(base: FlagLivenessConfig) -> FlagLivenessConfig {
    let call_defs_flags = FlagEffect::new(FlagSet::EMPTY, FlagSet::ALL);
    base.with_jsr_effect("$FFD2", call_defs_flags)
        // $AB3B opens with `LDA $13`, so no incoming flag survives being read; it ends in CHROUT, so treat all four as destroyed.
        .with_jsr_effect("$AB3B", call_defs_flags)
        .with_jsr_effect("$FFE4", call_defs_flags)
        .with_jsr_effect("$BBA2", call_defs_flags)
        .with_jsr_effect("$BBD4", call_defs_flags)
        .with_jsr_effect("$B391", call_defs_flags)
        .with_jsr_effect("$B3A2", call_defs_flags)
        .with_jsr_effect("$B7F7", call_defs_flags)
        .with_jsr_effect("$B1BF", call_defs_flags)
        .with_jsr_effect("$B867", call_defs_flags)
        .with_jsr_effect("$B850", call_defs_flags)
        .with_jsr_effect("$BA28", call_defs_flags)
        .with_jsr_effect("$BB0F", call_defs_flags)
        .with_jsr_effect("$BDDD", call_defs_flags)
        .with_jsr_effect("__PRINT_FAC", call_defs_flags)
        .with_jsr_effect("__FAC_BYTE", call_defs_flags)
        .with_jsr_effect("__FAC_BYTE_OK", call_defs_flags)
        .with_jsr_effect("__AYINT", call_defs_flags)
        .with_jsr_effect("__FAC_TO_INT16", call_defs_flags)
        .with_jsr_effect("__FAC_TO_INT16_NOTRAP", call_defs_flags)
        .with_jsr_prefix("__CHROUT_", call_defs_flags)
        .with_jsr_prefix("__LD_BYTE_FAC", call_defs_flags)
        .with_jsr_prefix("__LDV_", call_defs_flags)
        .with_jsr_prefix("__READ_", call_defs_flags)
}

// ----- Rules ----------------------------------------------------------------

/// PH001 — dead branch/jump to the very next instruction.
///
/// ```text
///     JMP L1
/// L1:
/// ```
///
/// The branch is a no-op (already falling through to the same place).
/// Drop it. Same for any conditional branch — falling through and
/// jumping land at the same point either way.
fn ph001_dead_jump_to_next(items: &mut Vec<Item>) -> bool {
    let mut changed = false;
    let mut i = 0;
    while i < items.len() {
        if let Item::Insn(insn) = &items[i] {
            if (insn.mnem == "JMP" || is_cond_branch(&insn.mnem))
                && let Some(target) = &insn.operand
            {
                // Remove jumps to the immediately following label group.
                let mut j = i + 1;
                let mut dead = false;
                while j < items.len() {
                    match &items[j] {
                        Item::Blank | Item::Comment(_) => j += 1,
                        Item::Label(name) if name == target => {
                            dead = true;
                            break;
                        }
                        Item::Label(_) => j += 1,
                        _ => break,
                    }
                }
                if dead {
                    items.remove(i);
                    changed = true;
                    continue;
                }
            }
        }
        i += 1;
    }
    changed
}

/// PH002 — dead code after an unconditional terminator.
///
/// After `JMP / RTS / RTI`, instructions remain unreachable until we
/// hit a label (which could be a branch target). Drop those orphan
/// instructions, but never directives or labels.
fn ph002_dead_after_terminator(items: &mut Vec<Item>) -> bool {
    let mut changed = false;
    let mut i = 0;
    while i < items.len() {
        let terminator = match &items[i] {
            Item::Insn(insn) => is_uncond_terminator(&insn.mnem),
            _ => false,
        };
        if terminator {
            // Walk forward dropping insns until we hit a label or a
            // directive. Blanks and comments stay (visual-noise).
            let mut j = i + 1;
            while j < items.len() {
                match &items[j] {
                    Item::Insn(_) => {
                        items.remove(j);
                        changed = true;
                        // don't advance j — next item shifted in
                    }
                    Item::Label(_) | Item::Directive(_) => break,
                    Item::Blank | Item::Comment(_) | Item::Origin(_) => j += 1,
                }
            }
        }
        i += 1;
    }
    changed
}

/// PH003 — `JSR helper` immediately followed by `RTS` becomes
/// `JMP helper`. Saves 1 byte and avoids the spurious push/pop pair.
///
/// Crucially, the JSR's target must itself end with `RTS` for this to
/// be safe. We can't know that universally, but we CAN know it for:
///   * Compiler-owned helpers we generate.
///   * Documented BASIC ROM routines that return with `RTS`.
///   * KERNAL routines that return with `RTS` (e.g. CHROUT).
///
/// For initial caution, only fire when the operand starts with `__`
/// (compiler helper) or is a 4-digit `$XXXX` literal (any ROM/KERNAL
/// jump target — we trust documented entry points to RTS-return).
fn ph003_jsr_rts_to_jmp(items: &mut Vec<Item>) -> bool {
    let mut changed = false;
    let mut i = 0;
    while i + 1 < items.len() {
        let do_swap = match (&items[i], next_struct(items, i).map(|j| (&items[j], j))) {
            (Item::Insn(jsr), Some((Item::Insn(rts), j)))
                if jsr.mnem == "JSR" && rts.mnem == "RTS" && rts.operand.is_none() =>
            {
                // No label between JSR and RTS — next_struct skips
                // only Blank/Comment, so a Label would have surfaced.
                Some((j, jsr.operand.clone()))
            }
            _ => None,
        };
        if let Some((j, target)) = do_swap {
            // Mutate the JSR into JMP; remove the RTS.
            if let Item::Insn(insn) = &mut items[i] {
                insn.mnem = "JMP".to_string();
                insn.operand = target;
            }
            items.remove(j);
            changed = true;
            // Don't advance i — the new JMP might enable PH001.
            continue;
        }
        i += 1;
    }
    changed
}

/// PH004 — jump cascade collapse.
///
/// If a `JMP X` (or conditional branch) targets a label whose very
/// first instruction is itself an unconditional `JMP Y`, retarget the
/// first jump straight to `Y`. The trampoline label stays in place
/// (other call sites might still reach it); a later dead-code pass
/// can prune it once nothing references it.
///
/// Same rule applies to `Bxx X` when X is a `JMP Y` trampoline:
/// rewrite to `Bxx Y` (semantics identical, one indirection saved).
///
/// We refuse to follow a branch's target into another *conditional*
/// branch — that would change semantics if the second branch's
/// condition differs from the first.
fn ph004_jump_cascade(items: &mut Vec<Item>) -> bool {
    let mut changed = false;
    // First pass: build label → next-instruction-mnem-and-operand map.
    let trampolines = collect_trampolines(items);
    if trampolines.is_empty() {
        return false;
    }
    let zp = ZpEquates::scan(items);
    // We can't iter_mut and call label_within_branch_range (which
    // borrows the list) at the same time. Stage rewrites first, then
    // apply.
    let mut rewrites: Vec<(usize, String)> = Vec::new();
    for (idx, it) in items.iter().enumerate() {
        if let Item::Insn(insn) = it
            && (insn.mnem == "JMP" || is_cond_branch(&insn.mnem))
            && let Some(target) = &insn.operand
            && let Some(final_target) = trampolines.get(target.as_str())
            && final_target != target
        {
            // 6502 conditional branches are 8-bit PC-relative.
            //   * Only retarget when the resolved label is itself a
            //     label (not a `$XXXX` ROM address).
            //   * Only retarget when the new target is in 8-bit
            //     reach — otherwise the assembler's long-branch
            //     fix-up would balloon the code.
            if is_cond_branch(&insn.mnem) {
                if !is_label_target(final_target) {
                    continue;
                }
                if !label_within_branch_range(items, idx, final_target, 100, &zp) {
                    continue;
                }
            }
            rewrites.push((idx, final_target.clone()));
        }
    }
    for (idx, new_target) in rewrites {
        if let Item::Insn(insn) = &mut items[idx] {
            insn.operand = Some(new_target);
            changed = true;
        }
    }
    changed
}

/// Route JSR directly through a JMP trampoline.
fn ph328_thread_calls_through_trampolines(items: &mut [Item]) -> bool {
    let trampolines = collect_trampolines(items);
    if trampolines.is_empty() {
        return false;
    }
    let mut changed = false;
    for it in items.iter_mut() {
        if let Item::Insn(insn) = it
            && insn.mnem == "JSR"
            && let Some(target) = &insn.operand
            && let Some(final_target) = trampolines.get(target.as_str())
            && final_target != target
            && !final_target.starts_with('(')
        {
            insn.operand = Some(final_target.clone());
            changed = true;
        }
    }
    changed
}

/// Walk the items once, finding labels whose first instruction is
/// `JMP <target>`. Return a map from trampoline label to terminal
/// target. Resolves multi-hop chains (A → B → C ⇒ A maps to C).
fn collect_trampolines(items: &[Item]) -> std::collections::HashMap<String, String> {
    use std::collections::HashMap;
    let mut direct: HashMap<String, String> = HashMap::new();
    let mut i = 0;
    while i < items.len() {
        if let Item::Label(name) = &items[i]
            && let Some(j) = next_struct(items, i)
        {
            if let Item::Insn(insn) = &items[j]
                && insn.mnem == "JMP"
                && let Some(t) = &insn.operand
            {
                direct.insert(name.clone(), t.clone());
            }
        }
        i += 1;
    }
    // Resolve chains. Cap at 8 hops to avoid loops in pathological
    // self-referencing trampolines.
    let mut resolved: HashMap<String, String> = HashMap::new();
    for (k, v) in direct.iter() {
        let mut cur = v.clone();
        for _ in 0..8 {
            match direct.get(&cur) {
                Some(nxt) if nxt != &cur => cur = nxt.clone(),
                _ => break,
            }
        }
        resolved.insert(k.clone(), cur);
    }
    resolved
}

/// Maximum relative-branch distance.
const PH005_REACH: usize = 127;

fn ph005_branch_around_jmp(items: &mut Vec<Item>) -> bool {
    let mut changed = false;
    let zp = ZpEquates::scan(items);
    let mut i = 0;
    while i + 2 < items.len() {
        let pattern = match (
            &items[i],
            next_struct(items, i),
            next_struct(items, i).and_then(|j| next_struct(items, j).map(|k| (j, k))),
        ) {
            (Item::Insn(branch), Some(_), Some((j, k))) if is_cond_branch(&branch.mnem) => {
                if let Item::Insn(jmp) = &items[j]
                    && jmp.mnem == "JMP"
                    && let Item::Label(target_label) = &items[k]
                    && Some(target_label) == branch.operand.as_ref()
                    && let Some(inverted) = invert_branch(&branch.mnem)
                    // The new branch needs to reach the JMP's target.
                    // Since branches are PC-relative-8, refuse if the
                    // JMP went to an absolute ROM address.
                    && jmp.operand.as_deref().map(is_label_target).unwrap_or(false)
                    // …and refuse if the label is too far away — the
                    // assembler would expand it back to a long-branch
                    // fix-up and we'd net-lose bytes.
                    && label_within_branch_range(
                        items,
                        i,
                        jmp.operand.as_deref().unwrap(),
                        PH005_REACH,
                        &zp,
                    )
                {
                    Some((j, inverted, jmp.operand.clone()))
                } else {
                    None
                }
            }
            _ => None,
        };
        if let Some((j, inv, jmp_target)) = pattern {
            if let Item::Insn(branch) = &mut items[i] {
                branch.mnem = inv.to_string();
                branch.operand = jmp_target;
            }
            items.remove(j);
            changed = true;
            continue;
        }
        i += 1;
    }
    changed
}

/// Invert branches over JMP when intervening labels have no references.
fn ph315_branch_over_jmp_past_dead_labels(items: &mut Vec<Item>) -> bool {
    let zp = ZpEquates::scan(items);
    let smc = smc_patched_labels(items);
    let mut changed = false;
    // Recount references after each rewrite.
    while let Some((i, j)) = ph315_find_site(items, &label_ref_counts(items), &smc, &zp) {
        let inverted = match &items[i] {
            Item::Insn(branch) => invert_branch(&branch.mnem),
            _ => None,
        };
        let else_target = match &items[j] {
            Item::Insn(jmp) => jmp.operand.clone(),
            _ => None,
        };
        if let (Some(inv), Some(target), Item::Insn(branch)) =
            (inverted, else_target, &mut items[i])
        {
            branch.mnem = inv.to_string();
            branch.operand = Some(target);
            // Remove the JMP and leave unused labels for their own pass.
            items.remove(j);
            changed = true;
        } else {
            break;
        }
    }
    changed
}

/// The first PH315 window in `items`, as `(branch index, JMP index)`.
fn ph315_find_site(
    items: &[Item],
    refs: &std::collections::HashMap<String, usize>,
    smc: &std::collections::HashSet<String>,
    zp: &ZpEquates,
) -> Option<(usize, usize)> {
    for i in 0..items.len() {
        let Item::Insn(branch) = &items[i] else { continue };
        if !is_cond_branch(&branch.mnem) {
            continue;
        }
        let Some(then_label) = branch.operand.as_deref() else { continue };
        // A `$XXXX` or `(ptr)` operand is not a label to match against.
        if !is_label_target(then_label) {
            continue;
        }
        // Keep instructions whose bytes change at runtime.
        if insn_is_smc_target(items, i, smc) {
            continue;
        }

        // Intervening labels must have no instruction or data references.
        let mut j = i + 1;
        let mut orphans = 0usize;
        while j < items.len() {
            match &items[j] {
                Item::Blank | Item::Comment(_) => j += 1,
                Item::Label(name) if refs.get(name).copied().unwrap_or(0) == 0 => {
                    orphans += 1;
                    j += 1;
                }
                _ => break,
            }
        }
        let Item::Insn(jmp) = items.get(j)? else {
            continue;
        };
        if jmp.mnem != "JMP" {
            continue;
        }
        let Some(else_label) = jmp.operand.as_deref() else {
            continue;
        };
        if else_label == then_label || !is_label_target(else_label) {
            continue;
        }
        // Keep jumps with runtime operand writes.
        if insn_is_smc_target(items, j, smc) || smc.contains(else_label) {
            continue;
        }

        // Labels after the JMP share the same address.
        let mut k = j + 1;
        let mut first_is_then = true;
        let mut found = false;
        let mut first_label = true;
        while k < items.len() {
            match &items[k] {
                Item::Blank | Item::Comment(_) => k += 1,
                Item::Label(name) => {
                    if first_label {
                        first_is_then = name == then_label;
                        first_label = false;
                    }
                    found |= name == then_label;
                    k += 1;
                }
                _ => break,
            }
        }
        if !found {
            continue;
        }
        if orphans == 0 && first_is_then {
            continue;
        }
        // Keep label groups containing code modified at runtime.
        if items[j + 1..k]
            .iter()
            .any(|it| matches!(it, Item::Label(n) if smc.contains(n)))
        {
            continue;
        }
        // Stop at reentry markers, data directives and origins.
        if items[i + 1..k].iter().any(|it| {
            matches!(it, Item::Directive(_) | Item::Origin(_)) || is_sys_wrap_marker(it)
        }) {
            continue;
        }
        // The inverted branch must reach the jump target.
        if !label_within_branch_range(items, i, else_label, PH005_REACH, zp) {
            continue;
        }
        // An origin makes the distance estimate unreliable.
        if ph315_origin_blocks(items, i, else_label) {
            continue;
        }
        return Some((i, j));
    }
    None
}

/// Whether an origin lies between the instruction and target.
fn ph315_origin_blocks(items: &[Item], from: usize, target: &str) -> bool {
    let Some(t) = items
        .iter()
        .position(|it| matches!(it, Item::Label(n) if n == target))
    else {
        return true;
    };
    let (lo, hi) = if t < from { (t, from) } else { (from, t) };
    items[lo..=hi]
        .iter()
        .any(|it| matches!(it, Item::Origin(_)))
}

/// Share matching instruction tails across straight-line blocks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CurlinHi {
    /// No reachable path yet.
    Unreached,
    Known(u8),
    Unknown,
}

impl CurlinHi {
    fn meet(self, other: Self) -> Self {
        match (self, other) {
            (CurlinHi::Unreached, x) | (x, CurlinHi::Unreached) => x,
            (CurlinHi::Known(a), CurlinHi::Known(b)) if a == b => CurlinHi::Known(a),
            (CurlinHi::Known(_), CurlinHi::Known(_)) => CurlinHi::Unknown,
            _ => CurlinHi::Unknown,
        }
    }
}

/// Can a store/RMW through `op` land on zero-page `addr`?
fn store_can_hit_zp(op: &str, zp_equ: &std::collections::HashMap<String, u16>, addr: u16) -> bool {
    let op = op.trim();
    if op.starts_with('(') {
        return true;
    }
    let (base, indexed) = match op.split_once(',') {
        Some((b, idx)) => {
            let idx = idx.trim();
            if idx != "X" && idx != "Y" {
                return true;
            }
            (b.trim(), true)
        }
        None => (op, false),
    };
    // `LABEL` or `LABEL+n`, resolved through the ZP EQUs.
    let (name, off) = match base.split_once('+') {
        Some((n, o)) => match o.trim().parse::<u16>() {
            Ok(off) => (n.trim(), off),
            Err(_) => return true,
        },
        None => (base, 0u16),
    };
    let resolved = if let Some(hex) = name.strip_prefix('$') {
        if !hex.chars().all(|c| c.is_ascii_hexdigit()) || hex.is_empty() || hex.len() > 4 {
            return true;
        }
        match u16::from_str_radix(hex, 16) {
            Ok(v) => {
                // A two-digit operand is zero-page addressing, and `$XX,X` wraps within page 0 ; it can reach any cell.
                if hex.len() <= 2 && indexed {
                    return addr < 0x0100;
                }
                v
            }
            Err(_) => return true,
        }
    } else if name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && !name.is_empty() {
        match zp_equ.get(name) {
            Some(&v) => v,
            // Not zero-page promoted, so it lives in the image.
            None => return false,
        }
    } else {
        return true;
    };
    let lo = resolved.saturating_add(off);
    if indexed {
        addr >= lo && addr <= lo.saturating_add(0xFF)
    } else {
        addr == lo
    }
}

/// What one item does to the tracked cell, before the dataflow looks at context.
#[derive(Clone, Copy, PartialEq, Eq)]
enum HiEffect {
    /// `LDA #k / STA <cell>` ; the stamp.
    Stamp(u8),

    Clobber,
    /// `JSR <label>`: transparent iff the callee cannot reach a `Stamp` or a `Clobber`.
    Call(Option<usize>),
    Transparent,
}

/// Recognized ON GOTO tables and their control flow edges.
struct OnDispatches {
    targets: std::collections::HashMap<usize, Vec<usize>>,
    table_rows: std::collections::HashSet<usize>,
}

fn on_dispatches(items: &[Item], labels: &std::collections::HashMap<String, usize>) -> OnDispatches {
    let mut out = OnDispatches {
        targets: std::collections::HashMap::new(),
        table_rows: std::collections::HashSet::new(),
    };
    let insn_at = |i: usize| match &items[i] {
        Item::Insn(insn) => Some((insn.mnem.as_str(), insn.operand.as_deref().unwrap_or("").trim())),
        _ => None,
    };
    // The entries of the `.byte` row under `label`, each with `part` (`<` or `>`) stripped.
    let row = |label: &str, part: char| -> Option<(usize, Vec<String>)> {
        let at = next_struct(items, *labels.get(label)?)?;
        let Item::Directive(raw) = &items[at] else {
            return None;
        };
        let body = raw.split(';').next()?.trim().strip_prefix(".byte")?;
        let entries = body
            .split(',')
            .map(|e| e.trim().strip_prefix(part).map(str::to_string))
            .collect::<Option<Vec<_>>>()?;
        Some((at, entries))
    };
    for i in 0..items.len() {
        if insn_at(i) != Some(("JMP", "($22)")) {
            continue;
        }
        let mut back = Vec::with_capacity(4);
        let mut j = i;
        for _ in 0..4 {
            let Some(p) = prev_struct(items, j) else { break };
            let Some(insn) = insn_at(p) else { break };
            back.push(insn);
            j = p;
        }
        let [("STA", "$23"), ("LDA", hi), ("STA", "$22"), ("LDA", lo)] = back.as_slice() else {
            continue;
        };
        let (Some(lo), Some(hi)) = (lo.strip_suffix("-1,X"), hi.strip_suffix("-1,X")) else {
            continue;
        };
        let (Some(k), Some(k_hi)) = (lo.strip_prefix("__ON_LO_"), hi.strip_prefix("__ON_HI_")) else {
            continue;
        };
        if k != k_hi {
            continue;
        }
        let (Some((lo_at, lo_row)), Some((hi_at, hi_row))) = (row(lo, '<'), row(hi, '>')) else {
            continue;
        };
        if lo_row != hi_row {
            continue;
        }
        let Some(to) = lo_row.iter().map(|l| labels.get(l).copied()).collect::<Option<Vec<_>>>()
        else {
            continue;
        };
        out.targets.insert(i, to);
        out.table_rows.insert(lo_at);
        out.table_rows.insert(hi_at);
    }
    out
}

fn ph306_classify(
    items: &[Item],
    labels: &std::collections::HashMap<String, usize>,
    zp_equ: &std::collections::HashMap<String, u16>,
    cell: u16,
    dispatches: &OnDispatches,
) -> Vec<HiEffect> {
    let cell_op = format!("${cell:02X}");
    let mut out = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let eff = match item {
            // A `.byte` blob is data, but `SYS` can enter one, and an `*=` moves the assembler somewhere new.
            Item::Directive(s) if s.trim_start().starts_with('.') => HiEffect::Clobber,
            Item::Origin(_) => HiEffect::Clobber,
            Item::Insn(insn) => {
                let op = insn.operand.as_deref().unwrap_or("");
                match insn.mnem.as_str() {
                    "STA" | "STX" | "STY" => {
                        if op.trim() == cell_op
                            && insn.mnem == "STA"
                            && let Some(p) = prev_struct(items, i)
                            && let Item::Insn(prev) = &items[p]
                            && prev.mnem == "LDA"
                            && let Some(imm) = prev.operand.as_deref()
                            && let Some(body) = imm.trim().strip_prefix('#')
                            && let Some(k) = parse_static_imm_byte(body)
                        {
                            HiEffect::Stamp(k)
                        } else if store_can_hit_zp(op, zp_equ, cell) {
                            HiEffect::Clobber
                        } else {
                            HiEffect::Transparent
                        }
                    }
                    "INC" | "DEC" | "ASL" | "LSR" | "ROL" | "ROR" => {
                        // `ASL`/`LSR`/`ROL`/`ROR` with no operand are the accumulator forms and touch no memory.
                        if insn.operand.is_none() || op.trim() == "A" {
                            HiEffect::Transparent
                        } else if store_can_hit_zp(op, zp_equ, cell) {
                            HiEffect::Clobber
                        } else {
                            HiEffect::Transparent
                        }
                    }
                    "JMP" if op.trim_start().starts_with('(') => {
                        if dispatches.targets.contains_key(&i) {
                            HiEffect::Transparent
                        } else {
                            HiEffect::Clobber
                        }
                    }
                    "JSR" => {
                        let t = op.trim();
                        if t.starts_with('(') {
                            HiEffect::Clobber
                        } else if t.starts_with('$') {
                            // A BASIC-ROM or KERNAL entry.
                            HiEffect::Transparent
                        } else {
                            HiEffect::Call(labels.get(t).copied())
                        }
                    }
                    _ => HiEffect::Transparent,
                }
            }
            _ => HiEffect::Transparent,
        };
        out.push(eff);
    }
    out
}

/// Forward control-flow edges, including `JSR` into the callee body so reachability can see what a call can do.
fn ph306_succs(
    items: &[Item],
    idx: usize,
    labels: &std::collections::HashMap<String, usize>,
    effects: &[HiEffect],
    dispatches: &OnDispatches,
    out: &mut Vec<usize>,
) {
    out.clear();
    let mut fallthrough = true;
    if let Item::Insn(insn) = &items[idx] {
        let op = insn.operand.as_deref().unwrap_or("").trim();
        if insn.mnem == "JMP" {
            fallthrough = false;
            if let Some(&t) = labels.get(op) {
                out.push(t);
            } else if let Some(to) = dispatches.targets.get(&idx) {
                out.extend(to);
            }
        } else if is_uncond_terminator(&insn.mnem) {
            fallthrough = false;
        } else if is_cond_branch(&insn.mnem) {
            if let Some(&t) = labels.get(op) {
                out.push(t);
            }
        } else if insn.mnem == "JSR"
            && let HiEffect::Call(Some(t)) = effects[idx]
        {
            out.push(t);
        }
    } else if matches!(&items[idx], Item::Directive(s) if s.trim_start().starts_with('.')) {
        // Opaque data: no flow out of it.
        fallthrough = false;
    }
    if fallthrough && idx + 1 < items.len() {
        out.push(idx + 1);
    }
}

fn ph306_curlin_known(items: &mut Vec<Item>, cell: u16) -> bool {
    let mut labels: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (i, it) in items.iter().enumerate() {
        if let Item::Label(name) = it {
            labels.insert(name.clone(), i);
        }
    }
    if labels.is_empty() {
        return false;
    }
    let zp_equ: std::collections::HashMap<String, u16> = collect_equ_values(items)
        .into_iter()
        .filter(|(_, v)| *v < 0x0100)
        .collect();
    let dispatches = on_dispatches(items, &labels);
    let effects = ph306_classify(items, &labels, &zp_equ, cell, &dispatches);
    if !effects.iter().any(|e| matches!(e, HiEffect::Stamp(_))) {
        return false;
    }

    let n = items.len();
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut succ_buf = Vec::new();
    for i in 0..n {
        ph306_succs(items, i, &labels, &effects, &dispatches, &mut succ_buf);
        for &s in &succ_buf {
            preds[s].push(i);
        }
    }
    let mut touches = vec![false; n];
    let mut work: Vec<usize> = Vec::new();
    for i in 0..n {
        if matches!(
            effects[i],
            HiEffect::Stamp(_) | HiEffect::Clobber
        ) {
            touches[i] = true;
            work.push(i);
        }
    }
    while let Some(i) = work.pop() {
        for &p in &preds[i] {
            if !touches[p] {
                touches[p] = true;
                work.push(p);
            }
        }
    }

    // Merge every value the callee can write before returning.
    let mut writes = vec![CurlinHi::Unreached; n];
    let mut work: Vec<usize> = Vec::new();
    for i in 0..n {
        writes[i] = match effects[i] {
            HiEffect::Stamp(k) => CurlinHi::Known(k),
            HiEffect::Clobber | HiEffect::Call(None) => CurlinHi::Unknown,
            _ => continue,
        };
        work.push(i);
    }
    while let Some(i) = work.pop() {
        for &p in &preds[i] {
            let merged = writes[p].meet(writes[i]);
            if merged != writes[p] {
                writes[p] = merged;
                work.push(p);
            }
        }
    }

    let mut external = vec![false; n];
    external[0] = true;
    for (i, it) in items.iter().enumerate() {
        let text = match it {
            Item::Insn(insn) => {
                let op = insn.operand.as_deref().unwrap_or("");
                let bare_control = matches!(insn.mnem.as_str(), "JMP" | "JSR")
                    || is_cond_branch(&insn.mnem);
                if bare_control && labels.contains_key(op.trim()) {
                    continue;
                }
                op.to_string()
            }
            // A dispatch table's entries are the dispatch's edges.
            Item::Directive(_) if dispatches.table_rows.contains(&i) => continue,
            Item::Directive(s) => s.clone(),
            _ => continue,
        };
        for tok in text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
            if tok.is_empty() {
                continue;
            }
            if let Some(&t) = labels.get(tok) {
                external[t] = true;
            }
        }
    }

    let mut state = vec![CurlinHi::Unreached; n];
    for i in 0..n {
        if external[i] {
            state[i] = CurlinHi::Unknown;
        }
    }

    let mut converged = false;
    for _ in 0..24 {
        let mut changed = false;
        for i in 0..n {
            let after = match effects[i] {
                HiEffect::Stamp(k) => CurlinHi::Known(k),
                HiEffect::Clobber => CurlinHi::Unknown,
                HiEffect::Call(Some(t)) => state[i].meet(writes[t]),
                HiEffect::Call(None) => CurlinHi::Unknown,
                HiEffect::Transparent => state[i],
            };
            if after == CurlinHi::Unreached {
                continue;
            }
            ph306_succs(items, i, &labels, &effects, &dispatches, &mut succ_buf);
            for &s in &succ_buf {
                let carried = match effects[i] {
                    HiEffect::Call(Some(t)) if s == t && s != i + 1 => state[i],
                    _ => after,
                };
                let merged = state[s].meet(carried);
                if merged != state[s] {
                    state[s] = merged;
                    changed = true;
                }
            }
        }
        if !changed {
            converged = true;
            break;
        }
    }
    if !converged {
        return false;
    }

    let mut drop = vec![false; n];
    let mut any = false;
    for i in 0..n {
        if let HiEffect::Stamp(k) = effects[i]
            && state[i] == CurlinHi::Known(k)
        {
            drop[i] = true;
            any = true;
        }
    }
    if !any {
        return false;
    }
    let mut idx = 0;
    items.retain(|_| {
        let keep = !drop[idx];
        idx += 1;
        keep
    });
    true
}

fn curlin_transparent_rom(addr: u16) -> bool {
    matches!(
        addr,
        // The three BASIC entries the walk clears.
        rt::MOVFM         // $BBA2 mem -> FAC
            | rt::FN_ABS
            | rt::QINT    // $BC9B FAC -> 32-bit int, unchecked Raw KERNAL.
            | rt::CHROUT | rt::CHRIN | rt::GETIN | rt::CLRCHN
            | rt::CHKIN | rt::CHKOUT | rt::KERNAL_OPEN | rt::KERNAL_CLOSE
            | rt::SETLFS | rt::SETNAM | rt::STOP | rt::PLOT | rt::RDTIM
    )

}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CurlinAct {
    /// `STA/STX/STY <cell>` ; a full definition.
    Def,
    /// A read-modify-write access that preserves dependence on the stored value.
    Rmw,
    /// Reads the cell: `PEEK(57)`/`PEEK(58)` lower to `LDA $39`/`LDA $3A`, and any indirect or zero-page-indexed operand could be aimed at it.
    Read,
    Transparent,
}

enum CurlinFlow {
    /// Falls through to the next item only.
    Straight,

    Return,
    /// Unconditional jump to a local label.
    Jump(usize),
    /// Conditional branch to a local label, plus fall-through.
    Branch(usize),
    /// `JSR` to a local label, plus fall-through.
    Call(usize),

    Opaque,
}

fn ph307_flow(
    items: &[Item],
    idx: usize,
    labels: &std::collections::HashMap<String, usize>,
) -> CurlinFlow {
    match &items[idx] {
        Item::Directive(s) if s.trim_start().starts_with('.') => CurlinFlow::Opaque,
        Item::Origin(_) => CurlinFlow::Opaque,
        Item::Insn(insn) => {
            let op = insn.operand.as_deref().unwrap_or("").trim();
            match insn.mnem.as_str() {
                "RTS" | "RTI" => CurlinFlow::Return,
                "BRK" => CurlinFlow::Opaque,
                "JMP" => {
                    if let Some(&t) = labels.get(op) {
                        CurlinFlow::Jump(t)
                    } else if let Some(a) = parse_plain_addr(op) {
                        if curlin_transparent_rom(a) {
                            CurlinFlow::Return
                        } else {
                            CurlinFlow::Opaque
                        }
                    } else {
                        CurlinFlow::Opaque
                    }
                }
                "JSR" => {
                    if let Some(&t) = labels.get(op) {
                        CurlinFlow::Call(t)
                    } else if let Some(a) = parse_plain_addr(op) {
                        if curlin_transparent_rom(a) {
                            CurlinFlow::Straight
                        } else {
                            CurlinFlow::Opaque
                        }
                    } else {
                        CurlinFlow::Opaque
                    }
                }
                m if is_cond_branch(m) => match labels.get(op) {
                    Some(&t) => CurlinFlow::Branch(t),
                    None => CurlinFlow::Opaque,
                },
                _ => CurlinFlow::Straight,
            }
        }
        _ => CurlinFlow::Straight,
    }
}

fn ph307_acts(
    items: &[Item],
    zp_equ: &std::collections::HashMap<String, u16>,
    cell: u16,
) -> Vec<CurlinAct> {
    let cell_op = format!("${cell:02X}");
    items
        .iter()
        .map(|item| {
            let Item::Insn(insn) = item else {
                return CurlinAct::Transparent;
            };
            let op = insn.operand.as_deref().unwrap_or("");
            match insn.mnem.as_str() {
                "STA" | "STX" | "STY" => {
                    if op.trim() == cell_op {
                        CurlinAct::Def
                    } else {

                        CurlinAct::Transparent
                    }
                }
                "INC" | "DEC" | "ASL" | "LSR" | "ROL" | "ROR" => {
                    if insn.operand.is_none() || op.trim() == "A" {
                        CurlinAct::Transparent
                    } else if op.trim() == cell_op || store_can_hit_zp(op, zp_equ, cell) {
                        CurlinAct::Rmw
                    } else {
                        CurlinAct::Transparent
                    }
                }
                // Everything else that names memory: if the operand could be the cell, it is a read.
                _ => {
                    let t = op.trim();
                    if t.is_empty() || t.starts_with('#') {
                        CurlinAct::Transparent
                    } else if t == cell_op || store_can_hit_zp(op, zp_equ, cell) {
                        CurlinAct::Read
                    } else {
                        CurlinAct::Transparent
                    }
                }
            }
        })
        .collect()
}

/// Remove BASIC line-number stores whose values cannot be read.
fn ph307_dead_curlin_stamp(items: &mut Vec<Item>) -> bool {
    let mut changed = false;
    for cell in [
        u16::from(crate::runtime::CURLIN_LO),
        u16::from(crate::runtime::CURLIN_HI),
    ] {
        changed |= ph307_one_cell(items, cell);
    }
    changed
}

fn ph307_one_cell(items: &mut Vec<Item>, cell: u16) -> bool {
    let n = items.len();
    if n == 0 {
        return false;
    }
    let mut labels: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (i, it) in items.iter().enumerate() {
        if let Item::Label(name) = it {
            labels.insert(name.clone(), i);
        }
    }
    let zp_equ: std::collections::HashMap<String, u16> = collect_equ_values(items)
        .into_iter()
        .filter(|(_, v)| *v < 0x0100)
        .collect();
    let acts = ph307_acts(items, &zp_equ, cell);
    if !acts
        .iter()
        .any(|a| matches!(a, CurlinAct::Def | CurlinAct::Rmw))
    {
        return false;
    }
    let flows: Vec<CurlinFlow> = (0..n).map(|i| ph307_flow(items, i, &labels)).collect();

    let mut can_observe = vec![false; n];
    let mut converged = false;
    for _ in 0..64 {
        let mut dirty = false;
        for i in (0..n).rev() {
            let fall = |i: usize| i + 1 < n && can_observe[i + 1];
            let v = match acts[i] {
                CurlinAct::Read => true,
                _ => match flows[i] {
                    CurlinFlow::Opaque => true,
                    CurlinFlow::Return => false,
                    CurlinFlow::Jump(t) => can_observe[t],
                    CurlinFlow::Branch(t) => can_observe[t] || fall(i),
                    CurlinFlow::Call(t) => can_observe[t] || fall(i),

                    CurlinFlow::Straight => i + 1 >= n || can_observe[i + 1],
                },
            };
            if v != can_observe[i] {
                can_observe[i] = v;
                dirty = true;
            }
        }
        if !dirty {
            converged = true;
            break;
        }
    }
    if !converged {
        return false;
    }

    let mut live = vec![false; n];
    let mut converged = false;
    for _ in 0..64 {
        let mut dirty = false;
        for i in (0..n).rev() {
            let fall = |i: usize| i + 1 >= n || live[i + 1];
            let v = match acts[i] {
                CurlinAct::Def => false,
                CurlinAct::Read => true,
                // Transparent for liveness ; see [`CurlinAct::Rmw`].
                CurlinAct::Rmw | CurlinAct::Transparent => match flows[i] {
                    CurlinFlow::Opaque => true,
                    // The caller resumes with this value in the cell and can error there.
                    CurlinFlow::Return => true,
                    CurlinFlow::Jump(t) => live[t],
                    CurlinFlow::Branch(t) => live[t] || fall(i),
                    CurlinFlow::Call(t) => can_observe[t] || fall(i),
                    CurlinFlow::Straight => fall(i),
                },
            };
            if v != live[i] {
                live[i] = v;
                dirty = true;
            }
        }
        if !dirty {
            converged = true;
            break;
        }
    }
    if !converged {
        return false;
    }

    let smc = smc_patched_labels(items);
    let mut drop = vec![false; n];
    let mut any = false;
    for i in 0..n {
        if !matches!(acts[i], CurlinAct::Def | CurlinAct::Rmw) {
            continue;
        }

        let Item::Insn(insn) = &items[i] else { continue };
        if insn.operand.as_deref().map(str::trim) != Some(format!("${cell:02X}").as_str()) {
            continue;
        }
        if i + 1 < n && live[i + 1] {
            continue;
        }
        if i + 1 >= n {
            continue;
        }
        if insn_is_smc_target(items, i, &smc) {
            continue;
        }
        drop[i] = true;
        any = true;
    }
    if !any {
        return false;
    }
    let mut idx = 0;
    items.retain(|_| {
        let keep = !drop[idx];
        idx += 1;
        keep
    });
    true
}

fn ph305_sink_common_tails(items: &mut Vec<Item>, min_gain: usize) -> bool {
    let runs = ph305_collect_runs(items);
    if runs.len() < 2 {
        return false;
    }
    let smc = smc_patched_labels(items);
    let zp = ZpEquates::scan(items);
    // Longest tails first: a long shared tail subsumes every shorter one, and claiming it first
    // stops a 2-instruction match from fragmenting a 6-instruction one.
    let max_len = runs.iter().map(|r| r.len()).max().unwrap_or(0).min(24);
    for len in (2..=max_len).rev() {
        let mut groups: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        for (ri, run) in runs.iter().enumerate() {
            if run.len() < len {
                continue;
            }

            if insn_is_smc_target(items, run[run.len() - len], &smc) {
                continue;
            }
            let mut sig = String::new();
            for &idx in &run[run.len() - len..] {
                let Item::Insn(insn) = &items[idx] else {
                    continue;
                };
                sig.push_str(&insn.mnem);
                if let Some(op) = insn.operand.as_deref() {
                    sig.push(' ');
                    sig.push_str(op);
                }
                sig.push('\n');
            }
            groups.entry(sig).or_default().push(ri);
        }
        // Deterministic: consider groups by first member position.
        let mut keys: Vec<&String> = groups.keys().collect();
        keys.sort_by_key(|k| groups[*k][0]);
        for key in keys {
            let group = &groups[key];
            if group.len() < 2 {
                continue;
            }
            let keeper = &runs[group[0]];
            let tail_bytes: usize = keeper[keeper.len() - len..]
                .iter()
                .map(|&i| match &items[i] {
                    Item::Insn(insn) => approx_insn_bytes(insn, &zp),
                    _ => 0,
                })
                .sum();
            // Each sunk copy trades its tail for a 3-byte JMP.
            if tail_bytes < 3 + min_gain {
                continue;
            }
            let label = format!("__TAIL_{}", peephole_next_tail_id(items));
            // Edits: insert the label at the keeper's tail start, and replace every other
            // member's tail with a JMP.
            let mut edits: Vec<(usize, usize, Vec<Item>)> = Vec::new();
            let keep_at = keeper[keeper.len() - len];
            edits.push((keep_at, keep_at, vec![Item::Label(label.clone())]));
            for &other in &group[1..] {
                let run = &runs[other];
                let first = run[run.len() - len];
                let last = *run.last().unwrap();
                edits.push((
                    first,
                    last + 1,
                    vec![Item::Insn(Insn {
                        mnem: "JMP".to_string(),
                        operand: Some(label.clone()),
                        comment: None,
                    })],
                ));
            }
            edits.sort_by(|a, b| b.0.cmp(&a.0));
            for (start, end, repl) in edits {
                items.splice(start..end, repl);
            }
            return true;
        }
    }
    false
}

/// Next free `__TAIL_<n>` id.
fn peephole_next_tail_id(items: &[Item]) -> u32 {
    let mut next = 0;
    for it in items {
        if let Item::Label(name) = it
            && let Some(rest) = name.strip_prefix("__TAIL_")
            && let Ok(n) = rest.parse::<u32>()
        {
            next = next.max(n + 1);
        }
    }
    next
}

/// Maximal straight lines of instructions that end in an unconditional terminator and contain
/// no label or directive.
fn ph305_collect_runs(items: &[Item]) -> Vec<Vec<usize>> {
    let mut runs = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    for (idx, it) in items.iter().enumerate() {
        match it {
            Item::Blank | Item::Comment(_) => {}
            Item::Label(_) | Item::Directive(_) | Item::Origin(_) => cur.clear(),
            Item::Insn(insn) => {
                cur.push(idx);
                if is_uncond_terminator(&insn.mnem) {
                    if cur.len() >= 2 {
                        runs.push(std::mem::take(&mut cur));
                    } else {
                        cur.clear();
                    }
                }
            }
        }
    }
    runs
}

/// Merge identical blocks when all references can be redirected safely.
fn ph304_merge_identical_blocks(items: &mut Vec<Item>) -> bool {
    let blocks = ph304_collect_blocks(items);
    if blocks.len() < 2 {
        return false;
    }
    let zp = ZpEquates::scan(items);
    let mut by_sig: std::collections::HashMap<&str, Vec<usize>> =
        std::collections::HashMap::new();
    for (i, b) in blocks.iter().enumerate() {
        by_sig.entry(b.sig.as_str()).or_default().push(i);
    }
    // Deterministic order: consider groups by their first block's position so helper naming and
    // output stay stable.
    let mut sigs: Vec<&&str> = by_sig.keys().collect();
    sigs.sort_by_key(|s| by_sig[**s][0]);

    for sig in sigs {
        let group = &by_sig[*sig];
        if group.len() < 2 {
            continue;
        }
        let keep = &blocks[group[0]];
        let Some(keep_label) = keep.labels.first() else {
            continue;
        };
        for &other in &group[1..] {
            let drop = &blocks[other];

            if drop.labels.iter().any(|l| l == keep_label) {
                continue;
            }
            if !ph304_entered_only_by_name(items, drop.start) {
                continue;
            }
            if !ph304_refs_are_redirectable(items, drop, keep_label, &zp) {
                continue;
            }
            // Apply: redirect, then delete.
            for it in items.iter_mut() {
                if let Item::Insn(insn) = it
                    && let Some(op) = insn.operand.as_deref()
                    && drop.labels.iter().any(|l| l == op)
                {
                    insn.operand = Some(keep_label.clone());
                }
            }
            items.drain(drop.start..=drop.end);
            return true;
        }
    }
    false
}

struct MergeBlock {
    /// Index of the block's first label item.
    start: usize,
    /// Index of the terminator instruction (inclusive).
    end: usize,
    labels: Vec<String>,
    sig: String,
}

/// Collect every straight-line, unconditionally-terminated block.
fn ph304_collect_blocks(items: &[Item]) -> Vec<MergeBlock> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < items.len() {
        if !matches!(items[i], Item::Label(_)) {
            i += 1;
            continue;
        }
        let start = i;
        let mut labels = Vec::new();
        let mut cur = i;

        while let Item::Label(name) = &items[cur] {
            labels.push(name.clone());
            match next_struct(items, cur) {
                Some(n) => cur = n,
                None => break,
            }
            if !matches!(items[cur], Item::Label(_)) {
                break;
            }
        }
        let mut sig = String::new();
        let mut end = None;
        loop {
            let Item::Insn(insn) = &items[cur] else {
                break; // label / directive: not a clean block
            };
            if is_cond_branch(&insn.mnem) {
                break; // leaves a fall-through successor
            }
            sig.push_str(&insn.mnem);
            if let Some(op) = insn.operand.as_deref() {
                sig.push(' ');
                sig.push_str(op);
            }
            sig.push('\n');
            if is_uncond_terminator(&insn.mnem) {
                end = Some(cur);
                break;
            }
            match next_struct(items, cur) {
                Some(n) => cur = n,
                None => break,
            }
        }
        if let Some(end) = end
            && !sig.is_empty()
        {
            out.push(MergeBlock {
                start,
                end,
                labels,
                sig,
            });
            i = end + 1;
            continue;
        }
        i = start + 1;
    }
    out
}

/// Whether a block can only be entered through a label.
fn ph304_entered_only_by_name(items: &[Item], start: usize) -> bool {
    match prev_struct(items, start) {
        Some(p) => matches!(&items[p], Item::Insn(i) if is_uncond_terminator(&i.mnem)),
        // Start of the stream is the program entry: reachable by fall-through from the loader,
        // never deletable.
        None => false,
    }
}

/// Whether all label references can be redirected within branch range.
fn ph304_refs_are_redirectable(
    items: &[Item],
    drop: &MergeBlock,
    keep_label: &str,
    zp: &ZpEquates,
) -> bool {
    for (idx, it) in items.iter().enumerate() {
        match it {
            Item::Insn(insn) => {
                let Some(op) = insn.operand.as_deref() else {
                    continue;
                };
                let mentions = drop
                    .labels
                    .iter()
                    .any(|l| extract_labels_from_operand(op).iter().any(|m| m == l));
                if !mentions {
                    continue;
                }

                if !drop.labels.iter().any(|l| l == op) {
                    return false;
                }
                if is_cond_branch(&insn.mnem)
                    && !label_within_branch_range(items, idx, keep_label, 100, zp)
                {
                    return false;
                }
            }
            Item::Directive(raw) | Item::Origin(raw) => {

                let toks = extract_labels_from_operand(raw);
                if drop.labels.iter().any(|l| toks.iter().any(|m| m == l)) {
                    return false;
                }
            }
            _ => {}
        }
    }
    true
}

fn is_factored_helper_label(name: &str) -> bool {
    ["__SEQ_", "__JSR2_", "__JSR3_", "__TAIL_"].iter().any(|p| {
        name.strip_prefix(p)
            .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
    })
}

struct HelperShape {
    /// Indices of the body instructions, terminator excluded.
    body: Vec<usize>,
    /// Index of the terminating instruction.
    end_idx: usize,
    /// The terminator's mnemonic (`RTS`, `JMP`, `RTI` or `BRK`).
    term: String,
    /// Byte estimate for the whole helper, terminator included.
    bytes: usize,
}

/// Analyze a helper's control flow and reject unsafe entry or exit paths.
fn helper_shape(items: &[Item], label_idx: usize, zp: &ZpEquates) -> Option<HelperShape> {
    let prev = prev_struct(items, label_idx)?;
    if !matches!(&items[prev], Item::Insn(i) if is_uncond_terminator(&i.mnem)) {
        return None;
    }
    let mut body: Vec<usize> = Vec::new();
    let mut bytes = 0usize;
    let mut cur = label_idx;
    loop {
        let next = next_struct(items, cur)?;
        if items[cur + 1..next].iter().any(is_sys_wrap_marker) {
            return None;
        }
        let Item::Insn(insn) = &items[next] else {
            return None; // interior label, directive or origin
        };
        if is_cond_branch(&insn.mnem) {
            return None;
        }
        bytes += approx_insn_bytes(insn, zp);
        if is_uncond_terminator(&insn.mnem) {
            return Some(HelperShape {
                body,
                end_idx: next,
                term: insn.mnem.clone(),
                bytes,
            });
        }
        body.push(next);
        cur = next;
    }
}

/// Every label mentioned by an instruction operand or a directive, with a count.
///
/// Directives include split `.byte <__FOO` / `.byte >__FOO` pointer tables.
fn label_ref_counts(items: &[Item]) -> std::collections::HashMap<String, usize> {
    let mut refs: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for it in items {
        match it {
            Item::Insn(insn) => {
                if let Some(op) = insn.operand.as_deref() {
                    for label in extract_labels_from_operand(op) {
                        *refs.entry(label).or_insert(0) += 1;
                    }
                }
            }
            Item::Directive(raw) | Item::Origin(raw) => {
                for label in directive_operand_labels(raw) {
                    *refs.entry(label).or_insert(0) += 1;
                }
            }
            _ => {}
        }
    }
    refs
}

/// Call sites of every factored helper, keyed by helper name, in one pass.
fn call_sites_by_name(
    items: &[Item],
    jsr_only: bool,
) -> std::collections::HashMap<String, Vec<usize>> {
    let mut out: std::collections::HashMap<String, Vec<usize>> = std::collections::HashMap::new();
    for (i, it) in items.iter().enumerate() {
        let Item::Insn(insn) = it else { continue };
        if insn.mnem != "JSR" && (jsr_only || insn.mnem != "JMP") {
            continue;
        }
        let Some(op) = insn.operand.as_deref() else {
            continue;
        };
        if !is_factored_helper_label(op) {
            continue;
        }
        out.entry(op.to_string()).or_default().push(i);
    }
    out
}

/// Whether an instruction accesses the hardware stack page as data.
fn any_insn_addresses_stack_page(items: &[Item], idxs: impl IntoIterator<Item = usize>) -> bool {
    idxs.into_iter().any(|i| {
        matches!(&items[i], Item::Insn(insn)
            if insn.operand.as_deref().is_some_and(operand_addresses_stack_page))
    })
}

/// Inline a factored helper with one caller when relocation is safe.
fn ph302_inline_single_call_helpers(items: &mut Vec<Item>) -> bool {
    let Some(plan) = ph302_find_candidate(items) else {
        return false;
    };
    let (label_idx, rts_idx, name, body) = plan;
    items.drain(label_idx..=rts_idx);

    let Some(call_idx) = items.iter().position(|it| {
        matches!(it, Item::Insn(insn)
            if matches!(insn.mnem.as_str(), "JSR" | "JMP")
                && insn.operand.as_deref() == Some(name.as_str()))
    }) else {

        return false;
    };
    items.splice(call_idx..=call_idx, body);
    true
}

/// Locate one inlinable helper: returns its label index, the index of its terminating `RTS`,
/// its name, and the items to splice over the single reference.
#[allow(clippy::type_complexity)]
fn ph302_find_candidate(items: &[Item]) -> Option<(usize, usize, String, Vec<Item>)> {
    let refs = label_ref_counts(items);
    if !refs.keys().any(|k| is_factored_helper_label(k)) {
        return None;
    }
    let zp = ZpEquates::scan(items);
    let smc = smc_patched_labels(items);
    let sites = call_sites_by_name(items, false);

    for (idx, it) in items.iter().enumerate() {
        let Item::Label(name) = it else { continue };
        if !is_factored_helper_label(name) || refs.get(name.as_str()).copied().unwrap_or(0) != 1 {
            continue;
        }
        if smc.contains(name) {
            continue;
        }
        let Some(shape) = helper_shape(items, idx, &zp) else {
            continue;
        };
        if shape.body.is_empty() {
            continue;
        }
        if any_insn_addresses_stack_page(
            items,
            shape.body.iter().copied().chain([shape.end_idx]),
        ) {
            continue;
        }

        let Some(&call_idx) = sites.get(name.as_str()).and_then(|v| v.first()) else {
            continue;
        };

        if (idx..=shape.end_idx).contains(&call_idx) {
            continue;
        }
        let Item::Insn(call) = &items[call_idx] else {
            continue;
        };
        let site_is_jmp = call.mnem == "JMP";

        if !site_is_jmp && !window_stack_balanced(items, &shape.body) {
            continue;
        }
        let mut body: Vec<Item> = shape.body.iter().map(|&b| items[b].clone()).collect();
        match shape.term.as_str() {
            "RTS" => {
                if site_is_jmp {
                    // Tail call: the helper's RTS is the caller's return.
                    body.push(items[shape.end_idx].clone());
                }
            }

            "JMP" => {
                let Item::Insn(term) = &items[shape.end_idx] else {
                    continue;
                };
                let Some(target) = term.operand.as_deref() else {
                    continue;
                };
                if target.starts_with('(') {
                    continue;
                }
                body.push(Item::Insn(Insn {
                    mnem: if site_is_jmp { "JMP" } else { "JSR" }.to_string(),
                    operand: Some(target.to_string()),
                    comment: None,
                }));
            }
            // `RTI` / `BRK` terminate, but neither returns to the call site the way the splice assumes.
            _ => continue,
        }
        return Some((idx, shape.end_idx, name.clone(), body));
    }
    None
}

/// Inline helpers whose remaining call sites no longer save space.
fn ph308_inline_uneconomic_helpers(items: &mut Vec<Item>) -> bool {
    let Some((label_idx, end_idx, name, body, rts)) = ph308_find_candidate(items) else {
        return false;
    };

    items.drain(label_idx..=end_idx);
    let sites = call_sites_by_name(items, false)
        .remove(name.as_str())
        .unwrap_or_default();
    for &site in sites.iter().rev() {
        let site_is_jmp = matches!(&items[site], Item::Insn(i) if i.mnem == "JMP");
        let mut rep = body.clone();
        if site_is_jmp {
            rep.push(rts.clone());
        }
        items.splice(site..=site, rep);
    }
    true
}

/// Locate one net-negative helper: label index, terminator index, name, the body to splice, and the `RTS` item a tail-`JMP` site has to keep.
#[allow(clippy::type_complexity)]
fn ph308_find_candidate(items: &[Item]) -> Option<(usize, usize, String, Vec<Item>, Item)> {
    let refs = label_ref_counts(items);
    if !refs.keys().any(|k| is_factored_helper_label(k)) {
        return None;
    }
    let zp = ZpEquates::scan(items);
    let smc = smc_patched_labels(items);
    let sites_by_name = call_sites_by_name(items, false);

    for (idx, it) in items.iter().enumerate() {
        let Item::Label(name) = it else { continue };
        if !is_factored_helper_label(name) || smc.contains(name) {
            continue;
        }
        let n_refs = refs.get(name.as_str()).copied().unwrap_or(0);
        if n_refs == 0 {
            continue; // Leave unreferenced-label removal to PH303.
        }
        let Some(shape) = helper_shape(items, idx, &zp) else {
            continue;
        };

        if shape.term != "RTS" || shape.body.is_empty() {
            continue;
        }
        if any_insn_addresses_stack_page(
            items,
            shape.body.iter().copied().chain([shape.end_idx]),
        ) {
            continue;
        }
        let Some(sites) = sites_by_name.get(name.as_str()) else {
            continue;
        };
        if sites.len() != n_refs {
            continue;
        }

        if sites.iter().any(|&s| (idx..=shape.end_idx).contains(&s)) {
            continue;
        }

        if !window_stack_balanced(items, &shape.body) {
            continue;
        }
        let n = sites.len();
        let any_jmp_site = sites
            .iter()
            .any(|&i| matches!(&items[i], Item::Insn(x) if x.mnem == "JMP"));
        let per = if any_jmp_site {
            shape.bytes
        } else {
            shape.bytes.saturating_sub(1)
        };
        let after = n.saturating_mul(per);
        let before = shape.bytes + 3 * n;
        if after >= before {
            continue;
        }
        let body: Vec<Item> = shape.body.iter().map(|&b| items[b].clone()).collect();
        return Some((
            idx,
            shape.end_idx,
            name.clone(),
            body,
            items[shape.end_idx].clone(),
        ));
    }
    None
}

/// Instructions that cannot move across a call boundary.
const RELOCATION_UNSAFE_MNEMONICS: [&str; 11] = [
    "PHA", "PHP", "PLA", "PLP", // explicit stack traffic
    "TSX", "TXS",
    "JSR", "JMP", "RTS", "RTI", "BRK",
];

/// Compare instruction operands while rejecting unsafe relocation.
fn relocatable_insn_key(
    items: &[Item],
    idx: usize,
    smc: &std::collections::HashSet<String>,
) -> Option<(String, Option<String>)> {
    let Item::Insn(insn) = &items[idx] else {
        return None;
    };
    if RELOCATION_UNSAFE_MNEMONICS.contains(&insn.mnem.as_str()) || is_cond_branch(&insn.mnem) {
        return None;
    }
    if let Some(op) = insn.operand.as_deref() {
        if op.contains("$FFFF") || operand_addresses_stack_page(op) {
            return None;
        }
    }
    if insn_is_smc_target(items, idx, smc) {
        return None;
    }
    Some((insn.mnem.clone(), insn.operand.clone()))
}

/// Move a common instruction before every helper call into its entry.
fn ph309_absorb_common_pred(items: &mut Vec<Item>) -> bool {
    let mut any = false;
    for _ in 0..40 {
        if !ph309_absorb_round(items) {
            break;
        }
        any = true;
    }
    any
}

fn ph309_absorb_round(items: &mut Vec<Item>) -> bool {
    let refs = label_ref_counts(items);
    if !refs.keys().any(|k| is_factored_helper_label(k)) {
        return false;
    }
    let zp = ZpEquates::scan(items);
    let smc = smc_patched_labels(items);
    let sites_by_name = call_sites_by_name(items, true);

    // label index -> instruction to insert directly after the label
    let mut prologue: Vec<(usize, Insn)> = Vec::new();
    let mut drop: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut claimed: std::collections::HashSet<usize> = std::collections::HashSet::new();

    for (idx, it) in items.iter().enumerate() {
        let Item::Label(name) = it else { continue };
        if !is_factored_helper_label(name) || smc.contains(name) {
            continue;
        }
        let n_refs = refs.get(name.as_str()).copied().unwrap_or(0);
        if n_refs < 2 {
            continue;
        }
        if helper_shape(items, idx, &zp).is_none() {
            continue;
        }
        let Some(sites) = sites_by_name.get(name.as_str()) else {
            continue;
        };
        // `sites` counts `JSR` only, so a helper that is also reached by a tail `JMP`, or named by a `.word`, fails here.
        if sites.len() < 2 || sites.len() != n_refs {
            continue;
        }
        let mut preds: Vec<usize> = Vec::with_capacity(sites.len());
        let mut key: Option<(String, Option<String>)> = None;
        let mut ok = true;
        for &site in sites {
            let Some(p) = prev_struct(items, site) else {
                ok = false;
                break;
            };

            if items[p + 1..site].iter().any(is_sys_wrap_marker) {
                ok = false;
                break;
            }
            let Some(k) = relocatable_insn_key(items, p, &smc) else {
                ok = false;
                break;
            };

            if matches!(prev_struct(items, p), Some(q) if matches!(&items[q], Item::Label(_))) {
                ok = false;
                break;
            }
            match &key {
                None => key = Some(k),
                Some(k0) if *k0 == k => {}
                Some(_) => {
                    ok = false;
                    break;
                }
            }
            preds.push(p);
        }
        if !ok {
            continue;
        }
        // Independence: neither the sites nor the predecessors may already be spoken for by a helper accepted this round.
        if sites.iter().chain(preds.iter()).any(|x| claimed.contains(x)) {
            continue;
        }
        let Item::Insn(model) = &items[preds[0]] else {
            continue;
        };
        for &x in sites.iter().chain(preds.iter()) {
            claimed.insert(x);
        }
        for &p in &preds {
            drop.insert(p);
        }
        prologue.push((idx, model.clone()));
    }
    if prologue.is_empty() {
        return false;
    }
    let insert_after: std::collections::HashMap<usize, Insn> = prologue.into_iter().collect();
    let mut out: Vec<Item> = Vec::with_capacity(items.len() + insert_after.len());
    for (i, it) in items.iter().enumerate() {
        if drop.contains(&i) {
            continue;
        }
        out.push(it.clone());
        if let Some(insn) = insert_after.get(&i) {
            out.push(Item::Insn(insn.clone()));
        }
    }
    *items = out;
    true
}

/// Move a common instruction after every helper call before its return.
fn ph310_absorb_common_succ(items: &mut Vec<Item>) -> bool {
    let mut any = false;
    for _ in 0..40 {
        if !ph310_absorb_round(items) {
            break;
        }
        any = true;
    }
    any
}

fn ph310_absorb_round(items: &mut Vec<Item>) -> bool {
    let refs = label_ref_counts(items);
    if !refs.keys().any(|k| is_factored_helper_label(k)) {
        return false;
    }
    let zp = ZpEquates::scan(items);
    let smc = smc_patched_labels(items);
    let sites_by_name = call_sites_by_name(items, true);

    // terminator index -> instruction to insert directly before it
    let mut epilogue: Vec<(usize, Insn)> = Vec::new();
    let mut drop: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut claimed: std::collections::HashSet<usize> = std::collections::HashSet::new();

    for (idx, it) in items.iter().enumerate() {
        let Item::Label(name) = it else { continue };
        if !is_factored_helper_label(name) || smc.contains(name) {
            continue;
        }
        let n_refs = refs.get(name.as_str()).copied().unwrap_or(0);
        if n_refs < 2 {
            continue;
        }
        let Some(shape) = helper_shape(items, idx, &zp) else {
            continue;
        };
        if shape.term != "RTS" {
            continue;
        }
        let Some(sites) = sites_by_name.get(name.as_str()) else {
            continue;
        };
        if sites.len() < 2 || sites.len() != n_refs {
            continue;
        }
        let mut succs: Vec<usize> = Vec::with_capacity(sites.len());
        let mut key: Option<(String, Option<String>)> = None;
        let mut ok = true;
        for &site in sites {
            let Some(s) = next_struct(items, site) else {
                ok = false;
                break;
            };

            if items[site + 1..s].iter().any(is_sys_wrap_marker) {
                ok = false;
                break;
            }
            let Some(k) = relocatable_insn_key(items, s, &smc) else {
                ok = false;
                break;
            };
            match &key {
                None => key = Some(k),
                Some(k0) if *k0 == k => {}
                Some(_) => {
                    ok = false;
                    break;
                }
            }
            succs.push(s);
        }
        if !ok {
            continue;
        }
        if sites
            .iter()
            .chain(succs.iter())
            .chain([&shape.end_idx])
            .any(|x| claimed.contains(x))
        {
            continue;
        }
        let Item::Insn(model) = &items[succs[0]] else {
            continue;
        };
        for &x in sites.iter().chain(succs.iter()).chain([&shape.end_idx]) {
            claimed.insert(x);
        }
        for &s in &succs {
            drop.insert(s);
        }
        epilogue.push((shape.end_idx, model.clone()));
    }
    if epilogue.is_empty() {
        return false;
    }
    let insert_before: std::collections::HashMap<usize, Insn> = epilogue.into_iter().collect();
    let mut out: Vec<Item> = Vec::with_capacity(items.len() + insert_before.len());
    for (i, it) in items.iter().enumerate() {
        if drop.contains(&i) {
            continue;
        }
        if let Some(insn) = insert_before.get(&i) {
            out.push(Item::Insn(insn.clone()));
        }
        out.push(it.clone());
    }
    *items = out;
    true
}

/// Helper body key excluding comments and runtime-modified operands.
fn helper_text_key(items: &[Item], shape: &HelperShape) -> Option<Vec<(String, Option<String>)>> {
    shape
        .body
        .iter()
        .copied()
        .chain([shape.end_idx])
        .map(|i| {
            let Item::Insn(insn) = &items[i] else {
                return None;
            };
            if insn.operand.as_deref().is_some_and(|o| o.contains("$FFFF")) {
                return None;
            }
            Some((insn.mnem.clone(), insn.operand.clone()))
        })
        .collect()
}

/// Merge helpers with identical instruction bodies.
fn ph311_merge_duplicate_helpers(items: &mut Vec<Item>) -> bool {
    let mut any = false;
    for _ in 0..16 {
        if !ph311_merge_round(items) {
            break;
        }
        any = true;
    }
    any
}

fn ph311_merge_round(items: &mut Vec<Item>) -> bool {
    let refs = label_ref_counts(items);
    if !refs.keys().any(|k| is_factored_helper_label(k)) {
        return false;
    }
    let zp = ZpEquates::scan(items);
    let smc = smc_patched_labels(items);
    let sites = call_sites_by_name(items, false);

    let mut keep: std::collections::HashMap<Vec<(String, Option<String>)>, String> =
        std::collections::HashMap::new();
    let mut rename: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut cut: Vec<(usize, usize)> = Vec::new();

    for (idx, it) in items.iter().enumerate() {
        let Item::Label(name) = it else { continue };
        if !is_factored_helper_label(name) || smc.contains(name) {
            continue;
        }
        let Some(shape) = helper_shape(items, idx, &zp) else {
            continue;
        };
        if shape.body.is_empty() {
            continue;
        }
        let Some(key) = helper_text_key(items, &shape) else {
            continue;
        };
        let Some(prev) = keep.get(&key) else {
            // First one with this text is the representative.
            keep.insert(key, name.clone());
            continue;
        };
        // The loser must have every one of its references in a form this pass can repoint.
        let n_refs = refs.get(name.as_str()).copied().unwrap_or(0);
        let n_sites = sites.get(name.as_str()).map_or(0, |v| v.len());
        if n_sites == 0 || n_sites != n_refs {
            continue;
        }
        rename.insert(name.clone(), prev.clone());
        cut.push((idx, shape.end_idx));
    }
    if rename.is_empty() {
        return false;
    }
    // Helper ranges are disjoint ; `helper_shape` stops at the first interior label ; so a plain membership test over the cut ranges is enough.
    let mut dropped: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for (a, b) in cut {
        dropped.extend(a..=b);
    }
    let mut out: Vec<Item> = Vec::with_capacity(items.len());
    for (i, it) in items.iter().enumerate() {
        if dropped.contains(&i) {
            continue;
        }
        match it {
            Item::Insn(insn)
                if matches!(insn.mnem.as_str(), "JSR" | "JMP")
                    && insn
                        .operand
                        .as_deref()
                        .is_some_and(|op| rename.contains_key(op)) =>
            {
                let target = rename[insn.operand.as_deref().unwrap()].clone();
                out.push(Item::Insn(Insn {
                    mnem: insn.mnem.clone(),
                    operand: Some(target),
                    comment: insn.comment.clone(),
                }));
            }
            _ => out.push(it.clone()),
        }
    }
    *items = out;
    true
}

/// Replace a duplicated helper tail with a jump to the shared body.
fn ph312_merge_helper_tails(items: &mut Vec<Item>) -> bool {
    let mut any = false;
    for _ in 0..8 {
        if !ph312_merge_round(items) {
            break;
        }
        any = true;
    }
    any
}

fn ph312_merge_round(items: &mut Vec<Item>) -> bool {
    let refs = label_ref_counts(items);
    if !refs.keys().any(|k| is_factored_helper_label(k)) {
        return false;
    }
    let zp = ZpEquates::scan(items);
    let smc = smc_patched_labels(items);

    // Every well-shaped helper, in stream order: name, label index, its full text and the per-instruction byte estimate behind it.
    struct Cand {
        name: String,
        text: Vec<(String, Option<String>)>,
        idxs: Vec<usize>,
    }
    let mut cands: Vec<Cand> = Vec::new();
    for (idx, it) in items.iter().enumerate() {
        let Item::Label(name) = it else { continue };
        if !is_factored_helper_label(name) || smc.contains(name) {
            continue;
        }
        let Some(shape) = helper_shape(items, idx, &zp) else {
            continue;
        };
        if shape.body.is_empty() {
            continue;
        }
        let Some(text) = helper_text_key(items, &shape) else {
            continue;
        };
        cands.push(Cand {
            name: name.clone(),
            idxs: shape.body.iter().copied().chain([shape.end_idx]).collect(),
            text,
        });
    }
    // Text -> the first helper in stream order carrying it.
    let mut by_text: std::collections::HashMap<&[(String, Option<String>)], &str> =
        std::collections::HashMap::new();
    for c in &cands {
        by_text.entry(c.text.as_slice()).or_insert(c.name.as_str());
    }

    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    for src in &cands {
        let m = src.text.len();
        // `len` counts down so the longest tail ; the biggest saving ; is taken first.
        for len in (1..m).rev() {
            let tail = &src.text[m - len..];
            let bytes: usize = src.idxs[m - len..]
                .iter()
                .map(|&i| match &items[i] {
                    Item::Insn(insn) => approx_insn_bytes(insn, &zp),
                    _ => 0,
                })
                .sum();
            if bytes <= 3 {

                break;
            }
            let Some(&target) = by_text.get(tail) else {
                continue;
            };
            if target == src.name {
                continue;
            }
            edits.push((
                src.idxs[m - len],
                *src.idxs.last().unwrap(),
                target.to_string(),
            ));
            break;
        }
    }
    if edits.is_empty() {
        return false;
    }
    let mut dropped: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut jmp_at: std::collections::HashMap<usize, String> = std::collections::HashMap::new();
    for (first, last, target) in &edits {
        jmp_at.insert(*first, target.clone());
        dropped.extend(*first..=*last);
    }
    let mut out: Vec<Item> = Vec::with_capacity(items.len());
    for (i, it) in items.iter().enumerate() {
        if let Some(target) = jmp_at.get(&i) {
            out.push(Item::Insn(Insn {
                mnem: "JMP".to_string(),
                operand: Some(target.clone()),
                comment: None,
            }));
        }
        if dropped.contains(&i) {
            continue;
        }
        out.push(it.clone());
    }
    *items = out;
    true
}

/// Remove unreferenced local labels.
fn ph029_drop_dead_local_labels(items: &mut Vec<Item>) -> bool {
    if !items
        .iter()
        .any(|it| matches!(it, Item::Label(name) if is_sweepable_local_label(name)))
    {
        return false;
    }
    let referenced = collect_referenced_idents(items);
    let before = items.len();
    items.retain(|it| match it {
        Item::Label(name) => !is_sweepable_local_label(name) || referenced.contains(name),
        _ => true,
    });
    items.len() != before
}

/// Local label families that can be removed when unreferenced.
fn is_sweepable_local_label(name: &str) -> bool {
    if let Some(rest) = name.strip_prefix('L')
        && !rest.is_empty()
        && rest.bytes().all(|b| b.is_ascii_digit())
    {
        return true;
    }
    name.starts_with("__IF_") || name.starts_with("__ABX_")
}

/// Every identifier token mentioned by an operand, directive or origin line.
fn collect_referenced_idents(items: &[Item]) -> std::collections::HashSet<String> {
    fn scan(s: &str, refs: &mut std::collections::HashSet<String>) {
        for tok in s.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
            if tok.is_empty() || tok.as_bytes()[0].is_ascii_digit() {
                continue;
            }
            refs.insert(tok.to_string());
        }
    }
    let mut refs = std::collections::HashSet::new();
    for it in items {
        match it {
            Item::Insn(insn) => {
                if let Some(op) = insn.operand.as_deref() {
                    scan(op, &mut refs);
                }
            }
            Item::Directive(s) | Item::Origin(s) => scan(s, &mut refs),
            _ => {}
        }
    }
    refs
}

/// Reorder independent load/store pairs to remove a repeated load.
fn ph030_swap_indep_store_pairs(items: &mut Vec<Item>) -> bool {
    let mut changed = false;
    let equ = collect_equ_values(items);
    let origin = lowest_origin_addr(items);
    let mut i = 0;
    while i < items.len() {
        if let Some((j1, j2, j3, j4)) = swap_pair_plan(items, i, &equ, origin) {

            let lda_a = items[i].clone();
            let sta_b = items[j1].clone();
            let lda_c = items[j2].clone();
            let sta_d = items[j3].clone();
            items[i] = lda_c;
            items[j1] = sta_d;
            items[j2] = lda_a;
            items[j3] = sta_b;
            items.remove(j4);
            changed = true;
        }
        i += 1;
    }
    changed
}

/// Find a load/store window whose memory accesses can be reordered.
fn swap_pair_plan(
    items: &[Item],
    i: usize,
    equ: &std::collections::HashMap<String, u16>,
    origin: Option<u16>,
) -> Option<(usize, usize, usize, usize)> {
    let Item::Insn(lda_a) = &items[i] else {
        return None;
    };
    if lda_a.mnem != "LDA" {
        return None;
    }
    let j1 = next_struct(items, i)?;
    let Item::Insn(sta_b) = &items[j1] else {
        return None;
    };
    if sta_b.mnem != "STA" {
        return None;
    }
    let j2 = next_struct(items, j1)?;
    let Item::Insn(lda_c) = &items[j2] else {
        return None;
    };
    if lda_c.mnem != "LDA" {
        return None;
    }
    let j3 = next_struct(items, j2)?;
    let Item::Insn(sta_d) = &items[j3] else {
        return None;
    };
    if sta_d.mnem != "STA" {
        return None;
    }
    let j4 = next_struct(items, j3)?;
    let Item::Insn(reload) = &items[j4] else {
        return None;
    };
    if reload.mnem != "LDA" {
        return None;
    }

    let a = lda_a.operand.as_deref()?;
    let b = sta_b.operand.as_deref()?;
    let c = lda_c.operand.as_deref()?;
    let d = sta_d.operand.as_deref()?;
    // The tail must reload exactly the first store's cell.
    if reload.operand.as_deref() != Some(b) {
        return None;
    }

    let b_cell = plain_label_cell(b, equ)?;
    let d_cell = plain_label_cell(d, equ)?;
    if mem_cells_may_alias(&b_cell, &d_cell, origin) {
        return None;
    }
    let a_read = read_footprint(a, equ)?;
    let c_read = read_footprint(c, equ)?;
    if read_may_alias_cell(&a_read, &d_cell, origin) {
        return None;
    }
    if read_may_alias_cell(&c_read, &b_cell, origin) {
        return None;
    }
    Some((j1, j2, j3, j4))
}

/// A memory cell named by a plain (non-indexed, non-indirect, offset-free) operand.
#[derive(Debug, Clone, PartialEq)]
enum MemCell {
    /// Address known exactly: a `$XX` zero-page literal or a label EQU-resolved from a `NAME =
    /// $XXXX` directive.
    Known(u16),
    /// A storage or code label without an equate.
    Sym(String),
}

/// The memory footprint a load operand reads from.
#[derive(Debug, Clone)]
enum ReadFootprint {
    /// An immediate operand with no memory read.
    Imm,
    /// One plain cell.
    Cell(MemCell),
    /// A literal absolute (possibly indexed) window `[lo, hi]`.
    Range(u16, u16),
}

/// Resolve a plain store target, including zero-page equates.
fn plain_label_cell(op: &str, equ: &std::collections::HashMap<String, u16>) -> Option<MemCell> {
    if op.is_empty()
        || !op
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        || op.as_bytes()[0].is_ascii_digit()
    {
        return None;
    }
    match equ.get(op) {
        Some(&v) if v < 0x0100 => Some(MemCell::Known(v)),
        Some(_) => None,
        None => Some(MemCell::Sym(op.to_string())),
    }
}

/// Classify a load's memory reads; reject indirect or I/O accesses.
fn read_footprint(
    op: &str,
    equ: &std::collections::HashMap<String, u16>,
) -> Option<ReadFootprint> {
    if op.starts_with('#') {
        return Some(ReadFootprint::Imm);
    }
    if op.contains('(') || op.contains('+') || op.contains('-') {
        return None;
    }
    if let Some(cell) = plain_label_cell(op, equ) {
        return Some(ReadFootprint::Cell(cell));
    }
    // `$XX`, `$XXXX`, `$XXXX,X`, `$XXXX,Y` literal forms.
    let (base, indexed) = match op.split_once(',') {
        Some((base, idx)) => {
            let idx = idx.trim();
            if idx != "X" && idx != "Y" {
                return None;
            }
            (base.trim(), true)
        }
        None => (op, false),
    };
    let hex = base.strip_prefix('$')?;
    if hex.is_empty() || hex.len() > 4 || !hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return None;
    }
    let lo = u16::from_str_radix(hex, 16).ok()?;
    let hi = if indexed { lo.checked_add(0xFF)? } else { lo };

    if hi >= 0xD000 && lo <= 0xDFFF {
        return None;
    }
    Some(if indexed || lo > 0xFF {
        ReadFootprint::Range(lo, hi)
    } else {
        ReadFootprint::Cell(MemCell::Known(lo))
    })
}

/// May two plain cells be the same byte?
fn mem_cells_may_alias(x: &MemCell, y: &MemCell, origin: Option<u16>) -> bool {
    match (x, y) {
        (MemCell::Known(a), MemCell::Known(b)) => a == b,
        (MemCell::Sym(a), MemCell::Sym(b)) => a == b,
        (MemCell::Known(v), MemCell::Sym(_)) | (MemCell::Sym(_), MemCell::Known(v)) => {
            match origin {
                Some(org) => *v >= org,
                None => true,
            }
        }
    }
}

/// May a load footprint overlap a store cell?
fn read_may_alias_cell(read: &ReadFootprint, cell: &MemCell, origin: Option<u16>) -> bool {
    match read {
        ReadFootprint::Imm => false,
        ReadFootprint::Cell(rc) => mem_cells_may_alias(rc, cell, origin),
        ReadFootprint::Range(lo, hi) => match cell {
            MemCell::Known(v) => *lo <= *v && *v <= *hi,
            MemCell::Sym(_) => match origin {
                Some(org) => *hi >= org,
                None => true,
            },
        },
    }
}

/// Remove a store overwritten without an intervening read.
fn ph031_drop_overwritten_store(items: &mut Vec<Item>) -> bool {
    let equ = collect_equ_values(items);
    let origin = lowest_origin_addr(items);
    let smc = smc_patched_labels(items);
    let mut changed = false;
    let mut i = 0;
    'outer: while i < items.len() {
        let is_candidate = match &items[i] {
            Item::Insn(first) if matches!(first.mnem.as_str(), "STA" | "STX" | "STY") => {
                first.operand.as_deref().is_some_and(|t| {
                    is_label_or_zp_safe(t) && !t.contains('+')
                })
            }
            _ => false,
        };

        let is_candidate = is_candidate && !insn_is_smc_target(items, i, &smc);
        if !is_candidate {
            i += 1;
            continue;
        }
        let t = match &items[i] {
            Item::Insn(insn) => insn.operand.clone().unwrap(),
            _ => unreachable!(),
        };
        let Some(t_cell) = cell_of_plain_operand(&t, &equ) else {
            i += 1;
            continue;
        };
        let mut j = i;
        loop {
            let Some(k) = next_struct(items, j) else {
                i += 1;
                continue 'outer;
            };
            let Item::Insn(insn) = &items[k] else {

                i += 1;
                continue 'outer;
            };
            if matches!(insn.mnem.as_str(), "STA" | "STX" | "STY")
                && insn.operand.as_deref() == Some(t.as_str())
            {
                items.remove(i);
                changed = true;
                continue 'outer; // Recheck the item at i.
            }
            if !window_insn_preserves_cell(insn, &t_cell, &equ, origin) {
                i += 1;
                continue 'outer;
            }
            j = k;
        }
    }
    changed
}

/// Whether an instruction leaves the tracked memory cell unchanged.
fn window_insn_preserves_cell(
    insn: &Insn,
    cell: &MemCell,
    equ: &std::collections::HashMap<String, u16>,
    origin: Option<u16>,
) -> bool {
    match insn.mnem.as_str() {
        // Register-only, no memory, no control flow.
        "TAX" | "TAY" | "TXA" | "TYA" | "INX" | "DEX" | "INY" | "DEY" | "CLC" | "SEC"
        | "CLV" | "NOP" => true,

        "LDA" | "LDX" | "LDY" | "CMP" | "CPX" | "CPY" | "AND" | "ORA" | "EOR" | "ADC"
        | "SBC" | "BIT" => {
            let Some(op) = insn.operand.as_deref() else {
                return false;
            };
            match read_footprint(op, equ) {
                Some(fp) => !read_may_alias_cell(&fp, cell, origin),
                None => false,
            }
        }
        // Writes (and RMWs) to a provably distinct location.
        "STA" | "STX" | "STY" | "INC" | "DEC" => {
            let Some(op) = insn.operand.as_deref() else {
                return false;
            };
            if let Some(other) = cell_of_plain_operand(op, equ) {
                return !mem_cells_may_alias(&other, cell, origin);
            }
            match read_footprint(op, equ) {
                Some(fp) => !read_may_alias_cell(&fp, cell, origin),
                None => false,
            }
        }
        _ => false,
    }
}

/// A one-byte cell named by a plain operand: a bare label (EQU- resolved when possible) or a
/// `$XX` zero-page literal.
fn cell_of_plain_operand(
    op: &str,
    equ: &std::collections::HashMap<String, u16>,
) -> Option<MemCell> {
    if let Some(cell) = plain_label_cell(op, equ) {
        return Some(cell);
    }
    let hex = op.strip_prefix('$')?;
    if hex.len() == 2 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        u16::from_str_radix(hex, 16).ok().map(MemCell::Known)
    } else {
        None
    }
}

/// Collect equate names and their numeric values.
fn collect_equ_values(items: &[Item]) -> std::collections::HashMap<String, u16> {
    let mut map = std::collections::HashMap::new();
    for it in items {
        let Item::Directive(s) = it else { continue };
        let t = s.trim();
        let Some(eq) = t.find('=') else { continue };
        let lhs = t[..eq].trim_end();
        let rhs = t[eq + 1..].trim();
        if lhs.is_empty()
            || !lhs
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        {
            continue;
        }
        let Some(hex) = rhs.strip_prefix('$') else {
            continue;
        };
        if hex.is_empty() || hex.len() > 4 || !hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
            continue;
        }
        if let Ok(v) = u16::from_str_radix(hex, 16) {
            map.insert(lhs.to_string(), v);
        }
    }
    map
}

/// Lowest assembly origin, below which labeled storage cannot occur.
fn lowest_origin_addr(items: &[Item]) -> Option<u16> {
    let mut lowest: Option<u16> = None;
    for it in items {
        let Item::Origin(s) = it else { continue };
        let t = s.trim();
        let Some(rest) = t.strip_prefix("*=") else {
            continue;
        };
        let Some(hex) = rest.trim().strip_prefix('$') else {
            continue;
        };
        if let Ok(v) = u16::from_str_radix(hex, 16) {
            lowest = Some(lowest.map_or(v, |cur: u16| cur.min(v)));
        }
    }
    lowest
}

/// PH008 — store/load forwarding.
///
/// ```text
///     STA tmp
///     LDA tmp
/// ```
///
/// The `LDA` reloads what we just stored — `A` already holds the
/// value. Drop the `LDA`. Same shape for `STX/LDX` and `STY/LDY`.
///
/// Safety guards:
///   * `tmp` must be a labeled address (not a `$XXXX` literal). We
///     don't touch `$D000-$DFFF` I/O ranges.
///   * The instruction after `LDA` must not consume flags from the
///     `LDA` — bail on every conditional branch and on `ADC/SBC/ROL/
///     ROR` which read `C`. (`STA` next is fine; it doesn't read
///     flags. A subsequent `LDA/LDX/LDY` resets flags, also fine.)
fn ph008_store_load_forward(items: &mut Vec<Item>) -> bool {
    let mut changed = false;
    let mut i = 0;
    while i + 1 < items.len() {
        let mut drop_idx: Option<usize> = None;
        if let Item::Insn(sta) = &items[i]
            && let Some(j) = next_struct(items, i)
            && let Item::Insn(lda) = &items[j]
            && let Some((store_op, ld_op)) = matching_store_load(sta, lda)
            && store_op == ld_op
            && is_label_or_zp_safe(store_op)
            && next_after_loads_flags_safely(items, j)
        {
            drop_idx = Some(j);
        }
        if let Some(j) = drop_idx {
            items.remove(j);
            changed = true;
            continue;
        }
        i += 1;
    }
    changed
}

/// Return (store_operand, load_operand) when `sta` and `lda` form a
/// matching store/load pair on the same register.
fn matching_store_load<'a>(sta: &'a Insn, lda: &'a Insn) -> Option<(&'a str, &'a str)> {
    let pair = match (sta.mnem.as_str(), lda.mnem.as_str()) {
        ("STA", "LDA") | ("STX", "LDX") | ("STY", "LDY") => true,
        _ => false,
    };
    if !pair {
        return None;
    }
    Some((sta.operand.as_deref()?, lda.operand.as_deref()?))
}

/// True iff `addr` is a label or low zero-page that we trust isn't
/// memory-mapped I/O. Refuses any `$XXXX` four-digit absolute (could
/// be VIC/CIA/SID) and any `$DX..` byte-form (zero-page mirrors of
/// I/O don't exist on the C64, but easier to just reject `$D*`).
fn is_label_or_zp_safe(addr: &str) -> bool {
    if let Some(rest) = addr.strip_prefix('$') {
        // Indexed forms like `$03,X` or `($FB),Y` — bail.
        if rest.contains(',') || addr.contains('(') {
            return false;
        }
        let hex_part = rest.split('+').next().unwrap_or(rest);
        // Two-digit zero-page only ($00-$FF). Anything wider is
        // potentially I/O.
        return hex_part.len() == 2 && hex_part.chars().all(|c| c.is_ascii_hexdigit());
    }
    // Named label — trust unless it starts with characters that
    // suggest a hardware constant (none currently in this codebase).
    !addr.contains(',') && !addr.starts_with('(')
}

/// Return true if the instruction at `j+1` (skipping blanks/comments)
/// neither reads the flags from the LDA at `j` nor cares about C
/// from the dropped CMP-equivalent. Conservative: bail for every
/// conditional branch and for ADC/SBC/ROL/ROR (they read C).
fn next_after_loads_flags_safely(items: &[Item], j: usize) -> bool {
    let Some(k) = next_struct(items, j) else {
        return true;
    };
    if let Item::Insn(after) = &items[k] {
        if is_cond_branch(&after.mnem) {
            return false;
        }
        if matches!(
            after.mnem.as_str(),
            "ADC" | "SBC" | "ROL" | "ROR" | "PLP" | "RTI"
        ) {
            return false;
        }
    }
    true
}

/// PH009 — `LDA X; STA X` — the store is a no-op.
///
/// We just loaded the value, didn't change it, and are storing it
/// back to the same location. Drop the `STA`. Same for `STX/LDX`
/// and `STY/LDY`.
///
/// Same volatility/operand guard as PH008. We also bail if the load
/// uses indexed addressing (`X` or `Y` index) — the index could
/// change between the load and a hypothetical re-store, but the
/// 6502 instruction we'd emit is identical, so semantically still
/// safe… defer this to a follow-up that handles it cleanly.
fn ph009_load_store_identity(items: &mut Vec<Item>) -> bool {
    let mut changed = false;
    let mut i = 0;
    while i + 1 < items.len() {
        let mut drop_idx: Option<usize> = None;
        if let Item::Insn(lda) = &items[i]
            && let Some(j) = next_struct(items, i)
            && let Item::Insn(sta) = &items[j]
            && matches!(
                (lda.mnem.as_str(), sta.mnem.as_str()),
                ("LDA", "STA") | ("LDX", "STX") | ("LDY", "STY")
            )
            && let (Some(lop), Some(sop)) = (lda.operand.as_deref(), sta.operand.as_deref())
            && lop == sop
            && is_label_or_zp_safe(lop)
        {
            drop_idx = Some(j);
        }
        if let Some(j) = drop_idx {
            items.remove(j);
            changed = true;
            continue;
        }
        i += 1;
    }
    changed
}

/// PH020 — forward known-value cleanup.
///
/// A small, conservative forward analysis tracks what A/X/Y are
/// known to contain at each instruction: an immediate byte and/or a
/// stable memory-origin. It also tracks the carry flag when it is
/// known from `CLC`/`SEC`. The pass uses those facts to:
///
///   * remove redundant `LDA/LDX/LDY` when the destination already
///     contains the same value and the removed load's N/Z flags are
///     dead or already known equivalent,
///   * replace a load with `TAX/TAY/TXA/TYA` when another register
///     already contains the same value,
///   * remove redundant `CLC`/`SEC` when C is already known to have
///     the requested value.
///
/// Labels/directives/origins and unknown calls reset the facts. We
/// only track stable compiler-style labels and low zero-page operands;
/// indexed/indirect/IO operands are opaque.
fn ph020_static_known_value_cleanup(items: &mut Vec<Item>) -> bool {
    ph020_known_value_cleanup(items, true)
}

/// Retain counter reloads until loop promotion has finished.
fn ph020_known_value_cleanup(items: &mut Vec<Item>, keep_counter_reloads: bool) -> bool {
    let facts = analyze_static_asm_facts(items);
    let flag_config = FlagLivenessConfig::default();
    let flag_live = analyze_flag_liveness_with_config(items, &flag_config);

    let inc_dec_cells: std::collections::HashSet<&str> = items
        .iter()
        .filter(|_| keep_counter_reloads)
        .filter_map(|it| match it {
            Item::Insn(insn) if matches!(insn.mnem.as_str(), "INC" | "DEC") => {
                insn.operand.as_deref()
            }
            _ => None,
        })
        .collect();

    #[derive(Debug, Clone, Copy)]
    enum Rewrite {
        Drop,
        Replace(&'static str),
    }

    let mut plans: Vec<(usize, Rewrite)> = Vec::new();

    for (idx, item) in items.iter().enumerate() {
        let Item::Insn(insn) = item else { continue };
        match insn.mnem.as_str() {
            "LDA" | "LDX" | "LDY" => {
                let Some(dest) = load_dest_reg(insn) else {
                    continue;
                };
                let fact = &facts[idx];

                if reg_fact_matches_load(fact, dest, insn)
                    && load_removal_preserves_live_flags(insn, fact, flag_live.live_out_at(idx))
                {
                    plans.push((idx, Rewrite::Drop));
                    continue;
                }

                if insn
                    .operand
                    .as_deref()
                    .is_some_and(|op| inc_dec_cells.contains(op))
                {
                    continue;
                }
                if let Some(transfer) = best_transfer_for_load(fact, dest, insn) {
                    plans.push((idx, Rewrite::Replace(transfer)));
                }
            }
            "CLC" if facts[idx].flag_c == Some(false) => plans.push((idx, Rewrite::Drop)),
            "SEC" if facts[idx].flag_c == Some(true) => plans.push((idx, Rewrite::Drop)),
            _ => {}
        }
    }

    if plans.is_empty() {
        return false;
    }

    for (idx, rewrite) in plans.into_iter().rev() {
        match rewrite {
            Rewrite::Drop => {
                items.remove(idx);
            }
            Rewrite::Replace(mnem) => {
                if let Some(Item::Insn(insn)) = items.get_mut(idx) {
                    insn.mnem = mnem.to_string();
                    insn.operand = None;
                }
            }
        }
    }
    true
}

fn load_dest_reg(insn: &Insn) -> Option<Reg> {
    match insn.mnem.as_str() {
        "LDA" => Some(Reg::A),
        "LDX" => Some(Reg::X),
        "LDY" => Some(Reg::Y),
        _ => None,
    }
}

fn reg_fact_matches_load(facts: &StaticAsmFacts, reg: Reg, insn: &Insn) -> bool {
    let fact = facts.reg(reg);
    if let Some(op) = insn.operand.as_deref()
        && let Some(imm) = ImmFact::from_operand(op)
    {
        return fact
            .imm
            .as_ref()
            .is_some_and(|known| known.same_value(&imm));
    }
    if let Some(op) = insn.operand.as_deref()
        && let Some(mem) = static_fact_mem_operand(op)
    {
        return fact.mem.as_deref().is_some_and(|m| facts.same_cell(m, &mem));
    }
    false
}

fn load_removal_preserves_live_flags(
    insn: &Insn,
    facts: &StaticAsmFacts,
    live_out: FlagSet,
) -> bool {
    let needs_z = live_out.contains(Flag::Z);
    let needs_n = live_out.contains(Flag::N);
    if !needs_z && !needs_n {
        return true;
    }
    if load_dest_reg(insn).is_some_and(|r| facts.nz_regs.contains(r)) {
        return true;
    }

    let Some(imm) = insn
        .operand
        .as_deref()
        .and_then(ImmFact::from_operand)
        .and_then(|imm| imm.byte)
    else {
        return false;
    };

    (!needs_z || facts.flag_z == Some(imm == 0))
        && (!needs_n || facts.flag_n == Some(imm & 0x80 != 0))
}

fn best_transfer_for_load(facts: &StaticAsmFacts, dest: Reg, insn: &Insn) -> Option<&'static str> {
    match dest {
        Reg::A => {
            if reg_fact_matches_load(facts, Reg::X, insn) {
                Some("TXA")
            } else if reg_fact_matches_load(facts, Reg::Y, insn) {
                Some("TYA")
            } else {
                None
            }
        }
        Reg::X => reg_fact_matches_load(facts, Reg::A, insn).then_some("TAX"),
        Reg::Y => reg_fact_matches_load(facts, Reg::A, insn).then_some("TAY"),
    }
}

/// PH023 — collapse runs of consecutive labels at the same address.
///
/// DISABLED. Naive label-aliasing breaks BSS / data labels that are
/// referenced via `#<label` / `#>label` / `label+offset` / etc.
/// Those forms aren't bare-operand matches, so the alias-rewrite
/// misses them; meanwhile the dead label gets removed and the
/// reference becomes undefined.
///
/// Re-enable only after operand rewriting is taught to handle
/// every PETSCII/ACME operand form, not just bare labels.
#[allow(dead_code)]
fn ph023_collapse_adjacent_labels(items: &mut Vec<Item>) -> bool {
    // Walk items; whenever we see a label, gather any contiguous
    // sequence of labels (with intervening blanks/comments allowed
    // — they don't change the address) into one group. The first
    // label survives, the rest are aliased to it.
    let mut alias: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut i = 0;
    while i < items.len() {
        if !matches!(&items[i], Item::Label(_)) {
            i += 1;
            continue;
        }
        let survivor = match &items[i] {
            Item::Label(name) => name.clone(),
            _ => unreachable!(),
        };
        let mut j = i + 1;
        while j < items.len() {
            match &items[j] {
                Item::Label(name) => {
                    if name != &survivor {
                        alias.insert(name.clone(), survivor.clone());
                    }
                    j += 1;
                }
                Item::Blank | Item::Comment(_) => j += 1,
                _ => break,
            }
        }
        i = j;
    }

    if alias.is_empty() {
        return false;
    }

    let mut changed = false;
    // Rewrite jump/JSR operands to point at the surviving label.
    for item in items.iter_mut() {
        if let Item::Insn(insn) = item
            && let Some(op) = insn.operand.as_deref()
        {
            let trimmed = op.trim();
            if let Some(repl) = alias.get(trimmed)
                && is_branch_or_jump_or_jsr(&insn.mnem)
            {
                insn.operand = Some(repl.clone());
                changed = true;
            }
        }
    }
    // Drop the redundant labels themselves.
    items.retain(|item| match item {
        Item::Label(name) => !alias.contains_key(name),
        _ => true,
    });
    if !alias.is_empty() {
        changed = true;
    }
    changed
}

fn is_branch_or_jump_or_jsr(mnem: &str) -> bool {
    matches!(
        mnem,
        "JMP" | "JSR" | "BEQ" | "BNE" | "BMI" | "BPL" | "BCC" | "BCS" | "BVC" | "BVS"
    )
}

/// PH026 — simplify identity-immediate ALU ops on A.
///
/// The 6502 has several identities on A:
///   * `AND #$FF` is a no-op on A (only sets N/Z).
///   * `ORA #$00` is a no-op on A (only sets N/Z).
///   * `EOR #$00` is a no-op on A (only sets N/Z).
///   * `ORA #$FF` always loads $FF into A.
///   * `AND #$00` always loads $00 into A.
///
/// When the flag side-effect is dead, the no-op variants vanish
/// entirely. The "becomes a constant" variants get rewritten to
/// `LDA #$XX`, which often unlocks PH020's redundant-load drop or
/// PH021's branch fold downstream.
fn ph026_simplify_alu_imm(items: &mut Vec<Item>) -> bool {
    let flag_config = FlagLivenessConfig::default();
    let live = analyze_flag_liveness_with_config(items, &flag_config);

    enum Plan {
        Drop,
        SetLda(u8),
    }

    let mut plans: Vec<(usize, Plan)> = Vec::new();
    for (idx, item) in items.iter().enumerate() {
        let Item::Insn(insn) = item else { continue };
        let op = match insn.operand.as_deref().and_then(ImmFact::from_operand) {
            Some(imm) => imm,
            None => continue,
        };
        let Some(byte) = op.byte else { continue };
        let live_out = live.live_out_at(idx);
        let nz_dead = !live_out.contains(Flag::N) && !live_out.contains(Flag::Z);
        match insn.mnem.as_str() {
            "AND" if byte == 0xFF && nz_dead => plans.push((idx, Plan::Drop)),
            "ORA" if byte == 0x00 && nz_dead => plans.push((idx, Plan::Drop)),
            "EOR" if byte == 0x00 && nz_dead => plans.push((idx, Plan::Drop)),
            "AND" if byte == 0x00 => plans.push((idx, Plan::SetLda(0))),
            "ORA" if byte == 0xFF => plans.push((idx, Plan::SetLda(0xFF))),
            _ => {}
        }
    }

    if plans.is_empty() {
        return false;
    }
    for (idx, plan) in plans.into_iter().rev() {
        match plan {
            Plan::Drop => {
                items.remove(idx);
            }
            Plan::SetLda(byte) => {
                if let Some(Item::Insn(insn)) = items.get_mut(idx) {
                    insn.mnem = "LDA".to_string();
                    insn.operand = Some(format!("#${byte:02X}"));
                }
            }
        }
    }
    true
}

/// PH025 — drop a `STA/STX/STY` whose target is already known to
/// hold the byte we're about to store.
///
/// Lean on PH020's `mem_imm` map: when an earlier `STA tmp` (with
/// known imm) recorded `mem_imm[tmp] = K`, a later `STA tmp` from
/// a register also known to hold `K` is just rewriting the same
/// byte. We skip the volatile / unstable case via the same
/// `static_fact_mem_operand` filter PH020 uses, so I/O writes
/// (`$D000-$DFFF`) are never touched.
/// PH028 — drop redundant `LDA m` after `INC m` / `DEC m`.
///
/// `INC m` / `DEC m` already set Z/N from the new memory value, so a
/// subsequent `LDA m` (loading the same byte) would set the same
/// Z/N — the only side effect is re-loading the value into A. When
/// A is dead at the LDA's OUT, the load is a 3-byte / 4-cycle no-op.
///
/// Catches the very common `var = var ± 1: IF var = 0 THEN ...` idiom
/// in hand-written BASIC counters, where the LET lowers to
/// `INC m` / `DEC m` and the IF's compare-against-zero emits the
/// `LDA m / BEQ ...` sequence right after.
fn ph028_drop_lda_after_inc_dec(items: &mut Vec<Item>) -> bool {
    let live = analyze_register_liveness(items);
    let mut to_drop = Vec::new();
    for (idx, item) in items.iter().enumerate() {
        let Item::Insn(insn) = item else {
            continue;
        };
        if !matches!(insn.mnem.as_str(), "INC" | "DEC") {
            continue;
        }
        let Some(rmw_op) = insn.operand.as_deref() else {
            continue;
        };
        if !is_label_or_zp_safe(rmw_op) {
            continue;
        }
        let Some(j) = next_struct(items, idx) else {
            continue;
        };
        let Some(Item::Insn(lda)) = items.get(j) else {
            continue;
        };
        if lda.mnem != "LDA" {
            continue;
        }
        if lda.operand.as_deref() != Some(rmw_op) {
            continue;
        }
        // A's load is pure overhead if the live-out at the LDA doesn't
        // include A — the byte just sits in A unused until the next
        // write. (Z/N flags from LDA == Z/N from the prior INC/DEC, so
        // they're already "in scope" for any branch that follows.)
        if live.is_live_out(j, Reg::A) {
            continue;
        }
        to_drop.push(j);
    }
    if to_drop.is_empty() {
        return false;
    }
    for idx in to_drop.into_iter().rev() {
        items.remove(idx);
    }
    true
}

fn ph025_drop_redundant_store(items: &mut Vec<Item>) -> bool {
    let facts = analyze_static_asm_facts(items);
    let smc = smc_patched_labels(items);
    let mut victims: Vec<usize> = Vec::new();
    for (idx, item) in items.iter().enumerate() {
        let Item::Insn(insn) = item else { continue };
        let reg = match insn.mnem.as_str() {
            "STA" => Reg::A,
            "STX" => Reg::X,
            "STY" => Reg::Y,
            _ => continue,
        };

        if insn_is_smc_target(items, idx, &smc) {
            continue;
        }
        let Some(op) = insn.operand.as_deref() else {
            continue;
        };
        let Some(mem) = static_fact_mem_operand(op) else {
            continue;
        };
        let fact = &facts[idx];
        let Some(stored) = fact.reg(reg).imm.as_ref().and_then(|i| i.byte) else {
            continue;
        };
        if fact.known_mem_imm(&mem) == Some(stored) {
            victims.push(idx);
        }
    }

    if victims.is_empty() {
        return false;
    }
    for idx in victims.into_iter().rev() {
        items.remove(idx);
    }
    true
}

/// Remove comparisons whose output flags are unused.
fn ph024_drop_dead_cmp(items: &mut Vec<Item>) -> bool {
    // Keep flags live across returns.
    let flag_config = FlagLivenessConfig::default();
    let live = analyze_flag_liveness_with_config(items, &flag_config);

    let mut victims: Vec<usize> = Vec::new();
    for (idx, item) in items.iter().enumerate() {
        let Item::Insn(insn) = item else { continue };
        if !matches!(insn.mnem.as_str(), "CMP" | "CPX" | "CPY" | "BIT") {
            continue;
        }

        if let Some(op) = insn.operand.as_deref()
            && let Some(addr) = parse_hex_addr(op)
            && (0xD000..=0xDFFF).contains(&addr)
        {
            continue;
        }
        let live_out = live.live_out_at(idx);
        // CMP/CPX/CPY define N/Z/C; BIT defines N/Z/V. If none of
        // those flags are live at the out-edge, the instruction is
        // dead.
        let defs = match insn.mnem.as_str() {
            "BIT" => FlagSet::NZ.union(FlagSet::V),
            _ => FlagSet::NZC,
        };
        // No flag defined by this insn is live out → it's dead.
        if live_out.difference(defs) == live_out {
            victims.push(idx);
        }
    }

    if victims.is_empty() {
        return false;
    }
    for idx in victims.into_iter().rev() {
        items.remove(idx);
    }
    true
}

/// Operand forms safe for replacing subtraction with comparison.
fn ph316_cmp_safe_operand(op: &str) -> bool {
    let op = op.trim();
    if op.is_empty() || op.contains('(') {
        return false;
    }
    let (base, idx) = match op.rsplit_once(',') {
        Some((b, i)) => (b, i.trim()),
        None => (op, ""),
    };
    if !matches!(idx, "" | "X" | "Y") {
        return false;
    }
    if let Some(imm) = base.strip_prefix('#') {
        return !imm.is_empty() && !imm.contains(',');
    }
    let (body, off) = match base.split_once('+') {
        Some((b, o)) => (b, Some(o)),
        None => (base, None),
    };
    let off_ok = off.is_none_or(|o| !o.is_empty() && o.chars().all(|c| c.is_ascii_digit()));
    if let Some(hex) = body.strip_prefix('$') {
        return off_ok
            && !hex.is_empty()
            && hex.len() <= 4
            && hex.chars().all(|c| c.is_ascii_hexdigit());
    }
    off_ok
        && !body.is_empty()
        && body.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !body.as_bytes()[0].is_ascii_digit()
}

/// Replace SEC/LDA/SBC/LDA with LDA/CMP/LDA when A and V are dead.
fn ph316_sec_lda_sbc_to_cmp(items: &mut Vec<Item>) -> bool {
    if items
        .iter()
        .any(|it| matches!(it, Item::Insn(i) if i.mnem == "SED"))
    {
        return false;
    }
    let smc = smc_patched_labels(items);
    let mut drops: Vec<usize> = Vec::new();
    let mut rewrites: Vec<usize> = Vec::new();
    let mut i = 0;
    while i < items.len() {
        if !matches!(&items[i], Item::Insn(ins) if ins.mnem == "SEC") {
            i += 1;
            continue;
        }
        let sec = i;
        let Some(lda1) = next_struct(items, sec) else {
            i += 1;
            continue;
        };
        let Some(sbc) = next_struct(items, lda1) else {
            i += 1;
            continue;
        };
        let Some(lda2) = next_struct(items, sbc) else {
            i += 1;
            continue;
        };
        let (Item::Insn(l1), Item::Insn(s), Item::Insn(l2)) =
            (&items[lda1], &items[sbc], &items[lda2])
        else {
            i += 1;
            continue;
        };
        if l1.mnem != "LDA" || s.mnem != "SBC" || l2.mnem != "LDA" {
            i += 1;
            continue;
        }
        let Some(op) = s.operand.as_deref() else {
            i += 1;
            continue;
        };
        let refused = !ph316_cmp_safe_operand(op)
            || parse_hex_addr(op).is_some_and(|a| {
                (0x0100..=0x01FF).contains(&a) || (0xD000..=0xDFFF).contains(&a)
            })
            || insn_is_smc_target(items, sbc, &smc)
            || insn_is_smc_target(items, sec, &smc)
            || crosses_sys_wrap(items, sec, lda2);
        if refused {
            i += 1;
            continue;
        }
        drops.push(sec);
        rewrites.push(sbc);
        i = lda2 + 1;
    }
    if drops.is_empty() {
        return false;
    }
    for &k in &rewrites {
        if let Item::Insn(ins) = &mut items[k] {
            ins.mnem = "CMP".to_string();
        }
    }
    for &k in drops.iter().rev() {
        items.remove(k);
    }
    true
}

/// Direct cell operands safe for 16 bit rewrites.
fn ph318_cell_operand(op: &str) -> bool {
    let op = op.trim();
    if op.is_empty() || op.contains('(') || op.contains(',') || op.starts_with('#') {
        return false;
    }
    let (body, off) = match op.split_once('+') {
        Some((b, o)) => (b, Some(o)),
        None => (op, None),
    };
    let off_ok = off.is_none_or(|o| !o.is_empty() && o.chars().all(|c| c.is_ascii_digit()));
    if let Some(hex) = body.strip_prefix('$') {
        return off_ok
            && !hex.is_empty()
            && hex.len() <= 4
            && hex.chars().all(|c| c.is_ascii_hexdigit());
    }
    off_ok
        && !body.is_empty()
        && body.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !body.as_bytes()[0].is_ascii_digit()
}

/// Read immediate zero or one.
fn ph318_imm(insn: &Insn) -> Option<u8> {
    let op = insn.operand.as_deref()?.trim();
    let hex = op.strip_prefix("#$")?;
    if hex.is_empty() || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u8::from_str_radix(hex, 16).ok()
}

/// Whether the high cell immediately follows the low cell.
fn ph318_next_cell(lo: &str, hi: &str) -> bool {
    if hi == format!("{lo}+1") {
        return true;
    }
    matches!(
        (parse_plain_addr(lo), parse_plain_addr(hi)),
        (Some(a), Some(b)) if b == a.wrapping_add(1)
    )
}

/// Replace carry propagation with INC or DEC when exit registers and flags are dead.
fn ph319_carry_fold(items: &mut Vec<Item>) -> bool {
    if items.iter().any(|it| {
        matches!(it, Item::Insn(i) if matches!(i.mnem.as_str(), "SED" | "PHP"))
    }) {
        return false;
    }
    if !items.iter().any(|it| {
        matches!(it, Item::Insn(i)
            if matches!(i.mnem.as_str(), "ADC" | "SBC") && ph318_imm(i) == Some(0))
    }) {
        return false;
    }
    let reg_config = liveness_config_with_helpers();
    let regs = analyze_register_liveness_with_config(items, &reg_config);
    let flags = analyze_flag_liveness_with_config(items, &FlagLivenessConfig::default());
    let smc = smc_patched_labels(items);
    let zp = ZpEquates::scan(items);

    struct Site {
        head: usize,
        tail: usize,
        inc: bool,
        cell: String,
        comment: Option<String>,
    }
    let mut sites: Vec<Site> = Vec::new();
    let mut i = 0;
    while i < items.len() {
        let site = (|| {
            let Item::Insn(lda) = &items[i] else { return None };
            if lda.mnem != "LDA" {
                return None;
            }
            let cell = lda.operand.as_deref()?.trim().to_string();
            let j_alu = next_struct(items, i)?;
            let Item::Insn(alu) = &items[j_alu] else { return None };
            let inc = match alu.mnem.as_str() {
                "ADC" => true,
                "SBC" => false,
                _ => return None,
            };
            if ph318_imm(alu) != Some(0) {
                return None;
            }
            let j_sta = next_struct(items, j_alu)?;
            let Item::Insn(sta) = &items[j_sta] else { return None };
            if sta.mnem != "STA" || sta.operand.as_deref().map(str::trim) != Some(cell.as_str()) {
                return None;
            }
            if !ph318_cell_operand(&cell)
                || parse_hex_addr(&cell).is_some_and(|a| {
                    (0x0100..=0x01FF).contains(&a) || (0xD000..=0xDFFF).contains(&a)
                })
            {
                return None;
            }
            for &k in &[i, j_alu, j_sta] {
                if insn_is_smc_target(items, k, &smc) {
                    return None;
                }
            }
            if crosses_sys_wrap(items, i, j_sta) {
                return None;
            }
            if regs.is_live_out(j_sta, Reg::A) {
                return None;
            }
            for flag in [Flag::N, Flag::Z, Flag::V, Flag::C] {
                if flags.is_live_out(j_sta, flag) {
                    return None;
                }
            }
            let skipped = Insn {
                mnem: if inc { "INC" } else { "DEC" }.to_string(),
                operand: Some(cell.clone()),
                comment: None,
            };
            if approx_insn_bytes(&skipped, &zp) > 127 {
                return None;
            }
            Some(Site {
                head: i,
                tail: j_sta,
                inc,
                cell,
                comment: alu.comment.clone(),
            })
        })();
        match site {
            Some(s) => {
                i = s.tail + 1;
                sites.push(s);
            }
            None => i += 1,
        }
    }
    if sites.is_empty() {
        return false;
    }
    for (next_id, site) in (ph319_next_label_id(items)..).zip(sites.into_iter().rev()) {
        let label = format!("__PH319_{next_id}");
        let branch = if site.inc { "BCC" } else { "BCS" };
        let rmw = if site.inc { "INC" } else { "DEC" };
        items.splice(
            site.head..=site.tail,
            [
                Item::Insn(Insn {
                    mnem: branch.to_string(),
                    operand: Some(label.clone()),
                    comment: None,
                }),
                Item::Insn(Insn {
                    mnem: rmw.to_string(),
                    operand: Some(site.cell),
                    comment: site.comment,
                }),
                Item::Label(label),
            ],
        );
    }
    true
}

/// Fresh-label id for PH319's skips (the ph318_next_label_id scan).
fn ph319_next_label_id(items: &[Item]) -> u32 {
    let mut next = 0;
    for it in items {
        if let Item::Label(name) = it
            && let Some(rest) = name.strip_prefix("__PH319_")
            && let Ok(n) = rest.parse::<u32>()
        {
            next = next.max(n + 1);
        }
    }
    next
}

/// Merge 16 bit increments when A and flags are dead. Limit runs to 255.
fn ph320_merge_inc_runs(items: &mut Vec<Item>) -> bool {
    if items.iter().any(|it| {
        matches!(it, Item::Insn(i) if matches!(i.mnem.as_str(), "SED" | "PHP"))
    }) {
        return false;
    }
    if !items.iter().any(|it| matches!(it, Item::Insn(i) if i.mnem == "INC")) {
        return false;
    }
    let reg_config = liveness_config_with_helpers();
    let regs = analyze_register_liveness_with_config(items, &reg_config);
    let flags = analyze_flag_liveness_with_config(items, &FlagLivenessConfig::default());
    let smc = smc_patched_labels(items);
    let zp = ZpEquates::scan(items);
    let refs = label_ref_counts(items);

    struct Site {
        head: usize,
        tail: usize,
        k: usize,
        lo: String,
        hi: String,
    }
    let mut sites: Vec<Site> = Vec::new();
    let mut i = 0;
    'outer: while i < items.len() {
        let Item::Insn(insn) = &items[i] else {
            i += 1;
            continue;
        };
        if insn.mnem != "INC" {
            i += 1;
            continue;
        }
        let Some(lo) = insn.operand.as_deref().map(str::trim) else {
            i += 1;
            continue;
        };
        // Try to match a run of blocks starting at i.
        let mut k = 0usize;
        let mut tail = i;
        let mut cur = i;
        let run_ok = true;
        let mut hi_cell: Option<String> = None;
        while run_ok {
            let Some(j_bne) = next_struct(items, cur) else { break; };
            let Item::Insn(bne) = &items[j_bne] else { break; };
            if bne.mnem != "BNE" {
                break;
            }
            let Some(bne_target) = bne.operand.as_deref().map(str::trim) else { break; };
            let Some(j_inc_hi) = next_struct(items, j_bne) else { break; };
            let Item::Insn(inc_hi) = &items[j_inc_hi] else { break; };
            if inc_hi.mnem != "INC" {
                break;
            }
            let hi = inc_hi.operand.as_deref().map(str::trim).unwrap_or("");
            if !ph318_next_cell(lo, hi) {
                break;
            }
            if let Some(first) = &hi_cell {
                if first != hi {
                    break;
                }
            } else {
                hi_cell = Some(hi.to_string());
            }
            let Some(j_lbl) = next_struct(items, j_inc_hi) else { break; };
            let Item::Label(name) = &items[j_lbl] else { break; };
            if name != bne_target {
                break;
            }
            k += 1;
            tail = j_lbl;
            if k == 255 {
                break;
            }
            match next_struct(items, j_lbl) {
                Some(next) => match &items[next] {
                    Item::Insn(nxt)
                        if nxt.mnem == "INC"
                            && nxt.operand.as_deref().map(str::trim) == Some(lo) =>
                    {
                        cur = next;
                    }
                    _ => break,
                },
                None => break,
            }
        }
        if k < 2 {
            i += 1;
            continue;
        }
        let Some(hi) = hi_cell else {
            i += 1;
            continue;
        };
        // Every block label referenced only by its own BNE.
        let mut idx = i;
        while idx <= tail {
            if let Item::Label(name) = &items[idx]
                && refs.get(name).copied().unwrap_or(0) != 1
            {
                i = idx + 1;
                continue 'outer;
            }
            idx += 1;
        }
        // Operand forms + volatile screens (PH318's, RMW discipline).
        let screens = |op: &str| {
            ph318_cell_operand(op)
                && !parse_hex_addr(op).is_some_and(|a| {
                    (0x0100..=0x01FF).contains(&a) || (0xD000..=0xDFFF).contains(&a)
                })
                && zp.map.get(op.split_once('+').map_or(op, |(b, _)| b).trim()) != Some(&0x00FF)
        };
        if !screens(lo) || !screens(&hi) {
            i = tail + 1;
            continue;
        }
        // SMC anchors anywhere in the run.
        let mut idx = i;
        while idx <= tail {
            if insn_is_smc_target(items, idx, &smc) {
                i = tail + 1;
                continue 'outer;
            }
            idx += 1;
        }
        if crosses_sys_wrap(items, i, tail) {
            i = tail + 1;
            continue;
        }
        // Everything the merge newly defines must be dead at the exit.
        if regs.is_live_out(tail, Reg::A) {
            i = tail + 1;
            continue;
        }
        if [Flag::N, Flag::Z, Flag::V, Flag::C]
            .iter()
            .any(|f| flags.is_live_out(tail, *f))
        {
            i = tail + 1;
            continue;
        }
        let skipped = Insn {
            mnem: "INC".to_string(),
            operand: Some(hi.clone()),
            comment: None,
        };
        if approx_insn_bytes(&skipped, &zp) > 127 {
            i = tail + 1;
            continue;
        }
        sites.push(Site {
            head: i,
            tail,
            k,
            lo: lo.to_string(),
            hi: hi.to_string(),
        });
        i = tail + 1;
    }
    if sites.is_empty() {
        return false;
    }
    for (next_id, site) in (ph320_next_label_id(items)..).zip(sites.into_iter().rev()) {
        let label = format!("__PH320_{next_id}");
        let ins = |mnem: &str, op: &str| {
            Item::Insn(Insn {
                mnem: mnem.to_string(),
                operand: Some(op.to_string()),
                comment: None,
            })
        };
        let repl = vec![
            Item::Insn(Insn { mnem: "CLC".to_string(), operand: None, comment: None }),
            ins("LDA", &site.lo),
            ins("ADC", &format!("#${:02X}", site.k)),
            ins("STA", &site.lo),
            Item::Insn(Insn {
                mnem: "BCC".to_string(),
                operand: Some(label.clone()),
                comment: None,
            }),
            ins("INC", &site.hi),
            Item::Label(label),
        ];
        items.splice(site.head..=site.tail, repl);
    }
    true
}

/// Fresh-label id for PH320's skips (the ph318_next_label_id scan).
fn ph320_next_label_id(items: &[Item]) -> u32 {
    let mut next = 0;
    for it in items {
        if let Item::Label(name) = it
            && let Some(rest) = name.strip_prefix("__PH320_")
            && let Ok(n) = rest.parse::<u32>()
        {
            next = next.max(n + 1);
        }
    }
    next
}

/// Move a second store next to its source store when reads and flags permit it.
fn ph321_store_forward_sink(items: &mut Vec<Item>) -> bool {
    if !items
        .iter()
        .any(|it| matches!(it, Item::Insn(i) if i.mnem == "STA"))
    {
        return false;
    }
    let equ = collect_equ_values(items);
    let origin = lowest_origin_addr(items);
    let smc = smc_patched_labels(items);
    let reg_config = liveness_config_with_helpers();
    let regs = analyze_register_liveness_with_config(items, &reg_config);
    let flags = analyze_flag_liveness_with_config(items, &FlagLivenessConfig::default());

    /// A store, its window, and the reload pair it feeds.
    struct Site {
        sta_x: usize,
        lda_x: usize,
        sta_z: usize,
        z_operand: String,
    }
    let mut sites: Vec<Site> = Vec::new();
    let mut i = 0;
    'outer: while i < items.len() {
        let (x_text, x_cell) = match &items[i] {
            Item::Insn(insn) if insn.mnem == "STA" => {
                let Some(t) = insn.operand.as_deref() else {
                    i += 1;
                    continue;
                };
                if !is_label_or_zp_safe(t) || t.contains('+') {
                    i += 1;
                    continue;
                }
                let Some(cell) = cell_of_plain_operand(t, &equ) else {
                    i += 1;
                    continue;
                };
                (t.to_string(), cell)
            }
            _ => {
                i += 1;
                continue;
            }
        };
        if insn_is_smc_target(items, i, &smc) {
            i += 1;
            continue;
        }
        let mut w_end = i;
        for _ in 0..8 {
            let Some(j) = next_struct(items, w_end) else {
                i += 1;
                continue 'outer;
            };
            let Item::Insn(insn) = &items[j] else {
                i += 1;
                continue 'outer;
            };
            if insn.mnem == "LDA" && insn.operand.as_deref() == Some(x_text.as_str()) {
                // The reload. Its partner must be a plain safe store.
                let Some(k) = next_struct(items, j) else {
                    i += 1;
                    continue 'outer;
                };
                let ok = match &items[k] {
                    Item::Insn(st) => {
                        st.mnem == "STA"
                            && st.operand.as_deref().is_some_and(|z| {
                                is_label_or_zp_safe(z) && !z.contains('+')
                            })
                    }
                    _ => false,
                };
                if !ok {
                    i += 1;
                    continue 'outer;
                }
                let z_text = match &items[k] {
                    Item::Insn(st) => st.operand.clone().unwrap(),
                    _ => unreachable!(),
                };
                let Some(z_cell) = cell_of_plain_operand(&z_text, &equ) else {
                    i += 1;
                    continue 'outer;
                };
                if mem_cells_may_alias(&x_cell, &z_cell, origin) {
                    i += 1;
                    continue 'outer;
                }
                if insn_is_smc_target(items, j, &smc)
                    || insn_is_smc_target(items, k, &smc)
                {
                    i += 1;
                    continue 'outer;
                }
                if crosses_sys_wrap(items, i, k) {
                    i += 1;
                    continue 'outer;
                }
                // W (strictly between the stores) never touches Z.
                let mut m = i;
                while m < j {
                    if let Item::Insn(w) = &items[m] {
                        if !window_insn_preserves_cell(w, &z_cell, &equ, origin) {
                            i += 1;
                            continue 'outer;
                        }
                    }
                    m += 1;
                }
                if regs.is_live_out(k, Reg::A)
                    || flags.is_live_out(k, Flag::N)
                    || flags.is_live_out(k, Flag::Z)
                {
                    i += 1;
                    continue 'outer;
                }
                sites.push(Site {
                    sta_x: i,
                    lda_x: j,
                    sta_z: k,
                    z_operand: z_text,
                });
                i = k + 1;
                continue 'outer;
            }
            // Window continues only through insns X survives.
            if !window_insn_preserves_cell(insn, &x_cell, &equ, origin) {
                i += 1;
                continue 'outer;
            }
            w_end = j;
        }
        i += 1;
    }
    if sites.is_empty() {
        return false;
    }
    for site in sites.into_iter().rev() {
        items.remove(site.sta_z);
        items.remove(site.lda_x);
        items.insert(
            site.sta_x + 1,
            Item::Insn(Insn {
                mnem: "STA".to_string(),
                operand: Some(site.z_operand),
                comment: None,
            }),
        );
    }
    true
}

/// Use absolute array addresses when the pointer and Y are dead.
fn ph322_const_deref_to_abs(items: &mut Vec<Item>) -> bool {
    if !items.iter().any(|it| matches!(it, Item::Insn(i) if i.mnem == "LDA")) {
        return false;
    }
    let equ = collect_equ_values(items);
    let origin = lowest_origin_addr(items);
    let smc = smc_patched_labels(items);
    let reg_config = liveness_config_with_helpers();
    let regs = analyze_register_liveness_with_config(items, &reg_config);
    let flags = analyze_flag_liveness_with_config(items, &FlagLivenessConfig::default());

    /// Add a constant offset to the operand text.
    fn base_plus(expr: &str, k: u8) -> Option<String> {
        let (sym, off) = match expr.strip_prefix('(').and_then(|e| e.strip_suffix(')')) {
            Some(inner) => {
                let (s, n) = inner.split_once('+')?;
                if n.is_empty() || !n.chars().all(|c| c.is_ascii_digit()) {
                    return None;
                }
                (s, n.parse::<u32>().ok()?)
            }
            None => (expr, 0),
        };
        if sym.is_empty()
            || !sym
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
            || sym.as_bytes()[0].is_ascii_digit()
        {
            return None;
        }
        Some(format!("{sym}+{}", off + k as u32))
    }

    struct Site {
        /// Instruction indices removed by the rewrite.
        del: Vec<usize>,
        /// (deref index, replacement operand) pairs
        derefs: Vec<(usize, String)>,
    }
    let mut sites: Vec<Site> = Vec::new();
    let mut i = 0;
    'outer: while i + 5 < items.len() {
        let expr = match &items[i] {
            Item::Insn(a) if a.mnem == "LDA" => {
                match a.operand.as_deref().unwrap_or("").strip_prefix("#<") {
                    Some(e) if !e.is_empty() && !e.contains(',') => e.to_string(),
                    _ => {
                        i += 1;
                        continue;
                    }
                }
            }
            _ => {
                i += 1;
                continue;
            }
        };
        if expr.is_empty() || expr.contains(',') {
            i += 1;
            continue;
        }
        let steps: Vec<Option<usize>> = (0..5).map(|d| next_struct(items, i + d)).collect();
        let (lo, hi) = match (
            steps[0].and_then(|j| match &items[j] {
                Item::Insn(x) if x.mnem == "STA" => x.operand.clone(),
                _ => None,
            }),
            steps[1].and_then(|j| match &items[j] {
                Item::Insn(x) if x.mnem == "LDA" => x.operand.clone(),
                _ => None,
            }),
            steps[2].and_then(|j| match &items[j] {
                Item::Insn(x) if x.mnem == "STA" => x.operand.clone(),
                _ => None,
            }),
            steps[3].and_then(|j| match &items[j] {
                Item::Insn(x) if x.mnem == "LDY" => x.operand.clone(),
                _ => None,
            }),
        ) {
            (Some(lo), Some(hi_e), Some(hi), Some(y)) => {
                if hi_e != format!("#>{expr}") || y != "#$00" {
                    i += 1;
                    continue;
                }
                (lo, hi)
            }
            _ => {
                i += 1;
                continue;
            }
        };
        let lo = lo.trim().to_string();
        let hi = hi.trim().to_string();
        if !ph318_next_cell(&lo, &hi) {
            i += 1;
            continue;
        }
        let Some(mut cur) = steps[3] else {
            i += 1;
            continue;
        };
        let lo_cell = match cell_of_plain_operand(&lo, &equ) {
            Some(c) => c,
            None => {
                i += 1;
                continue;
            }
        };
        let hi_cell = match cell_of_plain_operand(&hi, &equ) {
            Some(c) => c,
            None => {
                i += 1;
                continue;
            }
        };
        let mut del = vec![i];
        for &d in steps.iter().take(4) {
            del.push(d.expect("step"));
        }
        let mut derefs: Vec<(usize, String)> = Vec::new();
        let mut offset: u8 = 0;
        // Limit liveness checks to the rewritten instructions.
        let mut tail = steps[3].expect("ldy index");
        let mut saw_deref = false;
        for _ in 0..8 {
            let Some(j) = next_struct(items, cur) else { break; };
            let Item::Insn(insn) = &items[j] else { break; };
            let is_lo_deref = insn
                .operand
                .as_deref()
                .is_some_and(|o| o.trim() == format!("({lo}),Y"));
            if is_lo_deref && matches!(insn.mnem.as_str(), "LDA" | "STA") {
                let Some(repl) = base_plus(&expr, offset) else { break; };
                derefs.push((j, repl));
                saw_deref = true;
                tail = j;
                cur = j;
                continue;
            }
            if insn.mnem == "INY" && insn.operand.is_none() {
                if !saw_deref {
                    break;
                }
                offset = offset.saturating_add(1);
                del.push(j);
                tail = j;
                cur = j;
                continue;
            }
            _ = ();
            let keeps_cells = window_insn_preserves_cell(insn, &lo_cell, &equ, origin)
                && window_insn_preserves_cell(insn, &hi_cell, &equ, origin);
            let writes_y = match insn.mnem.as_str() {
                "LDY" | "TAY" | "INY" | "DEY" | "PLA" => true,
                _ => false,
            };
            if !keeps_cells || writes_y {
                break;
            }
            cur = j;
        }
        if !saw_deref {
            i += 1;
            continue;
        }
        // SMC anchors anywhere in the window.
        let mut idx = i;
        while idx <= tail {
            if insn_is_smc_target(items, idx, &smc) {
                i = tail + 1;
                continue 'outer;
            }
            idx += 1;
        }
        if crosses_sys_wrap(items, i, tail) {
            i = tail + 1;
            continue;
        }
        // Require deleted register and flag outputs to be dead.
        let del_set: std::collections::HashSet<usize> = del.iter().copied().collect();
        let mut last_a_def_deleted = true;
        let mut last_nz_def_deleted = true;
        for k in i..=tail {
            let Item::Insn(w) = &items[k] else { continue; };
            let defines_a = matches!(w.mnem.as_str(), "LDA" | "PLA" | "TXA" | "TYA" | "ADC" | "SBC");
            let defines_nz = defines_a
                || matches!(
                    w.mnem.as_str(),
                    "LDX" | "LDY" | "INC" | "DEC" | "ASL" | "LSR" | "ROL" | "ROR" | "BIT"
                );
            if defines_a {
                last_a_def_deleted = del_set.contains(&k);
            }
            if defines_nz {
                last_nz_def_deleted = del_set.contains(&k);
            }
        }
        if regs.is_live_out(tail, Reg::Y)
            || (last_a_def_deleted && regs.is_live_out(tail, Reg::A))
            || (last_nz_def_deleted
                && (flags.is_live_out(tail, Flag::N) || flags.is_live_out(tail, Flag::Z)))
        {
            i = tail + 1;
            continue;
        }
        sites.push(Site { del, derefs });
        i = tail + 1;
    }
    if sites.is_empty() {
        return false;
    }
    for site in sites.into_iter().rev() {
        for (idx, repl) in site.derefs.iter().rev() {
            if let Item::Insn(insn) = &mut items[*idx] {
                insn.operand = Some(repl.clone());
            }
        }
        // Then delete the setup insns, back to front.
        let mut del = site.del.clone();
        del.sort_unstable();
        del.dedup();
        for idx in del.into_iter().rev() {
            items.remove(idx);
        }
    }
    true
}

/// Fold constant array store indices when X and flags are dead.
fn ph323_const_index_store_to_abs(items: &mut Vec<Item>) -> bool {
    if !items.iter().any(|it| matches!(it, Item::Insn(i) if i.mnem == "LDX")) {
        return false;
    }
    let smc = smc_patched_labels(items);
    let zp = ZpEquates::scan(items);
    let reg_config = liveness_config_with_helpers();
    let regs = analyze_register_liveness_with_config(items, &reg_config);

    // Keep zero page indexed stores because their addresses wrap within the page.
    let zp_indexed = |op: &str| -> bool {
        let base = op.split_once('+').map_or(op, |(b, _)| b).trim();
        if let Some(hex) = base.strip_prefix('$') {
            return hex.len() <= 2;
        }
        zp.map.contains_key(base)
    };

    struct Site {
        /// First and last instruction indices replaced.
        head: usize,
        tail: usize,
        /// the whole replacement block
        repl: Vec<Item>,
    }
    let mut sites: Vec<Site> = Vec::new();
    let mut i = 0;
    'outer: while i + 2 < items.len() {
        let (k, k_idx) = match &items[i] {
            Item::Insn(x) if x.mnem == "LDX" => {
                let Some(v) = x.operand.as_deref().and_then(parse_byte_immediate) else {
                    i += 1;
                    continue;
                };
                (v, i)
            }
            _ => {
                i += 1;
                continue;
            }
        };
        if insn_is_smc_target(items, i, &smc) {
            i += 1;
            continue;
        }
        let s0 = next_struct(items, k_idx);
        let Some(s0) = s0 else {
            i += 1;
            continue;
        };
        let Item::Insn(lda) = &items[s0] else {
            i += 1;
            continue;
        };
        if lda.mnem == "LDA"
            && let Some(lo_op) = lda.operand.as_deref()
            && let Some(sym) = lo_op.strip_prefix("#<")
            && !sym.is_empty()
            && !sym.contains(',')
        {
            let steps: Vec<Option<usize>> = (0..4).map(|d| next_struct(items, s0 + d)).collect();
            let (b1, b2) = match (
                steps[0].and_then(|j| match &items[j] {
                    Item::Insn(x) if x.mnem == "LDY" => x.operand.clone(),
                    _ => None,
                }),
                steps[1].and_then(|j| match &items[j] {
                    Item::Insn(x) if x.mnem == "STA" => x.operand.clone(),
                    _ => None,
                }),
                steps[2].and_then(|j| match &items[j] {
                    Item::Insn(x) if x.mnem == "TYA" && x.operand.is_none() => Some(()),
                    _ => None,
                }),
                steps[3].and_then(|j| match &items[j] {
                    Item::Insn(x) if x.mnem == "STA" => x.operand.clone(),
                    _ => None,
                }),
            ) {
                (Some(hi_op), Some(b1), Some(()), Some(b2)) if hi_op == format!("#>{sym}") => {
                    (b1, b2)
                }
                _ => {
                    i += 1;
                    continue;
                }
            };
            // b,X and b+1,X with a constant k -> absolute forms.
            let (base1, base2) = match (b1.strip_suffix(",X"), b2.strip_suffix(",X")) {
                (Some(b1), Some(b2)) => (b1.to_string(), b2.to_string()),
                _ => {
                    i += 1;
                    continue;
                }
            };
            if !ph318_next_cell(&base1, &base2) || zp_indexed(&base1) || zp_indexed(&base2) {
                i += 1;
                continue;
            }
            let tail = steps[3].expect("st2 index");
            for &idx in &[k_idx, s0, steps[0].expect("ldy"), steps[1].expect("st1"), steps[2].expect("tya"), tail] {
                if insn_is_smc_target(items, idx, &smc) {
                    i = tail + 1;
                    continue 'outer;
                }
            }
            if crosses_sys_wrap(items, k_idx, tail) {
                i = tail + 1;
                continue;
            }
            if regs.is_live_out(tail, Reg::X) || regs.is_live_out(tail, Reg::A) {
                i = tail + 1;
                continue;
            }
            let ins = |mnem: &str, op: &str| {
                Item::Insn(Insn {
                    mnem: mnem.to_string(),
                    operand: Some(op.to_string()),
                    comment: None,
                })
            };
            let plus = |b: &str| if k == 0 { b.to_string() } else { format!("{b}+{k}") };
            sites.push(Site {
                head: k_idx,
                tail,
                repl: vec![
                    ins("LDA", &format!("#<{sym}")),
                    ins("STA", &plus(&base1)),
                    ins("LDY", &format!("#>{sym}")),
                    ins("STY", &plus(&base2)),
                ],
            });
            i = tail + 1;
            continue;
        }
        if lda.mnem == "LDA"
            && let Some(imm) = lda.operand.as_deref()
            && imm.starts_with("#$")
        {
            let Some(j) = next_struct(items, s0) else {
                i += 1;
                continue;
            };
            let base = match &items[j] {
                Item::Insn(x) if x.mnem == "STA" => {
                    x.operand.as_deref().and_then(|o| o.strip_suffix(",X"))
                }
                _ => None,
            }
            .map(str::to_string);
            let Some(base) = base else {
                i += 1;
                continue;
            };
            if zp_indexed(&base) {
                i = j + 1;
                continue;
            }
            if insn_is_smc_target(items, j, &smc) || crosses_sys_wrap(items, k_idx, j) {
                i = j + 1;
                continue;
            }
            if regs.is_live_out(j, Reg::X) {
                i = j + 1;
                continue;
            }
            sites.push(Site {
                head: k_idx,
                tail: j,
                repl: vec![
                    Item::Insn(Insn {
                        mnem: "LDA".to_string(),
                        operand: Some(imm.to_string()),
                        comment: None,
                    }),
                    Item::Insn(Insn {
                        mnem: "STA".to_string(),
                        operand: Some(if k == 0 {
                            base.clone()
                        } else {
                            format!("{base}+{k}")
                        }),
                        comment: None,
                    }),
                ],
            });
            i = j + 1;
            continue;
        }
        i += 1;
    }
    if sites.is_empty() {
        return false;
    }
    for site in sites.into_iter().rev() {
        items.splice(site.head..=site.tail, site.repl);
    }
    true
}

/// Whether N and Z are redefined before any use.
fn nz_dead_after(
    items: &[Item],
    idx: usize,
    labels: &std::collections::HashMap<String, usize>,
    smc: &std::collections::HashSet<String>,
    config: &FlagLivenessConfig,
    depth: usize,
) -> bool {
    let reads_nz = |set: FlagSet| set.contains(Flag::N) || set.contains(Flag::Z);
    let defines_nz = |set: FlagSet| set.contains(Flag::N) && set.contains(Flag::Z);
    let mut i = idx;
    while let Some(j) = next_struct(items, i) {
        i = j;
        let insn = match &items[j] {
            Item::Label(_) => continue,
            Item::Insn(insn) => insn,
            _ => return false,
        };
        match insn.mnem.as_str() {
            "JSR" | "JMP" => {
                if insn_is_smc_target(items, j, smc) {
                    return false;
                }
                let Some(target) = insn.operand.as_deref() else {
                    return false;
                };
                if let Some(eff) = config.jsr_effect(target) {
                    return !reads_nz(eff.uses) && defines_nz(eff.defs);
                }
                match labels.get(target) {
                    Some(&at) if depth > 0 => {
                        return nz_dead_after(items, at, labels, smc, config, depth - 1);
                    }
                    _ => return false,
                }
            }
            m if is_cond_branch(m) => return false,
            "RTS" | "RTI" | "BRK" | "PHP" => return false,
            _ => {
                let fx = insn_flag_effect_inner(insn, false, config);
                if reads_nz(fx.uses) {
                    return false;
                }
                if defines_nz(fx.defs) {
                    return true;
                }
            }
        }
    }
    false
}

/// Branch reach estimate with room for size uncertainty.
const PH325_REACH: i32 = 112;

/// Byte size of data directives and equates.
fn directive_bytes(raw: &str) -> Option<i32> {
    let body = raw.split(';').next().unwrap_or("").trim();
    let count = |list: &str| -> Option<i32> {
        let n = list.split(',').map(str::trim).filter(|e| !e.is_empty()).count();
        (!list.contains('"') && !list.contains('\'')).then_some(n as i32)
    };
    if let Some(list) = body.strip_prefix(".byte ") {
        return count(list);
    }
    if let Some(list) = body.strip_prefix(".word ") {
        return count(list).map(|n| 2 * n);
    }
    (!body.starts_with('.') && body.contains('=')).then_some(0)
}

/// Instructions PH326 walks back from a call.
const PH326_RUN: usize = 3;
/// Maximum rounds of shared call prefix extraction.
const PH326_ROUNDS: usize = 4;

/// Share repeated call prefixes through a new routine entry.
fn ph326_absorb_shared_call_prefix(items: &mut Vec<Item>) -> bool {
    let mut any = false;
    for _ in 0..PH326_ROUNDS {
        if !ph326_round(items) {
            break;
        }
        any = true;
    }
    any
}

/// Follow shared prefix entries to their routine.
fn ph326_chain_end(items: &[Item], at: usize) -> Option<&str> {
    let Item::Label(name) = &items[at] else { return None };
    if !name.starts_with("__PH326_") {
        return Some(name);
    }
    items[at + 1..].iter().find_map(|it| match it {
        Item::Label(n) if !n.starts_with("__PH326_") => Some(Some(n.as_str())),
        Item::Insn(_) | Item::Label(_) | Item::Blank | Item::Comment(_) => None,
        _ => Some(None),
    })?
}

fn ph326_touches_port(item: &Item, zp: &ZpEquates) -> bool {
    let Item::Insn(insn) = item else { return false };
    let Some(op) = insn.operand.as_deref() else { return false };
    if op.starts_with('#') {
        return false;
    }
    let base = op.split(',').next().unwrap_or(op).trim().trim_start_matches('(').trim_end_matches(')');
    matches!(parse_plain_addr(base).or_else(|| zp.map.get(base).copied()), Some(0x00 | 0x01))
}

fn ph326_round(items: &mut Vec<Item>) -> bool {
    type Key = (String, Option<String>);
    let zp = ZpEquates::scan(items);
    let smc = smc_patched_labels(items);
    let defined: std::collections::HashMap<&str, usize> = items
        .iter()
        .enumerate()
        .filter_map(|(i, it)| match it {
            Item::Label(name) => Some((name.as_str(), i)),
            _ => None,
        })
        .collect();
    let mut calls: std::collections::HashMap<String, Vec<(usize, Vec<usize>)>> =
        std::collections::HashMap::new();
    let mut refused: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (site, it) in items.iter().enumerate() {
        let Item::Insn(insn) = it else { continue };
        if !matches!(insn.mnem.as_str(), "JSR" | "JMP") {
            continue;
        }
        let Some(target) = insn.operand.as_deref() else { continue };
        if !defined.contains_key(target) {
            continue;
        }
        let inline_data = insn.mnem == "JSR"
            && matches!(next_struct(items, site), Some(n) if matches!(items[n], Item::Directive(_)));
        if insn_is_smc_target(items, site, &smc)
            || (inline_data
                && ph326_chain_end(items, defined[target]) != Some(STR_PRINT_INLINE_ENTRY))
        {
            refused.insert(target.to_string());
            continue;
        }
        let mut run = Vec::new();
        let mut at = site;
        while run.len() < PH326_RUN {
            let Some(p) = prev_struct(items, at) else { break };
            if crosses_sys_wrap(items, p, site)
                || relocatable_insn_key(items, p, &smc).is_none()
                || ph326_touches_port(&items[p], &zp)
            {
                break;
            }
            run.push(p);
            at = p;
        }
        calls.entry(target.to_string()).or_default().push((site, run));
    }

    let mut plans: Vec<(usize, usize, Vec<Key>, &str, Vec<(usize, Vec<usize>)>)> = Vec::new();
    for (target, sites) in &calls {
        if refused.contains(target) || smc.contains(target) || sites.len() < 2 {
            continue;
        }
        // The first label of the routine's label run, and what precedes it.
        let mut head = defined[target.as_str()];
        while let Some(p) = prev_struct(items, head) {
            if !matches!(items[p], Item::Label(_)) {
                break;
            }
            head = p;
        }
        let barrier = prev_struct(items, head).is_some_and(
            |p| matches!(&items[p], Item::Insn(i) if is_uncond_terminator(&i.mnem)),
        );
        if !barrier {
            continue;
        }
        let key_of = |idx: usize| relocatable_insn_key(items, idx, &smc).expect("screened");
        let mut best: Option<(usize, Vec<Key>)> = None;
        for len in 1..=PH326_RUN {
            let mut counts: std::collections::HashMap<Vec<Key>, usize> =
                std::collections::HashMap::new();
            for (_, run) in sites {
                if run.len() >= len {
                    let suffix: Vec<Key> = run[..len].iter().rev().map(|&i| key_of(i)).collect();
                    *counts.entry(suffix).or_default() += 1;
                }
            }
            for (suffix, n) in counts {
                if n < 2 {
                    continue;
                }
                let bytes: usize = sites
                    .iter()
                    .find(|(_, run)| {
                        run.len() >= len
                            && run[..len].iter().rev().map(|&i| key_of(i)).eq(suffix.iter().cloned())
                    })
                    .map(|(_, run)| {
                        run[..len]
                            .iter()
                            .map(|&i| match &items[i] {
                                Item::Insn(insn) => approx_insn_bytes(insn, &zp),
                                _ => 0,
                            })
                            .sum()
                    })
                    .unwrap_or(0);
                let gain = (n - 1) * bytes;
                if gain > 0 && best.as_ref().is_none_or(|(g, b)| gain > *g || (gain == *g && suffix < *b)) {
                    best = Some((gain, suffix));
                }
            }
        }
        let Some((gain, suffix)) = best else { continue };
        let len = suffix.len();
        let chosen: Vec<(usize, Vec<usize>)> = sites
            .iter()
            .filter(|(_, run)| {
                run.len() >= len && run[..len].iter().rev().map(|&i| key_of(i)).eq(suffix.iter().cloned())
            })
            .map(|(site, run)| (*site, run[..len].to_vec()))
            .collect();
        plans.push((head, gain, suffix, target.as_str(), chosen));
    }
    if plans.is_empty() {
        return false;
    }
    plans.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)).then_with(|| (&a.2, a.3).cmp(&(&b.2, b.3))));
    plans.dedup_by_key(|p| p.0);
    let plans: Vec<(usize, Vec<Key>, Vec<(usize, Vec<usize>)>)> =
        plans.into_iter().map(|(head, _, suffix, _, chosen)| (head, suffix, chosen)).collect();
    let mut next_id = items
        .iter()
        .filter_map(|it| match it {
            Item::Label(n) => n.strip_prefix("__PH326_")?.parse::<usize>().ok(),
            _ => None,
        })
        .max()
        .map_or(0, |n| n + 1);
    let mut drop: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut retarget: std::collections::HashMap<usize, String> = std::collections::HashMap::new();
    let mut entries: std::collections::HashMap<usize, Vec<Item>> = std::collections::HashMap::new();
    for (head, suffix, chosen) in plans {
        let entry = format!("__PH326_{next_id}");
        next_id += 1;
        let model = &chosen[0].1;
        let mut block = vec![Item::Label(entry.clone())];
        for &i in model.iter().rev() {
            let Item::Insn(insn) = &items[i] else { unreachable!() };
            block.push(Item::Insn(Insn { comment: None, ..insn.clone() }));
        }
        debug_assert_eq!(block.len(), suffix.len() + 1);
        for (site, run) in chosen {
            drop.extend(run);
            retarget.insert(site, entry.clone());
        }
        entries.insert(head, block);
    }
    let mut out = Vec::with_capacity(items.len());
    for (i, it) in items.drain(..).enumerate() {
        if let Some(block) = entries.remove(&i) {
            out.extend(block);
        }
        if drop.contains(&i) {
            continue;
        }
        match (it, retarget.get(&i)) {
            (Item::Insn(mut insn), Some(entry)) => {
                insn.operand = Some(entry.clone());
                out.push(Item::Insn(insn));
            }
            (it, _) => out.push(it),
        }
    }
    *items = out;
    true
}

/// Aim a branch at a reachable shared jump or its target.
fn ph325_branch_via_shared_jmp(items: &mut Vec<Item>) -> bool {
    let zp = ZpEquates::scan(items);
    let smc = smc_patched_labels(items);
    let mut pos: Vec<(u32, i32)> = Vec::with_capacity(items.len());
    let (mut seg, mut off) = (0u32, 0i32);
    for it in items.iter() {
        pos.push((seg, off));
        match it {
            Item::Insn(insn) => off += approx_insn_bytes(insn, &zp) as i32,
            Item::Directive(d) => match directive_bytes(d) {
                Some(n) => off += n,
                None => {
                    seg += 1;
                    off = 0;
                }
            },
            Item::Origin(_) => {
                seg += 1;
                off = 0;
            }
            Item::Label(_) | Item::Blank | Item::Comment(_) => {}
        }
    }
    let jmp_target = |idx: usize| -> Option<&str> {
        match &items[idx] {
            Item::Insn(i) if i.mnem == "JMP" => {
                let op = i.operand.as_deref()?.trim();
                (!op.starts_with('(') && !insn_is_smc_target(items, idx, &smc)).then_some(op)
            }
            _ => None,
        }
    };
    let mut anchors: std::collections::HashMap<&str, Vec<usize>> = std::collections::HashMap::new();
    for idx in 0..items.len() {
        if let Some(t) = jmp_target(idx) {
            anchors.entry(t).or_default().push(idx);
        }
    }
    let labels = collect_label_index(items);

    enum Aim {
        Label(String),
        Anchor(usize),
    }
    struct Site {
        branch: usize,
        aim: Aim,
    }
    let mut pairs: Vec<(usize, usize, &str)> = Vec::new();
    for b in 0..items.len() {
        let Item::Insn(br) = &items[b] else { continue };
        if !is_cond_branch(&br.mnem) || insn_is_smc_target(items, b, &smc) {
            continue;
        }
        let Some(skip) = br.operand.as_deref() else { continue };
        let Some(j) = next_struct(items, b) else { continue };
        let Some(x) = jmp_target(j) else { continue };
        let mut k = j;
        let mut lands = false;
        while let Some(n) = next_struct(items, k) {
            match &items[n] {
                Item::Label(name) => {
                    if name == skip {
                        lands = true;
                        break;
                    }
                    k = n;
                }
                _ => break,
            }
        }
        if lands {
            pairs.push((b, j, x));
        }
    }
    let pair_jmps: std::collections::HashSet<usize> = pairs.iter().map(|&(_, j, _)| j).collect();

    let mut sites: Vec<Site> = Vec::new();
    let mut removed: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut used: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for &(b, j, x) in &pairs {
        if used.contains(&j) {
            continue;
        }
        let from = (pos[b].0, pos[b].1 + 2);
        let reach = |at: usize| -> Option<i32> {
            let (s, o) = pos[at];
            let d = o - from.1;
            (s == from.0 && (-PH325_REACH..=PH325_REACH).contains(&d)).then_some(d.abs())
        };
        let direct = labels.get(x).and_then(|&at| reach(at)).map(|d| (d, Aim::Label(x.to_string())));
        let shared = anchors
            .get(x)
            .into_iter()
            .flatten()
            .filter(|&&a| a != j && !removed.contains(&a))
            .filter_map(|&a| {
                let rank = if used.contains(&a) {
                    0
                } else if !pair_jmps.contains(&a) {
                    1
                } else {
                    2
                };
                reach(a).map(|d| ((rank, d), Aim::Anchor(a)))
            })
            .min_by_key(|(key, _)| *key);
        let aim = match (direct, shared) {
            (Some(d), _) => d.1,
            (None, Some(s)) => s.1,
            (None, None) => continue,
        };
        if let Aim::Anchor(a) = aim {
            used.insert(a);
        }
        removed.insert(j);
        sites.push(Site { branch: b, aim });
    }
    if sites.is_empty() {
        return false;
    }

    // An anchor reuses a label already naming it, else gets a fresh one.
    let mut next_id = 0u32;
    for it in items.iter() {
        if let Item::Label(n) = it
            && let Some(rest) = n.strip_prefix("__PH325_")
            && let Ok(v) = rest.parse::<u32>()
        {
            next_id = next_id.max(v + 1);
        }
    }
    let mut anchor_label: std::collections::HashMap<usize, String> = std::collections::HashMap::new();
    let mut insert_before: std::collections::HashMap<usize, String> = std::collections::HashMap::new();
    for site in &sites {
        if let Aim::Anchor(a) = site.aim
            && !anchor_label.contains_key(&a)
        {
            let name = match prev_struct(items, a).map(|p| &items[p]) {
                Some(Item::Label(n)) => n.clone(),
                _ => {
                    let n = format!("__PH325_{next_id}");
                    next_id += 1;
                    insert_before.insert(a, n.clone());
                    n
                }
            };
            anchor_label.insert(a, name);
        }
    }
    let mut rewrite: std::collections::HashMap<usize, (&'static str, String)> = std::collections::HashMap::new();
    for site in &sites {
        let Item::Insn(br) = &items[site.branch] else { unreachable!() };
        let inv = invert_branch(&br.mnem).expect("conditional branch");
        let target = match &site.aim {
            Aim::Label(l) => l.clone(),
            Aim::Anchor(a) => anchor_label[a].clone(),
        };
        rewrite.insert(site.branch, (inv, target));
    }
    let old = std::mem::take(items);
    for (idx, it) in old.into_iter().enumerate() {
        if let Some(name) = insert_before.remove(&idx) {
            items.push(Item::Label(name));
        }
        if removed.contains(&idx) {
            continue;
        }
        match (rewrite.remove(&idx), it) {
            (Some((mnem, target)), Item::Insn(mut br)) => {
                br.mnem = mnem.to_string();
                br.operand = Some(target);
                items.push(Item::Insn(br));
            }
            (_, it) => items.push(it),
        }
    }
    true
}

/// Most window instructions PH324 hoists a copy past.
const PH324_WINDOW: usize = 6;

/// Replace reloads with earlier register transfers when values and flags permit it.
fn ph324_hoist_reload_to_transfer(items: &mut Vec<Item>) -> bool {
    if !items.iter().any(|it| {
        matches!(it, Item::Insn(i) if matches!(i.mnem.as_str(), "LDA" | "LDX" | "LDY")
            && i.operand.as_deref().is_some_and(|op| !op.starts_with('#')))
    }) {
        return false;
    }
    let smc = smc_patched_labels(items);
    let equ = collect_equ_values(items);
    let equ_names: std::collections::HashSet<String> = items
        .iter()
        .filter_map(|it| match it {
            Item::Directive(d) => d.trim().split_once('=').map(|(n, _)| n.trim().to_string()),
            _ => None,
        })
        .collect();
    let flags = analyze_flag_liveness_with_config(items, &FlagLivenessConfig::default());
    let contracts = conservative_return_flag_config();
    let labels = collect_label_index(items);
    let base_of = |op: &str| -> String {
        op.split(['+', '-']).next().unwrap_or(op).trim().to_string()
    };
    let resolve = |op: &str| -> Option<u16> {
        let (base, off) = match op.split_once('+') {
            Some((b, o)) => (b.trim(), i32::from(o.trim().parse::<u16>().ok()?)),
            None => match op.split_once('-') {
                Some((b, o)) => (b.trim(), -i32::from(o.trim().parse::<u16>().ok()?)),
                None => (op.trim(), 0),
            },
        };
        let v = match base.strip_prefix('$') {
            Some(_) => parse_plain_addr(base)?,
            None => *equ.get(base)?,
        };
        u16::try_from(i32::from(v) + off).ok()
    };
    let unresolved_equate = |op: &str| resolve(op).is_none() && equ_names.contains(&base_of(op));
    let may_hit = |w: &str, m: &str| -> bool {
        if w == m
            || w.contains(',')
            || w.contains('(')
            || matches!(resolve(w), Some(0x00 | 0x01))
            || unresolved_equate(w)
        {
            return true;
        }
        match (resolve(w), resolve(m)) {
            (Some(a), Some(b)) => a == b,
            (Some(a), None) => a >= 0x0100,
            (None, Some(b)) => b >= 0x0100,
            // Two distinct storage/code labels name distinct bytes.
            (None, None) => false,
        }
    };

    struct Site {
        store: usize,
        reload: usize,
        copy: &'static str,
    }
    let mut sites: Vec<Site> = Vec::new();
    let mut claimed_upto = 0usize;
    for j in 0..items.len() {
        let Item::Insn(ld) = &items[j] else { continue };
        let (dest, store_mnem, copy) = match ld.mnem.as_str() {
            "LDX" => (Reg::X, "STA", "TAX"),
            "LDY" => (Reg::Y, "STA", "TAY"),
            "LDA" => (Reg::A, "", ""),
            _ => continue,
        };
        let Some(m) = ld.operand.as_deref() else { continue };
        if static_fact_mem_operand(m).is_none()
            || unresolved_equate(m)
            || resolve(m).is_some_and(|a| a <= 0x01 || a >= 0x0100)
        {
            continue;
        }
        if insn_is_smc_target(items, j, &smc) {
            continue;
        }
        // Walk back to the store, auditing the window.
        let mut k = j;
        let mut window = 0usize;
        let mut window_sets_nz = false;
        let found = loop {
            let Some(p) = prev_struct(items, k) else { break None };
            k = p;
            let Item::Insn(x) = &items[p] else { break None };
            let is_store = match dest {
                Reg::A => matches!(x.mnem.as_str(), "STX" | "STY"),
                _ => x.mnem == store_mnem,
            };
            if is_store && x.operand.as_deref() == Some(m) {
                let copy = match (dest, x.mnem.as_str()) {
                    (Reg::A, "STX") => "TXA",
                    (Reg::A, _) => "TYA",
                    _ => copy,
                };
                break Some((p, copy));
            }
            let modelled = matches!(
                x.mnem.as_str(),
                "LDA" | "LDX" | "LDY" | "STA" | "STX" | "STY" | "TAX" | "TAY" | "TXA" | "TYA"
                    | "TSX" | "PHA" | "PLA" | "ADC" | "SBC" | "AND" | "ORA" | "EOR" | "CMP"
                    | "CPX" | "CPY" | "BIT" | "INC" | "DEC" | "INX" | "DEX" | "INY" | "DEY"
                    | "ASL" | "LSR" | "ROL" | "ROR" | "CLC" | "SEC" | "CLV" | "CLD" | "CLI"
                    | "SEI" | "NOP"
            );
            if window == PH324_WINDOW || !modelled {
                break None;
            }
            let eff = insn_register_effect_inner(x, false, &RegisterLivenessConfig::default());
            if eff.uses.contains(dest) || eff.defs.contains(dest) {
                break None;
            }
            let writes_mem = match x.mnem.as_str() {
                "STA" | "STX" | "STY" => true,
                "INC" | "DEC" | "ASL" | "LSR" | "ROL" | "ROR" => x
                    .operand
                    .as_deref()
                    .is_some_and(|op| !op.eq_ignore_ascii_case("A")),
                _ => false,
            };
            if writes_mem && x.operand.as_deref().is_none_or(|w| may_hit(w, m)) {
                break None;
            }
            let fx = insn_flag_effect_inner(x, false, &FlagLivenessConfig::default());
            window_sets_nz |= fx.defs.contains(Flag::N) || fx.defs.contains(Flag::Z);
            window += 1;
        };
        let Some((st, copy)) = found else { continue };
        if st < claimed_upto || insn_is_smc_target(items, st, &smc) || crosses_sys_wrap(items, st, j) {
            continue;
        }
        if window > 0 {
            let nz_live = |idx: usize| flags.is_live_out(idx, Flag::N) || flags.is_live_out(idx, Flag::Z);
            if nz_live(st)
                || (window_sets_nz
                    && nz_live(j)
                    && !nz_dead_after(items, j, &labels, &smc, &contracts, 3))
            {
                continue;
            }
        }
        claimed_upto = j + 1;
        sites.push(Site { store: st, reload: j, copy });
    }
    if sites.is_empty() {
        return false;
    }
    for site in sites.into_iter().rev() {
        items.remove(site.reload);
        items.insert(
            site.store + 1,
            Item::Insn(Insn {
                mnem: site.copy.to_string(),
                operand: None,
                comment: None,
            }),
        );
    }
    true
}

fn ph022_jmp_to_rts(items: &mut Vec<Item>) -> bool {
    let label_to_idx = collect_label_index(items);
    let mut plans: Vec<usize> = Vec::new();

    for (idx, item) in items.iter().enumerate() {
        let Item::Insn(insn) = item else { continue };
        if insn.mnem != "JMP" {
            continue;
        }
        let Some(target) = insn.operand.as_deref() else {
            continue;
        };
        let target = target.trim();
        let Some(&label_idx) = label_to_idx.get(target) else {
            continue;
        };
        if first_real_insn_is_rts(items, label_idx) {
            plans.push(idx);
        }
    }

    if plans.is_empty() {
        return false;
    }

    for idx in plans.into_iter().rev() {
        if let Some(Item::Insn(insn)) = items.get_mut(idx) {
            insn.mnem = "RTS".to_string();
            insn.operand = None;
        }
    }
    true
}

fn collect_label_index(items: &[Item]) -> std::collections::HashMap<String, usize> {
    let mut out = std::collections::HashMap::new();
    for (idx, item) in items.iter().enumerate() {
        if let Item::Label(label) = item {
            out.insert(label.clone(), idx);
        }
    }
    out
}

fn first_real_insn_is_rts(items: &[Item], label_idx: usize) -> bool {
    for item in &items[label_idx + 1..] {
        match item {
            Item::Insn(insn) => return insn.mnem == "RTS" && insn.operand.is_none(),
            Item::Label(_) | Item::Directive(_) | Item::Origin(_) => return false,
            Item::Blank | Item::Comment(_) => continue,
        }
    }
    false
}

/// Fold conditional branches when their flags are known.
fn ph021_constant_branch_fold(items: &mut Vec<Item>) -> bool {
    let facts = analyze_static_asm_facts(items);

    enum Plan {
        Drop,
        Jmp,
    }

    let mut plans: Vec<(usize, Plan)> = Vec::new();

    for (idx, item) in items.iter().enumerate() {
        let Item::Insn(insn) = item else { continue };
        let Some(taken) = static_branch_outcome(&facts[idx], &insn.mnem) else {
            continue;
        };
        // Rewrite only plain label targets.
        let Some(op) = insn.operand.as_deref() else {
            continue;
        };
        if op.trim().is_empty() {
            continue;
        }
        plans.push((idx, if taken { Plan::Jmp } else { Plan::Drop }));
    }

    if plans.is_empty() {
        return false;
    }

    for (idx, plan) in plans.into_iter().rev() {
        match plan {
            Plan::Drop => {
                items.remove(idx);
            }
            Plan::Jmp => {
                if let Some(Item::Insn(insn)) = items.get_mut(idx) {
                    insn.mnem = "JMP".to_string();
                }
            }
        }
    }
    true
}

fn static_branch_outcome(facts: &StaticAsmFacts, mnem: &str) -> Option<bool> {
    match mnem {
        "BEQ" => facts.flag_z,
        "BNE" => facts.flag_z.map(|z| !z),
        "BMI" => facts.flag_n,
        "BPL" => facts.flag_n.map(|n| !n),
        "BCC" => facts.flag_c.map(|c| !c),
        "BCS" => facts.flag_c,
        _ => None,
    }
}

/// Remove CMP zero only when N/Z match A and carry is dead.
fn ph012_drop_cmp_zero(items: &mut Vec<Item>) -> bool {
    let mut changed = false;
    let mut i = 0;
    while i + 2 < items.len() {
        let drop = matches!(
            (&items[i], next_struct(items, i).map(|j| (&items[j], j))),
            (Item::Insn(prev), Some((Item::Insn(cmp), _)))
                if sets_z_flag_for_cmp_zero(prev)
                && cmp.mnem == "CMP"
                && cmp.operand.as_deref() == Some("#$00")
        );
        if drop && let Some(j) = next_struct(items, i) {
            // Keep comparisons whose carry is read before redefinition.
            let mut scan = next_struct(items, j);
            let mut reads_c = false;
            while let Some(k) = scan {
                let Item::Insn(insn) = &items[k] else { break };
                match insn.mnem.as_str() {
                    "BCC" | "BCS" => {
                        reads_c = true;
                        break;
                    }
                    "SBC" | "ADC" | "ROL" | "ROR" | "PHP" => {
                        reads_c = true;
                        break;
                    }
                    "SEC" | "CLC" | "CMP" | "CPX" | "CPY" | "ASL" | "LSR" => {
                        break;
                    }
                    "BEQ" | "BNE" | "BMI" | "BPL" | "BVC" | "BVS" => {
                        // Continue checking for carry readers past N, Z and V branches.
                        scan = next_struct(items, k);
                    }
                    "JMP" | "RTS" | "RTI" | "BRK" | "JSR" => {
                        break;
                    }
                    _ => {
                        scan = next_struct(items, k);
                    }
                }
            }
            if reads_c {
                i += 1;
                continue;
            }
            items.remove(j);
            changed = true;
            // Recheck the same predecessor after removal.
            continue;
        }
        i += 1;
    }
    changed
}

/// Instructions whose Z flag already tests A for zero.
fn sets_z_flag_for_cmp_zero(insn: &Insn) -> bool {
    match insn.mnem.as_str() {
        "LDA" | "TAX" | "TAY" | "TXA" | "TYA" | "AND" | "ORA" | "EOR" | "ADC" | "SBC" | "PLA" => {
            true
        }
        // Memory shifts set Z from memory rather than A.
        "ASL" | "LSR" | "ROL" | "ROR" => insn.operand.is_none(),
        "JSR" => insn.operand.as_deref() == Some("__STRCMP"),
        _ => false,
    }
}

/// Extract label tokens from instruction operands.
fn extract_labels_from_operand(op: &str) -> Vec<String> {
    let mut out = Vec::new();
    // Skip identifiers inside hexadecimal literals.
    let bytes = op.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < bytes.len() {
                let cc = bytes[i] as char;
                if cc.is_ascii_alphanumeric() || cc == '_' {
                    i += 1;
                } else {
                    break;
                }
            }
            let before_is_hex_marker = start > 0 && bytes[start - 1] == b'$';
            // Skip register-name pseudo-operands (",X", ",Y" tail).
            let token = &op[start..i];
            if !before_is_hex_marker && token != "X" && token != "Y" && token != "A" {
                out.push(token.to_string());
            }
        } else {
            i += 1;
        }
    }
    out
}

/// Extract labels from directive operands without comments.
fn directive_operand_labels(raw: &str) -> Vec<String> {
    let body = raw.split(';').next().unwrap_or(raw).trim();
    let operands = match body.strip_prefix('.') {
        Some(rest) => rest.split_once(char::is_whitespace).map_or("", |(_, o)| o),
        None => body,
    };
    extract_labels_from_operand(operands)
}

/// Share repeated byte to FAC conversions.
fn ph106_factor_int_byte_to_fac(items: &mut Vec<Item>) -> bool {

    if items.iter().any(|item| matches!(item, Item::Label(name) if name == "__LD_BYTE_FAC")) {
        return false;
    }
    let mut sites: Vec<usize> = Vec::new();
    let mut i = 0;
    while i + 2 < items.len() {
        if let (Item::Insn(a), Some(j)) = (&items[i], next_struct(items, i))
            && a.mnem == "LDA"
            && a.operand.as_deref() == Some("#$00")
            && let Item::Insn(b) = &items[j]
            && b.mnem == "LDY"
            && b.operand
                .as_deref()
                .map(|o| o.starts_with("#$"))
                .unwrap_or(false)
            && let Some(k) = next_struct(items, j)
            && let Item::Insn(c) = &items[k]
            && c.mnem == "JSR"
            && c.operand.as_deref() == Some("$B391")
        {
            sites.push(i);
            i = k + 1;
            continue;
        }
        i += 1;
    }
    if sites.len() < 4 {
        return false;
    }
    // Rewrite from the end so instruction indices stay valid.
    for &start in sites.iter().rev() {
        let j = next_struct(items, start).expect("paired LDY");
        let k = next_struct(items, j).expect("paired JSR");
        if let Item::Insn(insn) = &mut items[k] {
            insn.operand = Some("__LD_BYTE_FAC".to_string());
        }
        items.remove(start);
    }
    // Place helpers before the runtime heap.
    let mut helpers: Vec<Item> = Vec::new();
    if !helper_section_present(items) {
        helpers.push(Item::Blank);
        helpers.push(Item::Comment(
            "; --- peephole-factored helpers ---".to_string(),
        ));
    }
    helpers.push(Item::Label("__LD_BYTE_FAC".to_string()));
    helpers.push(Item::Insn(Insn {
        mnem: "LDA".to_string(),
        operand: Some("#$00".to_string()),
        comment: None,
    }));
    helpers.push(Item::Insn(Insn {
        mnem: "JMP".to_string(),
        operand: Some("$B391".to_string()),
        comment: None,
    }));
    append_helpers_before_heap(items, helpers);
    true
}

/// Share byte to FAC conversions with the same constant.
fn ph109_factor_per_imm_byte_fac(items: &mut Vec<Item>) -> bool {
    use std::collections::HashMap;
    // Pre-scan for `LDY #$xx / JSR __LD_BYTE_FAC` pairs.
    let mut sites_by_imm: HashMap<u8, Vec<usize>> = HashMap::new();
    let mut i = 0;
    while i + 1 < items.len() {
        if let Item::Insn(a) = &items[i]
            && a.mnem == "LDY"
            && let Some(op) = a.operand.as_deref()
            && let Some(imm) = parse_byte_immediate(op)
            && let Some(j) = next_struct(items, i)
            && let Item::Insn(b) = &items[j]
            && b.mnem == "JSR"
            && b.operand.as_deref() == Some("__LD_BYTE_FAC")
        {
            sites_by_imm.entry(imm).or_default().push(i);
            i = j + 1;
            continue;
        }
        i += 1;
    }
    let mut to_factor: Vec<(u8, Vec<usize>)> = sites_by_imm
        .into_iter()
        .filter(|(imm, sites)| sites.len() >= 4 && !items.iter().any(|item|
            matches!(item, Item::Label(name) if name == &format!("__LDV_{imm:02X}_FAC"))))
        .collect();
    if to_factor.is_empty() {
        return false;
    }
    to_factor.sort_by_key(|(imm, _)| *imm);

    let mut removals: Vec<usize> = Vec::new();
    for (imm, sites) in &to_factor {
        let helper = format!("__LDV_{:02X}_FAC", imm);
        for &start in sites {
            let j = next_struct(items, start).expect("paired JSR");
            // Retarget the JSR.
            if let Item::Insn(insn) = &mut items[j] {
                insn.operand = Some(helper.clone());
            }
            // Drop the LDY at `start`.
            removals.push(start);
        }
    }
    removals.sort_unstable_by(|a, b| b.cmp(a));
    for idx in removals {
        items.remove(idx);
    }

    // Place helpers before the runtime heap.
    let mut helpers: Vec<Item> = Vec::new();
    if !helper_section_present(items) {
        helpers.push(Item::Blank);
        helpers.push(Item::Comment(
            "; --- peephole-factored helpers ---".to_string(),
        ));
    }
    for (imm, _) in &to_factor {
        helpers.push(Item::Label(format!("__LDV_{:02X}_FAC", imm)));
        helpers.push(Item::Insn(Insn {
            mnem: "LDA".to_string(),
            operand: Some("#$00".to_string()),
            comment: None,
        }));
        helpers.push(Item::Insn(Insn {
            mnem: "LDY".to_string(),
            operand: Some(format!("#${:02X}", imm)),
            comment: None,
        }));
        helpers.push(Item::Insn(Insn {
            mnem: "JMP".to_string(),
            operand: Some("$B391".to_string()),
            comment: None,
        }));
    }
    append_helpers_before_heap(items, helpers);
    true
}

/// Share repeated constant character output calls.
fn ph107_factor_chrout_byte(items: &mut Vec<Item>) -> bool {
    use std::collections::HashMap;
    // Collect output calls by immediate byte.
    let mut sites_by_imm: HashMap<u8, Vec<usize>> = HashMap::new();
    let mut i = 0;
    while i + 1 < items.len() {
        if let Item::Insn(a) = &items[i]
            && a.mnem == "LDA"
            && let Some(op) = a.operand.as_deref()
            && let Some(imm) = parse_byte_immediate(op)
            && let Some(j) = next_struct(items, i)
            && let Item::Insn(b) = &items[j]
            && b.mnem == "JSR"
            && b.operand.as_deref() == Some("$FFD2")
        {
            sites_by_imm.entry(imm).or_default().push(i);
            i = j + 1;
            continue;
        }
        i += 1;
    }
    // Collect imm-values that pass threshold.
    let mut to_factor: Vec<(u8, Vec<usize>)> = sites_by_imm
        .into_iter()
        .filter(|(imm, sites)| sites.len() >= 4 && !items.iter().any(|item|
            matches!(item, Item::Label(name) if name == &format!("__CHROUT_{imm:02X}"))))
        .collect();
    if to_factor.is_empty() {
        return ph107_sweep_chrout_tails(items);
    }
    to_factor.sort_by_key(|(imm, _)| *imm);

    let mut removals: Vec<usize> = Vec::new();
    for (imm, sites) in &to_factor {
        let helper = format!("__CHROUT_{:02X}", imm);
        for &start in sites {
            let j = next_struct(items, start).expect("paired JSR");
            // Retarget the JSR.
            if let Item::Insn(insn) = &mut items[j] {
                insn.operand = Some(helper.clone());
            }
            // Mark the LDA for removal.
            removals.push(start);
        }
    }
    removals.sort_unstable_by(|a, b| b.cmp(a));
    for idx in removals {
        items.remove(idx);
    }

    // Emit one helper per constant before the runtime heap.
    let mut helpers: Vec<Item> = Vec::new();
    if !helper_section_present(items) {
        helpers.push(Item::Blank);
        helpers.push(Item::Comment(
            "; --- peephole-factored helpers ---".to_string(),
        ));
    }
    for (imm, _) in &to_factor {
        helpers.push(Item::Label(format!("__CHROUT_{:02X}", imm)));
        helpers.push(Item::Insn(Insn {
            mnem: "LDA".to_string(),
            operand: Some(format!("#${:02X}", imm)),
            comment: None,
        }));
        helpers.push(Item::Insn(Insn {
            mnem: "JMP".to_string(),
            operand: Some("$FFD2".to_string()),
            comment: None,
        }));
    }
    append_helpers_before_heap(items, helpers);
    let _ = ph107_sweep_chrout_tails(items);
    true
}

/// Reuse existing character output helpers for tail calls.
fn ph107_sweep_chrout_tails(items: &mut Vec<Item>) -> bool {
    let existing: std::collections::HashSet<String> = items
        .iter()
        .filter_map(|it| match it {
            Item::Label(name) if name.starts_with("__CHROUT_") => Some(name.clone()),
            _ => None,
        })
        .collect();
    if existing.is_empty() {
        return false;
    }
    let mut removals: Vec<usize> = Vec::new();
    let mut i = 0;
    while i + 1 < items.len() {
        let matched = (|| {
            let Item::Insn(a) = &items[i] else { return None };
            if a.mnem != "LDA" {
                return None;
            }
            let imm = parse_byte_immediate(a.operand.as_deref()?)?;
            let helper = format!("__CHROUT_{imm:02X}");
            if !existing.contains(&helper) {
                return None;
            }
            let j = next_struct(items, i)?;
            let Item::Insn(b) = &items[j] else { return None };
            if b.mnem != "JMP" || b.operand.as_deref() != Some("$FFD2") {
                return None;
            }
            // Skip every label belonging to the helper itself.
            let mut p = i;
            while p > 0 {
                p -= 1;
                match &items[p] {
                    Item::Blank | Item::Comment(_) => {}
                    Item::Label(n) if n == &helper => return None,
                    Item::Label(_) => {}
                    _ => break,
                }
            }
            Some((j, helper))
        })();
        match matched {
            Some((j, helper)) => {
                if let Item::Insn(insn) = &mut items[j] {
                    insn.operand = Some(helper);
                }
                removals.push(i);
                i = j + 1;
                continue;
            }
            None => i += 1,
        }
    }
    if removals.is_empty() {
        return false;
    }
    removals.sort_unstable_by(|a, b| b.cmp(a));
    for idx in removals {
        items.remove(idx);
    }
    true
}

/// Parse a byte immediate, or return None.
fn parse_byte_immediate(op: &str) -> Option<u8> {
    let rest = op.strip_prefix("#$")?;
    if rest.len() != 2 {
        return None;
    }
    u8::from_str_radix(rest, 16).ok()
}

/// Cleanup can leave holes in an earlier factoring round's IDs.
fn allocate_helper_name(items: &[Item], prefix: &str, next: &mut u32) -> String {
    loop {
        let candidate = format!("{prefix}{next}");
        *next += 1;
        if !items.iter().any(|item| matches!(item, Item::Label(name) if name == &candidate)) {
            return candidate;
        }
    }
}

/// Share repeated triples of calls.
fn ph113_factor_jsr_triple(items: &mut Vec<Item>) -> bool {
    use std::collections::HashMap;
    // The first two calls add frames. The tail jump does not.
    let sensitive = frame_sensitive_labels(items);
    let mut sites: HashMap<(String, String, String), Vec<usize>> = HashMap::new();
    let mut i = 0;
    while i + 2 < items.len() {
        if let Item::Insn(a) = &items[i]
            && a.mnem == "JSR"
            && let Some(t_a) = a.operand.as_deref()
            && !sensitive.contains(t_a)
            && let Some(j) = next_struct(items, i)
            && !crosses_sys_wrap(items, i, j)
            && let Item::Insn(b) = &items[j]
            && b.mnem == "JSR"
            && let Some(t_b) = b.operand.as_deref()
            && !sensitive.contains(t_b)
            && let Some(k) = next_struct(items, j)
            && !crosses_sys_wrap(items, j, k)
            && let Item::Insn(c) = &items[k]
            && c.mnem == "JSR"
            && let Some(t_c) = c.operand.as_deref()
        {
            sites
                .entry((t_a.to_string(), t_b.to_string(), t_c.to_string()))
                .or_default()
                .push(i);
            i = k + 1;
            continue;
        }
        i += 1;
    }
    let mut to_factor: Vec<((String, String, String), Vec<usize>)> = sites
        .into_iter()
        .filter(|(_, sites)| sites.len() >= 4)
        .collect();
    if to_factor.is_empty() {
        return false;
    }
    to_factor.sort_by(|a, b| a.0.cmp(&b.0));

    let mut helper_idx = 0u32;

    let mut emitted: Vec<((String, String, String), String)> = Vec::new();
    let mut all_removals: Vec<(usize, usize, usize)> = Vec::new();
    for (key, site_list) in &to_factor {
        let helper = allocate_helper_name(items, "__JSR3_", &mut helper_idx);
        emitted.push((key.clone(), helper.clone()));
        for &start in site_list {
            let mid = next_struct(items, start).unwrap();
            let end = next_struct(items, mid).unwrap();
            all_removals.push((start, mid, end));
        }
    }
    all_removals.sort_by(|a, b| b.0.cmp(&a.0));
    for (start, mid, end) in &all_removals {
        let (Item::Insn(a), Item::Insn(b), Item::Insn(c)) =
            (&items[*start], &items[*mid], &items[*end])
        else {
            continue;
        };
        let key = (
            a.operand.clone().unwrap_or_default(),
            b.operand.clone().unwrap_or_default(),
            c.operand.clone().unwrap_or_default(),
        );
        let helper_label = emitted
            .iter()
            .find(|(k, _)| k == &key)
            .map(|(_, l)| l.clone());
        let Some(helper_label) = helper_label else {
            continue;
        };
        if let Item::Insn(insn) = &mut items[*start] {
            insn.operand = Some(helper_label);
        }
        items.remove(*end);
        items.remove(*mid);
    }

    let mut helpers: Vec<Item> = Vec::new();
    if !helper_section_present(items) {
        helpers.push(Item::Blank);
        helpers.push(Item::Comment(
            "; --- peephole-factored helpers ---".to_string(),
        ));
    }
    for ((a, b, c), label) in &emitted {
        helpers.push(Item::Label(label.clone()));
        helpers.push(Item::Insn(Insn {
            mnem: "JSR".to_string(),
            operand: Some(a.clone()),
            comment: None,
        }));
        helpers.push(Item::Insn(Insn {
            mnem: "JSR".to_string(),
            operand: Some(b.clone()),
            comment: None,
        }));
        helpers.push(Item::Insn(Insn {
            mnem: "JMP".to_string(),
            operand: Some(c.clone()),
            comment: None,
        }));
    }
    append_helpers_before_heap(items, helpers);
    true
}

/// Share repeated adjacent calls through a helper ending in a tail jump.
fn ph110_factor_jsr_pair(items: &mut Vec<Item>) -> bool {
    use std::collections::HashMap;
    /// An operand the program rewrites at run time.
    const PATCHED: &str = "$FFFF";

    let sensitive = frame_sensitive_labels(items);
    let mut sites_by_pair: HashMap<(String, String), Vec<usize>> = HashMap::new();
    let mut i = 0;
    while i + 1 < items.len() {
        if let Item::Insn(a) = &items[i]
            && a.mnem == "JSR"
            && let Some(target_a) = a.operand.as_deref()
            && target_a != PATCHED
            && !sensitive.contains(target_a)
            && let Some(j) = next_struct(items, i)
            && !crosses_sys_wrap(items, i, j)
            && let Item::Insn(b) = &items[j]
            && b.mnem == "JSR"
            && let Some(target_b) = b.operand.as_deref()
            && target_b != PATCHED
        {
            sites_by_pair
                .entry((target_a.to_string(), target_b.to_string()))
                .or_default()
                .push(i);
            // Advance one instruction so overlapping pairs remain candidates.
            i += 1;
            continue;
        }
        i += 1;
    }
    let mut to_factor: Vec<((String, String), Vec<usize>)> = sites_by_pair
        .into_iter()
        .filter(|(_, sites)| sites.len() >= 4)
        .collect();
    if to_factor.is_empty() {
        return false;
    }
    // Sort deterministically for stable helper naming.
    to_factor.sort_by(|a, b| a.0.cmp(&b.0));

    let mut helper_idx = 0u32;

    // Stage rewrites first, mutate after.
    let mut emitted: Vec<((String, String), String)> = Vec::new();
    let mut all_removals: Vec<(usize, usize)> = Vec::new(); // (start, second_jsr_idx)
    for ((a, b), sites) in &to_factor {
        let helper = allocate_helper_name(items, "__JSR2_", &mut helper_idx);
        emitted.push(((a.clone(), b.clone()), helper.clone()));
        for &start in sites {
            let j = next_struct(items, start).expect("paired JSR");
            all_removals.push((start, j));
        }
    }
    // Rewrite in reverse so removals don't shift earlier indices.
    all_removals.sort_by(|a, b| b.0.cmp(&a.0));
    let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (start, second) in &all_removals {
        // Determine target pair from the (still-present) instructions.
        let (Item::Insn(a_insn), Item::Insn(b_insn)) = (&items[*start], &items[*second]) else {
            continue;
        };
        let pair = (
            a_insn.operand.clone().unwrap_or_default(),
            b_insn.operand.clone().unwrap_or_default(),
        );
        let helper = emitted
            .iter()
            .find(|(p, _)| p == &pair)
            .map(|(_, l)| l.clone());
        let Some(helper_label) = helper else { continue };
        used.insert(helper_label.clone());
        // Replace the first JSR's operand with the helper.
        if let Item::Insn(insn) = &mut items[*start] {
            insn.operand = Some(helper_label);
        }
        // Drop the second JSR.
        items.remove(*second);
    }
    // Do not emit a helper when all its sites are taken.
    if used.is_empty() {
        return false;
    }

    // Emit helpers BEFORE __HEAP_BOTTOM.
    let mut helpers: Vec<Item> = Vec::new();
    if !helper_section_present(items) {
        helpers.push(Item::Blank);
        helpers.push(Item::Comment(
            "; --- peephole-factored helpers ---".to_string(),
        ));
    }
    for ((a, b), label) in &emitted {
        if !used.contains(label) {
            continue;
        }
        helpers.push(Item::Label(label.clone()));
        helpers.push(Item::Insn(Insn {
            mnem: "JSR".to_string(),
            operand: Some(a.clone()),
            comment: None,
        }));
        helpers.push(Item::Insn(Insn {
            mnem: "JMP".to_string(),
            operand: Some(b.clone()),
            comment: None,
        }));
    }
    append_helpers_before_heap(items, helpers);
    true
}

/// Share repeated variable pointer loads followed by calls.
fn ph112_factor_varptr_jsr(items: &mut Vec<Item>) -> bool {
    use std::collections::HashMap;
    let mut sites: HashMap<(String, String), Vec<usize>> = HashMap::new();
    let mut i = 0;
    while i + 2 < items.len() {
        if let Item::Insn(a) = &items[i]
            && a.mnem == "LDA"
            && let Some(a_op) = a.operand.as_deref()
            && !a_op.starts_with('#')
            && let Some(j) = next_struct(items, i)
            && let Item::Insn(b) = &items[j]
            && b.mnem == "LDY"
            && let Some(b_op) = b.operand.as_deref()
            // The LDY operand must equal LDA's + "+1".
            && b_op == &format!("{a_op}+1")
            && let Some(k) = next_struct(items, j)
            && let Item::Insn(c) = &items[k]
            && c.mnem == "JSR"
            && let Some(c_op) = c.operand.as_deref()
            && !c_op.starts_with("__VP")
        // don't recurse on our helpers
        {
            sites
                .entry((a_op.to_string(), c_op.to_string()))
                .or_default()
                .push(i);
            i = k + 1;
            continue;
        }
        i += 1;
    }
    let mut to_factor: Vec<((String, String), Vec<usize>)> = sites
        .into_iter()
        .filter(|(_, sites)| sites.len() >= 3)
        .collect();
    if to_factor.is_empty() {
        return false;
    }
    to_factor.sort_by(|a, b| a.0.cmp(&b.0));

    let mut helper_idx = 0u32;

    let mut emitted: Vec<((String, String), String)> = Vec::new();
    let mut all_removals: Vec<(usize, usize, usize)> = Vec::new();
    for (key, site_list) in &to_factor {
        let helper = allocate_helper_name(items, "__VP", &mut helper_idx);
        emitted.push((key.clone(), helper.clone()));
        for &start in site_list {
            let mid = next_struct(items, start).unwrap();
            let end = next_struct(items, mid).unwrap();
            all_removals.push((start, mid, end));
        }
    }
    all_removals.sort_by(|a, b| b.0.cmp(&a.0));
    for (start, mid, end) in &all_removals {
        let (Item::Insn(a), _, Item::Insn(c)) = (&items[*start], &items[*mid], &items[*end]) else {
            continue;
        };
        let key = (
            a.operand.clone().unwrap_or_default(),
            c.operand.clone().unwrap_or_default(),
        );
        let helper_label = emitted
            .iter()
            .find(|(k, _)| k == &key)
            .map(|(_, l)| l.clone());
        let Some(helper_label) = helper_label else {
            continue;
        };
        if let Item::Insn(insn) = &mut items[*start] {
            insn.mnem = "JSR".to_string();
            insn.operand = Some(helper_label);
            insn.comment = None;
        }
        items.remove(*end);
        items.remove(*mid);
    }

    let mut helpers: Vec<Item> = Vec::new();
    if !helper_section_present(items) {
        helpers.push(Item::Blank);
        helpers.push(Item::Comment(
            "; --- peephole-factored helpers ---".to_string(),
        ));
    }
    for ((var_op, target), label) in &emitted {
        helpers.push(Item::Label(label.clone()));
        helpers.push(Item::Insn(Insn {
            mnem: "LDA".to_string(),
            operand: Some(var_op.clone()),
            comment: None,
        }));
        helpers.push(Item::Insn(Insn {
            mnem: "LDY".to_string(),
            operand: Some(format!("{var_op}+1")),
            comment: None,
        }));
        helpers.push(Item::Insn(Insn {
            mnem: "JMP".to_string(),
            operand: Some(target.clone()),
            comment: None,
        }));
    }
    append_helpers_before_heap(items, helpers);
    true
}

/// Share repeated immediate pointer loads followed by calls.
fn ph111_factor_lda_ldy_jsr(items: &mut Vec<Item>) -> bool {
    use std::collections::HashMap;
    // Group sites by load instructions, operands and call target.
    let mut sites: HashMap<(String, String, String, String), Vec<usize>> = HashMap::new();
    let mut i = 0;
    while i + 2 < items.len() {
        if let Item::Insn(a) = &items[i]
            && (a.mnem == "LDA" || a.mnem == "LDX")
            && let Some(a_op) = a.operand.as_deref()
            && (a_op.starts_with("#<") || a_op.starts_with("#$"))
            && let Some(j) = next_struct(items, i)
            && let Item::Insn(b) = &items[j]
            && b.mnem == "LDY"
            && let Some(b_op) = b.operand.as_deref()
            && (b_op.starts_with("#>") || b_op.starts_with("#$"))
            && let Some(k) = next_struct(items, j)
            && let Item::Insn(c) = &items[k]
            && c.mnem == "JSR"
            && let Some(c_op) = c.operand.as_deref()
            // Skip generated helpers to avoid repeated wrapping.
            && !c_op.starts_with("__T")
        {
            sites
                .entry((
                    a.mnem.clone(),
                    a_op.to_string(),
                    b_op.to_string(),
                    c_op.to_string(),
                ))
                .or_default()
                .push(i);
            i = k + 1;
            continue;
        }
        i += 1;
    }
    let mut to_factor: Vec<((String, String, String, String), Vec<usize>)> = sites
        .into_iter()
        .filter(|(_, sites)| sites.len() >= 3)
        .collect();
    if to_factor.is_empty() {
        return false;
    }
    to_factor.sort_by(|a, b| a.0.cmp(&b.0));

    // Allocate helper IDs.
    let mut helper_idx = 0u32;

    // Stage rewrites and helpers.
    let mut emitted: Vec<((String, String, String, String), String)> = Vec::new();
    let mut all_removals: Vec<(usize, usize, usize)> = Vec::new(); // (start, mid, end)
    for (key, site_list) in &to_factor {
        let helper = allocate_helper_name(items, "__T", &mut helper_idx);
        emitted.push((key.clone(), helper.clone()));
        for &start in site_list {
            let mid = next_struct(items, start).expect("paired LDY");
            let end = next_struct(items, mid).expect("paired JSR");
            all_removals.push((start, mid, end));
        }
    }
    // Reverse order so removals don't disturb earlier indices.
    all_removals.sort_by(|a, b| b.0.cmp(&a.0));
    for (start, mid, end) in &all_removals {
        let (Item::Insn(a), Item::Insn(b), Item::Insn(c)) =
            (&items[*start], &items[*mid], &items[*end])
        else {
            continue;
        };
        let key = (
            a.mnem.clone(),
            a.operand.clone().unwrap_or_default(),
            b.operand.clone().unwrap_or_default(),
            c.operand.clone().unwrap_or_default(),
        );
        let helper_label = emitted
            .iter()
            .find(|(k, _)| k == &key)
            .map(|(_, l)| l.clone());
        let Some(helper_label) = helper_label else {
            continue;
        };
        if let Item::Insn(insn) = &mut items[*start] {
            insn.mnem = "JSR".to_string();
            insn.operand = Some(helper_label);
            insn.comment = None;
        }
        items.remove(*end);
        items.remove(*mid);
    }

    // Emit per-triple helpers.
    let mut helpers: Vec<Item> = Vec::new();
    if !helper_section_present(items) {
        helpers.push(Item::Blank);
        helpers.push(Item::Comment(
            "; --- peephole-factored helpers ---".to_string(),
        ));
    }
    for ((mnem_a, a_op, b_op, c_op), label) in &emitted {
        helpers.push(Item::Label(label.clone()));
        helpers.push(Item::Insn(Insn {
            mnem: mnem_a.clone(),
            operand: Some(a_op.clone()),
            comment: None,
        }));
        helpers.push(Item::Insn(Insn {
            mnem: "LDY".to_string(),
            operand: Some(b_op.clone()),
            comment: None,
        }));
        helpers.push(Item::Insn(Insn {
            mnem: "JMP".to_string(),
            operand: Some(c_op.clone()),
            comment: None,
        }));
    }
    append_helpers_before_heap(items, helpers);
    true
}

/// Call count needed to cover the inline-string trampoline's size.
const STR_PRINT_INLINE_MIN_SITES: usize = 17;

/// Read the string-print pointer pair from the helper's prologue.
fn str_print_zp_pair(items: &[Item]) -> Option<(String, String)> {
    let at = items
        .iter()
        .position(|it| matches!(it, Item::Label(n) if n == "__STR_PRINT"))?;
    let a = next_struct(items, at)?;
    let Item::Insn(sta) = &items[a] else {
        return None;
    };
    let b = next_struct(items, a)?;
    let Item::Insn(sty) = &items[b] else {
        return None;
    };
    if sta.mnem != "STA" || sty.mnem != "STY" {
        return None;
    }
    let lo = sta.operand.clone()?;
    let hi = sty.operand.clone()?;
    let zp = |s: &str| {
        s.len() == 3 && s.starts_with('$') && s[1..].bytes().all(|b| b.is_ascii_hexdigit())
    };
    (zp(&lo) && zp(&hi)).then_some((lo, hi))
}

/// The inline-argument trampoline. 34 bytes, emitted once.
fn append_str_print_inline_helper(items: &mut Vec<Item>, lo: String, hi: String) {
    items.push(Item::Label("__STR_PRINT_I".to_string()));

    push_insn(items, "PLA", None);
    push_insn(items, "STA", Some(lo.clone()));
    push_insn(items, "PLA", None);
    push_insn(items, "STA", Some(hi.clone()));
    // Push it back two bytes further on, so the callee's RTS skips the inline `.word`.
    push_insn(items, "CLC", None);
    push_insn(items, "LDA", Some(lo.clone()));
    push_insn(items, "ADC", Some("#$02".to_string()));
    push_insn(items, "TAX", None);
    push_insn(items, "LDA", Some(hi.clone()));
    push_insn(items, "ADC", Some("#$00".to_string()));
    push_insn(items, "PHA", None);
    push_insn(items, "TXA", None);
    push_insn(items, "PHA", None);
    // Read the argument, then reuse the pair to hold it.
    push_insn(items, "LDY", Some("#$02".to_string()));
    push_insn(items, "LDA", Some(format!("({lo}),Y")));
    push_insn(items, "TAX", None);
    push_insn(items, "DEY", None);
    push_insn(items, "LDA", Some(format!("({lo}),Y")));
    push_insn(items, "STA", Some(lo));
    push_insn(items, "STX", Some(hi));
    push_insn(items, "JMP", Some("__STR_PRINT_PTR".to_string()));
}

/// Bytes of one `LDA #< / LDY #> / JSR` print call, or of the run call that replaces a run.
const PH327_CALL: isize = 7;
/// Shortest run worth looking at.
const PH327_MIN_RUN: usize = 8;
/// `__PRINT_RUN`'s own bytes.
const PH327_HELPER: isize = 7;

/// Read plain string pool bytes after the length byte.
fn ph327_pool_record(items: &[Item], at: usize) -> Option<(Vec<u8>, usize)> {
    let Item::Label(name) = &items[at] else { return None };
    if !name.strip_prefix('S').is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())) {
        return None;
    }
    let mut bytes = Vec::new();
    let mut end = at + 1;
    while let Some(Item::Directive(raw)) = items.get(end) {
        let body = raw.trim().strip_prefix(".byte ")?;
        for v in body.split(',') {
            bytes.push(u8::from_str_radix(v.trim().strip_prefix('$')?, 16).ok()?);
        }
        end += 1;
    }
    let (&len, payload) = bytes.split_first()?;
    (usize::from(len) == payload.len()).then(|| (payload.to_vec(), end))
}

/// Print repeated characters with a loop when it saves space.
fn ph327_split_print_runs(items: &mut Vec<Item>) -> bool {
    use std::collections::HashMap;
    let mut records: HashMap<String, (usize, usize, Vec<u8>)> = HashMap::new();
    for at in 0..items.len() {
        if let Some((payload, end)) = ph327_pool_record(items, at)
            && let Item::Label(name) = &items[at]
        {
            records.insert(name.clone(), (at, end, payload));
        }
    }
    if records.is_empty() || !item_label_present(items, "__STR_PRINT") {
        return false;
    }
    // Every mention of a pool label, and the print calls among them.
    let mut mentions: HashMap<&str, usize> = HashMap::new();
    for it in items.iter() {
        let text = match it {
            Item::Insn(insn) => insn.operand.as_deref().unwrap_or(""),
            Item::Directive(raw) => raw.as_str(),
            _ => continue,
        };
        for tok in text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
            if let Some((name, _)) = records.get_key_value(tok) {
                *mentions.entry(name.as_str()).or_default() += 1;
            }
        }
    }
    let mut sites: HashMap<String, Vec<(usize, usize, usize)>> = HashMap::new();
    for i in 0..items.len() {
        let Item::Insn(lda) = &items[i] else { continue };
        let Some(name) = lda.operand.as_deref().and_then(|op| op.strip_prefix("#<")) else { continue };
        if lda.mnem != "LDA" || !records.contains_key(name) {
            continue;
        }
        let Some(j) = next_struct(items, i) else { continue };
        let Some(k) = next_struct(items, j) else { continue };
        let ok = matches!(&items[j], Item::Insn(ldy) if ldy.mnem == "LDY"
                && ldy.operand.as_deref() == Some(format!("#>{name}").as_str()))
            && matches!(&items[k], Item::Insn(call) if matches!(call.mnem.as_str(), "JSR" | "JMP")
                && call.operand.as_deref() == Some("__STR_PRINT"));
        if ok {
            sites.entry(name.to_string()).or_default().push((i, j, k));
        }
    }
    let smc = smc_patched_labels(items);
    let mut plans: Vec<(String, Vec<Result<Vec<u8>, (u8, usize)>>)> = Vec::new();
    let mut saved: isize = 0;
    for (name, at_sites) in &sites {
        if mentions.get(name.as_str()) != Some(&(2 * at_sites.len())) || smc.contains(name) {
            continue;
        }
        let payload = &records[name].2;
        let m = at_sites.len() as isize;
        // Maximal runs, longest first, kept while each lowers the total.
        let mut runs: Vec<(usize, usize)> = Vec::new();
        let mut i = 0;
        while i < payload.len() {
            let mut j = i;
            while j < payload.len() && payload[j] == payload[i] {
                j += 1;
            }
            if j - i >= PH327_MIN_RUN {
                runs.push((i, j));
            }
            i = j;
        }
        runs.sort_by_key(|&(a, b)| std::cmp::Reverse(b - a));
        let cost = |chosen: &[(usize, usize)]| -> isize {
            let mut sorted = chosen.to_vec();
            sorted.sort();
            let (mut parts, mut texts, mut run_bytes, mut at) = (0isize, 0isize, 0isize, 0usize);
            for &(a, b) in &sorted {
                if a > at {
                    parts += 1;
                    texts += 1;
                }
                parts += 1;
                run_bytes += (b - a) as isize;
                at = b;
            }
            if at < payload.len() {
                parts += 1;
                texts += 1;
            }
            PH327_CALL * m * (parts - 1) + texts - 1 - run_bytes
        };
        let mut chosen: Vec<(usize, usize)> = Vec::new();
        for run in runs {
            let mut with = chosen.clone();
            with.push(run);
            if cost(&with) < cost(&chosen) {
                chosen = with;
            }
        }
        if chosen.is_empty() || cost(&chosen) >= 0 {
            continue;
        }
        saved -= cost(&chosen);
        chosen.sort();
        let mut parts = Vec::new();
        let mut at = 0;
        for (a, b) in chosen {
            if a > at {
                parts.push(Ok(payload[at..a].to_vec()));
            }
            parts.push(Err((payload[a], b - a)));
            at = b;
        }
        if at < payload.len() {
            parts.push(Ok(payload[at..].to_vec()));
        }
        plans.push((name.clone(), parts));
    }
    let pool_start = items
        .iter()
        .position(|it| matches!(it, Item::Comment(c) if c.trim() == "; --- string pool (length-prefixed) ---"));
    let pool_end = pool_start.map(|start| {
        items[start + 1..]
            .iter()
            .position(|it| matches!(it, Item::Comment(c) if c.trim_start().starts_with("; ---")))
            .map_or(items.len(), |off| start + 1 + off)
    });
    let Some(last_record_end) = pool_start.zip(pool_end).and_then(|(start, end)| {
        records.values().filter(|r| r.0 > start && r.1 <= end).map(|r| r.1).max()
    }) else {
        return false;
    };
    // The loop itself has to pay too.
    if plans.is_empty() || (!item_label_present(items, "__PRINT_RUN") && saved <= PH327_HELPER) {
        return false;
    }
    plans.sort();
    let mut by_bytes: HashMap<Vec<u8>, String> = records
        .iter()
        .filter(|(name, _)| !plans.iter().any(|(p, _)| p == *name))
        .map(|(name, (_, _, b))| (b.clone(), name.clone()))
        .collect();
    let mut next = items
        .iter()
        .filter_map(|it| match it {
            Item::Label(n) => n.strip_prefix('S')?.parse::<usize>().ok(),
            _ => None,
        })
        .max()
        .map_or(0, |n| n + 1);
    let mut new_records: Vec<(String, Vec<u8>)> = Vec::new();
    let mut replace: HashMap<usize, Vec<Item>> = HashMap::new();
    let mut drop: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for (name, parts) in &plans {
        let mut labels: Vec<Result<String, (u8, usize)>> = Vec::new();
        for part in parts {
            labels.push(match part {
                Ok(text) => Ok(by_bytes
                    .entry(text.clone())
                    .or_insert_with(|| {
                        let label = format!("S{next}");
                        next += 1;
                        new_records.push((label.clone(), text.clone()));
                        label
                    })
                    .clone()),
                Err(run) => Err(*run),
            });
        }
        for &(i, j, k) in &sites[name] {
            let Item::Insn(call) = &items[k] else { unreachable!() };
            let tail = call.mnem.clone();
            let mut seq = Vec::new();
            for (n, part) in labels.iter().enumerate() {
                let mnem = if n + 1 == labels.len() { tail.as_str() } else { "JSR" };
                match part {
                    Ok(label) => {
                        push_insn(&mut seq, "LDA", Some(format!("#<{label}")));
                        push_insn(&mut seq, "LDY", Some(format!("#>{label}")));
                        push_insn(&mut seq, mnem, Some("__STR_PRINT".to_string()));
                    }
                    Err((byte, len)) => {
                        push_insn(&mut seq, "LDA", Some(format!("#${byte:02X}")));
                        push_insn(&mut seq, "LDX", Some(format!("#${len:02X}")));
                        push_insn(&mut seq, mnem, Some("__PRINT_RUN".to_string()));
                    }
                }
            }
            replace.insert(i, seq);
            drop.insert(j);
            drop.insert(k);
        }
        let (at, end, _) = records[name];
        drop.extend(at..end);
    }
    let mut out = Vec::with_capacity(items.len() + 8 * new_records.len());
    for (idx, it) in items.drain(..).enumerate() {
        if let Some(seq) = replace.remove(&idx) {
            out.extend(seq);
        } else if !drop.contains(&idx) {
            out.push(it);
        }
        if idx + 1 == last_record_end {
            for (label, text) in &new_records {
                out.push(Item::Label(label.clone()));
                let mut prefixed = vec![text.len() as u8];
                prefixed.extend_from_slice(text);
                let list: Vec<String> = prefixed.iter().map(|b| format!("${b:02X}")).collect();
                out.push(Item::Directive(format!("    .byte {}", list.join(","))));
            }
        }
    }
    *items = out;
    if !item_label_present(items, "__PRINT_RUN") {
        let mut helper = vec![Item::Label("__PRINT_RUN".to_string())];
        push_insn(&mut helper, "JSR", Some("$FFD2".to_string()));
        push_insn(&mut helper, "DEX", None);
        push_insn(&mut helper, "BNE", Some("__PRINT_RUN".to_string()));
        push_insn(&mut helper, "RTS", None);
        append_helpers_before_heap(items, helper);
    }
    true
}

/// Store literal PRINT arguments after the call to reduce call-site size.
fn ph114_str_print_inline_args(items: &mut Vec<Item>) -> bool {
    if !item_label_present(items, "__STR_PRINT_PTR") {
        return false;
    }
    let Some((zp_lo, zp_hi)) = str_print_zp_pair(items) else {
        return false;
    };
    // (lda index, ldy index, jsr index, pool label)
    let mut sites: Vec<(usize, usize, usize, String)> = Vec::new();
    let mut i = 0;
    while i + 2 < items.len() {
        if let Item::Insn(a) = &items[i]
            && a.mnem == "LDA"
            && let Some(a_op) = a.operand.as_deref()
            && let Some(label) = a_op.strip_prefix("#<")
            && is_plain_label(label)
            && let Some(j) = next_struct(items, i)
            && let Item::Insn(b) = &items[j]
            && b.mnem == "LDY"
            && b.operand.as_deref() == Some(format!("#>{label}").as_str())
            && let Some(k) = next_struct(items, j)
            && let Item::Insn(c) = &items[k]
            && c.mnem == "JSR"
            && c.operand.as_deref() == Some("__STR_PRINT")
        {
            sites.push((i, j, k, label.to_string()));
            i = k + 1;
            continue;
        }
        i += 1;
    }
    if sites.len() < STR_PRINT_INLINE_MIN_SITES {
        return false;
    }
    for (start, mid, end, label) in sites.iter().rev() {
        items.remove(*end);
        items.remove(*mid);
        if let Item::Insn(insn) = &mut items[*start] {
            insn.mnem = "JSR".to_string();
            insn.operand = Some("__STR_PRINT_I".to_string());
            insn.comment = None;
        }
        items.insert(*start + 1, Item::Directive(format!("    .word {label}")));
    }
    let mut helpers: Vec<Item> = Vec::new();
    if !helper_section_present(items) {
        helpers.push(Item::Blank);
        helpers.push(Item::Comment(
            "; --- peephole-factored helpers ---".to_string(),
        ));
    }
    append_str_print_inline_helper(&mut helpers, zp_lo, zp_hi);
    append_helpers_before_heap(items, helpers);
    true
}

/// Whether an operand is a bare label without arithmetic or indirection.
fn is_plain_label(s: &str) -> bool {
    !s.is_empty()
        && !s.as_bytes()[0].is_ascii_digit()
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The label the inline-argument trampoline is reached by.
pub const STR_PRINT_INLINE_ENTRY: &str = "__STR_PRINT_I";

/// Find shared prefix entries leading to inline PRINT.
fn str_print_inline_entries(items: &[Item]) -> (std::collections::HashMap<String, Vec<Insn>>, Option<usize>) {
    let mut entries = std::collections::HashMap::new();
    let Some(tramp) = items
        .iter()
        .position(|it| matches!(it, Item::Label(name) if name == STR_PRINT_INLINE_ENTRY))
    else {
        return (entries, None);
    };
    // The trampoline's own label run, then the entries' code in front of it.
    let mut head = tramp;
    while let Some(p) = prev_struct(items, head) {
        if !matches!(&items[p], Item::Label(n) if !n.starts_with("__PH326_")) {
            break;
        }
        head = p;
    }
    let mut start = head;
    while let Some(p) = prev_struct(items, start) {
        match &items[p] {
            Item::Label(n) if n.starts_with("__PH326_") => start = p,
            Item::Insn(insn) if !is_uncond_terminator(&insn.mnem) => start = p,
            _ => break,
        }
    }
    while start < head && !matches!(&items[start], Item::Label(n) if n.starts_with("__PH326_")) {
        start += 1;
    }
    if start == head {
        return (entries, None);
    }
    for k in start..head {
        if let Item::Label(name) = &items[k] {
            let run = items[k + 1..head]
                .iter()
                .filter_map(|it| match it {
                    Item::Insn(insn) => Some(insn.clone()),
                    _ => None,
                })
                .collect();
            entries.insert(name.clone(), run);
        }
    }
    (entries, Some(start))
}

/// Names of shared prefix entries that accept inline PRINT arguments.
pub fn str_print_inline_entry_names(asm: &str) -> std::collections::HashSet<String> {
    if !asm.contains("__PH326_") || !asm.contains(STR_PRINT_INLINE_ENTRY) {
        return std::collections::HashSet::new();
    }
    str_print_inline_entries(&parse(asm)).0.into_keys().collect()
}

/// Restore register-based PRINT arguments before reserved-range relocation.
pub fn undo_str_print_inline_args(asm: &str) -> String {
    undo_str_print_inline_args_at(asm, None)
}

/// Restore register arguments at selected inline PRINT calls.
pub fn undo_str_print_inline_args_at(
    asm: &str,
    sites: Option<&std::collections::HashSet<usize>>,
) -> String {
    if !asm.contains(STR_PRINT_INLINE_ENTRY) {
        return asm.to_string();
    }
    let items = parse(asm);
    let (entries, entries_start) = str_print_inline_entries(&items);
    let is_site = |it: &Item| {
        matches!(it, Item::Insn(insn)
            if insn.mnem == "JSR" && insn.operand.as_deref().is_some_and(
                |op| op == STR_PRINT_INLINE_ENTRY || entries.contains_key(op)))
    };
    let total = items.iter().filter(|it| is_site(it)).count();
    let undo_all = sites.is_none_or(|s| (0..total).all(|k| s.contains(&k)));

    let entry = items.iter().position(
        |it| matches!(it, Item::Label(name) if name == STR_PRINT_INLINE_ENTRY),
    );
    let tramp_end = entry.and_then(|at| {
        items[at + 1..]
            .iter()
            .position(|it| {
                matches!(it, Item::Insn(insn)
                    if insn.mnem == "JMP"
                        && insn.operand.as_deref() == Some("__STR_PRINT_PTR"))
            })
            .map(|off| at + 1 + off + 1)
    });
    if entry.is_some() && tramp_end.is_none() {
        return asm.to_string();
    }
    let mut out: Vec<Item> = Vec::with_capacity(items.len() + 64);
    let mut ordinal = 0usize;
    let mut i = 0usize;
    while i < items.len() {
        if undo_all && (Some(i) == entry || Some(i) == entries_start) {
            i = tramp_end.expect("checked above");
            continue;
        }
        let chosen = is_site(&items[i]) && {
            ordinal += 1;
            undo_all || sites.is_some_and(|s| s.contains(&(ordinal - 1)))
        };
        if chosen
            && let Some(k) = next_struct(&items, i)
            && let Item::Directive(d) = &items[k]
            && let Some(label) = d.trim().strip_prefix(".word ")
            && is_plain_label(label.trim())
        {
            let label = label.trim();
            if let Item::Insn(call) = &items[i]
                && let Some(run) = call.operand.as_deref().and_then(|op| entries.get(op))
            {
                out.extend(run.iter().cloned().map(Item::Insn));
            }
            push_insn(&mut out, "LDA", Some(format!("#<{label}")));
            push_insn(&mut out, "LDY", Some(format!("#>{label}")));
            push_insn(&mut out, "JSR", Some("__STR_PRINT".to_string()));
            // Remove blanks and comments within the pair.
            i = k + 1;
            continue;
        }
        out.push(items[i].clone());
        i += 1;
    }
    render(&out)
}

/// Share repeated instruction sequences that preserve stack and control flow.
#[allow(dead_code)]
fn ph301_factor_repeated_sequences(items: &mut Vec<Item>) -> bool {
    // Factor larger windows first.
    let mut any = false;
    any |= ph301_with_window(items, 7, JsrPolicy::Refused);
    any |= ph301_with_window(items, 6, JsrPolicy::Refused);
    any |= ph301_with_window(items, 5, JsrPolicy::Refused);
    any |= ph301_with_window(items, 4, JsrPolicy::Refused);
    any |= ph301_with_window(items, 3, JsrPolicy::Refused);
    any
}

/// Share repeated instruction windows after call factoring.
fn ph301_factor_late_with_jsr(
    items: &mut Vec<Item>,
    profile: Profile,
    aggressive_size_dedup: bool,
) -> bool {
    let mut any = false;
    let rescue = profile == Profile::Size && aggressive_size_dedup;
    let sites = if rescue {
        2
    } else {
        crate::opt_model::CostModel::default().ph301_min_sites()
    };
    let policy = if rescue {
        JsrPolicy::AllowThree
    } else {
        JsrPolicy::AllowOne
    };
    if rescue {
        for window in (8..=16).rev() {
            any |= ph301_with_window_min_sites(items, window, policy, sites);
        }
    }
    any |= ph301_with_window_min_sites(items, 7, policy, sites);
    any |= ph301_with_window_min_sites(items, 6, policy, sites);
    any |= ph301_with_window_min_sites(items, 5, policy, sites);
    any |= ph301_with_window_min_sites(items, 4, policy, sites);
    if ph301_window_3_is_size_only(profile) {
        any |= ph301_with_window_min_sites(items, 3, policy, sites);
    }
    any
}

/// Allow three-instruction factoring only in the size profile.
fn ph301_window_3_is_size_only(profile: Profile) -> bool {
    profile == Profile::Size
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JsrPolicy {
    /// Refuse windows containing calls.
    Refused,
    /// Allow declared integer and string storage during compact instruction factoring.
    CompactRefused,
    /// Allow one call to an inspected leaf that ends in RTS.
    CompactLeafCalls,
    /// Allow one call per window.
    AllowOne,
    /// Size-only mixed instruction windows may contain three calls.
    AllowThree,
}

/// Select non-overlapping instruction windows that have not been consumed.
fn ph301_disjoint_sites(
    items: &[Item],
    sites: &[usize],
    window_size: usize,
    consumed: Option<&std::collections::HashSet<usize>>,
) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut claimed: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for &start in sites {
        let mut last = start;
        let mut ok = true;
        for _ in 1..window_size {
            match next_struct(items, last) {
                Some(p) => last = p,
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            continue;
        }
        let clash = (start..=last).any(|x| {
            claimed.contains(&x) || consumed.is_some_and(|c| c.contains(&x))
        });
        if clash {
            continue;
        }
        for x in start..=last {
            claimed.insert(x);
        }
        out.push((start, last));
    }
    out
}

fn ph301_with_window(items: &mut Vec<Item>, window_size: usize, jsr: JsrPolicy) -> bool {
    let threshold = crate::opt_model::CostModel::default().ph301_min_sites();
    ph301_with_window_min_sites(items, window_size, jsr, threshold)
}

fn ph301_with_window_min_sites(
    items: &mut Vec<Item>,
    window_size: usize,
    jsr: JsrPolicy,
    threshold: usize,
) -> bool {
    use std::collections::HashMap;
    let jsr_budget = crate::opt_model::CostModel::default().ph301_late_jsr_budget();
    let compact = matches!(jsr, JsrPolicy::CompactRefused | JsrPolicy::CompactLeafCalls);
    let compact_sites = if jsr == JsrPolicy::CompactLeafCalls {
        Some(structured::compact_leaf_call_sites(items))
    } else {
        compact.then(|| structured::compact_instruction_sites(items))
    };

    let smc_targets = smc_patched_labels(items);

    let sensitive = frame_sensitive_labels(items);
    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
    let mut i = 0;
    while i < items.len() {
        // Try to gather window_size structural items starting at i.
        let mut idxs: Vec<usize> = Vec::with_capacity(window_size);
        let mut probe = i;
        let mut ok = true;
        for step in 0..window_size {
            let cur = if step == 0 {
                if matches!(items.get(i), Some(Item::Insn(_))) {
                    i
                } else {
                    i = i + 1;
                    ok = false;
                    break;
                }
            } else {
                match next_struct(items, probe) {
                    Some(p) => p,
                    None => {
                        ok = false;
                        break;
                    }
                }
            };

            if compact && step > 0 && crosses_sys_wrap(items, probe, cur) {
                ok = false;
                break;
            }
            // Disqualify on label, directive, or unsafe insn.
            match items.get(cur) {
                Some(Item::Insn(insn)) => {
                    if !is_factorable_insn_with_policy(insn, jsr)
                        || compact_sites.as_ref().is_some_and(|sites| !sites[cur])
                        || insn_is_smc_target(items, cur, &smc_targets)
                        || insn_adds_frame_to_sensitive(insn, &sensitive)
                    {
                        ok = false;
                        break;
                    }
                }
                Some(Item::Label(_)) | Some(Item::Directive(_)) | None => {
                    ok = false;
                    break;
                }
                _ => {
                    ok = false;
                    break;
                }
            }
            idxs.push(cur);
            probe = cur;
        }
        if !ok || idxs.len() != window_size {
            i += 1;
            continue;
        }
        // PH110/PH113 already handled adjacent calls.
        if !matches!(jsr, JsrPolicy::Refused | JsrPolicy::CompactRefused) {
            let jsr_count = idxs
                .iter()
                .filter(
                    |&&idx| matches!(items.get(idx), Some(Item::Insn(insn)) if insn.mnem == "JSR"),
                )
                .count();
            if jsr == JsrPolicy::CompactLeafCalls && jsr_count != 1 {
                i += 1;
                continue;
            }
            let budget = if jsr == JsrPolicy::AllowThree {
                3
            } else {
                jsr_budget
            };
            if jsr_count > budget {
                i += 1;
                continue;
            }
        }
        // A shared helper must leave its return address on top of the stack.
        if !window_stack_balanced(items, &idxs) {
            i += 1;
            continue;
        }
        let sig = window_signature(items, &idxs);
        groups.entry(sig).or_default().push(i);
        // Hash EVERY start alignment, not one per window.
        i += 1;
    }

    // Rank by REALIZABLE savings.
    let zp = ZpEquates::scan(items);
    let mut to_factor: Vec<(String, Vec<usize>, usize)> = Vec::new();
    for (sig, sites) in groups {
        let bytes_per_window = window_byte_estimate(items, sites[0], window_size, &zp);
        let savings_per_site = bytes_per_window.saturating_sub(3);
        if savings_per_site == 0 {
            continue;
        }
        let helper_overhead = bytes_per_window + 1; // sequence + RTS
        let disjoint = ph301_disjoint_sites(items, &sites, window_size, None);
        if disjoint.len() < threshold {
            continue;
        }
        let gross = savings_per_site * disjoint.len();
        if gross <= helper_overhead {
            continue;
        }
        to_factor.push((sig, sites, gross - helper_overhead));
    }
    if to_factor.is_empty() {
        return false;
    }
    to_factor.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));

    let next_seq_id = peephole_next_seq_id(items);
    let mut helper_idx = next_seq_id;
    let mut emitted: Vec<(String, String)> = Vec::new(); // (sig, helper-label)
    let mut chosen: Vec<(usize, usize, String)> = Vec::new();
    let mut consumed: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for (sig, sites, _) in &to_factor {
        let bytes_per_window = window_byte_estimate(items, sites[0], window_size, &zp);
        let helper_overhead = bytes_per_window + 1; // sequence + RTS
        let savings_per_site = bytes_per_window.saturating_sub(3); // JSR is 3
        let my_sites = ph301_disjoint_sites(items, sites, window_size, Some(&consumed));
        if my_sites.len() < threshold {
            continue;
        }
        let total_savings = savings_per_site * my_sites.len();
        if total_savings <= helper_overhead {
            continue;
        }
        let helper_label = format!("__SEQ_{}", helper_idx);
        helper_idx += 1;
        emitted.push((sig.clone(), helper_label.clone()));
        for (start, last) in &my_sites {
            for x in *start..=*last {
                consumed.insert(x);
            }
            chosen.push((*start, *last, sig.clone()));
        }
    }
    if emitted.is_empty() {
        return false;
    }

    chosen.sort_by(|a, b| b.0.cmp(&a.0));
    for (start, last, sig) in &chosen {
        let helper_label = emitted
            .iter()
            .find(|(s, _)| s == sig)
            .map(|(_, l)| l.clone());
        let Some(helper_label) = helper_label else {
            continue;
        };

        // Replace the first instruction in the window with the JSR.
        if let Item::Insn(insn) = &mut items[*start] {
            insn.mnem = "JSR".to_string();
            insn.operand = Some(helper_label);
            insn.comment = None;
        }
        let mut to_drop: Vec<usize> = Vec::new();
        let mut cur = *start;
        let mut ok = true;
        for _ in 1..window_size {
            match next_struct(items, cur) {
                Some(p) => {
                    cur = p;
                    to_drop.push(cur);
                }
                None => {
                    ok = false;
                    break;
                }
            }
        }
        let _ = last;
        if ok {
            to_drop.sort_unstable_by(|a, b| b.cmp(a));
            for idx in to_drop {
                items.remove(idx);
            }
        }
    }

    // Place helpers before the runtime heap.
    let mut helpers: Vec<Item> = Vec::new();
    if !helper_section_present(items) {
        helpers.push(Item::Blank);
        helpers.push(Item::Comment(
            "; --- peephole-factored helpers ---".to_string(),
        ));
    }
    for (sig, label) in &emitted {
        helpers.push(Item::Label(label.clone()));
        for line in sig.split('\n') {
            if line.is_empty() {
                continue;
            }
            let (mnem, operand) = match line.find(' ') {
                Some(idx) => (line[..idx].to_string(), Some(line[idx + 1..].to_string())),
                None => (line.to_string(), None),
            };
            helpers.push(Item::Insn(Insn {
                mnem,
                operand,
                comment: None,
            }));
        }
        helpers.push(Item::Insn(Insn {
            mnem: "RTS".to_string(),
            operand: None,
            comment: None,
        }));
    }
    append_helpers_before_heap(items, helpers);
    true
}

/// Whether an instruction is safe to move into a shared helper.
#[allow(dead_code)]
fn is_factorable_insn(insn: &Insn) -> bool {
    is_factorable_insn_with_policy(insn, JsrPolicy::Refused)
}

/// Require balanced stack use without reading the helper return address.
fn window_stack_balanced(items: &[Item], idxs: &[usize]) -> bool {
    let mut delta: i32 = 0;
    for &idx in idxs {
        if let Some(Item::Insn(insn)) = items.get(idx) {
            match insn.mnem.as_str() {
                "PHA" | "PHP" => delta += 1,
                "PLA" | "PLP" => {
                    delta -= 1;
                    if delta < 0 {
                        return false;
                    }
                }
                _ => {}
            }
        }
    }
    delta == 0
}

/// Labels that require exactly one call frame above the SYS prologue.
fn frame_sensitive_labels(items: &[Item]) -> std::collections::HashSet<String> {
    let mut set = std::collections::HashSet::new();
    set.insert("__SYS_TXT_OUT".to_string());

    set.insert("__SYS_EVAL_CHAIN".to_string());
    set.insert("__SYS_EVAL_RETURN".to_string());
    loop {
        let mut grew = false;
        let mut cur: Vec<&str> = Vec::new();
        let mut saw_insn = false;
        for it in items {
            match it {
                Item::Label(name) => {
                    if saw_insn {
                        cur.clear();
                        saw_insn = false;
                    }
                    cur.push(name.as_str());
                }
                Item::Insn(insn) => {
                    saw_insn = true;
                    if insn.mnem == "JMP"
                        && let Some(t) = insn.operand.as_deref()
                        && set.contains(t)
                    {
                        for l in &cur {
                            if !set.contains(*l) {
                                set.insert((*l).to_string());
                                grew = true;
                            }
                        }
                    }
                }
                Item::Directive(_) => {
                    cur.clear();
                    saw_insn = false;
                }
                _ => {}
            }
        }
        if !grew {
            break;
        }
    }
    set
}

fn insn_adds_frame_to_sensitive(
    insn: &Insn,
    sensitive: &std::collections::HashSet<String>,
) -> bool {
    insn.mnem == "JSR"
        && insn
            .operand
            .as_deref()
            .is_some_and(|t| sensitive.contains(t))
}

/// Check instructions against the requested call policy.
fn is_factorable_insn_with_policy(insn: &Insn, jsr: JsrPolicy) -> bool {
    // Keep control flow outside shared instruction windows.
    if is_uncond_terminator(&insn.mnem) || is_cond_branch(&insn.mnem) {
        return false;
    }
    if insn.mnem == "JSR" && matches!(jsr, JsrPolicy::Refused | JsrPolicy::CompactRefused) {
        return false;
    }
    // Keep stack pointer instructions outside shared windows.
    if matches!(insn.mnem.as_str(), "TSX" | "TXS") {
        return false;
    }
    // Check stack balance across the entire window.
    if let Some(op) = insn.operand.as_deref()
        // A complete numeric operand has no label.
        && !(jsr == JsrPolicy::CompactLeafCalls
            && parse_plain_addr(op.strip_prefix('#').unwrap_or(op)).is_some())
        && operand_has_unstable_label_with_policy(op,
            matches!(jsr, JsrPolicy::CompactRefused | JsrPolicy::CompactLeafCalls))
    {
        return false;
    }
    // Page one is the hardware stack.
    if let Some(op) = insn.operand.as_deref()
        && operand_addresses_stack_page(op)
    {
        return false;
    }
    true
}

/// Whether the operand addresses the hardware stack page.
fn operand_addresses_stack_page(op: &str) -> bool {
    let base = op.split(',').next().unwrap_or(op).trim();
    let base = base.trim_start_matches('(').trim_end_matches(')');
    let Some(hex) = base.strip_prefix('$') else {
        return false;
    };
    hex.len() == 4
        && hex.chars().all(|c| c.is_ascii_hexdigit())
        && u16::from_str_radix(hex, 16).is_ok_and(|a| (0x0100..=0x01FF).contains(&a))
}

/// Find instruction labels whose operands are modified by stores.
fn smc_patched_labels(items: &[Item]) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for it in items {
        let Item::Insn(insn) = it else { continue };
        if !matches!(insn.mnem.as_str(), "STA" | "STX" | "STY") {
            continue;
        }
        let Some(op) = insn.operand.as_deref() else {
            continue;
        };
        let Some((label, offset)) = op.split_once('+') else {
            continue;
        };
        let label = label.trim();
        if offset.trim().parse::<u16>().is_ok()
            && !label.is_empty()
            && label
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !label.as_bytes()[0].is_ascii_digit()
        {
            out.insert(label.to_string());
        }
    }
    out
}

/// Whether an instruction follows a label used for runtime operand writes.
fn insn_is_smc_target(
    items: &[Item],
    idx: usize,
    patched: &std::collections::HashSet<String>,
) -> bool {
    if patched.is_empty() {
        return false;
    }
    let mut at = idx;
    while let Some(prev) = prev_struct(items, at) {
        match &items[prev] {
            Item::Label(name) => {
                if patched.contains(name) {
                    return true;
                }
                at = prev;
            }
            _ => return false,
        }
    }
    false
}

/// Whether an operand mentions a label that cannot move safely.
fn operand_has_unstable_label_with_policy(op: &str, compact: bool) -> bool {
    let bytes = op.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < bytes.len() {
                let cc = bytes[i] as char;
                if cc.is_ascii_alphanumeric() || cc == '_' {
                    i += 1;
                } else {
                    break;
                }
            }
            let token = &op[start..i];
            // Hex preceded by `$` is fine (BBA2, FFD2 etc.)
            if start > 0 && bytes[start - 1] == b'$' {
                continue;
            }
            // Register-name suffix `,X`/`,Y`/`,A` after a comma.
            if start > 0
                && bytes[start - 1] == b','
                && (token == "X" || token == "Y" || token == "A")
            {
                continue;
            }
            // Whitelist compiler-stable label prefixes/shapes.
            if is_stable_compiler_label(token) {
                continue;
            }

            if compact && structured::is_integer_or_string_home(token) {
                continue;
            }
            return true;
        }
        i += 1;
    }
    false
}

/// Whether a generated storage or helper label can move safely.
fn is_stable_compiler_label(token: &str) -> bool {
    if token.starts_with("__") {
        return true;
    }
    if let Some(rest) = token.strip_prefix("V_") {
        return !rest.is_empty();
    }
    if structured::is_integer_or_string_home(token) {
        return true;
    }
    if let Some(rest) = token.strip_prefix("FI_") {
        return !rest.is_empty();
    }
    if let Some(rest) = token.strip_prefix("FU_") {
        return !rest.is_empty();
    }
    if let Some(rest) = token.strip_prefix("FE_") {
        return !rest.is_empty();
    }
    if let Some(rest) = token.strip_prefix("FS_") {
        return !rest.is_empty();
    }
    // Zero page equates keep their absolute addresses.
    if let Some(rest) = token.strip_prefix("ZSI_") {
        return !rest.is_empty();
    }
    if let Some(rest) = token.strip_prefix("ZARR_") {
        return !rest.is_empty();
    }
    if let Some(rest) = token.strip_prefix(['S', 'F', 'T'])
        && !rest.is_empty()
        && rest.chars().all(|c| c.is_ascii_digit())
    {
        return true;
    }
    false
}

/// Render a stable instruction signature for matching and helper emission.
fn window_signature(items: &[Item], idxs: &[usize]) -> String {
    let mut sig = String::new();
    for (n, &idx) in idxs.iter().enumerate() {
        if n > 0 {
            sig.push('\n');
        }
        if let Item::Insn(insn) = &items[idx] {
            sig.push_str(&insn.mnem);
            if let Some(op) = &insn.operand {
                sig.push(' ');
                sig.push_str(op);
            }
        }
    }
    sig
}

/// Estimate instruction bytes to check whether sharing saves space.
fn window_byte_estimate(
    items: &[Item],
    start: usize,
    window_size: usize,
    zp: &ZpEquates,
) -> usize {
    let mut total = 0usize;
    let mut cur = start;
    for n in 0..window_size {
        if n > 0 {
            cur = match next_struct(items, cur) {
                Some(c) => c,
                None => return total,
            };
        }
        if let Some(Item::Insn(insn)) = items.get(cur) {
            total += approx_insn_bytes(insn, zp);
        }
    }
    total
}

/// Labels that anchor the tail of the image, in the order a helper splice must prefer them.
const HELPER_ANCHOR_LABELS: [&str; 2] = ["__BSS_START", "__HEAP_BOTTOM"];

/// Find the insertion point for shared helpers outside program fall-through.
fn helper_anchor_index(items: &[Item]) -> Option<usize> {
    for anchor in HELPER_ANCHOR_LABELS {
        if let Some(idx) = items
            .iter()
            .position(|it| matches!(it, Item::Label(name) if name == anchor))
        {
            return Some(idx);
        }
    }
    None
}

/// Insert helpers at the heap boundary or append without a boundary.
fn append_helpers_before_heap(items: &mut Vec<Item>, helpers: Vec<Item>) {
    match helper_anchor_index(items) {
        Some(idx) => {
            // Splice in place.
            for (offset, item) in helpers.into_iter().enumerate() {
                items.insert(idx + offset, item);
            }
        }
        None => {
            for item in helpers {
                items.push(item);
            }
        }
    }
}

/// Whether the helper section marker is present.
fn helper_section_present(items: &[Item]) -> bool {
    items
        .iter()
        .any(|it| matches!(it, Item::Comment(c) if c.contains("peephole-factored helpers")))
}

fn item_label_present(items: &[Item], label: &str) -> bool {
    items
        .iter()
        .any(|it| matches!(it, Item::Label(name) if name == label))
}

fn item_label_referenced(items: &[Item], label: &str) -> bool {
    items.iter().any(|it| {
        if let Item::Insn(insn) = it
            && let Some(op) = insn.operand.as_deref()
        {
            extract_labels_from_operand(op)
                .iter()
                .any(|found| found == label)
        } else {
            false
        }
    })
}

/// Emit required helpers still referenced after factoring.
fn ensure_referenced_codegen_helpers(items: &mut Vec<Item>) {
    let mut helpers = Vec::new();
    if item_label_referenced(items, "__FAC_TO_INT16")
        && !item_label_present(items, "__FAC_TO_INT16")
    {
        append_fac_to_int16_helper(&mut helpers);
    }
    if item_label_referenced(items, "__FAC_TO_INT16_NOTRAP")
        && !item_label_present(items, "__FAC_TO_INT16_NOTRAP")
    {
        append_fac_to_int16_notrap_helper(&mut helpers);
    }
    if item_label_referenced(items, "__BOOL_TO_FAC") && !item_label_present(items, "__BOOL_TO_FAC")
    {
        append_bool_to_fac_helper(&mut helpers);
    }
    if !helpers.is_empty() {
        append_helpers_before_heap(items, helpers);
    }
}

fn push_insn(items: &mut Vec<Item>, mnem: &str, operand: Option<String>) {
    items.push(Item::Insn(Insn {
        mnem: mnem.to_string(),
        operand,
        comment: None,
    }));
}

fn append_fac_to_int16_helper(items: &mut Vec<Item>) {
    items.push(Item::Label("__FAC_TO_INT16".to_string()));
    push_insn(items, "LDA", Some(format!("${:02X}", rt::FAC_SIGN)));
    push_insn(items, "PHA", None);
    push_insn(items, "LDA", Some("#$00".to_string()));
    push_insn(items, "STA", Some(format!("${:02X}", rt::FAC_SIGN)));
    push_insn(items, "JSR", Some(format!("${:04X}", rt::FACWORD)));
    push_insn(items, "PLA", None);
    push_insn(items, "BPL", Some("__FTI_DONE".to_string()));
    push_insn(items, "SEC", None);
    push_insn(items, "LDA", Some("#$00".to_string()));
    push_insn(items, "SBC", Some(format!("${:02X}", rt::LINNUM_LO)));
    push_insn(items, "STA", Some(format!("${:02X}", rt::LINNUM_LO)));
    push_insn(items, "LDA", Some("#$00".to_string()));
    push_insn(items, "SBC", Some(format!("${:02X}", rt::LINNUM_HI)));
    push_insn(items, "STA", Some(format!("${:02X}", rt::LINNUM_HI)));
    items.push(Item::Label("__FTI_DONE".to_string()));
    push_insn(items, "RTS", None);
}

fn append_fac_to_int16_notrap_helper(items: &mut Vec<Item>) {
    items.push(Item::Label("__FAC_TO_INT16_NOTRAP".to_string()));
    push_insn(items, "LDA", Some(format!("${:02X}", rt::FAC_EXP)));
    push_insn(items, "BEQ", Some("__FTIN_ZERO".to_string()));
    push_insn(items, "BPL", Some("__FTIN_ZERO".to_string()));
    push_insn(items, "CMP", Some("#$91".to_string()));
    push_insn(items, "BCS", Some("__FTIN_OVERFLOW".to_string()));
    push_insn(items, "LDA", Some(format!("${:02X}", rt::FAC_SIGN)));
    push_insn(items, "PHA", None);
    push_insn(items, "LDA", Some("#$00".to_string()));
    push_insn(items, "STA", Some(format!("${:02X}", rt::FAC_SIGN)));
    push_insn(items, "JSR", Some(format!("${:04X}", rt::FACWORD)));
    push_insn(items, "PLA", None);
    push_insn(items, "BPL", Some("__FTIN_DONE".to_string()));
    push_insn(items, "SEC", None);
    push_insn(items, "LDA", Some("#$00".to_string()));
    push_insn(items, "SBC", Some(format!("${:02X}", rt::LINNUM_LO)));
    push_insn(items, "STA", Some(format!("${:02X}", rt::LINNUM_LO)));
    push_insn(items, "LDA", Some("#$00".to_string()));
    push_insn(items, "SBC", Some(format!("${:02X}", rt::LINNUM_HI)));
    push_insn(items, "STA", Some(format!("${:02X}", rt::LINNUM_HI)));
    push_insn(items, "RTS", None);
    items.push(Item::Label("__FTIN_ZERO".to_string()));
    push_insn(items, "LDA", Some("#$00".to_string()));
    push_insn(items, "STA", Some(format!("${:02X}", rt::LINNUM_LO)));
    push_insn(items, "STA", Some(format!("${:02X}", rt::LINNUM_HI)));
    items.push(Item::Label("__FTIN_DONE".to_string()));
    push_insn(items, "RTS", None);
    items.push(Item::Label("__FTIN_OVERFLOW".to_string()));
    push_insn(items, "LDA", Some("#$00".to_string()));
    push_insn(items, "STA", Some(format!("${:02X}", rt::LINNUM_LO)));
    push_insn(items, "STA", Some(format!("${:02X}", rt::LINNUM_HI)));
    push_insn(items, "RTS", None);
}

fn append_bool_to_fac_helper(items: &mut Vec<Item>) {
    items.push(Item::Label("__BOOL_TO_FAC".to_string()));
    push_insn(items, "TAY", None);
    push_insn(items, "JSR", Some(format!("${:04X}", rt::GIVAYF)));
    push_insn(items, "RTS", None);
}

/// Find the next free `__SEQ_<n>` id by scanning existing labels.
fn peephole_next_seq_id(items: &[Item]) -> u32 {
    let mut max = 0u32;
    for it in items {
        if let Item::Label(name) = it
            && let Some(rest) = name.strip_prefix("__SEQ_")
            && let Ok(n) = rest.parse::<u32>()
        {
            if n + 1 > max {
                max = n + 1;
            }
        }
    }
    max
}

/// Labels resolved by name during linking.
const PINNED_MARKER_LABELS: [&str; 3] = ["__DATA_END", "__VARS_START", "__VARS_END"];

/// Remove unreachable internal trampolines.
fn ph303_drop_unreferenced_trampolines(items: &mut Vec<Item>) -> bool {
    use std::collections::HashMap;
    // Count references in instructions and data operands.
    let mut refs: HashMap<String, u32> = HashMap::new();
    let mut label_pos: HashMap<String, usize> = HashMap::new();
    for (idx, it) in items.iter().enumerate() {
        match it {
            Item::Insn(insn) => {
                if let Some(op) = insn.operand.as_deref() {
                    for label in extract_labels_from_operand(op) {
                        *refs.entry(label).or_insert(0) += 1;
                    }
                }
            }
            Item::Directive(raw) | Item::Origin(raw) => {
                for label in directive_operand_labels(raw) {
                    *refs.entry(label).or_insert(0) += 1;
                }
            }
            Item::Label(name) => {
                label_pos.insert(name.clone(), idx);
            }
            _ => {}
        }
    }
    // Plan removals.
    let mut to_remove: Vec<usize> = Vec::new();
    let mut i = 0;
    while i < items.len() {
        if let Item::Label(name) = &items[i]
            && name.starts_with("__")
            && !PINNED_MARKER_LABELS.contains(&name.as_str())
            && refs.get(name.as_str()).copied().unwrap_or(0) == 0
        {

            let prev_terminator = prev_struct(items, i)
                .map(|j| matches!(&items[j], Item::Insn(insn) if is_uncond_terminator(&insn.mnem)))
                .unwrap_or(false);
            if let Some(j) = next_struct(items, i) {
                match &items[j] {
                    Item::Insn(insn)
                        if matches!(insn.mnem.as_str(), "JMP" | "RTS" | "RTI")
                            && prev_terminator =>
                    {
                        to_remove.push(i);
                        to_remove.push(j);
                    }
                    // Stacked labels share one address.
                    Item::Label(_) => {
                        to_remove.push(i);
                    }
                    Item::Insn(_) if prev_terminator => {
                        let region_end = scan_dead_stub_body(items, j)
                            .or_else(|| scan_dead_region(items, i, &refs, &label_pos));
                        if let Some(stub_end) = region_end {
                            for idx in i..=stub_end {
                                if matches!(items[idx], Item::Insn(_) | Item::Label(_)) {
                                    to_remove.push(idx);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        i += 1;
    }
    if to_remove.is_empty() {
        return false;
    }
    // Sort descending so removals don't invalidate later indices.
    to_remove.sort_unstable_by(|a, b| b.cmp(a));
    to_remove.dedup();
    for idx in to_remove {
        items.remove(idx);
    }
    true
}

/// Find the end of a removable linear helper body.
fn scan_dead_stub_body(items: &[Item], start: usize) -> Option<usize> {
    let mut i = start;
    let mut steps = 0usize;
    while i < items.len() {
        steps += 1;
        if steps > 8 {
            return None;
        }
        match &items[i] {
            Item::Insn(insn) => {
                if is_cond_branch(&insn.mnem) {
                    return None;
                }
                if is_uncond_terminator(&insn.mnem) {
                    return Some(i);
                }
            }
            Item::Label(_) | Item::Directive(_) => return None,
            Item::Blank | Item::Comment(_) | Item::Origin(_) => {}
        }
        i += 1;
    }
    None
}

/// Find the end of a helper region with no incoming references.
fn scan_dead_region(
    items: &[Item],
    label_idx: usize,
    refs: &std::collections::HashMap<String, u32>,
    label_pos: &std::collections::HashMap<String, usize>,
) -> Option<usize> {
    let body_start = next_struct(items, label_idx)?;
    // Labels that belong to this region (entry + any internal ones).
    let mut region_labels: Vec<String> = Vec::new();
    if let Item::Label(n) = &items[label_idx] {
        region_labels.push(n.clone());
    }
    let mut i = body_start;
    let mut frontier = body_start; // must scan at least this far forward
    let mut steps = 0usize;
    let mut end: Option<usize> = None;
    while i < items.len() {
        steps += 1;
        if steps > 64 {
            return None; // too large to risk treating as a dead stub
        }
        match &items[i] {
            Item::Insn(insn) => {
                let target = insn
                    .operand
                    .as_deref()
                    .and_then(|op| label_pos.get(op).copied());
                if is_cond_branch(&insn.mnem) {
                    // Every branch must stay within the region.
                    match target {
                        Some(t) if t >= label_idx => frontier = frontier.max(t),
                        _ => return None,
                    }
                } else if insn.mnem == "JMP" {
                    match target {
                        // Extend the region through an internal forward jump.
                        Some(t) if t >= label_idx => frontier = frontier.max(t),
                        // A tail call or jump outside the region ends the path.
                        _ => {
                            if i >= frontier {
                                end = Some(i);
                                break;
                            }
                        }
                    }
                } else if is_uncond_terminator(&insn.mnem) {
                    if i >= frontier {
                        end = Some(i);
                        break;
                    }
                }
            }
            Item::Label(n) => {
                region_labels.push(n.clone());
                frontier = frontier.max(i);
            }
            Item::Directive(_) => return None,
            Item::Blank | Item::Comment(_) | Item::Origin(_) => {}
        }
        i += 1;
    }
    let end = end?;
    // Every region label must have no references from outside it.
    for name in &region_labels {
        let total = refs.get(name).copied().unwrap_or(0);
        let mut internal = 0u32;
        for it in &items[label_idx..=end] {
            if let Item::Insn(insn) = it
                && let Some(op) = insn.operand.as_deref()
            {
                internal += extract_labels_from_operand(op)
                    .iter()
                    .filter(|l| *l == name)
                    .count() as u32;
            }
        }
        if total > internal {
            return None;
        }
    }
    Some(end)
}

/// Direct control flow target of an instruction.
fn insn_direct_target(insn: &Insn) -> Option<&str> {
    if !matches!(insn.mnem.as_str(), "JMP" | "JSR") && !is_cond_branch(&insn.mnem) {
        return None;
    }
    let op = insn.operand.as_deref()?.trim();

    if op.starts_with('(') || op.starts_with('$') {
        return None;
    }
    if op.contains(['+', '-', '<', '>', ',', '#']) {
        return None;
    }
    Some(op)
}

/// Labels whose addresses are taken by instructions or data.
fn address_taken_labels(items: &[Item]) -> std::collections::HashSet<String> {
    let mut taken = std::collections::HashSet::new();
    for it in items {
        match it {
            Item::Insn(insn) => {
                let Some(op) = insn.operand.as_deref() else {
                    continue;
                };
                let direct = insn_direct_target(insn);
                for label in extract_labels_from_operand(op) {
                    if Some(label.as_str()) != direct {
                        taken.insert(label);
                    }
                }
            }
            // Directives are opaque data.
            Item::Directive(raw) => {
                let body = raw.trim_start();
                let body = body
                    .strip_prefix('.')
                    .and_then(|rest| rest.split_once(char::is_whitespace))
                    .map_or(body, |(_, args)| args);
                for label in extract_labels_from_operand(body) {
                    taken.insert(label);
                }
            }
            _ => {}
        }
    }
    taken
}

/// Find reachable items from entry and all address references.
fn reachable_items(items: &[Item]) -> Vec<bool> {
    let mut label_at: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for (i, it) in items.iter().enumerate() {
        if let Item::Label(n) = it {
            label_at.insert(n.as_str(), i);
        }
    }
    let mut seen = vec![false; items.len()];
    let mut work: Vec<usize> = Vec::new();
    // Start at the image entry.
    if !items.is_empty() {
        work.push(0);
    }
    // Include labels whose addresses are taken.
    for name in address_taken_labels(items) {
        if let Some(&idx) = label_at.get(name.as_str()) {
            work.push(idx);
        }
    }
    while let Some(i) = work.pop() {
        if i >= items.len() || seen[i] {
            continue;
        }
        seen[i] = true;
        let mut fall_through = true;
        if let Item::Insn(insn) = &items[i] {
            if let Some(t) = insn_direct_target(insn)
                && let Some(&idx) = label_at.get(t)
            {
                work.push(idx);
            }

            if matches!(insn.mnem.as_str(), "JMP" | "RTS" | "RTI" | "BRK") {
                fall_through = false;
            }
        }
        if fall_through {
            work.push(i + 1);
        }
    }
    seen
}

/// Remove instructions unreachable from program entry or referenced data.
fn ph306_drop_unreachable_code(items: &mut Vec<Item>) -> bool {
    let seen = reachable_items(items);
    let mut drop = vec![false; items.len()];
    for (i, it) in items.iter().enumerate() {
        if seen[i] {
            continue;
        }
        match it {
            Item::Insn(_) => drop[i] = true,
            Item::Label(_) => {

                if let Some(j) = next_struct(items, i)
                    && matches!(items[j], Item::Insn(_))
                    && !seen[j]
                {
                    drop[i] = true;
                }
            }
            _ => {}
        }
    }
    if !drop.iter().any(|&d| d) {
        return false;
    }
    let mut i = 0;
    items.retain(|_| {
        let keep = !drop[i];
        i += 1;
        keep
    });
    true
}

/// Remove NOP unless timing is explicitly marked.
fn ph013_drop_nop(items: &mut Vec<Item>) -> bool {
    let mut changed = false;
    items.retain(|it| {
        if let Item::Insn(insn) = it
            && insn.mnem == "NOP"
            && insn.operand.is_none()
            && !insn
                .comment
                .as_deref()
                .map(|c| c.contains("@keep_nop"))
                .unwrap_or(false)
        {
            changed = true;
            return false;
        }
        true
    });
    changed
}

/// Replace sign masks with BMI or BPL when A and flags are dead.
fn ph204_and_signbit_to_branch(items: &mut Vec<Item>) -> bool {
    let cfg = liveness_config_with_helpers();
    let live = analyze_register_liveness_with_config(items, &cfg);
    let mut rewrites: Vec<(usize, usize, &'static str)> = Vec::new();
    let mut i = 0;
    while i < items.len() {
        let Item::Insn(lda) = &items[i] else {
            i += 1;
            continue;
        };
        if lda.mnem != "LDA" {
            i += 1;
            continue;
        }
        let Some(j) = next_struct(items, i) else {
            break;
        };
        let Some(Item::Insn(and)) = items.get(j) else {
            i += 1;
            continue;
        };
        if and.mnem != "AND" || and.operand.as_deref().and_then(parse_imm_byte) != Some(0x80) {
            i += 1;
            continue;
        }
        let Some(k) = next_struct(items, j) else {
            i += 1;
            continue;
        };
        let Some(Item::Insn(br)) = items.get(k) else {
            i += 1;
            continue;
        };
        let new_mnem = match br.mnem.as_str() {
            "BNE" => "BMI",
            "BEQ" => "BPL",
            _ => {
                i += 1;
                continue;
            }
        };
        if live.is_live_out(k, Reg::A) {
            i += 1;
            continue;
        }
        rewrites.push((j, k, new_mnem));
        i = k + 1;
    }
    if rewrites.is_empty() {
        return false;
    }
    // Apply in reverse so earlier indices stay valid.
    for (and_idx, br_idx, new_mnem) in rewrites.into_iter().rev() {
        if let Item::Insn(br) = &mut items[br_idx] {
            br.mnem = new_mnem.to_string();
        }
        items.remove(and_idx);
    }
    true
}

/// Fold branches on immediate loads and remove dead loads.
fn ph207_const_load_branch_fold(items: &mut Vec<Item>) -> bool {
    let cfg = liveness_config_with_helpers();
    let reg_live = analyze_register_liveness_with_config(items, &cfg);
    let flag_live = analyze_flag_liveness(items);

    enum Action {
        ToJmp,
        Drop,
    }
    let mut plans: Vec<(usize, usize, Action, bool)> = Vec::new();
    let mut i = 0;
    while i < items.len() {
        let Item::Insn(load) = &items[i] else {
            i += 1;
            continue;
        };
        let dest = match load.mnem.as_str() {
            "LDA" => Reg::A,
            "LDX" => Reg::X,
            "LDY" => Reg::Y,
            _ => {
                i += 1;
                continue;
            }
        };
        let Some(op) = load.operand.as_deref() else {
            i += 1;
            continue;
        };
        let Some(imm) = parse_imm_byte(op) else {
            i += 1;
            continue;
        };
        let Some(j) = next_struct(items, i) else {
            break;
        };
        let Some(Item::Insn(br)) = items.get(j) else {
            i += 1;
            continue;
        };
        let z_set_by_imm = imm == 0;
        let always_taken = match br.mnem.as_str() {
            "BEQ" => z_set_by_imm,
            "BNE" => !z_set_by_imm,
            _ => {
                i += 1;
                continue;
            }
        };
        let action = if always_taken {
            // Require a direct jump target.
            let Some(target) = br.operand.as_deref() else {
                i += 1;
                continue;
            };
            if target.contains(',') || target.starts_with('(') {
                i += 1;
                continue;
            }
            Action::ToJmp
        } else {
            Action::Drop
        };
        // Remove the load only when its register and flags are dead.
        let reg_dead = !reg_live.is_live_out(j, dest);
        let z_dead = !flag_live.is_live_out(j, Flag::Z);
        let n_dead = !flag_live.is_live_out(j, Flag::N);
        let drop_load = reg_dead && z_dead && n_dead;
        plans.push((i, j, action, drop_load));
        i = j + 1;
    }
    if plans.is_empty() {
        return false;
    }
    // Apply in reverse to keep earlier indices valid.
    for (load_idx, br_idx, action, drop_load) in plans.into_iter().rev() {
        match action {
            Action::ToJmp => {
                if let Item::Insn(br) = &mut items[br_idx] {
                    br.mnem = "JMP".to_string();
                }
            }
            Action::Drop => {
                items.remove(br_idx);
            }
        }
        if drop_load {
            items.remove(load_idx);
        }
    }
    true
}

/// Remove identity masks when N and Z are dead.
fn ph205_mask_identity(items: &mut Vec<Item>) -> bool {
    let mut to_drop = Vec::new();
    for (idx, it) in items.iter().enumerate() {
        let Item::Insn(insn) = it else { continue };
        let Some(op) = insn.operand.as_deref() else {
            continue;
        };
        let identity = match insn.mnem.as_str() {
            "AND" => parse_imm_byte(op).map_or(false, |b| b == 0xFF),
            "ORA" | "EOR" => parse_imm_byte(op).map_or(false, |b| b == 0x00),
            _ => false,
        };
        if !identity {
            continue;
        }
        // Keep masks when the next branch reads their flags.
        if let Some(j) = next_struct(items, idx)
            && let Some(Item::Insn(next)) = items.get(j)
            && is_cond_branch(&next.mnem)
        {
            continue;
        }
        to_drop.push(idx);
    }
    if to_drop.is_empty() {
        return false;
    }
    for idx in to_drop.into_iter().rev() {
        items.remove(idx);
    }
    true
}

/// Remove adjacent PHA/PLA only when N and Z are dead.
fn ph206_pha_pla_roundtrip(items: &mut Vec<Item>) -> bool {
    let mut to_drop = Vec::new();
    for (idx, it) in items.iter().enumerate() {
        let Item::Insn(push) = it else { continue };
        if push.mnem != "PHA" || push.operand.is_some() {
            continue;
        }
        let Some(j) = next_struct(items, idx) else {
            continue;
        };
        let Some(Item::Insn(pop)) = items.get(j) else {
            continue;
        };
        if pop.mnem != "PLA" || pop.operand.is_some() {
            continue;
        }
        if pla_flags_observed_before_redefined(items, j) {
            continue;
        }
        to_drop.push(idx);
        to_drop.push(j);
    }
    if to_drop.is_empty() {
        return false;
    }
    to_drop.sort_unstable_by(|a, b| b.cmp(a));
    to_drop.dedup();
    for idx in to_drop {
        items.remove(idx);
    }
    true
}

fn pla_flags_observed_before_redefined(items: &[Item], pla_idx: usize) -> bool {
    let mut cur = pla_idx;
    loop {
        let Some(next) = next_struct(items, cur) else {
            return true;
        };
        match &items[next] {
            Item::Insn(insn) => {
                if is_cond_branch(&insn.mnem) || is_uncond_terminator(&insn.mnem) {
                    return true;
                }
                if sets_status_flags(&insn.mnem) {
                    return false;
                }
                cur = next;
            }
            Item::Label(_) | Item::Directive(_) | Item::Origin(_) => return true,
            Item::Comment(_) | Item::Blank => {
                unreachable!("next_struct skips non-structural items")
            }
        }
    }
}

/// Replace a balanced pointer push/pop sequence with direct scratch stores.
fn ph209_spill_pha_pla_pair(items: &mut Vec<Item>) -> bool {

    if !ph209_has_candidate_triple(items) {
        return false;
    }
    let equ = collect_equ_values(items);
    let origin = lowest_origin_addr(items);
    let patched = smc_patched_labels(items);
    let regs = analyze_register_liveness_with_config(items, &liveness_config_with_helpers());
    let flags = analyze_flag_liveness_with_config(items, &peephole_flag_liveness_config());
    let labels = register_liveness_label_map(items);

    let mut plans: Vec<Ph209Plan> = Vec::new();
    let mut consumed_to = 0usize;
    for i in 0..items.len() {
        if i < consumed_to {
            continue;
        }
        let Some(plan) = ph209_plan(items, i, &equ, origin, &patched, &regs, &flags, &labels)
        else {
            continue;
        };
        consumed_to = plan.sta2 + 1;
        plans.push(plan);
    }
    if plans.is_empty() {
        return false;
    }

    for plan in plans.into_iter().rev() {
        items[plan.pha1] = Item::Insn(Insn {
            mnem: "STA".to_string(),
            operand: Some(plan.p2),

            comment: Some("ph209: spill hoisted out of a balanced push/pop".to_string()),
        });
        items[plan.tya] = Item::Insn(Insn {
            mnem: "STY".to_string(),
            operand: Some(plan.p1),
            comment: None,
        });
        for idx in [plan.sta2, plan.pla2, plan.sta1, plan.pla1, plan.pha2] {
            items.remove(idx);
        }
    }
    true
}

/// Instruction indices and scratch destinations for pointer-spill removal.
struct Ph209Plan {
    /// First push, replaced with STA to the second destination.
    pha1: usize,
    /// Register transfer, replaced with STY to the first destination.
    tya: usize,
    /// Second `PHA`, and the four tail items, all removed.
    pha2: usize,
    pla1: usize,
    sta1: usize,
    pla2: usize,
    sta2: usize,
    p1: String,
    p2: String,
}

/// Is there a `PHA` / `TYA` / `PHA` anywhere?
fn ph209_has_candidate_triple(items: &[Item]) -> bool {
    for (idx, it) in items.iter().enumerate() {
        let Item::Insn(insn) = it else { continue };
        if insn.mnem != "PHA" || insn.operand.is_some() {
            continue;
        }
        let Some(j) = next_struct(items, idx) else {
            continue;
        };
        if !matches!(&items[j], Item::Insn(n) if n.mnem == "TYA" && n.operand.is_none()) {
            continue;
        }
        let Some(k) = next_struct(items, j) else {
            continue;
        };
        if matches!(&items[k], Item::Insn(n) if n.mnem == "PHA" && n.operand.is_none()) {
            return true;
        }
    }
    false
}

/// Maximum number of intervening instructions to inspect.
const PH209_MAX_MIDDLE: usize = 16;

#[allow(clippy::too_many_arguments)]
fn ph209_plan(
    items: &[Item],
    i: usize,
    equ: &std::collections::HashMap<String, u16>,
    origin: Option<u16>,
    patched: &std::collections::HashSet<String>,
    regs: &RegisterLiveness,
    flags: &FlagLiveness,
    labels: &std::collections::HashMap<String, usize>,
) -> Option<Ph209Plan> {
    if !matches!(&items[i], Item::Insn(n) if n.mnem == "PHA" && n.operand.is_none()) {
        return None;
    }

    if insn_is_smc_target(items, i, patched) {
        return None;
    }
    let tya = next_struct(items, i)?;
    if !matches!(&items[tya], Item::Insn(n) if n.mnem == "TYA" && n.operand.is_none()) {
        return None;
    }
    let pha2 = next_struct(items, tya)?;
    if !matches!(&items[pha2], Item::Insn(n) if n.mnem == "PHA" && n.operand.is_none()) {
        return None;
    }

    let mut middle = Vec::new();
    let mut cur = pha2;
    let pla1 = loop {
        let next = next_struct(items, cur)?;
        let Item::Insn(insn) = &items[next] else {
            return None;
        };
        if insn.mnem == "PLA" && insn.operand.is_none() {
            break next;
        }
        if middle.len() >= PH209_MAX_MIDDLE {
            return None;
        }
        middle.push(next);
        cur = next;
    };

    let sta1 = next_struct(items, pla1)?;
    let p1 = ph209_store_target(&items[sta1])?;
    let pla2 = next_struct(items, sta1)?;
    if !matches!(&items[pla2], Item::Insn(n) if n.mnem == "PLA" && n.operand.is_none()) {
        return None;
    }
    let sta2 = next_struct(items, pla2)?;
    let p2 = ph209_store_target(&items[sta2])?;

    let p1_cell = cell_of_plain_operand(&p1, equ)?;
    let p2_cell = cell_of_plain_operand(&p2, equ)?;
    // Aliased destinations would swap which write wins.
    if mem_cells_may_alias(&p1_cell, &p2_cell, origin) {
        return None;
    }

    for &m in &middle {
        let Item::Insn(insn) = &items[m] else {
            return None;
        };
        if let Some(op) = insn.operand.as_deref()
            && operand_addresses_stack_page(op)
        {
            return None;
        }
        if !ph209_middle_preserves_cell(insn, &p1_cell, equ, origin)
            || !ph209_middle_preserves_cell(insn, &p2_cell, equ, origin)
        {
            return None;
        }
    }

    let middle_start = *middle.first().unwrap_or(&pla1);
    if regs.is_live_in(middle_start, Reg::A)
        || flags.is_live_in(middle_start, Flag::N)
        || flags.is_live_in(middle_start, Flag::Z)
    {
        return None;
    }

    if !ph209_dead_after(items, sta2, labels, |idx, live_in| match live_in {
        true => regs.is_live_in(idx, Reg::A),
        false => regs.is_live_out(idx, Reg::A),
    }) {
        return None;
    }
    if !ph209_dead_after(items, sta2, labels, |idx, live_in| match live_in {
        true => flags.is_live_in(idx, Flag::N) || flags.is_live_in(idx, Flag::Z),
        false => flags.is_live_out(idx, Flag::N) || flags.is_live_out(idx, Flag::Z),
    }) {
        return None;
    }

    Some(Ph209Plan {
        pha1: i,
        tya,
        pha2,
        pla1,
        sta1,
        pla2,
        sta2,
        p1,
        p2,
    })
}

/// Whether an intervening instruction preserves the pointer destination.
fn ph209_middle_preserves_cell(
    insn: &Insn,
    cell: &MemCell,
    equ: &std::collections::HashMap<String, u16>,
    origin: Option<u16>,
) -> bool {
    if window_insn_preserves_cell(insn, cell, equ, origin) {
        return true;
    }
    // Only mnemonics whose entire memory effect is the one named cell.
    if !matches!(
        insn.mnem.as_str(),
        "LDA"
            | "LDX"
            | "LDY"
            | "CMP"
            | "CPX"
            | "CPY"
            | "AND"
            | "ORA"
            | "EOR"
            | "ADC"
            | "SBC"
            | "BIT"
            | "STA"
            | "STX"
            | "STY"
            | "INC"
            | "DEC"
    ) {
        return false;
    }
    let Some(op) = insn.operand.as_deref() else {
        return false;
    };
    let Some(other) = ph209_label_offset_cell(op, equ) else {
        return false;
    };
    !mem_cells_may_alias(&other, cell, origin)
}

/// The cell named by a `LABEL+n` operand.
fn ph209_label_offset_cell(
    op: &str,
    equ: &std::collections::HashMap<String, u16>,
) -> Option<MemCell> {
    if op.starts_with('#') || op.contains(',') || op.contains('(') || op.contains('-') {
        return None;
    }
    let (label, offset) = op.split_once('+')?;
    let label = label.trim();
    let offset: u16 = offset.trim().parse().ok()?;
    if label.is_empty()
        || !label
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        || label.as_bytes()[0].is_ascii_digit()
    {
        return None;
    }
    match equ.get(label) {
        Some(&v) => match v.checked_add(offset) {
            Some(addr) if addr < 0x0100 => Some(MemCell::Known(addr)),
            _ => None,
        },
        None => Some(MemCell::Sym(label.to_string())),
    }
}

/// The operand of a `STA` whose target is a plain cell, or `None` for anything else.
fn ph209_store_target(item: &Item) -> Option<String> {
    let Item::Insn(insn) = item else { return None };
    if insn.mnem != "STA" {
        return None;
    }
    let op = insn.operand.as_deref()?;
    if op.contains(',') || op.contains('(') || operand_addresses_stack_page(op) {
        return None;
    }
    Some(op.to_string())
}

/// Is the resource `probe` tracks dead after the item at `idx`?
fn ph209_dead_after(
    items: &[Item],
    idx: usize,
    labels: &std::collections::HashMap<String, usize>,
    probe: impl Fn(usize, bool) -> bool,
) -> bool {
    if !probe(idx, false) {
        return true;
    }
    let Some(j) = next_struct(items, idx) else {
        return false;
    };
    let Item::Insn(insn) = &items[j] else {
        return false;
    };
    if insn.mnem != "JSR" {
        return false;
    }
    let Some(target) = insn.operand.as_deref() else {
        return false;
    };
    let Some(&entry) = labels.get(target) else {
        return false;
    };

    if !matches!(
        next_struct(items, entry).map(|k| &items[k]),
        Some(Item::Insn(_))
    ) {
        return false;
    }
    !probe(entry, true)
}

fn sets_status_flags(mnem: &str) -> bool {
    matches!(
        mnem,
        "LDA"
            | "LDX"
            | "LDY"
            | "TAX"
            | "TAY"
            | "TXA"
            | "TYA"
            | "TSX"
            | "PLA"
            | "AND"
            | "ORA"
            | "EOR"
            | "ADC"
            | "SBC"
            | "CMP"
            | "CPX"
            | "CPY"
            | "BIT"
            | "INC"
            | "INX"
            | "INY"
            | "DEC"
            | "DEX"
            | "DEY"
            | "ASL"
            | "LSR"
            | "ROL"
            | "ROR"
            | "CLC"
            | "SEC"
            | "CLI"
            | "SEI"
            | "CLD"
            | "SED"
            | "CLV"
            | "JSR"
    )
}

/// Replace register-transfer/store pairs with direct index-register stores.
fn ph211_transfer_store_to_index_store(items: &mut Vec<Item>) -> bool {
    if !items.iter().any(|it| {
        matches!(it, Item::Insn(insn)
            if matches!(insn.mnem.as_str(), "TYA" | "TXA") && insn.operand.is_none())
    }) {
        return false;
    }
    let equ = collect_equ_values(items);
    let smc = smc_patched_labels(items);
    let reg_config = liveness_config_with_helpers();
    let regs = analyze_register_liveness_with_config(items, &reg_config);
    let flag_config = conservative_return_flag_config();
    let flags = analyze_flag_liveness_with_config(items, &flag_config);

    // (transfer index, store index, replacement mnemonic).
    let mut plans: Vec<(usize, usize, &'static str)> = Vec::new();
    let mut i = 0;
    while i < items.len() {
        let Some(Item::Insn(xfer)) = items.get(i) else {
            i += 1;
            continue;
        };
        let new_mnem = match xfer.mnem.as_str() {
            "TYA" if xfer.operand.is_none() => "STY",
            "TXA" if xfer.operand.is_none() => "STX",
            _ => {
                i += 1;
                continue;
            }
        };
        let Some(j) = next_struct(items, i) else {
            i += 1;
            continue;
        };
        let Some(Item::Insn(store)) = items.get(j) else {
            i += 1;
            continue;
        };
        if store.mnem != "STA" {
            i += 1;
            continue;
        }
        let Some(op) = store.operand.as_deref() else {
            i += 1;
            continue;
        };
        if !index_store_accepts(new_mnem, op, &equ) {
            i += 1;
            continue;
        }
        // The rewrite does not set A, so A must be dead.
        if regs.is_live_out(j, Reg::A) {
            i += 1;
            continue;
        }
        // Stores do not set N and Z, so both must be dead.
        let live = flags.live_out_at(j);
        if live.contains(Flag::N) || live.contains(Flag::Z) {
            i += 1;
            continue;
        }
        if insn_is_smc_target(items, i, &smc) || insn_is_smc_target(items, j, &smc) {
            i += 1;
            continue;
        }
        plans.push((i, j, new_mnem));
        i = j + 1;
    }
    if plans.is_empty() {
        return false;
    }

    for (xfer_idx, store_idx, new_mnem) in plans.into_iter().rev() {
        if let Some(Item::Insn(store)) = items.get_mut(store_idx) {
            store.mnem = new_mnem.to_string();
        }
        items.remove(xfer_idx);
    }
    true
}

/// True iff `STY`/`STX` can encode the addressing mode `op` names.
fn index_store_accepts(mnem: &str, op: &str, equ: &std::collections::HashMap<String, u16>) -> bool {
    let op = op.trim();
    if op.is_empty() || op.starts_with('#') || op.starts_with('(') {
        return false;
    }
    if op.eq_ignore_ascii_case("A") {
        return false;
    }
    let Some((base, idx)) = op.split_once(',') else {
        return true;
    };
    let want = if mnem == "STY" { "X" } else { "Y" };
    if !idx.trim().eq_ignore_ascii_case(want) {
        return false;
    }
    matches!(
        cell_of_plain_operand(base.trim(), equ),
        Some(MemCell::Known(addr)) if addr < 0x0100
    )
}

/// Replace a shifted constant and register transfer with a constant index load.
fn ph212_lda_asl_tax_to_ldx(items: &mut Vec<Item>) -> bool {

    if !items.iter().any(
        |it| matches!(it, Item::Insn(insn) if insn.mnem == "ASL" && insn_targets_accumulator(insn)),
    ) {
        return false;
    }
    let smc = smc_patched_labels(items);
    let reg_config = liveness_config_with_helpers();
    let regs = analyze_register_liveness_with_config(items, &reg_config);
    let flag_config = conservative_return_flag_config();
    let flags = analyze_flag_liveness_with_config(items, &flag_config);

    // (lda index, asl index, transfer index, replacement mnemonic, byte).
    let mut plans: Vec<(usize, usize, usize, &'static str, u8)> = Vec::new();
    let mut i = 0;
    while i < items.len() {
        let Some(Item::Insn(lda)) = items.get(i) else {
            i += 1;
            continue;
        };
        let doubled = (|| {
            if lda.mnem != "LDA" {
                return None;
            }
            let byte = ImmFact::from_operand(lda.operand.as_deref()?)?.byte?;
            Some(byte.wrapping_shl(1))
        })();
        let Some(doubled) = doubled else {
            i += 1;
            continue;
        };
        let Some(asl_idx) = next_struct(items, i) else {
            i += 1;
            continue;
        };
        let Some(Item::Insn(asl)) = items.get(asl_idx) else {
            i += 1;
            continue;
        };
        if asl.mnem != "ASL" || !insn_targets_accumulator(asl) {
            i += 1;
            continue;
        }
        let Some(xfer_idx) = next_struct(items, asl_idx) else {
            i += 1;
            continue;
        };
        let Some(Item::Insn(xfer)) = items.get(xfer_idx) else {
            i += 1;
            continue;
        };
        let new_mnem = match xfer.mnem.as_str() {
            "TAX" if xfer.operand.is_none() => "LDX",
            "TAY" if xfer.operand.is_none() => "LDY",
            _ => {
                i += 1;
                continue;
            }
        };
        // `.A` is not written by the replacement, and `C` is not set.
        if regs.is_live_out(xfer_idx, Reg::A) || flags.is_live_out(xfer_idx, Flag::C) {
            i += 1;
            continue;
        }
        if insn_is_smc_target(items, i, &smc)
            || insn_is_smc_target(items, asl_idx, &smc)
            || insn_is_smc_target(items, xfer_idx, &smc)
        {
            i += 1;
            continue;
        }
        plans.push((i, asl_idx, xfer_idx, new_mnem, doubled));
        i = xfer_idx + 1;
    }
    if plans.is_empty() {
        return false;
    }
    for (lda_idx, asl_idx, xfer_idx, new_mnem, doubled) in plans.into_iter().rev() {
        if let Some(Item::Insn(lda)) = items.get_mut(lda_idx) {
            lda.mnem = new_mnem.to_string();
            lda.operand = Some(format!("#${doubled:02X}"));
        }
        items.remove(xfer_idx);
        items.remove(asl_idx);
    }
    true
}

/// Whether a shift or rotate targets the accumulator.
fn insn_targets_accumulator(insn: &Insn) -> bool {
    insn.operand
        .as_deref()
        .is_none_or(|op| op.trim().eq_ignore_ascii_case("A"))
}

/// Replace an adjacent constant index load with an increment or decrement.
fn ph213_imm_load_to_bump(items: &mut Vec<Item>) -> bool {

    if !items.iter().any(|it| {
        matches!(it, Item::Insn(insn)
            if matches!(insn.mnem.as_str(), "LDX" | "LDY")
                && insn.operand.as_deref().is_some_and(|op| op.starts_with('#')))
    }) {
        return false;
    }
    let facts = analyze_static_asm_facts(items);
    let smc = smc_patched_labels(items);
    let mut plans: Vec<(usize, &'static str)> = Vec::new();

    for (idx, item) in items.iter().enumerate() {
        let Item::Insn(insn) = item else { continue };
        let reg = match insn.mnem.as_str() {
            "LDX" => Reg::X,
            "LDY" => Reg::Y,
            _ => continue,
        };
        let Some(want) = insn
            .operand
            .as_deref()
            .and_then(ImmFact::from_operand)
            .and_then(|imm| imm.byte)
        else {
            continue;
        };
        let Some(have) = facts[idx].reg(reg).imm.as_ref().and_then(|imm| imm.byte) else {
            continue;
        };
        let mnem = if have.wrapping_add(1) == want {
            if reg == Reg::X { "INX" } else { "INY" }
        } else if have.wrapping_sub(1) == want {
            if reg == Reg::X { "DEX" } else { "DEY" }
        } else {
            continue;
        };
        if insn_is_smc_target(items, idx, &smc) {
            continue;
        }
        plans.push((idx, mnem));
    }
    if plans.is_empty() {
        return false;
    }
    for (idx, mnem) in plans {
        if let Some(Item::Insn(insn)) = items.get_mut(idx) {
            insn.mnem = mnem.to_string();
            insn.operand = None;
        }
    }
    true
}

fn ph214_and_bit0_to_lsr_carry(items: &mut Vec<Item>) -> bool {

    if !items.iter().any(|it| {
        matches!(it, Item::Insn(insn)
            if insn.mnem == "AND"
                && insn.operand.as_deref().and_then(parse_imm_byte) == Some(0x01))
    }) {
        return false;
    }
    let smc = smc_patched_labels(items);
    let reg_config = liveness_config_with_helpers();
    let regs = analyze_register_liveness_with_config(items, &reg_config);
    let flag_config = conservative_return_flag_config();
    let flags = analyze_flag_liveness_with_config(items, &flag_config);

    // (and index, branch index, replacement branch mnemonic).
    let mut plans: Vec<(usize, usize, &'static str)> = Vec::new();
    let mut i = 0;
    while i < items.len() {
        let Some(Item::Insn(and)) = items.get(i) else {
            i += 1;
            continue;
        };
        if and.mnem != "AND" || and.operand.as_deref().and_then(parse_imm_byte) != Some(0x01) {
            i += 1;
            continue;
        }
        let Some(br_idx) = next_struct(items, i) else {
            i += 1;
            continue;
        };
        let Some(Item::Insn(br)) = items.get(br_idx) else {
            i += 1;
            continue;
        };
        let new_branch = match br.mnem.as_str() {
            "BEQ" => "BCC",
            "BNE" => "BCS",
            _ => {
                i += 1;
                continue;
            }
        };
        if regs.is_live_out(br_idx, Reg::A) {
            i += 1;
            continue;
        }
        let live = flags.live_out_at(br_idx);
        if live.contains(Flag::Z) || live.contains(Flag::C) {
            i += 1;
            continue;
        }

        if insn_is_smc_target(items, i, &smc) || insn_is_smc_target(items, br_idx, &smc) {
            i += 1;
            continue;
        }
        plans.push((i, br_idx, new_branch));
        i = br_idx + 1;
    }
    if plans.is_empty() {
        return false;
    }
    for (and_idx, br_idx, new_branch) in plans {
        if let Some(Item::Insn(br)) = items.get_mut(br_idx) {
            br.mnem = new_branch.to_string();
        }
        if let Some(Item::Insn(and)) = items.get_mut(and_idx) {
            and.mnem = "LSR".to_string();
            and.operand = None;
        }
    }
    true
}

/// Hoist zero Y loads from loops that preserve Y.
fn ph208_hoist_ldy_zero_from_loop(items: &mut Vec<Item>) -> bool {
    for label_idx in 0..items.len() {
        let Item::Label(label) = &items[label_idx] else {
            continue;
        };
        if prev_struct_is_label(items, label_idx) {
            continue;
        }
        let Some(ldy_idx) = next_struct(items, label_idx) else {
            continue;
        };
        let Some(Item::Insn(ldy)) = items.get(ldy_idx) else {
            continue;
        };
        if ldy.mnem != "LDY" || ldy.operand.as_deref().and_then(parse_imm_byte) != Some(0) {
            continue;
        }
        let Some(back_idx) = single_jmp_ref_to_label(items, label) else {
            continue;
        };
        if back_idx <= ldy_idx {
            continue;
        }
        if !loop_body_preserves_y_zero(items, ldy_idx, back_idx) {
            continue;
        }
        let moved = items.remove(ldy_idx);
        items.insert(label_idx, moved);
        return true;
    }
    false
}

fn prev_struct_is_label(items: &[Item], idx: usize) -> bool {
    let mut cur = idx;
    while cur > 0 {
        cur -= 1;
        match &items[cur] {
            Item::Blank | Item::Comment(_) => {}
            Item::Label(_) => return true,
            _ => return false,
        }
    }
    false
}

fn single_jmp_ref_to_label(items: &[Item], label: &str) -> Option<usize> {
    let mut found = None;
    for (idx, item) in items.iter().enumerate() {
        let Item::Insn(insn) = item else { continue };
        if insn.operand.as_deref() != Some(label) {
            continue;
        }
        if found.is_some() {
            return None;
        }
        if insn.mnem != "JMP" {
            return None;
        }
        found = Some(idx);
    }
    found
}

fn loop_body_preserves_y_zero(items: &[Item], ldy_idx: usize, back_idx: usize) -> bool {
    let mut cur = ldy_idx;
    loop {
        let Some(next) = next_struct(items, cur) else {
            return false;
        };
        if next == back_idx {
            return true;
        }
        match &items[next] {
            Item::Insn(insn) => {
                if is_cond_branch(&insn.mnem) || is_uncond_terminator(&insn.mnem) {
                    return false;
                }
                if !insn_preserves_y(insn) {
                    return false;
                }
                cur = next;
            }
            Item::Label(_) | Item::Directive(_) | Item::Origin(_) => return false,
            Item::Comment(_) | Item::Blank => {
                unreachable!("next_struct skips non-structural items")
            }
        }
    }
}

fn insn_preserves_y(insn: &Insn) -> bool {
    match insn.mnem.as_str() {
        "LDY" | "TAY" | "INY" | "DEY" => false,
        "JSR" => insn
            .operand
            .as_deref()
            .map_or(false, jsr_target_preserves_y),
        _ => true,
    }
}

fn jsr_target_preserves_y(target: &str) -> bool {
    // GETIN's queued-key path loads Y from $0277 and does not restore it.
    target == "$FFD2" || target.starts_with("__CHROUT_")
}

/// Parse hexadecimal or decimal byte immediates.
fn parse_imm_byte(operand: &str) -> Option<u8> {
    let op = operand.trim();
    let body = op.strip_prefix('#')?;
    if let Some(hex) = body.strip_prefix('$') {
        u8::from_str_radix(hex, 16).ok()
    } else {
        body.parse::<i32>()
            .ok()
            .and_then(|n| u8::try_from(n & 0xFF).ok())
    }
}

/// Remove an immediate FAC reload after storing the same value.
fn ph101_movmf_movfm(items: &mut Vec<Item>) -> bool {
    let mut changed = false;
    let mut i = 0;
    while i + 5 < items.len() {
        if let Some(label) = match_movmf_movfm(&items[i..]) {
            // No label may enter between the store and reload.
            if let Some(next) = next_struct(items, i + 2)
                && next == i + 3
            {
                let _ = label; // already verified equality in matcher
// Drop the MOVFM triplet (items i+3, i+4, i+5).
                items.drain(i + 3..=i + 5);
                changed = true;
                continue;
            }
        }
        i += 1;
    }
    changed
}

/// Remove FAC reloads until the saved value or FAC changes.
fn ph102_fac_movfm_elision(items: &mut Vec<Item>) -> bool {
    let reg_live = analyze_register_liveness_with_config(items, &liveness_config_with_helpers());
    let flag_live = analyze_flag_liveness_with_config(items, &peephole_flag_liveness_config());
    let mut plans: Vec<(usize, usize)> = Vec::new();

    // Stop at control flow boundaries and unknown calls.
    let mut i = 0;
    while i < items.len() {
        let Some((label, movmf_jsr_idx)) = match_movmf_at(items, i) else {
            i += 1;
            continue;
        };
        let mut cur = movmf_jsr_idx;
        loop {
            let Some(next) = next_struct(items, cur) else {
                break;
            };
            if let Some((reload_label, movfm_jsr_idx)) = match_movfm_at(items, next)
                && reload_label == label
            {
                if movfm_outputs_dead(movfm_jsr_idx, &reg_live, &flag_live) {
                    plans.push((next, movfm_jsr_idx));
                }
                break;
            }
            let Some(Item::Insn(insn)) = items.get(next) else {
                break;
            };
            if is_cond_branch(&insn.mnem) || is_uncond_terminator(&insn.mnem) {
                break;
            }
            if !insn_preserves_fac_alias(insn, &label) {
                break;
            }
            cur = next;
        }
        i = movmf_jsr_idx + 1;
    }

    if plans.is_empty() {
        return false;
    }
    plans.sort_unstable_by(|a, b| b.0.cmp(&a.0));
    plans.dedup();
    for (start, end) in plans {
        items.drain(start..=end);
    }
    true
}

fn movfm_outputs_dead(
    jsr_idx: usize,
    reg_live: &RegisterLiveness,
    flag_live: &FlagLiveness,
) -> bool {
    !reg_live.is_live_out(jsr_idx, Reg::A)
        && !reg_live.is_live_out(jsr_idx, Reg::X)
        && !reg_live.is_live_out(jsr_idx, Reg::Y)
        && flag_live.live_out_at(jsr_idx).is_empty()
}

fn insn_preserves_fac_alias(insn: &Insn, label: &str) -> bool {
    if insn_writes_label(insn, label) {
        return false;
    }
    let config = fac_liveness_config_with_helpers();
    let eff = insn_fac_effect_inner(insn, false, &config);
    !eff.defs
}

fn insn_writes_label(insn: &Insn, label: &str) -> bool {
    if !matches!(
        insn.mnem.as_str(),
        "STA" | "STX" | "STY" | "INC" | "DEC" | "ASL" | "LSR" | "ROL" | "ROR"
    ) {
        return false;
    }
    insn.operand
        .as_deref()
        .is_some_and(|op| operand_mentions_label(op, label))
}

fn operand_mentions_label(op: &str, label: &str) -> bool {
    op.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .any(|token| token == label)
}

/// Remove pure FAC loads only when all their outputs are dead.
fn ph150_drop_dead_fac_def(items: &mut Vec<Item>) -> bool {
    let fac_live = analyze_fac_liveness(items);
    let reg_live = analyze_register_liveness_with_config(items, &liveness_config_with_helpers());
    let mut to_drop: Vec<usize> = Vec::new();
    for (idx, item) in items.iter().enumerate() {
        let Item::Insn(insn) = item else { continue };
        if insn.mnem != "JSR" {
            continue;
        }
        let Some(target) = insn.operand.as_deref() else {
            continue;
        };
        if !is_pure_fac_def_target(target) {
            continue;
        }
        // Both FAC and the clobbered registers must be dead.
        if fac_live.is_live_out(idx) {
            continue;
        }
        if reg_live.is_live_out(idx, Reg::A)
            || reg_live.is_live_out(idx, Reg::X)
            || reg_live.is_live_out(idx, Reg::Y)
        {
            continue;
        }
        to_drop.push(idx);
    }
    if to_drop.is_empty() {
        return false;
    }
    for idx in to_drop.into_iter().rev() {
        items.remove(idx);
    }
    true
}

/// Helpers that only set FAC without traps or other side effects.
fn is_pure_fac_def_target(target: &str) -> bool {
    matches!(
        target,
        "$BBA2" | "$B391" | "$B3A2" | "__BOOL_TO_FAC" | "__LD_BYTE_FAC"
    ) || target.starts_with("__LDV_")
}

/// Test the FAC sign with BMI when comparison carry is dead.
fn ph104_fac_sign_cmp_branch(items: &mut Vec<Item>) -> bool {
    let flag_live = analyze_flag_liveness_with_config(items, &peephole_flag_liveness_config());
    let mut plans = Vec::new();
    let mut i = 0;
    while i < items.len() {
        let Some(Item::Insn(lda)) = items.get(i) else {
            i += 1;
            continue;
        };
        if lda.mnem != "LDA" || lda.operand.as_deref() != Some("$66") {
            i += 1;
            continue;
        }
        let Some(cmp_idx) = next_struct(items, i) else {
            break;
        };
        let Some(Item::Insn(cmp)) = items.get(cmp_idx) else {
            i += 1;
            continue;
        };
        if cmp.mnem != "CMP" || !is_zero_immediate(cmp.operand.as_deref()) {
            i += 1;
            continue;
        }
        if flag_live.is_live_out(cmp_idx, Flag::C) {
            i += 1;
            continue;
        }
        let Some(branch_idx) = next_struct(items, cmp_idx) else {
            break;
        };
        let Some(Item::Insn(branch)) = items.get(branch_idx) else {
            i += 1;
            continue;
        };
        if branch.mnem == "BNE" {
            plans.push((cmp_idx, branch_idx));
            i = branch_idx + 1;
        } else {
            i += 1;
        }
    }
    if plans.is_empty() {
        return false;
    }
    for (cmp_idx, branch_idx) in plans.into_iter().rev() {
        if let Some(Item::Insn(branch)) = items.get_mut(branch_idx) {
            branch.mnem = "BMI".to_string();
        }
        items.remove(cmp_idx);
    }
    true
}

fn is_zero_immediate(op: Option<&str>) -> bool {
    matches!(op.map(str::trim), Some("#0" | "#$00" | "#00"))
}

/// Match a FAC store followed by a load of the same slot.
fn match_movmf_movfm(window: &[Item]) -> Option<&str> {
    let a = as_imm_low(window.first()?, "LDX")?;
    let b = as_imm_high(window.get(1)?, "LDY")?;
    let c = as_jsr(window.get(2)?, "$BBD4")?;
    let d = as_imm_low(window.get(3)?, "LDA")?;
    let e = as_imm_high(window.get(4)?, "LDY")?;
    let f = as_jsr(window.get(5)?, "$BBA2")?;
    let _ = (c, f);
    if a == b && b == d && d == e {
        Some(a)
    } else {
        None
    }
}

fn match_movmf_at(items: &[Item], idx: usize) -> Option<(String, usize)> {
    let a = as_imm_low(items.get(idx)?, "LDX")?.to_string();
    let j = next_struct(items, idx)?;
    let b = as_imm_high(items.get(j)?, "LDY")?;
    if b != a {
        return None;
    }
    let k = next_struct(items, j)?;
    as_jsr(items.get(k)?, "$BBD4")?;
    Some((a, k))
}

fn match_movfm_at(items: &[Item], idx: usize) -> Option<(String, usize)> {
    let a = as_imm_low(items.get(idx)?, "LDA")?.to_string();
    let j = next_struct(items, idx)?;
    let b = as_imm_high(items.get(j)?, "LDY")?;
    if b != a {
        return None;
    }
    let k = next_struct(items, j)?;
    as_jsr(items.get(k)?, "$BBA2")?;
    Some((a, k))
}

fn as_imm_low<'a>(item: &'a Item, mnem: &str) -> Option<&'a str> {
    let Item::Insn(insn) = item else { return None };
    if insn.mnem != mnem {
        return None;
    }
    insn.operand.as_deref()?.strip_prefix("#<")
}

fn as_imm_high<'a>(item: &'a Item, mnem: &str) -> Option<&'a str> {
    let Item::Insn(insn) = item else { return None };
    if insn.mnem != mnem {
        return None;
    }
    insn.operand.as_deref()?.strip_prefix("#>")
}

fn as_jsr<'a>(item: &'a Item, target: &str) -> Option<&'a str> {
    let Item::Insn(insn) = item else { return None };
    if insn.mnem != "JSR" {
        return None;
    }
    let op = insn.operand.as_deref()?;
    if op == target { Some(op) } else { None }
}

// ----- Loop-index register promotion ---------------------------------------

/// Known ROM register inputs. Unknown calls read every register.
fn rom_call_effect(operand: &str) -> Option<RegEffect> {
    let addr = operand
        .strip_prefix('$')
        .and_then(|h| u16::from_str_radix(h, 16).ok())?;
    use crate::runtime as rt;

    let uses_x = addr == rt::MOVMF
        || addr == rt::PLOT
        || addr == rt::SETLFS
        || addr == rt::SETNAM
        || addr == rt::CHKIN
        || addr == rt::CHKOUT
        || addr == rt::KERNAL_LOAD
        || addr == rt::KERNAL_SAVE
        || addr == rt::BASIC_ERROR;
    // Known routines that do not read X.
    let known_no_x = matches!(
        addr,
        rt::CHROUT
            | rt::PRINT_GAP
            | rt::GETIN
            | rt::STOP
            | rt::CHRIN
            | rt::RDTIM
            | rt::KERNAL_OPEN
            | rt::KERNAL_CLOSE
            | rt::CLRCHN
            | rt::STROUT
            | rt::GIVAYF
            | rt::MOVFM
            | rt::FADD
            | rt::FSUB
            | rt::FMULT
            | rt::FDIV
            | rt::FPWRT
            | rt::FACARG
            | rt::FCOMP
            | rt::FOUT
            | rt::FN_ABS
            | rt::FN_INT
            | rt::FN_SGN
            | rt::FN_SQR
            | rt::FN_SIN
            | rt::FN_COS
            | rt::FN_TAN
            | rt::FN_ATN
            | rt::FN_LOG
            | rt::FN_EXP
            | rt::FN_RND
            | rt::FACWORD
            | rt::BYTEFAC
            | rt::VAL_PARSE
            | rt::ERRSYN
            | rt::BASIC_WARM_START
            | rt::QINT
    );
    // Record routines that return a value in X.
    let defs_x = addr == rt::RDTIM; // returns the jiffy clock in .A/.X/.Y
    if uses_x {
        // Keep all registers live when the routine reads X.
        Some(RegEffect::new(RegSet::ALL, RegSet::EMPTY))
    } else if known_no_x {
        // Keep A and Y live for routines that do not read X.
        let defs = if defs_x { RegSet::X } else { RegSet::EMPTY };
        Some(RegEffect::new(RegSet::A.union(RegSet::Y), defs))
    } else {
        None // unknown ROM address -> caller treats it conservatively
    }
}

/// Derive helper register inputs from their bodies and known ROM calls.
fn build_precise_call_config(items: &[Item]) -> RegisterLivenessConfig {
    let labels = register_liveness_label_map(items);

    // Seed known ROM inputs and grow helper inputs through the call graph.
    let mut known: std::collections::HashMap<String, RegEffect> = std::collections::HashMap::new();

    // Compute helper inputs until their effects stop changing.
    for _ in 0..16 {
        let mut config = RegisterLivenessConfig {
            default_jsr: RegEffect::new(RegSet::ALL, RegSet::EMPTY),
            known_jsr: known.clone(),
            known_jsr_prefix: Vec::new(),
            external_jmp_uses: RegSet::ALL,
            return_live_out: RegSet::EMPTY,
        };
        // Include ROM calls by their operand text.
        for item in items {
            if let Item::Insn(insn) = item
                && matches!(insn.mnem.as_str(), "JSR" | "JMP")
                && let Some(op) = insn.operand.as_deref()
                && !config.known_jsr.contains_key(op)
                && let Some(eff) = rom_call_effect(op)
            {
                config.known_jsr.insert(op.to_string(), eff);
            }
        }
        let liveness = analyze_register_liveness_with_config(items, &config);

        let mut changed = false;
        for (name, &idx) in &labels {
            // Only labels that are actually called matter.
            let uses = liveness.live_in_at(idx);
            let eff = RegEffect::new(uses, RegSet::EMPTY);
            match known.get(name) {
                Some(prev) if prev.uses == uses => {}
                _ => {
                    known.insert(name.clone(), eff);
                    changed = true;
                }
            }
        }
        // Re-seed ROM each round (cheap) and stop when helper uses settle.
        if !changed {
            break;
        }
    }

    // Use derived effects and keep unknown calls conservative.
    let mut config = RegisterLivenessConfig {
        default_jsr: RegEffect::new(RegSet::ALL, RegSet::EMPTY),
        known_jsr: known,
        known_jsr_prefix: Vec::new(),
        external_jmp_uses: RegSet::ALL,
        return_live_out: RegSet::EMPTY,
    };
    for item in items {
        if let Item::Insn(insn) = item
            && matches!(insn.mnem.as_str(), "JSR" | "JMP")
            && let Some(op) = insn.operand.as_deref()
            && !config.known_jsr.contains_key(op)
            && let Some(eff) = rom_call_effect(op)
        {
            config.known_jsr.insert(op.to_string(), eff);
        }
    }
    config
}

/// Does `operand` mention `var` as a whole identifier token?
fn operand_mentions_var(operand: &str, var: &str) -> bool {
    let bytes = operand.as_bytes();
    let vb = var.as_bytes();
    let is_ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut i = 0;
    while i + vb.len() <= bytes.len() {
        if &bytes[i..i + vb.len()] == vb {
            let before_ok = i == 0 || !is_ident(bytes[i - 1]);
            let after = i + vb.len();
            let after_ok = after == bytes.len() || !is_ident(bytes[after]);
            if before_ok && after_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

fn next_insn_idx(items: &[Item], from: usize) -> Option<usize> {
    (from..items.len()).find(|&k| matches!(items[k], Item::Insn(_)))
}

/// Keep counting loop indices in X when calls and exits preserve it.
fn ph210_loop_index_to_x(items: &mut Vec<Item>) -> bool {
    let labels = register_liveness_label_map(items);
    let config = build_precise_call_config(items);
    let liveness = analyze_register_liveness_with_config(items, &config);

    for j in 0..items.len() {
        let Item::Insn(jmp) = &items[j] else { continue };
        let conditional_back_edge = is_cond_branch(&jmp.mnem);
        if jmp.mnem != "JMP" && !conditional_back_edge {
            continue;
        }
        let Some(head) = jmp.operand.clone() else {
            continue;
        };
        let Some(&h) = labels.get(&head) else {
            continue;
        };
        if h >= j || h == 0 {
            continue;
        }
        if let Item::Insn(prev) = &items[h - 1]
            && is_uncond_terminator(&prev.mnem)
        {
            continue;
        }

        // Require one back edge and no internal calls or extra jumps.
        let mut region_ok = true;
        for (k, it) in items.iter().enumerate().take(j).skip(h) {
            match it {
                Item::Directive(_) => region_ok = false,
                Item::Insn(ins) => {
                    if matches!(ins.mnem.as_str(), "JSR" | "RTS" | "RTI" | "BRK")
                        || (ins.mnem == "JMP" && k != j)
                    {
                        region_ok = false;
                    }
                }
                _ => {}
            }
            if !region_ok {
                break;
            }
        }
        if !region_ok {
            continue;
        }
        // Internal labels must have no incoming control flow from outside.
        let internal: std::collections::HashSet<&str> = (h + 1..=j)
            .filter_map(|k| match &items[k] {
                Item::Label(l) => Some(l.as_str()),
                _ => None,
            })
            .collect();
        let mut head_refs = 0usize;
        let mut bad_entry = false;
        for (k, it) in items.iter().enumerate() {
            let Item::Insn(ins) = it else { continue };
            let Some(op) = ins.operand.as_deref() else {
                continue;
            };
            if op == head {
                head_refs += 1;
            }
            if internal.contains(op) && !(h..=j).contains(&k) {
                bad_entry = true;
            }
        }
        if head_refs != 1 || bad_entry {
            continue;
        }

        // Require one increment and only index or comparison reads.
        let mut inc_var: Option<String> = None;
        let mut multi = false;
        for it in items.iter().take(j + 1).skip(h) {
            if let Item::Insn(ins) = it
                && ins.mnem == "INC"
                && let Some(op) = ins.operand.clone()
            {
                if inc_var.is_some() {
                    multi = true;
                }
                inc_var = Some(op);
            }
        }
        let Some(v) = inc_var else { continue };
        if multi {
            continue;
        }

        let mut v_ok = true;
        let mut lda_drop: std::collections::HashSet<usize> = std::collections::HashSet::new();
        for k in h..=j {
            let Item::Insn(ins) = &items[k] else { continue };
            let mentions = ins
                .operand
                .as_deref()
                .is_some_and(|op| operand_mentions_var(op, &v));
            if !mentions {
                continue;
            }
            let exact = ins.operand.as_deref() == Some(v.as_str());
            match ins.mnem.as_str() {
                "INC" | "LDX" if exact => {}
                "LDA" if exact => {
                    let Some(ci) = next_insn_idx(items, k + 1) else {
                        v_ok = false;
                        break;
                    };
                    let Item::Insn(cmp) = &items[ci] else {
                        v_ok = false;
                        break;
                    };
                    if cmp.mnem != "CMP" || liveness.is_live_out(ci, Reg::A) {
                        v_ok = false;
                        break;
                    }
                    lda_drop.insert(k);
                }
                _ => {
                    v_ok = false;
                    break;
                }
            }
        }
        if !v_ok {
            continue;
        }

        // X must be free at entry and dead on every exit.
        if liveness.is_live_in(h, Reg::X) {
            continue;
        }
        if conditional_back_edge && liveness.is_live_out(j, Reg::X) {
            continue;
        }
        let mut x_ok = true;
        for k in h..=j {
            let Item::Insn(ins) = &items[k] else { continue };
            // Internal branches do not read data registers.
            let is_branch = ins.mnem == "JMP" || is_cond_branch(&ins.mnem);
            let eff = insn_register_effect(ins, &RegisterLivenessConfig::default());
            if !is_branch
                && eff.defs.contains(Reg::X)
                && !(ins.mnem == "LDX" && ins.operand.as_deref() == Some(v.as_str()))
            {
                x_ok = false;
                break;
            }
            if !is_branch
                && eff.uses.contains(Reg::X)
                && !ins
                    .operand
                    .as_deref()
                    .is_some_and(|op| op.to_uppercase().contains(",X"))
            {
                x_ok = false;
                break;
            }
            if is_cond_branch(&ins.mnem)
                && let Some(t) = ins.operand.as_deref()
                && let Some(&ti) = labels.get(t)
                && !(h..=j).contains(&ti)
                && liveness.is_live_in(ti, Reg::X)
            {
                x_ok = false;
                break;
            }
        }
        if !x_ok {
            continue;
        }

        // Omit the counter store only when no outside read needs it and entry initializes it.
        let v_read_outside = items.iter().enumerate().any(|(k, it)| {
            if (h..=j).contains(&k) {
                return false;
            }
            let Item::Insn(ins) = it else { return false };
            ins.operand
                .as_deref()
                .is_some_and(|op| operand_mentions_var(op, &v))
                && !matches!(ins.mnem.as_str(), "STA" | "STX" | "STY")
        });
        let entry_writes_v = (0..h)
            .rev()
            .find(|&p| matches!(items[p], Item::Insn(_)))
            .and_then(|p| match &items[p] {
                Item::Insn(ins) => Some(ins),
                _ => None,
            })
            .is_some_and(|ins| ins.mnem == "STA" && ins.operand.as_deref() == Some(v.as_str()));
        let needs_sync = v_read_outside || !entry_writes_v;

        // Rewrite.
        let mut out: Vec<Item> = Vec::with_capacity(items.len() + 2);
        for (k, it) in items.iter().enumerate() {
            if k == h {
                out.push(Item::Insn(Insn {
                    mnem: "LDX".to_string(),
                    operand: Some(v.clone()),
                    comment: Some("loop index -> X".to_string()),
                }));
            }
            if (h..=j).contains(&k)
                && let Item::Insn(ins) = it
            {
                if ins.mnem == "INC" && ins.operand.as_deref() == Some(v.as_str()) {
                    out.push(Item::Insn(Insn {
                        mnem: "INX".to_string(),
                        operand: None,
                        comment: ins.comment.clone(),
                    }));
                    if needs_sync {
                        out.push(Item::Insn(Insn {
                            mnem: "STX".to_string(),
                            operand: Some(v.clone()),
                            comment: None,
                        }));
                    }
                    continue;
                }
                if ins.mnem == "LDX" && ins.operand.as_deref() == Some(v.as_str()) {
                    continue;
                }
                if lda_drop.contains(&k) {
                    continue;
                }
                if ins.mnem == "CMP"
                    && (0..k)
                        .rev()
                        .find(|&p| matches!(items[p], Item::Insn(_)))
                        .is_some_and(|p| lda_drop.contains(&p))
                {
                    out.push(Item::Insn(Insn {
                        mnem: "CPX".to_string(),
                        operand: ins.operand.clone(),
                        comment: ins.comment.clone(),
                    }));
                    continue;
                }
            }
            out.push(it.clone());
        }
        *items = out;
        return true;
    }
    false
}

// ----- Driver --------------------------------------------------------------

fn late_branch_fixpoint(items: &mut Vec<Item>) -> bool {
    let mut any = false;
    for _ in 0..8 {
        let mut changed = false;
        changed |= ph005_branch_around_jmp(items);
        changed |= ph315_branch_over_jmp_past_dead_labels(items);
        changed |= ph004_jump_cascade(items);
        changed |= ph029_drop_dead_local_labels(items);
        changed |= ph001_dead_jump_to_next(items);
        changed |= ph002_dead_after_terminator(items);
        changed |= ph003_jsr_rts_to_jmp(items);
        if !changed {
            break;
        }
        any = true;
    }
    any
}

/// Options for additional sequence sharing.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// Try additional factoring windows when the ordinary size layout cannot fit.
    pub aggressive_size_dedup: bool,
    /// Additional closed-region factoring for a final failed-layout rescue.
    pub compact_size_dedup: bool,
    /// Allow inline string-print arguments only when the image has no reserved ranges.
    pub inline_str_print_args: bool,
}

/// Run with the default rewrite options.
pub fn run(asm: &str, profile: Profile) -> String {
    run_with_options(asm, profile, Options::default())
}

pub fn run_with_options(asm: &str, profile: Profile, opts: Options) -> String {
    if profile == Profile::Size && opts.aggressive_size_dedup {
        // The late factoring/economics stages expose new signatures.
        let first = run_once_with_options(
            asm,
            profile,
            Options {
                inline_str_print_args: false,
                ..opts
            },
        );
        run_once_with_options(&first, profile, opts)
    } else {
        run_once_with_options(asm, profile, opts)
    }
}

fn run_once_with_options(asm: &str, profile: Profile, opts: Options) -> String {
    let mut items = parse(asm);
    canonicalize_zp_aliases(&mut items);
    if profile == Profile::Size && opts.compact_size_dedup {
        zp_stores::compact(&mut items);
    }
    // Bound the number of optimization rounds.
    for _ in 0..8 {
        let mut changed = false;
        changed |= ph101_movmf_movfm(&mut items);
        changed |= ph102_fac_movfm_elision(&mut items);
        changed |= ph150_drop_dead_fac_def(&mut items);
        changed |= ph104_fac_sign_cmp_branch(&mut items);
        changed |= ph005_branch_around_jmp(&mut items);
        changed |= ph315_branch_over_jmp_past_dead_labels(&mut items);
        changed |= ph004_jump_cascade(&mut items);
        changed |= ph029_drop_dead_local_labels(&mut items); // Threading can leave unreferenced labels.
        changed |= ph001_dead_jump_to_next(&mut items);
        changed |= ph002_dead_after_terminator(&mut items);
        changed |= ph003_jsr_rts_to_jmp(&mut items);
        changed |= ph030_swap_indep_store_pairs(&mut items); // Exposes load forwarding.
        changed |= ph031_drop_overwritten_store(&mut items); // Can leave unused loads.
        changed |= ph008_store_load_forward(&mut items);
        changed |= ph009_load_store_identity(&mut items);
        changed |= ph020_static_known_value_cleanup(&mut items);
        // Fold immediate load branches before the broader flag pass.
        changed |= ph207_const_load_branch_fold(&mut items);
        changed |= ph021_constant_branch_fold(&mut items);
        changed |= ph022_jmp_to_rts(&mut items);
        changed |= ph316_sec_lda_sbc_to_cmp(&mut items);
        changed |= ph024_drop_dead_cmp(&mut items);
        changed |= ph025_drop_redundant_store(&mut items);
        changed |= ph028_drop_lda_after_inc_dec(&mut items);
        changed |= ph026_simplify_alu_imm(&mut items);
        changed |= ph012_drop_cmp_zero(&mut items);
        // Repeat liveness rules as rewrites expose dead operations.
        changed |= ph019_lda_to_index_load(&mut items);
        changed |= ph015_drop_dead_transfer(&mut items);
        changed |= ph017_drop_dead_load(&mut items);
        // Remove loads whose values and flags are dead globally.
        changed |= ph317_drop_dead_load_via_flag_liveness(&mut items);
        changed |= ph303_drop_unreferenced_trampolines(&mut items);
        changed |= ph013_drop_nop(&mut items);
        changed |= ph204_and_signbit_to_branch(&mut items);
        changed |= ph205_mask_identity(&mut items);
        changed |= ph206_pha_pla_roundtrip(&mut items);

        changed |= ph209_spill_pha_pla_pair(&mut items);

        changed |= ph208_hoist_ldy_zero_from_loop(&mut items);
        changed |= ph210_loop_index_to_x(&mut items);

        changed |= ph304_merge_identical_blocks(&mut items);

        changed |= ph306_curlin_known(&mut items, u16::from(crate::runtime::CURLIN_HI));

        changed |= ph307_dead_curlin_stamp(&mut items);
        if !changed {
            break;
        }
    }

    ph106_factor_int_byte_to_fac(&mut items);
    ph109_factor_per_imm_byte_fac(&mut items);
    ph107_factor_chrout_byte(&mut items);
    ph111_factor_lda_ldy_jsr(&mut items);
    ph112_factor_varptr_jsr(&mut items);

    // Factor triples before overlapping pairs.
    if ph113_factor_jsr_triple(&mut items) {
        for _ in 0..8 {
            if !ph110_factor_jsr_pair(&mut items) {
                break;
            }
        }
    }

    for _ in 0..16 {
        if !ph110_factor_jsr_pair(&mut items) {
            break;
        }
    }

    for _ in 0..8 {
        if !ph301_factor_late_with_jsr(&mut items, profile, opts.aggressive_size_dedup) {
            break;
        }
        for _ in 0..8 {
            if !ph110_factor_jsr_pair(&mut items) {
                break;
            }
        }
    }
    // Common-tail sinking.
    {
        let min_gain = if profile == Profile::Size { 1 } else { 3 };
        for _ in 0..64 {
            if !ph305_sink_common_tails(&mut items, min_gain) {
                break;
            }
        }
    }
    // Remove helpers with no references after factoring.
    ph303_drop_unreferenced_trampolines(&mut items);

    for _ in 0..64 {
        if !ph302_inline_single_call_helpers(&mut items) {
            break;
        }
    }
    for _ in 0..64 {
        if !ph308_inline_uneconomic_helpers(&mut items) {
            break;
        }
    }
    ph303_drop_unreferenced_trampolines(&mut items);

    late_branch_fixpoint(&mut items);
    ph303_drop_unreferenced_trampolines(&mut items);

    for _ in 0..64 {
        if !ph302_inline_single_call_helpers(&mut items) {
            break;
        }
    }
    for _ in 0..64 {
        if !ph308_inline_uneconomic_helpers(&mut items) {
            break;
        }
    }
    ph303_drop_unreferenced_trampolines(&mut items);

    for _ in 0..4 {
        let mut changed = false;
        changed |= ph211_transfer_store_to_index_store(&mut items);
        changed |= ph212_lda_asl_tax_to_ldx(&mut items);
        changed |= ph213_imm_load_to_bump(&mut items);
        changed |= ph214_and_bit0_to_lsr_carry(&mut items);
        if !changed {
            break;
        }
    }

    for _ in 0..4 {
        let mut changed = false;
        changed |= ph309_absorb_common_pred(&mut items);
        changed |= ph310_absorb_common_succ(&mut items);
        changed |= ph311_merge_duplicate_helpers(&mut items);
        changed |= ph312_merge_helper_tails(&mut items);
        for _ in 0..64 {
            if !ph302_inline_single_call_helpers(&mut items) {
                break;
            }
            changed = true;
        }
        for _ in 0..64 {
            if !ph308_inline_uneconomic_helpers(&mut items) {
                break;
            }
            changed = true;
        }

        ph303_drop_unreferenced_trampolines(&mut items);
        if !changed {
            break;
        }
    }

    // Fold carry after factoring so fresh labels do not prevent sharing.
    ph319_carry_fold(&mut items);
    ph320_merge_inc_runs(&mut items);
    // Rewrite stores and pointer reads after shared sequences are settled.
    ph321_store_forward_sink(&mut items);
    ph322_const_deref_to_abs(&mut items);
    ph323_const_index_store_to_abs(&mut items);
    // Forward values after helper extraction and inlining.
    for _ in 0..4 {
        let mut changed = false;
        changed |= ph324_hoist_reload_to_transfer(&mut items);
        changed |= ph020_known_value_cleanup(&mut items, false);
        changed |= ph008_store_load_forward(&mut items);
        changed |= ph025_drop_redundant_store(&mut items);
        changed |= ph015_drop_dead_transfer(&mut items);
        changed |= ph017_drop_dead_load(&mut items);
        changed |= ph317_drop_dead_load_via_flag_liveness(&mut items);
        // Remove adjacent jumps before dead block removal.
        changed |= late_branch_fixpoint(&mut items);
        if !changed {
            break;
        }
    }
    ph328_thread_calls_through_trampolines(&mut items);
    ph303_drop_unreferenced_trampolines(&mut items);

    for _ in 0..8 {
        if !ph306_drop_unreachable_code(&mut items) {
            break;
        }
        ph303_drop_unreferenced_trampolines(&mut items);
    }
    if profile != Profile::Speed {
        ph325_branch_via_shared_jmp(&mut items);
    }
    // Keep trap jumps after dead block removal.
    ensure_referenced_codegen_helpers(&mut items);
    if profile == Profile::Size && opts.compact_size_dedup {
        structured::factor(&mut items);
        short_pairs::compact(&mut items);
    }

    ph327_split_print_runs(&mut items);
    if opts.inline_str_print_args {
        ph114_str_print_inline_args(&mut items);
    }
    ph326_absorb_shared_call_prefix(&mut items);
    ph330_drop_add_of_zero_with_known_carry(&mut items);
    ph329_branch_straight_to_the_jmp_target(&mut items);
    render(&items)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_default(input: &str) -> String {
        run(input, Profile::Default)
    }

    // shared helper anchor

    #[test]
    fn the_anchor_prefers_bss_start_over_heap_bottom() {
        let items = parse("    RTS\n__BSS_START:\n    .byte $00\n__HEAP_BOTTOM:\n");
        let idx = helper_anchor_index(&items).expect("an anchor exists");
        assert!(matches!(&items[idx], Item::Label(n) if n == "__BSS_START"));
    }

    #[test]
    fn the_anchor_falls_back_to_heap_bottom() {
        let items = parse("    RTS\n__HEAP_BOTTOM:\n");
        let idx = helper_anchor_index(&items).expect("an anchor exists");
        assert!(matches!(&items[idx], Item::Label(n) if n == "__HEAP_BOTTOM"));
    }

    fn ph329(src: &str) -> String {
        let mut items = parse(src);
        ph329_branch_straight_to_the_jmp_target(&mut items);
        render(&items)
    }

    #[test]
    fn a_label_one_forward_branch_enters_inherits_its_facts() {
        let items = parse(concat!(
            "    LDA $10\n    CMP #$01\n    BNE NEXT\n    JSR $FFD2\n    JMP OUT\n",
            "NEXT:\n    LDA $10\n    RTS\nOUT:\n    RTS\n"
        ));
        let entries = labels_entered_by_one_forward_branch(&items);
        let next = items.iter().position(|i| matches!(i, Item::Label(n) if n == "NEXT")).unwrap();
        assert!(entries.contains_key(&next));
        let facts = analyze_static_asm_facts(&items);
        let reload = next + 1;
        let Item::Insn(insn) = &items[reload] else { panic!() };
        assert!(reg_fact_matches_load(&facts[reload], Reg::A, insn), "A still holds $10");
    }

    #[test]
    fn a_label_reached_otherwise_starts_from_nothing() {
        for src in [
            // Falls in from above.
            "    LDA $10\n    BNE NEXT\n    LDX #$00\nNEXT:\n    LDA $10\n    RTS\n",
            // A second way in.
            "    LDA $10\n    BNE NEXT\n    JMP NEXT\nNEXT:\n    LDA $10\n    RTS\n",
            // Shares its address with another label.
            "    LDA $10\n    BNE NEXT\n    RTS\nNEXT:\nOTHER:\n    LDA $10\n    RTS\n",
            // Named in data.
            "    LDA $10\n    BNE NEXT\n    RTS\nNEXT:\n    LDA $10\n    RTS\n    .word NEXT\n",
            // Entered from below.
            "    RTS\nNEXT:\n    LDA $10\n    RTS\n    LDA $10\n    BNE NEXT\n    RTS\n",
        ] {
            assert!(labels_entered_by_one_forward_branch(&parse(src)).is_empty(), "{src}");
        }
    }

    fn canon(src: &str) -> String {
        let mut items = parse(src);
        canonicalize_zp_aliases(&mut items);
        render(&items)
    }

    #[test]
    fn zp_aliases_of_one_kind_take_the_first_name() {
        let out = canon(
            "VI_J = $A3\nVI_YF = $A3\nVI_K = $A5\n    STA VI_YF\n    LDA VI_YF+1\n    LDA (VI_YF),Y\n    STA VI_K\n    LDA #<VI_YF\n",
        );
        assert!(!out.contains("    STA VI_YF") && !out.contains("VI_YF+1") && !out.contains("(VI_YF)"), "{out}");
        assert!(out.contains("STA VI_J\n") && out.contains("LDA VI_J+1") && out.contains("LDA (VI_J),Y"), "{out}");
        assert!(out.contains("STA VI_K") && out.contains("LDA #<VI_J"), "{out}");
        // The equates themselves stay.
        assert!(out.contains("VI_YF = $A3"), "{out}");
    }

    #[test]
    fn zp_aliases_of_different_kinds_keep_their_names() {
        let src = "VI_J = $A3\nZSI_V_K = $A3\n    STA VI_J\n    STA ZSI_V_K\n";
        assert_eq!(canon(src), render(&parse(src)));
    }

    #[test]
    fn hex_and_non_zp_operands_are_left_alone() {
        let src = "FU_0 = $02\nFU_1 = $02\n    LDA $FU1\n    STA __FU_1_X\n    JSR FU_1X\n";
        assert_eq!(canon(src), render(&parse(src)));
    }

    fn ph330(src: &str) -> String {
        let mut items = parse(src);
        ph330_drop_add_of_zero_with_known_carry(&mut items);
        render(&items)
    }

    #[test]
    fn ph330_drops_an_add_of_zero_after_clc_and_a_subtract_after_sec() {
        let out = ph330("    CLC\n    LDA $10\n    ADC #$00\n    STA $20\n    LDA $11\n    ADC #$C0\n    STA $21\n    RTS\n");
        assert!(!out.contains("ADC #$00") && out.contains("ADC #$C0"), "{out}");
        let out = ph330("    SEC\n    LDA $10\n    SBC #$00\n    STA $20\n    LDA $11\n    SBC #$01\n    STA $21\n    RTS\n");
        assert!(!out.contains("SBC #$00"), "{out}");
    }

    #[test]
    fn ph330_keeps_an_add_whose_flags_are_read() {
        let src = "    CLC\n    LDA $10\n    LDX $11\n    ADC #$00\n    BEQ Z\n    RTS\nZ:\n    RTS\n";
        assert!(ph330(src).contains("ADC #$00"));
        let src = "    CLV\n    SEC\n    LDA $10\n    BIT $12\n    SBC #$00\n    STA $20\n    BVS O\n    RTS\nO:\n    RTS\n";
        assert!(ph330(src).contains("SBC #$00"));
    }

    #[test]
    fn ph330_keeps_an_add_whose_carry_is_not_known() {
        for src in [
            // A shift in between writes C.
            "    CLC\n    LDA $10\n    ASL\n    ADC #$00\n    STA $20\n    RTS\n",
            "    CLC\n    LDA $10\nIN:\n    ADC #$00\n    STA $20\n    RTS\n",
            // The wrong one of CLC/SEC.
            "    SEC\n    LDA $10\n    ADC #$00\n    STA $20\n    RTS\n",
        ] {
            assert!(ph330(src).contains("ADC #$00"), "{src}");
        }
    }

    #[test]
    fn ph329_branches_straight_to_the_jmp_target() {
        let out = ph329("    LDA $10
    BEQ SKIP
    JMP FAR
SKIP:
    RTS
FAR:
    RTS
");
        assert!(out.contains("BNE FAR") && !out.contains("JMP FAR"), "{out}");
        // The label stays for anything else that reaches it.
        assert!(out.contains("SKIP:"), "{out}");
    }

    #[test]
    fn ph329_finds_the_branch_target_among_the_labels_after_the_jmp() {
        let out = ph329("    BCC B
    JMP FAR
A:
B:
    RTS
FAR:
    RTS
");
        assert!(out.contains("BCS FAR") && !out.contains("JMP"), "{out}");
    }

    #[test]
    fn ph329_leaves_a_jmp_that_is_itself_a_target() {
        let out = ph329("    BEQ SKIP
HERE:
    JMP FAR
SKIP:
    RTS
FAR:
    RTS
");
        assert!(out.contains("JMP FAR") && out.contains("BEQ SKIP"), "{out}");
    }

    #[test]
    fn ph329_leaves_rom_indirect_and_unrelated_jumps() {
        for jmp in ["JMP $A437", "JMP ($22)"] {
            let src = format!("    BEQ SKIP
    {jmp}
SKIP:
    RTS
");
            assert_eq!(ph329(&src), render(&parse(&src)));
        }
        // The branch goes somewhere other than past the JMP.
        let src = "    BEQ ELSE
    JMP FAR
SKIP:
    RTS
ELSE:
FAR:
    RTS
";
        assert_eq!(ph329(src), render(&parse(src)));
    }

    #[test]
    fn a_program_with_neither_anchor_appends() {
        let mut items = parse("    RTS\n");
        assert!(helper_anchor_index(&items).is_none());
        append_helpers_before_heap(&mut items, vec![Item::Label("__H".to_string())]);
        assert!(matches!(items.last(), Some(Item::Label(n)) if n == "__H"));
    }

    #[test]
    fn two_splices_both_land_before_the_anchor() {
        let mut items = parse("    RTS\n__BSS_START:\n    .byte $00\n__HEAP_BOTTOM:\n");
        append_helpers_before_heap(&mut items, vec![Item::Label("__A".to_string())]);
        append_helpers_before_heap(&mut items, vec![Item::Label("__B".to_string())]);
        let pos = |name: &str| {
            items
                .iter()
                .position(|it| matches!(it, Item::Label(n) if n == name))
                .unwrap_or_else(|| panic!("{name} present"))
        };
        assert!(pos("__A") < pos("__BSS_START"));
        assert!(pos("__B") < pos("__BSS_START"));
        assert!(pos("__A") < pos("__B"), "splices keep their relative order");
    }

    fn str_print_program(sites: usize) -> String {
        let mut s = String::from("*=$080D\n");
        for i in 0..sites {
            s.push_str(&format!(
                "    LDA #<S{i}\n    LDY #>S{i}\n    JSR __STR_PRINT\n"
            ));
        }
        s.push_str(
            "    RTS\n__STR_PRINT:\n    STA $FD\n    STY $FE\n__STR_PRINT_PTR:\n    \
             LDY #$00\n    RTS\n__HEAP_BOTTOM:\n",
        );
        s
    }

    #[test]
    fn ph114_rewrites_every_site_and_emits_one_trampoline() {
        let mut items = parse(&str_print_program(20));
        assert!(ph114_str_print_inline_args(&mut items));
        let out = render(&items);
        assert_eq!(out.matches("JSR __STR_PRINT_I").count(), 20);
        assert_eq!(out.matches("__STR_PRINT_I:").count(), 1);
        assert_eq!(out.matches("    .word S").count(), 20);
        assert!(!out.contains("LDA #<S0"), "the triple must be gone");
        // Every `.word` immediately follows its own call.
        for (a, b) in out.lines().zip(out.lines().skip(1)) {
            if a.trim() == "JSR __STR_PRINT_I" {
                assert!(
                    b.trim().starts_with(".word S"),
                    "argument must be the next line, got `{b}`"
                );
            }
        }
    }

    #[test]
    fn size_rescue_repeats_optimization_before_inline_print_arguments() {
        // Keep inline data out of the intermediate instruction passes.
        let mut input = String::from("*=$2000\n");
        for i in 0..20 {
            input.push_str(&format!(
                "L{i}:\n    INC $0404\n    INC $0405\n    LDA #<S{i}\n    LDY #>S{i}\n    JSR __STR_PRINT\n"
            ));
        }
        input.push_str(concat!(
            "    JMP $FFFE\n__STR_PRINT:\n    STA $FD\n    STY $FE\n",
            "__STR_PRINT_PTR:\n    LDY #$00\n__PRINT_LOOP:\n    LDA ($FD),Y\n",
            "    BEQ __PRINT_DONE\n    JSR $FFD2\n    INY\n    BNE __PRINT_LOOP\n",
            "__PRINT_DONE:\n    RTS\n",
        ));
        for i in 0..20 {
            input.push_str(&format!("S{i}:\n    .byte ${:02X},$00\n", 65 + i));
            // Keep each block boundary address-taken so factoring cannot consume the next block's two INCs first.
            input.push_str(&format!("    .word L{i}\n"));
        }
        input.push_str("__HEAP_BOTTOM:\n");
        let outlined = run_with_options(
            &input,
            Profile::Size,
            Options {
                aggressive_size_dedup: true,
                inline_str_print_args: true,
                ..Options::default()
            },
        );
        assert_eq!(outlined.matches("    .word S").count(), 20, "{outlined}");
        let bytes = asm6502::Assembler6502::new().assemble_bytes(&outlined).unwrap();
        let mut system = emu64::System::new();
        system.mem.ram[0x2000..0x2000 + bytes.len()].copy_from_slice(&bytes);
        system.mem.ram[0x404] = 0;
        system.mem.ram[0x405] = 0;
        system.start_at(0x2000);
        system.cpu.regs.sp = 0xf0;
        for _ in 0..10_000 {
            if system.cpu.regs.pc == 0xfffe {
                break;
            }
            assert!(system.step());
        }
        assert_eq!(system.cpu.regs.pc, 0xfffe);
        assert_eq!(system.cpu.regs.sp, 0xf0);
        assert_eq!(system.output, (65..85).collect::<Vec<u8>>());
        assert_eq!(&system.mem.ram[0x404..0x406], &[20, 20]);
    }

    #[test]
    fn a_partial_undo_restores_only_the_named_sites() {
        let mut items = parse(&str_print_program(20));
        assert!(ph114_str_print_inline_args(&mut items));
        let inline = render(&items);
        let sites: std::collections::HashSet<usize> = [0, 7].into_iter().collect();
        let partial = undo_str_print_inline_args_at(&inline, Some(&sites));
        assert_eq!(partial.matches("JSR __STR_PRINT_I\n").count(), 18);
        assert!(partial.contains("    LDA #<S0\n    LDY #>S0\n    JSR __STR_PRINT\n"));
        assert!(partial.contains("    LDA #<S7\n    LDY #>S7\n    JSR __STR_PRINT\n"));
        assert!(!partial.contains(".word S0\n") && !partial.contains(".word S7\n"));
        assert!(partial.contains("__STR_PRINT_I:"), "18 sites still use the trampoline");
        // Naming every site is the full undo, trampoline included.
        let all: std::collections::HashSet<usize> = (0..20).collect();
        assert_eq!(
            undo_str_print_inline_args_at(&inline, Some(&all)),
            undo_str_print_inline_args(&inline)
        );
        assert!(!undo_str_print_inline_args(&inline).contains("__STR_PRINT_I"));
    }

    #[test]
    fn an_undo_puts_a_shared_stamp_back_in_front_of_its_site() {
        let program = str_print_program(20).replace(
            "    LDA #<S",
            "    INC $39\n    LDA #<S",
        );
        let mut items = parse(&program);
        assert!(ph114_str_print_inline_args(&mut items));
        assert!(ph326_absorb_shared_call_prefix(&mut items));
        let inline = render(&items);
        assert_eq!(inline.matches("    INC $39\n").count(), 1, "{inline}");
        let entries = str_print_inline_entry_names(&inline);
        assert_eq!(entries.len(), 1, "{inline}");
        let entry = entries.iter().next().unwrap();
        assert_eq!(inline.matches(&format!("JSR {entry}\n    .word S")).count(), 20, "{inline}");

        let sites: std::collections::HashSet<usize> = [3].into_iter().collect();
        let partial = undo_str_print_inline_args_at(&inline, Some(&sites));
        assert!(
            partial.contains("    INC $39\n    LDA #<S3\n    LDY #>S3\n    JSR __STR_PRINT\n"),
            "the undone site runs its stamp again:\n{partial}"
        );
        assert!(partial.contains(&format!("{entry}:\n    INC $39\n__STR_PRINT_I:")), "{partial}");

        let plain = undo_str_print_inline_args(&inline);
        assert!(!plain.contains("__STR_PRINT_I") && !plain.contains(entry.as_str()), "{plain}");
        assert_eq!(plain.matches("    INC $39\n    LDA #<S").count(), 20, "{plain}");
        let pool: String = (0..20).map(|i| format!("S{i}:\n    .byte $00\n")).collect();
        asm6502::Assembler6502::new()
            .assemble_bytes(&format!("{plain}{pool}"))
            .expect("the register form assembles");
    }

    fn ph327_program(payload: &[u8], sites: usize, tail_jump: bool) -> String {
        let mut s = String::from("*=$C000\n");
        for n in 0..sites {
            let call = if tail_jump && n + 1 == sites { "JMP" } else { "JSR" };
            s.push_str(&format!("    LDA #<S0\n    LDY #>S0\n    {call} __STR_PRINT\n"));
        }
        if !tail_jump {
            s.push_str("    RTS\n");
        }
        s.push_str("__STR_PRINT:\n    RTS\n; --- string pool (length-prefixed) ---\nS0:\n");
        let mut bytes = vec![payload.len() as u8];
        bytes.extend_from_slice(payload);
        let list: Vec<String> = bytes.iter().map(|b| format!("${b:02X}")).collect();
        s.push_str(&format!("    .byte {}\n__HEAP_BOTTOM:\n", list.join(",")));
        s
    }

    fn ph327_payload() -> Vec<u8> {
        let mut p = b"AB".to_vec();
        p.extend([0x20; 30]);
        p.extend(b"CD\r");
        p
    }

    #[test]
    fn ph327_prints_a_long_run_with_a_loop() {
        let (changed, got) = drive(ph327_split_print_runs, &ph327_program(&ph327_payload(), 1, false));
        assert!(changed, "{got}");
        assert!(
            got.contains(concat!(
                "    LDA #<S1\n    LDY #>S1\n    JSR __STR_PRINT\n",
                "    LDA #$20\n    LDX #$1E\n    JSR __PRINT_RUN\n",
                "    LDA #<S2\n    LDY #>S2\n    JSR __STR_PRINT\n",
            )),
            "{got}"
        );
        assert!(got.contains("S1:\n    .byte $02,$41,$42\n"), "{got}");
        assert!(got.contains("S2:\n    .byte $03,$43,$44,$0D\n"), "{got}");
        assert!(!got.contains("S0:"), "the whole string is gone:\n{got}");
        assert!(got.contains("__PRINT_RUN:\n    JSR $FFD2\n    DEX\n    BNE __PRINT_RUN\n    RTS\n"), "{got}");
    }

    #[test]
    fn ph327_adds_its_strings_inside_the_pool_not_after_a_later_record() {
        let asm = ph327_program(&ph327_payload(), 1, false).replace(
            "__HEAP_BOTTOM:\n",
            "; --- variable slots ---\n__BSS_START:\nS9:\n    .byte $00\nV_A:\n    .byte $00\n__HEAP_BOTTOM:\n",
        );
        let (changed, got) = drive(ph327_split_print_runs, &asm);
        assert!(changed, "{got}");
        let slots = got.find("; --- variable slots ---").expect("slots");
        for label in ["S10:", "S11:"] {
            let at = got.find(label).unwrap_or_else(|| panic!("{label} missing:\n{got}"));
            assert!(at < slots, "{label} landed past the pool:\n{got}");
        }
    }

    #[test]
    fn ph327_names_new_strings_past_a_record_it_cannot_read() {
        let long: Vec<String> = std::iter::once(0xFFu8)
            .chain(std::iter::repeat(0x41).take(260))
            .map(|b| format!("${b:02X}"))
            .collect();
        let asm = ph327_program(&ph327_payload(), 1, false)
            .replace("__HEAP_BOTTOM:\n", &format!("S1:\n    .byte {}\n__HEAP_BOTTOM:\n", long.join(",")));
        let (changed, got) = drive(ph327_split_print_runs, &asm);
        assert!(changed, "{got}");
        assert_eq!(got.matches("\nS1:\n").count(), 1, "{got}");
        assert!(got.contains("\nS2:\n") && got.contains("\nS3:\n"), "{got}");
    }

    #[test]
    fn ph327_keeps_the_tail_jump_on_the_last_part() {
        let (changed, got) = drive(ph327_split_print_runs, &ph327_program(&ph327_payload(), 1, true));
        assert!(changed, "{got}");
        assert!(got.contains("    JSR __PRINT_RUN\n    LDA #<S2\n    LDY #>S2\n    JMP __STR_PRINT\n"), "{got}");
    }

    #[test]
    fn ph327_leaves_a_string_with_any_other_reference_whole() {
        let asm = ph327_program(&ph327_payload(), 1, false).replace("    RTS\n__STR_PRINT:", "    LDA S0\n    RTS\n__STR_PRINT:");
        let (changed, got) = drive(ph327_split_print_runs, &asm);
        assert!(!changed, "{got}");
    }

    #[test]
    fn ph327_splits_only_where_the_run_outweighs_the_calls() {
        let (changed, _) = drive(ph327_split_print_runs, &ph327_program(&ph327_payload(), 3, false));
        assert!(!changed);
        let mut p = b"AB".to_vec();
        p.extend([0x11; 40]);
        let (changed, got) = drive(ph327_split_print_runs, &ph327_program(&p, 3, false));
        assert!(changed, "{got}");
        assert_eq!(got.matches("JSR __PRINT_RUN").count(), 3, "{got}");
        // Seven of a kind never pays.
        let mut p = b"AB".to_vec();
        p.extend([0x20; 7]);
        let (changed, _) = drive(ph327_split_print_runs, &ph327_program(&p, 1, false));
        assert!(!changed);
    }

    #[test]
    fn ph114_declines_below_the_break_even_point() {
        for sites in [1, 5, STR_PRINT_INLINE_MIN_SITES - 1] {
            let mut items = parse(&str_print_program(sites));
            assert!(
                !ph114_str_print_inline_args(&mut items),
                "{sites} sites saves {} bytes against a 34-byte trampoline",
                sites * 2
            );
            assert!(!render(&items).contains("__STR_PRINT_I"));
        }
    }

    #[test]
    fn ph114_needs_the_second_entry_point() {
        let src = str_print_program(20).replace("__STR_PRINT_PTR:\n", "");
        let mut items = parse(&src);
        assert!(!ph114_str_print_inline_args(&mut items));
    }

    #[test]
    fn ph114_leaves_the_register_convention_alone() {
        let mut src = String::from("*=$080D\n");
        for _ in 0..20 {
            src.push_str("    LDA V_A\n    LDY V_A+1\n    JSR __STR_PRINT\n");
        }
        src.push_str(
            "    RTS\n__STR_PRINT:\n    STA $FD\n    STY $FE\n__STR_PRINT_PTR:\n    \
             RTS\n__HEAP_BOTTOM:\n",
        );
        let mut items = parse(&src);
        assert!(!ph114_str_print_inline_args(&mut items));
    }

    #[test]
    fn ph114_puts_the_trampoline_before_the_heap() {
        let mut items = parse(&str_print_program(20));
        assert!(ph114_str_print_inline_args(&mut items));
        let out = render(&items);
        assert!(
            out.find("__STR_PRINT_I:").unwrap() < out.find("__HEAP_BOTTOM:").unwrap(),
            "the trampoline would be overwritten by the first few string allocations"
        );
    }

    #[test]
    fn ph114_puts_the_trampoline_before_the_bss_anchor() {
        let src = str_print_program(20).replace(
            "__HEAP_BOTTOM:\n",
            "__BSS_START:\n    .byte $00\n__HEAP_BOTTOM:\n",
        );
        let mut items = parse(&src);
        assert!(ph114_str_print_inline_args(&mut items));
        let out = render(&items);
        assert!(out.find("__STR_PRINT_I:").unwrap() < out.find("__BSS_START:").unwrap());
    }

    #[test]
    fn ph114_is_off_unless_the_caller_asks() {

        let out = run(&str_print_program(20), Profile::Default);
        assert!(!out.contains("__STR_PRINT_I"));
        let opts = Options {
            inline_str_print_args: true,
            ..Options::default()
        };
        let on = run_with_options(&str_print_program(20), Profile::Default, opts);
        assert!(on.contains("__STR_PRINT_I"));
    }

    #[test]
    fn reg_liveness_tracks_transfer_chain() {
        let items = parse("    LDA #$01\n    TAX\n    STX dst\n");
        let live = analyze_register_liveness(&items);
        assert!(live.is_live_out(0, Reg::A), "TAX needs A from LDA");
        assert!(live.is_live_in(1, Reg::A), "TAX reads A");
        assert!(live.is_live_out(1, Reg::X), "STX needs X from TAX");
        assert!(live.is_live_in(2, Reg::X), "STX reads X");
        assert!(!live.is_live_out(2, Reg::X), "X is dead after final store");
    }

    #[test]
    fn reg_liveness_reads_index_registers() {
        let items = parse("    LDA ($FD),Y\n    STA dst\n");
        let live = analyze_register_liveness(&items);
        assert!(live.is_live_in(0, Reg::Y), "(zp),Y addressing reads Y");
        assert!(!live.is_live_in(0, Reg::A), "LDA overwrites A");
        assert!(live.is_live_out(0, Reg::A), "STA consumes loaded A");
    }

    #[test]
    fn reg_liveness_merges_conditional_branch_paths() {
        let items = parse("    BEQ use\n    LDX #$02\nuse:\n    STX dst\n");
        let live = analyze_register_liveness(&items);
        assert!(
            live.is_live_in(0, Reg::X),
            "branch target can reach STX with old X"
        );
        assert!(live.is_live_out(1, Reg::X), "fallthrough LDX feeds STX");
    }

    #[test]
    fn reg_liveness_uses_jsr_contracts() {
        let items = parse("    LDY #$05\n    JSR __USE_Y\n    STA dst\n");
        let config = RegisterLivenessConfig::default()
            .with_jsr_effect("__USE_Y", RegEffect::new(RegSet::Y, RegSet::ALL));
        let live = analyze_register_liveness_with_config(&items, &config);
        assert!(live.is_live_out(0, Reg::Y), "known helper consumes Y");
        assert!(live.is_live_in(1, Reg::Y));
        assert!(
            !live.is_live_in(1, Reg::A),
            "helper defines A before STA uses it"
        );
        assert!(
            live.is_live_out(1, Reg::A),
            "STA consumes helper result in A"
        );
    }

    #[test]
    fn reg_liveness_treats_rts_as_conservative_boundary() {
        let items = parse("    RTS\n");
        let live = analyze_register_liveness(&items);
        assert!(live.is_live_in(0, Reg::A));
        assert!(live.is_live_in(0, Reg::X));
        assert!(live.is_live_in(0, Reg::Y));
    }

    #[test]
    fn reg_liveness_handles_branch_to_external_target() {
        let items = parse("    LDX #$01\n    BNE $A437\n    LDA #$00\n    RTS\n");
        let live = analyze_register_liveness(&items);
        assert!(
            live.is_live_out(0, Reg::X),
            "X live across the branch (target side)"
        );
    }

    #[test]
    fn ph017_drops_lda_immediately_overwritten() {
        let input = "    LDA $14\n    LDA $15\n    STA dst\n    RTS\n";
        let got = run_default(input);
        assert!(!got.contains("LDA $14"), "shadowed LDA dropped:\n{got}");
        assert!(got.contains("LDA $15"), "second LDA kept");
    }

    #[test]
    fn ph017_keeps_lda_when_io_volatile() {
        let input = "    LDA $D012\n    LDA $D013\n    STA dst\n    RTS\n";
        let got = run_default(input);
        assert!(got.contains("LDA $D012"), "I/O LDA preserved:\n{got}");
    }

    #[test]
    fn ph017_keeps_lda_when_branch_uses_flags() {
        let input = "    LDA $14\n    BNE skip\n    LDA $15\nskip:\n    STA dst\n    RTS\n";
        let got = run_default(input);
        assert!(got.contains("LDA $14"), "flag-feeding LDA kept:\n{got}");
    }

    #[test]
    fn ph019_lda_imm_tax_becomes_ldx_imm() {
        let input = "    LDA #$05\n    TAX\n    STX dst\n    LDA #$00\n    RTS\n";
        let got = run_default(input);
        assert!(got.contains("LDX #$05"), "rewrite to LDX:\n{got}");
        assert!(!got.contains("    TAX"), "TAX dropped");
    }

    #[test]
    fn ph019_keeps_lda_when_a_live_after() {
        // STA after TAX consumes A, so the rewrite is unsound.
        let input = "    LDA $14\n    TAX\n    STA scratch\n    STX dst\n    RTS\n";
        let got = run_default(input);
        assert!(got.contains("LDA $14"), "LDA kept; STA reads A:\n{got}");
        assert!(got.contains("    TAX"));
    }

    #[test]
    fn ph019_refuses_indirect_addressing() {
        let input = "    LDA ($FD),Y\n    TAX\n    STX dst\n    RTS\n";
        let got = run_default(input);
        assert!(got.contains("LDA ($FD),Y"));
        assert!(got.contains("    TAX"));
    }

    #[test]
    fn reg_liveness_handles_branch_with_unknown_label() {
        let items = parse("    LDX #$01\n    BNE missing_label\n    LDA #$00\n    RTS\n");
        let live = analyze_register_liveness(&items);
        assert!(live.is_live_out(0, Reg::X));
    }

    #[test]
    fn flag_liveness_tracks_branch_flags() {
        let items = parse("    LDA value\n    BEQ done\n    CLC\ndone:\n    RTS\n");
        let config = FlagLivenessConfig {
            return_live_out: FlagSet::EMPTY,
            ..FlagLivenessConfig::default()
        };
        let live = analyze_flag_liveness_with_config(&items, &config);
        assert!(live.is_live_out(0, Flag::Z), "BEQ consumes Z from LDA");
        assert!(live.is_live_in(1, Flag::Z));
        assert!(!live.is_live_in(1, Flag::C), "BEQ does not consume C");
    }

    #[test]
    fn flag_liveness_tracks_adc_carry_chain() {
        let items = parse("    CLC\n    ADC lo\n    ADC hi\n    STA dst\n");
        let live = analyze_flag_liveness(&items);
        assert!(
            live.is_live_out(0, Flag::C),
            "first ADC consumes CLC's carry"
        );
        assert!(
            live.is_live_out(1, Flag::C),
            "second ADC consumes first ADC carry"
        );
        assert!(!live.is_live_out(2, Flag::C), "carry dead after final ADC");
    }

    #[test]
    fn flag_liveness_uses_jsr_contracts() {
        let items = parse("    CMP #$00\n    JSR __KEEP_Z\n    BEQ done\ndone:\n    RTS\n");
        let config = FlagLivenessConfig::default()
            .with_jsr_effect("__KEEP_Z", FlagEffect::new(FlagSet::Z, FlagSet::C));
        let live = analyze_flag_liveness_with_config(&items, &config);
        assert!(live.is_live_out(0, Flag::Z), "helper contract consumes Z");
        assert!(live.is_live_in(1, Flag::Z));
        assert!(live.is_live_out(1, Flag::Z), "BEQ sees helper-preserved Z");
    }

    #[test]
    fn ph001_drops_jmp_to_next_label() {
        let input = "    JMP L1\nL1:\n    RTS\n";
        let got = run_default(input);
        assert!(
            !got.contains("JMP L1"),
            "JMP to next-label should be dropped:\n{got}"
        );

        assert!(!got.contains("L1:"), "dead label swept:\n{got}");
        assert!(got.contains("RTS"), "fall-through code kept:\n{got}");
    }

    #[test]
    fn ph001_keeps_jmp_when_label_differs() {
        let input = "    JMP L2\nL1:\n    RTS\n";
        let got = run_default(input);
        assert!(got.contains("JMP L2"));
    }

    #[test]
    fn ph001_drops_branch_to_next_label() {
        let input = "    BEQ L1\nL1:\n    RTS\n";
        let got = run_default(input);
        assert!(!got.contains("BEQ"), "BEQ to next label dropped:\n{got}");
    }

    #[test]
    fn ph001_drops_jmp_past_a_label_run() {
        let input = "    JMP __ON_DONE_4\n__ON_CASE_4_3:\n__ON_DONE_4:\n    RTS\n";
        let got = run_default(input);
        assert!(
            !got.contains("JMP __ON_DONE_4"),
            "JMP through a same-address label run dropped:\n{got}"
        );
        assert!(got.contains("RTS"), "body kept:\n{got}");
    }

    #[test]
    fn ph001_keeps_jmp_when_instructions_precede_the_target() {
        let input = "    JMP L1\n__OTHER:\n    LDA #$00\nL1:\n    INX\n    RTS\n    .word __OTHER\n";
        let got = run_default(input);
        assert!(
            got.contains("JMP L1"),
            "JMP kept when real code sits before the target:\n{got}"
        );
        assert!(got.contains("LDA #$00"), "referenced block kept:\n{got}");
    }

    #[test]
    fn ph002_drops_dead_code_after_jmp() {

        let input =
            "    JMP elsewhere\n    LDA #$00\n    STA $39\nKEEP1:\n    RTS\n    .word KEEP1\n";
        let got = run_default(input);
        assert!(!got.contains("LDA #$00"), "dead code dropped:\n{got}");
        assert!(!got.contains("STA $39"));
        assert!(got.contains("KEEP1:"));
    }

    #[test]
    fn ph002_stops_at_label() {
        let input = "    JMP elsewhere\nKEEP1:\n    LDA #$00\n    .word KEEP1\n";
        let got = run_default(input);
        assert!(
            got.contains("LDA #$00"),
            "label-protected code kept:\n{got}"
        );
    }

    #[test]
    fn ph003_jsr_rts_becomes_jmp() {
        let input = "    JSR __HELPER\n    RTS\n";
        let got = run_default(input);
        assert!(
            got.contains("JMP __HELPER"),
            "tail-call JMP emitted:\n{got}"
        );
        assert!(!got.contains("RTS"), "stale RTS removed");
    }

    #[test]
    fn ph003_keeps_jsr_when_label_intervenes() {
        let input = "    JSR __HELPER\nKEEP1:\n    RTS\n";
        let got = run_default(input);
        assert!(
            got.contains("JSR __HELPER"),
            "label barrier respected:\n{got}"
        );
        assert!(got.contains("RTS"));
    }

    #[test]
    fn ph008_drops_lda_after_sta_same_label() {
        let input = "    STA tmp\n    LDA tmp\n    STA dst\n";
        let got = run_default(input);
        assert!(!got.contains("LDA tmp"), "redundant LDA dropped:\n{got}");
        assert!(got.contains("STA tmp"));
        assert!(got.contains("STA dst"));
    }

    #[test]
    fn ph008_drops_lda_zp_after_sta_zp() {
        let input = "    STA $14\n    LDA $14\n    STA $15\n";
        let got = run_default(input);
        assert!(!got.contains("LDA $14"), "redundant zp LDA dropped:\n{got}");
    }

    #[test]
    fn ph008_keeps_when_volatile_address() {
        let input = "    STA $D020\n    LDA $D020\n    STA $D021\n";
        let got = run_default(input);
        assert!(got.contains("LDA $D020"), "volatile LDA preserved:\n{got}");
    }

    #[test]
    fn ph008_keeps_when_branch_uses_flags() {
        let input = "    STA tmp\n    LDA tmp\n    BEQ ok\n    RTS\nok:\n    RTS\n";
        let got = run_default(input);
        assert!(
            got.contains("LDA tmp"),
            "LDA kept; BEQ needs its flags:\n{got}"
        );
    }

    #[test]
    fn ph008_supports_x_and_y_register_pairs() {
        let input_x = "    STX scratch\n    LDX scratch\n    STX done\n";
        assert!(!run_default(input_x).contains("LDX scratch"));
        let input_y = "    STY scratch\n    LDY scratch\n    STY done\n";
        assert!(!run_default(input_y).contains("LDY scratch"));
    }

    #[test]
    fn ph009_drops_sta_after_lda_same_label() {
        let input = "    LDA tmp\n    STA tmp\n    RTS\n";
        let got = run_default(input);
        assert!(got.contains("LDA tmp"), "load kept");
        assert!(!got.contains("STA tmp"), "redundant store dropped:\n{got}");
    }

    #[test]
    fn ph009_keeps_when_address_volatile() {
        let input = "    LDA $D020\n    STA $D020\n";
        let got = run_default(input);
        assert!(got.contains("STA $D020"), "volatile STA kept:\n{got}");
    }

    #[test]
    fn ph020_drops_redundant_non_adjacent_imm_load() {
        let input = "    LDA #$01\n    STA tmp\n    LDA #$01\n    STA dst\n    RTS\n";
        let got = run_default(input);
        assert_eq!(
            got.matches("LDA #$01").count(),
            1,
            "second immediate load dropped:\n{got}"
        );
        assert!(got.contains("STA tmp"));
        assert!(got.contains("STA dst"));
    }

    #[test]
    fn ph020_keeps_redundant_imm_load_when_it_refreshes_branch_flags() {
        let input = "    LDA #$00\n    INX\n    LDA #$00\n    BEQ ok\n    RTS\nok:\n    RTS\n";
        let got = run_default(input);
        assert!(
            got.contains("INX\n    LDA #$00"),
            "the Z-refreshing load (after INX) must survive:\n{got}"
        );
        assert!(
            !got.contains("LDA #$00\n    INX"),
            "shadowed first load consumed by PH317:\n{got}"
        );
    }

    #[test]
    fn ph020_replaces_matching_imm_load_with_transfer() {
        let input = "    LDA #$7F\n    STA tmp\n    LDX #$7F\n    STX dst\n    RTS\n";
        let got = run_default(input);
        assert!(got.contains("    TAX"), "LDX replaced by TAX:\n{got}");
        assert!(
            !got.contains("LDX #$7F"),
            "old immediate load removed:\n{got}"
        );
    }

    #[test]
    fn ph020_replaces_matching_memory_load_with_transfer() {
        let input = "    LDA tmp\n    INY\n    LDX tmp\n    STX dst\n    RTS\n";
        let got = run_default(input);
        assert!(got.contains("    TAX"), "LDX tmp replaced by TAX:\n{got}");
        assert!(!got.contains("LDX tmp"), "old memory load removed:\n{got}");
    }

    #[test]
    fn ph020_resets_known_values_at_labels() {
        let input = "    LDA #$02\nKEEP1:\n    LDA #$02\n    STA dst\n    RTS\n";
        let got = run_default(input);
        assert_eq!(
            got.matches("LDA #$02").count(),
            2,
            "label barrier prevents cross-label fact reuse:\n{got}"
        );
    }

    #[test]
    fn ph028_drops_lda_after_dec_when_a_dead_at_branch() {
        let input = "*=$0801\nL20:\n    DEC VI_M\n    LDA VI_M\n    BEQ L80\nL25:\n    LDA VI_S\n    JMP done\nL80:\n    LDA #$50\n    STA dst\ndone:\n    RTS\n";
        let got = run_default(input);
        assert_eq!(
            got.matches("LDA VI_M").count(),
            0,
            "LDA VI_M must be dropped after DEC VI_M when both branches redefine A:\n{got}"
        );
        assert!(got.contains("DEC VI_M"));
    }

    #[test]
    fn ph028_keeps_lda_when_a_used_downstream() {
        let input = "*=$0801\nL1:\n    DEC VI_M\n    LDA VI_M\n    STA VI_C\n    RTS\n";
        let got = run_default(input);
        assert!(
            got.contains("LDA VI_M"),
            "STA VI_C reads A, so the load must stay.\n{got}"
        );
    }

    #[test]
    fn ph020_invalidates_memory_facts_on_unknown_store() {
        let input = "    LDA tmp\n    STA ($FB),Y\n    LDX tmp\n    STX dst\n    RTS\n";
        let got = run_default(input);
        assert!(
            got.contains("LDX tmp"),
            "indirect store may alias tmp, so reload stays:\n{got}"
        );
    }

    #[test]
    fn ph020_drops_redundant_clc_and_sec() {
        let clc = "    CLC\n    LDA foo\n    CLC\n    ADC bar\n    STA dst\n    RTS\n";
        let got = run_default(clc);
        assert_eq!(got.matches("CLC").count(), 1, "second CLC dropped:\n{got}");

        let sec = "    SEC\n    LDA foo\n    SEC\n    SBC bar\n    STA dst\n    RTS\n";
        let got = run_default(sec);
        assert_eq!(got.matches("SEC").count(), 1, "second SEC dropped:\n{got}");
    }

    #[test]
    fn ph020_propagates_inx_dex_to_known_value() {
        let input = "    LDX #$05\n    INX\n    CPX #$06\n    BEQ ok\n    RTS\n    NOP\nok:\n    INX\n    RTS\n";
        let got = run_default(input);
        assert!(
            !got.contains("BEQ ok"),
            "BEQ removed (always taken):\n{got}"
        );
    }

    #[test]
    fn ph020_folds_and_immediate() {
        let input =
            "    LDA #$F0\n    AND #$0F\n    BEQ done\n    RTS\n    NOP\ndone:\n    INX\n    RTS\n";
        let got = run_default(input);
        assert!(!got.contains("BEQ done"), "BEQ removed:\n{got}");
        assert!(
            !got.contains("RTS\n    NOP"),
            "fall-through dead code dropped"
        );
    }

    #[test]
    fn ph020_propagates_mem_imm_through_store_load() {
        let input = "    LDA #$05\n    STA tmp\n    JSR helper\n    LDA tmp\n    CMP #$05\n    BEQ ok\n    RTS\nok:\n    RTS\n";
        let got = run_default(input);
        assert!(
            got.contains("CMP #$05"),
            "JSR barrier prevents stale mem_imm:\n{got}"
        );
    }

    #[test]
    fn ph021_drops_never_taken_branch() {
        // After LDA #$01, Z=false so BEQ never branches.
        let input = "    LDA #$01\n    BEQ skip\n    INX\nskip:\n    RTS\n";
        let got = run_default(input);
        assert!(!got.contains("BEQ skip"), "dead BEQ removed:\n{got}");
        assert!(got.contains("INX"), "fall-through code preserved");
    }


    #[test]
    fn ph025_drops_redundant_store_after_known_value() {
        let input = "    LDA #$05\n    STA tmp\n    LDA #$05\n    STA tmp\n    RTS\n";
        let got = run_default(input);
        let stas: Vec<&str> = got.lines().filter(|l| l.contains("STA tmp")).collect();
        assert_eq!(stas.len(), 1, "second STA dropped:\n{got}");
    }

    #[test]
    fn ph025_keeps_store_when_value_changed() {

        let mut items =
            parse("    LDA #$05\n    STA tmp\n    LDA #$07\n    STA tmp\n    RTS\n");
        assert!(
            !ph025_drop_redundant_store(&mut items),
            "different-value store is not redundant"
        );
        let got = render(&items);
        assert_eq!(got.matches("STA tmp").count(), 2, "both STAs kept:\n{got}");
    }

    #[test]
    fn ph026_drops_no_op_and_ff() {
        let input = "    LDA val\n    AND #$FF\n    STA dst\n    RTS\n";
        let got = run_default(input);
        assert!(!got.contains("AND #$FF"), "AND #$FF dropped:\n{got}");
    }

    #[test]
    fn ph026_drops_no_op_ora_zero() {
        let input = "    LDA val\n    ORA #$00\n    STA dst\n    RTS\n";
        let got = run_default(input);
        assert!(!got.contains("ORA #$00"), "ORA #$00 dropped:\n{got}");
    }

    #[test]
    fn ph026_rewrites_and_zero_to_lda() {
        let input = "    LDA val\n    AND #$00\n    STA dst\n    RTS\n";
        let got = run_default(input);
        assert!(got.contains("LDA #$00"), "AND zero becomes LDA zero.\n{got}");
    }

    #[test]
    fn ph026_keeps_and_ff_when_z_is_live() {
        let input = "    LDA val\n    AND #$FF\n    BEQ skip\n    INX\nskip:\n    RTS\n";
        let got = run_default(input);
        assert!(got.contains("AND #$FF"), "AND #$FF kept (Z live):\n{got}");
    }

    #[test]
    fn ph024_keeps_cmp_when_branch_reads_flags() {
        // Branch right after CMP keeps it live.
        let input = "    LDA val\n    CMP #$05\n    BEQ ok\n    RTS\nok:\n    RTS\n";
        let got = run_default(input);
        assert!(got.contains("CMP #$05"), "live CMP kept:\n{got}");
    }

    fn poke_shape(hex: &str) -> String {
        format!("    LDA #$00\n    BIT ${hex}\n    STA ${hex}\n    CLV\n    LDA #$01\n    RTS\n")
    }

    #[test]
    fn ph024_keeps_bit_when_the_read_is_the_point() {
        // BIT preserves the dummy read of $D01E.
        for hex in ["D01E", "D01F", "DC0D", "DD0B"] {
            let got = run_default(&poke_shape(hex));
            assert!(
                got.contains(&format!("BIT ${hex}")),
                "I/O discard read kept at ${hex}:\n{got}"
            );
        }
    }

    #[test]
    fn ph024_still_drops_a_dead_bit_outside_io() {

        for hex in ["0400", "C000", "CFFF", "E000"] {
            let got = run_default(&poke_shape(hex));
            assert!(
                !got.contains(&format!("BIT ${hex}")),
                "dead BIT at ${hex} still dropped:\n{got}"
            );
            assert!(
                got.contains(&format!("STA ${hex}")),
                "the store survives:\n{got}"
            );
        }
    }

    #[test]
    fn ph022_replaces_jmp_to_rts_with_rts() {
        let input = "    JSR helper\n    JMP exit\nhelper:\n    INX\n    RTS\nexit:\n    RTS\n";
        let got = run_default(input);
        // The JMP exit becomes RTS since exit is just RTS.
        let lines: Vec<&str> = got.lines().collect();
        assert!(
            !lines.iter().any(|l| l.trim() == "JMP exit"),
            "JMP exit replaced:\n{got}"
        );
    }

    #[test]
    fn ph020_keeps_clc_after_adc_when_carry_is_unknown() {
        let input = "    CLC\n    ADC lo\n    CLC\n    ADC hi\n    STA dst\n    RTS\n";
        let got = run_default(input);
        assert_eq!(
            got.matches("CLC").count(),
            2,
            "ADC makes carry unknown, so second CLC stays:\n{got}"
        );
    }

    #[test]
    fn ph012_drops_cmp_zero_after_lda() {
        let input = "    LDA value\n    CMP #$00\n    BEQ zero\n    RTS\n";
        let got = run_default(input);
        assert!(!got.contains("CMP #$00"), "CMP after LDA dropped:\n{got}");
        assert!(got.contains("BEQ zero"));
    }

    #[test]
    fn ph012_drops_cmp_zero_after_strcmp() {
        let input = "    JSR __STRCMP\n    CMP #$00\n    BEQ ok\n    RTS\n";
        let got = run_default(input);
        assert!(
            !got.contains("CMP #$00"),
            "CMP after __STRCMP dropped:\n{got}"
        );
    }

    #[test]
    fn ph012_keeps_cmp_zero_when_carry_branch_follows() {
        // BCC reads C, so dropping the CMP would change semantics.
        let input = "    LDA value\n    CMP #$00\n    BCC less\n    RTS\n";
        let got = run_default(input);
        assert!(got.contains("CMP #$00"), "CMP kept before BCC:\n{got}");
    }

    #[test]
    fn ph012_keeps_cmp_zero_after_unrelated_op() {
        // STA doesn't set Z, so the CMP is meaningful.
        let input = "    STA mem\n    CMP #$00\n    BEQ ok\n";
        let got = run_default(input);
        assert!(got.contains("CMP #$00"), "CMP kept after STA:\n{got}");
    }

    #[test]
    fn ph012_keeps_cmp_zero_when_bcc_follows_bne_or_beq() {
        let input = "    LDA value\n    CMP #$00\n    BEQ skip\n    BCC skip\n    RTS\n";
        let got = run_default(input);
        assert!(
            got.contains("CMP #$00"),
            "CMP must NOT be dropped when a BCC/BCS sits past an \
             intervening BEQ. BCC needs the comparison carry.\n{got}"
        );
    }

    #[test]
    fn ph012_keeps_cmp_zero_when_sbc_consumes_the_carry() {
        let input = "\
    LDA value
    CMP #$00
    LDA value+1
    SBC #$00
    BVC nv
    EOR #$80
nv:
    BPL done
done:
    RTS
";
        let got = run_default(input);
        assert!(
            got.contains("CMP #$00"),
            "CMP must NOT be dropped when a later SBC consumes its carry:\n{got}"
        );
    }

    #[test]
    fn ph012_keeps_cmp_zero_when_rol_consumes_the_carry() {
        let input = "    LDA value\n    CMP #$00\n    ROL\n    RTS\n";
        let got = run_default(input);
        assert!(
            got.contains("CMP #$00"),
            "CMP must NOT be dropped when a ROL samples its carry:\n{got}"
        );
    }

    #[test]
    fn ph012_drops_cmp_zero_when_carry_is_redefined_first() {
        let input = "    LDA value\n    CMP #$00\n    CLC\n    SBC #$01\n    BNE done\ndone:\n    RTS\n";
        let got = run_default(input);
        assert!(
            !got.contains("CMP #$00"),
            "CMP is dead once CLC redefines carry before any reader:\n{got}"
        );
    }

    #[test]
    fn ph012_keeps_cmp_zero_after_ldx() {
        let input = "    LDA value\n    LDX #$01\n    CMP #$00\n    BNE skip\n    RTS\n";
        let got = run_default(input);
        assert!(
            got.contains("CMP #$00"),
            "CMP must stay because LDX sets Z from X.\n{got}"
        );
    }

    #[test]
    fn ph012_keeps_cmp_zero_after_ldy() {
        let input = "    LDA value\n    LDY #$01\n    CMP #$00\n    BNE skip\n    RTS\n";
        let got = run_default(input);
        assert!(
            got.contains("CMP #$00"),
            "CMP must stay because LDY sets Z from Y.\n{got}"
        );
    }

    #[test]
    fn ph012_keeps_cmp_zero_after_inx() {
        let input = "    LDA value\n    INX\n    CMP #$00\n    BNE skip\n    RTS\n";
        let got = run_default(input);
        assert!(
            got.contains("CMP #$00"),
            "CMP must stay because INX sets Z from X.\n{got}"
        );
    }

    #[test]
    fn ph012_keeps_cmp_zero_after_dec_memory() {
        let input = "    LDA value\n    DEC mem\n    CMP #$00\n    BNE skip\n    RTS\n";
        let got = run_default(input);
        assert!(
            got.contains("CMP #$00"),
            "CMP must stay because DEC sets Z from memory.\n{got}"
        );
    }

    #[test]
    fn ph012_drops_cmp_zero_after_implicit_asl() {
        let input = "    LDA value\n    ASL\n    CMP #$00\n    BEQ ok\n";
        let got = run_default(input);
        assert!(
            !got.contains("CMP #$00"),
            "CMP after implicit ASL is redundant:\n{got}"
        );
    }

    #[test]
    fn ph012_keeps_cmp_zero_after_asl_memory() {
        let input = "    LDA value\n    ASL mem\n    CMP #$00\n    BEQ ok\n";
        let got = run_default(input);
        assert!(
            got.contains("CMP #$00"),
            "CMP after ASL <mem> must NOT be dropped:\n{got}"
        );
    }

    #[test]
    fn ph106_factors_intbyte_givayf_at_threshold() {
        let mut input = String::new();
        for v in ["#$01", "#$02", "#$03", "#$04"] {
            input.push_str("    LDA #$00\n");
            input.push_str(&format!("    LDY {v}\n"));
            input.push_str("    JSR $B391\n");
            input.push_str("    JSR __PRINT_FAC\n");
        }
        let got = run_default(&input);
        assert!(got.contains("__LD_BYTE_FAC:"), "helper emitted:\n{got}");
        assert!(
            got.contains("JSR __LD_BYTE_FAC"),
            "callsite rewritten:\n{got}"
        );
        // The rewrite drops the LDA #$00 from each call site.
        assert_eq!(
            got.matches("LDA #$00").count(),
            1,
            "only the helper keeps LDA #$00"
        );
    }

    #[test]
    fn ph106_skips_under_threshold() {
        let mut input = String::new();
        for v in ["#$01", "#$02"] {
            input.push_str("    LDA #$00\n");
            input.push_str(&format!("    LDY {v}\n"));
            input.push_str("    JSR $B391\n");
        }
        let got = run_default(&input);
        assert!(
            !got.contains("__LD_BYTE_FAC"),
            "helper not emitted under threshold:\n{got}"
        );
    }

    #[test]
    fn ph106_keeps_unrelated_lda() {
        let input = concat!(
            "    LDA #$01\n",
            "    LDY #$10\n",
            "    JSR $B391\n",
            "    LDA #$01\n",
            "    LDY #$20\n",
            "    JSR $B391\n",
            "    LDA #$01\n",
            "    LDY #$30\n",
            "    JSR $B391\n",
            "    LDA #$01\n",
            "    LDY #$40\n",
            "    JSR $B391\n",
        );
        let got = run_default(input);
        assert!(
            !got.contains("__LD_BYTE_FAC"),
            "non-zero high byte not factored:\n{got}"
        );
    }

    #[test]
    fn ph107_factors_chrout_byte_at_threshold() {
        let mut input = String::new();
        for _ in 0..4 {
            input.push_str("    LDA #$0D\n");
            input.push_str("    JSR $FFD2\n");
        }
        let got = run_default(&input);
        assert!(got.contains("__CHROUT_0D:"), "helper emitted:\n{got}");
        assert!(
            got.contains("JSR __CHROUT_0D"),
            "callsite rewritten:\n{got}"
        );
        assert_eq!(
            got.matches("LDA #$0D").count(),
            1,
            "only the helper keeps LDA #$0D"
        );
        assert_eq!(got.matches("JSR $FFD2").count(), 0, "old JSR retargeted");
        assert!(got.contains("JMP $FFD2"), "helper falls through with JMP");
    }

    #[test]
    fn ph107_factors_per_byte_value() {
        let mut input = String::new();
        for _ in 0..4 {
            input.push_str("    LDA #$0D\n    JSR $FFD2\n");
            input.push_str("    LDA #$12\n    JSR $FFD2\n");
        }
        let got = run_default(&input);
        assert!(got.contains("__CHROUT_0D:"));
        assert!(got.contains("__CHROUT_12:"));
        assert!(got.contains("__CHROUT_0D"));
        assert!(got.contains("__CHROUT_12"));
    }

    #[test]
    fn ph107_skips_under_threshold() {
        let mut input = String::new();
        for _ in 0..3 {
            input.push_str("    LDA #$0D\n    JSR $FFD2\n");
        }
        let got = run_default(&input);
        assert!(
            !got.contains("__CHROUT_"),
            "no helper emitted under threshold:\n{got}"
        );
        assert_eq!(got.matches("LDA #$0D").count(), 3);
        assert_eq!(got.matches("JSR $FFD2").count(), 3);
    }

    #[test]
    fn ph107_doesnt_factor_label_target() {
        let mut input = String::new();
        for _ in 0..4 {
            input.push_str("    LDA #$0D\n    JSR __OWN_HELPER\n");
        }
        let got = run_default(&input);
        assert!(!got.contains("__CHROUT_"));
    }

    #[test]
    fn ph107_doesnt_factor_when_lda_isnt_byte_immediate() {
        let mut input = String::new();
        for _ in 0..4 {
            input.push_str("    LDA $0400\n    JSR $FFD2\n");
        }
        let got = run_default(&input);
        assert!(!got.contains("__CHROUT_"));
    }

    #[test]
    fn ph107_sweeps_chrout_tail_calls_through_existing_helpers() {
        let mut input = String::new();
        for _ in 0..4 {
            input.push_str("    LDA #$0D\n    JSR $FFD2\n    NOP\n");
        }
        input.push_str("    JSR $FFE7\n    LDA #$0D\n    JMP $FFD2\nAFTER:\n    RTS\n");
        let got = run_default(&input);
        assert!(got.contains("__CHROUT_0D:"), "helper emitted:\n{got}");
        assert!(
            got.contains("JMP __CHROUT_0D\n"),
            "the tail call jumps through the helper:\n{got}"
        );
        // The helper's own body is the one surviving LDA/JMP pair.
        assert!(
            got.contains("__CHROUT_0D:\n    LDA #$0D\n    JMP $FFD2\n"),
            "helper body not self-swept:\n{got}"
        );
        assert_eq!(got.matches("LDA #$0D").count(), 1, "tail's LDA dropped");
    }

    #[test]
    fn ph107_leaves_a_lone_tail_alone() {
        let got = run_default("    JSR $FFE7\n    LDA #$0D\n    JMP $FFD2\nAFTER:\n    RTS\n");
        assert!(
            got.contains("LDA #$0D\n    JMP $FFD2"),
            "lone tail untouched:\n{got}"
        );
        assert!(!got.contains("__CHROUT_"));
    }

    #[test]
    fn ph111_factors_lda_ldy_jsr_triple() {
        let mut input = String::new();
        for _ in 0..4 {
            input.push_str("    LDA #<__VAR_X\n");
            input.push_str("    LDY #>__VAR_X\n");
            input.push_str("    JSR $BBA2\n");
            input.push_str("    JSR __PRINT_FAC\n"); // observe FAC so PH150 keeps the MOVFM
            input.push_str("    NOP\n");
        }
        let got = run_default(&input);
        assert!(got.contains("__T0:"), "triple helper emitted:\n{got}");
        assert!(got.contains("JSR __T0"), "callsite rewritten");
        assert!(got.contains("JMP $BBA2"), "helper tail-calls target");
    }

    #[test]
    fn factoring_batches_skip_every_existing_id_after_cleanup_holes() {
        type Pass = fn(&mut Vec<Item>) -> bool;
        for (prefix, pass, kind) in [
            ("__JSR2_", ph110_factor_jsr_pair as Pass, 0),
            ("__JSR3_", ph113_factor_jsr_triple as Pass, 1),
            ("__VP", ph112_factor_varptr_jsr as Pass, 2),
            ("__T", ph111_factor_lda_ldy_jsr as Pass, 3),
        ] {
            let mut input = String::from("*=$2000\n");
            for id in [0, 2, 4] {
                input.push_str(&format!(" JSR {prefix}{id}\n NOP\n"));
            }
            for group in 0..3 {
                let pattern = match kind {
                    0 => format!(" JSR A{group}\n JSR B\n"),
                    1 => format!(" JSR A{group}\n JSR B\n JSR C\n"),
                    2 => format!(" LDA V{group}\n LDY V{group}+1\n JSR OUT\n"),
                    _ => format!(" LDA #$0{}\n LDY #$0{}\n JSR OUT\n", group+2, group+3),
                };
                for _ in 0..4 { input.push_str(&format!("{pattern} NOP\n")); }
            }
            input.push_str(" JMP $FFFE\n");
            for id in [0, 2, 4] {
                input.push_str(&format!("{prefix}{id}:\n INC $041{id}\n RTS\n"));
            }
            for group in 0..3 {
                input.push_str(&format!("A{group}:\n INC $040{group}\n RTS\nV{group}:\n .word {}\n", 300+group));
            }
            input.push_str("B:\n INC $0403\n RTS\nC:\n INC $0404\n RTS\nOUT:\n CLC\n ADC $0405\n STA $0405\n TYA\n CLC\n ADC $0406\n STA $0406\n RTS\n");
            let mut items = parse(&input);
            assert!(pass(&mut items), "{prefix}: fixture must factor three groups");
            let mut seen = std::collections::HashSet::new();
            for item in &items {
                if let Item::Label(name) = item {
                    assert!(seen.insert(name.clone()), "{prefix}: duplicate {name}");
                }
            }
            let output = render(&items);
            for id in [1, 3, 5] { assert!(output.contains(&format!("{prefix}{id}:")), "{prefix}: missing {id}\n{output}"); }
            let execute = |asm: &str| {
                let bytes = asm6502::Assembler6502::new().assemble_bytes(asm).unwrap();
                let mut machine = emu64::System::new();
                machine.mem.ram[0x2000..0x2000+bytes.len()].copy_from_slice(&bytes);
                machine.mem.ram[0x400..0x415].fill(0);
                machine.start_at(0x2000);
                machine.cpu.regs.sp = 0xF0;
                for _ in 0..2000 {
                    if machine.cpu.regs.pc == 0xFFFE { break; }
                    assert!(machine.step());
                }
                assert_eq!(machine.cpu.regs.pc, 0xFFFE);
                assert_eq!(machine.cpu.regs.sp, 0xF0);
                (machine.mem.ram[0x400..0x415].to_vec(), machine.cpu.regs.a, machine.cpu.regs.x, machine.cpu.regs.y, machine.cpu.regs.p)
            };
            assert_eq!(execute(&input), execute(&output), "{prefix}: changed call behavior");
        }
    }

    #[test]
    fn fixed_name_factoring_declines_live_helpers_on_a_later_round() {
        type Pass = fn(&mut Vec<Item>) -> bool;
        for (label, pass, pattern, helper) in [
            ("__LD_BYTE_FAC", ph106_factor_int_byte_to_fac as Pass, " LDA #$00\n LDY #$02\n JSR $B391\n", " LDA #$00\n JMP $B391\n"),
            ("__LDV_02_FAC", ph109_factor_per_imm_byte_fac as Pass, " LDY #$02\n JSR __LD_BYTE_FAC\n", " LDA #$00\n LDY #$02\n JMP $B391\n"),
            ("__CHROUT_41", ph107_factor_chrout_byte as Pass, " LDA #$41\n JSR $FFD2\n", " LDA #$41\n JMP $FFD2\n"),
        ] {
            let input = format!("{} JMP $FFFE\n{label}:\n{helper}", format!("{pattern} NOP\n").repeat(4));
            let mut items = parse(&input);
            let before = render(&items);
            assert!(!pass(&mut items));
            assert_eq!(items.iter().filter(|item| matches!(item, Item::Label(name) if name == label)).count(), 1, "{label}: duplicated existing definition");
            assert_eq!(render(&items), before, "{label}: existing helpers prevent recursive reuse");
        }

        let input = format!("{} JMP $FFFE\n__CHROUT_41:\n JMP SHARED_TAIL\nSHARED_TAIL:\n LDA #$41\n JSR $FFD2\n RTS\n", " LDA #$41\n JSR $FFD2\n NOP\n".repeat(4));
        let mut items = parse(&input);
        assert!(!ph107_factor_chrout_byte(&mut items));
        assert_eq!(render(&items), render(&parse(&input)));
    }

    /// PH106 creates a helper that supplies A=0 but still consumes Y.
    #[test]
    fn repeated_round_keeps_y_input_to_the_byte_fac_helper() {
        let values = [128u8, 40, 192, 88];
        let mut input = String::from("*=$2000\n");
        for (i, value) in values.iter().enumerate() {
            input.push_str(&format!(
                " LDA #$00\n LDY #${value:02X}\n JSR $B391\n\
                 LDA #${:02X}\n STA ${:04X}\n JSR $B7F7\n LDA $14\n STA ${:04X}\n",
                i + 1, 0x0420 + i, 0x0400 + i,
            ));
        }
        input.push_str(" JMP $FFFE\n");
        let opts = Options { aggressive_size_dedup: true, ..Options::default() };
        let once = run_once_with_options(&input, Profile::Size, opts);
        assert!(once.contains("__LD_BYTE_FAC:"), "fixture did not create the generic helper:\n{once}");
        let twice = run_with_options(&input, Profile::Size, opts);
        for (arm, asm) in [("original", input), ("first", once), ("repeated", twice)] {
            let bytes = asm6502::Assembler6502::new().assemble_bytes(&asm).unwrap();
            let mut machine = emu64::System::new();
            machine.mem.ram[0x2000..0x2000 + bytes.len()].copy_from_slice(&bytes);
            machine.mem.ram[0x0400..0x0404].fill(0);
            machine.start_at(0x2000);
            machine.cpu.regs.sp = 0xF0;
            machine.cpu.regs.y = 7;
            for _ in 0..10_000 {
                if machine.cpu.regs.pc == 0xFFFE { break; }
                assert!(machine.step(), "{arm}: stopped before returning");
            }
            assert_eq!(machine.cpu.regs.pc, 0xFFFE, "{arm}: did not return");
            assert_eq!(machine.cpu.regs.sp, 0xF0, "{arm}: stack imbalance");
            assert_eq!(&machine.mem.ram[0x0400..0x0404], &values, "{arm}: lost the byte-to-FAC Y argument\n{asm}");
        }
    }

    #[test]
    fn ph113_factors_jsr_triple() {
        let mut input = String::new();
        for _ in 0..4 {
            input.push_str("    JSR __SEQ_A\n");
            input.push_str("    JSR __SEQ_B\n");
            input.push_str("    JSR __SEQ_C\n");
            input.push_str("    NOP\n");
        }
        let got = run_default(&input);
        assert!(got.contains("__JSR3_0:"), "triple helper emitted:\n{got}");
        assert!(got.contains("JSR __JSR3_0"), "callsite rewritten");
    }

    const SYS_TXT_OUT_BODY: &str = "__SYS_TXT_OUT:\n    \
         LDY #$00\n    LDA ($7A),Y\n    TAY\n    TSX\n    \
         LDA $0103,X\n    STA $7B\n    LDA $0104,X\n    STA $7A\n    RTS\n";

    #[test]
    fn frame_sensitive_set_closes_over_tail_jumps() {
        let items = parse(
            "__JSR2_17:\n    JSR __SYS_REGS_OUT\n    JMP __SYS_TXT_OUT\n\
             __JSR2_30:\n    LDA #$01\n    JMP __JSR2_17\n\
             __JSR2_40:\n    JSR __OTHER\n    JMP __UNRELATED\n\
             __SYS_TXT_OUT:\n    RTS\n",
        );
        let set = frame_sensitive_labels(&items);
        // The seed, the direct alias, and the alias-of-the-alias.
        assert!(set.contains("__SYS_TXT_OUT"), "seed present: {set:?}");
        assert!(set.contains("__JSR2_17"), "direct tail-jump alias: {set:?}");
        assert!(set.contains("__JSR2_30"), "transitive alias: {set:?}");

        assert!(!set.contains("__JSR2_40"), "unrelated stub not swept in: {set:?}");
        let expected = [
            "__SYS_TXT_OUT",
            "__SYS_EVAL_RETURN",
            "__SYS_EVAL_CHAIN",
            "__JSR2_17",
            "__JSR2_30",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        assert_eq!(set, expected, "exactly the closure, no more");
    }

    #[test]
    fn only_a_jsr_adds_a_frame_to_a_sensitive_label() {
        let mut sensitive = std::collections::HashSet::new();
        sensitive.insert("__JSR2_17".to_string());
        let mk = |m: &str, op: &str| Insn {
            mnem: m.to_string(),
            operand: Some(op.to_string()),
            comment: None,
        };
        assert!(
            insn_adds_frame_to_sensitive(&mk("JSR", "__JSR2_17"), &sensitive),
            "a JSR to a frame-sensitive label adds the second frame"
        );
        assert!(
            !insn_adds_frame_to_sensitive(&mk("JMP", "__JSR2_17"), &sensitive),
            "a JMP is frame-transparent and must stay factorable"
        );
        assert!(
            !insn_adds_frame_to_sensitive(&mk("JSR", "__SOMETHING_ELSE"), &sensitive),
            "an ordinary call is untouched"
        );
    }

    #[test]
    fn ph110_allows_frame_sensitive_tail_jump() {
        let mut input = String::new();
        for _ in 0..4 {
            input.push_str("    JSR __SYS_REGS_OUT\n");
            input.push_str("    JSR __SYS_TXT_OUT\n");
            input.push_str("    NOP\n");
        }
        input.push_str(SYS_TXT_OUT_BODY);
        let got = run_default(&input);
        assert!(got.contains("__JSR2_0:"), "tail merge still allowed:\n{got}");
        // The sensitive target must be reached by JMP, never by JSR, from inside the stub.
        assert_eq!(
            got.matches("JMP __SYS_TXT_OUT").count(),
            1,
            "exactly one frame-transparent tail:\n{got}"
        );
        assert_eq!(
            got.matches("JSR __SYS_TXT_OUT").count(),
            0,
            "no call site left that adds a frame:\n{got}"
        );
    }

    #[test]
    fn ph301_refuses_window_calling_a_frame_sensitive_alias() {
        let mut input = String::new();
        for _ in 0..4 {
            input.push_str("    JSR __JSR2_17\n");
            input.push_str("    INC $39\n");
            input.push_str("    LDA #$02\n");
            input.push_str("    JSR __FOR_DISCARD\n");
            input.push_str("    LDX #$07\n");
        }

        input.push_str(SYS_TXT_OUT_BODY);
        input.push_str("__JSR2_17:\n    JSR __SYS_REGS_OUT\n    JMP __SYS_TXT_OUT\n");
        let got = run_default(&input);

        assert_eq!(
            got.matches("JMP __SYS_TXT_OUT").count(),
            1,
            "fixture intact: the tail jump that makes __JSR2_17 an alias:\n{got}"
        );
        // All four sites must still call the alias directly, at the depth codegen put them at.
        assert_eq!(
            got.matches("JSR __JSR2_17").count(),
            4,
            "every site still calls the alias directly:\n{got}"
        );

        for (idx, _) in got.match_indices("JSR __JSR2_17") {
            let before = &got[..idx];
            let region = before.rsplit('\n').find(|l| l.ends_with(':')).unwrap_or("");
            assert!(
                !region.starts_with("__SEQ_") && !region.starts_with("__JSR2_"),
                "call to a frame-sensitive alias was outlined into {region}:\n{got}"
            );
        }
    }

    #[test]
    fn ph113_refuses_frame_sensitive_in_middle_position() {
        let mut input = String::new();
        for _ in 0..4 {
            input.push_str("    JSR __SEQ_A\n");
            input.push_str("    JSR __SYS_TXT_OUT\n");
            input.push_str("    JSR __SEQ_C\n");
            input.push_str("    NOP\n");
        }
        input.push_str(SYS_TXT_OUT_BODY);
        let got = run_default(&input);
        assert_eq!(
            got.matches("JSR __JSR3_").count(),
            0,
            "no triple stub may bury a frame-sensitive call:\n{got}"
        );
    }

    #[test]
    fn inline_sys_epilogue_still_refused_by_stack_balance() {
        let mut input = String::new();
        for _ in 0..4 {
            input.push_str("    LDY #$00\n");
            input.push_str("    PLA\n");
            input.push_str("    STA $7B\n");
            input.push_str("    PLA\n");
            input.push_str("    STA $7A\n");
        }
        let got = run_default(&input);
        assert_eq!(
            got.matches("__SEQ_").count(),
            0,
            "unbalanced PLA window must never be outlined:\n{got}"
        );
    }

    #[test]
    fn ph112_factors_varptr_jsr() {
        let mut input = String::new();
        for _ in 0..4 {
            input.push_str("    LDA V_ES\n");
            input.push_str("    LDY V_ES+1\n");
            input.push_str("    JSR __STR_PRINT\n");
            input.push_str("    NOP\n");
        }
        let got = run_default(&input);
        assert!(got.contains("__VP0:"), "varptr helper emitted:\n{got}");
        assert!(got.contains("JSR __VP0"), "callsite rewritten");
    }

    #[test]
    fn ph111_skips_under_threshold() {
        let mut input = String::new();
        for _ in 0..2 {
            input.push_str("    LDA #<__VAR_X\n    LDY #>__VAR_X\n    JSR $BBA2\n    NOP\n");
        }
        let got = run_default(&input);
        assert!(!got.contains("__T0:"));
    }

    #[test]
    fn ph110_factors_jsr_pair_at_threshold() {
        let mut input = String::new();
        for _ in 0..4 {
            input.push_str("    JSR __SEQ_A\n");
            input.push_str("    JSR __SEQ_B\n");
            input.push_str("    NOP\n");
        }
        let got = run_default(&input);
        assert!(got.contains("__JSR2_0:"), "JSR-pair helper emitted:\n{got}");
        assert!(got.contains("JSR __JSR2_0"), "callsite rewritten");
        // Helper body should tail-call.
        assert!(got.contains("JSR __SEQ_A\n    JMP __SEQ_B"));
    }

    #[test]
    fn ph110_never_factors_a_self_modified_jsr() {
        let mut input = String::new();
        for _ in 0..6 {
            input.push_str("    JSR $FFFF\n    JSR __SYS_REGS_OUT\n    NOP\n");
        }
        let got = run_default(&input);
        assert!(
            !got.contains("__JSR2_"),
            "a patched JSR must never be shared:\n{got}"
        );

        let mut ok = String::new();
        for _ in 0..6 {
            ok.push_str("    JSR $C000\n    JSR __SYS_REGS_OUT\n    NOP\n");
        }
        let got_ok = run_default(&ok);
        assert!(
            got_ok.contains("__JSR2_"),
            "a real call pair should still factor:\n{got_ok}"
        );
    }

    #[test]
    fn ph110_skips_under_threshold() {
        let mut input = String::new();
        for _ in 0..3 {
            input.push_str("    JSR __SEQ_A\n    JSR __SEQ_B\n    NOP\n");
        }
        let got = run_default(&input);
        assert!(!got.contains("__JSR2_"));
    }

    #[test]
    fn ph110_recurses_on_own_helpers() {
        let mut input = String::new();
        for _ in 0..4 {
            input.push_str("    JSR __JSR2_0\n    JSR __JSR2_1\n    NOP\n");
        }
        let got = run_default(&input);
        assert!(
            got.contains("__JSR2_2:") || got.contains("__JSR2_0:"),
            "fixpoint produced a wrapper:\n{got}"
        );
    }

    #[test]
    fn ph110_keeps_codegen_helper_dependencies() {
        let mut input = String::from("*=$080D\n");
        for _ in 0..4 {
            input.push_str("    JSR __BOOL_TO_FAC\n    JSR __FAC_TO_INT16\n");
        }
        input.push_str("__HEAP_BOTTOM:\n");
        let got = run_default(&input);
        assert!(got.contains("__JSR2_"), "pair helper emitted:\n{got}");
        assert!(
            got.contains("__BOOL_TO_FAC:"),
            "missing bool helper repaired:\n{got}"
        );
        assert!(
            got.contains("__FAC_TO_INT16:"),
            "missing helper repaired:\n{got}"
        );
        assert!(
            got.contains("JMP __FAC_TO_INT16"),
            "helper tail-calls dependency:\n{got}"
        );
    }

    #[test]
    fn ph109_factors_per_imm_byte_fac() {
        let mut input = String::new();
        for _ in 0..4 {
            input.push_str("    LDA #$00\n");
            input.push_str("    LDY #$01\n");
            input.push_str("    JSR $B391\n");
            input.push_str("    JSR __PRINT_FAC\n");
        }
        let got = run_default(&input);
        assert!(
            got.contains("__LDV_01_FAC:"),
            "per-imm helper emitted:\n{got}"
        );
        assert!(got.contains("JSR __LDV_01_FAC"), "callsite rewritten");
        assert!(!got.contains("LDY #$01\n    JSR __LD_BYTE_FAC"));
    }

    #[test]
    fn ph109_skips_under_threshold_per_imm() {
        let mut input = String::new();
        for _ in 0..2 {
            input.push_str("    LDA #$00\n    LDY #$01\n    JSR $B391\n");
            input.push_str("    LDA #$00\n    LDY #$02\n    JSR $B391\n");
        }
        let got = run_default(&input);
        assert!(!got.contains("__LDV_"), "no per-imm helper:\n{got}");
    }

    #[test]
    fn ph301_factors_repeated_4window() {
        let mut input = String::new();
        for _ in 0..4 {
            input.push_str("    LDA $14\n");
            input.push_str("    STA $FD\n");
            input.push_str("    LDA $15\n");
            input.push_str("    STA $FE\n");

            input.push_str("    LDA ($FD),Y\n");
            input.push_str("    STA out\n");
        }
        let got = run_default(&input);
        assert!(got.contains("__SEQ_0:"), "helper emitted:\n{got}");
        assert!(got.contains("JSR __SEQ_0"), "callsite rewritten");
        // The inline sequence stays only in the helper, not at each site.
        assert_eq!(got.matches("LDA $14").count(), 1, "only helper has LDA $14");
    }

    #[test]
    fn factoring_family_fires_on_speed_profile() {
        let mut input = String::new();
        for _ in 0..4 {
            input.push_str("    LDA $14\n");
            input.push_str("    STA $FD\n");
            input.push_str("    LDA $15\n");
            input.push_str("    STA $FE\n");
            input.push_str("    LDA ($FD),Y\n");
            input.push_str("    STA out\n");
        }
        let got = run(&input, Profile::Speed);
        assert!(got.contains("__SEQ_0:"), "helper emitted on speed:\n{got}");
        assert!(got.contains("JSR __SEQ_0"), "callsite rewritten on speed");
        assert_eq!(
            got,
            run_default(&input),
            "speed and default agree on the factoring family"
        );
    }

    #[test]
    fn jsr_pair_family_fires_on_speed_profile() {
        let mut input = String::new();
        for _ in 0..6 {
            input.push_str("    JSR __HELP_A\n");
            input.push_str("    JSR __HELP_B\n");
            input.push_str("    NOP\n");
        }
        input.push_str("__HELP_A:\n    LDA $14\n    RTS\n");
        input.push_str("__HELP_B:\n    LDA $15\n    RTS\n");
        let got = run(&input, Profile::Speed);
        assert!(
            got.contains("__JSR2_0"),
            "pair stub emitted on speed:\n{got}"
        );
        assert_eq!(
            got,
            run_default(&input),
            "speed and default agree on the pair family"
        );
    }

    #[test]
    fn ph301_skips_under_threshold() {
        let mut input = String::new();
        for _ in 0..3 {
            input.push_str("    LDA $14\n    STA $FD\n    LDA $15\n    STA $FE\n");
            input.push_str("    NOP\n");
        }
        let got = run_default(&input);
        assert!(!got.contains("__SEQ_"), "no helper under threshold:\n{got}");
    }

    #[test]
    fn ph301_factors_stable_label_operand() {
        let mut input = String::new();
        for _ in 0..4 {
            input.push_str("    LDA #<__ARR_X\n");
            input.push_str("    STA $FD\n");
            input.push_str("    LDA #>__ARR_X\n");
            input.push_str("    STA $FE\n");

            input.push_str("    LDA ($FD),Y\n");
            input.push_str("    STA out\n");
        }
        let got = run_default(&input);
        assert!(
            got.contains("__SEQ_"),
            "stable labels DO get factored:\n{got}"
        );
    }

    #[test]
    fn ph301_refuses_basic_line_label() {
        let mut input = String::new();
        for _ in 0..4 {
            input.push_str("    LDA L100\n");
            input.push_str("    STA $FD\n");
            input.push_str("    LDA L100+1\n");
            input.push_str("    STA $FE\n");
            input.push_str("    NOP\n");
        }
        let got = run_default(&input);
        assert!(!got.contains("__SEQ_"), "line labels not factored:\n{got}");
    }

    #[test]
    fn ph301_refuses_jsr_in_first_pass() {
        let mut input = String::new();
        for _ in 0..4 {
            input.push_str("    LDA $14\n");
            input.push_str("    LDX $15\n");
            input.push_str("    LDY $16\n");
            input.push_str("    STA $FE\n");
            input.push_str("    NOP\n");
        }
        let mut items = parse(&input);
        assert!(ph013_drop_nop(&mut items));
        assert!(ph301_with_window(&mut items, 4, JsrPolicy::Refused));
        let got = render(&items);
        assert!(got.contains("__SEQ_"), "no-JSR 4-window factors:\n{got}");
    }

    #[test]
    fn ph301_factors_repeated_3window() {
        let mut input = String::new();
        for _ in 0..4 {
            input.push_str("    LDA V_A\n");
            input.push_str("    STA T0\n");
            input.push_str("    LDY #$00\n");
        }
        let mut items = parse(&input);
        assert!(ph301_with_window(&mut items, 3, JsrPolicy::Refused));
        let got = render(&items);
        assert!(got.contains("__SEQ_"), "3-window should factor:\n{got}");
    }

    #[test]
    fn ph301_factors_repeated_5window() {
        let mut input = String::new();
        for _ in 0..4 {
            input.push_str("    LDA V_A\n");
            input.push_str("    STA T0\n");
            input.push_str("    LDA V_B\n");
            input.push_str("    STA T1\n");
            input.push_str("    LDY #$00\n");
        }
        let mut items = parse(&input);
        assert!(ph301_with_window(&mut items, 5, JsrPolicy::Refused));
        let got = render(&items);
        assert!(got.contains("__SEQ_"), "5-window should factor:\n{got}");
    }

    #[test]
    fn ph301_late_pass_allows_single_jsr() {
        let mut input = String::new();
        for _ in 0..4 {
            input.push_str("    LDA $14\n");
            input.push_str("    JSR $BBA2\n");
            input.push_str("    LDA $15\n");
            input.push_str("    STA $FE\n");
            input.push_str("    JSR __PRINT_FAC\n");
            input.push_str("    NOP\n");
        }
        let got = run_default(&input);
        assert!(
            got.contains("__SEQ_"),
            "late JSR-tolerant pass factors:\n{got}"
        );
    }

    #[test]
    fn ph301_late_pass_rejects_two_jsr_window() {
        let mut input = String::new();
        for _ in 0..4 {
            input.push_str("    JSR $BBA2\n");
            input.push_str("    JSR $BBD4\n");
            input.push_str("    LDA $14\n");
            input.push_str("    STA $FE\n");
            input.push_str("    NOP\n");
        }
        let got = run_default(&input);
        assert!(
            !got.contains("__SEQ_"),
            "two-JSR window not factored:\n{got}"
        );
    }

    #[test]
    fn ph301_size_outlines_two_long_mixed_call_sites_without_changing_execution() {
        // Keep the pushed value live across the call.
        let body = concat!(
            "    LDA #$31\n    PHA\n    JSR __ADD\n    STA $0400\n",
            "    PLA\n    ADC #$01\n    JSR __ADD\n    EOR #$55\n",
            "    STA $0401\n    JSR __ADD\n    STX $0402\n    STY $0403\n",
        );
        let input = format!(
            "*=$2000\n__SITE0:\n{body}__SITE1:\n{body}    JMP $FFFE\n\
             __ADD:\n    CLC\n    ADC #$01\n    TAX\n    INY\n    RTS\n"
        );
        for profile in [Profile::Default, Profile::Speed, Profile::Size] {
            let mut items = parse(&input);
            assert!(!ph301_factor_late_with_jsr(&mut items, profile, false));
            assert_eq!(render(&items), render(&parse(&input)));
            if profile != Profile::Size {
                assert!(!ph301_factor_late_with_jsr(&mut items, profile, true));
                assert_eq!(render(&items), render(&parse(&input)));
            }
        }
        let mut items = parse(&input);
        assert!(ph301_factor_late_with_jsr(&mut items, Profile::Size, true));
        let outlined = render(&items);
        // The whole twelve-instruction group becomes one shared helper.
        assert_eq!(outlined.matches("JSR __SEQ_").count(), 2, "{outlined}");
        assert_eq!(outlined.matches("JSR __ADD").count(), 3, "{outlined}");

        let execute = |source: &str| {
            let mut assembler = asm6502::Assembler6502::new();
            let bytes = assembler.assemble_bytes(source).unwrap();
            let add = assembler.lookup("__ADD").unwrap() as u16;
            let mut system = emu64::System::new();
            system.mem.ram[0x2000..0x2000 + bytes.len()].copy_from_slice(&bytes);
            system.start_at(0x2000);
            system.cpu.regs.sp = 0xf0;
            system.cpu.regs.x = 7;
            system.cpu.regs.y = 10;
            let mut calls = 0;
            for _ in 0..1_000 {
                if system.cpu.regs.pc == 0xfffe {
                    break;
                }
                calls += usize::from(system.cpu.regs.pc == add);
                assert!(system.step());
            }
            assert_eq!(system.cpu.regs.pc, 0xfffe, "did not return");
            assert_eq!(system.cpu.regs.sp, 0xf0, "leaked a stack frame");
            assert_eq!(calls, 6, "missing or duplicated helper execution");
            (
                bytes.len(),
                system.cpu.regs.a,
                system.cpu.regs.x,
                system.cpu.regs.y,
                system.cpu.regs.p,
                system.mem.ram[0x400..0x404].to_vec(),
            )
        };
        let before = execute(&input);
        let after = execute(&outlined);
        assert!(after.0 < before.0, "the actual assembly must shrink");
        assert_eq!(
            (before.1, before.2, before.3, before.4, before.5),
            (after.1, after.2, after.3, after.4, after.5)
        );
    }

    #[test]
    fn ph301_size_two_sites_still_require_positive_assembled_savings() {

        let input = "__A:\n    INX\n    INY\n    DEX\n    DEY\n\
                     __B:\n    INX\n    INY\n    DEX\n    DEY\n";
        let mut items = parse(input);
        assert!(!ph301_factor_late_with_jsr(&mut items, Profile::Size, true));
    }

    #[test]
    fn ph301_size_relaxation_preserves_stack_and_control_barriers() {
        for body in [
            "    PLA\n    LDA $0400\n    STA $0401\n    PHA\n",
            "    LDA $0400\n    PHA\n    STA $0401\n    INX\n",
            "    LDA $0103,X\n    STA $0401\n    LDA $0402\n    STA $0403\n",
            "    TSX\n    LDA $0400\n    STA $0401\n    TXS\n",
            "    LDA $0400\n    JSR L100\n    STA $0401\n    INX\n",
            "    LDA $0400\n    BNE __OUT\n    STA $0401\n    INX\n",
        ] {
            let input = format!("__A:\n{body}__B:\n{body}");
            let mut items = parse(&input);
            assert!(
                !ph301_with_window_min_sites(&mut items, 4, JsrPolicy::AllowThree, 2),
                "{input}"
            );
        }
    }

    #[test]
    fn ph301_size_two_sites_preserve_smc_targets_and_sensitive_tail_aliases() {
        let input = concat!(
            "    STA __PATCH_A+1\n    STA __PATCH_B+1\n",
            "__PATCH_A:\n    LDA $0000,Y\n    STA $0401\n    INX\n    INY\n",
            "__PATCH_B:\n    LDA $0000,Y\n    STA $0401\n    INX\n    INY\n",
            "__CALL_A:\n    LDA $0400\n    JSR __ALIAS\n    STA $0401\n    INX\n",
            "__CALL_B:\n    LDA $0400\n    JSR __ALIAS\n    STA $0401\n    INX\n",
            "__ALIAS:\n    JMP __SYS_TXT_OUT\n",
        );
        let mut items = parse(input);
        assert!(!ph301_factor_late_with_jsr(&mut items, Profile::Size, true));
    }

    #[test]
    fn ph301_refuses_branch_in_window() {
        let mut input = String::new();
        for _ in 0..4 {
            input.push_str("    LDA $14\n");
            input.push_str("    BEQ skip\n");
            input.push_str("    LDA $15\n");
            input.push_str("    STA $FE\n");
            input.push_str("skip:\n");
            input.push_str("    NOP\n");
        }
        let got = run_default(&input);
        assert!(!got.contains("__SEQ_"));
    }

    #[test]
    fn ph303_keeps_a_label_named_only_by_a_split_byte_pointer_table() {
        let input = concat!(
            "    RTS\n",
            "__TAB_LO:\n",
            "    .byte <__DAT\n",
            "__TAB_HI:\n",
            "    .byte >__DAT\n",
            "__DAT:\n",
            "__NEXT:\n",
            "    .byte $01\n",
        );
        for profile in [Profile::Default, Profile::Size] {
            let got = run(input, profile);
            assert!(
                got.contains("__DAT:"),
                "a .byte <label reference pins its target ({profile:?}):\n{got}"
            );
        }
    }

    /// An unnamed stacked label is dropped.
    #[test]
    fn ph303_still_drops_a_stacked_label_nothing_names() {
        let input = concat!(
            "    RTS\n",
            "__TAB_LO:\n",
            "    .byte <__DAT\n",
            "__ORPHAN:\n",
            "__DAT:\n",
            "    .byte $01\n",
        );
        let got = run(input, Profile::Size);
        assert!(
            got.contains("__DAT:"),
            "the named target still survives:\n{got}"
        );
        assert!(
            !got.contains("__ORPHAN:"),
            "a stacked label nothing names is still dropped:\n{got}"
        );
    }

    #[test]
    fn a_comment_word_is_not_a_label_reference() {
        assert_eq!(
            directive_operand_labels("    .byte $01  ; touches __DAT here"),
            Vec::<String>::new()
        );
        assert_eq!(
            directive_operand_labels("    .byte <__DAT"),
            vec!["__DAT".to_string()]
        );
        assert_eq!(
            directive_operand_labels("    .word __A, __B"),
            vec!["__A".to_string(), "__B".to_string()]
        );
    }

    #[test]
    fn ph303_drops_unreferenced_trampoline() {
        let input = concat!(
            "    JMP __TRAMP\n",
            "    RTS\n",
            "__TRAMP:\n",
            "    JMP __REAL\n",
            "OTHER:\n",
            "    JSR __REAL\n",
            "    RTS\n",
            "__REAL:\n",
            "    INX\n",
            "    RTS\n",
        );
        let got = run_default(input);
        assert!(
            !got.contains("__TRAMP:"),
            "dead trampoline label dropped:\n{got}"
        );
        assert!(got.contains("__REAL"), "live target kept");
    }

    #[test]
    fn ph303_keeps_referenced_trampoline() {
        let input = concat!(
            "    JSR __TRAMP\n",
            "    RTS\n",
            "__TRAMP:\n",
            "    JMP __REAL\n",
            "__REAL:\n",
            "    RTS\n",
        );
        let got = run_default(input);
        assert!(
            !got.contains("JSR __TRAMP"),
            "JSR to absorbed trampoline must not survive:\n{got}"
        );
    }

    #[test]
    fn ph303_keeps_function_end_rts_after_inverted_branch() {
        let input = concat!(
            "L1:\n",
            "    JSR __BNDS_CHECK\n",
            "    RTS\n",
            "__BNDS_CHECK:\n",
            "    CMP $14\n",
            "    TXA\n",
            "    SBC $15\n",
            "    BCS __BC_OK\n",
            "    JMP __BAD_SUBSCRIPT\n",
            "__BC_OK:\n",
            "    RTS\n",
            "__BNDS_10:\n",
            "    LDA #<10\n",
            "    LDX #>10\n",
            "    JMP __BNDS_CHECK\n",
        );
        let got = run_default(input);
        assert!(
            got.lines().any(|l| l.trim() == "RTS"),
            "function-end RTS must survive PH303:\n{got}"
        );
    }

    #[test]
    fn ph303_drops_dead_branchy_stub() {
        let input = concat!(
            "L1:\n",
            "    LDA #$01\n",
            "    RTS\n",
            "__LD_XI:\n",
            "    LDY VI_X\n",
            "    TYA\n",
            "    ASL\n",
            "    LDA #$00\n",
            "    BCC __LD_XI_SXT\n",
            "    LDA #$FF\n",
            "__LD_XI_SXT:\n",
            "    JMP $B391\n",
        );
        let got = run_default(input);
        assert!(
            !got.contains("__LD_XI"),
            "dead branchy stub removed:\n{got}"
        );

        assert!(got.contains("LDA #$01"), "live entry code kept:\n{got}");
    }

    #[test]
    fn ph303_keeps_referenced_line_labels() {

        let input = "L10:\n    LDA $02\nL99:\n    DEC $02\n    BNE L99\n    RTS\n";
        let got = run_default(input);
        assert!(got.contains("L99:"), "referenced line label kept:\n{got}");
        assert!(got.contains("BNE L99"));
        assert!(!got.contains("L10:"), "dead entry label swept:\n{got}");
    }

    fn drive(pass: fn(&mut Vec<Item>) -> bool, asm: &str) -> (bool, String) {
        let mut items = parse(asm);
        let changed = pass(&mut items);
        (changed, render(&items))
    }

    #[test]
    fn ph302_f3b_inlines_a_single_call_trampoline() {
        let (changed, got) = drive(
            ph302_inline_single_call_helpers,
            "    JSR __JSR2_0\n    LDA #$01\n    RTS\n__JSR2_0:\n    JSR __FOO\n    JMP __BAR\n",
        );
        assert!(changed, "the trampoline must be inlined:\n{got}");
        assert!(!got.contains("__JSR2_0"), "helper and site both gone:\n{got}");
        assert!(
            got.contains("    JSR __FOO\n    JSR __BAR\n    LDA #$01"),
            "body spliced with the JMP rewritten to a JSR:\n{got}"
        );
    }

    #[test]
    fn ph302_f3b_keeps_the_jmp_at_a_tail_call_site() {
        // A `JMP __JSR2_0` site is already a tail call, so the trampoline's own `JMP B` is copied verbatim.
        let (changed, got) = drive(
            ph302_inline_single_call_helpers,
            "    JMP __JSR2_0\n__JSR2_0:\n    JSR __FOO\n    JMP __BAR\n",
        );
        assert!(changed, "the trampoline must be inlined:\n{got}");
        assert!(
            got.contains("    JSR __FOO\n    JMP __BAR"),
            "tail call preserved as a JMP:\n{got}"
        );
        assert!(!got.contains("__JSR2_0"), "helper gone:\n{got}");
    }

    #[test]
    fn ph302_f3b_refuses_a_trampoline_with_two_callers() {

        let (changed, got) = drive(
            ph302_inline_single_call_helpers,
            "    JSR __JSR2_0\n    JSR __JSR2_0\n    RTS\n\
             __JSR2_0:\n    JSR __FOO\n    JMP __BAR\n",
        );
        assert!(!changed, "two callers is not a single-caller helper:\n{got}");
        assert!(got.contains("__JSR2_0:"), "helper kept:\n{got}");
    }

    #[test]
    fn ph302_refuses_a_helper_whose_pushes_balance_only_across_its_tail_jump() {

        let broken = "    JSR __SEQ_18\n    LDA #$01\n    RTS\n\
                      __SEQ_18:\n    PHA\n    LDA V_X+4\n    PHA\n    JSR __LDV_0C_FAC\n\
                      \x20   JMP __TAIL_12\n\
                      __OTHER:\n    PHA\n    PHA\n    JMP __TAIL_12\n\
                      __TAIL_12:\n    PLA\n    STA V_X+4\n    PLA\n    RTS\n";
        let (changed, got) = drive(ph302_inline_single_call_helpers, broken);
        assert!(
            !changed,
            "a body that leaves two bytes on the stack must not be spliced in front of \
             a return address:\n{got}"
        );
        assert!(got.contains("__SEQ_18:"), "helper kept:\n{got}");

        let ok = "    JSR __SEQ_18\n    LDA #$01\n    RTS\n\
                  __SEQ_18:\n    PHA\n    LDA V_X+4\n    PHA\n    JSR __LDV_0C_FAC\n\
                  \x20   PLA\n    STA V_X+4\n    PLA\n    JMP __TAIL_12\n\
                  __TAIL_12:\n    RTS\n";
        let (changed, got) = drive(ph302_inline_single_call_helpers, ok);
        assert!(changed, "a balanced body is still inlinable:\n{got}");
        assert!(
            got.contains("    PLA\n    JSR __TAIL_12"),
            "the tail JMP became a JSR at the JSR site:\n{got}"
        );
    }

    #[test]
    fn ph302_refuses_a_helper_whose_only_reference_is_its_own_body() {

        let (changed, got) = drive(
            ph302_inline_single_call_helpers,
            "    JMP __SEQ_0\n__SEQ_0:\n    LDA #$01\n    JSR __SEQ_0\n    RTS\n",
        );
        assert!(!changed, "a self-call must not be spliced:\n{got}");
        assert!(got.contains("__SEQ_0:"), "helper kept:\n{got}");
        assert!(got.contains("JSR __SEQ_0"), "the self-call kept:\n{got}");
    }

    #[test]
    fn ph308_refuses_a_body_that_does_not_balance() {
        let (changed, got) = drive(
            ph308_inline_uneconomic_helpers,
            "    JSR __SEQ_0\n    NOP\n    JSR __SEQ_0\n    RTS\n\
             __SEQ_0:\n    PHA\n    RTS\n",
        );
        assert!(!changed, "an unbalanced body must not be spliced:\n{got}");

        let (changed, got) = drive(
            ph308_inline_uneconomic_helpers,
            "    JSR __SEQ_0\n    NOP\n    JSR __SEQ_0\n    RTS\n\
             __SEQ_0:\n    PHA\n    PLA\n    RTS\n",
        );
        assert!(changed, "a balanced body is still inlinable:\n{got}");
    }

    #[test]
    fn ph302_f3b_refuses_an_indirect_tail_jump() {
        // `JMP (vec)` has no `JSR` form to rewrite into.
        let (changed, got) = drive(
            ph302_inline_single_call_helpers,
            "    JSR __JSR2_0\n    RTS\n__JSR2_0:\n    JSR __FOO\n    JMP ($C000)\n",
        );
        assert!(!changed, "an indirect tail jump is not splicable:\n{got}");
    }

    #[test]
    fn ph308_f3c_inlines_a_helper_that_costs_more_than_it_saves() {
        // Body is one byte plus the RTS.
        let (changed, got) = drive(
            ph308_inline_uneconomic_helpers,
            "    JSR __SEQ_0\n    LDA #$01\n    JSR __SEQ_0\n    RTS\n__SEQ_0:\n    INX\n    RTS\n",
        );
        assert!(changed, "a net-negative helper must be inlined:\n{got}");
        assert!(!got.contains("__SEQ_0"), "helper and both sites gone:\n{got}");
        assert_eq!(
            got.matches("    INX\n").count(),
            2,
            "the body lands at both sites:\n{got}"
        );
    }

    #[test]
    fn ph308_f3c_keeps_a_helper_that_pays_for_itself() {

        let body = "    LDA $1234\n    STA $1235\n    LDA $1236\n    STA $1237\n";
        let mut input = String::new();
        for _ in 0..4 {
            input.push_str("    JSR __SEQ_0\n    NOP\n");
        }
        input.push_str("    RTS\n__SEQ_0:\n");
        input.push_str(body);
        input.push_str("    RTS\n");
        let (changed, got) = drive(ph308_inline_uneconomic_helpers, &input);
        assert!(!changed, "a profitable helper must be left alone:\n{got}");
        assert!(got.contains("__SEQ_0:"), "helper kept:\n{got}");
    }

    #[test]
    fn ph308_f3c_refuses_a_helper_named_by_data() {

        let (changed, got) = drive(
            ph308_inline_uneconomic_helpers,
            "    JSR __SEQ_0\n    JSR __SEQ_0\n    RTS\n\
             __SEQ_0:\n    INX\n    RTS\n__VEC:\n    .word __SEQ_0\n",
        );
        assert!(!changed, "a data reference blocks the inline:\n{got}");
        assert!(got.contains("__SEQ_0:"), "helper kept:\n{got}");
    }

    /// Two `JSR __SEQ_0` sites sharing a predecessor, with `pred` as the instruction in front of each.
    fn absorb_pred_fixture(pred: &str) -> String {
        format!(
            "    {pred}\n    JSR __SEQ_0\n    LDA #$07\n    {pred}\n    JSR __SEQ_0\n    RTS\n\
             __SEQ_0:\n    STA $C000\n    RTS\n"
        )
    }

    #[test]
    fn ph309_f3a_absorbs_a_shared_predecessor() {
        let (changed, got) = drive(ph309_absorb_common_pred, &absorb_pred_fixture("LDY #$00"));
        assert!(changed, "the shared predecessor must be absorbed:\n{got}");
        assert_eq!(
            got.matches("LDY #$00").count(),
            1,
            "two copies became one:\n{got}"
        );
        assert!(
            got.contains("__SEQ_0:\n    LDY #$00\n    STA $C000"),
            "the survivor is the helper's new first instruction:\n{got}"
        );
    }

    #[test]
    fn ph309_f3a_refuses_to_move_a_jsr_across_the_call_boundary() {
        // THE hardened rule, and the one the study's reference implementation gets wrong.
        let (changed, got) = drive(ph309_absorb_common_pred, &absorb_pred_fixture("JSR __P"));
        assert!(!changed, "a JSR must never be absorbed:\n{got}");
        assert_eq!(
            got.matches("JSR __P").count(),
            2,
            "both copies stay at their call sites:\n{got}"
        );
    }

    #[test]
    fn ph309_f3a_refuses_every_stack_touching_predecessor() {

        for pred in ["PHA", "PHP", "PLA", "PLP", "TSX", "TXS", "JMP __P", "BEQ __P"] {
            let (changed, got) = drive(ph309_absorb_common_pred, &absorb_pred_fixture(pred));
            assert!(!changed, "{pred} must not be absorbed:\n{got}");
        }
    }

    #[test]
    fn ph309_f3a_refuses_a_runtime_patched_operand_and_the_stack_page() {
        for pred in ["STA $FFFF", "LDA $0100,Y"] {
            let (changed, got) = drive(ph309_absorb_common_pred, &absorb_pred_fixture(pred));
            assert!(!changed, "{pred} must not be absorbed:\n{got}");
        }

        for pred in ["STA $FFFE", "LDA $0200,Y"] {
            let (changed, got) = drive(ph309_absorb_common_pred, &absorb_pred_fixture(pred));
            assert!(changed, "{pred} is ordinary and must be absorbed:\n{got}");
        }
    }

    #[test]
    fn ph309_f3a_refuses_predecessors_that_are_not_identical() {
        let (changed, got) = drive(
            ph309_absorb_common_pred,
            "    LDY #$00\n    JSR __SEQ_0\n    LDY #$01\n    JSR __SEQ_0\n    RTS\n\
             __SEQ_0:\n    STA $C000\n    RTS\n",
        );
        assert!(!changed, "different operands are not a shared pred:\n{got}");
    }

    #[test]
    fn ph309_f3a_refuses_a_predecessor_that_is_a_branch_target() {
        let (changed, got) = drive(
            ph309_absorb_common_pred,
            "    BNE L1\n    LDY #$00\n    JSR __SEQ_0\nL1:\n    LDY #$00\n    JSR __SEQ_0\n\
                 RTS\n__SEQ_0:\n    STA $C000\n    RTS\n",
        );
        assert!(!changed, "a labelled predecessor must not move:\n{got}");
    }

    #[test]
    fn ph309_f3a_refuses_a_helper_reached_by_a_tail_jump() {
        // The jump reference has no call site to rewrite.
        let (changed, got) = drive(
            ph309_absorb_common_pred,
            "    LDY #$00\n    JSR __SEQ_0\n    LDY #$00\n    JSR __SEQ_0\n    LDY #$00\n\
                 JMP __SEQ_0\n__SEQ_0:\n    STA $C000\n    RTS\n",
        );
        assert!(!changed, "an unaccounted entry blocks absorption:\n{got}");
    }

    #[test]
    fn ph309_f3a_refuses_a_helper_that_can_be_fallen_into() {
        let (changed, got) = drive(
            ph309_absorb_common_pred,
            "    LDY #$00\n    JSR __SEQ_0\n    LDY #$00\n    JSR __SEQ_0\n    LDA #$01\n\
             __SEQ_0:\n    STA $C000\n    RTS\n",
        );
        assert!(!changed, "a fall-through entry blocks absorption:\n{got}");
    }

    fn absorb_succ_fixture(succ: &str) -> String {
        format!(
            "    JSR __SEQ_0\n    {succ}\n    LDA #$07\n    JSR __SEQ_0\n    {succ}\n    RTS\n\
             __SEQ_0:\n    STA $C000\n    RTS\n"
        )
    }

    #[test]
    fn ph310_f4_absorbs_a_shared_successor() {
        let (changed, got) = drive(ph310_absorb_common_succ, &absorb_succ_fixture("INY"));
        assert!(changed, "the shared successor must be absorbed:\n{got}");
        assert_eq!(got.matches("INY").count(), 1, "two copies became one:\n{got}");
        assert!(
            got.contains("    STA $C000\n    INY\n    RTS"),
            "the survivor sits immediately before the RTS:\n{got}"
        );
    }

    #[test]
    fn ph310_f4_refuses_the_same_stack_touching_set() {
        // A successor inside the helper observes its return address on the stack.
        for succ in ["PHA", "PLA", "TSX", "TXS", "JSR __P", "JMP __P", "STA $FFFF"] {
            let (changed, got) = drive(ph310_absorb_common_succ, &absorb_succ_fixture(succ));
            assert!(!changed, "{succ} must not be absorbed:\n{got}");
        }
    }

    #[test]
    fn ph310_f4_refuses_a_successor_that_carries_a_label() {
        let (changed, got) = drive(
            ph310_absorb_common_succ,
            "    JSR __SEQ_0\n    INY\n    LDA #$07\n    JSR __SEQ_0\nL1:\n    INY\n    RTS\n\
             __SEQ_0:\n    STA $C000\n    RTS\n",
        );
        assert!(!changed, "a labelled successor must not move:\n{got}");
    }

    #[test]
    fn ph310_f4_refuses_a_tail_call_trampoline() {

        let (changed, got) = drive(
            ph310_absorb_common_succ,
            "    JSR __JSR2_0\n    INY\n    LDA #$07\n    JSR __JSR2_0\n    INY\n    RTS\n\
             __JSR2_0:\n    JSR __FOO\n    JMP __BAR\n",
        );
        assert!(!changed, "a JMP-terminated helper has no epilogue:\n{got}");
    }

    #[test]
    fn the_absorbing_passes_are_a_no_op_where_there_is_nothing_to_absorb() {

        let plain = "    LDY #$00\n    JSR __PRINT_FAC\n    LDY #$00\n    JSR __PRINT_FAC\n    RTS\n";
        for pass in [
            ph309_absorb_common_pred,
            ph310_absorb_common_succ,
            ph308_inline_uneconomic_helpers,
            ph302_inline_single_call_helpers,
            ph311_merge_duplicate_helpers,
            ph312_merge_helper_tails,
        ] {
            let (changed, got) = drive(pass, plain);
            assert!(!changed, "no helper, no work:\n{got}");
            assert_eq!(got, plain, "the stream came back byte-identical:\n{got}");
        }
    }

    fn dup_fixture(second_body: &str, extra: &str) -> String {
        format!(
            "    JSR __SEQ_3\n    JSR __SEQ_3\n    JSR __SEQ_7\n    JSR __SEQ_7\n{extra}    RTS\n\
             __SEQ_3:\n    LDA V_A\n    STA $C000\n    RTS\n\
             __SEQ_7:\n{second_body}"
        )
    }

    #[test]
    fn ph311_merges_two_helpers_with_identical_bodies() {
        let (changed, got) = drive(
            ph311_merge_duplicate_helpers,
            &dup_fixture("    LDA V_A\n    STA $C000\n    RTS\n", ""),
        );
        assert!(changed, "identical bodies must merge:\n{got}");
        assert!(!got.contains("__SEQ_7:"), "the loser's body is gone:\n{got}");
        assert_eq!(
            got.matches("JSR __SEQ_3").count(),
            4,
            "all four sites now call the survivor:\n{got}"
        );
        assert_eq!(
            got.matches("STA $C000").count(),
            1,
            "the body exists once:\n{got}"
        );
    }

    #[test]
    fn ph311_refuses_bodies_that_differ_by_one_operand() {

        let (changed, got) = drive(
            ph311_merge_duplicate_helpers,
            &dup_fixture("    LDA V_A\n    STA $C001\n    RTS\n", ""),
        );
        assert!(!changed, "different bodies must not merge:\n{got}");
        assert!(got.contains("__SEQ_7:"), "both helpers kept:\n{got}");
    }

    #[test]
    fn ph311_refuses_a_runtime_patched_body() {
        // `$FFFF` is the tree-wide placeholder for an operand the program rewrites before it runs.
        let (changed, got) = drive(
            ph311_merge_duplicate_helpers,
            &dup_fixture("    LDA V_A\n    STA $FFFF\n    RTS\n", "")
                .replace("    STA $C000\n    RTS\n__SEQ_7", "    STA $FFFF\n    RTS\n__SEQ_7"),
        );
        assert!(!changed, "a $FFFF body must not be shared:\n{got}");
        assert!(got.contains("__SEQ_7:"), "both helpers kept:\n{got}");
    }

    #[test]
    fn ph311_refuses_an_smc_patched_representative() {
        let (changed, got) = drive(
            ph311_merge_duplicate_helpers,
            &dup_fixture("    LDA V_A\n    STA $C000\n    RTS\n", "    STA __SEQ_3+1\n"),
        );
        assert!(
            !changed,
            "callers must not be adopted onto a patched body:\n{got}"
        );
        assert!(got.contains("__SEQ_7:"), "both helpers kept:\n{got}");
    }

    #[test]
    fn ph311_refuses_a_word_captured_helper() {
        // The word reference cannot be redirected by this pass.
        let (changed, got) = drive(
            ph311_merge_duplicate_helpers,
            &format!(
                "{}    .word __SEQ_7\n",
                dup_fixture("    LDA V_A\n    STA $C000\n    RTS\n", "")
            ),
        );
        assert!(!changed, "a .word-captured helper must not merge:\n{got}");
        assert!(got.contains("__SEQ_7:"), "both helpers kept:\n{got}");
    }

    #[test]
    fn ph311_refuses_a_helper_that_can_be_fallen_into() {

        let (changed, got) = drive(
            ph311_merge_duplicate_helpers,
            "    JSR __SEQ_3\n    JSR __SEQ_3\n    JSR __SEQ_7\n    JSR __SEQ_7\n    RTS\n\
             __SEQ_3:\n    LDA V_A\n    STA $C000\n    RTS\n\
             __FALL:\n    LDA #$02\n\
             __SEQ_7:\n    LDA V_A\n    STA $C000\n    RTS\n",
        );
        assert!(!changed, "a fall-through entry must not merge:\n{got}");
    }

    #[test]
    fn ph312_replaces_a_shared_tail_with_a_jump() {
        // Sharing the seven byte suffix replaces it with a three byte jump.
        let (changed, got) = drive(
            ph312_merge_helper_tails,
            "    JSR __SEQ_3\n    JSR __SEQ_9\n    RTS\n\
             __SEQ_3:\n    LDA V_A\n    TAY\n    LDA V_B\n    STA $C000\n    RTS\n\
             __SEQ_9:\n    LDA V_B\n    STA $C000\n    RTS\n",
        );
        assert!(changed, "the shared tail must become a jump:\n{got}");
        assert!(
            got.contains("__SEQ_3:\n    LDA V_A\n    TAY\n    JMP __SEQ_9\n"),
            "source keeps its prefix and tail-jumps:\n{got}"
        );
        assert!(
            got.contains("__SEQ_9:\n    LDA V_B\n    STA $C000\n    RTS\n"),
            "the target is untouched:\n{got}"
        );
    }

    #[test]
    fn ph312_refuses_a_tail_too_short_to_pay_for_the_jump() {

        let (changed, got) = drive(
            ph312_merge_helper_tails,
            "    JSR __SEQ_3\n    JSR __SEQ_9\n    RTS\n\
             __SEQ_3:\n    LDA V_A\n    STA $C000\n    TAY\n    RTS\n\
             __SEQ_9:\n    TAY\n    RTS\n",
        );
        assert!(!changed, "a 2-byte tail must not cost 3 bytes:\n{got}");
        assert!(!got.contains("JMP __SEQ_9"), "no jump was planted:\n{got}");

        let (changed, got) = drive(
            ph312_merge_helper_tails,
            "    JSR __SEQ_3\n    JSR __SEQ_9\n    RTS\n\
             __SEQ_3:\n    LDA V_A\n    STA $C000\n    TAY\n    RTS\n\
             __SEQ_9:\n    STA $C000\n    TAY\n    RTS\n",
        );
        assert!(changed, "a 5-byte tail pays for the jump:\n{got}");
        assert!(
            got.contains("__SEQ_3:\n    LDA V_A\n    JMP __SEQ_9\n"),
            "source tail-jumps:\n{got}"
        );
    }

    #[test]
    fn ph312_refuses_an_equal_length_match_and_never_cycles() {

        let (changed, got) = drive(
            ph312_merge_helper_tails,
            &dup_fixture("    LDA V_A\n    STA $C000\n    RTS\n", ""),
        );
        assert!(!changed, "an exact duplicate is not a tail merge:\n{got}");
        assert!(
            !got.contains("JMP __SEQ_3") && !got.contains("JMP __SEQ_7"),
            "no helper may jump to the other:\n{got}"
        );
    }

    #[test]
    fn ph312_refuses_a_runtime_patched_tail() {
        let (changed, got) = drive(
            ph312_merge_helper_tails,
            "    JSR __SEQ_3\n    JSR __SEQ_9\n    RTS\n\
             __SEQ_3:\n    LDA V_A\n    TAY\n    LDA V_B\n    STA $FFFF\n    RTS\n\
             __SEQ_9:\n    LDA V_B\n    STA $FFFF\n    RTS\n",
        );
        assert!(!changed, "a $FFFF tail must not be shared:\n{got}");
    }

    #[test]
    fn ph312_keeps_a_jsr_in_the_tail_because_a_jmp_moves_no_stack() {

        let (changed, got) = drive(
            ph312_merge_helper_tails,
            "    JSR __SEQ_3\n    JSR __SEQ_9\n    RTS\n\
             __SEQ_3:\n    LDA V_A\n    TAY\n    JSR __FOO\n    STA $C000\n    RTS\n\
             __SEQ_9:\n    JSR __FOO\n    STA $C000\n    RTS\n",
        );
        assert!(changed, "a JSR tail is a legitimate merge:\n{got}");
        assert!(
            got.contains("__SEQ_3:\n    LDA V_A\n    TAY\n    JMP __SEQ_9\n"),
            "the JSR tail moved behind the jump:\n{got}"
        );
    }

    #[test]
    fn ph013_drops_plain_nop() {
        let input = "    NOP\n    RTS\n";
        let got = run_default(input);
        assert!(!got.contains("NOP"));
    }

    #[test]
    fn ph013_keeps_marked_nop() {
        let input = "    NOP ; @keep_nop padding for $D000 timing\n";
        let got = run_default(input);
        assert!(got.contains("NOP"), "annotated NOP preserved:\n{got}");
    }

    #[test]
    fn ph101_drops_movfm_after_movmf_same_label() {
        let input = concat!(
            "    LDX #<V_X\n",
            "    LDY #>V_X\n",
            "    JSR $BBD4\n",
            "    LDA #<V_X\n",
            "    LDY #>V_X\n",
            "    JSR $BBA2\n",
        );
        let got = run_default(input);
        assert!(got.contains("$BBD4"), "MOVMF kept");
        assert!(!got.contains("$BBA2"), "redundant MOVFM dropped:\n{got}");
    }

    #[test]
    fn ph101_keeps_when_label_between() {
        let input = concat!(
            "    LDX #<V_X\n",
            "    LDY #>V_X\n",
            "    JSR $BBD4\n",
            "KEEP1:\n",
            "    LDA #<V_X\n",
            "    LDY #>V_X\n",
            "    JSR $BBA2\n",
        );
        let got = run_default(input);
        assert!(got.contains("$BBA2"), "MOVFM kept across label:\n{got}");
    }

    #[test]
    fn fac_liveness_tracks_movmf_use_and_movfm_def() {
        let items = parse(concat!(
            "    LDA #<V_X\n",
            "    LDY #>V_X\n",
            "    JSR $BBA2\n",
            "    JSR __PRINT_FAC\n",
        ));
        let live = analyze_fac_liveness(&items);
        assert!(!live.is_live_in(0), "MOVFM does not need incoming FAC");
        assert!(live.is_live_out(2), "PRINT needs FAC produced by MOVFM");
        assert!(live.is_live_in(3), "PRINT reads FAC");
    }

    #[test]
    fn ph102_drops_movfm_when_fac_still_matches_slot() {
        let input = concat!(
            "    LDX #<T0\n",
            "    LDY #>T0\n",
            "    JSR $BBD4\n",
            "    STA $0400\n",
            "    LDA #<T0\n",
            "    LDY #>T0\n",
            "    JSR $BBA2\n",
            "    JSR __PRINT_FAC\n",
        );
        let got = run_default(input);
        assert!(got.contains("$BBD4"), "MOVMF kept");
        assert!(!got.contains("$BBA2"), "redundant MOVFM dropped:\n{got}");
        assert!(got.contains("JSR __PRINT_FAC"));
    }

    #[test]
    fn ph102_keeps_movfm_after_fac_clobber() {
        let input = concat!(
            "    LDX #<T0\n",
            "    LDY #>T0\n",
            "    JSR $BBD4\n",
            "    LDA #$01\n",
            "    LDY #$00\n",
            "    JSR $B391\n",
            "    JSR __PRINT_FAC\n", // observe GIVAYF's FAC so it survives
            "    LDA #<T0\n",
            "    LDY #>T0\n",
            "    JSR $BBA2\n",
            "    JSR __PRINT_FAC\n",
        );
        let got = run_default(input);
        assert!(
            got.contains("$BBA2"),
            "MOVFM needed after FAC clobber:\n{got}"
        );
        assert!(got.contains("$B391"), "GIVAYF clobber preserved:\n{got}");
    }

    #[test]
    fn ph150_drops_movfm_when_fac_overwritten() {
        let input = concat!(
            "    LDA #<F0\n",
            "    LDY #>F0\n",
            "    JSR $BBA2\n",
            "    LDA #<F1\n",
            "    LDY #>F1\n",
            "    JSR $BBA2\n",
            "    JSR __PRINT_FAC\n",
        );
        let got = run_default(input);
        assert_eq!(
            got.matches("JSR $BBA2").count(),
            1,
            "first MOVFM dropped:\n{got}"
        );
        assert!(got.contains("JSR __PRINT_FAC"));
    }

    #[test]
    fn ph150_keeps_movfm_when_fac_used_downstream() {
        let input = concat!(
            "    LDA #<F0\n",
            "    LDY #>F0\n",
            "    JSR $BBA2\n",
            "    JSR __PRINT_FAC\n",
        );
        let got = run_default(input);
        assert!(
            got.contains("JSR $BBA2"),
            "MOVFM with live use kept:\n{got}"
        );
    }

    #[test]
    fn ph150_does_not_drop_fadd() {
        let input = concat!(
            "    LDA #<F0\n",
            "    LDY #>F0\n",
            "    JSR $BBA2\n",
            "    LDA #<F1\n",
            "    LDY #>F1\n",
            "    JSR $B867\n",
            "    LDA #<F2\n",
            "    LDY #>F2\n",
            "    JSR $BBA2\n",
            "    JSR __PRINT_FAC\n",
        );
        let got = run_default(input);
        assert!(
            got.contains("JSR $B867"),
            "FADD with overflow trap kept:\n{got}"
        );
    }

    #[test]
    fn ph150_drops_givayf_with_dead_result() {
        let input = concat!(
            "    LDA #$00\n",
            "    LDY #$2A\n",
            "    JSR $B391\n",
            "    LDA #<F0\n",
            "    LDY #>F0\n",
            "    JSR $BBA2\n",
            "    JSR __PRINT_FAC\n",
        );
        let got = run_default(input);
        assert!(!got.contains("JSR $B391"), "dead GIVAYF dropped:\n{got}");
        assert!(got.contains("JSR $BBA2"));
    }

    #[test]
    fn ph150_keeps_movfm_when_register_is_live() {
        let input = concat!(
            "    LDA #<F0\n",
            "    LDY #>F0\n",
            "    JSR $BBA2\n",
            "    LDA #<F1\n",
            "    LDY #>F1\n",
            "    JSR $BBA2\n",
            "    LDA $0400,Y\n",
            "    STA $0401\n",
        );
        let got = run_default(input);
        assert_eq!(got.matches("JSR $BBA2").count(), 1, "{got}");
        assert!(got.contains("LDA $0400,Y"));
    }

    #[test]
    fn ph104_turns_fac_sign_cmp_bne_into_bmi() {
        let input = concat!(
            "    LDA $66\n",
            "    CMP #$00\n",
            "    BNE NEG\n",
            "    RTS\n",
            "NEG:\n",
            "    RTS\n",
        );
        let got = run_default(input);
        assert!(got.contains("BMI NEG"), "sign branch rewritten:\n{got}");
        assert!(!got.contains("CMP #$00"), "compare removed:\n{got}");
    }

    #[test]
    fn ph004_collapses_trampoline() {
        let input = concat!(
            "    JMP TRAMP\n",
            "    RTS\n",
            "TRAMP:\n",
            "    JMP REAL\n",
            "REAL:\n",
            "    INX\n",
            "    RTS\n",
        );
        let got = run_default(input);
        assert!(!got.contains("JMP TRAMP"), "trampoline collapsed:\n{got}");
        assert!(!got.contains("JMP REAL"), "cascade removed the jump:\n{got}");
        assert!(got.contains("INX"));
    }

    #[test]
    fn ph004_collapses_branch_to_trampoline() {
        let input = concat!(
            "    BEQ TRAMP\n",
            "    LDA #$00\n",
            "    RTS\n",
            "TRAMP:\n",
            "    JMP REAL\n",
            "REAL:\n",
            "    RTS\n",
        );
        let got = run_default(input);
        assert!(
            got.contains("BEQ REAL"),
            "branch retargeted past trampoline:\n{got}"
        );
    }

    #[test]
    fn ph005_inverts_branch_around_jmp() {

        let input = concat!(
            "    BEQ L_true\n",
            "    JMP L_false\n",
            "L_true:\n",
            "    LDA #$01\n",
            "    RTS\n",
            "L_false:\n",
            "    LDA #$02\n",
            "    RTS\n",
        );
        let got = run_default(input);
        assert!(
            got.contains("BNE L_false"),
            "branch inverted to fall-through:\n{got}"
        );
        assert!(!got.contains("JMP L_false"), "JMP collapsed");
        assert!(got.contains("L_true:"), "label kept");
    }

    #[test]
    fn ph005_refuses_jmp_to_rom_address() {
        let input = concat!("    BEQ L_t\n", "    JMP $AF08\n", "L_t:\n", "    RTS\n",);
        let got = run_default(input);
        assert!(
            got.contains("BEQ L_t"),
            "branch preserved when JMP targets ROM:\n{got}"
        );
        assert!(got.contains("JMP $AF08"), "JMP preserved");
    }

    #[test]
    fn ph004_refuses_to_retarget_branch_to_rom_address() {
        let input = concat!(
            "    BEQ TRAMP\n",
            "    RTS\n",
            "TRAMP:\n",
            "    JMP $AF08\n",
        );
        let got = run_default(input);
        assert!(
            got.contains("BEQ TRAMP"),
            "branch kept on trampoline label:\n{got}"
        );
    }

    #[test]
    fn ph005_handles_all_branch_types() {
        for (orig, inv) in [
            ("BCC", "BCS"),
            ("BCS", "BCC"),
            ("BMI", "BPL"),
            ("BPL", "BMI"),
            ("BVC", "BVS"),
            ("BVS", "BVC"),
        ] {

            let input = format!(
                "    {orig} L_t\n    JMP L_f\nL_t:\n    LDA #$01\n    RTS\nL_f:\n    LDA #$02\n    RTS\n"
            );
            let got = run_default(&input);
            assert!(
                got.contains(&format!("{inv} L_f")),
                "{orig} should invert to {inv}:\n{got}"
            );
        }
    }

    /// Generate labeled branch-distance padding.
    fn ph005_reach_probe(n: usize) -> String {
        let mut filler = String::new();
        for _ in 0..n {
            // `LDA #$00` is 2 (immediate) and `STA $39` is 2 (zp).
            filler.push_str("    LDA #$00\n    STA $39\n");
        }
        format!("    BEQ L_t\n    JMP L_far\nL_t:\n    RTS\nL_keep:\n{filler}L_far:\n    RTS\n")
    }

    #[test]
    fn ph005_skips_when_target_too_far() {

        let mut items = parse(&ph005_reach_probe(40));
        assert!(
            !ph005_branch_around_jmp(&mut items),
            "direct PH005 should refuse the long branch"
        );
        let got = render(&items);
        assert!(
            got.contains("BEQ L_t"),
            "branch left alone when target out of reach:\n{got}"
        );
        assert!(got.contains("JMP L_far"));
    }

    #[test]
    fn ph005_folds_at_the_full_hardware_reach() {
        let mut items = parse(&ph005_reach_probe(30));
        assert!(
            ph005_branch_around_jmp(&mut items),
            "PH005 must take a target that is 100 < d <= 127 bytes away"
        );
        let got = render(&items);
        assert!(got.contains("BNE L_far"), "inverted to the JMP target:\n{got}");
        assert!(!got.contains("JMP L_far"), "the JMP is gone:\n{got}");
    }


    #[test]
    fn ph315_folds_the_heap_guard_shape() {
        let input = concat!(
            "    CPX #$02\n",
            "    BCS __HGIL_NO\n",
            "__HGIL_GC:\n",
            "    JMP __GC_RUN\n",
            "__HGIL_NO:\n",
            "    RTS\n",
            "__GC_RUN:\n",
            "    LDA #<__HEAP_BOTTOM\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        assert!(
            ph315_branch_over_jmp_past_dead_labels(&mut items),
            "the orphan must not block the fold"
        );
        let got = render(&items);
        assert!(got.contains("BCC __GC_RUN"), "branch inverted:\n{got}");
        assert!(!got.contains("JMP __GC_RUN"), "trampoline JMP gone:\n{got}");
        assert!(got.contains("__HGIL_GC:"), "orphan label left for PH029:\n{got}");
        assert!(got.contains("__HGIL_NO:"), "branch target label kept:\n{got}");
    }

    #[test]
    fn ph315_folds_the_for_back_edge_shape() {
        let input = concat!(
            "__FOR_TOP_0:\n",
            "    INC $FB\n",
            "    LDA $66\n",
            "    BPL __FOR_EXIT_0\n",
            "__FOR_CONT_0:\n",
            "    JMP __FOR_TOP_0\n",
            "__FOR_EXIT_0:\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        assert!(ph315_branch_over_jmp_past_dead_labels(&mut items));
        let got = render(&items);
        assert!(got.contains("BMI __FOR_TOP_0"), "branch inverted:\n{got}");
        assert!(!got.contains("JMP __FOR_TOP_0"), "back-edge JMP gone:\n{got}");
        assert!(got.contains("__FOR_CONT_0:"), "labels kept:\n{got}");
        assert!(got.contains("__FOR_EXIT_0:"), "labels kept:\n{got}");
    }

    #[test]
    fn ph315_takes_stacked_labels_after_the_jmp() {
        let input = concat!(
            "    BEQ L_true\n",
            "    JMP L_false\n",
            "L_OTHER:\n",
            "L_true:\n",
            "    LDA #<L_OTHER\n",
            "    RTS\n",
            "L_false:\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        assert!(
            ph315_branch_over_jmp_past_dead_labels(&mut items),
            "a referenced alias may stack under the target"
        );
        let got = render(&items);
        assert!(got.contains("BNE L_false"), "branch inverted:\n{got}");
        assert!(!got.contains("JMP L_false"), "JMP gone:\n{got}");
        assert!(got.contains("L_OTHER:"), "alias kept:\n{got}");
        assert!(got.contains("L_true:"), "target kept:\n{got}");
    }

    #[test]
    fn ph315_refuses_an_orphan_named_by_a_word_table() {
        let input = concat!(
            "    BCS L_NO\n",
            "ORPH:\n",
            "    JMP L_GC\n",
            "L_NO:\n",
            "    RTS\n",
            "L_GC:\n",
            "    .word ORPH\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        assert!(!ph315_branch_over_jmp_past_dead_labels(&mut items));
        let got = render(&items);
        assert!(got.contains("BCS L_NO"), "branch untouched:\n{got}");
        assert!(got.contains("JMP L_GC"), "JMP untouched:\n{got}");
    }

    #[test]
    fn ph315_refuses_an_orphan_named_by_an_immediate() {
        let input = concat!(
            "    BCS L_NO\n",
            "ORPH:\n",
            "    JMP L_GC\n",
            "L_NO:\n",
            "    RTS\n",
            "L_GC:\n",
            "    LDA #<ORPH\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        assert!(!ph315_branch_over_jmp_past_dead_labels(&mut items));
        let got = render(&items);
        assert!(got.contains("JMP L_GC"), "JMP untouched:\n{got}");
    }

    fn ph315_reach_probe(n: usize) -> String {
        let mut filler = String::new();
        for _ in 0..n {
            filler.push_str("    LDA #$00\n    STA $39\n");
        }
        format!(
            "    BEQ L_t\nORPH:\n    JMP L_far\nL_t:\n    RTS\nL_keep:\n{filler}L_far:\n    RTS\n"
        )
    }

    #[test]
    fn ph315_folds_when_the_jmp_target_is_in_reach() {
        let mut items = parse(&ph315_reach_probe(30));
        assert!(ph315_branch_over_jmp_past_dead_labels(&mut items));
        let got = render(&items);
        assert!(got.contains("BNE L_far"), "inverted to the JMP target:\n{got}");
        assert!(!got.contains("JMP L_far"), "JMP gone:\n{got}");
    }

    #[test]
    fn ph315_refuses_when_the_target_is_out_of_reach() {
        let mut items = parse(&ph315_reach_probe(40));
        assert!(
            !ph315_branch_over_jmp_past_dead_labels(&mut items),
            "the long-branch fix-up would cost more than the JMP saves"
        );
        let got = render(&items);
        assert!(got.contains("BEQ L_t"), "branch left alone:\n{got}");
        assert!(got.contains("JMP L_far"), "JMP left alone:\n{got}");
    }

    #[test]
    fn ph315_refuses_a_directive_inside_the_window() {
        let input = concat!(
            "    BEQ L_t\n",
            "ORPH:\n",
            "    .byte $00\n",
            "    JMP L_f\n",
            "L_t:\n",
            "    RTS\n",
            "L_f:\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        assert!(!ph315_branch_over_jmp_past_dead_labels(&mut items));
        let got = render(&items);
        assert!(got.contains("JMP L_f"), "JMP left alone:\n{got}");
    }

    #[test]
    fn ph315_refuses_to_cross_a_sys_wrap_marker() {
        let input = concat!(
            "    BEQ L_t\n",
            "    ; YAB_SYS_WRAP_IN\n",
            "ORPH:\n",
            "    JMP L_f\n",
            "L_t:\n",
            "    RTS\n",
            "L_f:\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        assert!(!ph315_branch_over_jmp_past_dead_labels(&mut items));
        let got = render(&items);
        assert!(got.contains("JMP L_f"), "JMP left alone:\n{got}");
    }

    #[test]
    fn ph315_refuses_a_runtime_patched_branch() {
        let input = concat!(
            "PATCH:\n",
            "    BEQ L_t\n",
            "ORPH:\n",
            "    JMP L_f\n",
            "L_t:\n",
            "    RTS\n",
            "L_f:\n",
            "    RTS\n",
            "    STA PATCH+2\n",
        );
        let mut items = parse(input);
        assert!(!ph315_branch_over_jmp_past_dead_labels(&mut items));
        let got = render(&items);
        assert!(got.contains("BEQ L_t"), "patched branch untouched:\n{got}");
        assert!(got.contains("JMP L_f"), "JMP untouched:\n{got}");
    }

    #[test]
    fn ph315_refuses_a_runtime_patched_jmp_target() {
        let input = concat!(
            "    BEQ L_t\n",
            "ORPH:\n",
            "    JMP L_f\n",
            "L_t:\n",
            "    RTS\n",
            "L_f:\n",
            "    RTS\n",
            "    STA L_f+1\n",
        );
        let mut items = parse(input);
        assert!(!ph315_branch_over_jmp_past_dead_labels(&mut items));
        let got = render(&items);
        assert!(got.contains("JMP L_f"), "JMP untouched:\n{got}");
    }

    #[test]
    fn ph315_leaves_the_adjacent_shape_to_ph005() {
        let input = concat!(
            "    BEQ L_true\n",
            "    JMP L_false\n",
            "L_true:\n",
            "    LDA #$01\n",
            "    RTS\n",
            "L_false:\n",
            "    LDA #$02\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        assert!(
            !ph315_branch_over_jmp_past_dead_labels(&mut items),
            "the adjacent shape is PH005's"
        );
        let got = render(&items);
        assert!(got.contains("BEQ L_true"), "direct call left it alone:\n{got}");
        let got = run_default(input);
        assert!(got.contains("BNE L_false"), "PH005 took it:\n{got}");
        assert!(!got.contains("JMP L_false"), "folded once, not twice:\n{got}");
        assert!(got.contains("L_true:"), "label kept:\n{got}");
    }

    #[test]
    fn ph315_leaves_a_target_first_stack_to_ph005() {
        let input = concat!(
            "    BEQ L_true\n",
            "    JMP L_false\n",
            "L_true:\n",
            "L_OTHER:\n",
            "    LDA #<L_OTHER\n",
            "    RTS\n",
            "L_false:\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        assert!(
            !ph315_branch_over_jmp_past_dead_labels(&mut items),
            "PH005's adjacency check accepts this one"
        );
        let got = run_default(input);
        assert!(got.contains("BNE L_false"), "PH005 took it:\n{got}");
        assert!(!got.contains("JMP L_false"), "folded once:\n{got}");
    }

    #[test]
    fn ph315_folds_the_heap_guard_in_the_pipeline() {
        let input = concat!(
            "    LDA $38\n",
            "    SBC __HEAP_PTR+1\n",
            "    BCC __GC_RUN\n",
            "    CMP #$01\n",
            "    BNE __HGIL_NO\n",
            "    CPX #$02\n",
            "    BCS __HGIL_NO\n",
            "__HGIL_GC:\n",
            "    JMP __GC_RUN\n",
            "__HGIL_NO:\n",
            "    RTS\n",
            "__GC_RUN:\n",
            "    LDA #<__HEAP_BOTTOM\n",
            "    RTS\n",
        );
        let got = run_default(input);
        assert!(got.contains("BCC __GC_RUN"), "guard branch inverted:\n{got}");
        assert!(!got.contains("JMP __GC_RUN"), "trampoline JMP gone:\n{got}");
    }

    // ZP equate table

    #[test]
    fn zp_equates_are_collected_from_the_header() {
        let items = parse("VI_J = $A7\nVI_T = $A8\nBIG = $C000\n    LDA VI_J\n");
        let zp = ZpEquates::scan(&items);
        assert!(zp.is_zp_operand("VI_J"), "$A7 is zero page");
        assert!(zp.is_zp_operand("VI_T+1"), "$A8+1 = $A9, still zero page");
        assert!(!zp.is_zp_operand("BIG"), "$C000 is not zero page");
        assert!(!zp.is_zp_operand("T0"), "unknown label stays absolute");
    }

    #[test]
    fn zp_equate_operands_cost_two_bytes_not_three() {
        let items = parse("VI_J = $A7\n    LDA VI_J\n    LDA T0\n");
        let zp = ZpEquates::scan(&items);
        let Item::Insn(zp_load) = &items[1] else {
            panic!("expected an instruction")
        };
        let Item::Insn(abs_load) = &items[2] else {
            panic!("expected an instruction")
        };
        assert_eq!(approx_insn_bytes(zp_load, &zp), 2, "LDA VI_J is zero page");
        assert_eq!(approx_insn_bytes(abs_load, &zp), 3, "LDA T0 is absolute");

        let none = ZpEquates::default();
        assert_eq!(approx_insn_bytes(zp_load, &none), 3);
    }

    #[test]
    fn zp_equates_refuse_indexed_and_immediate_forms() {
        let items = parse("VI_J = $A7\n");
        let zp = ZpEquates::scan(&items);
        assert!(!zp.is_zp_operand("VI_J,Y"));
        assert!(!zp.is_zp_operand("VI_J,X"));
        assert!(!zp.is_zp_operand("#<VI_J"));
        assert!(!zp.is_zp_operand("(VI_J),Y"));
    }

    #[test]
    fn zp_equate_offset_that_leaves_zero_page_is_not_zp() {
        let items = parse("EDGE = $FF\n");
        let zp = ZpEquates::scan(&items);
        assert!(zp.is_zp_operand("EDGE"), "$FF is the last zp byte");
        assert!(!zp.is_zp_operand("EDGE+1"), "$FF+1 = $0100, out of zp");
    }

    #[test]
    fn zp_equates_widen_ph005_reach() {
        // 32 pairs of `LDA VI_J / STA VI_T`.
        let mut filler = String::new();
        for _ in 0..30 {
            filler.push_str("    LDA VI_J\n    STA VI_T\n");
        }
        let header = "VI_J = $A7\nVI_T = $A8\n";
        let body =
            format!("    BEQ L_t\n    JMP L_far\nL_t:\n    RTS\nL_keep:\n{filler}L_far:\n    RTS\n");

        let mut with_equates = parse(&format!("{header}{body}"));
        assert!(
            ph005_branch_around_jmp(&mut with_equates),
            "equate table proves the target is in reach"
        );

        let mut without = parse(&body);
        assert!(
            !ph005_branch_around_jmp(&mut without),
            "without the table the same target reads as too far"
        );
    }

    #[test]
    fn late_branch_fixpoint_clears_post_factoring_residue() {
        let mut items = parse(
            "\
ENTRY:
    JSR __SEQ_9
    RTS
CHECK:
    BEQ SKIP
    JMP __TAIL_0
SKIP:
    LDA #$02
    RTS
__SEQ_9:
    LDA #$01
    STA $39
    RTS
__TAIL_0:
    LDA #$03
    RTS
",
        );
        assert!(late_branch_fixpoint(&mut items), "the pass must fire");
        let got = render(&items);
        assert!(
            got.contains("JMP __SEQ_9") && !got.contains("JSR __SEQ_9"),
            "PH003: tail JSR becomes a tail JMP:\n{got}"
        );
        assert!(
            got.contains("BNE __TAIL_0") && !got.contains("JMP __TAIL_0"),
            "PH005: branch-around-JMP folds:\n{got}"
        );
    }

    /// A `__JSR2_*` pair-stub ends in `JMP`, not `RTS`.
    #[test]
    fn late_branch_fixpoint_handles_tail_call_stubs() {
        let mut items = parse(
            "\
ENTRY:
    JSR __JSR2_0
    RTS
OTHER:
    LDA #$05
    RTS
__JSR2_0:
    JSR $B391
    JMP $B7F7
",
        );
        assert!(late_branch_fixpoint(&mut items));
        let got = render(&items);
        assert!(
            got.contains("JMP __JSR2_0"),
            "tail-call stub is a legal PH003 target:\n{got}"
        );
    }

    #[test]
    fn late_branch_fixpoint_reaches_a_fixpoint() {
        let mut items = parse(
            "\
ENTRY:
    JSR __SEQ_9
    RTS
__SEQ_9:
    LDA #$01
    RTS
",
        );
        assert!(late_branch_fixpoint(&mut items), "first call does work");
        assert!(
            !late_branch_fixpoint(&mut items),
            "second call must find nothing left to do"
        );
    }

    #[test]
    fn ph029_sweeps_unreferenced_line_label() {
        let input = "    LDA $02\nL5:\n    RTS\n";
        let got = run_default(input);
        assert!(!got.contains("L5:"), "dead line label swept:\n{got}");
        assert!(got.contains("LDA $02"), "code untouched:\n{got}");
    }

    #[test]
    fn ph029_keeps_referenced_line_label() {
        let input = "    BEQ L5\n    LDA #$01\n    STA dst\nL5:\n    RTS\n";
        let got = run_default(input);
        assert!(got.contains("L5:"), "referenced label kept:\n{got}");
        assert!(got.contains("BEQ L5"));
    }

    #[test]
    fn ph029_keeps_label_referenced_from_word_table() {
        let input = "    RTS\n.word L7\nL7:\n    RTS\n";
        let got = run_default(input);
        assert!(got.contains("L7:"), "table-referenced label kept:\n{got}");
    }

    #[test]
    fn ph029_keeps_label_referenced_via_lo_hi_imm() {
        let input = concat!(
            "    LDA #<L9\n",
            "    STA $FB\n",
            "    LDA #>L9\n",
            "    STA $FC\n",
            "    JMP ($FB)\n",
            "L9:\n",
            "    RTS\n",
        );
        let got = run_default(input);
        assert!(got.contains("L9:"), "vector-referenced label kept:\n{got}");
    }

    #[test]
    fn ph029_only_sweeps_local_families() {

        let input = "    LDA $02\n__FOR_TOP_1:\n    INC $02\nMYLOOP:\n    RTS\n";
        let got = run_default(input);
        assert!(got.contains("__FOR_TOP_1:"), "runtime label kept:\n{got}");
        assert!(got.contains("MYLOOP:"), "user-style label kept:\n{got}");
    }

    #[test]
    fn ph029_unblocks_ph005_behind_dead_if_trampoline() {

        let input = concat!(
            "    LDA VI_C\n",
            "    CMP VI_D\n",
            "    BEQ L70\n",
            "    BCS __IF_END_2\n",
            "__IF_U8VV_KEEP_3:\n",
            "    JMP L70\n",
            "__IF_END_2:\n",
            "    LDA #$01\n",
            "    STA dst\n",
            "L70:\n",
            "    INC $02\n",
            "    RTS\n",
        );
        let got = run_default(input);
        assert!(got.contains("BCC L70"), "PH005 fired after sweep:\n{got}");
        assert!(!got.contains("JMP L70"), "branch-around JMP gone:\n{got}");
        assert!(!got.contains("__IF_"), "both IF labels swept:\n{got}");
        assert!(got.contains("L70:"), "referenced line label kept:\n{got}");
    }

    fn run_size(input: &str) -> String {
        run(input, Profile::Size)
    }

    /// Two runs that do different work, then share a 10-byte epilogue.
    const SHARED_TAIL: &str = concat!(
        "    JSR A1\n",
        "    JMP B1\n",
        "A1:\n",
        "    STA V_P\n",
        "    LDA V_X\n",
        "    STA V_Y\n",
        "    LDA V_Z\n",
        "    RTS\n",
        "B1:\n",
        "    STA V_Q\n",
        "    LDA V_X\n",
        "    STA V_Y\n",
        "    LDA V_Z\n",
        "    RTS\n",
    );

    #[test]
    fn ph305_sinks_a_shared_tail() {
        let got = run_default(SHARED_TAIL);
        assert!(got.contains("__TAIL_"), "tail label introduced:\n{got}");
        assert_eq!(
            got.matches("STA V_Y").count(),
            1,
            "shared epilogue kept once:\n{got}"
        );
        assert!(got.contains("STA V_P"), "distinct head kept:\n{got}");
        assert!(got.contains("STA V_Q"), "distinct head kept:\n{got}");
        assert_eq!(
            got.matches("JMP __TAIL_").count(),
            1,
            "the other copy jumps to it:\n{got}"
        );
    }

    #[test]
    fn ph305_refuses_a_tail_too_small_to_pay_for_the_jmp() {

        let input = concat!(
            "    JSR A1\n",
            "    JMP B1\n",
            "A1:\n",
            "    STA V_P\n",
            "    INX\n",
            "    RTS\n",
            "B1:\n",
            "    STA V_Q\n",
            "    INX\n",
            "    RTS\n",
        );
        let got = run_default(input);
        assert!(!got.contains("__TAIL_"), "unprofitable tail left alone:\n{got}");
        assert_eq!(got.matches("INX").count(), 2, "both copies kept:\n{got}");
    }

    #[test]
    fn ph305_size_profile_takes_tails_default_declines() {

        let input = concat!(
            "    JSR A1\n",
            "    JMP B1\n",
            "A1:\n",
            "    STA V_P\n",
            "    LDA V_X\n",
            "    TAY\n",
            "    RTS\n",
            "B1:\n",
            "    STA V_Q\n",
            "    LDA V_X\n",
            "    TAY\n",
            "    RTS\n",
        );
        assert!(
            !run_default(input).contains("__TAIL_"),
            "default declines the marginal tail"
        );
        let got = run_size(input);
        assert!(got.contains("__TAIL_"), "size profile takes it:\n{got}");
    }

    #[test]
    fn ph305_never_sinks_across_a_label() {
        let input = concat!(
            "    JSR A1\n",
            "    JMP B1\n",
            "A1:\n",
            "    STA V_P\n",
            "MID:\n",
            "    LDA V_X\n",
            "    STA V_Y\n",
            "    LDA V_Z\n",
            "    RTS\n",
            "B1:\n",
            "    STA V_Q\n",
            "    LDA V_X\n",
            "    STA V_Y\n",
            "    LDA V_Z\n",
            "    JMP MID\n",
        );
        let got = run_default(input);
        assert!(got.contains("MID:"), "entry label preserved:\n{got}");
        assert!(
            got.contains("JMP MID"),
            "reference into the tail preserved:\n{got}"
        );
    }

    #[test]
    fn ph305_runs_on_speed_profile() {
        let got = run(SHARED_TAIL, Profile::Speed);
        assert!(got.contains("__TAIL_"), "speed profile sinks it:\n{got}");
        assert_eq!(
            got.matches("STA V_Y").count(),
            1,
            "the shared copy is sunk, not duplicated:\n{got}"
        );
        assert_eq!(got, run_default(SHARED_TAIL), "speed matches default");
    }

    #[test]
    fn ph304_merges_two_identical_blocks() {
        let input = concat!(
            "    JMP A1\n",
            "A1:\n",
            "    LDA V_X\n",
            "    STA V_Y\n",
            "    RTS\n",
            "B1:\n",
            "    LDA V_X\n",
            "    STA V_Y\n",
            "    RTS\n",
            "    JMP B1\n",
        );
        let got = run_default(input);
        assert_eq!(
            got.matches("STA V_Y").count(),
            1,
            "duplicate block deleted:\n{got}"
        );
        assert!(!got.contains("B1"), "dead label and refs retargeted:\n{got}");
    }

    #[test]
    fn ph304_keeps_blocks_that_differ() {

        let input = concat!(
            "    JSR A1\n",
            "    JMP B1\n",
            "A1:\n",
            "    LDA V_X\n",
            "    RTS\n",
            "B1:\n",
            "    LDA V_Z\n",
            "    RTS\n",
        );
        let got = run_default(input);
        assert!(got.contains("LDA V_X"), "first block kept:\n{got}");
        assert!(got.contains("LDA V_Z"), "distinct block kept:\n{got}");
    }

    #[test]
    fn ph304_refuses_block_reachable_by_fallthrough() {
        let input = concat!(
            "    JSR A1\n",
            "    JMP C1\n",
            "A1:\n",
            "    LDA V_X\n",
            "    RTS\n",
            "C1:\n",
            "    LDA #$01\n",
            "    STA V_W\n",
            "B1:\n",
            "    LDA V_X\n",
            "    RTS\n",
        );
        let got = run_default(input);
        assert!(
            got.contains("B1:"),
            "fall-through-reachable block kept:\n{got}"
        );
    }

    #[test]
    fn ph304_refuses_when_a_reference_is_not_a_bare_operand() {
        let input = concat!(
            "    LDA #<B1\n",
            "    STA $FB\n",
            "    JMP A1\n",
            "A1:\n",
            "    LDA V_X\n",
            "    RTS\n",
            "B1:\n",
            "    LDA V_X\n",
            "    RTS\n",
        );
        let got = run_default(input);
        assert!(got.contains("B1:"), "address-taken block kept:\n{got}");
        assert!(got.contains("LDA #<B1"), "reference untouched:\n{got}");
    }

    #[test]
    fn ph302_inlines_helper_with_a_single_jsr_caller() {
        let input = concat!(
            "    LDA #$01\n",
            "    JSR __SEQ_9\n",
            "    LDA #$02\n",
            "    RTS\n",
            "__SEQ_9:\n",
            "    LDA V_A\n",
            "    STA V_B\n",
            "    LDA V_C\n",
            "    STA V_D\n",
            "    RTS\n",
        );
        let got = run_default(input);
        assert!(!got.contains("__SEQ_9"), "helper fully removed:\n{got}");
        assert!(got.contains("LDA V_A"), "body spliced in:\n{got}");
        assert!(got.contains("STA V_D"), "body spliced in:\n{got}");
    }

    #[test]
    fn ph302_keeps_rts_when_the_caller_is_a_tail_jmp() {

        let input = concat!(
            "    LDA #$01\n",
            "    JMP __SEQ_9\n",
            "OTHER:\n",
            "    LDA #$05\n",
            "    RTS\n",
            "__SEQ_9:\n",
            "    LDA V_A\n",
            "    STA V_B\n",
            "    LDA V_C\n",
            "    STA V_D\n",
            "    RTS\n",
        );
        let got = run_default(input);
        assert!(!got.contains("__SEQ_9"), "helper fully removed:\n{got}");
        assert!(got.contains("STA V_D"), "body spliced in:\n{got}");

        let tail = &got[got.find("STA V_D").unwrap()..];
        assert!(
            tail.trim_start_matches(|c| c != '\n').trim().starts_with("RTS"),
            "tail-call RTS preserved right after the body:\n{got}"
        );
    }

    #[test]
    fn ph302_keeps_helper_with_two_callers() {

        let input = concat!(
            "    JSR __SEQ_9\n",
            "    LDA #$02\n",
            "    JSR __SEQ_9\n",
            "    LDA #$03\n",
            "    STA V_Z\n",
            "    RTS\n",
            "__SEQ_9:\n",
            "    LDA V_A\n",
            "    STA V_B\n",
            "    LDA V_C\n",
            "    STA V_D\n",
            "    RTS\n",
        );
        let got = run_default(input);
        assert!(got.contains("__SEQ_9:"), "shared helper kept:\n{got}");
        assert_eq!(
            got.matches("JSR __SEQ_9").count(),
            2,
            "both call sites kept:\n{got}"
        );
    }

    #[test]
    fn ph302_refuses_helper_reachable_by_fallthrough() {

        let input = concat!(
            "    LDA #$01\n",
            "    STA V_Z\n",
            "__SEQ_9:\n",
            "    LDA V_A\n",
            "    STA V_B\n",
            "    RTS\n",
            "    JSR __SEQ_9\n",
            "    RTS\n",
        );
        let got = run_default(input);
        assert!(
            got.contains("__SEQ_9:"),
            "fall-through-reachable helper kept:\n{got}"
        );
    }

    #[test]
    fn ph301_disjoint_sites_rejects_overlapping_windows() {
        // Eight instructions, candidate starts at every alignment.
        let items = parse(
            "    LDA V_1\n    STA V_2\n    LDA V_3\n    STA V_4\n\
                 LDA V_5\n    STA V_6\n    LDA V_7\n    STA V_8\n",
        );
        let picked = ph301_disjoint_sites(&items, &[0, 1, 2, 3, 4, 5], 3, None);
        assert_eq!(
            picked,
            vec![(0, 2), (3, 5)],
            "greedy in stream order, no overlap"
        );
    }

    #[test]
    fn ph301_disjoint_sites_respects_already_consumed_indices() {
        let items = parse(
            "    LDA V_1\n    STA V_2\n    LDA V_3\n    STA V_4\n\
                 LDA V_5\n    STA V_6\n",
        );
        let consumed: std::collections::HashSet<usize> = [1usize].into_iter().collect();
        let picked = ph301_disjoint_sites(&items, &[0, 2], 2, Some(&consumed));
        assert_eq!(
            picked,
            vec![(2, 3)],
            "a window touching a claimed index is skipped"
        );
    }

    #[test]
    fn ph301_factors_a_window_the_old_stride_scan_could_not_see() {
        // The repeated 4-window starts at alignment 1, not 0.
        let mut input = String::from("    NOP\n");
        for _ in 0..4 {
            input.push_str(
                "    LDA $14\n    STA $FD\n    LDA $15\n    STA $FE\n    LDA ($FD),Y\n    STA out\n",
            );
        }
        input.push_str("    RTS\n");
        let got = run_default(&input);
        assert!(
            got.contains("JSR __SEQ_"),
            "off-phase repetition now factored:\n{got}"
        );
    }

    #[test]
    fn ph301_refuses_to_factor_self_modified_instructions() {

        let mut input = String::new();
        for n in 0..4 {
            input.push_str(&format!("    LDA #$10\n    STA __HCS_{n}+1\n"));
        }
        for n in 0..4 {
            input.push_str(&format!(
                "__HCS_{n}:\n    LDA $0000,Y\n    STA dst\n    INY\n    DEX\n"
            ));
        }
        let got = run_default(&input);
        assert_eq!(
            got.matches("LDA $0000,Y").count(),
            4,
            "SMC targets must stay inline:\n{got}"
        );
        for n in 0..4 {
            assert!(
                got.contains(&format!("__HCS_{n}:")),
                "patched label must survive:\n{got}"
            );
        }
    }

    #[test]
    fn ph301_refuses_to_factor_stack_page_accesses() {
        let mut input = String::new();
        for _ in 0..4 {
            input.push_str("    LDA $0100,Y\n    STA dst\n    INY\n    DEX\n    NOP\n");
        }
        input.push_str("    RTS\n");
        let got = run_default(&input);
        assert_eq!(
            got.matches("LDA $0100,Y").count(),
            4,
            "stack-page reads must stay inline:\n{got}"
        );
    }

    #[test]
    fn ph301_still_factors_ordinary_repeated_windows() {

        let mut input = String::new();
        for _ in 0..4 {
            input.push_str(
                "    LDA $14\n    STA $FD\n    LDA $15\n    STA $FE\n    LDA ($FD),Y\n    STA out\n",
            );
        }
        input.push_str("    RTS\n");
        let got = run_default(&input);
        assert!(
            got.contains("JSR __SEQ_"),
            "ordinary windows must still factor:\n{got}"
        );
    }

    #[test]
    fn ph031_drops_stamp_overwritten_before_any_read() {
        let input = concat!(
            "    LDA #$0A\n",
            "    STA $39\n",
            "    LDA #$00\n",
            "    STA $3A\n",
            "    LDA #$14\n",
            "    STA $39\n",
            "    RTS\n",
        );
        let got = run_default(input);
        assert_eq!(got.matches("STA $39").count(), 1, "dead stamp dropped:\n{got}");
        assert!(!got.contains("#$0A"), "orphaned feeder load mopped:\n{got}");
        assert!(got.contains("LDA #$14"), "live stamp kept:\n{got}");
    }

    #[test]
    fn ph031_drops_prologue_init_overwritten_by_for_header() {
        let input = concat!(
            "    LDA #$00\n",
            "    STA VI_I\n",
            "    STA FU_0\n",
            "    LDA #$01\n",
            "    STA FU_0\n",
            "    STA VI_I\n",
            "    RTS\n",
        );
        let got = run_default(input);
        assert_eq!(got.matches("STA FU_0").count(), 1, "dead init dropped:\n{got}");
        assert_eq!(got.matches("STA VI_I").count(), 1, "dead init dropped:\n{got}");
        assert!(!got.contains("#$00"), "zero feeder mopped:\n{got}");
    }

    #[test]
    fn ph031_keeps_store_read_before_overwrite() {

        let mut items = parse(concat!(
            "    LDA #$01\n",
            "    STA V1\n",
            "    LDX V1\n",
            "    STX out\n",
            "    LDA #$02\n",
            "    STA V1\n",
            "    RTS\n",
        ));
        assert!(
            !ph031_drop_overwritten_store(&mut items),
            "read inside the window blocks the drop"
        );
        let got = render(&items);
        assert_eq!(got.matches("STA V1").count(), 2, "both stores kept:\n{got}");
    }

    #[test]
    fn ph031_keeps_store_across_jsr_and_branch() {

        let input = concat!(
            "    LDA V9\n",
            "    STA V1\n",
            "    JSR __HELP\n",
            "    LDA V8\n",
            "    STA V1\n",
            "    BEQ done\n",
            "    LDA V7\n",
            "    STA V1\n",
            "done:\n",
            "    RTS\n",
        );
        let got = run_default(input);
        assert_eq!(
            got.matches("STA V1").count(),
            3,
            "JSR and branch both end the window:\n{got}"
        );
    }

    #[test]
    fn ph031_respects_equ_alias_between_labels() {

        let input = concat!(
            "*=$080D\n",
            "FU_0 = $A9\n",
            "    LDA #$01\n",
            "    STA FU_0\n",
            "    LDA $A9\n",
            "    STA out\n",
            "    LDA #$02\n",
            "    STA FU_0\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        ph031_drop_overwritten_store(&mut items);
        let got = render(&items);
        assert_eq!(
            got.matches("STA FU_0").count(),
            2,
            "aliased read blocks the drop:\n{got}"
        );
    }

    #[test]
    fn ph020_forwards_zp_fact_across_screen_store() {
        let input = concat!(
            "*=$080D\n",
            "FU_2 = $A9\n",
            "    LDX FU_2\n",
            "    LDA $0400,X\n",
            "    STA $0401,X\n",
            "    LDX FU_2\n",
            "    STA $0402,X\n",
            "    LDA #$00\n",
            "    STA out\n",
            "    RTS\n",
        );
        let got = run_default(input);
        assert_eq!(
            got.matches("LDX FU_2").count(),
            1,
            "redundant reload dropped across screen store:\n{got}"
        );
    }

    #[test]
    fn ph020_drops_zp_fact_when_indexed_store_can_wrap() {

        let input = concat!(
            "*=$080D\n",
            "FU_2 = $A9\n",
            "    LDX FU_2\n",
            "    LDA $0400,X\n",
            "    STA $FF80,X\n",
            "    LDX FU_2\n",
            "    STA $0402,X\n",
            "    LDA #$00\n",
            "    STA out\n",
            "    RTS\n",
        );
        let got = run_default(input);
        assert_eq!(
            got.matches("LDX FU_2").count(),
            2,
            "reload kept when the store can wrap into ZP:\n{got}"
        );
    }

    #[test]
    fn ph030_swaps_pairs_and_drops_reload() {
        let input = concat!(
            "*=$080D\n",
            "VI_C = $A8\n",
            "VI_D = $AB\n",
            "    LDX FU_1\n",
            "    LDA $0400,X\n",
            "    STA VI_C\n",
            "    LDA $0401,X\n",
            "    STA VI_D\n",
            "    LDA VI_C\n",
            "    CMP VI_D\n",
            "    BEQ done\n",
            "    STA dst\n",
            "done:\n",
            "    RTS\n",
        );
        let got = run_default(input);

        assert!(!got.contains("LDA VI_C"), "reload dropped:\n{got}");

        let d_load = got.find("LDA $0401,X").expect("D load kept");
        let c_load = got.find("LDA $0400,X").expect("C load kept");
        assert!(d_load < c_load, "pairs swapped:\n{got}");
        assert!(got.contains("STA VI_C"), "C store kept:\n{got}");
        assert!(got.contains("CMP VI_D"), "compare kept:\n{got}");
    }

    #[test]
    fn ph030_refuses_when_stores_share_a_cell() {

        let input = concat!(
            "*=$080D\n",
            "FU_0 = $A9\n",
            "VB_I = $A9\n",
            "    LDA V1\n",
            "    STA FU_0\n",
            "    LDA V2\n",
            "    STA VB_I\n",
            "    LDA FU_0\n",
            "    STA out\n",
            "    RTS\n",
        );
        let got = run_default(input);
        let v1 = got.find("LDA V1").expect("V1 load kept");
        let v2 = got.find("LDA V2").expect("V2 load kept");
        assert!(v1 < v2, "order untouched when cells alias:\n{got}");
    }

    #[test]
    fn ph030_refuses_when_second_load_aliases_first_store() {
        let input = concat!(
            "*=$080D\n",
            "VB = $10\n",
            "    LDA V1\n",
            "    STA VB\n",
            "    LDA $10\n",
            "    STA V3\n",
            "    LDA VB\n",
            "    STA out\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        ph030_swap_indep_store_pairs(&mut items);
        let got = render(&items);
        let v1 = got.find("LDA V1").expect("V1 load kept");
        let alias = got.find("LDA $10").expect("aliasing load kept");
        assert!(v1 < alias, "order untouched when c aliases b:\n{got}");
    }

    #[test]
    fn ph030_refuses_when_first_load_may_alias_second_store() {

        let input = concat!(
            "*=$0480\n",
            "    LDA $0400,X\n",
            "    STA V1\n",
            "    LDA V2\n",
            "    STA V3\n",
            "    LDA V1\n",
            "    STA out\n",
            "    RTS\n",
        );
        let got = run_default(input);
        let a = got.find("LDA $0400,X").expect("a load kept");
        let c = got.find("LDA V2").expect("c load kept");
        assert!(a < c, "order untouched on possible overlap:\n{got}");
    }

    #[test]
    fn ph030_swaps_label_only_window_without_origin() {
        let input = concat!(
            "    LDA #$05\n",
            "    STA V1\n",
            "    LDA V2\n",
            "    STA V3\n",
            "    LDA V1\n",
            "    STA out\n",
            "    RTS\n",
        );
        let got = run_default(input);
        assert!(!got.contains("LDA V1"), "reload dropped:\n{got}");
        let v2 = got.find("LDA V2").expect("V2 load kept");
        let imm = got.find("LDA #$05").expect("imm load kept");
        assert!(v2 < imm, "pairs swapped:\n{got}");
    }

    #[test]
    fn ph206_drops_pha_pla_when_flags_redefined() {
        let input = "    PHA\n    PLA\n    LDA #$00\n    RTS\n";
        let got = run_default(input);
        assert!(!got.contains("PHA"), "roundtrip PHA dropped:\n{got}");
        assert!(!got.contains("PLA"), "roundtrip PLA dropped:\n{got}");
        assert!(got.contains("LDA #$00"));
    }

    #[test]
    fn ph206_keeps_pla_when_branch_reads_flags() {
        let input = "    PHA\n    PLA\n    STA dst\n    BEQ done\ndone:\n    RTS\n";
        let got = run_default(input);
        assert!(
            got.contains("PHA"),
            "PHA kept because BEQ observes PLA flags:\n{got}"
        );
        assert!(
            got.contains("PLA"),
            "PLA kept because BEQ observes its flags:\n{got}"
        );
    }

    /// A string helper that reads its pointer arguments from zero page.
    const PH209_CALLEE: &str = concat!(
        "__STRCMP:\n",
        "    LDY #$00\n",
        "    LDA ($03),Y\n",
        "    CMP ($05),Y\n",
        "    RTS\n",
    );

    fn ph209_window(middle: &str, tail: &str) -> String {
        format!(
            concat!(
                "*=$0801\n",
                "    LDA VS_A\n",
                "    LDY VS_A+1\n",
                "    PHA\n",
                "    TYA\n",
                "    PHA\n",
                "{}",
                "    PLA\n",
                "    STA $04\n",
                "    PLA\n",
                "    STA $03\n",
                "{}",
            ),
            middle, tail
        )
    }

    /// Run the pointer-spill pass without other optimization passes.
    fn ph209_only(input: &str) -> (bool, String) {
        let mut items = parse(input);
        let fired = ph209_spill_pha_pla_pair(&mut items);
        (fired, render(&items))
    }

    #[test]
    fn ph209_hoists_the_string_operand_spill() {
        let input = ph209_window(
            "    LDA #<S1\n    LDY #>S1\n    STA $05\n    STY $06\n",
            &format!("    JSR __STRCMP\n    RTS\n{PH209_CALLEE}"),
        );
        let got = run_default(&input);
        assert!(!got.contains("PHA"), "no push survives:\n{got}");
        assert!(!got.contains("PLA"), "no pop survives:\n{got}");
        let sta = got.find("STA $03").expect("low byte stored");
        let sty = got.find("STY $04").expect("high byte stored");
        assert!(sta < sty, "low byte stored before .Y is clobbered:\n{got}");
        assert!(
            sty < got.find("LDA #<S1").expect("middle kept"),
            "both stores precede the middle:\n{got}"
        );
    }

    #[test]
    fn ph209_accepts_a_label_plus_offset_read_in_the_middle() {
        // `LDY VS_B+1` is how codegen names the high half of a 16-bit slot.
        let input = ph209_window(
            "    LDA VS_B\n    LDY VS_B+1\n    STA $05\n    STY $06\n",
            &format!("    JSR __STRCMP\n    RTS\n{PH209_CALLEE}"),
        );
        let (fired, got) = ph209_only(&input);
        assert!(fired, "LABEL+n middle accepted:\n{got}");
        assert!(got.contains("STY $04"), "high byte stored:\n{got}");
    }

    #[test]
    fn ph209_refuses_when_the_middle_calls() {
        let input = ph209_window(
            "    LDY VI_I\n    JSR __CHR_NEW\n    STA $05\n    STY $06\n",
            &format!("    JSR __STRCMP\n    RTS\n{PH209_CALLEE}"),
        );
        let (fired, got) = ph209_only(&input);
        assert!(!fired, "a JSR in the window is opaque to the stack:\n{got}");
    }

    #[test]
    fn ph209_refuses_when_the_middle_writes_a_pop_destination() {
        let input = ph209_window(
            "    LDA #$01\n    STA $04\n    STY $06\n",
            &format!("    JSR __STRCMP\n    RTS\n{PH209_CALLEE}"),
        );
        let (fired, got) = ph209_only(&input);
        assert!(
            !fired,
            "hoisting past a write to $04 would lose that write:\n{got}"
        );
    }

    #[test]
    fn ph209_refuses_when_the_middle_reads_a_pop_destination() {

        let input = ph209_window(
            "    LDA $03\n    STA $05\n    STY $06\n",
            &format!("    JSR __STRCMP\n    RTS\n{PH209_CALLEE}"),
        );
        let (fired, got) = ph209_only(&input);
        assert!(!fired, "a read of $03 pins the store after it:\n{got}");
    }

    #[test]
    fn ph209_refuses_when_the_middle_addresses_the_stack_page() {
        // `__STR_FROM_NUMBER` reads FOUT's digits out of $0100,Y.
        let input = ph209_window(
            "    LDA $0100,Y\n    STA $05\n    STY $06\n",
            &format!("    JSR __STRCMP\n    RTS\n{PH209_CALLEE}"),
        );
        let (fired, got) = ph209_only(&input);
        assert!(!fired, "page one is a modelled barrier:\n{got}");
    }

    #[test]
    fn ph209_refuses_when_the_middle_touches_the_stack() {
        let input = ph209_window(
            "    LDA #$01\n    PHA\n    PLA\n    STA $05\n    STY $06\n",
            &format!("    JSR __STRCMP\n    RTS\n{PH209_CALLEE}"),
        );
        let (fired, got) = ph209_only(&input);
        assert!(!fired, "an interior push/pop is not balanced away:\n{got}");
    }

    #[test]
    fn ph209_refuses_when_a_label_splits_the_window() {
        let input = ph209_window(
            "    LDA #<S1\nmid:\n    STA $05\n    STY $06\n",
            &format!("    JSR __STRCMP\n    RTS\n{PH209_CALLEE}"),
        );
        let (fired, got) = ph209_only(&input);
        assert!(
            !fired,
            "an entry into the middle sees a different stack:\n{got}"
        );
    }

    #[test]
    fn ph209_refuses_when_the_pop_destinations_alias() {
        let mut input = String::from("*=$0801\n    LDA VS_A\n    LDY VS_A+1\n");
        input.push_str("    PHA\n    TYA\n    PHA\n");
        input.push_str("    LDA #<S1\n    STA $05\n    STY $06\n");

        input.push_str("    PLA\n    STA $03\n    PLA\n    STA $03\n");
        input.push_str(&format!("    JSR __STRCMP\n    RTS\n{PH209_CALLEE}"));
        let (fired, got) = ph209_only(&input);
        assert!(!fired, "aliased destinations swap which write wins:\n{got}");
    }

    #[test]
    fn ph209_refuses_an_indexed_pop_destination() {
        let mut input = String::from("*=$0801\n    LDA VS_A\n    LDY VS_A+1\n");
        input.push_str("    PHA\n    TYA\n    PHA\n");
        input.push_str("    LDA #<S1\n    STA $05\n    STY $06\n");
        input.push_str("    PLA\n    STA $0400,X\n    PLA\n    STA $03\n");
        input.push_str(&format!("    JSR __STRCMP\n    RTS\n{PH209_CALLEE}"));
        let (fired, got) = ph209_only(&input);
        assert!(!fired, "indexed destination refused:\n{got}");
    }

    #[test]
    fn ph209_refuses_when_the_callee_reads_a_on_entry() {

        let callee = concat!("__WANTS_A:\n", "    STA $10\n", "    RTS\n");
        let input = ph209_window(
            "    LDA #<S1\n    LDY #>S1\n    STA $05\n    STY $06\n",
            &format!("    JSR __WANTS_A\n    RTS\n{callee}"),
        );
        let (fired, got) = ph209_only(&input);
        assert!(!fired, "callee's live-in .A keeps the pop:\n{got}");
    }

    #[test]
    fn ph209_refuses_when_a_branch_reads_the_pop_flags() {
        let input = ph209_window(
            "    LDA #<S1\n    LDY #>S1\n    STA $05\n    STY $06\n",
            "    BEQ done\ndone:\n    RTS\n",
        );
        let (fired, got) = ph209_only(&input);
        assert!(!fired, "N/Z from the PLA are observed:\n{got}");
    }

    #[test]
    fn ph209_refuses_when_the_middle_reads_a() {

        let input = ph209_window(
            "    STA $05\n    LDA #<S1\n    STA $06\n",
            &format!("    JSR __STRCMP\n    RTS\n{PH209_CALLEE}"),
        );
        let (fired, got) = ph209_only(&input);
        assert!(!fired, "the middle consumes TYA's .A:\n{got}");
    }

    #[test]
    fn ph206_adjacent_pair_still_cancels_beside_a_ph209_window() {

        let mut input = String::from("*=$0801\n");
        input.push_str("    LDA $20\n    PHA\n    PLA\n    LDA #$00\n");
        input.push_str("    LDA VS_A\n    LDY VS_A+1\n");
        input.push_str("    PHA\n    TYA\n    PHA\n");
        input.push_str("    LDA #<S1\n    LDY #>S1\n    STA $05\n    STY $06\n");
        input.push_str("    PLA\n    STA $04\n    PLA\n    STA $03\n");
        input.push_str(&format!("    JSR __STRCMP\n    RTS\n{PH209_CALLEE}"));
        let got = run_default(&input);
        assert!(!got.contains("PHA"), "both pushes gone:\n{got}");
        assert!(!got.contains("PLA"), "both pops gone:\n{got}");
        assert!(got.contains("STY $04"), "PH209 still fired:\n{got}");
    }

    #[test]
    fn ph209_leaves_a_lone_adjacent_pair_to_ph206() {

        let items = &mut parse("    PHA\n    PLA\n    LDA #$00\n    RTS\n");
        assert!(
            !ph209_spill_pha_pla_pair(items),
            "PH209 declines PH206's shape"
        );
    }

    #[test]
    fn ph204_folds_and80_bne_into_bmi() {
        let input = concat!(
            "    LDA $1234\n",
            "    AND #$80\n",
            "    BNE neg\n",
            "    LDA #$01\n",
            "    STA dst\n",
            "    RTS\n",
            "neg:\n",
            "    LDA #$02\n",
            "    STA dst\n",
            "    RTS\n",
        );
        let got = run_default(input);
        assert!(!got.contains("AND #$80"), "AND eliminated:\n{got}");
        assert!(got.contains("BMI neg"), "branch becomes BMI:\n{got}");
    }

    #[test]
    fn ph204_folds_and80_beq_into_bpl() {
        let input = concat!(
            "    LDA $1234\n",
            "    AND #$80\n",
            "    BEQ pos\n",
            "    LDA #$11\n",
            "    STA dst\n",
            "    RTS\n",
            "pos:\n",
            "    LDA #$22\n",
            "    STA dst\n",
            "    RTS\n",
        );
        let got = run_default(input);
        assert!(!got.contains("AND #$80"), "AND eliminated:\n{got}");
        assert!(got.contains("BPL pos"), "branch becomes BPL:\n{got}");
    }

    #[test]
    fn ph204_keeps_and_when_a_live_after_branch() {
        let input = concat!(
            "    LDA $1234\n",
            "    AND #$80\n",
            "    BNE neg\n",
            "    STA dst\n",
            "    RTS\n",
            "neg:\n",
            "    LDA #$00\n",
            "    STA dst\n",
            "    RTS\n",
        );
        let got = run_default(input);
        assert!(
            got.contains("AND #$80"),
            "AND kept when A live after branch:\n{got}"
        );
    }

    #[test]
    fn ph207_lda_zero_beq_eliminates_branch() {
        let input = concat!(
            "    LDA #$00\n",
            "    BEQ tgt\n",
            "    LDA #$ff\n",
            "    STA dst\n",
            "    RTS\n",
            "tgt:\n",
            "    LDA #$01\n",
            "    STA dst\n",
            "    RTS\n",
        );
        let got = run_default(input);
        assert!(!got.contains("BEQ tgt"), "always-taken BEQ removed:\n{got}");
        assert!(!got.contains("LDA #$ff"), "dead path dropped:\n{got}");
    }

    #[test]
    fn ph207_lda_zero_bne_dropped() {
        let input = concat!(
            "    LDA #$00\n",
            "    BNE tgt\n",
            "    LDA #$22\n",
            "    STA dst\n",
            "    RTS\n",
            "tgt:\n",
            "    LDA #$33\n",
            "    STA dst\n",
            "    RTS\n",
        );
        let got = run_default(input);
        assert!(!got.contains("BNE tgt"), "never-taken BNE dropped:\n{got}");
    }

    #[test]
    fn ph207_lda_nonzero_bne_eliminates_branch() {
        let input = concat!(
            "    LDA #$05\n",
            "    BNE tgt\n",
            "    LDA #$11\n",
            "    STA dst\n",
            "    RTS\n",
            "tgt:\n",
            "    LDA #$22\n",
            "    STA dst\n",
            "    RTS\n",
        );
        let got = run_default(input);
        assert!(!got.contains("BNE tgt"), "always-taken BNE removed:\n{got}");
        assert!(!got.contains("LDA #$11"), "dead path dropped:\n{got}");
    }

    #[test]
    fn ph207_drops_load_when_dead() {
        let input = concat!(
            "    LDA #$05\n",
            "    BNE tgt\n",
            "    LDA #$11\n",
            "    STA between\n",
            "    RTS\n",
            "tgt:\n",
            "    LDA #$22\n",
            "    STA dst\n",
            "    RTS\n",
        );
        let got = run_default(input);
        assert!(
            !got.contains("LDA #$05"),
            "dead constant load removed:\n{got}"
        );
        assert!(!got.contains("BNE tgt"), "branch folded:\n{got}");
    }

    #[test]
    fn ph208_hoists_ldy_zero_from_straight_loop() {
        let input = concat!(
            "    LDA #$01\n",
            "L:\n",
            "    LDY #$00\n",
            "    LDA ($FD),Y\n",
            "    STA dst\n",
            "    JMP L\n",
        );
        let got = run_default(input);
        assert!(
            got.contains("LDY #$00\nL:"),
            "LDY hoisted before loop label:\n{got}"
        );
        assert!(
            !got.contains("L:\n    LDY #$00"),
            "in-loop LDY removed:\n{got}"
        );
    }

    #[test]
    fn ph208_keeps_ldy_zero_when_loop_clobbers_y() {
        let input = concat!(
            "L:\n",
            "    LDY #$00\n",
            "    LDA ($FD),Y\n",
            "    INY\n",
            "    JMP L\n",
        );
        let got = run_default(input);
        assert!(
            got.contains("L:\n    LDY #$00"),
            "LDY kept when Y changes in loop:\n{got}"
        );
    }

    #[test]
    fn ph208_keeps_ldy_zero_across_getin() {
        let input = "L:\n    LDY #$00\n    STY $0400\n    JSR $FFE4\n    JMP L\n";
        let mut items = parse(input);
        assert!(!ph208_hoist_ldy_zero_from_loop(&mut items));
        assert_eq!(render(&items), input);
    }

    #[test]
    fn ph208_queued_getin_keeps_each_iteration_zero_on_actual_6502() {
        let input = concat!(
            "*=$2000\n    LDA #$00\n    STA $0401\n",
            "    LDA #$41\n    STA $0277\n    LDA #$01\n    STA $C6\n",
            "LOOP:\n    LDY #$00\n    STY $0400\n    INC $0401\n",
            "    JSR $FFE4\n    JMP LOOP\n",
        );
        for profile in [Profile::Default, Profile::Speed, Profile::Size] {
            let once = run(input, profile);
            let twice = run(&once, profile);
            for (stage, source) in [("input", input), ("once", &once), ("twice", &twice)] {
                let mut assembler = asm6502::Assembler6502::new();
                let bytes = assembler.assemble_bytes(source).unwrap();
                let head = assembler.lookup("LOOP").unwrap();
                let mut system = emu64::System::new();
                system.mem.ram[0x2000..0x2000 + bytes.len()].copy_from_slice(&bytes);
                system.start_at(0x2000);
                let mut complete = false;
                for _ in 0..1000 {
                    if system.cpu.regs.pc == head && system.mem.ram[0x0401] == 3 {
                        complete = true;
                        break;
                    }

                    assert!(system.cpu.step(&mut system.mem));
                }
                assert!(complete, "{profile:?}/{stage}: loop did not advance");
                assert_eq!(system.mem.ram[0xC6], 0, "queued key must be drained");
                assert_eq!(system.cpu.regs.sp, 0xFF, "GETIN stack must balance");
                assert_eq!(system.mem.ram[0x0400], 0, "{profile:?}/{stage}:\n{source}");
            }
        }
    }

    #[test]
    fn fixpoint_chains_rules() {
        let input = "    JSR __H\n    RTS\n    LDA #$00\nL1:\n    RTS\n";
        let got = run_default(input);
        assert!(got.contains("JMP __H"));
        assert!(!got.contains("LDA #$00"));
    }

    #[test]
    fn parser_roundtrips_simple_program() {
        let input = "*=$080D\n\nL10:\n    LDA #$0A\n    JSR $FFD2\n    RTS\n";
        let got = render(&parse(input));
        assert_eq!(got, input);
    }

    #[test]
    fn ph210_promotes_loop_index_to_x() {
        let input = "    LDA #$FF\n    STA VI_X\n\
L1:\n    INC VI_X\n    LDA VI_X\n    CMP VI_M\n    BEQ L2\n\
L3:\n    LDX VI_X\n    LDA $0400,X\n    STA VI_D\n    JMP L1\n\
L2:\n    RTS\n";
        let got = run_default(input);
        assert!(got.contains("INX"), "INC->INX:\n{got}");
        assert!(got.contains("CPX VI_M"), "CMP->CPX:\n{got}");
        assert!(!got.contains("LDX VI_X"), "LDX reload dropped:\n{got}");
    }

    #[test]
    fn ph210_bails_when_x_used_otherwise() {
        let input = "    LDA #$FF\n    STA VI_X\n\
L1:\n    INC VI_X\n    LDA VI_X\n    CMP VI_M\n    BEQ L2\n\
    LDX VI_X\n    TXA\n    STA VI_D\n    JMP L1\n\
L2:\n    RTS\n";
        let got = run_default(input);
        assert!(got.contains("INC VI_X"), "INC kept (not promoted):\n{got}");
    }

    const KILL_TAIL: &str = "    LDA #$00\n    CMP VI_Q\n    RTS\n";

    fn ph211_body(body: &str) -> String {
        format!("*=$080D\n{body}{KILL_TAIL}")
    }

    #[test]
    fn ph211_folds_tya_into_sty() {
        let got = run_default(&ph211_body("    LDY VS_A\n    TYA\n    STA VI_P\n"));
        assert!(got.contains("STY VI_P"), "TYA/STA -> STY:\n{got}");
        assert!(!got.contains("    TYA\n"), "transfer gone:\n{got}");
    }

    #[test]
    fn ph211_folds_txa_into_stx() {
        let got = run_default(&ph211_body("    LDX VS_A\n    TXA\n    STA VI_P\n"));
        assert!(got.contains("STX VI_P"), "TXA/STA -> STX:\n{got}");
        assert!(!got.contains("    TXA\n"), "transfer gone:\n{got}");
    }

    #[test]
    fn ph211_keeps_transfer_when_a_is_live_after() {
        // `STA VI_R` downstream reads the byte `TYA` put in `.A`.
        let input = "*=$080D\n    LDY VS_A\n    TYA\n    STA VI_P\n    STA VI_R\n    RTS\n";
        let got = run_default(input);
        assert!(
            got.contains("    TYA\n"),
            "A still live, transfer kept:\n{got}"
        );
    }

    #[test]
    fn ph211_keeps_transfer_when_flags_are_live_after() {
        let input = "*=$080D\n    LDY VS_A\n    TYA\n    STA VI_P\n    BEQ L1\n\
    LDA #$01\n    STA VI_R\nL1:\n    RTS\n";
        let got = run_default(input);
        assert!(got.contains("    TYA\n"), "N/Z live, transfer kept:\n{got}");
    }

    #[test]
    fn ph211_refuses_indirect_and_absolute_indexed_stores() {
        // `STY` has neither `(zp),Y` nor `abs,X`.
        for op in ["($FD),Y", "$C000,X"] {
            let got = run_default(&ph211_body(&format!(
                "    LDY VS_A\n    TYA\n    STA {op}\n"
            )));
            assert!(
                got.contains("    TYA\n"),
                "STY cannot encode `{op}`, transfer must stay:\n{got}"
            );
        }
    }

    #[test]
    fn ph211_refuses_the_wrong_index_register() {
        let got = run_default(&ph211_body("    LDY VS_A\n    TYA\n    STA $40,Y\n"));
        assert!(got.contains("    TYA\n"), "no STY zp,Y encoding:\n{got}");
        let got = run_default(&ph211_body("    LDX VS_A\n    TXA\n    STA $40,Y\n"));
        assert!(got.contains("STX $40,Y"), "STX zp,Y exists:\n{got}");
    }

    #[test]
    fn ph211_takes_zero_page_indexed_only_when_the_base_is_proved() {
        let zp = format!("*=$080D\nFU_0 = $A9\n    LDY VS_A\n    TYA\n    STA FU_0,X\n{KILL_TAIL}");
        let got = run_default(&zp);
        assert!(got.contains("STY FU_0,X"), "proved ZP base folds:\n{got}");

        let got = run_default(&ph211_body("    LDY VS_A\n    TYA\n    STA ARR_B,X\n"));
        assert!(
            got.contains("    TYA\n"),
            "unproved base could be absolute:\n{got}"
        );
    }

    #[test]
    fn ph211_refuses_a_self_modified_store() {

        let input = format!(
            "*=$080D\n    LDA #$07\n    STA __W+1\n    LDY VS_A\n\
__W:\n    TYA\n    STA VI_P\n{KILL_TAIL}"
        );
        let got = run_default(&input);
        assert!(got.contains("    TYA\n"), "SMC target left alone:\n{got}");
    }

    #[test]
    fn ph212_folds_the_doubling_into_an_immediate_load() {
        let input = format!(
            "*=$080D\n    LDA #$09\n    ASL\n    TAX\n\
    LDA #$96\n    STA __ARR_H2_I,X\n{KILL_TAIL}"
        );
        let got = run_default(&input);
        assert!(got.contains("LDX #$12"), "2*$09 = $12:\n{got}");
        assert!(!got.contains("    ASL\n"), "shift gone:\n{got}");
        assert!(!got.contains("    TAX\n"), "transfer gone:\n{got}");
    }

    #[test]
    fn ph212_wraps_like_the_shift_it_replaces() {
        let input = format!("*=$080D\n    LDA #$C0\n    ASL\n    TAX\n    STX VI_P\n{KILL_TAIL}");
        let got = run_default(&input);
        assert!(got.contains("LDX #$80"), "low byte of 2*$C0:\n{got}");
    }

    #[test]
    fn ph212_handles_the_tay_twin_and_the_explicit_accumulator() {
        let input = format!("*=$080D\n    LDA #$03\n    ASL A\n    TAY\n    STY VI_P\n{KILL_TAIL}");
        let got = run_default(&input);
        assert!(got.contains("LDY #$06"), "`ASL A` spelling, TAY:\n{got}");
    }

    #[test]
    fn ph212_keeps_the_shift_when_carry_is_live() {
        let input = "*=$080D\n    LDA #$C0\n    ASL\n    TAX\n    BCC L1\n\
    LDA #$01\n    STA VI_P\nL1:\n    LDA #$00\n    STA VI_Q\n    RTS\n";
        let got = run_default(input);
        assert!(got.contains("    ASL\n"), "carry live, shift kept:\n{got}");
    }

    #[test]
    fn ph212_keeps_the_shift_when_a_is_live() {
        let input = format!("*=$080D\n    LDA #$09\n    ASL\n    TAX\n    STA VI_P\n{KILL_TAIL}");
        let got = run_default(&input);
        assert!(got.contains("    ASL\n"), "A live, shift kept:\n{got}");
    }

    #[test]
    fn ph212_refuses_a_memory_shift() {

        let input =
            format!("*=$080D\n    LDA #$09\n    ASL VI_P\n    TAX\n    STX VI_R\n{KILL_TAIL}");
        let got = run_default(&input);
        assert!(got.contains("ASL VI_P"), "memory shift untouched:\n{got}");
    }

    #[test]
    fn ph213_steps_a_known_index_up_and_down() {
        let cases = [
            ("LDX", "#$05", "#$06", "INX"),
            ("LDX", "#$05", "#$04", "DEX"),
            ("LDY", "#$05", "#$06", "INY"),
            ("LDY", "#$05", "#$04", "DEY"),
        ];
        for (ld, first, second, want) in cases {
            let input = format!(
                "*=$080D\n    {ld} {first}\n    STX VI_P\n    STY VI_R\n\
    {ld} {second}\n    STX VI_S\n    STY VI_T\n    RTS\n"
            );
            let got = run_default(&input);
            assert!(
                got.contains(&format!("    {want}\n")),
                "{ld} {first} then {second} -> {want}:\n{got}"
            );
        }
    }

    #[test]
    fn ph213_wraps_at_both_ends() {
        let input = "*=$080D\n    LDX #$FF\n    STX VI_P\n    LDX #$00\n    STX VI_R\n    RTS\n";
        let got = run_default(&input);
        assert!(got.contains("    INX\n"), "$FF -> $00 is INX:\n{got}");

        let input = "*=$080D\n    LDY #$00\n    STY VI_P\n    LDY #$FF\n    STY VI_R\n    RTS\n";
        let got = run_default(&input);
        assert!(got.contains("    DEY\n"), "$00 -> $FF is DEY:\n{got}");
    }

    #[test]
    fn ph213_needs_a_known_value_in_the_same_register() {
        // `.A` holding $05 says nothing about `.X`.
        let input = "*=$080D\n    LDA #$05\n    STA VI_P\n    LDX #$06\n    STX VI_R\n    RTS\n";
        let got = run_default(input);
        assert!(got.contains("LDX #$06"), "no X fact, load kept:\n{got}");
    }

    #[test]
    fn ph213_resets_its_facts_at_a_label() {

        let input = "*=$080D\n    LDA VI_G\n    BEQ L1\n    LDX #$05\n    STX VI_P\n\
L1:\n    LDX #$06\n    STX VI_R\n    RTS\n";
        let got = run_default(input);
        assert!(got.contains("LDX #$06"), "fact reset at label:\n{got}");
    }

    #[test]
    fn ph213_leaves_a_two_step_gap_alone() {
        let input = "*=$080D\n    LDX #$05\n    STX VI_P\n    LDX #$07\n    STX VI_R\n    RTS\n";
        let got = run_default(input);
        assert!(got.contains("LDX #$07"), "delta 2 is not a bump:\n{got}");
    }

    #[test]
    fn ph213_refuses_a_self_modified_immediate() {

        let input = "*=$080D\n    LDA #$07\n    STA __W+1\n    LDX #$05\n    STX VI_P\n\
__W:\n    LDX #$06\n    STX VI_R\n    RTS\n";
        let got = run_default(input);
        assert!(got.contains("LDX #$06"), "patched immediate kept:\n{got}");
    }

    fn ph214_body(branch: &str) -> String {
        format!(
            "*=$080D\n    LDA VI_P\n    AND #$01\n    {branch} L1\n\
    LDA #$01\n    STA VI_R\nL1:\n{KILL_TAIL}"
        )
    }

    #[test]
    fn ph214_turns_the_bit_test_into_a_carry_test() {
        let got = run_default(&ph214_body("BEQ"));
        assert!(got.contains("    LSR\n"), "AND -> LSR:\n{got}");
        assert!(got.contains("BCC L1"), "BEQ -> BCC:\n{got}");
    }

    #[test]
    fn ph214_inverts_the_other_polarity() {
        let got = run_default(&ph214_body("BNE"));
        assert!(got.contains("    LSR\n"), "AND -> LSR:\n{got}");
        assert!(got.contains("BCS L1"), "BNE -> BCS:\n{got}");
    }

    #[test]
    fn ph214_keeps_the_mask_when_a_survives_the_branch() {
        // `STA VI_R` on the fallthrough reads `a & 1`, which `LSR` does not produce.
        let input = format!(
            "*=$080D\n    LDA VI_P\n    AND #$01\n    BEQ L1\n    STA VI_R\nL1:\n{KILL_TAIL}"
        );
        let got = run_default(&input);
        assert!(got.contains("AND #$01"), "A live, mask kept:\n{got}");
    }

    #[test]
    fn ph214_keeps_the_mask_when_carry_survives_the_branch() {
        let input = format!(
            "*=$080D\n    LDA VI_P\n    AND #$01\n    BEQ L1\n\
    LDA #$01\n    STA VI_R\nL1:\n    BCS L2\n    LDA #$02\n    STA VI_S\n\
L2:\n{KILL_TAIL}"
        );
        let got = run_default(&input);
        assert!(got.contains("AND #$01"), "carry live, mask kept:\n{got}");
    }

    #[test]
    fn ph214_refuses_the_self_modified_wait_mask() {
        let input = format!(
            "*=$080D\n    JSR __FAC_BYTE\n    STA __WAIT_AND_0+1\n\
__WAIT_TOP_0:\n__WAIT_LDA_0:\n    LDA $FFFF\n__WAIT_AND_0:\n    AND #$01\n\
    BEQ __WAIT_TOP_0\n{KILL_TAIL}"
        );
        let got = run_default(&input);
        assert!(got.contains("AND #$01"), "patched mask untouched:\n{got}");
        assert!(!got.contains("    LSR\n"), "no LSR anywhere:\n{got}");
    }

    /// Six identical windows containing a foldable `TYA`.
    #[test]
    fn ph301_still_factors_a_window_containing_a_foldable_transfer() {

        let mut src = String::from("*=$080D\n");
        for _ in 0..6 {
            src.push_str(
                "    LDY #$07\n    STA __LAST_STR_PTR_LO\n    TYA\n\
    STA __LAST_STR_PTR_HI\n    LDA #$00\n    JSR __OPAQUE\n",
            );
        }
        src.push_str("    RTS\n__OPAQUE:\n    STA VI_Q\n    RTS\n__BSS_START:\n    .byte $00\n");
        let got = run_default(&src);
        assert!(
            got.contains("__SEQ_0:"),
            "the repeated window must still be factored:\n{got}"
        );

        assert_eq!(
            got.matches("__LAST_STR_PTR_LO").count(),
            2,
            "five of six copies must fold into the helper:\n{got}"
        );
        assert_eq!(
            got.matches("STY __LAST_STR_PTR_HI").count(),
            2,
            "PH211 folds the transfer in the helper AND inline:\n{got}"
        );
        assert!(!got.contains("    TYA\n"), "no transfer left:\n{got}");
    }

    #[test]
    fn ph019_still_folds_the_plain_load_transfer_pair() {
        let input = "*=$080D\n    LDA VI_P\n    TAX\n    LDA #$00\n    STA VI_Q\n\
    STX VI_R\n    RTS\n";
        let got = run_default(input);
        assert!(got.contains("LDX VI_P"), "PH019 still fires:\n{got}");
        assert!(!got.contains("    TAX\n"), "transfer gone:\n{got}");
    }

    #[test]
    fn ph020_still_drops_the_equal_valued_reload() {
        let input = "*=$080D\n    LDX #$05\n    STX VI_P\n    LDX #$05\n    STX VI_R\n    RTS\n";
        let got = run_default(input);
        assert_eq!(
            got.matches("LDX #$05").count(),
            1,
            "the second load is dropped, not bumped:\n{got}"
        );
        assert!(!got.contains("    INX\n"), "no spurious bump:\n{got}");
        assert!(!got.contains("    DEX\n"), "no spurious bump:\n{got}");
    }

    #[test]
    fn ph206_still_cancels_an_adjacent_push_pop() {
        let input = "*=$080D\n    LDY VS_A\n    TYA\n    PHA\n    PLA\n\
    STA VI_P\n    LDA #$00\n    STA VI_Q\n    RTS\n";
        let got = run_default(input);
        assert!(!got.contains("    PHA\n"), "push gone:\n{got}");
        assert!(!got.contains("    PLA\n"), "pop gone:\n{got}");
        assert!(got.contains("STY VI_P"), "and PH211 then folds:\n{got}");
    }

    #[test]
    fn the_new_rules_reach_a_fixpoint() {
        let input = "*=$080D\n    LDY VS_A\n    TYA\n    STA VI_P\n    LDX #$05\n\
    STX VI_R\n    LDX #$06\n    STX VI_S\n    LDA #$09\n    ASL\n    TAX\n\
    STX VI_T\n    LDA #$00\n    STA VI_Q\n    RTS\n";
        let once = run_default(input);
        let twice = run_default(&once);
        assert_eq!(once, twice, "second pass must change nothing");
    }

    #[test]
    fn ph307_an_inc_delta_keeps_the_stamp_it_reads() {
        let input = concat!(
            "    LDA #$0A\n",
            "    STA $39\n",
            "    INC $39\n",
            "    JSR $B867\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        ph307_dead_curlin_stamp(&mut items);
        let got = render(&items);
        assert!(
            got.contains("STA $39"),
            "the INC below reads this stamp:\n{got}"
        );
        assert!(got.contains("INC $39"), "and the INC itself is live:\n{got}");
    }

    #[test]
    fn ph307_a_dead_inc_takes_the_stamp_with_it() {
        let input = concat!(
            "    LDA #$0A\n",
            "    STA $39\n",
            "    INC $39\n",
            "    JSR $BBA2\n",
            "    LDA #$14\n",
            "    STA $39\n",
            "    JSR $B867\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        assert!(ph307_dead_curlin_stamp(&mut items));
        let got = render(&items);
        assert!(!got.contains("INC $39"), "the INC's result is dead:\n{got}");
        assert_eq!(
            got.matches("STA $39").count(),
            1,
            "only the stamp $B867 can read survives:\n{got}"
        );
        assert!(got.contains("LDA #$14"), "and it is the right one:\n{got}");
    }

    #[test]
    fn ph307_strout_observes_the_stamp() {
        let input = concat!(
            "    LDA #$0A\n",
            "    STA $39\n",
            "    JSR $AB1E\n",
            "    LDA #$14\n",
            "    STA $39\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        ph307_dead_curlin_stamp(&mut items);
        let got = render(&items);
        assert!(
            got.contains("LDA #$0A"),
            "STROUT can raise ?DEVICE NOT PRESENT and name the line:\n{got}"
        );
    }

    #[test]
    fn ph307_a_pure_fac_move_does_not_observe() {
        let input = concat!(
            "    LDA #$0A\n",
            "    STA $39\n",
            "    JSR $BBA2\n",
            "    LDA #$14\n",
            "    STA $39\n",
            "    JSR $AB1E\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        assert!(ph307_dead_curlin_stamp(&mut items));
        let got = render(&items);
        assert_eq!(
            got.matches("STA $39").count(),
            1,
            "$BBA2 only copies FAC1 in and reads nothing:\n{got}"
        );
    }

    #[test]
    fn ph307_a_float_store_observes_because_rounding_can_overflow() {
        let input = concat!(
            "    LDA #$0A\n",
            "    STA $39\n",
            "    JSR $BBD4\n",
            "    LDA #$14\n",
            "    STA $39\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        ph307_dead_curlin_stamp(&mut items);
        let got = render(&items);
        assert_eq!(
            got.matches("STA $39").count(),
            2,
            "MOVMF rounds, and rounding can overflow:\n{got}"
        );
    }

    /// `PEEK(57)` lowers to `LDA $39`.
    #[test]
    fn ph307_a_direct_read_keeps_the_stamp() {
        let input = concat!(
            "    LDA #$0A\n",
            "    STA $39\n",
            "    LDA $39\n",
            "    STA VI_A\n",
            "    LDA #$14\n",
            "    STA $39\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        ph307_dead_curlin_stamp(&mut items);
        let got = render(&items);
        assert_eq!(
            got.matches("STA $39").count(),
            2,
            "the LDA $39 between them reads the first:\n{got}"
        );
    }

    #[test]
    fn ph307_rts_observes_the_stamp() {
        let input = concat!(
            "__SUB:\n",
            "    LDA #$0A\n",
            "    STA $39\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        assert!(!ph307_dead_curlin_stamp(&mut items));
    }

    #[test]
    fn ph307_an_unknown_call_target_observes() {
        let input = concat!(
            "    LDA #$0A\n",
            "    STA $39\n",
            "    JSR $FFFF\n",
            "    LDA #$14\n",
            "    STA $39\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        ph307_dead_curlin_stamp(&mut items);
        let got = render(&items);
        assert_eq!(
            got.matches("STA $39").count(),
            2,
            "a SYS thunk is opaque:\n{got}"
        );
    }

    #[test]
    fn ph307_a_branch_keeps_a_stamp_live_through_one_arm() {
        let input = concat!(
            "    LDA #$0A\n",
            "    STA $39\n",
            "    BEQ __HOT\n",
            "    JSR $BBA2\n",
            "    LDA #$14\n",
            "    STA $39\n",
            "    RTS\n",
            "__HOT:\n",
            "    JSR $B867\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        ph307_dead_curlin_stamp(&mut items);
        let got = render(&items);
        assert!(
            got.contains("LDA #$0A"),
            "the taken arm reaches FADD:\n{got}"
        );
    }

    /// A `JSR` to a local helper observes iff the helper can.
    #[test]
    fn ph307_a_call_inherits_its_callees_answer() {
        let quiet = concat!(
            "    LDA #$0A\n",
            "    STA $39\n",
            "    JSR __QUIET\n",
            "    LDA #$14\n",
            "    STA $39\n",
            "    JSR $B867\n",
            "    RTS\n",
            "__QUIET:\n",
            "    JSR $BBA2\n",
            "    RTS\n",
        );
        let mut items = parse(quiet);
        assert!(ph307_dead_curlin_stamp(&mut items));
        let got = render(&items);
        assert_eq!(
            got.matches("STA $39").count(),
            1,
            "__QUIET cannot raise, so the first stamp is dead:\n{got}"
        );

        let loud = quiet.replace("__QUIET:\n    JSR $BBA2", "__QUIET:\n    JSR $B867");
        let mut items = parse(&loud);
        ph307_dead_curlin_stamp(&mut items);
        let got = render(&items);
        assert_eq!(
            got.matches("STA $39").count(),
            2,
            "__QUIET reaches FADD, so the first stamp is live:\n{got}"
        );
    }

    #[test]
    fn ph307_the_two_bytes_die_independently() {
        let input = concat!(
            "    LDA #$0A\n",
            "    STA $39\n",
            "    LDA #$01\n",
            "    STA $3A\n",
            "    LDA #$00\n",
            "    STA $3A\n",
            "    JSR $B867\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        assert!(ph307_dead_curlin_stamp(&mut items));
        let got = render(&items);
        assert_eq!(
            got.matches("STA $3A").count(),
            1,
            "the $01 high stamp is overwritten before anything reads it:\n{got}"
        );
        assert!(
            got.contains("STA $39"),
            "the low byte is untouched by that:\n{got}"
        );
    }

    #[test]
    fn ph307_an_unresolved_store_does_not_kill_the_cell() {
        let input = concat!(
            "    LDA #$0A\n",
            "    STA $39\n",
            "    LDA #$00\n",
            "    STA ($FB),Y\n",
            "    JSR $B867\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        assert!(!ph307_dead_curlin_stamp(&mut items));
    }


    #[test]
    fn ph316_folds_the_gc_sixteen_bit_pair() {
        let input = concat!(
            "__GC_LOOP:\n",
            "    SEC\n",
            "    LDA __GC_LOW_LO\n",
            "    SBC __HEAP_PTR\n",
            "    LDA __GC_LOW_HI\n",
            "    SBC __HEAP_PTR+1\n",
            "    BCS __GC_DONE\n",
            "    EOR #$80\n",
            "__GC_DONE:\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        assert!(ph316_sec_lda_sbc_to_cmp(&mut items));
        let got = render(&items);
        assert!(!got.contains("SEC"), "the SEC dies:\n{got}");
        assert!(
            got.contains("CMP __HEAP_PTR\n"),
            "first SBC rewritten to CMP:\n{got}"
        );
        assert!(
            got.contains("SBC __HEAP_PTR+1"),
            "the borrow-chained second SBC stays a subtraction:\n{got}"
        );
        assert!(got.contains("BCS __GC_DONE"), "carry consumer intact:\n{got}");
    }

    #[test]
    fn ph316_folds_a_single_site() {
        let input = concat!(
            "    SEC\n",
            "    LDA #$05\n",
            "    SBC ZSI_V_N\n",
            "    LDA #$00\n",
            "    SBC ZSI_V_N+1\n",
            "    BVC NV\n",
            "    EOR #$80\n",
            "NV:\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        assert!(ph316_sec_lda_sbc_to_cmp(&mut items));
        let got = render(&items);
        assert!(!got.contains("SEC"), "SEC dropped:\n{got}");
        assert!(
            got.contains("CMP ZSI_V_N\n"),
            "SBC became CMP, operand verbatim:\n{got}"
        );
    }

    #[test]
    fn ph316_tolerates_blanks_and_comments_in_the_window() {
        let input = concat!(
            "    SEC\n",
            "\n",
            "    LDA T1\n",
            "; keep the flags\n",
            "    SBC T0\n",
            "\n",
            "    LDA T1+1\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        assert!(ph316_sec_lda_sbc_to_cmp(&mut items));
        let got = render(&items);
        assert!(got.contains("CMP T0"), "folded across filler:\n{got}");
        assert!(!got.contains("SEC"), "SEC dropped:\n{got}");
    }

    #[test]
    fn ph316_refuses_a_label_inside_the_window() {
        let input = concat!(
            "    SEC\n",
            "MID:\n",
            "    LDA T1\n",
            "    SBC T0\n",
            "    LDA T1+1\n",
            "    JMP MID\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        assert!(!ph316_sec_lda_sbc_to_cmp(&mut items));
        let got = render(&items);
        assert!(got.contains("SEC"), "SEC kept:\n{got}");
        assert!(got.contains("SBC T0"), "SBC kept:\n{got}");
    }

    #[test]
    fn ph316_refuses_when_sed_exists_anywhere() {
        let input = concat!(
            "    SEC\n",
            "    LDA T1\n",
            "    SBC T0\n",
            "    LDA T1+1\n",
            "    SED\n",
            "    SBC #$01\n",
            "    CLD\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        assert!(!ph316_sec_lda_sbc_to_cmp(&mut items));
        let got = render(&items);
        assert!(got.contains("SBC T0"), "SBC kept:\n{got}");
    }

    #[test]
    fn ph316_refuses_a_runtime_patched_sbc() {
        let input = concat!(
            "    SEC\n",
            "    LDA T1\n",
            "PATCH:\n",
            "    SBC T0\n",
            "    LDA T1+1\n",
            "    RTS\n",
            "    STA PATCH+1\n",
        );
        let mut items = parse(input);
        assert!(!ph316_sec_lda_sbc_to_cmp(&mut items));
        let got = render(&items);
        assert!(got.contains("SBC T0"), "patched SBC untouched:\n{got}");
        assert!(got.contains("SEC"), "SEC kept with it:\n{got}");
    }

    #[test]
    fn ph316_refuses_stack_io_and_indirect_operands() {
        for op in ["$01FB", "$D012", "($FB),Y"] {
            let input = format!(
                concat!(
                    "    SEC\n",
                    "    LDA T1\n",
                    "    SBC {op}\n",
                    "    LDA T1+1\n",
                    "    RTS\n",
                ),
                op = op
            );
            let mut items = parse(&input);
            assert!(
                !ph316_sec_lda_sbc_to_cmp(&mut items),
                "{op} must be refused"
            );
            let got = render(&items);
            assert!(got.contains("SEC"), "SEC kept for {op}:\n{got}");
        }
    }




    fn ph319_tail() -> &'static str {
        concat!(
            "    CLC
",
            "    CLV
",
            "    LDA #$05
",
            "    RTS
",
        )
    }

    #[test]
    fn ph319_folds_the_add_carry_shape() {
        let input = format!(
            concat!(
                "    CLC\n",
                "    LDA ZSI_V_K\n",
                "    ADC ZSI_V_J\n",
                "    STA ZSI_V_K\n",
                "    LDA ZSI_V_K+1\n",
                "    ADC #$00\n",
                "    STA ZSI_V_K+1\n",
                "{tail}",
            ),
            tail = ph319_tail()
        );
        let mut items = parse(&input);
        assert!(ph319_carry_fold(&mut items));
        let got = render(&items);
        assert!(
            got.contains("    BCC __PH319_0\n    INC ZSI_V_K+1\n__PH319_0:\n"),
            "high half folded on the low add's carry:\n{got}"
        );
        assert!(
            got.contains("    ADC ZSI_V_J\n"),
            "the low half that produced the carry is untouched:\n{got}"
        );
        assert!(!got.contains("ADC #$00"), "zero-operand ALU gone:\n{got}");
    }

    #[test]
    fn ph319_folds_the_sbc_borrow_shape() {
        let input = format!(
            concat!(
                "    SEC\n",
                "    LDA ZSI_V_K\n",
                "    SBC ZSI_V_J\n",
                "    STA ZSI_V_K\n",
                "    LDA ZSI_V_K+1\n",
                "    SBC #$00\n",
                "    STA ZSI_V_K+1\n",
                "{tail}",
            ),
            tail = ph319_tail()
        );
        let mut items = parse(&input);
        assert!(ph319_carry_fold(&mut items));
        let got = render(&items);
        assert!(
            got.contains("    BCS __PH319_0\n    DEC ZSI_V_K+1\n__PH319_0:\n"),
            "high half folded on the low subtract's borrow:\n{got}"
        );
        assert!(!got.contains("SBC #$00"), "zero-operand ALU gone:\n{got}");
    }

    #[test]
    fn ph319_folds_a_carry_from_a_compare() {
        let input = format!(
            concat!(
                "    LDA ZSI_V_K\n",
                "    CMP #$10\n",
                "    LDA ZSI_V_K+1\n",
                "    ADC #$00\n",
                "    STA ZSI_V_K+1\n",
                "{tail}",
            ),
            tail = ph319_tail()
        );
        let mut items = parse(&input);
        assert!(ph319_carry_fold(&mut items));
        let got = render(&items);
        assert!(
            got.contains("    BCC __PH319_0\n    INC ZSI_V_K+1\n__PH319_0:\n"),
            "carry source is irrelevant:\n{got}"
        );
    }

    #[test]
    fn ph319_refuses_when_any_exit_state_is_live() {
        for (consumer, what) in [
            ("    TAX\n", "A"),
            ("    BPL SKIP\nSKIP:\n", "N"),
            ("    BEQ SKIP\nSKIP:\n", "Z"),
            ("    BVC SKIP\nSKIP:\n", "V"),
            ("    BCS SKIP\nSKIP:\n", "C"),
        ] {
            let input = format!(
                concat!(
                    "    CLC\n",
                    "    LDA ZSI_V_K\n",
                    "    ADC ZSI_V_J\n",
                    "    STA ZSI_V_K\n",
                    "    LDA ZSI_V_K+1\n",
                    "    ADC #$00\n",
                    "    STA ZSI_V_K+1\n",
                    "{consumer}",
                    "    CLC\n",
                    "    CLV\n",
                    "    LDA #$05\n",
                    "    RTS\n",
                ),
                consumer = consumer
            );
            let mut items = parse(&input);
            assert!(
                !ph319_carry_fold(&mut items),
                "a {what}-reading consumer must refuse the fold"
            );
            let got = render(&items);
            assert!(got.contains("ADC #$00"), "{what} site kept:\n{got}");
        }
    }

    #[test]
    fn ph319_refuses_a_store_to_a_different_cell() {
        let input = format!(
            concat!(
                "    CLC\n",
                "    LDA ZSI_V_K\n",
                "    ADC ZSI_V_J\n",
                "    STA ZSI_V_I\n",
                "    LDA ZSI_V_K+1\n",
                "    ADC #$00\n",
                "    STA ZSI_V_I+1\n",
                "{tail}",
            ),
            tail = ph319_tail()
        );
        let mut items = parse(&input);
        assert!(!ph319_carry_fold(&mut items));
        let got = render(&items);
        assert!(got.contains("ADC #$00"), "copy shape kept:\n{got}");
    }

    #[test]
    fn ph319_refuses_a_label_inside_the_window() {
        let input = format!(
            concat!(
                "    CLC\n",
                "    LDA ZSI_V_K\n",
                "    ADC ZSI_V_J\n",
                "    STA ZSI_V_K\n",
                "MID:\n",
                "    LDA ZSI_V_K+1\n",
                "    ADC #$00\n",
                "    STA ZSI_V_K+1\n",
                "    JMP MID\n",
                "{tail}",
            ),
            tail = ph319_tail()
        );
        let mut items = parse(&input);
        assert!(!ph319_carry_fold(&mut items));
        let got = render(&items);
        assert!(got.contains("ADC #$00"), "window kept:\n{got}");
    }

    #[test]
    fn ph319_refuses_when_sed_or_php_exists_anywhere() {
        for extra in ["    SED\n    CLD\n", "    PHP\n    PLA\n"] {
            let input = format!(
                concat!(
                    "    CLC\n",
                    "    LDA ZSI_V_K\n",
                    "    ADC ZSI_V_J\n",
                    "    STA ZSI_V_K\n",
                    "    LDA ZSI_V_K+1\n",
                    "    ADC #$00\n",
                    "    STA ZSI_V_K+1\n",
                    "{extra}",
                    "{tail}",
                ),
                extra = extra,
                tail = ph319_tail()
            );
            let mut items = parse(&input);
            assert!(!ph319_carry_fold(&mut items));
            let got = render(&items);
            assert!(got.contains("ADC #$00"), "site kept:\n{got}");
        }
    }

    #[test]
    fn ph319_refuses_a_runtime_patched_window() {
        let build = |patch: &str| {
            format!(
                concat!(
                    "    CLC\n",
                    "    LDA ZSI_V_K\n",
                    "    ADC ZSI_V_J\n",
                    "    STA ZSI_V_K\n",
                    "    LDA ZSI_V_K+1\n",
                    "    ADC #$00\n",
                    "    STA ZSI_V_K+1\n",
                    "{patch}",
                    "{tail}",
                ),
                patch = patch,
                tail = ph319_tail()
            )
        };
        let mut items = parse(&build(""));
        assert!(ph319_carry_fold(&mut items), "control folds");
        let mut items = parse(&build(""));
        let head = items
            .iter()
            .position(|it| matches!(it, Item::Insn(i) if i.mnem == "LDA" && i.operand.as_deref() == Some("ZSI_V_K+1")))
            .expect("window head");
        items.insert(head, Item::Label("PATCH".to_string()));
        items.push(Item::Insn(Insn {
            mnem: "STA".to_string(),
            operand: Some("PATCH+1".to_string()),
            comment: None,
        }));
        assert!(!ph319_carry_fold(&mut items), "patched insn refused");
        let got = render(&items);
        assert!(got.contains("ADC #$00"), "window kept:\n{got}");
    }

    #[test]
    fn ph319_mints_unique_labels_and_settles() {
        let one = concat!(
            "    CLC\n",
            "    LDA ZSI_V_K\n",
            "    ADC ZSI_V_J\n",
            "    STA ZSI_V_K\n",
            "    LDA ZSI_V_K+1\n",
            "    ADC #$00\n",
            "    STA ZSI_V_K+1\n",
        );
        let input = format!(
            concat!(
                "{one}",
                "    CLC\n",
                "    CLV\n",
                "    LDA #$05\n",
                "{one}",
                "{tail}",
            ),
            one = one,
            tail = ph319_tail()
        );
        let mut items = parse(&input);
        assert!(ph319_carry_fold(&mut items));
        let got = render(&items);
        assert!(got.contains("__PH319_0:"), "first label:\n{got}");
        assert!(got.contains("__PH319_1:"), "second label:\n{got}");
        assert!(!ph319_carry_fold(&mut items), "settled");
    }

    #[test]
    fn ph107_never_sweeps_its_own_body_behind_a_stacked_label() {
        for extra in ["", "__OTHER:
"] {
            let input = format!(
                concat!(
                    "    LDA #$0D
",
                    "    JMP $FFD2
",
                    "__CHROUT_0D:
",
                    "{extra}",
                    "    LDA #$0D
",
                    "    JMP $FFD2
",
                ),
                extra = extra
            );
            let mut items = parse(&input);
            ph107_sweep_chrout_tails(&mut items);
            let got = render(&items);
            let body = got.split("__CHROUT_0D:").nth(1).unwrap_or("");
            assert!(
                !body.contains("JMP __CHROUT_0D"),
                "helper body jumps to itself (extra label {extra:?}):
{got}"
            );
        }
    }

    #[test]
    fn ph320_caps_a_run_at_two_hundred_fifty_five_blocks() {
        let mut input = String::new();
        for n in 0..260 {
            input.push_str(&format!(
                "    INC ZSI_V_K
    BNE __A{n}
    INC ZSI_V_K+1
__A{n}:
"
            ));
        }
        input.push_str(ph319_tail());
        let mut items = parse(&input);
        assert!(ph320_merge_inc_runs(&mut items));
        let got = render(&items);
        // Every minted immediate stays a single byte.
        for line in got.lines().filter(|l| l.trim_start().starts_with("ADC #$")) {
            let imm = line.trim().trim_start_matches("ADC #$");
            assert!(
                imm.len() <= 2,
                "immediate wider than one byte: {line}
{got}"
            );
        }
        assert!(
            !got.contains("ADC #$104"),
            "the run merged past a one-byte immediate:
{got}"
        );
        assert!(got.contains("ADC #$05"), "the trailing five merge:
{got}");
    }


    #[test]
    fn ph320_merges_a_triple_inc_run() {
        let input = format!(
            concat!(
                "    INC ZSI_V_K\n",
                "    BNE __A1\n",
                "    INC ZSI_V_K+1\n",
                "__A1:\n",
                "    INC ZSI_V_K\n",
                "    BNE __A2\n",
                "    INC ZSI_V_K+1\n",
                "__A2:\n",
                "    INC ZSI_V_K\n",
                "    BNE __A3\n",
                "    INC ZSI_V_K+1\n",
                "__A3:\n",
                "{tail}",
            ),
            tail = ph319_tail()
        );
        let mut items = parse(&input);
        assert!(ph320_merge_inc_runs(&mut items));
        let got = render(&items);
        assert!(
            got.contains(
                "    CLC\n    LDA ZSI_V_K\n    ADC #$03\n    STA ZSI_V_K\n    BCC __PH320_0\n    INC ZSI_V_K+1\n__PH320_0:\n"
            ),
            "three blocks folded into one add-immediate:\n{got}"
        );
        assert!(!got.contains("BNE"), "block chain gone:\n{got}");
    }

    #[test]
    fn ph320_merges_a_pair_run_and_settles() {
        let input = format!(
            concat!(
                "    INC ZSI_V_K\n",
                "    BNE __A1\n",
                "    INC ZSI_V_K+1\n",
                "__A1:\n",
                "    INC ZSI_V_K\n",
                "    BNE __A2\n",
                "    INC ZSI_V_K+1\n",
                "__A2:\n",
                "{tail}",
            ),
            tail = ph319_tail()
        );
        let mut items = parse(&input);
        assert!(ph320_merge_inc_runs(&mut items));
        let got = render(&items);
        assert!(
            got.contains("    ADC #$02\n"),
            "pair run folds with ADC #$02:\n{got}"
        );
        assert!(!ph320_merge_inc_runs(&mut items), "settled");
    }

    #[test]
    fn ph320_leaves_a_single_block_alone() {
        let input = format!(
            concat!(
                "    INC ZSI_V_K\n",
                "    BNE __A1\n",
                "    INC ZSI_V_K+1\n",
                "__A1:\n",
                "{tail}",
            ),
            tail = ph319_tail()
        );
        let mut items = parse(&input);
        assert!(!ph320_merge_inc_runs(&mut items));
        let got = render(&items);
        assert!(got.contains("BNE __A1"), "lone block kept:\n{got}");
    }

    #[test]
    fn ph320_refuses_an_externally_referenced_block_label() {
        let input = format!(
            concat!(
                "    INC ZSI_V_K\n",
                "    BNE __A1\n",
                "    INC ZSI_V_K+1\n",
                "__A1:\n",
                "    INC ZSI_V_K\n",
                "    BNE __A2\n",
                "    INC ZSI_V_K+1\n",
                "__A2:\n",
                "    JMP __A1\n",
                "{tail}",
            ),
            tail = ph319_tail()
        );
        let mut items = parse(&input);
        assert!(!ph320_merge_inc_runs(&mut items));
        let got = render(&items);
        assert!(got.contains("ADC") == false || !got.contains("ADC #$02"), "run kept:\n{got}");
    }

    #[test]
    fn ph320_refuses_when_any_exit_state_is_live() {
        for (consumer, what) in [
            ("    TAX\n", "A"),
            ("    BPL SKIP\nSKIP:\n", "N"),
            ("    BEQ SKIP\nSKIP:\n", "Z"),
            ("    BVC SKIP\nSKIP:\n", "V"),
            ("    BCS SKIP\nSKIP:\n", "C"),
        ] {
            let run = concat!(
                "    INC ZSI_V_K\n    BNE __A1\n    INC ZSI_V_K+1\n__A1:\n",
                "    INC ZSI_V_K\n    BNE __A2\n    INC ZSI_V_K+1\n__A2:\n",
            );
            let input = format!("{run}{consumer}    CLC\n    CLV\n    LDA #$05\n    RTS\n");
            let mut items = parse(&input);
            assert!(
                !ph320_merge_inc_runs(&mut items),
                "a {what}-reading consumer must refuse the merge"
            );
        }
    }


    #[test]
    fn ph321_sinks_the_second_store_to_the_definition() {
        let input = format!(
            concat!(
                "*=$C000\n",
                "    LDA __SRC\n",
                "    STA __GC_TMP_HI\n",
                "    LDA __GC_TMP_LO\n",
                "    STA $FD\n",
                "    LDA __GC_TMP_HI\n",
                "    STA $FE\n",
                "{tail}",
            ),
            tail = ph319_tail()
        );
        let mut items = parse(&input);
        assert!(ph321_store_forward_sink(&mut items));
        let got = render(&items);
        assert!(
            got.contains("    STA __GC_TMP_HI\n    STA $FE\n    LDA __GC_TMP_LO\n    STA $FD\n"),
            "store moved up, reload gone:\n{got}"
        );
        assert_eq!(got.matches("LDA __GC_TMP_HI").count(), 0, "reload dropped");
    }

    #[test]
    fn ph321_refuses_a_window_write_of_x() {
        let input = format!(
            concat!(
                "*=$C000\n",
                "    LDA __SRC\n",
                "    STA __GC_TMP_HI\n",
                "    LDA __OTHER\n",
                "    STA __GC_TMP_HI\n",
                "    LDA __GC_TMP_HI\n",
                "    STA $FE\n",
                "{tail}",
            ),
            tail = ph319_tail()
        );
        let mut items = parse(&input);
        assert!(ph321_store_forward_sink(&mut items));
        let got = render(&items);
        assert_eq!(got.matches("LDA __GC_TMP_HI").count(), 0, "reload dropped:\n{got}");
        assert!(
            got.contains("    STA __GC_TMP_HI\n    STA $FE\n"),
            "the sink rides the second store:\n{got}"
        );
    }

    #[test]
    fn ph321_refuses_a_window_read_of_z() {
        let input = format!(
            concat!(
                "*=$C000\n",
                "    LDA __SRC\n",
                "    STA __GC_TMP_HI\n",
                "    LDA $FE\n",
                "    TAX\n",
                "    LDA __GC_TMP_HI\n",
                "    STA $FE\n",
                "{tail}",
            ),
            tail = ph319_tail()
        );
        let mut items = parse(&input);
        assert!(!ph321_store_forward_sink(&mut items));
    }

    #[test]
    fn ph321_refuses_when_the_reloads_outputs_are_live() {
        for (consumer, _what) in [
            ("    TAX\n", "A"),
            ("    BPL SKIP\nSKIP:\n", "N"),
            ("    BEQ SKIP\nSKIP:\n", "Z"),
        ] {
            let input = format!(
                concat!(
                    "*=$C000\n",
                    "    LDA __SRC\n",
                    "    STA __GC_TMP_HI\n",
                    "    LDA __GC_TMP_LO\n",
                    "    STA $FD\n",
                    "    LDA __GC_TMP_HI\n",
                    "    STA $FE\n",
                    "{consumer}",
                    "    CLC\n    CLV\n    LDA #$05\n    RTS\n",
                ),
                consumer = consumer
            );
            let mut items = parse(&input);
            assert!(
                !ph321_store_forward_sink(&mut items),
                "a live output must refuse the sink"
            );
        }
    }

    #[test]
    fn ph321_refuses_when_z_aliases_x() {
        let input = format!(
            concat!(
                "*=$C000\n",
                "    LDA __SRC\n",
                "    STA __GC_TMP_HI\n",
                "    LDA __GC_TMP_LO\n",
                "    STA $FD\n",
                "    LDA __GC_TMP_HI\n",
                "    STA __GC_TMP_HI\n",
                "{tail}",
            ),
            tail = ph319_tail()
        );
        let mut items = parse(&input);
        assert!(!ph321_store_forward_sink(&mut items));
    }


    #[test]
    fn ph323_folds_the_string_form() {
        let input = concat!(
            "*=$C000
",
            "    LDX #$04
",
            "    LDA #<__STR_7
",
            "    LDY #>__STR_7
",
            "    STA __ARR_N_S,X
",
            "    TYA
",
            "    STA __ARR_N_S+1,X
",
            "    LDX #$00
",
            "    LDA #$01
",
            "    RTS
",
        );
        let mut items = parse(input);
        assert!(ph323_const_index_store_to_abs(&mut items));
        let got = render(&items);
        assert!(
            got.contains(
                "    LDA #<__STR_7
    STA __ARR_N_S+4
    LDY #>__STR_7
    STY __ARR_N_S+1+4
"
            ),
            "both halves went absolute at the constant index:
{got}"
        );
        assert!(!got.contains(",X"), "indexed stores gone:
{got}");
        assert!(!got.contains("TYA"), "the transfer is gone:
{got}");
    }

    #[test]
    fn ph323_folds_the_byte_form() {
        let input = concat!(
            "*=$C000
",
            "    LDX #$03
",
            "    LDA #$2A
",
            "    STA __ARR_N_S,X
",
            "    LDX #$00
",
            "    RTS
",
        );
        let mut items = parse(input);
        assert!(ph323_const_index_store_to_abs(&mut items));
        let got = render(&items);
        assert!(
            got.contains("    LDA #$2A
    STA __ARR_N_S+3
"),
            "byte store went absolute:
{got}"
        );
    }

    #[test]
    fn ph323_refuses_when_x_is_live_after() {
        let input = concat!(
            "*=$C000
",
            "    LDX #$03
",
            "    LDA #$2A
",
            "    STA __ARR_N_S,X
",
            "    STA __ARR_W_S,X
",
            "    RTS
",
        );
        let mut items = parse(input);
        assert!(!ph323_const_index_store_to_abs(&mut items));
        let got = render(&items);
        assert!(got.contains("LDX #$03"), "window kept:
{got}");
    }

    #[test]
    fn ph323_refuses_a_zero_page_base() {
        for base in ["$40", "ZPBASE"] {
            let input = format!(
                concat!(
                    "ZPBASE = $40
",
                    "*=$C000
",
                    "    LDX #$03
",
                    "    LDA #$2A
",
                    "    STA {base},X
",
                    "    LDX #$00
",
                    "    RTS
",
                ),
                base = base
            );
            let mut items = parse(&input);
            assert!(
                !ph323_const_index_store_to_abs(&mut items),
                "zero-page base {base} must be refused"
            );
        }
    }


    #[test]
    fn facts_a_store_through_an_aliased_equate_invalidates_the_cell() {
        let input = concat!(
            "FI_0 = $F7\n",
            "FI_1 = $F7\n",
            "*=$C000\n",
            "    LDA #$00\n",
            "    STA FI_0\n",
            "    LDA #$05\n",
            "    STA FI_1\n",
            "    LDA #$00\n",
            "    STA FI_0\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        ph025_drop_redundant_store(&mut items);
        let got = render(&items);
        assert_eq!(got.matches("STA FI_0").count(), 2, "{got}");
    }

    #[test]
    fn facts_a_store_of_the_same_byte_through_an_alias_is_redundant() {
        let input = concat!(
            "FI_0 = $F7\n",
            "FI_1 = $F7\n",
            "*=$C000\n",
            "    LDA #$00\n",
            "    STA FI_0\n",
            "    STA FI_0+1\n",
            "    STA FI_1\n",
            "    STA FI_1+1\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        assert!(ph025_drop_redundant_store(&mut items));
        let got = render(&items);
        assert!(!got.contains("STA FI_1"), "{got}");
        assert!(got.contains("STA FI_0\n") && got.contains("STA FI_0+1\n"), "{got}");
    }

    #[test]
    fn facts_a_load_through_an_alias_reads_the_known_byte() {
        // `$F7` and FU_0 name one byte that A already holds.
        let input = concat!(
            "FU_0 = $F7\n",
            "*=$C000\n",
            "    LDA $0400\n",
            "    STA FU_0\n",
            "    LDA $F7\n",
            "    STA $0401\n",
            "    LDA #$01\n",
            "    STA $0402\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        assert!(ph020_static_known_value_cleanup(&mut items));
        assert!(!render(&items).contains("LDA $F7"));
    }

    #[test]
    fn ph020_drops_a_reload_whose_flags_already_reflect_the_register() {
        let input = concat!(
            "*=$C000\n",
            "    LDA V_A\n",
            "    STA V_B\n",
            "    LDA V_B\n",
            "    BEQ L1\n",
            "    RTS\n",
            "L1:\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        assert!(ph020_static_known_value_cleanup(&mut items));
        assert!(!render(&items).contains("LDA V_B"));
    }

    #[test]
    fn ph020_does_not_trust_adc_flags_in_a_program_with_sed() {
        // Decimal-mode ADC leaves N/Z off the BCD result on NMOS.
        let input = concat!(
            "*=$C000\n",
            "    SED\n",
            "    CLD\n",
            "    CLC\n",
            "    LDA V_A\n",
            "    ADC #$01\n",
            "    STA V_B\n",
            "    LDA V_B\n",
            "    BEQ L1\n",
            "    RTS\n",
            "L1:\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        ph020_static_known_value_cleanup(&mut items);
        assert!(render(&items).contains("LDA V_B"));
        // Without the SED the same reload goes.
        let mut items = parse(&input.replace("    SED\n", ""));
        assert!(ph020_static_known_value_cleanup(&mut items));
        assert!(!render(&items).contains("LDA V_B"));
    }

    #[test]
    fn ph020_keeps_a_reload_when_a_compare_set_the_flags() {
        let input = concat!(
            "*=$C000\n",
            "    LDA V_A\n",
            "    STA V_B\n",
            "    CPX #$01\n",
            "    LDA V_B\n",
            "    BEQ L1\n",
            "    RTS\n",
            "L1:\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        ph020_static_known_value_cleanup(&mut items);
        assert!(render(&items).contains("LDA V_B"));
    }

    #[test]
    fn ph015_keeps_a_transfer_whose_z_a_later_branch_reads() {
        let input = concat!(
            "*=$C000\n",
            "    LDA V_A\n",
            "    CMP #$05\n",
            "    TAX\n",
            "    STA V_B\n",
            "    BEQ L1\n",
            "    LDX #$00\n",
            "    RTS\n",
            "L1:\n",
            "    LDX #$01\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        ph015_drop_dead_transfer(&mut items);
        assert!(render(&items).contains("    TAX\n"));
    }

    #[test]
    fn ph015_drops_a_transfer_whose_flags_already_reflect_the_byte() {
        let input = concat!(
            "*=$C000\n",
            "    LDA V_A\n",
            "    TAX\n",
            "    STA V_B\n",
            "    BEQ L1\n",
            "    LDX #$00\n",
            "    RTS\n",
            "L1:\n",
            "    LDX #$01\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        assert!(ph015_drop_dead_transfer(&mut items));
        assert!(!render(&items).contains("TAX"));
    }


    #[test]
    fn ph324_hoists_the_for_setup_reload_past_a_load() {
        let input = concat!(
            "FU_5 = $AB\n",
            "*=$C000\n",
            "    LDA #$33\n",
            "    STA FU_5\n",
            "    LDA #$00\n",
            "    LDY FU_5\n",
            "    JSR $B391\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        assert!(ph324_hoist_reload_to_transfer(&mut items));
        let got = render(&items);
        assert!(
            got.contains("    STA FU_5\n    TAY\n    LDA #$00\n    JSR $B391\n"),
            "{got}"
        );
    }

    #[test]
    fn ph324_turns_an_adjacent_reload_into_a_transfer() {
        for (store, load, copy) in [
            ("STA", "LDX", "TAX"),
            ("STA", "LDY", "TAY"),
            ("STX", "LDA", "TXA"),
            ("STY", "LDA", "TYA"),
        ] {
            let input = format!(
                "*=$C000\n    {store} V_A\n    {load} V_A\n    BNE L1\nL1:\n    RTS\n"
            );
            let mut items = parse(&input);
            assert!(ph324_hoist_reload_to_transfer(&mut items), "{store}/{load}");
            let got = render(&items);
            assert!(got.contains(&format!("    {store} V_A\n    {copy}\n    BNE L1\n")), "{got}");
        }
    }

    #[test]
    fn ph324_follows_a_call_into_a_helper_that_redefines_the_flags() {
        let input = concat!(
            "*=$C000\n",
            "    LDA #$01\n",
            "    STA V_A\n",
            "    LDA #$00\n",
            "    LDY V_A\n",
            "    JSR __JSR2_1\n",
            "    RTS\n",
            "__JSR2_1:\n",
            "    JSR $B391\n",
            "    JMP $BBD4\n",
        );
        let mut items = parse(input);
        assert!(ph324_hoist_reload_to_transfer(&mut items));
        assert!(render(&items).contains("    STA V_A\n    TAY\n    LDA #$00\n"));
    }

    #[test]
    fn ph324_refuses_when_the_reloads_flags_are_read() {
        let input = concat!(
            "*=$C000\n",
            "    STA V_A\n",
            "    LDA #$00\n",
            "    LDY V_A\n",
            "    BEQ L1\n",
            "    RTS\n",
            "L1:\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        assert!(!ph324_hoist_reload_to_transfer(&mut items));
    }

    #[test]
    fn ph324_refuses_a_window_that_touches_the_register_or_the_cell() {
        for middle in [
            "    STA $0400,Y\n", // reads Y
            "    INY\n",         // writes Y
            "    INC $F7\n",     // writes the cell through its literal
            "    STA FU_1\n",    // writes the cell through an alias
            "    STA ($FB),Y\n", // could write anywhere
            "    JSR $FFD2\n",   // call
            "    STA $01\n",     // banks another byte in under the cell
            "    SAX $F0\n",     // Unmodeled opcode effects.
        ] {
            let input = format!(
                "FU_0 = $F7\nFU_1 = $F7\n*=$C000\n    STA FU_0\n{middle}    LDY FU_0\n    STY $0400\n    RTS\n"
            );
            let mut items = parse(&input);
            assert!(
                !ph324_hoist_reload_to_transfer(&mut items),
                "window `{}` must block the hoist",
                middle.trim()
            );
        }
    }

    #[test]
    fn ph324_refuses_the_processor_port_and_patched_operands() {
        for (equate, cell) in [("BORDER = $D020\n", "BORDER"), ("SLOT = BASE+1\n", "SLOT")] {
            let input = format!("{equate}*=$C000\n    STA {cell}\n    LDY {cell}\n    STY $0400\n    RTS\n");
            let mut items = parse(&input);
            assert!(!ph324_hoist_reload_to_transfer(&mut items), "{cell}");
        }
        // $01 does not read back the byte written to it.
        let mut items = parse("*=$C000\n    STA $01\n    LDY $01\n    STY $0400\n    RTS\n");
        assert!(!ph324_hoist_reload_to_transfer(&mut items));
        let mut items = parse(concat!(
            "*=$C000\n",
            "    STA V_A\n",
            "__P:\n",
            "    LDY V_A\n",
            "    STY $0400\n",
            "    LDA #$10\n",
            "    STA __P+1\n",
            "    RTS\n",
        ));
        assert!(!ph324_hoist_reload_to_transfer(&mut items));
    }


    fn ph326_program(before: &str) -> String {
        format!(
            concat!(
                "*=$C000\n",
                "    LDA #$01\n",
                "    LDY #$02\n",
                "    STA $05\n",
                "    STY $06\n",
                "    JSR __H\n",
                "    LDA #$03\n",
                "    STA $05\n",
                "    STY $06\n",
                "    JSR __H\n",
                "    LDX #$00\n",
                "    JSR __H\n",
                "{}",
                "__H:\n",
                "    LDY #$00\n",
                "    LDA ($05),Y\n",
                "    RTS\n",
            ),
            before,
        )
    }

    #[test]
    fn ph326_moves_a_shared_call_prefix_into_a_new_entry() {
        let (changed, got) = drive(ph326_absorb_shared_call_prefix, &ph326_program("    RTS\n"));
        assert!(changed, "{got}");
        assert_eq!(got.matches("    JSR __PH326_0\n").count(), 2, "{got}");
        assert!(got.contains("    LDX #$00\n    JSR __H\n"), "the third call keeps its target:\n{got}");
        assert!(
            got.contains("    RTS\n__PH326_0:\n    STA $05\n    STY $06\n__H:\n"),
            "the entry falls into the routine:\n{got}"
        );
        assert_eq!(got.matches("    STA $05\n").count(), 1, "{got}");
    }

    #[test]
    fn ph326_gives_a_shared_label_run_one_entry() {
        let src = concat!(
            "*=$C000\n",
            "    TAX\n",
            "    JSR __A\n",
            "    TAX\n",
            "    JSR __A\n",
            "    STA $05\n",
            "    STY $06\n",
            "    JSR __B\n",
            "    STA $05\n",
            "    STY $06\n",
            "    JSR __B\n",
            "    RTS\n",
            "__A:\n",
            "__B:\n",
            "    LDA $05\n",
            "    RTS\n",
        );
        let (changed, got) = drive(ph326_absorb_shared_call_prefix, src);
        assert!(changed, "{got}");
        for line in got.lines() {
            if let Some(target) = line.trim().strip_prefix("JSR ") {
                assert!(got.contains(&format!("\n{target}:\n")), "{target} is defined:\n{got}");
            }
        }
        assert_eq!(got.matches("__PH326_").count(), 3, "one entry, two calls:\n{got}");
        assert!(got.contains("    STA $05\n    STY $06\n__A:\n"), "the larger prefix wins:\n{got}");
        assert_eq!(got.matches("    TAX\n    JSR __A\n").count(), 2, "{got}");
    }

    #[test]
    fn ph326_needs_a_barrier_in_front_of_the_routine() {
        let (changed, got) = drive(ph326_absorb_shared_call_prefix, &ph326_program("    INC $02\n"));
        assert!(!changed, "{got}");
        // Data in front may be a call's inline arguments, which return into `__H`.
        let (changed, got) = drive(
            ph326_absorb_shared_call_prefix,
            &ph326_program("    RTS\n    JSR __P\n    .word $1234\n"),
        );
        assert!(!changed, "{got}");
    }

    #[test]
    fn ph326_leaves_routines_with_inline_arguments_alone() {
        let asm = ph326_program("    RTS\n").replace(
            "    LDX #$00\n    JSR __H\n",
            "    LDX #$00\n    JSR __H\n    .word $1234\n",
        );
        let (changed, got) = drive(ph326_absorb_shared_call_prefix, &asm);
        assert!(!changed, "{got}");
    }

    #[test]
    fn ph326_refuses_stack_patched_and_label_split_prefixes() {
        // A stack-page store would land below the pushed return address.
        let asm = ph326_program("    RTS\n").replace("    STY $06\n", "    STY $0105\n");
        let (_, got) = drive(ph326_absorb_shared_call_prefix, &asm);
        assert!(got.contains("    STY $0105\n    JSR"), "{got}");
        let asm = ph326_program("    RTS\n").replacen("    STY $06\n", "M:\n    STY $06\n", 1);
        let (changed, got) = drive(ph326_absorb_shared_call_prefix, &asm);
        assert!(changed, "{got}");
        assert!(got.contains("__PH326_0:\n    STY $06\n__H:\n"), "{got}");
        assert!(got.contains("    STA $05\nM:\n    JSR __PH326_0\n"), "{got}");
        let asm = ph326_program("    RTS\n").replace("    STY $06\n", "    STY $01\n");
        let (_, got) = drive(ph326_absorb_shared_call_prefix, &asm);
        assert!(got.contains("    STY $01\n    JSR"), "{got}");
        let asm = ph326_program("    RTS\n").replace("    STA $05\n", "P:\n    STA $05\n") + "    STA P+1\n";
        let (_, got) = drive(ph326_absorb_shared_call_prefix, &asm);
        assert!(!got.contains("__PH326_0:\n    STA $05\n    STY"), "{got}");
    }

    #[test]
    fn ph326_extends_its_entry_over_rounds_and_takes_tail_jumps() {
        let asm = concat!(
            "*=$C000\n",
            "    LDA #$07\n    STA $05\n    STY $06\n    INC $39\n    JSR __H\n",
            "    LDA #$07\n    STA $05\n    STY $06\n    INC $39\n    JMP __H\n",
            "    RTS\n",
            "__H:\n    RTS\n",
        );
        let (changed, got) = drive(ph326_absorb_shared_call_prefix, asm);
        assert!(changed, "{got}");
        assert!(got.contains("    JSR __PH326_"), "{got}");
        assert!(got.contains("    JMP __PH326_"), "{got}");
        assert_eq!(got.matches("    LDA #$07\n").count(), 1, "{got}");
        assert_eq!(got.matches("    INC $39\n").count(), 1, "{got}");
        assert!(
            got.contains(
                "__PH326_1:\n    LDA #$07\n__PH326_0:\n    STA $05\n    STY $06\n    INC $39\n__H:\n"
            ),
            "the second round's entry falls into the first's, in program order:\n{got}"
        );
    }

    /// Two IF-style pairs jumping to a far `L100`, then 200 bytes of data.
    fn ph325_program(between: &str) -> String {
        format!(
            concat!(
                "*=$C000\n",
                "    LDA V_A\n",
                "    BNE S1\n",
                "    JMP L100\n",
                "S1:\n",
                "{}",
                "    LDA V_B\n",
                "    BEQ S2\n",
                "    JMP L100\n",
                "S2:\n",
                "    RTS\n",
                "{}",
                "L100:\n",
                "    RTS\n",
            ),
            between,
            "    .byte $00,$00,$00,$00,$00,$00,$00,$00,$00,$00\n".repeat(20),
        )
    }

    #[test]
    fn ph325_aims_a_branch_at_a_nearby_jmp_to_the_same_place() {
        let mut items = parse(&ph325_program(""));
        assert!(ph325_branch_via_shared_jmp(&mut items));
        let got = render(&items);
        // The first pair branches to the second's JMP, which stays.
        assert!(got.contains("    LDA V_A\n    BEQ __PH325_0\nS1:\n"), "{got}");
        assert!(got.contains("__PH325_0:\n    JMP L100\nS2:\n"), "{got}");
        assert_eq!(got.matches("JMP L100").count(), 1, "{got}");
    }

    #[test]
    fn ph325_aims_at_the_target_itself_when_it_is_close() {
        let input = concat!(
            "*=$C000\n",
            "    LDA V_A\n",
            "    BCC S1\n",
            "    JMP L1\n",
            "S1:\n",
            "    RTS\n",
            "L1:\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        assert!(ph325_branch_via_shared_jmp(&mut items));
        assert!(render(&items).contains("    BCS L1\nS1:\n"));
    }

    #[test]
    fn ph325_refuses_an_anchor_past_a_directive_of_unknown_size() {
        let mut items = parse(&ph325_program("    .text \"ABC\"\n"));
        assert!(!ph325_branch_via_shared_jmp(&mut items));
    }

    #[test]
    fn ph325_refuses_a_jmp_whose_operand_is_patched() {
        let src = ph325_program("").replace(
            "    LDA V_B\n    BEQ S2\n    JMP L100\n",
            "    LDA V_B\n    BEQ S2\n__P:\n    JMP L100\n",
        ) + "    LDA #$00\n    STA __P+1\n";
        let mut items = parse(&src);
        assert!(!ph325_branch_via_shared_jmp(&mut items));
    }


    #[test]
    fn ph322_folds_the_store_window() {
        let input = format!(
            concat!(
                "*=$C000\n",
                "    LDA #<(__ARR_B_I+92)\n",
                "    STA $FD\n",
                "    LDA #>(__ARR_B_I+92)\n",
                "    STA $FE\n",
                "    LDY #$00\n",
                "    LDA #$C8\n",
                "    STA ($FD),Y\n",
                "    INY\n",
                "    LDA #$00\n",
                "    STA ($FD),Y\n",
                "    LDA #$28\n",
                "    STA $39\n",
                "    LDY #$05\n",
                "    RTS\n",
            )
        );
        let mut items = parse(&input);
        assert!(ph322_const_deref_to_abs(&mut items));
        let got = render(&items);
        assert!(
            got.contains("    LDA #$C8\n    STA __ARR_B_I+92\n    LDA #$00\n    STA __ARR_B_I+93\n"),
            "both halves went absolute at their offsets:\n{got}"
        );
        assert!(!got.contains("($FD),Y"), "indirect derefs gone:\n{got}");
        assert!(!got.contains("#<"), "materialization gone:\n{got}");
        assert!(!got.contains("INY"), "offset bumps gone:\n{got}");
    }

    #[test]
    fn ph322_folds_the_load_window() {
        let input = format!(
            concat!(
                "*=$C000\n",
                "    LDA #<(__ARR_B_I+100)\n",
                "    STA $FD\n",
                "    LDA #>(__ARR_B_I+100)\n",
                "    STA $FE\n",
                "    LDY #$00\n",
                "    LDA ($FD),Y\n",
                "    STA __ARRAY_INT_TMP\n",
                "    INY\n",
                "    LDA ($FD),Y\n",
                "    LDY __ARRAY_INT_TMP\n",
                "    JSR $B391\n",
                "    RTS\n",
            )
        );
        let mut items = parse(&input);
        assert!(ph322_const_deref_to_abs(&mut items));
        let got = render(&items);
        assert!(
            got.contains("    LDA __ARR_B_I+100\n") && got.contains("    LDA __ARR_B_I+101\n"),
            "loads went absolute:\n{got}"
        );
        assert_eq!(got.matches("INY").count(), 0, "bumps gone:\n{got}");
    }

    #[test]
    fn ph322_refuses_a_label_inside_the_window() {
        let input = format!(
            concat!(
                "*=$C000\n",
                "    LDA #<(__ARR_B_I+4)\n",
                "    STA $FD\n",
                "    LDA #>(__ARR_B_I+4)\n",
                "    STA $FE\n",
                "    LDY #$00\n",
                "MID:\n",
                "    LDA ($FD),Y\n",
                "    STA $06\n",
                "    JMP MID\n",
            )
        );
        let mut items = parse(&input);
        assert!(!ph322_const_deref_to_abs(&mut items));
        let got = render(&items);
        assert!(got.contains("($FD),Y"), "window kept:\n{got}");
    }

    #[test]
    fn ph322_refuses_when_y_is_live_after() {
        let input = format!(
            concat!(
                "*=$C000\n",
                "    LDA #<(__ARR_B_I+4)\n",
                "    STA $FD\n",
                "    LDA #>(__ARR_B_I+4)\n",
                "    STA $FE\n",
                "    LDY #$00\n",
                "    LDA ($FD),Y\n",
                "    INY\n",
                "    TYA\n",
                "    RTS\n",
            )
        );
        let mut items = parse(&input);
        assert!(!ph322_const_deref_to_abs(&mut items));
        let got = render(&items);
        assert!(got.contains("($FD),Y"), "window kept:\n{got}");
    }

    #[test]
    fn ph322_refuses_a_pointer_cell_write_in_the_body() {
        let input = format!(
            concat!(
                "*=$C000\n",
                "    LDA #<(__ARR_B_I+4)\n",
                "    STA $FD\n",
                "    LDA #>(__ARR_B_I+4)\n",
                "    STA $FE\n",
                "    LDY #$00\n",
                "    LDA #$77\n",
                "    STA $FE\n",
                "    LDA ($FD),Y\n",
                "    STA $06\n",
                "    RTS\n",
            )
        );
        let mut items = parse(&input);
        assert!(!ph322_const_deref_to_abs(&mut items));
        let got = render(&items);
        assert!(got.contains("($FD),Y"), "window kept:\n{got}");
    }

    #[test]
    fn ph317_drops_imm_load_across_flag_only_op() {
        let input = concat!(
            "    LDA #$32\n",
            "    CLC\n",
            "    LDA VI_I\n",
            "    ADC #$01\n",
            "    STA VI_I\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        assert!(ph317_drop_dead_load_via_flag_liveness(&mut items));
        let got = render(&items);
        assert!(!got.contains("#$32"), "dead immediate dropped:\n{got}");
        assert!(got.contains("CLC"), "CLC stays because ADC reads carry.\n{got}");
        assert!(got.contains("LDA VI_I"), "redefining load kept:\n{got}");
    }

    #[test]
    fn ph317_keeps_imm_load_when_later_branch_reads_nz() {
        let input = concat!(
            "    LDA #$32\n",
            "    BNE SKIP\n",
            "SKIP:\n",
            "    LDA VI_I\n",
            "    ADC #$01\n",
            "    STA VI_I\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        assert!(!ph317_drop_dead_load_via_flag_liveness(&mut items));
        let got = render(&items);
        assert!(got.contains("LDA #$32"), "flag-feeding load kept:\n{got}");
    }

    #[test]
    fn ph317_keeps_imm_load_when_a_live_out() {
        let input = concat!(
            "    LDA #$32\n",
            "    CLC\n",
            "    STA DST\n",
            "    LDA VI_I\n",
            "    ADC #$01\n",
            "    STA OUT\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        assert!(!ph317_drop_dead_load_via_flag_liveness(&mut items));
        let got = render(&items);
        assert!(got.contains("LDA #$32"), "load with live .A kept:\n{got}");
    }

    #[test]
    fn ph317_keeps_imm_load_when_flags_cross_rts() {
        let input = concat!(
            "__H_RET:\n",
            "    LDA #$32\n",
            "    RTS\n",
            "    JSR __H_RET\n",
            "    BNE LATER\n",
            "LATER:\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        assert!(!ph317_drop_dead_load_via_flag_liveness(&mut items));
        let got = render(&items);
        assert!(got.contains("LDA #$32"), "load before RTS kept:\n{got}");
    }

    #[test]
    fn ph317_drops_the_loop_head_stamp() {
        let input = concat!(
            "    LDA #$01\n",
            "    STA VI_I\n",
            "    TAX\n",
            "TOP:\n",
            "    LDA #$28\n",
            "    CPX #$64\n",
            "    BEQ EXIT\n",
            "    INX\n",
            "    JMP TOP\n",
            "EXIT:\n",
            "    LDA #$00\n",
            "    STA OUT\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        assert!(ph317_drop_dead_load_via_flag_liveness(&mut items));
        let got = render(&items);
        assert!(!got.contains("#$28"), "loop-head stamp dropped:\n{got}");
        assert!(got.contains("LDA #$01"), "stored init kept:\n{got}");
        assert!(got.contains("CPX #$64"), "loop test kept:\n{got}");
    }

    #[test]
    fn ph317_refuses_a_runtime_patched_imm_load() {
        let input = concat!(
            "PATCH:\n",
            "    LDA #$32\n",
            "    CLC\n",
            "    LDA VI_I\n",
            "    ADC #$01\n",
            "    STA VI_I\n",
            "    RTS\n",
            "    STA PATCH+1\n",
        );
        let mut items = parse(input);
        assert!(!ph317_drop_dead_load_via_flag_liveness(&mut items));
        let got = render(&items);
        assert!(got.contains("LDA #$32"), "patched load kept:\n{got}");
    }

    #[test]
    fn ph317_refuses_when_php_exists_anywhere() {
        let input = concat!(
            "    LDA #$32\n",
            "    CLC\n",
            "    PHP\n",
            "    PLP\n",
            "    LDA VI_I\n",
            "    ADC #$01\n",
            "    STA VI_I\n",
            "    RTS\n",
        );
        let mut items = parse(input);
        assert!(!ph317_drop_dead_load_via_flag_liveness(&mut items));
        let got = render(&items);
        assert!(got.contains("LDA #$32"), "load kept beside PHP:\n{got}");
    }
}
