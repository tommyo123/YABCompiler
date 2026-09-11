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
        }
    }
}

impl StaticAsmFacts {
    fn reset(&mut self) {
        let zp_equ = std::rc::Rc::clone(&self.zp_equ);
        *self = Self::default();
        self.zp_equ = zp_equ;
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

    fn invalidate_mem_origin(&mut self, mem: &str) {
        for fact in &mut self.regs {
            if fact.mem.as_deref() == Some(mem) {
                fact.mem = None;
            }
        }
        self.mem_imm.remove(mem);
    }

    fn record_mem_imm(&mut self, mem: &str, value: u8) {
        self.mem_imm.insert(mem.to_string(), value);
    }

    fn known_mem_imm(&self, mem: &str) -> Option<u8> {
        self.mem_imm.get(mem).copied()
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
    let (base, off) = match key.split_once('+') {
        Some((b, o)) => match o.trim().parse::<u16>() {
            Ok(off) => (b.trim(), off),
            Err(_) => return false,
        },
        None => (key, 0),
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
    matches!(val, Some(v) if v.saturating_add(off) < 0x0100)
}

fn analyze_static_asm_facts(items: &[Item]) -> Vec<StaticAsmFacts> {
    let mut current = StaticAsmFacts::default();
    current.zp_equ = std::rc::Rc::new(
        collect_equ_values(items)
            .into_iter()
            .filter(|(_, v)| *v < 0x0100)
            .collect(),
    );
    let mut facts = Vec::with_capacity(items.len());
    for item in items {
        facts.push(current.clone());
        current.update_item(item);
    }
    facts
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

/// PH015 — drop dead register transfer.
///
/// Pattern: `TAX|TAY|TXA|TYA` where the destination register is dead
/// at the live-out of the transfer. Rare in freshly-emitted code,
/// but later peephole passes (PH008/PH009/dead-store) can leave a
/// transfer whose only consumer is gone.
///
/// Caveat: register transfers also set the N/Z flags on the value.
/// We don't track flags, so we conservatively refuse if the next
/// structural instruction is a conditional branch — the branch
/// could be relying on the flags the transfer set.
fn ph015_drop_dead_transfer(items: &mut Vec<Item>) -> bool {
    let config = liveness_config_with_helpers();
    let live = analyze_register_liveness_with_config(items, &config);
    let smc = smc_patched_labels(items);
    let mut to_drop = Vec::new();
    for (idx, it) in items.iter().enumerate() {
        let Item::Insn(insn) = it else { continue };
        let dest = match insn.mnem.as_str() {
            "TAX" => Reg::X,
            "TAY" => Reg::Y,
            "TXA" | "TYA" => Reg::A,
            _ => continue,
        };
        if live.is_live_out(idx, dest) {
            continue;
        }

        if insn_is_smc_target(items, idx, &smc) {
            continue;
        }
        // Flag-safety: the next structural item must not be a
        // conditional branch reading the flags this transfer sets.
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
                // Search forward for the next label / non-blank-non-comment.
                // If the very next structural item is a label matching
                // our operand, drop the branch.
                if let Some(next) = next_struct(items, i)
                    && let Item::Label(name) = &items[next]
                    && name == target
                {
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

/// Share matching instruction tails across straight-line blocks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CurlinHi {
    /// No path reaches here yet ; the identity for `meet`.
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

fn ph306_classify(
    items: &[Item],
    labels: &std::collections::HashMap<String, usize>,
    zp_equ: &std::collections::HashMap<String, u16>,
    cell: u16,
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
                    "JMP" if op.trim_start().starts_with('(') => HiEffect::Clobber,
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
    let effects = ph306_classify(items, &labels, &zp_equ, cell);
    if !effects.iter().any(|e| matches!(e, HiEffect::Stamp(_))) {
        return false;
    }

    let n = items.len();
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut succ_buf = Vec::new();
    for i in 0..n {
        ph306_succs(items, i, &labels, &effects, &mut succ_buf);
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

    let mut external = vec![false; n];
    external[0] = true;
    for (i, it) in items.iter().enumerate() {
        let text = match it {
            Item::Insn(insn) => {
                let op = insn.operand.as_deref().unwrap_or("");
                let bare_control = (insn.mnem == "JMP" || is_cond_branch(&insn.mnem))
                    && labels.contains_key(op.trim());
                if bare_control {
                    continue;
                }
                if insn.mnem == "JSR"
                    && let Some(&t) = labels.get(op.trim())
                {
                    external[t] = true;
                    continue;
                }
                op.to_string()
            }
            Item::Directive(s) => s.clone(),
            _ => continue,
        };
        let _ = i;
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
                HiEffect::Call(t) => match t {
                    Some(t) if !touches[t] => state[i],
                    Some(_) => CurlinHi::Unknown,
                    None => CurlinHi::Unknown,
                },
                HiEffect::Transparent => state[i],
            };
            if after == CurlinHi::Unreached {
                continue;
            }
            ph306_succs(items, i, &labels, &effects, &mut succ_buf);
            for &s in &succ_buf {

                let merged = state[s].meet(after);
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
    let facts = analyze_static_asm_facts(items);
    let flag_config = FlagLivenessConfig::default();
    let flag_live = analyze_flag_liveness_with_config(items, &flag_config);

    let inc_dec_cells: std::collections::HashSet<&str> = items
        .iter()
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

                if reg_fact_matches_load(fact.reg(dest), insn)
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

fn reg_fact_matches_load(fact: &StaticRegFact, insn: &Insn) -> bool {
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
        return fact.mem.as_deref() == Some(mem.as_str());
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
            if reg_fact_matches_load(facts.reg(Reg::X), insn) {
                Some("TXA")
            } else if reg_fact_matches_load(facts.reg(Reg::Y), insn) {
                Some("TYA")
            } else {
                None
            }
        }
        Reg::X => reg_fact_matches_load(facts.reg(Reg::A), insn).then_some("TAX"),
        Reg::Y => reg_fact_matches_load(facts.reg(Reg::A), insn).then_some("TAY"),
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
    // Use the default conservative config: `return_live_out: ALL`.
    // We do have helpers that return meaningful flags (e.g.
    // `__DIV_BY_10` exits with Z reflecting the remainder so a
    // PRINT-comma caller can `BEQ` on "at zone boundary"). An
    // EMPTY-return config would let PH024 drop the trailing
    // `CMP #$00` we deliberately emit there, silently breaking
    // the caller's branch — caught by an infinite-loop hang in
    // `print "x", t`. We forfeit the tail-CMP optimisation in
    // exchange for correctness across the helper boundary.
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

/// PH022 — replace `JMP <label>` with `RTS` when the label's body
/// is a bare `RTS`.
///
/// Saves 2 bytes (3-byte JMP → 1-byte RTS) plus the wasted cycles for the JMP
/// and the redundant pipeline fetch at the destination. Common in
/// codegen output where multiple early-exit paths funnel through a
/// shared `JMP rts_helper`.
///
/// Rewrite is conservative:
///   * we only act on plain `JMP`, never on conditional branches
///   * the destination must be a known label whose first non-blank
///     non-comment item is exactly `RTS`
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

/// PH021 — constant-fold conditional branches.
///
/// When the same forward static analysis as PH020 has nailed down
/// a flag's value (Z, N, or C), a conditional branch becomes
/// statically resolvable:
///
/// * branch always taken    → rewrite to `JMP target`
/// * branch never taken     → drop the instruction entirely
///
/// The bias is conservative — flags must be Some(known) at the
/// point of the branch, and the operand must be a recognisable
/// label (not a relative offset, not a numeric address). We don't
/// touch BVS/BVC because we don't track V.
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
        // Bail when the operand isn't a clean label — relative
        // numeric branches and missing operands aren't safe to
        // rewrite without more context.
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

/// PH012 — drop redundant `CMP #$00` after a flag-setting op.
///
/// Pattern:
/// ```text
///     LDA value
///     CMP #$00
///     BEQ zero
/// ```
///
/// `LDA` already sets `Z` and `N` based on the loaded byte, so the
/// `CMP #$00` is pure overhead. Same goes for any instruction whose
/// "Z based on A/X/Y" semantics matches `CMP #$00`.
///
/// Caveat: `CMP` also clears `C` (sets it for ≥, clears for <). If
/// any subsequent instruction reads `C` before resetting it, we'd
/// break that. Conservatively bail when the branch immediately
/// after `CMP #$00` is `BCC/BCS` — those read `C`. All others
/// (BEQ/BNE/BMI/BPL/BVC/BVS) don't, and they're the common case.
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
            // Check what follows the CMP. If any of the
            // immediately-following branches read the carry
            // flag (BCC / BCS), bail — those bits come from CMP,
            // not LDA. A single-step look-ahead isn't enough: codegen
            // patterns like `IF X > 0` emit
            //   LDA X / CMP #$00 / BEQ skip / BCC skip
            // where the BCC sits TWO branches after the CMP. The
            // BEQ in between only reads Z (which LDA does set),
            // but dropping the CMP would still corrupt the BCC's
            // carry. Scan forward through consecutive branch
            // instructions and bail if any of them is C-based.
            let mut scan = next_struct(items, j);
            let mut reads_c = false;
            while let Some(k) = scan {
                let Item::Insn(insn) = &items[k] else { break };
                match insn.mnem.as_str() {
                    "BCC" | "BCS" => {
                        reads_c = true;
                        break;
                    }
                    "BEQ" | "BNE" | "BMI" | "BPL" | "BVC" | "BVS" => {
                        // Pure Z/N/V branch — keep looking; the
                        // CMP's carry might still feed a later
                        // BCC/BCS in the same IF lowering.
                        scan = next_struct(items, k);
                    }
                    _ => break,
                }
            }
            if reads_c {
                i += 1;
                continue;
            }
            items.remove(j);
            changed = true;
            // Don't advance — the same prev might collide with the
            // next CMP after re-shuffling.
            continue;
        }
        i += 1;
    }
    changed
}

/// Helpers / instructions that already leave the `Z` flag set
/// equivalently to a `CMP #$00` against `A`. Conservatively gated —
/// the peephole drops a `CMP #$00`, so the prior instruction's Z
/// flag must reflect `A`, not some other register or memory byte:
///   * `LDA` (immediate or memory) sets Z based on the loaded byte
///     in A.
///   * A-arithmetic / A-logic ops (AND, ORA, EOR, ADC, SBC) set Z
///     based on the result in A. ADC/SBC also touch C/V — but PH012
///     only drops the CMP, not the following branch, and C/V are
///     the post-arith values which the coder-supplied branch
///     presumably wants anyway.
///   * `TXA` / `TYA` / `PLA` write A and set Z accordingly.
///   * `TAX` / `TAY` set Z based on the byte transferred — which
///     equals A — so the post-instruction Z still answers
///     "A == 0?" correctly.
///   * Implicit-mode `ASL` / `LSR` / `ROL` / `ROR` (no memory
///     operand) shift A and set Z on the result. With a memory
///     operand they instead set Z on the modified byte, so guard
///     on `insn.operand.is_none()`.
///   * Calls into compiler-owned helpers that document Z-preserving
///     return contracts. Right now the only one is `__STRCMP`.
///
/// Excluded — these set Z based on X/Y or memory, NOT A. Lumping
/// them in here would let the pass delete `CMP #$00` against A in
/// sequences like `LDX #$01 / CMP #$00 / BNE ...`, breaking control
/// flow in `__GET_STR`.
///   * `LDX` / `LDY` — Z reflects X/Y.
///   * `INX` / `INY` / `DEX` / `DEY` — Z reflects X/Y.
///   * `INC` / `DEC` — Z reflects the modified memory byte.
fn sets_z_flag_for_cmp_zero(insn: &Insn) -> bool {
    match insn.mnem.as_str() {
        "LDA" | "TAX" | "TAY" | "TXA" | "TYA" | "AND" | "ORA" | "EOR" | "ADC" | "SBC" | "PLA" => {
            true
        }
        // Shift/rotate set Z on A only in implicit-accumulator mode.
        // With an explicit memory operand, Z reflects the memory
        // byte, which doesn't equal A.
        "ASL" | "LSR" | "ROL" | "ROR" => insn.operand.is_none(),
        "JSR" => insn.operand.as_deref() == Some("__STRCMP"),
        _ => false,
    }
}

/// Extract every label-like token from an instruction operand.
///
/// Matches both bare label refs (`JMP __HELPER`, `STA __PTR_LO`)
/// and high/low-byte immediate forms (`LDA #<__VARS_START`,
/// `LDY #>__HEAP_END`). Labels are anything that starts with an
/// ASCII letter or `_` and uses identifier-friendly characters.
fn extract_labels_from_operand(op: &str) -> Vec<String> {
    let mut out = Vec::new();
    // Walk the string collecting maximal identifier runs. Skip any
    // run that's preceded by a `$` (hex literal) or starts inside
    // a `#$` immediate.
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
            // Was this preceded by a `$`? If so it was a hex literal
            // that just happened to contain a letter (e.g. `$BBA2`).
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

/// Labels named on a directive or origin line.
///
/// Strips the comment and the leading `.mnemonic` first, then scans the rest.
fn directive_operand_labels(raw: &str) -> Vec<String> {
    let body = raw.split(';').next().unwrap_or(raw).trim();
    let operands = match body.strip_prefix('.') {
        Some(rest) => rest.split_once(char::is_whitespace).map_or("", |(_, o)| o),
        None => body,
    };
    extract_labels_from_operand(operands)
}

/// PH106 — factor `LDA #$00 ; LDY #imm ; JSR $B391` into a helper.
///
/// This is the dominant int-byte-to-FAC pattern: integer constants
/// in [0,255] go through GIVAYF with `A=$00, Y=byte`. Each callsite
/// is 7 bytes (2+2+3); routing through a `__LD_BYTE_FAC` stub turns
/// each callsite into 5 bytes (`LDY #imm; JSR __LD_BYTE_FAC`) at
/// the cost of one 5-byte helper (`LDA #$00; JMP $B391`).
///
/// Net: −2 bytes per call, +5 bytes for the helper. Break-even at
/// 3 calls; we conservatively gate at 4+ to stay net-positive even
/// after rounding.
///
/// Runtime cost: ~3 extra cycles per call (the JSR+JMP detour vs
/// inline LDA+LDY+JSR). Within the Default profile's size bias.
fn ph106_factor_int_byte_to_fac(items: &mut Vec<Item>) -> bool {

    if items.iter().any(|item| matches!(item, Item::Label(name) if name == "__LD_BYTE_FAC")) {
        return false;
    }
    // First pass: locate all matching triples.
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
    // Second pass: rewrite. Walk in reverse so removing triples
    // doesn't invalidate later indices.
    for &start in sites.iter().rev() {
        let j = next_struct(items, start).expect("paired LDY");
        let k = next_struct(items, j).expect("paired JSR");
        // Replace JSR $B391 → JSR __LD_BYTE_FAC.
        if let Item::Insn(insn) = &mut items[k] {
            insn.operand = Some("__LD_BYTE_FAC".to_string());
        }
        // Drop LDA #$00 (start). LDY remains in place. The JSR has
        // been retargeted at index k.
        items.remove(start);
    }
    // Inject the helper as labelled instructions BEFORE the runtime
    // string heap starts (`__HEAP_BOTTOM:`). If we appended after the
    // heap label, the heap allocator would overwrite our helper code
    // at runtime as soon as a few strings get allocated.
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

/// PH109 — per-imm specialisation of `__LD_BYTE_FAC`. Runs AFTER
/// PH106 has already shrunk `LDA #$00 / LDY #imm / JSR $B391` into
/// `LDY #imm / JSR __LD_BYTE_FAC`. Frequently repeated immediates
/// get their own helper.
/// Adding a per-imm stub turns each callsite into a single
/// `JSR __LDV_<imm>_FAC` (3 bytes) at the cost of a 7-byte helper
/// per emitted imm.
///
/// Net per call: 5 bytes → 3 bytes. Helper: 7 bytes once.
/// Break-even: 4 calls per imm.
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

    // Rewrite phase: stage replacements before mutating.
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

    // Emit per-imm helpers BEFORE __HEAP_BOTTOM (same constraint as
    // PH106/PH107).
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

/// PH107 — factor `LDA #imm ; JSR <rom>` pairs into per-(imm, rom)
/// helpers when the same pair appears often enough.
///
/// Dominant case: `LDA #$0D ; JSR $FFD2` (CHROUT carriage-return)
/// at repeated PRINT line endings.
///
/// Each callsite drops from 5 bytes (LDA imm + JSR addr) to 3 bytes
/// (JSR helper). The helper is 5 bytes (LDA imm + JMP addr — fall
/// through saves the RTS). Break-even at 3 callsites; we gate at
/// **4+** so the win is at least 3 bytes per fired pattern.
///
/// Currently restricted to `JSR $FFD2` (KERNAL CHROUT). Generalising
/// to other ROM addresses would help future patterns, but CHROUT is
/// where the volume is — extending later when measurement says so.
///
/// Safety: the JSR target must be a known ROM routine that returns
/// with `RTS`. CHROUT is documented to do so. We bail on any callsite
/// whose preceding instruction isn't a *bare* `LDA #imm` immediate
/// (no further expressions).
fn ph107_factor_chrout_byte(items: &mut Vec<Item>) -> bool {
    use std::collections::HashMap;
    // Pre-scan: collect all (i, imm_byte) pairs where items[i] is
    // `LDA #$<imm>` and the next struct is `JSR $FFD2`.
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
        return false;
    }
    // Sort descending so removals don't disturb earlier indices we
    // still need.
    to_factor.sort_by_key(|(imm, _)| *imm);

    // Phase 1: collect all removal indices across all imm-groups, in
    // descending order, and rewrite each JSR site in-place to the
    // matching helper. We do the rewrites first (still valid because
    // we haven't removed anything), then drop LDAs in reverse.
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

    // Phase 2: emit one helper per imm-value. Routed through
    // `append_helpers_before_heap` so we don't drop them into the
    // runtime string heap region (which would clobber them as soon
    // as the allocator started writing).
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
    true
}

/// Parse `#$XX` immediate operand. Returns the byte value, or `None`
/// for non-byte-immediate operands (like labels, `#<...`, or 16-bit
/// hex literals).
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

/// PH113 — factor adjacent `JSR A; JSR B; JSR C` triples that recur.
/// Same shape as PH110 but a step longer. Each callsite drops from
/// 9 bytes to 3 bytes; helper is 9 bytes (`JSR A; JSR B; JMP C`).
/// Save 6 bytes per call. Break-even at 2 calls; we gate at 4+.
fn ph113_factor_jsr_triple(items: &mut Vec<Item>) -> bool {
    use std::collections::HashMap;
    // `A` and `B` stay `JSR`s inside the stub and so gain a frame; only `C`, which becomes the tail `JMP`, is frame-transparent.
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
            i = j + 1;
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

    // Allocate helper IDs starting at 0 (no collision with PH301's
    // __SEQ_ namespace).
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
        // Replace the first JSR's operand with the helper.
        if let Item::Insn(insn) = &mut items[*start] {
            insn.operand = Some(helper_label);
        }
        // Drop the second JSR.
        items.remove(*second);
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

/// PH112 — factor `LDA <var> / LDY <var>+1 / JSR T` triples that
/// recur. This is the dominant "load string-descriptor pointer for
/// var, then call helper" idiom (`__STR_PRINT`, string concat, etc.).
///
/// Pattern requires the LDA's operand to be a stable label `X` and
/// the LDY's operand to be exactly `X+1` — the two halves of the
/// var slot. We also accept indirect zero-page forms (`$03`/`$03+1`
/// etc.) since those follow the same shape.
///
/// Per call: 9 bytes (LDA + LDY + JSR, all absolute) → 3 bytes.
/// Helper: 10 bytes (LDA + LDY + JMP). Save 6 bytes per call.
/// Break-even at 2 calls; we gate at 4 for margin.
fn ph112_factor_varptr_jsr(items: &mut Vec<Item>) -> bool {
    use std::collections::HashMap;
    let mut sites: HashMap<(String, String), Vec<usize>> = HashMap::new();
    let mut i = 0;
    while i + 2 < items.len() {
        if let Item::Insn(a) = &items[i]
            && a.mnem == "LDA"
            && let Some(a_op) = a.operand.as_deref()
            // Reject immediate operands — those go through PH111.
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
    // Threshold = 3 (same math as PH111): each site saves 4 bytes
    // (7-byte triple → 3-byte JSR), helper costs 7 bytes, so 3
    // sites is the smallest net-positive group.
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

/// PH111 — factor `LDA #<X / LDY #>X / JSR T` triples that recur.
///
/// Setup-and-call to ROM/helper subroutines is the dominant 7-byte
/// shape in our generated asm. PH301 doesn't catch these because
/// it refuses windows that contain a JSR. PH110 doesn't catch them
/// because they're 3 instructions, not 2.
///
/// Per call: 7 bytes (LDA imm + LDY imm + JSR) → 3 bytes (JSR
/// helper). Helper: 7 bytes once (LDA imm + LDY imm + JMP target).
/// Save 4 bytes per call; break-even at 2 calls. We gate at 4+ for
/// margin and to avoid emitting too many tiny helpers.
///
/// Useful for repeated FAC setup-and-call shapes.
fn ph111_factor_lda_ldy_jsr(items: &mut Vec<Item>) -> bool {
    use std::collections::HashMap;
    // Sites grouped by (mnem_a, lda_operand, ldy_operand, jsr_target).
    // The first mnem can be `LDA` (for FAC float ops) or `LDX` (for
    // MOVMF-style calls that take pointer in X/Y).
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
            // Don't factor a triple that already calls one of OUR
            // own __T<n> helpers (would create endless wrappers).
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
    // Threshold = 3: each site saves 4 bytes (7-byte triple → 3-byte
    // JSR), helper itself is 7 bytes. At 3 sites: 12 saved - 7 helper
    // = 5 bytes net. Threshold 4 was the original (9 bytes net),
    // dropping to 3 catches the long tail of medium-frequency triples
    // — roughly 1-2% smaller on real programs.
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
        // Replace LDA/LDX at `start` with JSR helper. Drop LDY (mid)
        // and JSR (end).
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
    // Descending, so the rewrite of one site cannot move the indices of a site that has not
    // been rewritten yet.
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

/// Restore register-based PRINT arguments before reserved-range relocation.
pub fn undo_str_print_inline_args(asm: &str) -> String {
    if !asm.contains(STR_PRINT_INLINE_ENTRY) {
        return asm.to_string();
    }
    let items = parse(asm);

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
    let mut i = 0usize;
    while i < items.len() {
        if Some(i) == entry {
            i = tramp_end.expect("checked above");
            continue;
        }
        if let Item::Insn(insn) = &items[i]
            && insn.mnem == "JSR"
            && insn.operand.as_deref() == Some(STR_PRINT_INLINE_ENTRY)
            && let Some(k) = next_struct(&items, i)
            && let Item::Directive(d) = &items[k]
            && let Some(label) = d.trim().strip_prefix(".word ")
            && is_plain_label(label.trim())
        {
            let label = label.trim();
            push_insn(&mut out, "LDA", Some(format!("#<{label}")));
            push_insn(&mut out, "LDY", Some(format!("#>{label}")));
            push_insn(&mut out, "JSR", Some("__STR_PRINT".to_string()));
            // Anything decorative between the two (a blank line or a comment) is dropped with the pair it belonged to.
            i = k + 1;
            continue;
        }
        out.push(items[i].clone());
        i += 1;
    }
    render(&out)
}

/// PH301 — general sequence factoring (size-mode-leaning).
///
/// Find any 4-instruction window with byte-identical operands that
/// repeats ≥ 4 times in the program, and factor into a shared helper:
///
/// ```text
/// __SEQ_<n>:
///     <inst-1>
///     <inst-2>
///     <inst-3>
///     <inst-4>
///     RTS
/// ```
///
/// Each callsite drops from `4*N + per-instruction-cost` bytes down
/// to 3 bytes. The helper is `<sequence-bytes> + 1` bytes (RTS = 1).
/// Break-even depends on the original sequence length, but for a
/// 4-instruction window of ~10-12 bytes it kicks in around 4
/// occurrences.
///
/// Useful for repeated array-index prologues.
///
/// Safety constraints:
///   * Window must not cross a label (a basic-block boundary). We
///     scan only within straight-line stretches.
///   * Window must not contain branches/JMP/JSR (a JSR itself is
///     fine, but factoring sequences containing JSRs invites
///     stack-balance pitfalls in fixpoint iterations — defer).
///   * Window operands must be byte-identical strings between
///     occurrences. We use the rendered text as the hash key.
///   * Window must not contain RTS/RTI/BRK (terminates control —
///     the factored helper would still RTS, so semantics OK, but
///     PH002 is a better tool there).
///   * We refuse to factor windows whose operands contain *any*
///     label reference. Labels can be relocated by other passes;
///     a peephole-introduced helper that captures `LDA #<L1` would
///     break if L1 later moves. Pure hex / register operands only.
///
/// Cycle cost: each fired call adds ~12 cycles per execution
/// (JSR + RTS overhead vs inline). The early pass is currently
/// gated off; the late pass below (`ph301_factor_late_with_jsr`)
/// is the only live caller path.
#[allow(dead_code)]
fn ph301_factor_repeated_sequences(items: &mut Vec<Item>) -> bool {
    // Try windows of decreasing size. Larger windows are tried first
    // so they claim sites before smaller ones (a 7-instr helper
    // saves more per call than a 3-instr one). Each call is
    // independent and uses disjoint scan internally.
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
    /// Original PH301 behaviour — refuse any window containing a JSR.
    /// Lets PH110/PH113 handle JSR sequences with their dedicated
    /// pair/triple shapes.
    Refused,
    /// Allow declared integer and string storage during compact instruction factoring.
    CompactRefused,
    /// Compact instruction windows containing exactly one call to a defined, inspected straight-line leaf ending in RTS.
    CompactLeafCalls,
    /// Allow at most ONE JSR per window. Rules out JSR-only windows
    /// (which PH110/PH113 collapses better) but admits the common
    /// "address compute + helper call" pattern that mixes loads,
    /// stores, and one helper invocation.
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

    // Collect candidate windows: indices where a window_size-long run
    // of safe instructions starts. We index into the *condensed*
    // instruction stream so adjacent non-Insn items (blank lines,
    // comments) don't break a window — we scan over them via
    // next_struct.
    //
    // For each candidate start, render the window's signature
    // (joined `mnem operand`) and group by signature.
    //
    // Disjoint occurrences: once we pick a start `s`, the next legal
    // start is at the position *after* the window (s + 4 insn-steps).
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
            // First step starts at `i` itself if it's an Insn;
            // subsequent steps walk via next_struct.
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
        // Stack balance: the helper RTS pops the top of stack as the
        // return address, so a window with non-zero net stack delta
        // would jump somewhere unintended. Allow balanced PHA/PLA
        // pairs inside the window — only refuse when push count
        // doesn't match pop count.
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
    // Most profitable first; signature breaks ties so helper naming stays deterministic.
    to_factor.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));

    let next_seq_id = peephole_next_seq_id(items);
    let mut helper_idx = next_seq_id;
    let mut emitted: Vec<(String, String)> = Vec::new(); // (sig, helper-label)
// Per-site (start, last). We also remember the SIG for each so
// the rewrite phase can look up the helper label.
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

    // Apply rewrites in reverse order so removals don't disturb
    // earlier indices.
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
        // Drop the remaining 3 instructions. If structure shifted
        // such that we can't find them (shouldn't happen, but
        // defensive), skip the drop.
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

    // Emit helpers BEFORE `__HEAP_BOTTOM:` so the runtime string
    // heap doesn't overwrite them.
    let mut helpers: Vec<Item> = Vec::new();
    if !helper_section_present(items) {
        helpers.push(Item::Blank);
        helpers.push(Item::Comment(
            "; --- peephole-factored helpers ---".to_string(),
        ));
    }
    // Re-walk groups to emit each helper body. We have the signature
    // → label mapping; we parse the signature back into the
    // instruction stream and append an RTS.
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

/// True if `insn` is safe to include in a factored sequence: no
/// control-flow effects, no JSR, and any label operands must be of
/// "compiler-stable" kinds (array-data, var-slot, helper, etc.) —
/// not BASIC line labels that downstream passes might relocate.
///
/// Currently kept only as documentation for the JSR-Refused policy
/// — every active call site picks its policy explicitly via
/// `is_factorable_insn_with_policy`.
#[allow(dead_code)]
fn is_factorable_insn(insn: &Insn) -> bool {
    is_factorable_insn_with_policy(insn, JsrPolicy::Refused)
}

/// True iff the window's stack discipline is safe to wrap in JSR/RTS.
/// PH301 wraps the window in `JSR helper; ...; RTS`, and the helper
/// has no stack frame of its own beyond the return address pushed by
/// the wrapping JSR. Two distinct hazards have to be ruled out:
///
///  1. **Net delta non-zero**: pushes and pops in the window must
///     balance — otherwise the helper's terminating RTS pops the
///     wrong bytes (caught by edge29 mid\$/left\$/right\$).
///  2. **Running delta goes negative**: a PLA before any in-window
///     PHA pops the JSR's own return address (which the wrapping
///     `JSR helper` just pushed) — even if a later PHA balances the
///     count. The helper's RTS at the end then jumps to garbage.
///     Caught by edge167 (`a\$+b\$+c\$+a\$+b\$+c\$` chain factored
///     PLA-...-PHA into `__SEQ_0` and broke at runtime).
///
/// Counts each PHA/PHP as +1, each PLA/PLP as −1.
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
    // Fixpoint over frame-transparent `JMP`s.
    loop {
        let mut grew = false;
        let mut cur: Vec<&str> = Vec::new();
        let mut saw_insn = false;
        for it in items {
            match it {
                Item::Label(name) => {
                    // A label after a body starts a fresh region; stacked labels share one.
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

/// True iff `insn` may appear inside a PH301 factor window. The
/// JSR policy is a soft hint — `Refused` rejects every JSR; the
/// caller-side window scan layers an extra "≤1 JSR per window"
/// rule on top of `AllowOne`.
fn is_factorable_insn_with_policy(insn: &Insn, jsr: JsrPolicy) -> bool {
    // Control flow ops: never factor. JMP/RTS/RTI break the
    // sequential signature; conditional branches refer to local
    // labels that wouldn't translate to a hoisted helper.
    if is_uncond_terminator(&insn.mnem) || is_cond_branch(&insn.mnem) {
        return false;
    }
    if insn.mnem == "JSR" && matches!(jsr, JsrPolicy::Refused | JsrPolicy::CompactRefused) {
        return false;
    }
    // TSX/TXS rewrite SP itself — never factor.
    if matches!(insn.mnem.as_str(), "TSX" | "TXS") {
        return false;
    }
    // PHA/PLA/PHP/PLP allowed at the per-instruction level — the
    // window-level scan below also runs `window_stack_balanced` to
    // refuse factoring any window whose net stack delta isn't zero.
    // (An unbalanced window wrapped in JSR/RTS would let the helper's
    // RTS pop whatever the window pushed instead of the caller's
    // return address — caught by edge29 with mid$/left$/right$ on the
    // same string, where `LDA V_X; LDY V_X+1; PHA; TYA; PHA` pushed
    // a pointer that the caller's later PLA-PLA was supposed to
    // consume; the wrap diverted into the pushed bytes as code.)
    // Operand references: allow stable compiler labels (array data,
    // variable slots, internal helpers), refuse BASIC line labels
    // and unknown identifiers.
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

/// True iff the operand names an absolute address in page one (`$0100`-`$01FF`), in any
/// addressing form.
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

/// True iff the operand references at least one identifier that
/// isn't a "compiler-stable" label (`__*`, `V_*`, `FI_*`, `FU_*`,
/// `FE_*`, `FS_*`, `S<digits>`, `F<digits>`, `T<digits>`).
///
/// Stable labels are emitted once at stable positions in the BSS /
/// constant-pool section by codegen and aren't relocated by later
/// passes. Capturing them in a peephole-introduced helper is safe.
///
/// Unstable labels (e.g. `L<digits>` for BASIC line numbers) are
/// reachable via `GOTO`/`GOSUB` and could theoretically move; we
/// refuse to factor any window that mentions them.
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

/// True iff `token` is a compiler-stable label name — one of the
/// stable-position labels emitted by codegen / earlier peephole
/// passes that won't be relocated. Examples:
///   * `__ARR_<X>`, `__SBUF_<X>`, `__HEAP_*` — BSS data labels
///   * `V_<X>`, `V_<X>I` — variable slots
///   * `FI_<n>`, `FU_<n>`, `FE_<n>`, `FS_SIGN_<n>` — FOR-loop slots
///   * `S<digits>` — string-pool entries
///   * `F<digits>` — float-pool entries
///   * `T<digits>` — int-temp slots
///   * `__SEQ_<n>`, `__JSR2_<n>`, `__LDV_*`, `__CHROUT_*`,
///     `__LD_*`, `__ST_*`, `__FADD_*`, `__FSUB_*`, `__FMULT_*`,
///     `__FDIV_*` — peephole / codegen-emitted helpers
///   * `__BNDS_<n>`, `__INTBY_OK_*`, `__POKE_*`, `__PEEK_*` — local
///     helpers and self-modifying operand labels
///
/// Refused: bare `L<digits>` (BASIC line labels), unknown letter
/// tokens (could be user-named labels via inline assembly, though we
/// don't currently support that).
fn is_stable_compiler_label(token: &str) -> bool {
    if token.starts_with("__") {
        return true;
    }
    if let Some(rest) = token.strip_prefix("V_") {
        return !rest.is_empty();
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
    // ZP-promoted slots emitted as EQU at the top of the binary —
    // bound to an absolute `$A?` byte address that can never relocate.
    // Treated identically to BSS labels for peephole factorisation
    // safety.
    if let Some(rest) = token.strip_prefix("ZSI_") {
        return !rest.is_empty();
    }
    if let Some(rest) = token.strip_prefix("ZARR_") {
        return !rest.is_empty();
    }
    // Single-letter + digits patterns: S<n>, F<n>, T<n>.
    if let Some(rest) = token.strip_prefix(['S', 'F', 'T'])
        && !rest.is_empty()
        && rest.chars().all(|c| c.is_ascii_digit())
    {
        return true;
    }
    false
}

/// Render a window's instructions as a deterministic signature
/// (one line per instruction, no leading whitespace). Used both as
/// hashmap key and to emit the helper body afterwards.
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

/// Estimate total byte cost of an N-instruction window starting at
/// `start`. Used by PH301 to decide whether factoring nets bytes.
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

/// Splice helper items into `items` at [`helper_anchor_index`], or append when the program has
/// no tail anchor at all.
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

/// True iff a peephole-factored helpers section header has already
/// been emitted (so subsequent factoring rules don't add duplicates).
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

/// Late factoring can move the last direct call to a codegen-owned
/// helper into a peephole helper body. If an older codegen path failed
/// to mark the helper as used, the final asm can otherwise contain a
/// dangling `JMP __FAC_TO_INT16`. Repair those dependencies right
/// before rendering.
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
    push_insn(items, "CMP", Some("#$80".to_string()));
    push_insn(items, "BCC", Some("__FTIN_ZERO".to_string()));
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

/// Markers resolved by name during linking, even without instruction references.
const PINNED_MARKER_LABELS: [&str; 1] = ["__DATA_END"];

/// PH303 — drop unreferenced compiler-internal trampolines.
///
/// After PH004 retargets jumps past a trampoline label like
/// `__BNDS_RT_BAD_12: JMP __BAD_SUBSCRIPT`, the label may have zero
/// remaining references. If so, the label + its trampoline body are
/// dead code — strip them.
///
/// Restricted to:
///   * Labels starting with `__` (compiler-owned, not user-visible).
///   * Followed by exactly one `JMP`/`RTS`/`RTI`. Multi-instruction
///     bodies need fuller dataflow before we can prove deadness, so
///     we leave them alone.
fn ph303_drop_unreferenced_trampolines(items: &mut Vec<Item>) -> bool {
    use std::collections::HashMap;
    // Reference counts: label -> usage count.
    //
    // We have to scan every operand, not just JMP/JSR/Bxx — labels
    // also flow through immediate forms like `LDA #<__HEAP_START`
    // (high/low byte split for pointer setup) and zero-page-offset
    // forms like `STA __NEW_PTR_LO`.
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
                    // Stacked label — pure alias with no body of
                    // its own. The next item is reached the same
                    // way it would have been before.
                    Item::Label(_) => {
                        to_remove.push(i);
                    }
                    // Multi-instruction body. Per-var stubs
                    // (\`__LD_X\` / \`__ST_X\` / \`__FADD_X\` /
                    // \`__BNDS_<n>\` etc.) are loader-then-tail-call
                    // shapes 2-3 instructions long that codegen emits
                    // unconditionally based on a usage-count
                    // threshold. When every call site collapses
                    // away (e.g. the FAC cache lets `PRINT A` reuse
                    // the LET's FAC value, dropping the
                    // \`JSR __LD_A\`), the stub's def stays as dead
                    // weight. Only fire when:
                    //   * The body up to and including a terminator
                    //     contains nothing branchy / no internal
                    //     labels — a self-contained linear thunk.
                    //   * The prior structural item is also an
                    //     uncond terminator (no fall-through entry).
                    Item::Insn(_) if prev_terminator => {
                        // Linear thunk first (cheap); then the general
                        // self-contained region scan for branchy stubs
                        // like `__LD_XI` (load + sign-extend branch).
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

/// Scan a candidate dead stub body starting at `start` (index of the
/// first instruction after the orphan label). Returns `Some(end)`
/// where `end` is the inclusive index of the trailing terminator if
/// the stub is safe to drop wholesale. Returns `None` when the body
/// contains shapes that make wholesale drop unsound:
///   * Conditional or short branches whose target may live OUTSIDE
///     the stub. Their refs would otherwise count toward the parent
///     ref-count tally and accidentally pin labels we later drop.
///   * An interior label or directive — that's the start of a
///     different routine and we'd be deleting code that follows.
///   * No terminator before end-of-items — defensive guard.
fn scan_dead_stub_body(items: &[Item], start: usize) -> Option<usize> {
    let mut i = start;
    let mut steps = 0usize;
    while i < items.len() {
        steps += 1;
        if steps > 8 {
            // Realistic per-var stubs are 2–3 instructions; a body
            // longer than that is more likely a real function we
            // shouldn't risk deleting.
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

/// Scan a dead, *self-contained* helper region starting at the entry
/// label `label_idx` (whose external ref-count the caller has already
/// confirmed is 0, with no fall-through entry). Unlike
/// `scan_dead_stub_body`, this tolerates internal control flow — a
/// branch (`BCC __X_SXT`) plus an internal label — as long as every
/// label in the region is referenced *only* from within it and every
/// conditional branch stays inside. Returns the inclusive index of the
/// region's last item, or `None` when it can't prove the region is a
/// safe, single-entry, unreachable unit.
///
/// Motivating case: `__LD_XI` (signed-byte int load) is a 2-label,
/// branchy stub that `scan_dead_stub_body` rejects, so it lingered as
/// dead weight whenever int promotion left it uncalled.
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
                    // A conditional branch must stay within the region
                    // (target at/after the entry). Anything else and we
                    // can't bound the region soundly.
                    match target {
                        Some(t) if t >= label_idx => frontier = frontier.max(t),
                        _ => return None,
                    }
                } else if insn.mnem == "JMP" {
                    match target {
                        // Internal forward jump — extend the frontier.
                        Some(t) if t >= label_idx => frontier = frontier.max(t),
                        // Tail call / external jump — a region exit.
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
    // Every label in the region must be referenced ONLY from inside it
    // (otherwise external code can enter mid-region and the body is
    // live). Compare total refs to refs originating within [entry..end].
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

/// Does this instruction transfer control to the label named in its operand, as opposed to
/// merely mentioning it?
fn insn_direct_target(insn: &Insn) -> Option<&str> {
    if !matches!(insn.mnem.as_str(), "JMP" | "JSR") && !is_cond_branch(&insn.mnem) {
        return None;
    }
    let op = insn.operand.as_deref()?.trim();

    if op.starts_with('(') || op.starts_with('$') {
        return None;
    }
    // Anything with arithmetic or a byte-select in it is not a plain label target; let the
    // address-taken rule handle those.
    if op.contains(['+', '-', '<', '>', ',', '#']) {
        return None;
    }
    Some(op)
}

/// Every label whose *address* is taken somewhere in the image, i.e. every label that some path
/// could reach without any instruction in the stream naming it as a jump target.
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

/// Indices reachable from the program entry, closed over control flow and over every
/// address-taken label.
fn reachable_items(items: &[Item]) -> Vec<bool> {
    let mut label_at: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for (i, it) in items.iter().enumerate() {
        if let Item::Label(n) = it {
            label_at.insert(n.as_str(), i);
        }
    }
    let mut seen = vec![false; items.len()];
    let mut work: Vec<usize> = Vec::new();
    // Root 1: the top of the image.
    if !items.is_empty() {
        work.push(0);
    }
    // Root 2: every address-taken label.
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

/// PH013 — drop NOP. Our codegen never emits NOP currently, but it's
/// trivial to support and future-proof. Don't fire if the comment
/// flags it as intentional padding.
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

/// PH204 — fold `AND #$80 / Bxx` sign-bit test into `BMI/BPL`.
///
/// Pattern (next-structural between each insn):
/// ```text
///     LDA value
///     AND #$80
///     BNE tgt        ; or BEQ tgt
/// ```
///
/// Becomes:
/// ```text
///     LDA value
///     BMI tgt        ; if original was BNE
///     BPL tgt        ; if original was BEQ
/// ```
///
/// Saves 2 bytes (drops the AND #$80) and 2 cycles per execution.
///
/// Why correct:
///   * `AND #$80` produces N = bit 7 of `value`, Z = (bit 7 == 0).
///   * `LDA value` already produces N = bit 7 of `value`.
///   * `BNE` after `AND #$80` fires when bit 7 set → BMI on LDA's N.
///   * `BEQ` after `AND #$80` fires when bit 7 clear → BPL on LDA's N.
///
/// Safety gates:
///   * `.A` must be dead at live-out of the branch — after the rewrite
///     `.A` holds the original value, not the masked one.
///   * The branch must be the very next structural insn after AND;
///     anything between would consume AND's flags differently.
///   * The LDA's source must be a register-load form (immediate,
///     zp/abs, indexed). Indirect modes are still safe — they set N
///     based on the loaded byte the same way — but we keep the rule
///     narrow.
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

/// PH207 — fold `LD{A,X,Y} #imm / B{EQ,NE} tgt` once `imm` is known.
///
/// The branch condition is fully decided by the immediate, so:
///   * always-taken → rewrite the branch to `JMP tgt`.
///   * never-taken  → drop the branch entirely.
///
/// In addition, when the load's destination register and Z/N flags
/// are dead at the load's live-out, drop the now-pointless load too.
///
/// Pattern (next-structural between):
/// ```text
///     LDA #$NN          ; or LDX/LDY #$NN
///     BEQ tgt           ; or BNE tgt
/// ```
///
/// Why correct: LDA/LDX/LDY set Z based on the loaded byte and N on
/// bit 7. With `imm` known, both flags are constants, so the branch
/// outcome is constant and the destination register holds a known
/// value the dataflow can fold elsewhere.
///
/// Safety gates:
///   * Branch must be the next structural insn after the load.
///   * Replacement uses `JMP` only when the operand is a label (not
///     an absolute `$XXXX` literal that conditional branches accept
///     via long-branch fix-up — JMP needs the same form, but be
///     defensive against odd operands).
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
            // Validate the operand looks safe for JMP — labels and
            // bare `$XXXX` absolutes are fine, indexed/indirect aren't.
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
        // Can the load itself be dropped?
        //
        // After rewrite, nothing between the load and the (possibly
        // gone) branch reads the load's flags or register, so check
        // liveness at the BRANCH's live-out, not the load's: that's
        // the live state past the rewrite point.
        //   * Drop case: branch removed → fallthrough live_in.
        //   * ToJmp case: branch becomes JMP → target's live_in,
        //     which is exactly the branch's live_out for an
        //     always-taken conditional too (both successors merge to
        //     the same target).
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

/// PH205 — mask identities.
///
/// Drop instructions whose immediate operand is a no-op for the
/// op:
///   * `AND #$FF`  (or signed-equivalent `#-1`) — preserves .A
///   * `ORA #$00`  — preserves .A
///   * `EOR #$00`  — preserves .A
///
/// These appear from int-island lowerings where the high-byte AND
/// mask is statically zero (`AND #$00 / STA dest+1` after the
/// stride-truncation patterns) or from ConstantFold leaving an
/// identity mask in place. None affect flags meaningfully for the
/// downstream code we generate (we always re-test via a separate
/// CMP/LDA before branches), so the elision is purely a size +
/// cycle win.
///
/// Skips when the next structural insn is a conditional branch —
/// the AND/ORA/EOR may be needed only to set Z/N flags. (Codegen
/// doesn't currently emit identity-masks just for flag-setting,
/// but be defensive.)
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
        // Don't drop when the next structural instruction is a
        // conditional branch — even an identity op updates Z/N
        // flags. Defensive; current codegen doesn't rely on this.
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

/// PH206 — drop an adjacent `PHA` / `PLA` roundtrip.
///
/// The pair restores .A to its original value, but `PLA` also sets
/// N/Z. Removing it is safe only when those flags are overwritten
/// before any conditional branch can read them.
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

    // Middle: everything up to the first `PLA`.
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
    // Cheap gate: skip both liveness analyses when there is no accumulator-bound transfer in
    // the stream at all.
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
        // `.A` must be dead once the store has run: the rewrite never writes it.
        if regs.is_live_out(j, Reg::A) {
            i += 1;
            continue;
        }
        // N/Z must be dead too: `STY`/`STX` set neither.
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

/// Test bit zero through carry when the accumulator and flags permit it.
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

/// PH208 — hoist `LDY #$00` out of a straight-line self-loop.
///
/// Shape:
/// ```text
/// L:
///     LDY #$00
///     ...
///     JMP L
/// ```
///
/// If the loop body preserves Y as zero, move the load before the
/// label so the back-edge skips it. This is intentionally narrow:
/// exactly one back-edge, no interior labels/branches, and no helper
/// calls unless their Y-preservation contract is known.
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

/// Parse `#$XX` or `#NNN` immediate to its byte value. Returns
/// `None` for non-immediate operands.
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

/// PH101 — drop redundant `MOVFM X` after `MOVMF X`.
///
/// Pattern (6 instructions, possibly with intervening blank/comment):
/// ```text
///     LDX #<X      } MOVMF X (FAC -> X)
///     LDY #>X      }  ↑ keep
///     JSR $BBD4    }
///     LDA #<X      } MOVFM X (X -> FAC)
///     LDY #>X      }  ↓ drop, FAC already has the value
///     JSR $BBA2    }
/// ```
///
/// Bail out at any label between the two triplets — a branch in
/// could enter at the MOVFM half without the FAC contract holding.
fn ph101_movmf_movfm(items: &mut Vec<Item>) -> bool {
    let mut changed = false;
    let mut i = 0;
    while i + 5 < items.len() {
        if let Some(label) = match_movmf_movfm(&items[i..]) {
            // Verify there's no intervening Label between i+2 (MOVMF
            // JSR) and i+3 (LDA). next_struct from i+2 should return
            // i+3 directly.
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

/// PH102 — FAC-aware MOVFM elision.
///
/// `MOVMF X` stores FAC to `X` without changing FAC. If a later
/// straight-line `MOVFM X` reloads the same slot before anything can
/// change FAC or `X`, the reload is redundant.
fn ph102_fac_movfm_elision(items: &mut Vec<Item>) -> bool {
    let reg_live = analyze_register_liveness_with_config(items, &liveness_config_with_helpers());
    let flag_live = analyze_flag_liveness_with_config(items, &peephole_flag_liveness_config());
    let mut plans: Vec<(usize, usize)> = Vec::new();

    // Value-equivalent reload: after MOVMF, FAC still equals the
    // stored slot. Stop at labels/control-flow/unknown calls so no
    // branch can enter the reload with a different FAC.
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

/// PH150 — drop pure-FAC-def JSRs whose result is dead.
///
/// A handful of helpers exist solely to load the floating accumulator
/// from somewhere else (memory, a 16-bit integer, a u8 byte). When
/// the JSR's only observable effect is the FAC value AND the FAC
/// value isn't read before the next FAC def, the entire JSR is
/// dead — and the operand-loading LDA/LDY that preceded it become
/// dead too on the next pass through PH017.
///
/// Targets are restricted to side-effect-free FAC defs:
///   * `$BBA2 MOVFM` — memory → FAC
///   * `$B391 GIVAYF` — i16 (.A=hi, .Y=lo) → FAC
///   * `$B3A2 BYTEFAC` — u8 (.Y) → FAC
///   * `__BOOL_TO_FAC` — basic-bool flag → FAC
///   * `__LD_BYTE_FAC` and `__LDV_<imm>_FAC` — byte→FAC stub family
///
/// Routines that can raise `?ILLEGAL QUANTITY` / `?OVERFLOW` (the
/// arithmetic and transcendental ROM entries) are EXCLUDED — even
/// when the result is dead, the error trap is observable. Routines
/// that touch other state (FOUT writes $0100, FACWORD writes LINNUM,
/// FACARG writes ARG, MOVMF writes user memory, READ updates DATA
/// pointer) are also excluded for the same reason.
///
/// We also require `.A`, `.X`, `.Y` to be dead-out at the JSR. The
/// helpers clobber all three; if any is live, dropping the JSR
/// would lose a value the caller still needs. (PH017's own
/// register-liveness check handles the operand-load drops on the
/// next iteration once this rule has removed the JSR.)
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
        // Must be safe to drop: the value FAC is computing isn't
        // read before being overwritten, and the registers the JSR
        // clobbers aren't observed downstream either.
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

/// True iff a JSR target is a pure FAC-defining helper with no
/// other observable side effects (no error trap, no memory write
/// outside FAC, no DATA-pointer advance). Match must be conservative;
/// when in doubt, keep the call.
fn is_pure_fac_def_target(target: &str) -> bool {
    matches!(
        target,
        "$BBA2" | "$B391" | "$B3A2" | "__BOOL_TO_FAC" | "__LD_BYTE_FAC"
    ) || target.starts_with("__LDV_")
}

/// PH104 — `LDA $66 / CMP #0 / BNE target` -> `LDA $66 / BMI target`.
///
/// FAC sign is stored as `$00` for non-negative and `$FF` for
/// negative, so LDA already sets N exactly as the zero-compare's BNE
/// would classify the value. Dropping CMP is only safe when its Carry
/// result is dead.
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

/// Matcher for the 6-instruction MOVMF/MOVFM pattern. Returns the
/// shared label on match.
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

/// Register *inputs* of well-known C64 ROM entry points — the registers
/// a routine reads as arguments. Only routines whose convention is
/// textbook-certain are listed; everything else stays conservative
/// (treated as reading every register). `defs` is intentionally left
/// empty: under-approximating clobbers keeps the liveness sound (it can
/// only ever keep a value live longer, never drop a live one), which is
/// the safe direction for the consumer below.
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
    // Recognised non-.X routines — known to read at most .A/.Y. (codegen
    // emits a small set; anything not listed stays conservative.)
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
    // Routines that *return* a value in .X (so they definitely write it):
    // modelling the def is what stops a later read of that produced value
    // from looking like a read of the caller's .X. Under-approximating
    // defs stays sound; we only list certain ones.
    let defs_x = addr == rt::RDTIM; // returns the jiffy clock in .A/.X/.Y
    if uses_x {
        // Reads .X; over-approximate the rest as ALL (we only reason
        // precisely about .X for the loop-index pass).
        Some(RegEffect::new(RegSet::ALL, RegSet::EMPTY))
    } else if known_no_x {
        // Definitely doesn't read .X; conservatively keep .A/.Y as used.
        let defs = if defs_x { RegSet::X } else { RegSet::EMPTY };
        Some(RegEffect::new(RegSet::A.union(RegSet::Y), defs))
    } else {
        None // unknown ROM address -> caller treats it conservatively
    }
}

/// Build a liveness config in which every call (`JSR`) reads only the
/// registers it actually consumes, rather than the conservative
/// "everything". Compiler-owned helper effects are *derived* from their
/// own emitted bodies via a call-graph fixpoint (sound by construction);
/// known ROM routines come from [`rom_call_effect`]; anything still
/// unknown stays conservative. `return_live_out` is empty because a
/// SYS-entered program owes BASIC no particular register on return.
fn build_precise_call_config(items: &[Item]) -> RegisterLivenessConfig {
    let labels = register_liveness_label_map(items);

    // Seed: ROM routines (by operand text) get their known input set;
    // everything else starts optimistic (reads nothing) and grows to a
    // least fixpoint — over-approximating `uses` as we go keeps it sound.
    let mut known: std::collections::HashMap<String, RegEffect> = std::collections::HashMap::new();

    // Fixpoint over helper bodies. On each round we run liveness with
    // the current effect estimates and a `return_live_out` of EMPTY, so
    // a label's `live_in` is exactly the registers its body reads before
    // writing — i.e. its `uses`.
    for _ in 0..16 {
        let mut config = RegisterLivenessConfig {
            default_jsr: RegEffect::new(RegSet::ALL, RegSet::EMPTY),
            known_jsr: known.clone(),
            known_jsr_prefix: Vec::new(),
            external_jmp_uses: RegSet::ALL,
            return_live_out: RegSet::EMPTY,
        };
        // ROM routines aren't labels, so the liveness sees them through
        // `known_jsr` keyed by their operand string — add any not yet
        // present so the first round already has them.
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

    // Final config used by consumers: derived helpers + ROM, conservative
    // for anything still unknown.
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

/// Loop-index register promotion. Keeps a counting loop's index variable in
/// the X register across the body (`INC V`→`INX`, compare→`CPX`, `LDX V`
/// reloads dropped, `STX V` to keep memory synced) when it's used purely
/// as increment + compare + array index, and the *precise* call-effect
/// liveness ([`build_precise_call_config`]) proves X is free. Bails
/// (returns false) unless every guard holds.
fn ph210_loop_index_to_x(items: &mut Vec<Item>) -> bool {
    let labels = register_liveness_label_map(items);
    let config = build_precise_call_config(items);
    let liveness = analyze_register_liveness_with_config(items, &config);

    for j in 0..items.len() {
        let Item::Insn(jmp) = &items[j] else { continue };
        if jmp.mnem != "JMP" {
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

        // Region [h..=j]: no JSR/data/extra-JMP; back-edge at j only.
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
        // Head targeted only by the back-edge; no external entry to an
        // internal label.
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

        // Index var V: exactly one `INC V`; every other mention is
        // `LDX V` or `LDA V` immediately before a `CMP` whose A is dead.
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

        // X must be free: dead at head, only def'd by `LDX V`, only used
        // as an index, and dead on every loop exit.
        if liveness.is_live_in(h, Reg::X) {
            continue;
        }
        let mut x_ok = true;
        for k in h..=j {
            let Item::Insn(ins) = &items[k] else { continue };
            // JMP / conditional branches are control flow — they read no
            // data register (the default config's "external jump reads
            // everything" is meaningless for an internal edge). Their
            // exit targets are vetted by the liveness check below.
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

        // The per-iteration `STX V` keeps V's memory current. It can be
        // dropped only when BOTH hold:
        //  (1) V's memory is never *read* outside the loop (so no
        //      external code observes the loop-updated value), and
        //  (2) the fall-through entry immediately before the head
        //      stores V (`STA V`), so the inserted preheader `LDX V`
        //      always reloads a fresh value on every (re-)entry rather
        //      than a stale leftover from a prior run.
        // Otherwise the sync stays (the safe default).
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

/// One bounded fixpoint of the branch-shaping rules.
fn late_branch_fixpoint(items: &mut Vec<Item>) -> bool {
    let mut any = false;
    for _ in 0..8 {
        let mut changed = false;
        changed |= ph005_branch_around_jmp(items);
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

/// Run the peephole pass to fixpoint and return the rewritten asm.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// Try additional factoring windows when the ordinary size layout cannot fit.
    pub aggressive_size_dedup: bool,
    /// Additional closed-region factoring for a final failed-layout rescue.
    pub compact_size_dedup: bool,
    /// Allow inline string-print arguments only when the image has no reserved ranges.
    pub inline_str_print_args: bool,
}

/// Back-compatible entry point: every optional rewrite off.
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
    if profile == Profile::Size && opts.compact_size_dedup {
        zp_stores::compact(&mut items);
    }
    // Bound the iteration count — most realistic asm settles in 2-3
    // rounds, but malformed input shouldn't be able to spin forever.
    for _ in 0..8 {
        let mut changed = false;
        changed |= ph101_movmf_movfm(&mut items);
        changed |= ph102_fac_movfm_elision(&mut items);
        changed |= ph150_drop_dead_fac_def(&mut items);
        changed |= ph104_fac_sign_cmp_branch(&mut items);
        changed |= ph005_branch_around_jmp(&mut items);
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
        // PH207 must run BEFORE PH021: PH021's broader fact analysis
        // would happily fold the same branches PH207 targets, but it
        // doesn't know to drop the now-pointless constant load. Give
        // PH207 the first look so it can handle the load too.
        changed |= ph207_const_load_branch_fold(&mut items);
        changed |= ph021_constant_branch_fold(&mut items);
        changed |= ph022_jmp_to_rts(&mut items);
        changed |= ph024_drop_dead_cmp(&mut items);
        changed |= ph025_drop_redundant_store(&mut items);
        changed |= ph028_drop_lda_after_inc_dec(&mut items);
        changed |= ph026_simplify_alu_imm(&mut items);
        changed |= ph012_drop_cmp_zero(&mut items);
        // Liveness-driven rules. They re-run the whole liveness
        // analysis on each call (cheap relative to what they save);
        // included in the fixpoint so each rewrite exposes new dead
        // operations to the next iteration.
        changed |= ph019_lda_to_index_load(&mut items);
        changed |= ph015_drop_dead_transfer(&mut items);
        changed |= ph017_drop_dead_load(&mut items);
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

    for _ in 0..16 {
        if !ph110_factor_jsr_pair(&mut items) {
            break;
        }
    }

    if ph113_factor_jsr_triple(&mut items) {
        for _ in 0..8 {
            if !ph110_factor_jsr_pair(&mut items) {
                break;
            }
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
    // Drop any helpers that ended up unreferenced after the factoring
    // rounds (e.g. an inner pair-stub whose only caller got rewritten
    // into a different helper by a later round).
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

    for _ in 0..8 {
        if !ph306_drop_unreachable_code(&mut items) {
            break;
        }
        ph303_drop_unreferenced_trampolines(&mut items);
    }
    ensure_referenced_codegen_helpers(&mut items);
    if profile == Profile::Size && opts.compact_size_dedup {
        structured::factor(&mut items);
        short_pairs::compact(&mut items);
    }

    if opts.inline_str_print_args {
        ph114_str_print_inline_args(&mut items);
    }
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

    #[test]
    fn a_program_with_neither_anchor_appends() {
        let mut items = parse("    RTS\n");
        assert!(helper_anchor_index(&items).is_none());
        append_helpers_before_heap(&mut items, vec![Item::Label("__H".to_string())]);
        assert!(matches!(items.last(), Some(Item::Label(n)) if n == "__H"));
    }

    #[test]
    fn two_splices_both_land_before_the_anchor() {
        // The anchor index MUST be re-derived: the first splice pushes `__BSS_START` down, and
        // a cached index aims the second one past it.
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

    /// N distinct `LDA #<S / LDY #>S / JSR __STR_PRINT` triples plus a minimal `__STR_PRINT`
    /// body with the second entry point.
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
        // An asm without `__STR_PRINT_PTR:` predates the convention; the trampoline would jump
        // at a label that does not exist.
        let src = str_print_program(20).replace("__STR_PRINT_PTR:\n", "");
        let mut items = parse(&src);
        assert!(!ph114_str_print_inline_args(&mut items));
    }

    #[test]
    fn ph114_leaves_the_register_convention_alone() {
        // Pointer-in-a-variable calls (`LDA V / LDY V+1`) and computed string expressions
        // cannot carry an inline argument.
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
        // BNE to a literal $XXXX address (e.g. into ROM) has no
        // matching label in the items list. The successor lookup
        // can't capture that arm of the CFG, so the effect must
        // pull in `external_jmp_uses` so registers stay
        // conservatively live across the branch.
        let items = parse("    LDX #$01\n    BNE $A437\n    LDA #$00\n    RTS\n");
        let live = analyze_register_liveness(&items);
        assert!(
            live.is_live_out(0, Reg::X),
            "X live across the branch (target side)"
        );
    }

    #[test]
    fn ph017_drops_lda_immediately_overwritten() {
        // LDA $14 / LDA $15 / STA dst — the first LDA is shadowed
        // by the second (no read of A in between). Drop it.
        let input = "    LDA $14\n    LDA $15\n    STA dst\n    RTS\n";
        let got = run_default(input);
        assert!(!got.contains("LDA $14"), "shadowed LDA dropped:\n{got}");
        assert!(got.contains("LDA $15"), "second LDA kept");
    }

    #[test]
    fn ph017_keeps_lda_when_io_volatile() {
        // Reads from $D012 (VIC raster line) have side effects we
        // can't see — refuse the drop.
        let input = "    LDA $D012\n    LDA $D013\n    STA dst\n    RTS\n";
        let got = run_default(input);
        assert!(got.contains("LDA $D012"), "I/O LDA preserved:\n{got}");
    }

    #[test]
    fn ph017_keeps_lda_when_branch_uses_flags() {
        // BNE follows the load — the BNE branches on the load's
        // N/Z flags so we can't drop it.
        let input = "    LDA $14\n    BNE skip\n    LDA $15\nskip:\n    STA dst\n    RTS\n";
        let got = run_default(input);
        assert!(got.contains("LDA $14"), "flag-feeding LDA kept:\n{got}");
    }

    #[test]
    fn ph019_lda_imm_tax_becomes_ldx_imm() {
        // Body uses .X for STX, then redefines .A before RTS so the
        // analyser can see that the LDA #$05 value never escapes.
        // (Without the trailing redefinition, RTS is conservatively
        // treated as observing all three registers and the rewrite
        // is correctly refused.)
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
        // (zp),Y has no LDX equivalent — the rewrite would be
        // illegal, so leave the pair intact.
        let input = "    LDA ($FD),Y\n    TAX\n    STX dst\n    RTS\n";
        let got = run_default(input);
        assert!(got.contains("LDA ($FD),Y"));
        assert!(got.contains("    TAX"));
    }

    #[test]
    fn reg_liveness_handles_branch_with_unknown_label() {
        // Branch operand is a name but not in the label map (e.g.
        // forward-declared / not yet emitted). Same conservative
        // treatment as a literal address.
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
        assert_eq!(
            got.matches("LDA #$00").count(),
            2,
            "second load must refresh Z for BEQ:\n{got}"
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
        // In `DEC m / LDA m / BEQ ...` the DEC already set Z from the
        // new memory value, so the LDA is a 3-byte / 4-cycle redundancy
        // that only re-fills A. With A dead at the BEQ (both branch
        // targets redefine A before any read), drop the LDA. This is the
        // shape `M=M-1: IF M=0 THEN 80` lowers to: `DEC VI_M` followed by
        // `LDA VI_M / BEQ L80`.
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
        // Same DEC/LDA pair but A is consumed before any redefine —
        // the LDA must stay since A's loaded value is live out.
        let input = "*=$0801\nL1:\n    DEC VI_M\n    LDA VI_M\n    STA VI_C\n    RTS\n";
        let got = run_default(input);
        assert!(
            got.contains("LDA VI_M"),
            "LDA VI_M must NOT be dropped — STA VI_C consumes A:\n{got}"
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
        // LDX #$05 / INX → X is known to be 6, so a follow-up
        // CPX #$06 / BEQ ok statically resolves; PH021 drops the
        // BEQ (and the dead RTS that followed). The `INX` may also
        // be dropped if X is dead — keep an explicit use to
        // anchor the test to the comparison fold.
        let input = "    LDX #$05\n    INX\n    CPX #$06\n    BEQ ok\n    RTS\n    NOP\nok:\n    INX\n    RTS\n";
        let got = run_default(input);
        assert!(
            !got.contains("BEQ ok"),
            "BEQ removed (always taken):\n{got}"
        );
    }

    #[test]
    fn ph020_folds_and_immediate() {
        // A=$F0, AND #$0F → A=$00, Z=true. BEQ becomes always-taken
        // and eventually disappears via the PH021/PH001 chain.
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
        // STA tmp records mem_imm[tmp]=$05; subsequent LDA tmp
        // restores A.imm=$05 so a follow-up CMP can constant-fold.
        let input = "    LDA #$05\n    STA tmp\n    JSR helper\n    LDA tmp\n    CMP #$05\n    BEQ ok\n    RTS\nok:\n    RTS\n";
        let got = run_default(input);
        // After JSR, mem_imm[tmp] is reset by reset(), so this
        // negative test verifies we DON'T over-propagate across
        // call barriers. The CMP must remain.
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

    // PH024 (drop dead CMP/CPX/CPY) intentionally uses the
    // conservative liveness config that treats RTS and labels as
    // flags-live boundaries. That means the rule only fires in
    // narrow cases where another flag-defining op fully shadows
    // the comparison BEFORE control reaches a barrier — rare in
    // practice. The simpler-looking tests we tried for it kept
    // running into the conservatism (label barrier, RTS barrier),
    // so we don't assert on it here. The keep-when-live test
    // below verifies we don't regress the safety side.

    #[test]
    fn ph025_drops_redundant_store_after_known_value() {
        // First STA records mem_imm[tmp]=$05; the LDA #$05/STA tmp
        // pair after it stores the same value, so the second STA
        // is redundant.
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
        assert!(got.contains("LDA #$00"), "AND #$00 → LDA #$00:\n{got}");
    }

    #[test]
    fn ph026_keeps_and_ff_when_z_is_live() {
        // Z is read by the BEQ → AND #$FF must stay.
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
        // `emit_poke_store_abs` emits `BIT $D01E` to reproduce the dummy read of the interpreter's own `STA ($14),Y`.
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

    /// `LDX` / `LDY` set Z based on the loaded byte in X/Y, NOT in A,
    /// so a `CMP #$00 / BNE` after them must not be dropped. In
    /// `__GET_STR`'s no-key path the helper runs
    /// `JSR GETIN / STA __GET_HEAP+1 / LDX #$01 / CMP #$00 / BNE …`
    /// where the CMP tests A (the key code) against zero; dropping it
    /// would make BNE branch on Z-from-LDX instead of the key byte.
    /// A BCC/BCS that sits TWO branches after the CMP must still gate
    /// the drop: the intervening BEQ only reads Z (which LDA does set),
    /// but the BCC reads carry, which only CMP sets. A single-step
    /// look-ahead would see only the BEQ and drop the CMP too early.
    #[test]
    fn ph012_keeps_cmp_zero_when_bcc_follows_bne_or_beq() {
        let input = "    LDA value\n    CMP #$00\n    BEQ skip\n    BCC skip\n    RTS\n";
        let got = run_default(input);
        assert!(
            got.contains("CMP #$00"),
            "CMP must NOT be dropped when a BCC/BCS sits past an \
             intervening BEQ — the BCC needs the carry CMP set:\n{got}"
        );
    }

    #[test]
    fn ph012_keeps_cmp_zero_after_ldx() {
        let input = "    LDA value\n    LDX #$01\n    CMP #$00\n    BNE skip\n    RTS\n";
        let got = run_default(input);
        assert!(
            got.contains("CMP #$00"),
            "CMP after LDX must NOT be dropped — Z from LDX reflects X, not A:\n{got}"
        );
    }

    #[test]
    fn ph012_keeps_cmp_zero_after_ldy() {
        let input = "    LDA value\n    LDY #$01\n    CMP #$00\n    BNE skip\n    RTS\n";
        let got = run_default(input);
        assert!(
            got.contains("CMP #$00"),
            "CMP after LDY must NOT be dropped — Z from LDY reflects Y, not A:\n{got}"
        );
    }

    #[test]
    fn ph012_keeps_cmp_zero_after_inx() {
        let input = "    LDA value\n    INX\n    CMP #$00\n    BNE skip\n    RTS\n";
        let got = run_default(input);
        assert!(
            got.contains("CMP #$00"),
            "CMP after INX must NOT be dropped — Z from INX reflects X, not A:\n{got}"
        );
    }

    #[test]
    fn ph012_keeps_cmp_zero_after_dec_memory() {
        let input = "    LDA value\n    DEC mem\n    CMP #$00\n    BNE skip\n    RTS\n";
        let got = run_default(input);
        assert!(
            got.contains("CMP #$00"),
            "CMP after DEC <mem> must NOT be dropped — Z reflects memory, not A:\n{got}"
        );
    }

    #[test]
    fn ph012_drops_cmp_zero_after_implicit_asl() {
        // `ASL` with no operand shifts A and sets Z on the result —
        // safe to drop the redundant CMP.
        let input = "    LDA value\n    ASL\n    CMP #$00\n    BEQ ok\n";
        let got = run_default(input);
        assert!(
            !got.contains("CMP #$00"),
            "CMP after implicit ASL is redundant:\n{got}"
        );
    }

    #[test]
    fn ph012_keeps_cmp_zero_after_asl_memory() {
        // `ASL <mem>` shifts memory; Z reflects the byte at <mem>,
        // not A. CMP #$00 against A is still meaningful.
        let input = "    LDA value\n    ASL mem\n    CMP #$00\n    BEQ ok\n";
        let got = run_default(input);
        assert!(
            got.contains("CMP #$00"),
            "CMP after ASL <mem> must NOT be dropped:\n{got}"
        );
    }

    #[test]
    fn ph106_factors_intbyte_givayf_at_threshold() {
        // 4 sites — at-threshold for the rule. Each FAC value is
        // observed by `__PRINT_FAC` so PH150 doesn't drop dead defs
        // before factoring sees the duplicates.
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
        // High byte non-zero — different value, can't factor.
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
        // 4 sites — at-threshold for PH107.
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
        // Only the helper keeps the LDA; each rewritten site drops it.
        assert_eq!(
            got.matches("LDA #$0D").count(),
            1,
            "only the helper keeps LDA #$0D"
        );
        // Original JSR $FFD2 only present in the helper's body.
        assert_eq!(got.matches("JSR $FFD2").count(), 0, "old JSR retargeted");
        assert!(got.contains("JMP $FFD2"), "helper falls through with JMP");
    }

    #[test]
    fn ph107_factors_per_byte_value() {
        // Two distinct imm values — each gets its own helper.
        let mut input = String::new();
        for _ in 0..4 {
            input.push_str("    LDA #$0D\n    JSR $FFD2\n");
            input.push_str("    LDA #$12\n    JSR $FFD2\n");
        }
        let got = run_default(&input);
        assert!(got.contains("__CHROUT_0D:"));
        assert!(got.contains("__CHROUT_12:"));
        // Both helpers are referenced — either directly (`JSR __CHROUT_0D`)
        // or via a downstream PH110 JSR-pair helper. Only require that
        // each helper label is reached somewhere.
        assert!(got.contains("__CHROUT_0D"));
        assert!(got.contains("__CHROUT_12"));
    }

    #[test]
    fn ph107_skips_under_threshold() {
        // 3 sites — under threshold, no helper.
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
        // Target $FFD2 is the only one we factor. JSR to a label
        // (e.g. our own helper) shouldn't fire.
        let mut input = String::new();
        for _ in 0..4 {
            input.push_str("    LDA #$0D\n    JSR __OWN_HELPER\n");
        }
        let got = run_default(&input);
        assert!(!got.contains("__CHROUT_"));
    }

    #[test]
    fn ph107_doesnt_factor_when_lda_isnt_byte_immediate() {
        // `LDA <addr>` (no #) — not a byte-imm, skip.
        let mut input = String::new();
        for _ in 0..4 {
            input.push_str("    LDA $0400\n    JSR $FFD2\n");
        }
        let got = run_default(&input);
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

    /// `JSR` adds a frame; `JMP` does not.
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

    /// PH113's first two positions stay `JSR`s inside the stub, so both are frame-adding; only the third becomes the tail `JMP`.
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
        // Threshold is 3 sites — under 3 should not factor.
        let mut input = String::new();
        for _ in 0..2 {
            input.push_str("    LDA #<__VAR_X\n    LDY #>__VAR_X\n    JSR $BBA2\n    NOP\n");
        }
        let got = run_default(&input);
        assert!(!got.contains("__T0:"));
    }

    #[test]
    fn ph110_factors_jsr_pair_at_threshold() {
        // 4 sites with same JSR pair → JSR2 helper.
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
        // PH110 iterates to fixpoint, so pairs of factored helpers
        // ARE captured by subsequent passes. Confirm a pair of
        // existing __JSR2_* helpers gets a new __JSR2_2 wrapper.
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
        // 4 sites with same imm → per-imm helper. Observe FAC at each
        // site so PH150 doesn't prune duplicates before factoring.
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
        // No naked LDY #$01 + JSR __LD_BYTE_FAC sequences should
        // remain (all rolled into PH109).
        assert!(!got.contains("LDY #$01\n    JSR __LD_BYTE_FAC"));
    }

    #[test]
    fn ph109_skips_under_threshold_per_imm() {
        // 2 sites with same imm, 2 with another — each below threshold.
        let mut input = String::new();
        for _ in 0..2 {
            input.push_str("    LDA #$00\n    LDY #$01\n    JSR $B391\n");
            input.push_str("    LDA #$00\n    LDY #$02\n    JSR $B391\n");
        }
        let got = run_default(&input);
        // PH106 fired (it has 4 total triples, one per-LDY value but
        // the imm varies, so PH106 still factors all 4 pairs into
        // shared __LD_BYTE_FAC). PH109 should NOT fire because no
        // single imm hits 4.
        assert!(!got.contains("__LDV_"), "no per-imm helper:\n{got}");
    }

    #[test]
    fn ph301_factors_repeated_4window() {
        // 4 occurrences of the LINNUM→ARR copy pattern. Should
        // produce a __SEQ_0 helper.
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
        // Stable compiler-internal labels (`__ARR_*`) ARE factorable
        // because PH303 already ran and won't drop them later.
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
        // BASIC line labels (`L<digits>`) are GOTO-reachable and
        // potentially relocatable; refuse to factor windows that
        // mention them.
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
        // First PH301 pass refuses any JSR — those go through PH110/PH113
        // first. The late pass with `JsrPolicy::AllowOne` picks up the
        // mixed (load + helper-call + store) shape after PH110/PH113
        // have settled. This test pins the first-pass behaviour by
        // running a no-JSR window through the same harness and
        // expecting __SEQ_; the existence of the late pass is covered
        // by `ph301_late_pass_allows_single_jsr`.
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
        // The late pass admits a single JSR per 4-window. This is the
        // common "address load + helper call + landing store" pattern.
        // Stub the same shape four times and verify a
        // __SEQ_ helper is emitted. JSR __PRINT_FAC observes FAC so
        // PH150 doesn't drop the MOVFM before factoring.
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
        // Even the late pass refuses a window with two or more JSRs
        // — those are the patterns PH110/PH113 already collapse into
        // tighter pair/triple stubs. We don't want PH301 wrapping
        // them in __SEQ_n on top.
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
        // Two occurrences, twelve instructions, three interleaved calls, and a live value pushed across a call.
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

    /// A label named only from a split `.byte <label` / `.byte >label` pointer
    /// table must survive, even when stacked in front of another label.
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

    /// A word in a comment, or the directive's own mnemonic, is not a
    /// label reference.
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
        // After PH004 retargets the JSR, __TRAMP becomes unreachable.
        // We keep an external __REAL reference (via JSR) so the
        // resolved target sticks around. __REAL has a non-trivial
        // body so PH022 (jmp-to-rts) doesn't fold the trampoline
        // into a bare RTS.
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
        // Trampoline still has a caller — must not strip the
        // resolved target. (After PH003 + PH004, the original
        // `JSR __TRAMP` cascades to `JMP __REAL` and __TRAMP itself
        // becomes 0-ref. PH303 then drops the now-orphan label-plus-
        // JMP since its previous structural item is the cascaded JMP
        // — an unconditional terminator, so falling through into
        // it isn't possible. We only need to verify the live target
        // (__REAL) stays wired up.)
        let input = concat!(
            "    JSR __TRAMP\n",
            "    RTS\n",
            "__TRAMP:\n",
            "    JMP __REAL\n",
            "__REAL:\n",
            "    RTS\n",
        );
        let got = run_default(input);
        // The whole snippet collapses after PH003 + PH004 (caller
        // folds to nothing, both helpers become unreachable). What
        // we want from PH303 here is simply that no \`JSR __TRAMP\`
        // is left dangling against a label we already collapsed.
        assert!(
            !got.contains("JSR __TRAMP"),
            "JSR to absorbed trampoline must not survive:\n{got}"
        );
    }

    #[test]
    fn ph303_keeps_function_end_rts_after_inverted_branch() {
        // PH005 turns `BCS X / JMP Y / X: / RTS` into
        // `BCC Y / X: / RTS`, leaving X 0-ref. PH303 must NOT
        // strip the X-label-plus-RTS while the function is still
        // *live* (here `__BNDS_CHECK` is called from `L1`): the
        // BCC falls through into the RTS, which is the function's
        // only no-error return path. (When the whole helper is
        // dead — no callers — removing it wholesale is fine; that
        // case is covered by `ph303_drops_dead_branchy_stub`.)
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
        // Whether __BC_OK survives doesn't matter — what matters
        // is that __BNDS_CHECK still has a return path so a
        // successful bounds check leaves the helper instead of
        // falling into __BNDS_10's setup.
        assert!(
            got.lines().any(|l| l.trim() == "RTS"),
            "function-end RTS must survive PH303:\n{got}"
        );
    }

    #[test]
    fn ph303_drops_dead_branchy_stub() {
        // A `__LD_XI`-style stub: unreferenced entry, an internal
        // sign-extend branch + internal label, ending in a tail call.
        // The whole self-contained region is dead and must be removed
        // (the old linear-only scan left it as dead weight).
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
        // `__JSR2_0: JSR A / JMP B` with one `JSR` caller.
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
        // PH308's arithmetic says inline (two sites, one-byte body); the stack says no.
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
        // `$FFFF` is the tree-wide placeholder for an operand the program rewrites at run time: same text, different effect.
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
        // `L1:` means something can jump straight at the `LDY`, so it does not run once per call and cannot move into the callee.
        let (changed, got) = drive(
            ph309_absorb_common_pred,
            "    BNE L1\n    LDY #$00\n    JSR __SEQ_0\nL1:\n    LDY #$00\n    JSR __SEQ_0\n\
                 RTS\n__SEQ_0:\n    STA $C000\n    RTS\n",
        );
        assert!(!changed, "a labelled predecessor must not move:\n{got}");
    }

    #[test]
    fn ph309_f3a_refuses_a_helper_reached_by_a_tail_jump() {
        // A `JMP __SEQ_0` entry has no predecessor this pass looked at, so the site count and the reference count disagree.
        let (changed, got) = drive(
            ph309_absorb_common_pred,
            "    LDY #$00\n    JSR __SEQ_0\n    LDY #$00\n    JSR __SEQ_0\n    LDY #$00\n\
                 JMP __SEQ_0\n__SEQ_0:\n    STA $C000\n    RTS\n",
        );
        assert!(!changed, "an unaccounted entry blocks absorption:\n{got}");
    }

    #[test]
    fn ph309_f3a_refuses_a_helper_that_can_be_fallen_into() {
        // `LDA #$01` before the label, not a terminator: control reaches the body without passing a call site.
        let (changed, got) = drive(
            ph309_absorb_common_pred,
            "    LDY #$00\n    JSR __SEQ_0\n    LDY #$00\n    JSR __SEQ_0\n    LDA #$01\n\
             __SEQ_0:\n    STA $C000\n    RTS\n",
        );
        assert!(!changed, "a fall-through entry blocks absorption:\n{got}");
    }

    /// Mirror of [`absorb_pred_fixture`]: `succ` sits after each site.
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
        // `L1:` means the successor can be reached without returning from the helper.
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
        // A name captured by a `.word` has a caller this pass cannot repoint, so references outnumber call sites.
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
        // `__SEQ_9` is exactly the last three instructions of `__SEQ_3`, and those three are 7 bytes against the 3 a JMP costs.
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
        // GIVAYF clobbers FAC between MOVMF and MOVFM. The clobber's
        // result is observed by `__PRINT_FAC` (so PH150 can't drop the
        // GIVAYF), and PH102 must NOT eliminate the second MOVFM
        // because FAC's been overwritten in between.
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
        // Two MOVFMs back-to-back: the first defines FAC, the second
        // immediately redefines it before any use. The first JSR is
        // dead and should be removed.
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
        // Exactly one $BBA2 should remain (the second MOVFM whose
        // result feeds __PRINT_FAC).
        assert_eq!(
            got.matches("JSR $BBA2").count(),
            1,
            "first MOVFM dropped:\n{got}"
        );
        assert!(got.contains("JSR __PRINT_FAC"));
    }

    #[test]
    fn ph150_keeps_movfm_when_fac_used_downstream() {
        // FAC value from MOVFM is consumed by FOUT — must keep.
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
        // FADD can ?OVERFLOW even when the result is dead. Even if
        // FAC becomes dead-out we must NOT drop the JSR — the error
        // trap is observable.
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
        // GIVAYF defines FAC; if the next instruction is another
        // FAC define, the GIVAYF is dead.
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
        // After MOVFM, .Y is consumed by a downstream LDA absolute,Y.
        // Even though FAC is dead, dropping the JSR would leave .Y
        // holding the wrong value.
        //
        // Note: in practice MOVFM clobbers .A/.X/.Y so this scenario
        // doesn't actually arise — the JSR's clobbers make register
        // liveness say A/X/Y die at the JSR. We test the gate by
        // wrapping the JSR in a way where Y has a known-live value
        // after, ensuring the rule's reg-live check is exercised.
        // The test is here to lock the conservative gate in place.
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
        // First MOVFM is dropped (FAC dead-out) — second MOVFM and
        // the LDA $0400,Y both stay because Y is alive into the LDA.
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
        // REAL has a non-trivial body so PH022's jmp-to-rts
        // collapse doesn't fire — we want to verify PH004's
        // trampoline-cascade rewrite in isolation.
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
        assert!(got.contains("JMP REAL"), "first JMP retargeted:\n{got}");
        // After PH001, the outer "JMP REAL" right before REAL: drops too,
        // but the trampoline label and its body remain (no dead-label pass).
        assert!(got.contains("TRAMP:"));
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
        // Inverting would produce e.g. `BNE $AF08`, but BNE is 8-bit
        // PC-relative and cannot reach an absolute ROM address. Must
        // leave the pattern alone.
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
        // Trampoline label resolves to a ROM address. PH004 may
        // rewrite the JMP, but a *branch* must stay pointed at the
        // label so the assembler can route through the trampoline.
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

    /// Indexed operands keep the 3-byte estimate: the 6502 has no zero-page,Y for `LDA`, and
    /// guessing low would claim a branch reaches when it does not.
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

    /// The payoff: a target the naive 3-byte-per-operand estimate puts out of reach, but the
    /// equate table proves is in reach.
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

    // late branch fixpoint
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
        // The would-be shared tail contains a label, so it is an entry point: deleting one copy
        // would strand anything jumping there.
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
        // C1 is live (reached by the JMP) and falls straight into B1, so B1 is reachable
        // without being named.
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
        // `#<B1` is a compound operand; retargeting it would mean string surgery, so the merge
        // is refused outright.
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
        // A hand-built helper with one caller: the body belongs at the call site, and the JSR +
        // RTS detour should vanish.
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
        // `LDA $0100,Y` reads FOUT's digit buffer inside the hardware stack page; wrapping it
        // in JSR/RTS pushes a return address toward that buffer.
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
        // A CURLIN stamp for a line that compiled to nothing: the lo byte is re-stamped two
        // instructions later with nothing in between that could read it or raise an error.
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
        let got = run_default(input);
        assert_eq!(
            got.matches("STA FU_0").count(),
            2,
            "aliased read blocks the drop:\n{got}"
        );
    }

    #[test]
    fn ph020_forwards_zp_fact_across_screen_store() {
        // `STA $0401,X` can't touch zero page, so the X-holds-FU_2 fact survives and the second
        // reload is redundant.
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
        // The bubblesort compare shape: C and D loaded from screen memory, stored to ZP slots,
        // C immediately reloaded for the CMP.
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
        // c reads the same cell b stores to (via EQU): reordering would read the stale
        // pre-store byte.
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
        let got = run_default(input);
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
        // Distinct EQU-less labels are distinct storage even when no origin line is present; an
        // immediate `a` trivially can't alias `d`.
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
        // `.A` held the low byte and `.Y` the high byte; the pops stored high into $04 and low
        // into $03, so the stores have to land that way round.
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
        // `STY` has no `abs,X` encoding, and the index register may not even hold the same
        // value where the store would move to.
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
        // Both successors of the branch redefine A immediately, so A
        // is dead at the branch's live-out and the fold is safe.
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
        // Fallthrough uses A as the masked value via STA dst; rewrite
        // would change A's contents, so refuse the fold.
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
        // Both arms re-define A. After PH207 (or PH021) the BEQ goes
        // away — we don't care which exact JMP/fallthrough shape the
        // fixpoint settles on, just that the conditional branch and
        // the never-taken path are gone.
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
        // BNE on nonzero LDA is always-taken. Both successors redefine
        // A and its flags before any read, so PH207 should drop both
        // the load and the now-redundant branch (after a JMP rewrite).
        // Avoid the `Bxx tgt / tgt:` shape that PH001 would short-
        // circuit before PH207 ever sees it.
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
        // JSR helper / RTS / dead code should reduce to JMP helper +
        // the dead code dropped.
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
        // Counting loop using VI_X purely as INC + compare + array
        // index, with X otherwise free and dead at the (RTS) end.
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
        // Same shape but the body also does `TXA` (reads X for a
        // non-index purpose) — promotion would clobber that, so the
        // pass must leave the loop alone.
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
        // The branch reads the N/Z the transfer set; `STY` sets none.
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
        // `STY zp,Y` does not exist; `STX zp,Y` does.
        let got = run_default(&ph211_body("    LDY VS_A\n    TYA\n    STA $40,Y\n"));
        assert!(got.contains("    TYA\n"), "no STY zp,Y encoding:\n{got}");
        let got = run_default(&ph211_body("    LDX VS_A\n    TXA\n    STA $40,Y\n"));
        assert!(got.contains("STX $40,Y"), "STX zp,Y exists:\n{got}");
    }

    #[test]
    fn ph211_takes_zero_page_indexed_only_when_the_base_is_proved() {
        // `FU_0 = $A9` is zero page, so `STY FU_0,X` assembles; a label with no EQU could be
        // anywhere and must be refused.
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
        // `ASL` on $C0 gives $80 and sets carry; the immediate keeps the low byte, which is all
        // `TAX` ever transferred.
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
        // $FF + 1 == $00 and $00 - 1 == $FF; `INX`/`DEX` agree, and so do the N/Z they set.
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

    /// `AND #$01 / Bxx L1` where both arms immediately redefine `.A` and N/Z/C, which is what
    /// the rule needs and what a bare `RTS` tail does not give (see [`KILL_TAIL`]).
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
        // `AND` preserves carry; `LSR` overwrites it.
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

    /// An `RTS` is an observer: the caller resumes with this value in the cell and can raise there.
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

    /// An unresolved jump target ; `JSR $FFFF` is the self-modified `SYS` thunk, and the machine code behind it can do anything, including read `$39`.
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

    /// Liveness follows both arms of a branch: a stamp is live if either successor can read it.
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

    /// The high byte is its own cell: a dead `STA $3A` goes without disturbing a live `STA $39` beside it.
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
}
