//! REM-based optimisation hints from third-party BASIC compilers.
//!
//! Two dialects are recognised, each enabled by an explicit option;
//! the parser ignores hint syntax from the other dialect.
//!
//! * **Basic 64** (Data Becker, 1985): `REM@i=A,B,C` declares scalar
//!   variables as integer; `REM@r=...` declares them as real (the
//!   default, so it is a no-op here); `REM@b=...` declares bytes.
//! * **Basic-Boss**: `REM@ \BYTE A,B,C` declares bytes,
//!   `REM@ \WORD A,B,C` declares 16-bit integers, and a trailing
//!   `=FAST` on any name asks for zero-page placement.
//!
//! Hints are user assertions: the compiler trusts them and skips the
//! range proof that normally guards byte/integer promotion. A program
//! that hints `\BYTE C` and then assigns `C=300` will wrap to 44 at
//! runtime, just as it would under the source compiler.
//!
//! Implementation: a pre-pass scans the raw PRG bytes for hint REMs
//! (token `$8F`) and returns three name sets. The parser then upgrades
//! Float `VarName`s whose base sits in the integer set; the codegen
//! force-promotes byte names regardless of range and biases ZP names
//! into the pool.

use std::collections::HashSet;

/// Which third-party compiler's REM-hint syntax to honour. Mutually
/// exclusive: a program may carry hints from only one source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BasicHintDialect {
    /// Ignore hint syntax entirely; REM stays a plain comment.
    #[default]
    None,
    /// Basic 64 (Data Becker): `REM@i=...`, `REM@r=...`, `REM@b=...`.
    Basic64,
    /// Basic-Boss: `REM@ \BYTE ...`, `REM@ \WORD ...`, `=FAST`.
    BasicBoss,
}

impl BasicHintDialect {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "none" | "off" => Ok(Self::None),
            "basic64" | "basic-64" | "b64" => Ok(Self::Basic64),
            "basicboss" | "basic-boss" | "boss" => Ok(Self::BasicBoss),
            other => Err(format!(
                "unknown hint dialect '{other}': expected basic64 or basic-boss"
            )),
        }
    }
}

/// Variables the user has declared via REM hints. All keys are the
/// upper-case 1-or-2 char base name (matching `VarName::base`).
#[derive(Debug, Clone, Default)]
pub struct VarTypeHints {
    /// Promote to integer storage. Subsumes `byte_vars`: every byte
    /// hint also lands here.
    pub int_vars: HashSet<String>,
    /// Promote to single-byte storage. Bypasses the range proof in
    /// `collect_u8_int_vars`.
    pub byte_vars: HashSet<String>,
    /// Ask for zero-page placement (Basic-Boss `=FAST` suffix).
    pub zp_vars: HashSet<String>,
}

impl VarTypeHints {
    pub fn is_empty(&self) -> bool {
        self.int_vars.is_empty() && self.byte_vars.is_empty() && self.zp_vars.is_empty()
    }
}

const TOK_REM: u8 = 0x8F;

/// Walk each tokenised line body, find REM payloads, and pull hints
/// out of each one. Unknown syntax is silently ignored so a `REM @foo`
/// in a Basic64 build does not abort the compile.
pub fn extract_hints(
    line_bodies: impl IntoIterator<Item = &'static [u8]>,
    dialect: BasicHintDialect,
) -> VarTypeHints {
    extract_hints_from(line_bodies, dialect)
}

/// Same as [`extract_hints`] without the `'static` lifetime bound,
/// for callers that already have an iterator over borrowed bodies.
pub fn extract_hints_from<'a, I>(line_bodies: I, dialect: BasicHintDialect) -> VarTypeHints
where
    I: IntoIterator<Item = &'a [u8]>,
{
    let mut hints = VarTypeHints::default();
    if dialect == BasicHintDialect::None {
        return hints;
    }
    for body in line_bodies {
        if let Some(payload) = rem_payload(body) {
            let text = decode_petscii(payload);
            match dialect {
                BasicHintDialect::Basic64 => parse_basic64(&text, &mut hints),
                BasicHintDialect::BasicBoss => parse_basicboss(&text, &mut hints),
                BasicHintDialect::None => {}
            }
        }
    }
    hints
}

/// Find the first `$8F` (REM) in a tokenised line and return the
/// payload after it, or None if the line has no REM. Anything before
/// the REM token is real code we leave alone.
fn rem_payload(line: &[u8]) -> Option<&[u8]> {
    let pos = line.iter().position(|&b| b == TOK_REM)?;
    Some(&line[pos + 1..])
}

/// Best-effort PETSCII to ASCII conversion good enough for hint
/// keywords. Hints only use letters, digits, `@`, `\`, `=`, `,`, and
/// spaces, all of which map identically to ASCII.
fn decode_petscii(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| match b {
            0x41..=0x5A => (b + 0x20) as char, // PETSCII uppercase -> ASCII lowercase
            0x20..=0x7E => b as char,
            _ => ' ',
        })
        .collect()
}

/// Basic 64 syntax: a leading `@` (after optional whitespace), then
/// `i=`, `r=`, or `b=`, then a comma-separated list of names.
fn parse_basic64(text: &str, hints: &mut VarTypeHints) {
    let trimmed = text.trim_start();
    let body = match trimmed.strip_prefix('@') {
        Some(rest) => rest,
        None => return,
    };
    let mut chars = body.char_indices();
    let Some((_, kind_char)) = chars.next() else {
        return;
    };
    let Some((eq_idx, eq_char)) = chars.next() else {
        return;
    };
    if eq_char != '=' {
        return;
    }
    let names_part = &body[eq_idx + 1..];
    match kind_char.to_ascii_lowercase() {
        'i' => collect_names(names_part, &mut hints.int_vars),
        'b' => {
            let mut names = HashSet::new();
            collect_names(names_part, &mut names);
            hints.int_vars.extend(names.iter().cloned());
            hints.byte_vars.extend(names);
        }
        'r' => { /* real = default float, nothing to record */ }
        _ => {}
    }
}

/// Basic-Boss syntax: a leading `@`, then one or more space-separated
/// `\KEYWORD value` clauses on the same line. We honour `\BYTE`,
/// `\WORD`, and the `=FAST` suffix on individual names; everything
/// else (e.g. `\FASTFOR`, `\DATATYPE BYTE`) is read and discarded.
fn parse_basicboss(text: &str, hints: &mut VarTypeHints) {
    let trimmed = text.trim_start();
    let body = match trimmed.strip_prefix('@') {
        Some(rest) => rest.trim_start(),
        None => return,
    };
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'\\' {
            return;
        }
        i += 1;
        let kw_start = i;
        while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
            i += 1;
        }
        let keyword = body[kw_start..i].to_ascii_uppercase();
        while i < bytes.len() && bytes[i] == b' ' {
            i += 1;
        }
        let list_start = i;
        while i < bytes.len() && bytes[i] != b'\\' {
            i += 1;
        }
        let list = &body[list_start..i];
        match keyword.as_str() {
            "BYTE" => {
                let mut names = HashSet::new();
                collect_boss_names(list, &mut names, &mut hints.zp_vars);
                hints.int_vars.extend(names.iter().cloned());
                hints.byte_vars.extend(names);
            }
            "WORD" | "INT" | "INTEGER" => {
                let mut names = HashSet::new();
                collect_boss_names(list, &mut names, &mut hints.zp_vars);
                hints.int_vars.extend(names);
            }
            _ => {}
        }
    }
}

/// Parse `"A,B,C"` into uppercase 2-char names, skipping whitespace.
fn collect_names(s: &str, out: &mut HashSet<String>) {
    for raw in s.split(',') {
        let name = canonical_var(raw);
        if !name.is_empty() {
            out.insert(name);
        }
    }
}

/// Basic-Boss variant: names may carry a `=FAST` (or `=ZP`) modifier
/// that routes them into the zero-page request set.
fn collect_boss_names(s: &str, names: &mut HashSet<String>, zp: &mut HashSet<String>) {
    for raw in s.split(',') {
        let (head, modifier) = match raw.split_once('=') {
            Some((h, m)) => (h, Some(m.trim())),
            None => (raw, None),
        };
        let name = canonical_var(head);
        if name.is_empty() {
            continue;
        }
        if matches!(
            modifier.map(|m| m.to_ascii_uppercase()).as_deref(),
            Some("FAST") | Some("ZP")
        ) {
            zp.insert(name.clone());
        }
        names.insert(name);
    }
}

/// Take the first 1 or 2 alphanumeric characters of `s`, uppercased.
/// Matches BASIC v2's variable-name canonicalisation.
fn canonical_var(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_uppercase());
            if out.len() == 2 {
                break;
            }
        } else if !c.is_ascii_whitespace() {
            break;
        }
    }
    out
}

/// Trim the integer-promotion set to vars where the promotion is
/// likely to pay off. A hinted var whose value is assigned from a
/// float-typed expression would force float-to-int conversion at
/// every store and int-to-float conversion at every read in a float
/// context; on programs where the user wrote `int(...)` purely for
/// rounding rather than to enable integer storage, the conversions
/// cost more than they save.
///
/// The heuristic: scan every LET-into-hinted-var. If the right-hand
/// side reads any Float scalar that is not itself in the hint set,
/// drop the LHS from the integer set. The byte and ZP sets are
/// untouched — those only carry a forced var IF it survives this
/// filter, since byte/ZP both require integer kind.
pub fn filter_safe(program: &crate::ast::Program, hints: VarTypeHints) -> VarTypeHints {
    if hints.int_vars.is_empty() {
        return hints;
    }
    let mut tainted: HashSet<String> = HashSet::new();
    for line in &program.lines {
        for stmt in &line.statements {
            scan_stmt(stmt, &hints.int_vars, &mut tainted);
        }
    }
    let int_vars: HashSet<String> = hints.int_vars.difference(&tainted).cloned().collect();
    let byte_vars: HashSet<String> = hints.byte_vars.intersection(&int_vars).cloned().collect();
    let zp_vars: HashSet<String> = hints.zp_vars.intersection(&int_vars).cloned().collect();
    VarTypeHints {
        int_vars,
        byte_vars,
        zp_vars,
    }
}

/// Walk one statement looking for LET-into-hinted-var assignments
/// whose RHS reads a Float scalar outside the hint set. Recurses
/// into IF / IFELSE bodies; everything else is shallow.
///
/// The taint is LHS-only on purpose. A read-side taint (mark a var
/// when it is read inside a float-typed expression) catches more
/// conversion sites but cascades aggressively: a single line like
/// `u = z + x * xx - x * yy * ux` taints x, xx, yy off the back of
/// one float var (`u`, `z`, `ux`), even when those vars are used
/// in integer contexts elsewhere in the program. LHS-only keeps the
/// cost predictable: it drops a var when *the user's own
/// assignment* forces a conversion, and leaves the rest alone.
fn scan_stmt(
    stmt: &crate::ast::Statement,
    int_hints: &HashSet<String>,
    tainted: &mut HashSet<String>,
) {
    use crate::ast::Statement;
    match stmt {
        Statement::Let { name, value } => {
            if int_hints.contains(&name.base) && expr_touches_outside_float(value, int_hints) {
                tainted.insert(name.base.clone());
            }
        }
        Statement::If { then_branch, .. } => {
            if let crate::ast::ThenBranch::Stmts(inner) = then_branch {
                for s in inner {
                    scan_stmt(s, int_hints, tainted);
                }
            }
        }
        Statement::IfElse {
            then_branch,
            else_branch,
            ..
        } => {
            for br in [then_branch, else_branch] {
                if let crate::ast::ThenBranch::Stmts(inner) = br {
                    for s in inner {
                        scan_stmt(s, int_hints, tainted);
                    }
                }
            }
        }
        _ => {}
    }
}

/// True iff `e` reads any Float scalar whose base is NOT in
/// `int_hints`. A hinted-int var reading another hinted-int var is
/// fine (both end up integer-typed). Anything outside the set is a
/// float variable that would force a conversion if its value flowed
/// into an integer destination.
fn expr_touches_outside_float(e: &crate::ast::Expr, int_hints: &HashSet<String>) -> bool {
    use crate::ast::{Expr, VarKind};
    match e {
        Expr::Var(v) => v.kind == VarKind::Float && !int_hints.contains(&v.base),
        Expr::ArrayRef(v, idx) => {
            (v.kind == VarKind::Float && !int_hints.contains(&v.base))
                || idx.iter().any(|x| expr_touches_outside_float(x, int_hints))
        }
        Expr::Neg(inner) | Expr::Not(inner) => expr_touches_outside_float(inner, int_hints),
        Expr::Bin(_, l, r) => {
            expr_touches_outside_float(l, int_hints) || expr_touches_outside_float(r, int_hints)
        }
        Expr::Func1(_, inner) => expr_touches_outside_float(inner, int_hints),
        Expr::Peek(inner) | Expr::MemPeek(inner) => expr_touches_outside_float(inner, int_hints),
        Expr::FnCall(_, inner) => expr_touches_outside_float(inner, int_hints),
        Expr::Pos(inner) | Expr::Fre(inner) | Expr::Usr(inner) => {
            expr_touches_outside_float(inner, int_hints)
        }
        Expr::Joy(inner) | Expr::Pot(inner) => expr_touches_outside_float(inner, int_hints),
        Expr::At(a, b) | Expr::Test(a, b) => {
            expr_touches_outside_float(a, int_hints) || expr_touches_outside_float(b, int_hints)
        }
        // Anything else (numeric literals, string-derived numerics
        // that produce an int-sized result, system constants) is
        // safe to feed into an integer-typed destination.
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic64_int_decl() {
        let mut h = VarTypeHints::default();
        parse_basic64("@i=xx,yy,e,cl", &mut h);
        assert!(h.int_vars.contains("XX"));
        assert!(h.int_vars.contains("YY"));
        assert!(h.int_vars.contains("E"));
        assert!(h.int_vars.contains("CL"));
        assert!(h.byte_vars.is_empty());
    }

    #[test]
    fn basic64_real_noop() {
        let mut h = VarTypeHints::default();
        parse_basic64("@r=uc,uv", &mut h);
        assert!(h.int_vars.is_empty());
    }

    #[test]
    fn basicboss_byte_word_fast() {
        let mut h = VarTypeHints::default();
        parse_basicboss("@ \\BYTE C,D,M \\WORD W=FAST", &mut h);
        assert!(h.byte_vars.contains("C"));
        assert!(h.byte_vars.contains("D"));
        assert!(h.byte_vars.contains("M"));
        assert!(h.int_vars.contains("W"));
        assert!(!h.byte_vars.contains("W"));
        assert!(h.zp_vars.contains("W"));
    }

    #[test]
    fn basicboss_unknown_keywords_ignored() {
        let mut h = VarTypeHints::default();
        parse_basicboss("@ \\FASTFOR \\DATATYPE BYTE \\BYTE X", &mut h);
        assert!(h.byte_vars.contains("X"));
    }
}
