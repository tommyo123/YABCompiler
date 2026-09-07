//! Shared optimisation thresholds and lightweight decision bookkeeping.
//!
//! The first version is intentionally small: it centralises the magic
//! numbers that were spread across codegen, IR passes, and peephole
//! factoring. Later passes can hang richer byte/cycle estimates and
//! per-program statistics from the same place without changing every
//! caller.

#![allow(dead_code)]

use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum OptDecision {
    FloatLoadStub,
    FloatStoreStub,
    IntLoadStub,
    FacOpStub,
    ArrayPtrInduction,
    LoopInduction,
    Ph301Sequence,
}

#[derive(Debug, Default, Clone)]
pub struct OptStats {
    accepted: BTreeMap<OptDecision, u32>,
    rejected: BTreeMap<OptDecision, u32>,
}

impl OptStats {
    pub fn note(&mut self, decision: OptDecision, accepted: bool) {
        let map = if accepted {
            &mut self.accepted
        } else {
            &mut self.rejected
        };
        *map.entry(decision).or_insert(0) += 1;
    }

    pub fn accepted(&self, decision: OptDecision) -> u32 {
        self.accepted.get(&decision).copied().unwrap_or(0)
    }

    pub fn rejected(&self, decision: OptDecision) -> u32 {
        self.rejected.get(&decision).copied().unwrap_or(0)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CostModel;

impl CostModel {
    pub const fn default() -> Self {
        Self
    }

    pub fn float_load_stub_worth_it(self, count: u32) -> bool {
        count >= FLOAT_LOAD_STUB_MIN_USES
    }

    pub fn float_store_stub_worth_it(self, count: u32) -> bool {
        count >= FLOAT_STORE_STUB_MIN_USES
    }

    pub fn int_load_stub_worth_it(self, count: u32) -> bool {
        count >= INT_LOAD_STUB_MIN_USES
    }

    pub fn fac_op_stub_worth_it(self, count: u32) -> bool {
        count >= FAC_OP_STUB_MIN_USES
    }

    pub fn array_ptr_induction_worth_it(self, access_count: u32) -> bool {
        access_count >= ARRAY_PTR_INDUCTION_MIN_USES
    }

    pub fn loop_induction_worth_it(self, k: f64, step: f64) -> bool {
        if !k.is_finite() || !step.is_finite() {
            return false;
        }
        if k == 0.0 || k == 1.0 || k == -1.0 {
            return false;
        }
        if k.fract() == 0.0 && k.abs() <= 65536.0 {
            let abs_int = k.abs() as u32;
            if abs_int.is_power_of_two() {
                return false;
            }
        }
        // Correctness gate.
        let advance = k * step;
        if advance == 0.0 {

            return true;
        }

        match mflpt_mantissa_trailing_zeros(advance) {
            Some(tz) if tz >= INDUCTION_MIN_TRAILING_ZEROS => {}
            _ => return false,
        }

        match mflpt_mantissa_trailing_zeros(step) {
            Some(tz) => tz >= INDUCTION_MIN_TRAILING_ZEROS,
            None => false,
        }
    }

    pub fn ph301_min_sites(self) -> usize {
        PH301_MIN_SITES
    }

    pub fn ph301_late_jsr_budget(self) -> usize {
        PH301_LATE_JSR_BUDGET
    }
}

pub fn mflpt_mantissa_trailing_zeros(v: f64) -> Option<u32> {
    if v == 0.0 || !v.is_finite() {
        return None;
    }
    let bits = v.abs().to_bits();
    let raw_exp = ((bits >> 52) & 0x7FF) as i32;
    if raw_exp == 0 {

        return None;
    }

    let sig = (1u64 << 52) | (bits & ((1u64 << 52) - 1));
    // The C64 keeps 32 mantissa bits, so drop the low 21, rounding half up exactly as the ROM's own narrowing does.
    let mut m = (sig + (1 << 20)) >> 21;
    let mut e = raw_exp - 894;
    if m == 1u64 << 32 {
        m >>= 1;
        e += 1;
    }

    if !(1..=255).contains(&e) {
        return None;
    }
    Some((m as u32).trailing_zeros())
}

/// How much headroom the induction increment must keep.
pub const INDUCTION_MIN_TRAILING_ZEROS: u32 = 16;

pub const FLOAT_LOAD_STUB_MIN_USES: u32 = 3;
pub const FLOAT_STORE_STUB_MIN_USES: u32 = 3;
pub const INT_LOAD_STUB_MIN_USES: u32 = 3;
pub const FAC_OP_STUB_MIN_USES: u32 = 2;
pub const ARRAY_PTR_INDUCTION_MIN_USES: u32 = 5;
pub const PH301_MIN_SITES: usize = 4;
pub const PH301_LATE_JSR_BUDGET: usize = 1;
