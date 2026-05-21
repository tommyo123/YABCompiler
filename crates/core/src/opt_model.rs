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

    pub fn loop_induction_worth_it(self, k: f64) -> bool {
        if !k.is_finite() {
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
        true
    }

    pub fn ph301_min_sites(self) -> usize {
        PH301_MIN_SITES
    }

    pub fn ph301_late_jsr_budget(self) -> usize {
        PH301_LATE_JSR_BUDGET
    }
}

pub const FLOAT_LOAD_STUB_MIN_USES: u32 = 3;
pub const FLOAT_STORE_STUB_MIN_USES: u32 = 3;
pub const INT_LOAD_STUB_MIN_USES: u32 = 3;
pub const FAC_OP_STUB_MIN_USES: u32 = 2;
pub const ARRAY_PTR_INDUCTION_MIN_USES: u32 = 5;
pub const PH301_MIN_SITES: usize = 4;
pub const PH301_LATE_JSR_BUDGET: usize = 1;
