//! Integer-island expression typing.
//!
//! Reference predicates kept around as documentation for the
//! int-safe semantics codegen and dataflow rely on; the production
//! call sites use their own per-pass classifiers
//! (`Codegen::is_int_island_addsub_only` and `passes::int_stayable`).
//! The module-wide `dead_code` allowance keeps these documented
//! variants compiling without warnings.
//!
//! Classifies an `Expr` as "int16-safe" (the whole subtree can be
//! evaluated using native 6502 16-bit integer arithmetic, no FAC
//! roundtrip) or not. Used by codegen's `try_emit_int16_*` paths to
//! decide when to lower an expression natively.

#![allow(dead_code)]

//!
//! Semantic match with BASIC v2: integer ops accept overflow with
//! wraparound (matching the existing inline `LET X% = A + B` path).
//! When real BASIC would error on overflow, we produce a different
//! error (or silent wrap) — but this matches existing intent for
//! programs that explicitly use `%`-typed vars and is the cost of
//! using the int-fast path. A future range analysis can tighten the
//! safety predicate if needed.
//!
//! The complement of this analysis lives in `passes::int_stayable`
//! (used for var-level promotion). The two predicates differ slightly
//! because they answer different questions:
//!
//!   * `int_stayable` — given a candidate set of vars-to-promote, does
//!     this expression keep its result in int16? (Pre-promotion query.)
//!   * `is_int_island` — given the IR as it stands, is every var,
//!     constant, and operation in this expression already in the int
//!     domain? (Post-promotion query, run at codegen time.)

use crate::ast::{BinOp, Func1, VarKind};
use crate::ir::{Expr, StrExpr};

/// True iff `e` can be evaluated entirely in int16 domain using
/// only 6502 native arithmetic (no FAC). Conservative: every leaf
/// must be int16-typed, every operator must preserve int16 with
/// silent wraparound, and every helper call must return an int16.
///
/// Operations classified as int-safe:
///   * `Number(n)` for integral `n` in `[-32768, 32767]`
///   * `Var(v)` where `v.kind == Integer`
///   * `Neg(int)`, `Not(int)`
///   * `Bin(Add|Sub|Mul, int, int)` — wraparound matches our existing
///     int LET fast path
///   * `Bin(And|Or, int, int)` — bitwise on i16 is int
///   * Comparisons `Bin(Eq|Ne|Lt|Le|Gt|Ge, int, int)` — produce -1/0,
///     fits in int16
///   * `Func1(Abs|Int|Sgn, int)` — preserves int16
///   * `Peek(addr)` — returns 0..255, always int
///   * `Asc/Len/Pos/Fre` — return small ints
///   * `ArrayRef(int_array, int_indices...)`
///
/// Refused:
///   * `Bin(Div|Pow, …)` — fractional results possible
///   * `Func1(Sqr|Log|Exp|Sin|Cos|Tan|Atn|Rnd, …)` — float-only
///   * `FnCall`, `Usr` — opaque
///   * `Val` — float result
///   * `String`, `StrCompare(…)` — wrong domain (compare returns -1/0
///     which IS int, but we still flag it Float here because the
///     pattern is rare and the FAC path is already optimised for
///     string compare).
pub fn is_int_island(e: &Expr) -> bool {
    match e {
        Expr::Number(n) => n.is_finite() && n.fract() == 0.0 && (-32768.0..=32767.0).contains(n),
        Expr::Var(v) => v.kind == VarKind::Integer && v.base != "TI" && v.base != "ST",
        Expr::String(_) => false,
        Expr::Neg(inner) | Expr::Not(inner) => is_int_island(inner),
        Expr::Bin(op, l, r) => is_int_safe_binop(*op) && is_int_island(l) && is_int_island(r),
        Expr::Func1(f, arg) => match f {
            Func1::Abs | Func1::Int | Func1::Sgn => is_int_island(arg),
            _ => false,
        },
        Expr::Peek(addr) | Expr::MemPeek(addr) => is_int_island(addr),
        // ASC raises on empty string; LEN is total bytes (always int).
        Expr::Asc(_) => false,
        Expr::Len(_) => true,
        Expr::Val(_) | Expr::Nrm(_) => false,
        Expr::Pos(_) => true,
        Expr::Fre(_) => true,
        Expr::Usr(_) => false,
        Expr::Joy(_) => false,
        Expr::Pot(_) => false,
        Expr::Inkey => false,
        Expr::Lin => false,
        Expr::At(_, _) => false,
        Expr::Test(_, _) => false,
        Expr::Check { .. } => false,
        Expr::Inst { .. } => false,
        Expr::FnCall(_, _) => false,
        Expr::ArrayRef(name, idx) => name.kind == VarKind::Integer && idx.iter().all(is_int_island),
        Expr::StrCompare(_, _, _) => false,
    }
}

/// Predicate: this BinOp keeps its result in int16 when both operands
/// are int16. Add/Sub/Mul wraparound is treated as semantically valid
/// (matches existing fast path); Div/Pow are not.
fn is_int_safe_binop(op: BinOp) -> bool {
    matches!(
        op,
        BinOp::Add
            | BinOp::Sub
            | BinOp::Mul
            | BinOp::And
            | BinOp::Or
            | BinOp::Xor
            | BinOp::Eq
            | BinOp::Ne
            | BinOp::Lt
            | BinOp::Le
            | BinOp::Gt
            | BinOp::Ge
    )
}

/// Stricter variant: the expression is int-safe AND avoids
/// multiplication. Useful for codegen paths that don't have a 16-bit
/// MUL helper available — addition and subtraction stay native, but
/// `*` falls back to FAC.
pub fn is_int_island_no_mul(e: &Expr) -> bool {
    if !is_int_island(e) {
        return false;
    }
    match e {
        Expr::Bin(BinOp::Mul, _, _) => false,
        Expr::Bin(_, l, r) => is_int_island_no_mul(l) && is_int_island_no_mul(r),
        Expr::Neg(inner) | Expr::Not(inner) => is_int_island_no_mul(inner),
        Expr::Func1(_, arg) => is_int_island_no_mul(arg),
        Expr::Peek(addr) | Expr::MemPeek(addr) => is_int_island_no_mul(addr),
        Expr::Pot(addr) => is_int_island_no_mul(addr),
        Expr::At(row, col) => is_int_island_no_mul(row) && is_int_island_no_mul(col),
        Expr::Test(x, y) => is_int_island_no_mul(x) && is_int_island_no_mul(y),
        Expr::Check { first, second } => {
            is_int_island_no_mul(first) && second.as_ref().is_none_or(|e| is_int_island_no_mul(e))
        }
        Expr::Inst { start, .. } => start.as_ref().is_none_or(|e| is_int_island_no_mul(e)),
        Expr::ArrayRef(_, idx) => idx.iter().all(is_int_island_no_mul),
        _ => true,
    }
}

/// True if `s` is a string-domain expression that can't be lowered
/// natively. Counterpart predicate so callers can skip int-island
/// classification when they're already in the string domain.
#[allow(dead_code)]
pub fn is_string_expr(_s: &StrExpr) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::VarName;
    use crate::ir::Expr;

    fn ivar(name: &str) -> VarName {
        VarName {
            base: name.to_string(),
            kind: VarKind::Integer,
        }
    }
    fn fvar(name: &str) -> VarName {
        VarName {
            base: name.to_string(),
            kind: VarKind::Float,
        }
    }

    #[test]
    fn int_const_is_island() {
        assert!(is_int_island(&Expr::Number(42.0)));
        assert!(is_int_island(&Expr::Number(-32768.0)));
        assert!(is_int_island(&Expr::Number(32767.0)));
    }

    #[test]
    fn out_of_range_is_not_island() {
        assert!(!is_int_island(&Expr::Number(40000.0)));
        assert!(!is_int_island(&Expr::Number(3.5)));
    }

    #[test]
    fn int_var_is_island() {
        assert!(is_int_island(&Expr::Var(ivar("X"))));
    }

    #[test]
    fn float_var_is_not_island() {
        assert!(!is_int_island(&Expr::Var(fvar("X"))));
    }

    #[test]
    fn nested_add_of_ints_is_island() {
        // (X% + Y%) + Z%
        let inner = Expr::Bin(
            BinOp::Add,
            Box::new(Expr::Var(ivar("X"))),
            Box::new(Expr::Var(ivar("Y"))),
        );
        let outer = Expr::Bin(BinOp::Add, Box::new(inner), Box::new(Expr::Var(ivar("Z"))));
        assert!(is_int_island(&outer));
    }

    #[test]
    fn float_taints_island() {
        // (X% + F) — F is float
        let e = Expr::Bin(
            BinOp::Add,
            Box::new(Expr::Var(ivar("X"))),
            Box::new(Expr::Var(fvar("F"))),
        );
        assert!(!is_int_island(&e));
    }

    #[test]
    fn div_taints_island() {
        let e = Expr::Bin(
            BinOp::Div,
            Box::new(Expr::Var(ivar("X"))),
            Box::new(Expr::Number(2.0)),
        );
        assert!(!is_int_island(&e));
    }

    #[test]
    fn no_mul_variant() {
        let mul = Expr::Bin(
            BinOp::Mul,
            Box::new(Expr::Var(ivar("X"))),
            Box::new(Expr::Number(2.0)),
        );
        assert!(is_int_island(&mul));
        assert!(!is_int_island_no_mul(&mul));
    }

    #[test]
    fn peek_is_int() {
        let e = Expr::Peek(Box::new(Expr::Number(53280.0)));
        assert!(!is_int_island(&e), "53280 not in i16 range");
        let e2 = Expr::Peek(Box::new(Expr::Number(1024.0)));
        assert!(is_int_island(&e2));
    }
}
