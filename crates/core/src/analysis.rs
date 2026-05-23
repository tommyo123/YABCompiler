//! Analysis registry — caching and dependency-tracking layer for IR
//! analyses.
//!
//! An `Analysis` is a function `Module → Output` that may itself
//! depend on other analyses. The `Registry` caches results so each
//! analysis runs at most once per IR snapshot, and serves as the
//! dependency hub: an analysis calls `deps.get(&AnotherAnalysis)`
//! to pull a cached upstream result and the registry computes it
//! on demand.
//!
//! Optimisation passes that mutate the IR call `registry.invalidate()`
//! when they're done so subsequent passes recompute against the new
//! state. (We don't yet do per-analysis dependency tracking — coarse
//! invalidation is plenty for our pass count.)

#![allow(dead_code)]

use std::any::{Any, TypeId};
use std::collections::{HashMap, HashSet};

use crate::ast::{BinOp, VarKind, VarName};
use crate::cfg::StmtPath;
use crate::ir::{Expr, Module, PrintPiece, ReadTarget, Stmt, StrExpr, ThenIr};
use crate::visit::{Visitor, walk_stmt};

/// An analysis that produces a value of type `Output` from a module.
/// Implement on a zero-sized type:
///
/// ```ignore
/// pub struct MyAnalysis;
/// impl Analysis for MyAnalysis {
///     type Output = HashMap<VarName, u32>;
///     fn name(&self) -> &'static str { "my-analysis" }
///     fn run(&self, m: &Module, deps: &mut Registry) -> Self::Output {
///         let upstream = deps.get(m, &SomeOtherAnalysis).clone();
///         /* ... */
///     }
/// }
///
/// // Usage:
/// let result = registry.get(module, &MyAnalysis);
/// ```
#[allow(dead_code)] // wired up; first consumer lands with integer-island opts
pub trait Analysis: 'static {
    type Output: 'static;
    fn name(&self) -> &'static str;
    fn run(&self, module: &Module, deps: &mut Registry) -> Self::Output;
}

/// Cached analysis results. Keyed by `TypeId` of the analysis type
/// itself — analyses are zero-sized so the type identifies the
/// computation uniquely.
pub struct Registry {
    cache: HashMap<TypeId, Box<dyn Any>>,
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

impl Registry {
    pub fn new() -> Self {
        Self {
            cache: HashMap::new(),
        }
    }

    /// Get the cached result of `analysis`, computing it if needed.
    /// Recursive: an analysis's `run` may call `get` on others.
    #[allow(dead_code)] // wired up; first consumer lands with integer-island opts
    pub fn get<A: Analysis>(&mut self, module: &Module, analysis: &A) -> &A::Output {
        let id = TypeId::of::<A>();
        if !self.cache.contains_key(&id) {
            let output = analysis.run(module, self);
            self.cache.insert(id, Box::new(output));
        }
        self.cache
            .get(&id)
            .expect("just inserted")
            .downcast_ref::<A::Output>()
            .expect("analysis output type matches its TypeId")
    }

    /// Drop every cached result. Call after any IR mutation so the
    /// next `get` recomputes against the new state.
    pub fn invalidate(&mut self) {
        self.cache.clear();
    }
}

// ===== Built-in analyses =====

/// Per-variable read/write counts. Reads count any expression-level
/// `Var(v)` reference; writes count anything that assigns the slot
/// (LET, FOR header, INPUT/READ/GET targets). Useful as a building
/// block for dead-var elimination, integer-promotability, etc.
#[allow(dead_code)] // first analysis published; consumers in next phases
#[derive(Debug, Default, Clone)]
pub struct VarStats {
    pub reads: u32,
    pub let_writes: u32,
    pub other_writes: u32,
}

#[allow(dead_code)]
pub struct VarUsage;

impl Analysis for VarUsage {
    type Output = HashMap<VarName, VarStats>;
    fn name(&self) -> &'static str {
        "var-usage"
    }
    fn run(&self, module: &Module, _: &mut Registry) -> Self::Output {
        let mut counter = VarUsageCounter {
            stats: HashMap::new(),
        };
        crate::visit::walk_module(&mut counter, module);
        counter.stats
    }
}

struct VarUsageCounter {
    stats: HashMap<VarName, VarStats>,
}

impl Visitor for VarUsageCounter {
    fn visit_var_read(&mut self, v: &VarName) {
        self.stats.entry(v.clone()).or_default().reads += 1;
    }

    fn visit_stmt(&mut self, line_no: u16, stmt: &Stmt) {
        match stmt {
            Stmt::Let { var, .. } | Stmt::LetStr { var, .. } => {
                self.stats.entry(var.clone()).or_default().let_writes += 1;
            }
            Stmt::For { var, .. } => {
                self.stats.entry(var.clone()).or_default().other_writes += 1;
            }
            Stmt::Get { var } => {
                self.stats.entry(var.clone()).or_default().other_writes += 1;
            }
            Stmt::GetFile { vars, .. } => {
                for v in vars {
                    self.stats.entry(v.clone()).or_default().other_writes += 1;
                }
            }
            Stmt::Read(targets) | Stmt::Input { targets, .. } => {
                for t in targets {
                    if let crate::ir::ReadTarget::Scalar(v) = t {
                        self.stats.entry(v.clone()).or_default().other_writes += 1;
                    }
                }
            }
            Stmt::InputFile { targets, .. } => {
                for t in targets {
                    if let crate::ir::ReadTarget::Scalar(v) = t {
                        self.stats.entry(v.clone()).or_default().other_writes += 1;
                    }
                }
            }
            _ => {}
        }
        walk_stmt(self, line_no, stmt);
    }
}

/// Discriminator for "this var is a static numeric constant" — set
/// exactly once via a top-level LET to a numeric literal AND never
/// touched by INPUT/READ/GET/FOR/etc. Used by ConstVarProp to decide
/// what to inline. (Currently the existing ConstVarProp pass does
/// its own walk; this wraps that knowledge in an analysis so other
/// passes can query it without re-walking.)
#[allow(dead_code)]
pub struct ConstScalarVars;

impl Analysis for ConstScalarVars {
    type Output = HashMap<VarName, f64>;
    fn name(&self) -> &'static str {
        "const-scalar-vars"
    }
    fn run(&self, module: &Module, deps: &mut Registry) -> Self::Output {
        if module_has_clr(module) {
            return HashMap::new();
        }
        let usage = deps.get(module, &VarUsage).clone();
        // Filter VarUsage down to single-LET, no other writes, with
        // a numeric-literal RHS. The RHS check needs a second walk
        // since VarUsage doesn't track values.
        let mut candidate_values: HashMap<VarName, Option<f64>> = HashMap::new();
        let mut collector = LetLiteralCollector {
            values: &mut candidate_values,
        };
        crate::visit::walk_module(&mut collector, module);

        let mut out = HashMap::new();
        for (var, stats) in &usage {
            if var.kind == VarKind::String {
                continue;
            }
            if stats.let_writes != 1 {
                continue;
            }
            if stats.other_writes != 0 {
                continue;
            }
            if let Some(Some(value)) = candidate_values.get(var) {
                if value.is_finite() {
                    out.insert(var.clone(), *value);
                }
            }
        }
        out
    }
}

fn module_has_clr(module: &Module) -> bool {
    fn stmts_have_clr(stmts: &[Stmt]) -> bool {
        stmts.iter().any(|stmt| match stmt {
            Stmt::Clr => true,
            Stmt::If {
                then: ThenIr::Stmts(inner),
                ..
            } => stmts_have_clr(inner),
            Stmt::IfElse {
                then, else_then, ..
            } => then_has_clr(then) || then_has_clr(else_then),
            Stmt::Rcomp { then, else_then } => {
                then_has_clr(then) || else_then.as_ref().map_or(false, then_has_clr)
            }
            _ => false,
        })
    }
    fn then_has_clr(then: &ThenIr) -> bool {
        matches!(then, ThenIr::Stmts(inner) if stmts_have_clr(inner))
    }
    module.lines.iter().any(|line| stmts_have_clr(&line.stmts))
}

struct LetLiteralCollector<'a> {
    values: &'a mut HashMap<VarName, Option<f64>>,
}

impl<'a> Visitor for LetLiteralCollector<'a> {
    fn visit_stmt(&mut self, line_no: u16, stmt: &Stmt) {
        if let Stmt::Let { var, value } = stmt {
            let lit = match value {
                crate::ir::Expr::Number(n) if n.is_finite() => Some(*n),
                crate::ir::Expr::Neg(inner) => match inner.as_ref() {
                    crate::ir::Expr::Number(n) if n.is_finite() => Some(-n),
                    _ => None,
                },
                _ => None,
            };
            // First time: record. Second time: poison (None) so the
            // VarUsage filter still requires let_writes == 1 anyway.
            self.values
                .entry(var.clone())
                .and_modify(|e| *e = None)
                .or_insert(lit);
        }
        walk_stmt(self, line_no, stmt);
    }
}

// ===== Effect summaries =====================================================

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EffectRegion {
    ScalarFloat(VarName),
    ScalarInteger(VarName),
    ScalarString(VarName),
    ArrayMeta(VarName),
    ArrayData(VarName),
    DataPtr,
    Heap,
    IoState,
    SysMemKnown(u16),
    SysMemUnknown,
    ProgramState,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct EffectSummary {
    pub reads: HashSet<EffectRegion>,
    pub writes: HashSet<EffectRegion>,
    pub opaque_read: bool,
    pub opaque_write: bool,
    pub control_flow: bool,
    pub may_allocate: bool,
    pub may_raise: bool,
}

impl EffectSummary {
    pub fn reads_region(&self, region: &EffectRegion) -> bool {
        self.reads.contains(region) || self.opaque_read
    }

    pub fn writes_region(&self, region: &EffectRegion) -> bool {
        self.writes.contains(region) || self.opaque_write
    }

    fn read_scalar(&mut self, var: &VarName) {
        self.reads.insert(scalar_region(var));
    }

    fn write_scalar(&mut self, var: &VarName) {
        self.writes.insert(scalar_region(var));
    }

    fn read_array(&mut self, name: &VarName) {
        self.reads.insert(EffectRegion::ArrayMeta(name.clone()));
        self.reads.insert(EffectRegion::ArrayData(name.clone()));
    }

    fn write_array_data(&mut self, name: &VarName) {
        self.reads.insert(EffectRegion::ArrayMeta(name.clone()));
        self.writes.insert(EffectRegion::ArrayData(name.clone()));
    }
}

#[allow(dead_code)]
#[derive(Debug, Default, Clone)]
pub struct EffectSummaryResult {
    pub per_node: Vec<EffectSummary>,
    pub per_path: HashMap<StmtPath, EffectSummary>,
}

impl EffectSummaryResult {
    pub fn summary_for_node(&self, node: usize) -> Option<&EffectSummary> {
        self.per_node.get(node)
    }

    #[allow(dead_code)]
    pub fn summary_for_path(&self, path: &StmtPath) -> Option<&EffectSummary> {
        self.per_path.get(path)
    }
}

pub struct EffectSummaryAnalysis;

impl Analysis for EffectSummaryAnalysis {
    type Output = EffectSummaryResult;
    fn name(&self) -> &'static str {
        "effect-summary"
    }
    fn run(&self, module: &Module, deps: &mut Registry) -> Self::Output {
        let cfg = deps.get(module, &crate::cfg::CfgBuild).clone();
        let mut per_node = Vec::with_capacity(cfg.nodes.len());
        let mut per_path = HashMap::new();
        for (node_id, node) in cfg.nodes.iter().enumerate() {
            let stmt = cfg.stmt_at(node_id, module);
            let summary = summarize_stmt_effect(stmt);
            per_path.insert(node.stmt.clone(), summary.clone());
            per_node.push(summary);
        }
        EffectSummaryResult { per_node, per_path }
    }
}

pub fn effect_summary_for_stmt(stmt: &Stmt) -> EffectSummary {
    summarize_stmt_effect(stmt)
}

fn scalar_region(var: &VarName) -> EffectRegion {
    match var.kind {
        VarKind::Float => EffectRegion::ScalarFloat(var.clone()),
        VarKind::Integer => EffectRegion::ScalarInteger(var.clone()),
        VarKind::String => EffectRegion::ScalarString(var.clone()),
    }
}

fn summarize_stmt_effect(stmt: &Stmt) -> EffectSummary {
    let mut out = EffectSummary::default();
    match stmt {
        Stmt::Print { items, .. } | Stmt::PrintFile { items, .. } | Stmt::Cmd { items, .. } => {
            for item in items {
                summarize_print_piece(item, &mut out);
            }
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::Let { var, value } => {
            summarize_expr(value, &mut out);
            out.write_scalar(var);
        }
        Stmt::LetStr { var, value } => {
            summarize_str(value, &mut out);
            out.write_scalar(var);
        }
        Stmt::ArrayLet {
            name,
            indices,
            value,
        } => {
            for idx in indices {
                summarize_expr(idx, &mut out);
            }
            summarize_expr(value, &mut out);
            out.write_array_data(name);
        }
        Stmt::ArrayLetStr {
            name,
            indices,
            value,
        } => {
            for idx in indices {
                summarize_expr(idx, &mut out);
            }
            summarize_str(value, &mut out);
            out.write_array_data(name);
        }
        Stmt::If { cond, then } => {
            summarize_expr(cond, &mut out);
            out.control_flow = true;
            if let ThenIr::Stmts(stmts) = then {
                for s in stmts {
                    out = merge_effects(out, summarize_stmt_effect(s));
                }
            }
        }
        Stmt::IfElse {
            cond,
            then,
            else_then,
        } => {
            summarize_expr(cond, &mut out);
            out.control_flow = true;
            if let ThenIr::Stmts(stmts) = then {
                for s in stmts {
                    out = merge_effects(out, summarize_stmt_effect(s));
                }
            }
            if let ThenIr::Stmts(stmts) = else_then {
                for s in stmts {
                    out = merge_effects(out, summarize_stmt_effect(s));
                }
            }
        }
        Stmt::DoIf { cond } | Stmt::Until { cond } => {
            summarize_expr(cond, &mut out);
            out.control_flow = true;
        }
        Stmt::ExitLoop { cond } => {
            if let Some(cond) = cond {
                summarize_expr(cond, &mut out);
            }
            out.control_flow = true;
        }
        Stmt::ComputedGoto { target } => {
            summarize_expr(target, &mut out);
            out.control_flow = true;
        }
        Stmt::Rcomp { then, else_then } => {
            out.control_flow = true;
            if let ThenIr::Stmts(stmts) = then {
                for s in stmts {
                    out = merge_effects(out, summarize_stmt_effect(s));
                }
            }
            if let Some(ThenIr::Stmts(stmts)) = else_then {
                for s in stmts {
                    out = merge_effects(out, summarize_stmt_effect(s));
                }
            }
        }
        Stmt::For {
            var,
            start,
            end,
            step,
            ..
        } => {
            summarize_expr(start, &mut out);
            summarize_expr(end, &mut out);
            summarize_expr(step, &mut out);
            out.write_scalar(var);
            out.control_flow = true;
        }
        Stmt::Next { vars } => {
            for v in vars.iter().flatten() {
                out.read_scalar(v);
                out.write_scalar(v);
            }
            out.control_flow = true;
        }
        Stmt::Poke { addr, value } => {
            summarize_expr(addr, &mut out);
            summarize_expr(value, &mut out);
            note_sysmem_write(addr, &mut out);
        }
        Stmt::Dpoke { addr, value } => {
            summarize_expr(addr, &mut out);
            summarize_expr(value, &mut out);
            note_sysmem_write(addr, &mut out);
            if let Some(addr) = literal_addr(addr) {
                out.writes
                    .insert(EffectRegion::SysMemKnown(addr.wrapping_add(1)));
                if (0xD000..=0xDFFF).contains(&addr.wrapping_add(1)) {
                    out.writes.insert(EffectRegion::IoState);
                }
            }
        }
        Stmt::PokeFill {
            dst_start,
            dst_end,
            value,
        } => {
            // Effects mirror an unrolled run of Pokes — every byte in
            // [dst_start, dst_end] is written, and we don't know the
            // exact range without constant-folding both endpoints.
            // Conservatively flag the whole sysmem region as written.
            summarize_expr(dst_start, &mut out);
            summarize_expr(dst_end, &mut out);
            summarize_expr(value, &mut out);
            note_sysmem_write(dst_start, &mut out);
            note_sysmem_write(dst_end, &mut out);
        }
        Stmt::ScreenRect {
            row,
            col,
            width,
            height,
            ch,
            color,
            ..
        } => {
            summarize_expr(row, &mut out);
            summarize_expr(col, &mut out);
            summarize_expr(width, &mut out);
            summarize_expr(height, &mut out);
            if let Some(e) = ch {
                summarize_expr(e, &mut out);
            }
            if let Some(e) = color {
                summarize_expr(e, &mut out);
            }
            out.writes.insert(EffectRegion::SysMemUnknown);
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::ScreenMove {
            row,
            col,
            width,
            height,
            dest_row,
            dest_col,
        } => {
            for e in [row, col, width, height, dest_row, dest_col] {
                summarize_expr(e, &mut out);
            }
            out.reads.insert(EffectRegion::SysMemUnknown);
            out.reads.insert(EffectRegion::IoState);
            out.writes.insert(EffectRegion::SysMemUnknown);
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::ScreenScroll {
            row,
            col,
            width,
            height,
            ..
        } => {
            for e in [row, col, width, height] {
                summarize_expr(e, &mut out);
            }
            out.reads.insert(EffectRegion::SysMemUnknown);
            out.reads.insert(EffectRegion::IoState);
            out.writes.insert(EffectRegion::SysMemUnknown);
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::Color {
            border,
            background,
            pen,
        } => {
            for e in border.iter().chain(background.iter()).chain(pen.iter()) {
                summarize_expr(e, &mut out);
            }
            out.writes.insert(EffectRegion::SysMemKnown(0xD020));
            out.writes.insert(EffectRegion::SysMemKnown(0xD021));
            out.writes.insert(EffectRegion::SysMemKnown(0x0286));
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::MobEnable { index, .. } => {
            summarize_expr(index, &mut out);
            out.writes.insert(EffectRegion::SysMemKnown(0xD015));
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::Multi { .. } => {
            out.writes.insert(EffectRegion::SysMemKnown(0xD016));
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::MultiColors { c1, c2, c3 } => {
            summarize_expr(c1, &mut out);
            summarize_expr(c2, &mut out);
            summarize_expr(c3, &mut out);
            // Fills the bitmap screen-matrix ($C000+), colour RAM, and
            // flips $D016 — broad enough that SysMemUnknown is the
            // honest summary.
            out.writes.insert(EffectRegion::SysMemKnown(0xD016));
            out.writes.insert(EffectRegion::SysMemUnknown);
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::Hires { .. } => {
            // HIRES touches VIC mode bits and clears bitmap + screen
            // RAM. Conservative: mark IoState plus the VIC + CIA2
            // mode registers we write so DSE can still drop earlier
            // POKEs to those addresses if they're shadowed.
            out.writes.insert(EffectRegion::SysMemKnown(0xD011));
            out.writes.insert(EffectRegion::SysMemKnown(0xD016));
            out.writes.insert(EffectRegion::SysMemKnown(0xD018));
            out.writes.insert(EffectRegion::SysMemKnown(0xDD00));
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::Border { color } => {
            summarize_expr(color, &mut out);
            out.writes.insert(EffectRegion::SysMemKnown(0xD020));
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::Line {
            x1,
            y1,
            x2,
            y2,
            mode,
        }
        | Stmt::Block {
            x1,
            y1,
            x2,
            y2,
            mode,
        } => {
            for e in [x1, y1, x2, y2] {
                summarize_expr(e, &mut out);
            }
            if let Some(e) = mode {
                summarize_expr(e, &mut out);
            }
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::Rec {
            x,
            y,
            width,
            height,
            mode,
        } => {
            for e in [x, y, width, height] {
                summarize_expr(e, &mut out);
            }
            if let Some(e) = mode {
                summarize_expr(e, &mut out);
            }
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::Draw { x, y, mode } | Stmt::DrawTo { x, y, mode } | Stmt::Paint { x, y, mode } => {
            summarize_expr(x, &mut out);
            summarize_expr(y, &mut out);
            if let Some(e) = mode {
                summarize_expr(e, &mut out);
            }
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::Circle {
            cx,
            cy,
            radius,
            ry,
            start,
            end,
            step,
            mode,
        } => {
            summarize_expr(cx, &mut out);
            summarize_expr(cy, &mut out);
            summarize_expr(radius, &mut out);
            for opt in [ry, start, end, step, mode] {
                if let Some(e) = opt {
                    summarize_expr(e, &mut out);
                }
            }
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::Char {
            x,
            y,
            code,
            mode,
            zoom,
        } => {
            summarize_expr(x, &mut out);
            summarize_expr(y, &mut out);
            summarize_expr(code, &mut out);
            if let Some(e) = mode {
                summarize_expr(e, &mut out);
            }
            if let Some(e) = zoom {
                summarize_expr(e, &mut out);
            }
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::Text {
            x,
            y,
            text,
            mode,
            zoom,
            kerning,
        } => {
            summarize_expr(x, &mut out);
            summarize_expr(y, &mut out);
            summarize_str(text, &mut out);
            if let Some(e) = mode {
                summarize_expr(e, &mut out);
            }
            if let Some(e) = zoom {
                summarize_expr(e, &mut out);
            }
            if let Some(e) = kerning {
                summarize_expr(e, &mut out);
            }
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::Rot { direction, length } => {
            summarize_expr(direction, &mut out);
            if let Some(l) = length {
                summarize_expr(l, &mut out);
            }
        }
        Stmt::DrawString { code, x, y, mode } => {
            summarize_str(code, &mut out);
            summarize_expr(x, &mut out);
            summarize_expr(y, &mut out);
            if let Some(e) = mode {
                summarize_expr(e, &mut out);
            }
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::Angl {
            cx,
            cy,
            angle,
            rx,
            ry,
            mode,
        } => {
            for e in [cx, cy, angle, rx] {
                summarize_expr(e, &mut out);
            }
            for opt in [ry, mode] {
                if let Some(e) = opt {
                    summarize_expr(e, &mut out);
                }
            }
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::Sound { voice, freq } => {
            summarize_expr(voice, &mut out);
            summarize_expr(freq, &mut out);
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::Envelope {
            voice,
            attack,
            decay,
            sustain,
            release,
        } => {
            for e in [voice, attack, decay, sustain, release] {
                summarize_expr(e, &mut out);
            }
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::Wave {
            voice,
            control,
            pulse,
        } => {
            summarize_expr(voice, &mut out);
            summarize_expr(control, &mut out);
            if let Some(e) = pulse {
                summarize_expr(e, &mut out);
            }
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::Music { tempo, tune } => {
            summarize_expr(tempo, &mut out);
            summarize_str(tune, &mut out);
            out.writes.insert(EffectRegion::ProgramState);
        }
        Stmt::Play { mode } => {
            summarize_expr(mode, &mut out);
            out.reads.insert(EffectRegion::ProgramState);
            out.writes.insert(EffectRegion::ProgramState);
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::Flash {
            speed,
            color1,
            color2,
            ..
        }
        | Stmt::Bflash {
            speed,
            color1,
            color2,
            ..
        } => {
            for e in speed.iter().chain(color1.iter()).chain(color2.iter()) {
                summarize_expr(e, &mut out);
            }
            out.writes.insert(EffectRegion::ProgramState);
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::HiCol => {
            out.writes.insert(EffectRegion::ProgramState);
        }
        Stmt::LowCol {
            color1,
            color2,
            color3,
        } => {
            summarize_expr(color1, &mut out);
            summarize_expr(color2, &mut out);
            if let Some(e) = color3 {
                summarize_expr(e, &mut out);
            }
            out.writes.insert(EffectRegion::ProgramState);
        }
        Stmt::Mod { ink, paper } => {
            summarize_expr(ink, &mut out);
            summarize_expr(paper, &mut out);
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::Dup {
            src_x,
            src_y,
            width,
            height,
            dst_x,
            dst_y,
            mode,
            zoom,
        } => {
            for e in [src_x, src_y, width, height, dst_x, dst_y] {
                summarize_expr(e, &mut out);
            }
            for opt in [mode, zoom] {
                if let Some(e) = opt {
                    summarize_expr(e, &mut out);
                }
            }
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::Copy { src, dst, len } => {
            summarize_expr(src, &mut out);
            summarize_expr(dst, &mut out);
            summarize_expr(len, &mut out);
            out.reads.insert(EffectRegion::SysMemUnknown);
            out.writes.insert(EffectRegion::SysMemUnknown);
        }
        Stmt::ScrSave { addr, mode } | Stmt::ScrLoad { addr, mode } => {
            if let Some(e) = addr {
                summarize_expr(e, &mut out);
            }
            if let Some(e) = mode {
                summarize_expr(e, &mut out);
            }
            out.reads.insert(EffectRegion::SysMemUnknown);
            out.writes.insert(EffectRegion::SysMemUnknown);
            out.reads.insert(EffectRegion::IoState);
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::ScrDef { addr, mode, .. } => {
            summarize_expr(addr, &mut out);
            if let Some(e) = mode {
                summarize_expr(e, &mut out);
            }
            out.writes.insert(EffectRegion::SysMemUnknown);
        }
        Stmt::ScrRestore { .. } => {
            out.writes.insert(EffectRegion::SysMemUnknown);
        }
        Stmt::MemClr { addr, len, value } => {
            summarize_expr(addr, &mut out);
            summarize_expr(len, &mut out);
            if let Some(e) = value {
                summarize_expr(e, &mut out);
            }
            out.writes.insert(EffectRegion::SysMemUnknown);
        }
        Stmt::MemTransfer { .. } => {
            out.reads.insert(EffectRegion::IoState);
            out.writes.insert(EffectRegion::IoState);
            out.reads.insert(EffectRegion::SysMemUnknown);
            out.writes.insert(EffectRegion::SysMemUnknown);
        }
        Stmt::MemDef {
            len,
            c64_addr,
            reu_addr,
            reu_bank,
            auto_inc,
            fixed,
        } => {
            summarize_expr(len, &mut out);
            for e in [c64_addr, reu_addr, reu_bank, auto_inc, fixed]
                .into_iter()
                .flatten()
            {
                summarize_expr(e, &mut out);
            }
            out.reads.insert(EffectRegion::IoState);
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::MemLen { len } => {
            summarize_expr(len, &mut out);
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::MemC64Addr { addr } => {
            summarize_expr(addr, &mut out);
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::MemReuPos { addr, bank } => {
            summarize_expr(addr, &mut out);
            summarize_expr(bank, &mut out);
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::MemRestore { auto_inc } => {
            summarize_expr(auto_inc, &mut out);
            out.reads.insert(EffectRegion::IoState);
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::MemCont { mode } => {
            summarize_expr(mode, &mut out);
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::Design { addr, bytes } => {
            summarize_expr(addr, &mut out);
            for e in bytes {
                summarize_expr(e, &mut out);
            }
            out.writes.insert(EffectRegion::SysMemUnknown);
        }
        Stmt::Mmob { index, x, y } => {
            summarize_expr(index, &mut out);
            summarize_expr(x, &mut out);
            summarize_expr(y, &mut out);
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::MmobGlide {
            index,
            sx,
            sy,
            ex,
            ey,
            size,
            speed,
        } => {
            for e in [index, sx, sy, ex, ey] {
                summarize_expr(e, &mut out);
            }
            if let Some(e) = size {
                summarize_expr(e, &mut out);
            }
            if let Some(e) = speed {
                summarize_expr(e, &mut out);
            }
            // Glide reads the VIC pos shadow it maintains and writes
            // $D000-$D010 every step.
            out.reads.insert(EffectRegion::IoState);
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::MobSet {
            index,
            block,
            color,
            priority,
            multicolor,
            size,
            speed,
        } => {
            for e in [index, block, color, priority, multicolor] {
                summarize_expr(e, &mut out);
            }
            if let Some(e) = size {
                summarize_expr(e, &mut out);
            }
            if let Some(e) = speed {
                summarize_expr(e, &mut out);
            }
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::Rlocmob {
            index,
            dx,
            dy,
            speed,
        } => {
            summarize_expr(index, &mut out);
            summarize_expr(dx, &mut out);
            summarize_expr(dy, &mut out);
            if let Some(e) = speed {
                summarize_expr(e, &mut out);
            }
            out.reads.insert(EffectRegion::IoState);
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::Detect { mode } => {
            summarize_expr(mode, &mut out);
            out.writes.insert(EffectRegion::ProgramState);
        }
        Stmt::Cmob { color1, color2 } => {
            summarize_expr(color1, &mut out);
            summarize_expr(color2, &mut out);
            out.writes.insert(EffectRegion::SysMemKnown(0xD025));
            out.writes.insert(EffectRegion::SysMemKnown(0xD026));
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::Bckgnds {
            color0,
            color1,
            color2,
            color3,
        } => {
            summarize_expr(color0, &mut out);
            summarize_expr(color1, &mut out);
            summarize_expr(color2, &mut out);
            summarize_expr(color3, &mut out);
            out.writes.insert(EffectRegion::SysMemKnown(0xD021));
            out.writes.insert(EffectRegion::SysMemKnown(0xD022));
            out.writes.insert(EffectRegion::SysMemKnown(0xD023));
            out.writes.insert(EffectRegion::SysMemKnown(0xD024));
            out.writes.insert(EffectRegion::SysMemKnown(0xD011));
            out.writes.insert(EffectRegion::SysMemKnown(0xD016));
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::Nrm | Stmt::MemModeOn => {
            out.writes.insert(EffectRegion::SysMemKnown(0xDD00));
            out.writes.insert(EffectRegion::SysMemKnown(0xD011));
            out.writes.insert(EffectRegion::SysMemKnown(0xD016));
            out.writes.insert(EffectRegion::SysMemKnown(0xD018));
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::Cset { mode } => {
            summarize_expr(mode, &mut out);
            out.writes.insert(EffectRegion::SysMemKnown(0xD018));
            out.writes.insert(EffectRegion::SysMemKnown(0xD011));
            out.writes.insert(EffectRegion::SysMemKnown(0xDD00));
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::Pause { message, ticks } => {
            if let Some(m) = message {
                summarize_str(m, &mut out);
            }
            summarize_expr(ticks, &mut out);
            out.reads.insert(EffectRegion::IoState);
            if message.is_some() {
                out.writes.insert(EffectRegion::IoState);
            }
        }
        Stmt::Sys { addr, regs, .. } => {
            summarize_expr(addr, &mut out);
            for r in regs {
                summarize_expr(r, &mut out);
            }
            out.opaque_read = true;
            out.opaque_write = true;
            out.control_flow = true;
        }
        Stmt::Wait { addr, mask, eor } => {
            summarize_expr(addr, &mut out);
            summarize_expr(mask, &mut out);
            if let Some(e) = eor {
                summarize_expr(e, &mut out);
            }
            note_sysmem_read(addr, &mut out);
            out.reads.insert(EffectRegion::IoState);
        }
        Stmt::Read(targets) => {
            out.reads.insert(EffectRegion::DataPtr);
            out.writes.insert(EffectRegion::DataPtr);
            for target in targets {
                summarize_read_target_write(target, &mut out);
            }
        }
        Stmt::Restore | Stmt::Reset { .. } => {
            out.writes.insert(EffectRegion::DataPtr);
        }
        Stmt::Input { targets, .. } => {
            out.reads.insert(EffectRegion::IoState);
            out.writes.insert(EffectRegion::IoState);
            out.may_allocate = true;
            for target in targets {
                summarize_read_target_write(target, &mut out);
            }
        }
        Stmt::InputFile { file_num, targets } => {
            summarize_expr(file_num, &mut out);
            out.reads.insert(EffectRegion::IoState);
            out.writes.insert(EffectRegion::IoState);
            out.may_allocate = true;
            for target in targets {
                summarize_read_target_write(target, &mut out);
            }
        }
        Stmt::Get { var } | Stmt::KeyGet { var } => {
            out.reads.insert(EffectRegion::IoState);
            out.writes.insert(EffectRegion::IoState);
            out.write_scalar(var);
            if var.kind == VarKind::String {
                out.may_allocate = true;
            }
        }
        Stmt::GetFile { vars, .. } => {
            out.reads.insert(EffectRegion::IoState);
            out.writes.insert(EffectRegion::IoState);
            for v in vars {
                out.write_scalar(v);
                if v.kind == VarKind::String {
                    out.may_allocate = true;
                }
            }
        }
        Stmt::Fetch {
            control,
            max_len,
            target,
            target_indices,
            force,
            position,
        } => {
            summarize_str(control, &mut out);
            summarize_expr(max_len, &mut out);
            for e in target_indices {
                summarize_expr(e, &mut out);
            }
            if let Some(e) = force {
                summarize_expr(e, &mut out);
            }
            if let Some((r, c)) = position {
                summarize_expr(r, &mut out);
                summarize_expr(c, &mut out);
            }
            out.reads.insert(EffectRegion::IoState);
            out.writes.insert(EffectRegion::IoState);
            out.write_scalar(target);
        }
        Stmt::KeySet { index, text } => {
            summarize_expr(index, &mut out);
            summarize_str(text, &mut out);
            out.writes.insert(EffectRegion::ProgramState);
        }
        Stmt::DisplayKeys => {
            out.reads.insert(EffectRegion::ProgramState);
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::SwapStr { lhs, rhs } => {
            out.read_scalar(lhs);
            out.read_scalar(rhs);
            out.write_scalar(lhs);
            out.write_scalar(rhs);
        }
        Stmt::InsertBox {
            pattern,
            row,
            col,
            width,
            height,
            color,
        } => {
            summarize_str(pattern, &mut out);
            for e in [row, col, width, height, color] {
                summarize_expr(e, &mut out);
            }
            out.reads.insert(EffectRegion::SysMemUnknown);
            out.writes.insert(EffectRegion::SysMemUnknown);
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::Open {
            file_num,
            device,
            secondary,
            filename,
        } => {
            summarize_expr(file_num, &mut out);
            if let Some(e) = device {
                summarize_expr(e, &mut out);
            }
            if let Some(e) = secondary {
                summarize_expr(e, &mut out);
            }
            if let Some(s) = filename {
                summarize_str(s, &mut out);
            }
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::Close { file_num } => {
            summarize_expr(file_num, &mut out);
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::Load {
            filename,
            device,
            secondary,
            load_addr,
        } => {
            summarize_str(filename, &mut out);
            if let Some(e) = device {
                summarize_expr(e, &mut out);
            }
            if let Some(e) = secondary {
                summarize_expr(e, &mut out);
            }
            if let Some(e) = load_addr {
                summarize_expr(e, &mut out);
            }
            out.reads.insert(EffectRegion::IoState);
            out.writes.insert(EffectRegion::IoState);
            out.opaque_write = true;
        }
        Stmt::Verify {
            filename,
            device,
            secondary,
        }
        | Stmt::Save {
            filename,
            device,
            secondary,
        } => {
            summarize_str(filename, &mut out);
            if let Some(e) = device {
                summarize_expr(e, &mut out);
            }
            if let Some(e) = secondary {
                summarize_expr(e, &mut out);
            }
            out.reads.insert(EffectRegion::IoState);
            out.writes.insert(EffectRegion::IoState);
            out.opaque_write = true;
        }
        Stmt::Disk { command } => {
            summarize_str(command, &mut out);
            out.reads.insert(EffectRegion::IoState);
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::Dim(specs) => {
            for spec in specs {
                for dim in &spec.dims {
                    summarize_expr(dim, &mut out);
                }
                out.writes
                    .insert(EffectRegion::ArrayMeta(spec.name.clone()));
                out.writes
                    .insert(EffectRegion::ArrayData(spec.name.clone()));
                out.may_allocate = true;
            }
        }
        Stmt::OnBranch { value, .. } => {
            summarize_expr(value, &mut out);
            out.control_flow = true;
        }
        Stmt::DefFn { param, body, .. } => {
            out.write_scalar(param);
            summarize_expr(body, &mut out);
        }
        Stmt::OnKey { keys, .. } => {
            summarize_str(keys, &mut out);
            out.writes.insert(EffectRegion::IoState);
            out.control_flow = true;
        }
        Stmt::Clr | Stmt::Run(_) => {
            out.writes.insert(EffectRegion::ProgramState);
            out.writes.insert(EffectRegion::Heap);
            out.writes.insert(EffectRegion::DataPtr);
            out.opaque_write = true;
            out.control_flow = matches!(stmt, Stmt::Run(_));
        }
        Stmt::Goto { .. }
        | Stmt::GoSub { .. }
        | Stmt::Return
        | Stmt::End
        | Stmt::Stop
        | Stmt::Loop
        | Stmt::EndLoop
        | Stmt::Else
        | Stmt::Done
        | Stmt::Resume { .. } => {
            out.control_flow = true;
        }
        Stmt::OnError { .. } => {
            // Installs/clears the handler state — affects future
            // error dispatch but isn't itself a transfer.
            out.writes.insert(EffectRegion::ProgramState);
        }
        Stmt::ErrorRaise { code } => {
            summarize_expr(code, &mut out);
            out.control_flow = true;
        }
        Stmt::DoNull => {
            out.reads.insert(EffectRegion::IoState);
            out.writes.insert(EffectRegion::IoState);
        }
        Stmt::Data(_) | Stmt::Rem(_) | Stmt::Do | Stmt::Repeat | Stmt::Disable => {}
    }
    out
}

fn merge_effects(mut a: EffectSummary, b: EffectSummary) -> EffectSummary {
    a.reads.extend(b.reads);
    a.writes.extend(b.writes);
    a.opaque_read |= b.opaque_read;
    a.opaque_write |= b.opaque_write;
    a.control_flow |= b.control_flow;
    a.may_allocate |= b.may_allocate;
    a.may_raise |= b.may_raise;
    a
}

fn summarize_print_piece(piece: &PrintPiece, out: &mut EffectSummary) {
    match piece {
        PrintPiece::Expr(e)
        | PrintPiece::CharOut(e)
        | PrintPiece::TabTo(e)
        | PrintPiece::Spc(e) => summarize_expr(e, out),
        PrintPiece::StrExpr(s) => summarize_str(s, out),
        PrintPiece::PositionAt(r, c) => {
            summarize_expr(r, out);
            summarize_expr(c, out);
        }
        PrintPiece::UseField { value, .. } => summarize_expr(value, out),
        PrintPiece::LiteralString(_) | PrintPiece::Tab => {}
    }
}

fn summarize_read_target_write(target: &ReadTarget, out: &mut EffectSummary) {
    match target {
        ReadTarget::Scalar(v) => out.write_scalar(v),
        ReadTarget::Array { name, indices } => {
            for idx in indices {
                summarize_expr(idx, out);
            }
            out.write_array_data(name);
        }
    }
}

fn summarize_expr(expr: &Expr, out: &mut EffectSummary) {
    match expr {
        Expr::Number(_) | Expr::String(_) => {}
        Expr::Var(v) => out.read_scalar(v),
        Expr::Neg(e) | Expr::Not(e) | Expr::Func1(_, e) | Expr::Pos(e) | Expr::Fre(e) => {
            summarize_expr(e, out);
            if matches!(expr, Expr::Fre(_)) {
                out.reads.insert(EffectRegion::Heap);
            }
        }
        Expr::Bin(op, l, r) => {
            summarize_expr(l, out);
            summarize_expr(r, out);
            if matches!(op, BinOp::Div | BinOp::Pow) {
                out.may_raise = true;
            }
        }
        Expr::Peek(addr) | Expr::MemPeek(addr) => {
            summarize_expr(addr, out);
            note_sysmem_read(addr, out);
        }
        Expr::ArrayRef(name, indices) => {
            for idx in indices {
                summarize_expr(idx, out);
            }
            out.read_array(name);
        }
        Expr::Len(s) | Expr::Asc(s) | Expr::Val(s) | Expr::Nrm(s) => summarize_str(s, out),
        Expr::StrCompare(_, l, r) => {
            summarize_str(l, out);
            summarize_str(r, out);
        }
        Expr::FnCall(_, arg) => {
            summarize_expr(arg, out);
            out.opaque_read = true;
            out.opaque_write = true;
        }
        Expr::Usr(arg) => {
            summarize_expr(arg, out);
            out.opaque_read = true;
            out.opaque_write = true;
        }
        Expr::Joy(arg) => {
            summarize_expr(arg, out);
            out.reads.insert(EffectRegion::IoState);
            out.reads.insert(EffectRegion::SysMemKnown(0xDC00));
            out.reads.insert(EffectRegion::SysMemKnown(0xDC01));
        }
        Expr::Pot(arg) => {
            summarize_expr(arg, out);
            out.reads.insert(EffectRegion::IoState);
            out.reads.insert(EffectRegion::SysMemKnown(0xD419));
            out.reads.insert(EffectRegion::SysMemKnown(0xD41A));
        }
        Expr::At(row, col) => {
            summarize_expr(row, out);
            summarize_expr(col, out);
            out.reads.insert(EffectRegion::SysMemUnknown);
        }
        Expr::Test(x, y) => {
            summarize_expr(x, out);
            summarize_expr(y, out);
            out.reads.insert(EffectRegion::SysMemUnknown);
        }
        Expr::Check { first, second } => {
            summarize_expr(first, out);
            if let Some(e) = second {
                summarize_expr(e, out);
            }
            out.reads.insert(EffectRegion::IoState);
            out.writes.insert(EffectRegion::IoState);
        }
        Expr::Inst {
            haystack,
            needle,
            start,
        } => {
            summarize_str(haystack, out);
            summarize_str(needle, out);
            if let Some(e) = start {
                summarize_expr(e, out);
            }
        }
        Expr::Inkey => {
            out.reads.insert(EffectRegion::IoState);
            out.writes.insert(EffectRegion::IoState);
        }
        Expr::Lin => {
            out.reads.insert(EffectRegion::IoState);
        }
    }
}

fn summarize_str(expr: &StrExpr, out: &mut EffectSummary) {
    match expr {
        StrExpr::Literal(_) => {}
        StrExpr::Var(v) => out.read_scalar(v),
        StrExpr::Chr(e) | StrExpr::Str(e) | StrExpr::HexFmt(e) | StrExpr::BinFmt(e) => {
            summarize_expr(e, out);
            out.may_allocate = true;
            out.writes.insert(EffectRegion::Heap);
        }
        StrExpr::GetKey => {
            out.reads.insert(EffectRegion::IoState);
            out.writes.insert(EffectRegion::IoState);
            out.may_allocate = true;
        }
        StrExpr::Concat(l, r) => {
            summarize_str(l, out);
            summarize_str(r, out);
            out.may_allocate = true;
            out.writes.insert(EffectRegion::Heap);
        }
        StrExpr::Left(s, n) | StrExpr::Right(s, n) => {
            summarize_str(s, out);
            summarize_expr(n, out);
            out.may_allocate = true;
            out.writes.insert(EffectRegion::Heap);
        }
        StrExpr::Mid(s, start, len) => {
            summarize_str(s, out);
            summarize_expr(start, out);
            if let Some(len) = len {
                summarize_expr(len, out);
            }
            out.may_allocate = true;
            out.writes.insert(EffectRegion::Heap);
        }
        StrExpr::Dup(s, n) => {
            summarize_str(s, out);
            summarize_expr(n, out);
            out.may_allocate = true;
            out.writes.insert(EffectRegion::Heap);
        }
        StrExpr::Insert(s, t, pos) => {
            summarize_str(s, out);
            summarize_str(t, out);
            summarize_expr(pos, out);
            out.may_allocate = true;
            out.writes.insert(EffectRegion::Heap);
        }
        StrExpr::ArrayRef(name, indices) => {
            for idx in indices {
                summarize_expr(idx, out);
            }
            out.read_array(name);
        }
    }
}

fn literal_addr(expr: &Expr) -> Option<u16> {
    let Expr::Number(n) = expr else { return None };
    if !n.is_finite() || n.fract() != 0.0 || !(0.0..=65535.0).contains(n) {
        return None;
    }
    Some(*n as u16)
}

fn note_sysmem_read(addr_expr: &Expr, out: &mut EffectSummary) {
    if let Some(addr) = literal_addr(addr_expr) {
        out.reads.insert(EffectRegion::SysMemKnown(addr));
        if (0xD000..=0xDFFF).contains(&addr) {
            out.reads.insert(EffectRegion::IoState);
        }
    } else {
        out.reads.insert(EffectRegion::SysMemUnknown);
        out.reads.insert(EffectRegion::IoState);
    }
}

fn note_sysmem_write(addr_expr: &Expr, out: &mut EffectSummary) {
    if let Some(addr) = literal_addr(addr_expr) {
        out.writes.insert(EffectRegion::SysMemKnown(addr));
        if (0xD000..=0xDFFF).contains(&addr) {
            out.writes.insert(EffectRegion::IoState);
        }
    } else {
        out.writes.insert(EffectRegion::SysMemUnknown);
        out.writes.insert(EffectRegion::IoState);
    }
}

// ===== Loop and induction analyses ==========================================

#[derive(Debug, Clone)]
pub struct LoopInfo {
    pub id: usize,
    pub header: usize,
    pub latch: usize,
    pub loop_var: Option<VarName>,
    pub body: Vec<usize>,
    pub exits: Vec<usize>,
    pub invariant_stmts: Vec<usize>,
}

#[allow(dead_code)]
#[derive(Debug, Default, Clone)]
pub struct LoopAnalysisResult {
    pub loops: Vec<LoopInfo>,
    pub by_header: HashMap<usize, usize>,
    pub by_latch: HashMap<usize, usize>,
}

pub struct LoopAnalysis;

impl Analysis for LoopAnalysis {
    type Output = LoopAnalysisResult;
    fn name(&self) -> &'static str {
        "loop-analysis"
    }
    fn run(&self, module: &Module, deps: &mut Registry) -> Self::Output {
        let cfg = deps.get(module, &crate::cfg::CfgBuild).clone();
        let effects = deps.get(module, &EffectSummaryAnalysis).clone();
        let mut stack: Vec<(usize, VarName)> = Vec::new();
        let mut loops = Vec::new();

        for (node_id, _node) in cfg.nodes.iter().enumerate() {
            match cfg.stmt_at(node_id, module) {
                Stmt::For { var, .. } => stack.push((node_id, var.clone())),
                Stmt::Next { vars } => {
                    for next_var in vars {
                        let pos = match next_var {
                            Some(v) => stack.iter().rposition(|(_, open)| open == v),
                            None => stack.len().checked_sub(1),
                        };
                        let Some(pos) = pos else { continue };
                        let (header, var) = stack.remove(pos);
                        let body = loop_body_nodes(header, node_id);
                        let exits = loop_exit_nodes(&cfg, header, node_id, &body);
                        let invariant_stmts =
                            loop_invariant_nodes(&effects, header, node_id, &body);
                        let id = loops.len();
                        loops.push(LoopInfo {
                            id,
                            header,
                            latch: node_id,
                            loop_var: Some(var),
                            body,
                            exits,
                            invariant_stmts,
                        });
                    }
                }
                _ => {}
            }
        }

        let mut by_header = HashMap::new();
        let mut by_latch = HashMap::new();
        for lp in &loops {
            by_header.insert(lp.header, lp.id);
            by_latch.insert(lp.latch, lp.id);
        }
        LoopAnalysisResult {
            loops,
            by_header,
            by_latch,
        }
    }
}

fn loop_body_nodes(header: usize, latch: usize) -> Vec<usize> {
    if latch <= header + 1 {
        Vec::new()
    } else {
        ((header + 1)..latch).collect()
    }
}

fn loop_exit_nodes(
    cfg: &crate::cfg::Cfg,
    header: usize,
    latch: usize,
    body: &[usize],
) -> Vec<usize> {
    let mut in_loop: HashSet<usize> = body.iter().copied().collect();
    in_loop.insert(header);
    in_loop.insert(latch);
    let mut exits = Vec::new();
    for node in in_loop.iter().copied() {
        for &succ in &cfg.nodes[node].successors {
            if !in_loop.contains(&succ) && !exits.contains(&succ) {
                exits.push(succ);
            }
        }
    }
    exits.sort_unstable();
    exits
}

fn loop_invariant_nodes(
    effects: &EffectSummaryResult,
    header: usize,
    latch: usize,
    body: &[usize],
) -> Vec<usize> {
    let mut writes = HashSet::new();
    for node in std::iter::once(header)
        .chain(body.iter().copied())
        .chain(std::iter::once(latch))
    {
        if let Some(summary) = effects.summary_for_node(node) {
            writes.extend(summary.writes.iter().cloned());
            if summary.opaque_write {
                writes.insert(EffectRegion::ProgramState);
            }
        }
    }
    body.iter()
        .copied()
        .filter(|node| {
            let Some(summary) = effects.summary_for_node(*node) else {
                return false;
            };
            effect_is_loop_invariant_candidate(summary, &writes)
        })
        .collect()
}

fn effect_is_loop_invariant_candidate(
    summary: &EffectSummary,
    loop_writes: &HashSet<EffectRegion>,
) -> bool {
    if summary.opaque_read
        || summary.opaque_write
        || summary.control_flow
        || summary.may_allocate
        || summary.may_raise
    {
        return false;
    }
    if summary.reads.iter().any(|r| loop_writes.contains(r)) {
        return false;
    }
    summary.writes.iter().all(|w| {
        matches!(
            w,
            EffectRegion::ScalarFloat(_)
                | EffectRegion::ScalarInteger(_)
                | EffectRegion::ScalarString(_)
        )
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InductionFactKind {
    ForCounter {
        step: Option<i32>,
    },
    SelfUpdate {
        delta: i32,
    },
    LinearVar {
        base: VarName,
        scale: i32,
        offset: i32,
    },
    ArrayIndex {
        array: VarName,
        axis: usize,
        base: VarName,
        scale: i32,
        offset: i32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InductionFact {
    pub loop_id: usize,
    pub node: usize,
    pub var: VarName,
    pub kind: InductionFactKind,
}

#[derive(Debug, Default, Clone)]
pub struct InductionVarResult {
    pub facts: Vec<InductionFact>,
}

pub struct InductionVarAnalysis;

impl Analysis for InductionVarAnalysis {
    type Output = InductionVarResult;
    fn name(&self) -> &'static str {
        "induction-var-analysis"
    }
    fn run(&self, module: &Module, deps: &mut Registry) -> Self::Output {
        let cfg = deps.get(module, &crate::cfg::CfgBuild).clone();
        let loops = deps.get(module, &LoopAnalysis).clone();
        let mut facts = Vec::new();

        for lp in &loops.loops {
            let Some(loop_var) = &lp.loop_var else {
                continue;
            };
            if let Stmt::For { step, .. } = cfg.stmt_at(lp.header, module) {
                facts.push(InductionFact {
                    loop_id: lp.id,
                    node: lp.header,
                    var: loop_var.clone(),
                    kind: InductionFactKind::ForCounter {
                        step: int_literal(step),
                    },
                });
            }
            for &node in &lp.body {
                let stmt = cfg.stmt_at(node, module);
                collect_stmt_induction_facts(stmt, lp.id, node, loop_var, &mut facts);
            }
        }

        InductionVarResult { facts }
    }
}

fn collect_stmt_induction_facts(
    stmt: &Stmt,
    loop_id: usize,
    node: usize,
    loop_var: &VarName,
    out: &mut Vec<InductionFact>,
) {
    match stmt {
        Stmt::Let { var, value } => {
            if let Some(delta) = self_update_delta(var, value) {
                out.push(InductionFact {
                    loop_id,
                    node,
                    var: var.clone(),
                    kind: InductionFactKind::SelfUpdate { delta },
                });
            }
            if let Some((scale, offset)) = linear_in_loop(value, loop_var) {
                out.push(InductionFact {
                    loop_id,
                    node,
                    var: var.clone(),
                    kind: InductionFactKind::LinearVar {
                        base: loop_var.clone(),
                        scale,
                        offset,
                    },
                });
            }
            collect_array_index_facts_expr(value, loop_id, node, loop_var, out);
        }
        Stmt::ArrayLet {
            name,
            indices,
            value,
        } => {
            collect_array_index_facts(name, indices, loop_id, node, loop_var, out);
            collect_array_index_facts_expr(value, loop_id, node, loop_var, out);
        }
        Stmt::ArrayLetStr {
            name,
            indices,
            value,
        } => {
            collect_array_index_facts(name, indices, loop_id, node, loop_var, out);
            collect_array_index_facts_str(value, loop_id, node, loop_var, out);
        }
        Stmt::Print { items, .. } | Stmt::PrintFile { items, .. } | Stmt::Cmd { items, .. } => {
            for item in items {
                match item {
                    PrintPiece::Expr(e)
                    | PrintPiece::CharOut(e)
                    | PrintPiece::TabTo(e)
                    | PrintPiece::Spc(e) => {
                        collect_array_index_facts_expr(e, loop_id, node, loop_var, out)
                    }
                    PrintPiece::StrExpr(s) => {
                        collect_array_index_facts_str(s, loop_id, node, loop_var, out)
                    }
                    PrintPiece::PositionAt(r, c) => {
                        collect_array_index_facts_expr(r, loop_id, node, loop_var, out);
                        collect_array_index_facts_expr(c, loop_id, node, loop_var, out);
                    }
                    PrintPiece::UseField { value, .. } => {
                        collect_array_index_facts_expr(value, loop_id, node, loop_var, out);
                    }
                    PrintPiece::LiteralString(_) | PrintPiece::Tab => {}
                }
            }
        }
        Stmt::If { cond, then } => {
            collect_array_index_facts_expr(cond, loop_id, node, loop_var, out);
            if let ThenIr::Stmts(stmts) = then {
                for s in stmts {
                    collect_stmt_induction_facts(s, loop_id, node, loop_var, out);
                }
            }
        }
        _ => {}
    }
}

fn collect_array_index_facts(
    array: &VarName,
    indices: &[Expr],
    loop_id: usize,
    node: usize,
    loop_var: &VarName,
    out: &mut Vec<InductionFact>,
) {
    for (axis, index) in indices.iter().enumerate() {
        if let Some((scale, offset)) = linear_in_loop(index, loop_var) {
            out.push(InductionFact {
                loop_id,
                node,
                var: loop_var.clone(),
                kind: InductionFactKind::ArrayIndex {
                    array: array.clone(),
                    axis,
                    base: loop_var.clone(),
                    scale,
                    offset,
                },
            });
        }
    }
}

fn collect_array_index_facts_expr(
    expr: &Expr,
    loop_id: usize,
    node: usize,
    loop_var: &VarName,
    out: &mut Vec<InductionFact>,
) {
    match expr {
        Expr::ArrayRef(name, indices) => {
            collect_array_index_facts(name, indices, loop_id, node, loop_var, out);
            for index in indices {
                collect_array_index_facts_expr(index, loop_id, node, loop_var, out);
            }
        }
        Expr::Neg(e)
        | Expr::Not(e)
        | Expr::Func1(_, e)
        | Expr::Peek(e)
        | Expr::MemPeek(e)
        | Expr::FnCall(_, e)
        | Expr::Pos(e)
        | Expr::Fre(e)
        | Expr::Usr(e)
        | Expr::Joy(e)
        | Expr::Pot(e) => {
            collect_array_index_facts_expr(e, loop_id, node, loop_var, out);
        }
        Expr::At(row, col) => {
            collect_array_index_facts_expr(row, loop_id, node, loop_var, out);
            collect_array_index_facts_expr(col, loop_id, node, loop_var, out);
        }
        Expr::Test(x, y) => {
            collect_array_index_facts_expr(x, loop_id, node, loop_var, out);
            collect_array_index_facts_expr(y, loop_id, node, loop_var, out);
        }
        Expr::Check { first, second } => {
            collect_array_index_facts_expr(first, loop_id, node, loop_var, out);
            if let Some(e) = second {
                collect_array_index_facts_expr(e, loop_id, node, loop_var, out);
            }
        }
        Expr::Inst {
            haystack,
            needle,
            start,
        } => {
            collect_array_index_facts_str(haystack, loop_id, node, loop_var, out);
            collect_array_index_facts_str(needle, loop_id, node, loop_var, out);
            if let Some(e) = start {
                collect_array_index_facts_expr(e, loop_id, node, loop_var, out);
            }
        }
        Expr::Len(s) | Expr::Asc(s) | Expr::Val(s) | Expr::Nrm(s) => {
            collect_array_index_facts_str(s, loop_id, node, loop_var, out);
        }
        Expr::Bin(_, l, r) => {
            collect_array_index_facts_expr(l, loop_id, node, loop_var, out);
            collect_array_index_facts_expr(r, loop_id, node, loop_var, out);
        }
        Expr::StrCompare(_, l, r) => {
            collect_array_index_facts_str(l, loop_id, node, loop_var, out);
            collect_array_index_facts_str(r, loop_id, node, loop_var, out);
        }
        Expr::Number(_) | Expr::String(_) | Expr::Var(_) | Expr::Inkey | Expr::Lin => {}
    }
}

fn collect_array_index_facts_str(
    expr: &StrExpr,
    loop_id: usize,
    node: usize,
    loop_var: &VarName,
    out: &mut Vec<InductionFact>,
) {
    match expr {
        StrExpr::Chr(e) | StrExpr::Str(e) | StrExpr::HexFmt(e) | StrExpr::BinFmt(e) => {
            collect_array_index_facts_expr(e, loop_id, node, loop_var, out)
        }
        StrExpr::Concat(l, r) => {
            collect_array_index_facts_str(l, loop_id, node, loop_var, out);
            collect_array_index_facts_str(r, loop_id, node, loop_var, out);
        }
        StrExpr::Left(s, n) | StrExpr::Right(s, n) => {
            collect_array_index_facts_str(s, loop_id, node, loop_var, out);
            collect_array_index_facts_expr(n, loop_id, node, loop_var, out);
        }
        StrExpr::Mid(s, start, len) => {
            collect_array_index_facts_str(s, loop_id, node, loop_var, out);
            collect_array_index_facts_expr(start, loop_id, node, loop_var, out);
            if let Some(len) = len {
                collect_array_index_facts_expr(len, loop_id, node, loop_var, out);
            }
        }
        StrExpr::Dup(s, n) => {
            collect_array_index_facts_str(s, loop_id, node, loop_var, out);
            collect_array_index_facts_expr(n, loop_id, node, loop_var, out);
        }
        StrExpr::Insert(s, t, pos) => {
            collect_array_index_facts_str(s, loop_id, node, loop_var, out);
            collect_array_index_facts_str(t, loop_id, node, loop_var, out);
            collect_array_index_facts_expr(pos, loop_id, node, loop_var, out);
        }
        StrExpr::ArrayRef(name, indices) => {
            collect_array_index_facts(name, indices, loop_id, node, loop_var, out);
        }
        StrExpr::Literal(_) | StrExpr::Var(_) | StrExpr::GetKey => {}
    }
}

fn self_update_delta(var: &VarName, value: &Expr) -> Option<i32> {
    match value {
        Expr::Bin(BinOp::Add, l, r) => match (l.as_ref(), r.as_ref()) {
            (Expr::Var(v), Expr::Number(n)) if v == var => int_f64(*n),
            (Expr::Number(n), Expr::Var(v)) if v == var => int_f64(*n),
            _ => None,
        },
        Expr::Bin(BinOp::Sub, l, r) => match (l.as_ref(), r.as_ref()) {
            (Expr::Var(v), Expr::Number(n)) if v == var => int_f64(*n)?.checked_neg(),
            _ => None,
        },
        _ => None,
    }
}

fn linear_in_loop(expr: &Expr, loop_var: &VarName) -> Option<(i32, i32)> {
    match expr {
        Expr::Var(v) if v == loop_var => Some((1, 0)),
        Expr::Number(n) => Some((0, int_f64(*n)?)),
        Expr::Bin(BinOp::Add, l, r) => {
            let (ls, lo) = linear_in_loop(l, loop_var)?;
            let (rs, ro) = linear_in_loop(r, loop_var)?;
            Some((ls.checked_add(rs)?, lo.checked_add(ro)?))
        }
        Expr::Bin(BinOp::Sub, l, r) => {
            let (ls, lo) = linear_in_loop(l, loop_var)?;
            let (rs, ro) = linear_in_loop(r, loop_var)?;
            Some((ls.checked_sub(rs)?, lo.checked_sub(ro)?))
        }
        Expr::Bin(BinOp::Mul, l, r) => match (l.as_ref(), r.as_ref()) {
            (Expr::Number(n), other) | (other, Expr::Number(n)) => {
                let k = int_f64(*n)?;
                let (scale, offset) = linear_in_loop(other, loop_var)?;
                Some((scale.checked_mul(k)?, offset.checked_mul(k)?))
            }
            _ => None,
        },
        _ => None,
    }
}

fn int_literal(expr: &Expr) -> Option<i32> {
    match expr {
        Expr::Number(n) => int_f64(*n),
        Expr::Neg(inner) => int_literal(inner).and_then(i32::checked_neg),
        _ => None,
    }
}

fn int_f64(n: f64) -> Option<i32> {
    if !n.is_finite() || n.fract() != 0.0 {
        return None;
    }
    if n < i32::MIN as f64 || n > i32::MAX as f64 {
        return None;
    }
    Some(n as i32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Line, Module};

    fn fvar(base: &str) -> VarName {
        VarName {
            base: base.to_string(),
            kind: VarKind::Float,
        }
    }

    fn ivar(base: &str) -> VarName {
        VarName {
            base: base.to_string(),
            kind: VarKind::Integer,
        }
    }

    fn add(l: Expr, r: Expr) -> Expr {
        Expr::Bin(BinOp::Add, Box::new(l), Box::new(r))
    }

    fn mul(l: Expr, r: Expr) -> Expr {
        Expr::Bin(BinOp::Mul, Box::new(l), Box::new(r))
    }

    #[test]
    fn effect_summary_tracks_sysmem_and_scalar_regions() {
        let a = fvar("A");
        let m = Module {
            lines: vec![
                Line {
                    number: 10,
                    stmts: vec![Stmt::Let {
                        var: a.clone(),
                        value: Expr::Peek(Box::new(Expr::Number(53280.0))),
                    }],
                },
                Line {
                    number: 20,
                    stmts: vec![Stmt::Poke {
                        addr: Expr::Number(1024.0),
                        value: Expr::Var(a.clone()),
                    }],
                },
            ],
        };
        let mut reg = Registry::new();
        let effects = reg.get(&m, &EffectSummaryAnalysis).clone();
        let first = effects.summary_for_node(0).unwrap();
        assert!(first.reads_region(&EffectRegion::SysMemKnown(53280)));
        assert!(first.writes_region(&EffectRegion::ScalarFloat(a.clone())));
        let second = effects.summary_for_node(1).unwrap();
        assert!(second.reads_region(&EffectRegion::ScalarFloat(a)));
        assert!(second.writes_region(&EffectRegion::SysMemKnown(1024)));
    }

    #[test]
    fn loop_analysis_finds_body_exit_and_invariant_let() {
        let i = fvar("I");
        let n = fvar("N");
        let t = fvar("T");
        let m = Module {
            lines: vec![
                Line {
                    number: 10,
                    stmts: vec![Stmt::For {
                        var: i.clone(),
                        start: Expr::Number(1.0),
                        end: Expr::Number(3.0),
                        step: Expr::Number(1.0),
                        body_int_safe: true,
                        body_reads_loop_var: true,
                        induction_const: None,
                        array_inductions: Vec::new(),
                    }],
                },
                Line {
                    number: 20,
                    stmts: vec![Stmt::Let {
                        var: t,
                        value: add(Expr::Var(n), Expr::Number(1.0)),
                    }],
                },
                Line {
                    number: 30,
                    stmts: vec![Stmt::Next {
                        vars: vec![Some(i)],
                    }],
                },
                Line {
                    number: 40,
                    stmts: vec![Stmt::End],
                },
            ],
        };
        let mut reg = Registry::new();
        let loops = reg.get(&m, &LoopAnalysis).clone();
        assert_eq!(loops.loops.len(), 1);
        let lp = &loops.loops[0];
        assert_eq!(lp.header, 0);
        assert_eq!(lp.latch, 2);
        assert_eq!(lp.body, vec![1]);
        assert_eq!(lp.exits, vec![3]);
        assert_eq!(lp.invariant_stmts, vec![1]);
    }

    #[test]
    fn induction_analysis_detects_linear_vars_and_array_indices() {
        let i = fvar("I");
        let j = ivar("J");
        let a = fvar("A");
        let m = Module {
            lines: vec![
                Line {
                    number: 10,
                    stmts: vec![Stmt::For {
                        var: i.clone(),
                        start: Expr::Number(0.0),
                        end: Expr::Number(10.0),
                        step: Expr::Number(2.0),
                        body_int_safe: true,
                        body_reads_loop_var: true,
                        induction_const: None,
                        array_inductions: Vec::new(),
                    }],
                },
                Line {
                    number: 20,
                    stmts: vec![Stmt::Let {
                        var: j.clone(),
                        value: add(
                            mul(Expr::Var(i.clone()), Expr::Number(2.0)),
                            Expr::Number(3.0),
                        ),
                    }],
                },
                Line {
                    number: 30,
                    stmts: vec![Stmt::ArrayLet {
                        name: a.clone(),
                        indices: vec![add(Expr::Var(i.clone()), Expr::Number(1.0))],
                        value: Expr::Number(7.0),
                    }],
                },
                Line {
                    number: 40,
                    stmts: vec![Stmt::Next {
                        vars: vec![Some(i.clone())],
                    }],
                },
            ],
        };
        let mut reg = Registry::new();
        let facts = reg.get(&m, &InductionVarAnalysis).clone();
        assert!(facts.facts.iter().any(|f| {
            f.var == i && matches!(f.kind, InductionFactKind::ForCounter { step: Some(2) })
        }));
        assert!(facts.facts.iter().any(|f| {
            f.var == j
                && matches!(
                    &f.kind,
                    InductionFactKind::LinearVar { base, scale: 2, offset: 3 } if base == &i
                )
        }));
        assert!(facts.facts.iter().any(|f| {
            matches!(
                &f.kind,
                InductionFactKind::ArrayIndex {
                    array,
                    axis: 0,
                    base,
                    scale: 1,
                    offset: 1,
                } if array == &a && base == &i
            )
        }));
    }
}
