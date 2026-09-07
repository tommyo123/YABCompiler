//! Find variable references in SYS parameter text for runtime substitution.

use crate::ast::VarKind;

/// One variable reference found in a parameter string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VarRef {
    /// Offset of the first byte of the reference within the parameter string.
    pub start: usize,
    /// Length in bytes, covering the name, any spaces BASIC ignores inside it, and the `%`/`$` type suffix.
    pub len: usize,

    pub base: String,
    pub kind: VarKind,
    /// A `(` follows the name: an array element, or a user-function call when `is_fn` is also set.
    pub subscripted: bool,
    /// The `FN` token ($A5) immediately precedes the name.
    pub is_fn: bool,
}

impl VarRef {

    pub fn is_plain_numeric_scalar(&self) -> bool {
        !self.subscripted && !self.is_fn && matches!(self.kind, VarKind::Float | VarKind::Integer)
    }
}

const TOK_FN: u8 = 0xA5;

/// Scan variable references while respecting quoted text and token boundaries.
pub fn scan(params: &[u8]) -> Vec<VarRef> {
    let mut out = Vec::new();
    let mut i = 0usize;
    let mut quoted = false;
    let mut fn_pending = false;
    while i < params.len() {
        let b = params[i];
        if quoted {
            if b == b'"' {
                quoted = false;
            }
            i += 1;
            continue;
        }
        if b == b'"' {
            quoted = true;
            i += 1;
            continue;
        }
        if b >= 0x80 {

            fn_pending = b == TOK_FN;
            i += 1;
            continue;
        }
        if b.is_ascii_digit() || b == b'.' {
            i = skip_number(params, i);
            fn_pending = false;
            continue;
        }
        if b.is_ascii_alphabetic() {
            let start = i;
            let mut base = String::new();
            base.push(b.to_ascii_uppercase() as char);
            i += 1;
            // BASIC v2 ignores spaces inside an identifier and keeps only the first two characters; the rest are consumed and dropped.
            let mut j = skip_spaces(params, i);
            if matches!(params.get(j), Some(c) if c.is_ascii_alphanumeric()) {
                base.push(params[j].to_ascii_uppercase() as char);
                j += 1;
                loop {
                    j = skip_spaces(params, j);
                    match params.get(j) {
                        Some(c) if c.is_ascii_alphanumeric() => j += 1,
                        _ => break,
                    }
                }
            }
            i = j;
            let kind = match params.get(i) {
                Some(b'%') => {
                    i += 1;
                    VarKind::Integer
                }
                Some(b'$') => {
                    i += 1;
                    VarKind::String
                }
                _ => VarKind::Float,
            };

            let after = skip_spaces(params, i);
            let subscripted = params.get(after) == Some(&b'(');
            out.push(VarRef {
                start,
                len: i - start,
                base,
                kind,
                subscripted,
                is_fn: fn_pending,
            });
            fn_pending = false;
            continue;
        }
        if b != b' ' {
            fn_pending = false;
        }
        i += 1;
    }
    out
}

/// One `SYS`-parameter variable reference, with the BASIC line it sits on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Site {
    pub line: u16,
    pub var: VarRef,
    /// Whether codegen will render this reference's value into the staged text.
    pub substituted: bool,
}

/// Every `SYS`-parameter variable reference in `module`, in program order.
pub fn scan_module(module: &crate::ir::Module) -> Vec<Site> {
    use crate::ir::{Stmt, ThenIr};
    fn in_then(t: &ThenIr, line: u16, out: &mut Vec<Site>) {
        if let ThenIr::Stmts(inner) = t {
            in_stmts(inner, line, out);
        }
    }
    fn in_stmts(stmts: &[Stmt], line: u16, out: &mut Vec<Site>) {
        for s in stmts {
            match s {
                Stmt::Sys {
                    params, param_vars, ..
                } if !params.is_empty() => {
                    for var in scan(params) {
                        let substituted = param_vars
                            .iter()
                            .any(|p| var.start >= p.start && var.start < p.start + p.len);
                        out.push(Site {
                            line,
                            var,
                            substituted,
                        });
                    }
                }
                Stmt::If { then, .. } => in_then(then, line, out),
                Stmt::IfElse {
                    then, else_then, ..
                } => {
                    in_then(then, line, out);
                    in_then(else_then, line, out);
                }
                Stmt::Rcomp { then, else_then } => {
                    in_then(then, line, out);
                    if let Some(e) = else_then {
                        in_then(e, line, out);
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    for line in &module.lines {
        in_stmts(&line.stmts, line.number, &mut out);
    }
    out
}

fn skip_spaces(p: &[u8], mut i: usize) -> usize {
    while p.get(i) == Some(&b' ') {
        i += 1;
    }
    i
}

fn skip_number(p: &[u8], mut i: usize) -> usize {
    while matches!(p.get(i), Some(c) if c.is_ascii_digit() || *c == b'.') {
        i += 1;
    }
    if matches!(p.get(i), Some(b'E')) {
        i += 1;
        if matches!(p.get(i), Some(b'+') | Some(b'-')) {
            i += 1;
        }
        while matches!(p.get(i), Some(c) if c.is_ascii_digit()) {
            i += 1;
        }
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(s: &[u8]) -> Vec<String> {
        scan(s)
            .into_iter()
            .map(|v| format!("{}{}", v.base, suffix(v.kind)))
            .collect()
    }
    fn suffix(k: VarKind) -> &'static str {
        match k {
            VarKind::Float => "",
            VarKind::Integer => "%",
            VarKind::String => "$",
        }
    }

    #[test]
    fn constants_and_punctuation_have_no_variables() {
        assert!(scan(b"").is_empty());
        assert!(scan(b",1,24").is_empty());
        assert!(scan(b",1,24,").is_empty());
        assert!(scan(b" 15").is_empty());
        assert!(scan(b",-1").is_empty());
        assert!(scan(b",3.14159").is_empty());
        assert!(scan(b",.5").is_empty());
    }

    #[test]
    fn letters_inside_a_string_literal_are_content_not_variables() {
        assert!(scan(b",1,24,\"LOST 2.00 0.60\"").is_empty());
        assert!(scan(b",9,13,\"UCCCCCCCCCI\"").is_empty());
        // Unterminated literal runs to the end of the text.
        assert!(scan(b",\"ABC").is_empty());
        // A quote closes, so what follows is code again.
        assert_eq!(names(b",\"AB\",X"), vec!["X"]);
    }

    #[test]
    fn exponent_e_is_part_of_the_number() {
        assert!(scan(b",1E3").is_empty());
        assert!(scan(b",1E-3").is_empty());
        assert!(scan(b",1E+3").is_empty());
        assert!(scan(b",1E").is_empty());
        assert!(scan(b",2.5E10,7").is_empty());

        assert_eq!(names(b",E"), vec!["E"]);
    }

    #[test]
    fn tokens_are_not_names() {
        // $C2 = PEEK, $B2 = `=`, $AA = `+`.
        assert_eq!(names(b",\xC2(\x58)"), vec!["X"]);
        assert!(scan(b",\xC2(53280)").is_empty());
    }

    #[test]
    fn plain_scalar() {
        let v = scan(b",X");
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].start, 1);
        assert_eq!(v[0].len, 1);
        assert_eq!(v[0].base, "X");
        assert_eq!(v[0].kind, VarKind::Float);
        assert!(!v[0].subscripted);
        assert!(v[0].is_plain_numeric_scalar());
    }

    #[test]
    fn the_drop_the_lot_line() {
        // Preserve surrounding SYS text when substituting numeric references.
        let v = scan(b",1,X\xAB1,\"\x1E*\"");
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].base, "X");
        assert_eq!(v[0].start, 3);
        assert_eq!(v[0].len, 1);
        assert!(v[0].is_plain_numeric_scalar());
    }

    #[test]
    fn type_suffixes_and_truncation() {
        let v = scan(b",DD%,EE$,ABCDE");
        assert_eq!(
            names(b",DD%,EE$,ABCDE"),
            vec!["DD%".to_string(), "EE$".to_string(), "AB".to_string()]
        );
        assert_eq!(v[0].len, 3);
        assert_eq!(v[1].len, 3);
        assert_eq!(v[2].len, 5);
        assert!(v[0].is_plain_numeric_scalar());
        assert!(!v[1].is_plain_numeric_scalar());
    }

    #[test]
    fn embedded_spaces_are_ignored_inside_a_name() {
        // `RN B(5)` is array RN indexed by 5 ; parse::var_name's rule.
        let v = scan(b",RN B(5)");
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].base, "RN");
        assert!(v[0].subscripted);
        assert!(!v[0].is_plain_numeric_scalar());
    }

    #[test]
    fn arrays_and_fn_calls_are_flagged_not_substitutable() {
        let v = scan(b",P(3)");
        assert_eq!(v.len(), 1);
        assert!(v[0].subscripted);
        assert!(!v[0].is_plain_numeric_scalar());

        let f = scan(b",\xA5F(2)");
        assert_eq!(f.len(), 1);
        assert!(f[0].is_fn);
        assert!(!f[0].is_plain_numeric_scalar());
    }

    #[test]
    fn several_references_keep_their_offsets() {
        let s = b",X,Y%,Z";
        let v = scan(s);
        assert_eq!(v.len(), 3);
        for r in &v {
            assert_eq!(
                &s[r.start..r.start + r.len],
                match r.base.as_str() {
                    "X" => &b"X"[..],
                    "Y" => &b"Y%"[..],
                    _ => &b"Z"[..],
                }
            );
        }
    }
}
