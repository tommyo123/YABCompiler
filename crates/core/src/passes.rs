//! IR optimization passes.
//!
//! Each pass implements `ir::Pass` and is registered in `compile.rs`.
//! Passes are intentionally small and easy to reason about — bigger
//! transformations should be split into a sequence of smaller passes
//! so each step's behaviour can be inspected independently.

use std::collections::HashSet;

use std::collections::HashMap;

use crate::analysis::{EffectRegion, effect_summary_for_stmt};
use crate::ast::{self, BinOp, Func1, OnBranchKind, ProcName, VarKind, VarName};
use crate::ir::{
    self, ArrayInduction, ArrayInductionIndex, Expr, PrintPiece, Stmt, StrExpr, ThenIr,
};

// ===== AST-level LOCAL/GLOBAL scoping ====================================

/// Walk every `PROC ... END PROC` body in the program and rewrite
/// references to LOCAL-declared variables so each body sees its own
/// uniquely-named storage slot. Mangled names follow the scheme
/// `<PROC>__<orig>` — the double underscore can't appear in a real
/// BASIC v2 identifier (parser only accepts `[A-Z][A-Z0-9]?`), so the
/// new names never collide with anything user-typed.
///
/// All downstream passes (ConstVarProp, IntPromote, shadow-int,
/// integer-island codegen, peephole) treat the mangled VarName as an
/// ordinary independent variable, so every existing optimisation
/// keeps working unchanged.
///
/// `GLOBAL` declarations strip the corresponding name out of the
/// LOCAL set — the documented "this name still refers to the
/// program-wide slot" affirmation. With no LOCAL of the same name
/// they're a no-op.
///
/// Runs BEFORE `inline_procs_ast` so when a body is cloned into call
/// sites the cloned statements already carry mangled identifiers.
/// Non-inlined bodies are mangled in place; their JSR-call sites in
/// callers continue to see the original (global) names.
pub fn localize_proc_vars(prog: &mut ast::Program) {
    let bodies = collect_proc_bodies(prog);
    if bodies.is_empty() {
        return;
    }
    // Source order, not `HashMap` order: `rewrite_body_vars` mutates `prog` in place, so with
    // overlapping/nested bodies the visit order is observable in the output.
    let mut ordered: Vec<(&ProcName, &ProcBody)> = bodies.iter().collect();
    ordered.sort_by_key(|(_, body)| (body.start_line, body.end_line));
    for (proc_name, body) in ordered {
        let mangle_map = collect_local_mangle_map(prog, body, proc_name);
        if mangle_map.is_empty() {
            continue;
        }
        rewrite_body_vars(prog, body, &mangle_map);
    }
}

/// Build the rename map `original VarName → mangled VarName` from
/// every `LOCAL` declaration inside the body, minus anything also
/// appearing in a `GLOBAL` declaration (GLOBAL wins so the user can
/// opt back out of localisation by listing the name).
fn collect_local_mangle_map(
    prog: &ast::Program,
    body: &ProcBody,
    proc_name: &ProcName,
) -> HashMap<VarName, VarName> {
    let mut locals: HashSet<VarName> = HashSet::new();
    let mut globals: HashSet<VarName> = HashSet::new();

    let prefix = proc_name.display_lossy();
    for line in &prog.lines {
        if !line_inside_body(line.number, body) {
            continue;
        }
        for stmt in &line.statements {
            collect_scope_decls(stmt, &mut locals, &mut globals);
        }
    }

    let mut map = HashMap::new();
    for v in locals {
        if globals.contains(&v) {
            continue;
        }
        let mangled = VarName {
            base: format!("{prefix}__{}", v.base),
            kind: v.kind,
        };
        map.insert(v, mangled);
    }
    map
}

fn collect_scope_decls(
    stmt: &ast::Statement,
    locals: &mut HashSet<VarName>,
    globals: &mut HashSet<VarName>,
) {
    match stmt {
        ast::Statement::Local { vars } => {
            locals.extend(vars.iter().cloned());
        }
        ast::Statement::Global { vars } => {
            globals.extend(vars.iter().cloned());
        }
        ast::Statement::If { then_branch, .. } => {
            scope_decls_in_then(then_branch, locals, globals);
        }
        ast::Statement::IfElse {
            then_branch,
            else_branch,
            ..
        } => {
            scope_decls_in_then(then_branch, locals, globals);
            scope_decls_in_then(else_branch, locals, globals);
        }
        ast::Statement::Rcomp {
            then_branch,
            else_branch,
        } => {
            scope_decls_in_then(then_branch, locals, globals);
            if let Some(b) = else_branch {
                scope_decls_in_then(b, locals, globals);
            }
        }
        _ => {}
    }
}

fn scope_decls_in_then(
    branch: &ast::ThenBranch,
    locals: &mut HashSet<VarName>,
    globals: &mut HashSet<VarName>,
) {
    if let ast::ThenBranch::Stmts(stmts) = branch {
        for s in stmts {
            collect_scope_decls(s, locals, globals);
        }
    }
}

fn line_inside_body(line: u16, body: &ProcBody) -> bool {
    line >= body.start_line && line <= body.end_line
}

fn rewrite_body_vars(prog: &mut ast::Program, body: &ProcBody, map: &HashMap<VarName, VarName>) {
    for line in &mut prog.lines {
        if !line_inside_body(line.number, body) {
            continue;
        }
        for stmt in &mut line.statements {
            rewrite_vars_in_stmt(stmt, map);
        }
    }
}

fn map_var(map: &HashMap<VarName, VarName>, v: &mut VarName) {
    if let Some(mangled) = map.get(v) {
        *v = mangled.clone();
    }
}

fn rewrite_vars_in_expr(e: &mut ast::Expr, map: &HashMap<VarName, VarName>) {
    use ast::Expr;
    match e {
        Expr::Number(_) | Expr::String(_) | Expr::Inkey | Expr::Lin => {}
        Expr::Var(v) => map_var(map, v),
        Expr::Neg(inner) | Expr::Not(inner) => rewrite_vars_in_expr(inner, map),
        Expr::Bin(_, l, r) => {
            rewrite_vars_in_expr(l, map);
            rewrite_vars_in_expr(r, map);
        }
        Expr::Func1(_, arg)
        | Expr::Peek(arg)
        | Expr::MemPeek(arg)
        | Expr::Pos(arg)
        | Expr::Fre(arg)
        | Expr::Usr(arg)
        | Expr::Joy(arg)
        | Expr::Pot(arg)
        | Expr::FnCall(_, arg) => rewrite_vars_in_expr(arg, map),
        Expr::ArrayRef(name, indices) => {
            map_var(map, name);
            for ix in indices {
                rewrite_vars_in_expr(ix, map);
            }
        }
        Expr::Len(s) | Expr::Asc(s) | Expr::Val(s) | Expr::Nrm(s) => {
            rewrite_vars_in_str_expr(s, map)
        }
        Expr::StrCompare(_, l, r) => {
            rewrite_vars_in_str_expr(l, map);
            rewrite_vars_in_str_expr(r, map);
        }
        Expr::At(row, col) => {
            rewrite_vars_in_expr(row, map);
            rewrite_vars_in_expr(col, map);
        }
        Expr::Test(x, y) => {
            rewrite_vars_in_expr(x, map);
            rewrite_vars_in_expr(y, map);
        }
        Expr::Check { first, second } => {
            rewrite_vars_in_expr(first, map);
            if let Some(s) = second {
                rewrite_vars_in_expr(s, map);
            }
        }
        Expr::Inst {
            haystack,
            needle,
            start,
        } => {
            rewrite_vars_in_str_expr(haystack, map);
            rewrite_vars_in_str_expr(needle, map);
            if let Some(s) = start {
                rewrite_vars_in_expr(s, map);
            }
        }
    }
}

fn rewrite_vars_in_str_expr(s: &mut ast::StrExpr, map: &HashMap<VarName, VarName>) {
    use ast::StrExpr;
    match s {
        StrExpr::Literal(_) | StrExpr::GetKey => {}
        StrExpr::Var(v) => map_var(map, v),
        StrExpr::Chr(arg) | StrExpr::Str(arg) | StrExpr::HexFmt(arg) | StrExpr::BinFmt(arg) => {
            rewrite_vars_in_expr(arg, map)
        }
        StrExpr::Concat(l, r) => {
            rewrite_vars_in_str_expr(l, map);
            rewrite_vars_in_str_expr(r, map);
        }
        StrExpr::Left(s, n) | StrExpr::Right(s, n) | StrExpr::Dup(s, n) => {
            rewrite_vars_in_str_expr(s, map);
            rewrite_vars_in_expr(n, map);
        }
        StrExpr::Mid(s, start, length) => {
            rewrite_vars_in_str_expr(s, map);
            rewrite_vars_in_expr(start, map);
            if let Some(len) = length {
                rewrite_vars_in_expr(len, map);
            }
        }
        StrExpr::Insert(a, b, pos) => {
            rewrite_vars_in_str_expr(a, map);
            rewrite_vars_in_str_expr(b, map);
            rewrite_vars_in_expr(pos, map);
        }
        StrExpr::ArrayRef(name, indices) => {
            map_var(map, name);
            for ix in indices {
                rewrite_vars_in_expr(ix, map);
            }
        }
    }
}

fn rewrite_vars_in_then(branch: &mut ast::ThenBranch, map: &HashMap<VarName, VarName>) {
    if let ast::ThenBranch::Stmts(stmts) = branch {
        for s in stmts {
            rewrite_vars_in_stmt(s, map);
        }
    }
}

fn rewrite_vars_in_stmt(stmt: &mut ast::Statement, map: &HashMap<VarName, VarName>) {
    use ast::Statement;
    match stmt {
        Statement::RaiseError(_) | Statement::Hang => {}
        Statement::Print(p) => {
            for item in &mut p.items {
                rewrite_vars_in_print_item(item, map);
            }
        }
        Statement::UseFormat { at, values, .. } => {
            if let Some((r, c)) = at {
                rewrite_vars_in_expr(r, map);
                rewrite_vars_in_expr(c, map);
            }
            for value in values {
                rewrite_vars_in_expr(value, map);
            }
        }
        Statement::UseFormatDyn {
            at, ctrl, values, ..
        } => {
            if let Some((r, c)) = at {
                rewrite_vars_in_expr(r, map);
                rewrite_vars_in_expr(c, map);
            }
            rewrite_vars_in_str_expr(ctrl, map);
            for value in values {
                rewrite_vars_in_expr(value, map);
            }
        }
        Statement::Let { name, value } => {
            map_var(map, name);
            rewrite_vars_in_expr(value, map);
        }
        Statement::LetStr { var, value } => {
            map_var(map, var);
            rewrite_vars_in_str_expr(value, map);
        }
        Statement::ArrayLet {
            name,
            indices,
            value,
        } => {
            map_var(map, name);
            for ix in indices {
                rewrite_vars_in_expr(ix, map);
            }
            rewrite_vars_in_expr(value, map);
        }
        Statement::ArrayLetStr {
            name,
            indices,
            value,
        } => {
            map_var(map, name);
            for ix in indices {
                rewrite_vars_in_expr(ix, map);
            }
            rewrite_vars_in_str_expr(value, map);
        }
        Statement::If {
            cond, then_branch, ..
        } => {
            rewrite_vars_in_expr(cond, map);
            rewrite_vars_in_then(then_branch, map);
        }
        Statement::IfElse {
            cond,
            then_branch,
            else_branch,
        } => {
            rewrite_vars_in_expr(cond, map);
            rewrite_vars_in_then(then_branch, map);
            rewrite_vars_in_then(else_branch, map);
        }
        Statement::DoIf { cond } | Statement::Until { cond } => {
            rewrite_vars_in_expr(cond, map);
        }
        Statement::ExitLoop { cond } => {
            if let Some(c) = cond {
                rewrite_vars_in_expr(c, map);
            }
        }
        Statement::ComputedGoto { target, .. } => rewrite_vars_in_expr(target, map),
        Statement::Reset { target } => rewrite_vars_in_expr(target, map),
        Statement::Rcomp {
            then_branch,
            else_branch,
        } => {
            rewrite_vars_in_then(then_branch, map);
            if let Some(b) = else_branch {
                rewrite_vars_in_then(b, map);
            }
        }
        Statement::OnKey { keys, .. } => rewrite_vars_in_str_expr(keys, map),
        Statement::For {
            var,
            start,
            end,
            step,
        } => {
            map_var(map, var);
            rewrite_vars_in_expr(start, map);
            rewrite_vars_in_expr(end, map);
            rewrite_vars_in_expr(step, map);
        }
        Statement::Next { vars } => {
            for v in vars {
                if let Some(name) = v {
                    map_var(map, name);
                }
            }
        }
        Statement::Get { var, indices } => {
            map_var(map, var);
            for ix in indices {
                rewrite_vars_in_expr(ix, map);
            }
        }
        Statement::KeyGet { var } => map_var(map, var),
        Statement::Input { targets, .. } => {
            for t in targets {
                rewrite_vars_in_read_target(t, map);
            }
        }
        Statement::InputFile { file_num, targets } => {
            rewrite_vars_in_expr(file_num, map);
            for t in targets {
                rewrite_vars_in_read_target(t, map);
            }
        }
        Statement::Read(targets) => {
            for t in targets {
                rewrite_vars_in_read_target(t, map);
            }
        }
        Statement::Dim(specs) => {
            for s in specs {
                map_var(map, &mut s.name);
                for d in &mut s.dims {
                    rewrite_vars_in_expr(d, map);
                }
            }
        }
        Statement::Poke { addr, value } | Statement::Dpoke { addr, value } => {
            rewrite_vars_in_expr(addr, map);
            rewrite_vars_in_expr(value, map);
        }
        Statement::Sys { addr, regs, .. } => {
            rewrite_vars_in_expr(addr, map);
            for r in regs {
                rewrite_vars_in_expr(r, map);
            }
        }
        Statement::ErrorRaise { code } => rewrite_vars_in_expr(code, map),
        Statement::Wait { addr, mask, eor } => {
            rewrite_vars_in_expr(addr, map);
            rewrite_vars_in_expr(mask, map);
            if let Some(e) = eor {
                rewrite_vars_in_expr(e, map);
            }
        }
        Statement::Pause { message, ticks } => {
            if let Some(m) = message {
                rewrite_vars_in_str_expr(m, map);
            }
            rewrite_vars_in_expr(ticks, map);
        }
        Statement::Cset { mode } => rewrite_vars_in_expr(mode, map),
        Statement::Color {
            border,
            background,
            pen,
        } => {
            for opt in [border, background, pen] {
                if let Some(e) = opt {
                    rewrite_vars_in_expr(e, map);
                }
            }
        }
        Statement::ScreenRect {
            row,
            col,
            width,
            height,
            ch,
            color,
            ..
        } => {
            rewrite_vars_in_expr(row, map);
            rewrite_vars_in_expr(col, map);
            rewrite_vars_in_expr(width, map);
            rewrite_vars_in_expr(height, map);
            if let Some(e) = ch {
                rewrite_vars_in_expr(e, map);
            }
            if let Some(e) = color {
                rewrite_vars_in_expr(e, map);
            }
        }
        Statement::ScreenMove {
            row,
            col,
            width,
            height,
            dest_row,
            dest_col,
        } => {
            for e in [row, col, width, height, dest_row, dest_col] {
                rewrite_vars_in_expr(e, map);
            }
        }
        Statement::ScreenScroll {
            row,
            col,
            width,
            height,
            ..
        } => {
            for e in [row, col, width, height] {
                rewrite_vars_in_expr(e, map);
            }
        }
        Statement::MobEnable { index, .. } => rewrite_vars_in_expr(index, map),
        Statement::Sound { voice, freq } => {
            rewrite_vars_in_expr(voice, map);
            rewrite_vars_in_expr(freq, map);
        }
        Statement::Envelope {
            voice,
            attack,
            decay,
            sustain,
            release,
        } => {
            for e in [voice, attack, decay, sustain, release] {
                rewrite_vars_in_expr(e, map);
            }
        }
        Statement::Wave {
            voice,
            control,
            pulse,
        } => {
            rewrite_vars_in_expr(voice, map);
            rewrite_vars_in_expr(control, map);
            if let Some(p) = pulse {
                rewrite_vars_in_expr(p, map);
            }
        }
        Statement::Music { tempo, tune } => {
            rewrite_vars_in_expr(tempo, map);
            rewrite_vars_in_str_expr(tune, map);
        }
        Statement::Play { mode } => rewrite_vars_in_expr(mode, map),
        Statement::Flash {
            speed,
            color1,
            color2,
            ..
        }
        | Statement::Bflash {
            speed,
            color1,
            color2,
            ..
        } => {
            for opt in [speed, color1, color2] {
                if let Some(e) = opt {
                    rewrite_vars_in_expr(e, map);
                }
            }
        }
        Statement::LowCol {
            color1,
            color2,
            color3,
        } => {
            rewrite_vars_in_expr(color1, map);
            rewrite_vars_in_expr(color2, map);
            if let Some(e) = color3 {
                rewrite_vars_in_expr(e, map);
            }
        }
        Statement::Mod { ink, paper } => {
            rewrite_vars_in_expr(ink, map);
            rewrite_vars_in_expr(paper, map);
        }
        Statement::Dup {
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
                rewrite_vars_in_expr(e, map);
            }
            for opt in [mode, zoom] {
                if let Some(e) = opt {
                    rewrite_vars_in_expr(e, map);
                }
            }
        }
        Statement::Copy { src, dst, len } => {
            rewrite_vars_in_expr(src, map);
            rewrite_vars_in_expr(dst, map);
            rewrite_vars_in_expr(len, map);
        }
        Statement::ScrSave { addr, mode } | Statement::ScrLoad { addr, mode } => {
            if let Some(e) = addr {
                rewrite_vars_in_expr(e, map);
            }
            if let Some(e) = mode {
                rewrite_vars_in_expr(e, map);
            }
        }
        Statement::ScrDef { addr, mode, .. } => {
            rewrite_vars_in_expr(addr, map);
            if let Some(e) = mode {
                rewrite_vars_in_expr(e, map);
            }
        }
        Statement::ScrRestore { .. } => {}
        Statement::MemClr { addr, len, value } => {
            rewrite_vars_in_expr(addr, map);
            rewrite_vars_in_expr(len, map);
            if let Some(e) = value {
                rewrite_vars_in_expr(e, map);
            }
        }
        Statement::MemTransfer { .. } => {}
        Statement::MemDef {
            len,
            c64_addr,
            reu_addr,
            reu_bank,
            auto_inc,
            fixed,
        } => {
            rewrite_vars_in_expr(len, map);
            for opt in [c64_addr, reu_addr, reu_bank, auto_inc, fixed] {
                if let Some(e) = opt {
                    rewrite_vars_in_expr(e, map);
                }
            }
        }
        Statement::MemLen { len } => rewrite_vars_in_expr(len, map),
        Statement::MemC64Addr { addr } => rewrite_vars_in_expr(addr, map),
        Statement::MemReuPos { addr, bank } => {
            rewrite_vars_in_expr(addr, map);
            rewrite_vars_in_expr(bank, map);
        }
        Statement::MemRestore { auto_inc } => rewrite_vars_in_expr(auto_inc, map),
        Statement::MemCont { mode } => rewrite_vars_in_expr(mode, map),
        Statement::Design { addr, bytes } => {
            rewrite_vars_in_expr(addr, map);
            for e in bytes {
                rewrite_vars_in_expr(e, map);
            }
        }
        Statement::Mmob { index, x, y } => {
            rewrite_vars_in_expr(index, map);
            rewrite_vars_in_expr(x, map);
            rewrite_vars_in_expr(y, map);
        }
        Statement::MmobGlide {
            index,
            sx,
            sy,
            ex,
            ey,
            size,
            speed,
        } => {
            for e in [index, sx, sy, ex, ey] {
                rewrite_vars_in_expr(e, map);
            }
            if let Some(e) = size {
                rewrite_vars_in_expr(e, map);
            }
            if let Some(e) = speed {
                rewrite_vars_in_expr(e, map);
            }
        }
        Statement::MobSet {
            index,
            block,
            color,
            priority,
            multicolor,
            size,
            speed,
        } => {
            for e in [index, block, color, priority, multicolor] {
                rewrite_vars_in_expr(e, map);
            }
            if let Some(s) = size {
                rewrite_vars_in_expr(s, map);
            }
            if let Some(s) = speed {
                rewrite_vars_in_expr(s, map);
            }
        }
        Statement::Rlocmob {
            index,
            dx,
            dy,
            speed,
        } => {
            rewrite_vars_in_expr(index, map);
            rewrite_vars_in_expr(dx, map);
            rewrite_vars_in_expr(dy, map);
            if let Some(e) = speed {
                rewrite_vars_in_expr(e, map);
            }
        }
        Statement::Detect { mode } => rewrite_vars_in_expr(mode, map),
        Statement::Cmob { color1, color2 } => {
            rewrite_vars_in_expr(color1, map);
            rewrite_vars_in_expr(color2, map);
        }
        Statement::Bckgnds {
            color0,
            color1,
            color2,
            color3,
        } => {
            for e in [color0, color1, color2, color3] {
                rewrite_vars_in_expr(e, map);
            }
        }
        Statement::Border { color } => rewrite_vars_in_expr(color, map),
        Statement::Line {
            x1,
            y1,
            x2,
            y2,
            mode,
        }
        | Statement::Block {
            x1,
            y1,
            x2,
            y2,
            mode,
        } => {
            for e in [x1, y1, x2, y2] {
                rewrite_vars_in_expr(e, map);
            }
            if let Some(e) = mode {
                rewrite_vars_in_expr(e, map);
            }
        }
        Statement::Rec {
            x,
            y,
            width,
            height,
            mode,
        } => {
            for e in [x, y, width, height] {
                rewrite_vars_in_expr(e, map);
            }
            if let Some(e) = mode {
                rewrite_vars_in_expr(e, map);
            }
        }
        Statement::Draw { x, y, mode }
        | Statement::DrawTo { x, y, mode }
        | Statement::Paint { x, y, mode } => {
            rewrite_vars_in_expr(x, map);
            rewrite_vars_in_expr(y, map);
            if let Some(e) = mode {
                rewrite_vars_in_expr(e, map);
            }
        }
        Statement::Circle {
            cx,
            cy,
            radius,
            ry,
            start,
            end,
            step,
            mode,
        } => {
            rewrite_vars_in_expr(cx, map);
            rewrite_vars_in_expr(cy, map);
            rewrite_vars_in_expr(radius, map);
            for opt in [ry, start, end, step, mode] {
                if let Some(e) = opt {
                    rewrite_vars_in_expr(e, map);
                }
            }
        }
        Statement::Char {
            x,
            y,
            code,
            mode,
            zoom,
        } => {
            rewrite_vars_in_expr(x, map);
            rewrite_vars_in_expr(y, map);
            rewrite_vars_in_expr(code, map);
            if let Some(e) = mode {
                rewrite_vars_in_expr(e, map);
            }
            if let Some(e) = zoom {
                rewrite_vars_in_expr(e, map);
            }
        }
        Statement::Text {
            x,
            y,
            text,
            mode,
            zoom,
            kerning,
        } => {
            rewrite_vars_in_expr(x, map);
            rewrite_vars_in_expr(y, map);
            rewrite_vars_in_str_expr(text, map);
            if let Some(e) = mode {
                rewrite_vars_in_expr(e, map);
            }
            if let Some(e) = zoom {
                rewrite_vars_in_expr(e, map);
            }
            if let Some(e) = kerning {
                rewrite_vars_in_expr(e, map);
            }
        }
        Statement::Rot { direction, length } => {
            rewrite_vars_in_expr(direction, map);
            if let Some(l) = length {
                rewrite_vars_in_expr(l, map);
            }
        }
        Statement::DrawString { code, x, y, mode } => {
            rewrite_vars_in_str_expr(code, map);
            rewrite_vars_in_expr(x, map);
            rewrite_vars_in_expr(y, map);
            if let Some(e) = mode {
                rewrite_vars_in_expr(e, map);
            }
        }
        Statement::Angl {
            cx,
            cy,
            angle,
            rx,
            ry,
            mode,
        } => {
            for e in [cx, cy, angle, rx] {
                rewrite_vars_in_expr(e, map);
            }
            for opt in [ry, mode] {
                if let Some(e) = opt {
                    rewrite_vars_in_expr(e, map);
                }
            }
        }
        Statement::MultiColors { c1, c2, c3 } => {
            rewrite_vars_in_expr(c1, map);
            rewrite_vars_in_expr(c2, map);
            rewrite_vars_in_expr(c3, map);
        }
        Statement::Multi { .. }
        | Statement::HiCol
        | Statement::Hires { .. }
        | Statement::Nrm
        | Statement::HardCopy
        | Statement::Cold
        | Statement::Retrace
        | Statement::MemModeOn
        | Statement::Disable
        | Statement::Resume { .. }
        | Statement::OnError { .. }
        | Statement::Goto(_)
        | Statement::GoSub(_)
        | Statement::Return
        | Statement::Do
        | Statement::DoNull
        | Statement::Done
        | Statement::Else
        | Statement::Repeat
        | Statement::Loop
        | Statement::EndLoop
        | Statement::ProcDef(_)
        | Statement::ProcCall(_)
        | Statement::ProcTailCall(_)
        | Statement::EndProc
        | Statement::Local { .. }
        | Statement::Global { .. }
        | Statement::DisplayKeys
        | Statement::Rem(_)
        | Statement::End
        | Statement::Stop
        | Statement::Run(_)
        | Statement::Clr
        | Statement::Restore
        | Statement::Data(_)
        | Statement::DesignRow(_) => {}
        Statement::Open {
            file_num,
            device,
            secondary,
            ..
        } => {
            rewrite_vars_in_expr(file_num, map);
            if let Some(e) = device {
                rewrite_vars_in_expr(e, map);
            }
            if let Some(e) = secondary {
                rewrite_vars_in_expr(e, map);
            }
        }
        Statement::Close { file_num } => rewrite_vars_in_expr(file_num, map),
        Statement::PrintFile { file_num, body } => {
            rewrite_vars_in_expr(file_num, map);
            for it in &mut body.items {
                rewrite_vars_in_print_item(it, map);
            }
        }
        Statement::GetFile { file_num, vars } => {
            rewrite_vars_in_expr(file_num, map);
            for v in vars {
                map_var(map, v);
            }
        }
        Statement::Cmd { file_num, body } => {
            rewrite_vars_in_expr(file_num, map);
            for it in &mut body.items {
                rewrite_vars_in_print_item(it, map);
            }
        }
        Statement::Load {
            filename,
            device,
            secondary,
            load_addr,
        } => {
            rewrite_vars_in_str_expr(filename, map);
            if let Some(e) = device {
                rewrite_vars_in_expr(e, map);
            }
            if let Some(e) = secondary {
                rewrite_vars_in_expr(e, map);
            }
            if let Some(e) = load_addr {
                rewrite_vars_in_expr(e, map);
            }
        }
        Statement::Verify {
            filename,
            device,
            secondary,
        }
        | Statement::Save {
            filename,
            device,
            secondary,
        } => {
            rewrite_vars_in_str_expr(filename, map);
            if let Some(e) = device {
                rewrite_vars_in_expr(e, map);
            }
            if let Some(e) = secondary {
                rewrite_vars_in_expr(e, map);
            }
        }
        Statement::Disk { command } => rewrite_vars_in_str_expr(command, map),
        Statement::OnBranch { value, .. } => rewrite_vars_in_expr(value, map),
        Statement::Fetch {
            control,
            max_len,
            target,
            target_indices,
            force,
            position,
        } => {
            rewrite_vars_in_str_expr(control, map);
            rewrite_vars_in_expr(max_len, map);
            map_var(map, target);
            for e in target_indices.iter_mut() {
                rewrite_vars_in_expr(e, map);
            }
            if let Some(e) = force {
                rewrite_vars_in_expr(e, map);
            }
            if let Some((r, c)) = position {
                rewrite_vars_in_expr(r, map);
                rewrite_vars_in_expr(c, map);
            }
        }
        Statement::KeySet { index, text } => {
            rewrite_vars_in_expr(index, map);
            rewrite_vars_in_str_expr(text, map);
        }
        Statement::SwapStr { lhs, rhs } => {
            map_var(map, lhs);
            map_var(map, rhs);
        }
        Statement::InsertBox {
            pattern,
            row,
            col,
            width,
            height,
            color,
        } => {
            rewrite_vars_in_str_expr(pattern, map);
            for e in [row, col, width, height, color] {
                rewrite_vars_in_expr(e, map);
            }
        }
        Statement::DefFn { param, body, .. } => {
            map_var(map, param);
            rewrite_vars_in_expr(body, map);
        }
    }
}

fn rewrite_vars_in_read_target(t: &mut ast::ReadTarget, map: &HashMap<VarName, VarName>) {
    use ast::ReadTarget;
    match t {
        ReadTarget::Scalar(v) => map_var(map, v),
        ReadTarget::Array { name, indices } => {
            map_var(map, name);
            for ix in indices {
                rewrite_vars_in_expr(ix, map);
            }
        }
    }
}

fn rewrite_vars_in_print_item(item: &mut ast::PrintItem, map: &HashMap<VarName, VarName>) {
    use ast::PrintItem;
    match item {
        PrintItem::Expr(e) | PrintItem::CharOut(e) | PrintItem::Spc(e) | PrintItem::Tab(e) => {
            rewrite_vars_in_expr(e, map)
        }
        PrintItem::StrExpr(s) => rewrite_vars_in_str_expr(s, map),
        PrintItem::PositionAt(r, c) => {
            rewrite_vars_in_expr(r, map);
            rewrite_vars_in_expr(c, map);
        }
        PrintItem::String(_) | PrintItem::Semi | PrintItem::Comma => {}
    }
}

// ===== AST-level PROC inlining ============================================

/// Inline `PROC ... END PROC` bodies into their `EXEC`/`CALL` sites
/// using a cost-model: 1 caller is always inlined; 2-5 callers only
/// when the body is short; 6-10 callers only for trivial bodies; 11+
/// never (the per-call JSR is cheaper than duplicating the body that
/// many times).
///
/// Runs at the AST level BEFORE `ir::lower` so the IR sees the
/// inlined statement sequence directly — no special PROC-tracking
/// needed downstream. Bodies that exit via GOTO / GOSUB / RETURN /
/// nested PROC / loop control stay non-inlinable: their non-linear
/// flow doesn't compose under multiple call sites.
///
/// The inlining preserves behavior:
///   * Each ProcCall is replaced by the body's statement list at the
///     call site (in-place).
///   * The original PROC body lines are emptied (kept as Rem so any
///     dangling line-number reference still resolves to a valid
///     line, but with no executable content).
///   * `END` or `Goto` immediately before the PROC body in source
///     order normally guards against fall-through; we leave that
///     alone.
pub fn inline_procs_ast(prog: &mut ast::Program) {
    fn branch_has_on_key(branch: &ast::ThenBranch) -> bool {
        matches!(branch, ast::ThenBranch::Stmts(stmts) if stmts.iter().any(stmt_has_on_key))
    }
    fn stmt_has_on_key(stmt: &ast::Statement) -> bool {
        match stmt {
            ast::Statement::OnKey { .. }
            | ast::Statement::OnError { .. }
            | ast::Statement::Resume { .. } => true,
            ast::Statement::If { then_branch, .. } => branch_has_on_key(then_branch),
            ast::Statement::IfElse {
                then_branch,
                else_branch,
                ..
            } => branch_has_on_key(then_branch) || branch_has_on_key(else_branch),
            ast::Statement::Rcomp {
                then_branch,
                else_branch,
            } => {
                branch_has_on_key(then_branch)
                    || else_branch.as_ref().is_some_and(branch_has_on_key)
            }
            _ => false,
        }
    }

    if prog
        .lines
        .iter()
        .any(|line| line.statements.iter().any(stmt_has_on_key))
    {
        return;
    }
    let bodies = collect_proc_bodies(prog);
    if bodies.is_empty() {
        return;
    }
    let call_counts = count_proc_calls(prog, &bodies);

    // Decide which procs to inline based on cost-model.
    let mut inline_set: HashMap<ProcName, Vec<ast::Statement>> = HashMap::new();
    for (name, body) in &bodies {
        if !proc_body_is_inlinable(&body.stmts) {
            continue;
        }
        let count = call_counts.get(name).copied().unwrap_or(0);
        if count == 0 {
            // Never called — inlining would cost nothing but the
            // body lines also wouldn't be reached, so leave the
            // existing dead-code-elim to handle them.
            continue;
        }
        let body_stmts = body.stmts.len();
        let should_inline = match count {
            1 => true,
            2..=5 => body_stmts <= 3,
            6..=10 => body_stmts <= 1,
            _ => false,
        };
        if should_inline {
            inline_set.insert(name.clone(), body.stmts.clone());
        }
    }
    if inline_set.is_empty() {
        return;
    }

    // Replace ProcCall sites with the inlined body. The walk is
    // top-down so a body containing nested ProcCalls would re-process
    // them (we don't currently support that — inlinable bodies forbid
    // ProcCall, see `proc_body_is_inlinable`).
    for line in &mut prog.lines {
        rewrite_proc_calls_in_stmts(&mut line.statements, &inline_set);
    }

    // Empty out PROC bodies that were fully inlined. We keep the
    // PROC header line as `Rem` (already what ProcDef lowers to)
    // and replace internal lines + the `EndProc` with `Rem` so any
    // remaining line-number reference still hits a valid line.
    for (name, body) in &bodies {
        if !inline_set.contains_key(name) {
            continue;
        }
        for line in &mut prog.lines {
            if line.number == body.start_line {
                line.statements = vec![ast::Statement::Rem(Vec::new())];
            } else if line.number > body.start_line && line.number <= body.end_line {
                line.statements = vec![ast::Statement::Rem(Vec::new())];
            }
        }
    }
}

// ===== DESIGN block grouping =====================================
//
// `DESIGN type, addr` statement is followed by 8 (chars) or
// 21 (sprites) `@`-prefixed bitmap rows giving the pixel data via
// ASCII art. The line-by-line parser produces a `Statement::Design`
// (treating the source as `DESIGN addr_expr, byte_expr...` with
// `bytes = [addr_arg]`) plus a sequence of `Statement::DesignRow`s.
// This pass folds them: it decodes the rows into bytes per the type
// nibble and rewrites the Design with `addr = bytes[0]` (the real
// target) and `bytes = <decoded>`. The consumed DesignRow lines are
// replaced with empty REMs so any GOTO targeting one still lands on
// a valid line.
//
// Type encoding (low 3 bits):
//   bit 0 set → multicolor (2 bits/pixel), else hires (1 bit/pixel)
//   bit 1 set → char (8 rows), else mob/sprite (21 rows)
//   bit 2 set → "double" — every other source char ignored, so each
//               pixel position consumes 2 source columns
//
// Char objects produce 1 byte per row (8 bytes total).
// Mob objects produce 3 bytes per row (24 bits wide → 63 bytes total).
//
// Pixel decoding:
//   `.`, ` `, `A`  → transparent / clear (0)
//   `B`            → pen-1 bit (hires: 1; multi: %01)
//   `C`            → pen-2 bit (multi only:  %10)
//   `D`            → pen-3 bit (multi only:  %11)
pub fn group_design_blocks(prog: &mut ast::Program) -> Result<(), crate::parse::ParseError> {
    let mut line_idx = 0;
    while line_idx < prog.lines.len() {
        let mut stmt_idx = 0;
        while stmt_idx < prog.lines[line_idx].statements.len() {
            // Is this a candidate Design (native form: bytes
            // has exactly one expression — the real target addr)?
            let (type_n, line_no) = match &prog.lines[line_idx].statements[stmt_idx] {
                ast::Statement::Design { addr, bytes } if bytes.len() == 1 => {
                    match const_int_value(addr) {
                        Some(n) if (0..=7).contains(&n) => (n as u8, prog.lines[line_idx].number),
                        // Non-literal or out-of-range type → leave as
                        // legacy form; codegen will treat the single
                        // "byte" expression as a runtime POKE value.
                        _ => {
                            stmt_idx += 1;
                            continue;
                        }
                    }
                }
                _ => {
                    stmt_idx += 1;
                    continue;
                }
            };
            let spec = DesignSpec::from_type(type_n);
            // Walk forward to collect `spec.rows` DesignRow statements.
            // The rows can live in the same line (when DESIGN is part
            // of an inlined PROC body) or on subsequent lines (when
            // the source has DESIGN + @-rows on individual lines).
            let mut rows: Vec<Vec<u8>> = Vec::with_capacity(spec.rows);
            let mut taken: Vec<(usize, usize)> = Vec::with_capacity(spec.rows);
            let mut probe_line = line_idx;
            let mut probe_stmt = stmt_idx + 1;
            'collect: while rows.len() < spec.rows {
                while probe_line < prog.lines.len()
                    && probe_stmt >= prog.lines[probe_line].statements.len()
                {
                    probe_line += 1;
                    probe_stmt = 0;
                }
                if probe_line >= prog.lines.len() {
                    break 'collect;
                }
                match &prog.lines[probe_line].statements[probe_stmt] {
                    ast::Statement::DesignRow(r) => {
                        rows.push(r.clone());
                        taken.push((probe_line, probe_stmt));
                        probe_stmt += 1;
                    }
                    // Skip empty REMs interleaved between rows
                    // (inline_procs_ast leaves them on emptied PROC
                    // body lines; without skipping, a DESIGN whose
                    // rows live on lines later than the inline site
                    // wouldn't ever be reached).
                    ast::Statement::Rem(b) if b.is_empty() => {
                        probe_stmt += 1;
                    }
                    _ => break 'collect,
                }
            }
            if rows.len() < spec.rows {
                return Err(crate::parse::ParseError::UnsupportedFeature {
                    line: line_no,
                    what: "DESIGN block: not enough @-rows for declared type",
                });
            }
            let decoded = decode_design_rows(&rows, spec, line_no)?;
            let real_addr = match std::mem::replace(
                &mut prog.lines[line_idx].statements[stmt_idx],
                ast::Statement::Rem(Vec::new()),
            ) {
                ast::Statement::Design { bytes, .. } => bytes.into_iter().next().unwrap(),
                _ => unreachable!(),
            };
            prog.lines[line_idx].statements[stmt_idx] = ast::Statement::Design {
                addr: real_addr,
                bytes: decoded
                    .into_iter()
                    .map(|b| ast::Expr::Number(b as f64))
                    .collect(),
            };
            for (pl, ps) in taken {
                prog.lines[pl].statements[ps] = ast::Statement::Rem(Vec::new());
            }
            stmt_idx += 1;
        }
        line_idx += 1;
    }
    Ok(())
}

#[derive(Copy, Clone)]
struct DesignSpec {
    /// Bits per pixel: 1 (hires) or 2 (multi).
    bpp: u8,
    /// Number of source chars per pixel in the row: 1 (single) or 2 (double).
    chars_per_pixel: u8,
    /// Number of bytes per row: 1 for char, 3 for mob/sprite.
    bytes_per_row: u8,
    /// Number of rows in the object: 8 for char, 21 for mob.
    rows: usize,
}

impl DesignSpec {
    fn from_type(t: u8) -> Self {
        let multi = (t & 0x01) != 0;
        let is_char = (t & 0x02) != 0;
        let double = (t & 0x04) != 0;
        DesignSpec {
            bpp: if multi { 2 } else { 1 },
            chars_per_pixel: if double { 2 } else { 1 },
            bytes_per_row: if is_char { 1 } else { 3 },
            rows: if is_char { 8 } else { 21 },
        }
    }

    /// Source columns per row = bits-per-byte × bytes-per-row, scaled
    /// for the 2-bit multi packing and the optional doubling.
    fn cols_per_row(&self) -> usize {
        let pixels_per_byte: usize = if self.bpp == 1 { 8 } else { 4 };
        pixels_per_byte * self.bytes_per_row as usize * self.chars_per_pixel as usize
    }
}

/// Pull a constant integer out of an AST expression. Accepts plain
/// `Number` literals plus the `+`/`-`/`*` arithmetic that's common
/// in DESIGN's `$E000+8*N` addressing — the parser hasn't run
/// constant-folding yet at this point.
fn const_int_value(e: &ast::Expr) -> Option<i64> {
    match e {
        ast::Expr::Number(n) => Some(n.round() as i64),
        ast::Expr::Bin(op, l, r) => {
            let l = const_int_value(l)?;
            let r = const_int_value(r)?;
            Some(match op {
                ast::BinOp::Add => l.wrapping_add(r),
                ast::BinOp::Sub => l.wrapping_sub(r),
                ast::BinOp::Mul => l.wrapping_mul(r),
                _ => return None,
            })
        }
        _ => None,
    }
}

/// Decode the raw row chars (one Vec<u8> per row) into a flat byte
/// sequence. Each pixel column contributes `bpp` bits to the current
/// byte; in `double` mode we read every second source column and skip
/// the rest. Pen letters: '.', ' ', 'A' → 0; 'B' → pen 1; 'C' → pen
/// 2 (multi only); 'D' → pen 3 (multi only).
fn decode_design_rows(
    rows: &[Vec<u8>],
    spec: DesignSpec,
    line_no: u16,
) -> Result<Vec<u8>, crate::parse::ParseError> {
    let cols_needed = spec.cols_per_row();
    let mut out = Vec::with_capacity(rows.len() * spec.bytes_per_row as usize);
    for row in rows {
        // Strip whitespace at end (real listings sometimes pad with
        // trailing spaces) but require enough leading columns for
        // the pixel width the type demands.
        let trimmed: Vec<u8> = row.iter().copied().take_while(|&b| b != 0).collect();
        if trimmed.len() < cols_needed {
            return Err(crate::parse::ParseError::UnsupportedFeature {
                line: line_no,
                what: "DESIGN @-row too short for declared type",
            });
        }
        let mut col = 0usize;
        for _byte_idx in 0..spec.bytes_per_row {
            let mut byte: u8 = 0;
            let pixels_per_byte: usize = if spec.bpp == 1 { 8 } else { 4 };
            for px in 0..pixels_per_byte {
                let ch = trimmed[col];
                col += spec.chars_per_pixel as usize;
                let pen = match ch {
                    b'.' | b' ' | b'A' => 0u8,
                    b'B' => 1,
                    b'C' if spec.bpp == 2 => 2,
                    b'D' if spec.bpp == 2 => 3,
                    _ => {
                        return Err(crate::parse::ParseError::UnsupportedFeature {
                            line: line_no,
                            what: "DESIGN @-row: invalid pixel char (use . space A B C D)",
                        });
                    }
                };
                if spec.bpp == 1 {
                    if pen != 0 {
                        byte |= 0x80 >> px;
                    }
                } else {
                    // Multi: pixel slots are 2 bits, packed left-to-right.
                    byte |= pen << (6 - px * 2);
                }
            }
            out.push(byte);
        }
    }
    Ok(out)
}

#[derive(Debug, Clone)]
struct ProcBody {
    start_line: u16,
    end_line: u16,
    stmts: Vec<ast::Statement>,
}

fn collect_proc_bodies(prog: &ast::Program) -> HashMap<ProcName, ProcBody> {
    let mut out = HashMap::new();
    let mut i = 0;
    while i < prog.lines.len() {
        let line = &prog.lines[i];
        // Find a line whose first stmt is ProcDef. Multi-stmt PROC
        // headers are unusual but harmless; we only consider the
        // first stmt.
        let proc_name = match line.statements.first() {
            Some(ast::Statement::ProcDef(name)) => name.clone(),
            _ => {
                i += 1;
                continue;
            }
        };
        let start_line = line.number;
        // Collect body stmts: rest of this line, plus all following
        // lines until we hit EndProc.
        let mut body = Vec::new();
        for s in line.statements.iter().skip(1) {
            if matches!(s, ast::Statement::EndProc) {
                // PROC and END PROC on one line — empty body.
                out.insert(
                    proc_name.clone(),
                    ProcBody {
                        start_line,
                        end_line: line.number,
                        stmts: body.clone(),
                    },
                );
                i += 1;
                continue;
            }
            body.push(s.clone());
        }
        let mut end_line = start_line;
        let mut found_end = false;
        let mut j = i + 1;
        while j < prog.lines.len() {
            let lj = &prog.lines[j];
            for s in &lj.statements {
                if matches!(s, ast::Statement::EndProc) {
                    found_end = true;
                    end_line = lj.number;
                    break;
                }
                body.push(s.clone());
            }
            if found_end {
                break;
            }
            j += 1;
        }
        if found_end {
            out.insert(
                proc_name,
                ProcBody {
                    start_line,
                    end_line,
                    stmts: body,
                },
            );
            i = j + 1;
        } else {
            // Malformed: PROC without END PROC. Don't track it —
            // codegen will error out at lowering / FOR-balance
            // checks anyway.
            i += 1;
        }
    }
    out
}

fn count_proc_calls(
    prog: &ast::Program,
    bodies: &HashMap<ProcName, ProcBody>,
) -> HashMap<ProcName, u32> {
    let mut counts: HashMap<ProcName, u32> = HashMap::new();
    for line in &prog.lines {
        // Skip lines inside a PROC body — those calls would be
        // counted toward the inner PROC, but if the OUTER PROC is
        // inlined N times, the inner calls also multiply.
        let in_body = bodies
            .values()
            .any(|b| line.number > b.start_line && line.number <= b.end_line)
            || bodies.values().any(|b| line.number == b.start_line);
        for stmt in &line.statements {
            count_proc_calls_in_stmt(stmt, &mut counts, in_body);
        }
    }
    counts
}

fn count_proc_calls_in_stmt(
    stmt: &ast::Statement,
    counts: &mut HashMap<ProcName, u32>,
    in_body: bool,
) {
    match stmt {
        ast::Statement::ProcCall(name) | ast::Statement::ProcTailCall(name) if !in_body => {
            *counts.entry(name.clone()).or_insert(0) += 1;
        }
        ast::Statement::ProcCall(_) | ast::Statement::ProcTailCall(_) => {
            // Inside a PROC body — counts handled when caller is
            // walked. Keep behaviour conservative: don't double-count.
        }
        ast::Statement::If { then_branch, .. } => {
            count_proc_calls_in_then(then_branch, counts, in_body);
        }
        ast::Statement::IfElse {
            then_branch,
            else_branch,
            ..
        } => {
            count_proc_calls_in_then(then_branch, counts, in_body);
            count_proc_calls_in_then(else_branch, counts, in_body);
        }
        ast::Statement::Rcomp {
            then_branch,
            else_branch,
        } => {
            count_proc_calls_in_then(then_branch, counts, in_body);
            if let Some(b) = else_branch {
                count_proc_calls_in_then(b, counts, in_body);
            }
        }
        _ => {}
    }
}

fn count_proc_calls_in_then(
    then: &ast::ThenBranch,
    counts: &mut HashMap<ProcName, u32>,
    in_body: bool,
) {
    if let ast::ThenBranch::Stmts(inner) = then {
        for s in inner {
            count_proc_calls_in_stmt(s, counts, in_body);
        }
    }
}

/// True iff `stmts` (a PROC body) only uses statements safe to inline:
/// no internal control transfers (GOTO / GOSUB / RETURN / nested PROC
/// definitions / loop control / RUN / END / STOP / RESUME / Computed
/// GOTO / OnBranch). The inline pass copies the body verbatim, so any
/// non-linear flow would compose unpredictably across multiple call
/// sites.
fn proc_body_is_inlinable(stmts: &[ast::Statement]) -> bool {
    stmts.iter().all(stmt_is_inlinable)
}

fn stmt_is_inlinable(stmt: &ast::Statement) -> bool {
    use ast::Statement;
    match stmt {
        Statement::Goto(_)
        | Statement::GoSub(_)
        | Statement::Return
        | Statement::ProcDef(_)
        | Statement::ProcCall(_)
        | Statement::ProcTailCall(_)
        | Statement::EndProc
        | Statement::Run(_)
        | Statement::End
        | Statement::Stop
        | Statement::Resume { .. }
        | Statement::OnError { .. }
        | Statement::ErrorRaise { .. }
        | Statement::ComputedGoto { .. }
        | Statement::Rcomp { .. }
        | Statement::OnBranch { .. }
        | Statement::OnKey { .. }
        | Statement::Repeat
        | Statement::Until { .. }
        | Statement::Loop
        | Statement::EndLoop
        | Statement::ExitLoop { .. }
        | Statement::DoIf { .. }
        | Statement::Do
        | Statement::DoNull
        | Statement::Done
        | Statement::Else
        | Statement::For { .. }
        | Statement::Next { .. }
        | Statement::DefFn { .. }
        | Statement::Dim(_) => false,
        // Recurse into IF/IF-ELSE bodies — they're inlinable iff
        // every contained statement is inlinable.
        Statement::If { then_branch, .. } => then_is_inlinable(then_branch),
        Statement::IfElse {
            then_branch,
            else_branch,
            ..
        } => then_is_inlinable(then_branch) && then_is_inlinable(else_branch),
        // Everything else (Print, Let, Poke, Color, MOB, ...) is
        // safe to duplicate.
        _ => true,
    }
}

fn then_is_inlinable(then: &ast::ThenBranch) -> bool {
    match then {
        ast::ThenBranch::Goto(_) => false,
        ast::ThenBranch::Stmts(inner) => inner.iter().all(stmt_is_inlinable),
    }
}

fn rewrite_proc_calls_in_stmts(
    stmts: &mut Vec<ast::Statement>,
    inline_set: &HashMap<ProcName, Vec<ast::Statement>>,
) {
    let mut i = 0;
    while i < stmts.len() {
        if let ast::Statement::ProcCall(name) = &stmts[i] {
            if let Some(body) = inline_set.get(name) {
                let body_clone = body.clone();
                stmts.splice(i..=i, body_clone.iter().cloned());
                i += body_clone.len();
                continue;
            }
        }
        // Recurse into IF / IF-ELSE bodies.
        match &mut stmts[i] {
            ast::Statement::If { then_branch, .. } => {
                rewrite_proc_calls_in_then(then_branch, inline_set);
            }
            ast::Statement::IfElse {
                then_branch,
                else_branch,
                ..
            } => {
                rewrite_proc_calls_in_then(then_branch, inline_set);
                rewrite_proc_calls_in_then(else_branch, inline_set);
            }
            _ => {}
        }
        i += 1;
    }
}

fn rewrite_proc_calls_in_then(
    then: &mut ast::ThenBranch,
    inline_set: &HashMap<ProcName, Vec<ast::Statement>>,
) {
    if let ast::ThenBranch::Stmts(inner) = then {
        rewrite_proc_calls_in_stmts(inner, inline_set);
    }
}

/// Folds expressions whose operands are all numeric literals into a
/// single `Number` node. Runs bottom-up so a fully-literal expression
/// like `2 * (3 + 4)` collapses in one walk to `Number(14.0)`.
///
/// The pass is conservative around any operation whose runtime
/// behaviour we can't faithfully reproduce at compile time — division
/// by zero stays as-is so BASIC's runtime error fires, and `RND` is
/// never folded since its result isn't deterministic.
pub struct ConstantFold;

impl ir::Pass for ConstantFold {
    fn name(&self) -> &'static str {
        "constant-fold"
    }

    fn run(&self, module: &mut ir::Module) -> Result<(), ir::PassError> {
        let mut folder = ConstantFolder {
            fre_observable: module_uses_fre(module),
        };
        crate::visit::walk_module_mut(&mut folder, module);
        Ok(())
    }
}

/// Rewrites IF/IF-ELSE nodes whose condition has already folded to a
/// numeric literal. This runs after LocalConstProp + ConstantFold so
/// conditions like `IF A=5 THEN ...` can collapse once `A` is known.
pub struct IfConditionFold;

impl ir::Pass for IfConditionFold {
    fn name(&self) -> &'static str {
        "if-condition-fold"
    }

    fn run(&self, module: &mut ir::Module) -> Result<(), ir::PassError> {
        // RCOMP/standalone ELSE observes the last IF result via
        // __LAST_IF. Codegen preserves that even for constant IFs; this
        // structural fold would erase the producer, so leave such
        // modules in codegen's existing safe path.
        if module_has_rcomp(module) {
            return Ok(());
        }
        for line in &mut module.lines {
            fold_const_ifs_in_stmts(&mut line.stmts);
        }
        Ok(())
    }
}

fn fold_const_ifs_in_stmts(stmts: &mut Vec<Stmt>) {
    let mut i = 0;
    while i < stmts.len() {
        let replacement = match &mut stmts[i] {
            Stmt::If { cond, then } => {
                fold_const_ifs_in_then(then);
                if let Expr::Number(n) = cond {
                    Some(if *n != 0.0 {
                        then_to_stmts(then.clone())
                    } else {
                        Vec::new()
                    })
                } else {
                    None
                }
            }
            Stmt::IfElse {
                cond,
                then,
                else_then,
            } => {
                fold_const_ifs_in_then(then);
                fold_const_ifs_in_then(else_then);
                if let Expr::Number(n) = cond {
                    Some(if *n != 0.0 {
                        then_to_stmts(then.clone())
                    } else {
                        then_to_stmts(else_then.clone())
                    })
                } else {
                    None
                }
            }
            Stmt::Rcomp { then, else_then } => {
                fold_const_ifs_in_then(then);
                if let Some(else_then) = else_then {
                    fold_const_ifs_in_then(else_then);
                }
                None
            }
            _ => None,
        };

        if let Some(mut new_stmts) = replacement {
            let inserted = new_stmts.len();
            stmts.splice(i..=i, new_stmts.drain(..));
            i += inserted;
        } else {
            i += 1;
        }
    }
}

fn fold_const_ifs_in_then(then: &mut ThenIr) {
    if let ThenIr::Stmts(inner) = then {
        fold_const_ifs_in_stmts(inner);
    }
}

fn then_to_stmts(then: ThenIr) -> Vec<Stmt> {
    match then {
        ThenIr::Goto(target) => vec![Stmt::Goto { target }],
        ThenIr::Stmts(stmts) => stmts,
    }
}

fn module_has_rcomp(module: &ir::Module) -> bool {
    module
        .lines
        .iter()
        .flat_map(|line| &line.stmts)
        .any(stmt_has_rcomp)
}

fn stmt_has_rcomp(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Rcomp { .. } => true,
        Stmt::If { then, .. } => then_has_rcomp(then),
        Stmt::IfElse {
            then, else_then, ..
        } => then_has_rcomp(then) || then_has_rcomp(else_then),
        _ => false,
    }
}

fn then_has_rcomp(then: &ThenIr) -> bool {
    matches!(then, ThenIr::Stmts(inner) if inner.iter().any(stmt_has_rcomp))
}

/// Whether folding a string binding changes heap usage visible through FRE.
fn str_binding_fold_is_fre_visible(value: &StrExpr, fre_observable: bool) -> bool {
    fre_observable && !str_is_pure(value)
}

struct ConstantFolder {
    /// Whether the program observes string-heap usage through FRE.
    fre_observable: bool,
}

impl crate::visit::MutVisitor for ConstantFolder {
    fn visit_stmt_mut(&mut self, line_no: u16, stmt: &mut Stmt) {

        match stmt {
            Stmt::LetStr { value, .. }
                if str_binding_fold_is_fre_visible(value, self.fre_observable) =>
            {
                crate::visit::walk_str_expr_mut(self, value);
            }
            Stmt::ArrayLetStr { indices, value, .. }
                if str_binding_fold_is_fre_visible(value, self.fre_observable) =>
            {
                for e in indices.iter_mut() {
                    self.visit_expr_mut(e);
                }
                crate::visit::walk_str_expr_mut(self, value);
            }
            _ => crate::visit::walk_stmt_mut(self, line_no, stmt),
        }
    }

    fn visit_expr_mut(&mut self, e: &mut Expr) {
        // Recurse first so children fold before we look at this node.
        crate::visit::walk_expr_mut(self, e);
        // Now try to collapse — if every child is a Number, the whole
        // tree can become a single Number.
        if let Some(folded) = try_fold(e) {
            *e = Expr::Number(folded);
            return;
        }
        // Algebraic identity rewrite (`x + 0`, `x * 1`, `x XOR 0` …):
        // catches synthesised dead arithmetic that the literal-fold
        // above can't touch because one operand is still a non-literal
        // sub-tree.
        if let Some(simplified) = try_simplify_identity(e) {
            *e = simplified;
            return;
        }

    }

    fn visit_str_expr_mut(&mut self, s: &mut StrExpr) {
        crate::visit::walk_str_expr_mut(self, s);
        if let Some(folded) = try_fold_str(s) {
            *s = StrExpr::Literal(folded);
        }
    }
}

fn try_fold_str(s: &StrExpr) -> Option<Vec<u8>> {
    match s {
        StrExpr::Literal(b) => Some(b.clone()),
        StrExpr::Chr(arg) => {
            // CHR$(<const>) folds to the single PETSCII byte. Useful
            // for the `RL$ = CHR$(ASC("...") OR $40)` shape that
            // programs use to build INSERT box patterns.
            let n = try_fold(arg)?;
            if n.is_finite() && (0.0..=255.0).contains(&n) && n.fract() == 0.0 {
                Some(vec![n as u8])
            } else {
                None
            }
        }
        StrExpr::Concat(a, b) => {
            let mut out = try_fold_str(a)?;
            out.extend_from_slice(&try_fold_str(b)?);
            Some(out)
        }
        StrExpr::Left(inner, n) => {
            let bytes = try_fold_str(inner)?;
            let n = byte_arg_from_expr(n)?;
            // BASIC clamps to length but raises on negative; we already
            // bail above if n is negative.
            Some(bytes.iter().take(n).copied().collect())
        }
        StrExpr::Right(inner, n) => {
            let bytes = try_fold_str(inner)?;
            let n = byte_arg_from_expr(n)?;
            let take = n.min(bytes.len());
            Some(bytes[bytes.len() - take..].to_vec())
        }
        StrExpr::Mid(inner, start, n) => {
            let bytes = try_fold_str(inner)?;
            let start = byte_arg_from_expr(start)?;
            // BASIC raises ?ILL QTY for start == 0; let runtime handle.
            if start == 0 {
                return None;
            }
            let begin = (start - 1).min(bytes.len());
            let end = match n {
                Some(boxed) => {
                    let count = byte_arg_from_expr(boxed)?;
                    (begin + count).min(bytes.len())
                }
                None => bytes.len(),
            };
            Some(bytes[begin..end].to_vec())
        }
        _ => None,
    }
}

/// Read a constant byte argument for a string function.
fn byte_arg_from_expr(e: &Expr) -> Option<usize> {
    if let Expr::Number(n) = e {
        if n.is_finite() && *n >= 0.0 && n.fract() == 0.0 && *n <= 255.0 {
            return Some(*n as usize);
        }
    }
    None
}

/// zero, etc.) so the original node is preserved.
fn try_fold(e: &Expr) -> Option<f64> {
    match e {
        Expr::Number(n) => Some(*n),
        Expr::Neg(inner) => {
            let v = try_fold(inner)?;
            Some(-v)
        }
        Expr::Not(inner) => {
            let v = try_fold(inner)?;
            // `NOT` narrows through AYINT ($B1BF) exactly like AND/OR do, so it FLOORS and it
            // TRAPS outside -32768..32767.
            Some(!(ayint(crate::codegen::round_to_mflpt(v))? as i16) as f64)
        }
        Expr::Bin(op, l, r) => {
            let lv = try_fold(l)?;
            let rv = try_fold(r)?;
            fold_bin(*op, lv, rv)
        }
        Expr::Func1(f, arg) => {
            let v = try_fold(arg)?;
            fold_func1(*f, v)
        }
        Expr::Len(s) => {
            let bytes = try_fold_str(s)?;
            // BASIC's LEN truncates strings >255 to 255; we should never
            // hit that with literals but match the runtime cap anyway.
            Some(bytes.len().min(255) as f64)
        }
        Expr::Asc(s) => {
            let bytes = try_fold_str(s)?;
            // ASC of empty string is ?ILLEGAL QUANTITY at runtime — leave
            // it for the runtime to raise so behaviour stays identical.
            if bytes.is_empty() {
                None
            } else {
                Some(bytes[0] as f64)
            }
        }
        Expr::Val(s) => {
            // VAL of a literal string parses to a number per BASIC v2
            // rules — skip leading whitespace, then parse the longest
            // numeric prefix. Folding only the clean cases (parses
            // fully) — partial-parse / parse-failure cases are left to
            // the runtime so observable semantics stay identical.
            let bytes = try_fold_str(s)?;
            let text = std::str::from_utf8(&bytes).ok()?;
            let trimmed = text.trim();
            let n = trimmed.parse::<f64>().ok().filter(|n| n.is_finite())?;

            if crate::codegen::mflpt_overflows(n) {
                return None;
            }
            Some(crate::codegen::round_to_mflpt(n))
        }
        _ => None,
    }
}

/// Constant-fold `l <op> r` the way the BASIC ROM would.
fn fold_bin(op: BinOp, l: f64, r: f64) -> Option<f64> {
    let l = crate::codegen::round_to_mflpt(l);
    let r = crate::codegen::round_to_mflpt(r);
    let folded = fold_bin_exact(op, l, r)?;
    if crate::codegen::mflpt_overflows(folded) {
        return None;
    }
    Some(crate::codegen::round_to_mflpt(folded))
}

fn fold_bin_exact(op: BinOp, l: f64, r: f64) -> Option<f64> {
    let bool_to_basic = |b: bool| if b { -1.0 } else { 0.0 };
    match op {
        BinOp::Add => Some(l + r),
        BinOp::Sub => Some(l - r),
        BinOp::Mul => Some(l * r),
        // BASIC v2 raises ?DIVISION BY ZERO at runtime; let codegen
        // preserve that by leaving the expression alone.
        BinOp::Div => {
            if r == 0.0 {
                None
            } else {
                Some(l / r)
            }
        }
        // `^` is NOT `powf`.
        BinOp::Pow => {
            if r == 0.0 {
                Some(1.0)
            } else if l == 0.0 {
                Some(0.0)
            } else {
                None
            }
        }
        BinOp::Eq => Some(bool_to_basic(l == r)),
        BinOp::Ne => Some(bool_to_basic(l != r)),
        BinOp::Lt => Some(bool_to_basic(l < r)),
        BinOp::Le => Some(bool_to_basic(l <= r)),
        BinOp::Gt => Some(bool_to_basic(l > r)),
        BinOp::Ge => Some(bool_to_basic(l >= r)),

        BinOp::And => ayint(l).zip(ayint(r)).map(|(a, b)| (a & b) as i16 as f64),
        BinOp::Or => ayint(l).zip(ayint(r)).map(|(a, b)| (a | b) as i16 as f64),
        BinOp::Xor => ayint(l).zip(ayint(r)).map(|(a, b)| (a ^ b) as i16 as f64),
    }
}

/// BASIC's AYINT: floor to a signed 16-bit integer, or `None` when the value is out of range
/// (?ILLEGAL QUANTITY at runtime).
fn ayint(v: f64) -> Option<u16> {
    let f = v.floor();
    (f >= i16::MIN as f64 && f <= i16::MAX as f64).then(|| f as i32 as u16)
}

/// Apply BASIC assignment conversion, returning None for a range error.
pub(crate) fn narrow_for_int_store(var: &VarName, v: f64) -> Option<f64> {
    if var.kind == VarKind::Integer && var.base != "TI" && var.base != "ST" {
        let f = v.floor();
        (-32768.0..=32767.0).contains(&f).then_some(f)
    } else {
        Some(v)
    }
}

/// Algebraic identity simplifications that only apply when one operand is a specific constant
/// (0, 1, -1) and the OTHER operand can stay live.
fn try_simplify_identity(e: &Expr) -> Option<Expr> {
    let Expr::Bin(op, l, r) = e else { return None };
    let l_lit = as_simple_literal(l);
    let r_lit = as_simple_literal(r);
    match (op, l_lit, r_lit) {
        // Add: x + 0  /  0 + x  -> x
        (BinOp::Add, _, Some(0.0)) => Some((**l).clone()),
        (BinOp::Add, Some(0.0), _) => Some((**r).clone()),
        // Sub: x - 0 -> x ;  0 - x -> -x
        (BinOp::Sub, _, Some(0.0)) => Some((**l).clone()),
        (BinOp::Sub, Some(0.0), _) => Some(Expr::Neg(r.clone())),
        // Mul: x * 1  /  1 * x -> x ;  x * -1  /  -1 * x -> -x
        (BinOp::Mul, _, Some(1.0)) => Some((**l).clone()),
        (BinOp::Mul, Some(1.0), _) => Some((**r).clone()),
        (BinOp::Mul, _, Some(-1.0)) => Some(Expr::Neg(l.clone())),
        (BinOp::Mul, Some(-1.0), _) => Some(Expr::Neg(r.clone())),
        // Div: x / 1 -> x  (NOT 0/x: x might be 0 -> ?DIVISION BY ZERO)
        (BinOp::Div, _, Some(1.0)) => Some((**l).clone()),
        // Or:  x | 0  /  0 | x -> x
        (BinOp::Or, _, Some(0.0)) if expr_is_ayint_narrowed(l) => Some((**l).clone()),
        (BinOp::Or, Some(0.0), _) if expr_is_ayint_narrowed(r) => Some((**r).clone()),
        // Xor: x ^ 0  /  0 ^ x -> x
        (BinOp::Xor, _, Some(0.0)) if expr_is_ayint_narrowed(l) => Some((**l).clone()),
        (BinOp::Xor, Some(0.0), _) if expr_is_ayint_narrowed(r) => Some((**r).clone()),
        // And: x & -1  /  -1 & x -> x  (BASIC -1 = 0xFFFF)
        (BinOp::And, _, Some(-1.0)) if expr_is_ayint_narrowed(l) => Some((**l).clone()),
        (BinOp::And, Some(-1.0), _) if expr_is_ayint_narrowed(r) => Some((**r).clone()),
        _ => None,
    }
}

/// Whether the expression is an integer in the signed 16-bit range.
fn expr_is_ayint_narrowed(e: &Expr) -> bool {
    // A literal survives AYINT iff it is integral and in range.
    let narrow_lit = |n: f64| n.fract() == 0.0 && (-32768.0..=32767.0).contains(&n);
    match e {
        Expr::Number(n) => narrow_lit(*n),
        Expr::Neg(inner) => matches!(inner.as_ref(), Expr::Number(n) if narrow_lit(-n)),
        // `%` scalars and `%` array elements live in signed 16-bit slots, so a read is narrow
        // by construction.
        Expr::Var(v) | Expr::ArrayRef(v, _) => {
            v.kind == VarKind::Integer && v.base != "TI" && v.base != "ST"
        }
        // Comparisons yield 0/-1; the bitwise operators yield the 16-bit result of their own
        // AYINT-narrowed operands.
        Expr::Bin(op, _, _) => matches!(
            op,
            BinOp::Eq
                | BinOp::Ne
                | BinOp::Lt
                | BinOp::Le
                | BinOp::Gt
                | BinOp::Ge
                | BinOp::And
                | BinOp::Or
                | BinOp::Xor
        ),
        Expr::StrCompare(..) => true,
        Expr::Not(_) => true,
        // Byte-valued by construction: 0..=255.
        Expr::Peek(_) | Expr::MemPeek(_) | Expr::Len(_) | Expr::Asc(_) => true,
        _ => false,
    }
}

/// Returns `Some(n)` for `Expr::Number(n)` or `Expr::Neg(Number(n))`.
/// Used by `try_simplify_identity` to recognise the small set of
/// special constants (0, 1, -1) that drive the rewrites.
fn as_simple_literal(e: &Expr) -> Option<f64> {
    match e {
        Expr::Number(n) if n.is_finite() => Some(*n),
        Expr::Neg(inner) => match inner.as_ref() {
            Expr::Number(n) if n.is_finite() => Some(-n),
            _ => None,
        },
        _ => None,
    }
}

/// Fold a unary function only when its result matches BASIC floating-point behavior.
fn fold_func1(f: Func1, v: f64) -> Option<f64> {
    let v = crate::codegen::round_to_mflpt(v);
    let folded = match f {
        Func1::Abs => v.abs(),
        Func1::Int => v.floor(),
        Func1::Sgn => {
            // BASIC SGN: -1, 0, or 1. Use a direct compare so SGN(-0.0)
            // returns 0.0 rather than f64::signum's -0.0 quirk.
            if v > 0.0 {
                1.0
            } else if v < 0.0 {
                -1.0
            } else {
                0.0
            }
        }

        Func1::Sqr
        | Func1::Sin
        | Func1::Cos
        | Func1::Tan
        | Func1::Atn
        | Func1::Log
        | Func1::Exp => return None,
        // RND is non-deterministic: never fold.
        Func1::Rnd => return None,
    };
    if crate::codegen::mflpt_overflows(folded) {
        return None;
    }
    Some(crate::codegen::round_to_mflpt(folded))
}

/// Removes lines that no execution path can reach.
///
/// A line is "live" if either (a) the previous line is live and ends in
/// something that can fall through, or (b) any GOTO / GOSUB / THEN /
/// ON-branch anywhere in the program names it as a target. The first
/// line is always live (entry point). The pass is intentionally
/// conservative — references from a possibly-dead line still count, so
/// we don't iterate to convergence and risk removing something that
/// turned out to be load-bearing.
pub struct DeadLineElim;

impl ir::Pass for DeadLineElim {
    fn name(&self) -> &'static str {
        "dead-line-elim"
    }

    fn run(&self, module: &mut ir::Module) -> Result<(), ir::PassError> {
        let mut targets = HashSet::new();
        // CGOTO and ON-ERROR programs can RESUME / CGOTO into any
        // line at runtime — dead-line elim must not drop any of
        // them. Force every line into the target set so the live
        // check below short-circuits to true.
        if module_has_computed_goto(module) || module_has_on_error(module) {
            targets.extend(module.lines.iter().map(|line| line.number));
        }
        for line in &module.lines {
            for stmt in &line.stmts {
                collect_jump_targets(stmt, &mut targets);
            }
        }

        let mut prev_falls_through = true;
        module.lines.retain(|line| {
            // DATA lines contribute to the runtime DATA pool regardless
            // of whether control ever reaches them, so they must survive
            // dead-line elimination even when wedged after a RETURN/END.
            let has_data = line.stmts.iter().any(|s| matches!(s, Stmt::Data(_)));
            // Structured-block closers (DONE / END LOOP / UNTIL / ELSE
            // / END PROC) pair up with an earlier opener that codegen
            // pushed onto its loop / DO / proc stack. Dropping a closer
            // while keeping the opener leaves the stack unbalanced.
            // `END` inside a conditional branch must not make the
            // following closers look unreachable.
            let has_block_marker = line.stmts.iter().any(|s| {
                matches!(
                    s,
                    Stmt::Done
                        | Stmt::EndLoop
                        | Stmt::Until { .. }
                        | Stmt::Else
                        | Stmt::Do
                        | Stmt::DoIf { .. }
                        | Stmt::DoNull
                        | Stmt::Loop
                        | Stmt::Repeat
                )
            });
            let live = has_data
                || has_block_marker
                || prev_falls_through
                || targets.contains(&line.number);
            if live {
                prev_falls_through = !line_ends_unconditionally(&line.stmts);
            } else {
                prev_falls_through = false;
            }
            live
        });
        Ok(())
    }
}

fn collect_jump_targets(stmt: &Stmt, out: &mut HashSet<u16>) {
    match stmt {
        Stmt::Goto { target } | Stmt::GoSub { target } => {
            out.insert(*target);
        }
        // `RUN <line>` resets state and jumps — same reachability
        // contribution as GOTO. Without this DeadLineElim can drop
        // the target line and the JMP L<n> emitted by codegen ends
        // up referencing an undefined label.
        Stmt::Run(Some(target)) => {
            out.insert(*target);
        }
        Stmt::If { then, .. } => match then {
            ThenIr::Goto(n) => {
                out.insert(*n);
            }
            ThenIr::Stmts(inner) => {
                for s in inner {
                    collect_jump_targets(s, out);
                }
            }
        },
        Stmt::IfElse {
            then, else_then, ..
        } => {
            collect_jump_targets_in_then(then, out);
            collect_jump_targets_in_then(else_then, out);
        }
        Stmt::Rcomp { then, else_then } => {
            collect_jump_targets_in_then(then, out);
            if let Some(else_then) = else_then {
                collect_jump_targets_in_then(else_then, out);
            }
        }
        Stmt::OnBranch { targets, .. } => {
            for t in targets {
                out.insert(*t);
            }
        }
        // ON KEY's target is reached asynchronously when the key
        // fires — DeadLineElim must keep that line alive even
        // though no static control flow reaches it from the
        // surrounding code.
        Stmt::OnKey {
            target: Some(action),
            ..
        } => {
            out.insert(action.target());
        }
        // `ON ERROR GOTO <line>` reaches the handler line via the
        // BASIC error vector, not through a visible jump. Same
        // story as ON KEY — the handler must survive dead-line
        // elimination.
        Stmt::OnError { target: Some(line) } => {
            out.insert(*line);
        }
        // `RESUME <line>` is a runtime-targeted jump.
        Stmt::Resume {
            target: crate::ast::ResumeTarget::Line(line),
        } => {
            out.insert(*line);
        }
        _ => {}
    }
}

fn collect_jump_targets_in_then(then: &ThenIr, out: &mut HashSet<u16>) {
    match then {
        ThenIr::Goto(n) => {
            out.insert(*n);
        }
        ThenIr::Stmts(inner) => {
            for s in inner {
                collect_jump_targets(s, out);
            }
        }
    }
}

fn module_has_computed_goto(module: &ir::Module) -> bool {
    module
        .lines
        .iter()
        .flat_map(|line| &line.stmts)
        .any(stmt_has_computed_goto)
}

fn stmt_has_computed_goto(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::ComputedGoto { .. } => true,
        Stmt::If { then, .. } => then_has_computed_goto(then),
        Stmt::IfElse {
            then, else_then, ..
        } => then_has_computed_goto(then) || then_has_computed_goto(else_then),
        Stmt::Rcomp { then, else_then } => {
            then_has_computed_goto(then) || else_then.as_ref().map_or(false, then_has_computed_goto)
        }
        _ => false,
    }
}

fn then_has_computed_goto(then: &ThenIr) -> bool {
    matches!(then, ThenIr::Stmts(inner) if inner.iter().any(stmt_has_computed_goto))
}

fn module_has_on_error(module: &ir::Module) -> bool {
    module
        .lines
        .iter()
        .flat_map(|line| &line.stmts)
        .any(stmt_has_on_error)
}

fn stmt_has_on_error(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::OnError { .. } | Stmt::Resume { .. } | Stmt::ErrorRaise { .. } => true,
        Stmt::If { then, .. } => then_has_on_error(then),
        Stmt::IfElse {
            then, else_then, ..
        } => then_has_on_error(then) || then_has_on_error(else_then),
        Stmt::Rcomp { then, else_then } => {
            then_has_on_error(then) || else_then.as_ref().map_or(false, then_has_on_error)
        }
        _ => false,
    }
}

fn then_has_on_error(then: &ThenIr) -> bool {
    matches!(then, ThenIr::Stmts(inner) if inner.iter().any(stmt_has_on_error))
}

/// True iff control cannot fall off the end of `stmts` into the next
/// line. Only the very last statement matters — earlier statements'
/// effects are assumed to land on the same line via fall-through.
fn line_ends_unconditionally(stmts: &[Stmt]) -> bool {
    matches!(
        stmts.last(),
        Some(
            Stmt::Goto { .. }
                | Stmt::ComputedGoto { .. }
                | Stmt::Run(_)
                | Stmt::End
                | Stmt::Stop
                | Stmt::Return
                | Stmt::Resume { .. }
                | Stmt::ErrorRaise { .. },
        ),
    )
}

/// Walks every FOR/NEXT pair in source order and clears the FOR's
/// `body_int_safe` flag whenever its body contains anything that could
/// invalidate the int-FOR fast path: a write to the loop variable, a
/// GOSUB (could call code we can't see), an `ON ... GOSUB`, a nested
/// FOR with the same variable, or any user-function call (since
/// FN F(...) writes to F's parameter slot — possibly our loop var).
pub struct IntForBodyAnalysis;

impl ir::Pass for IntForBodyAnalysis {
    fn name(&self) -> &'static str {
        "int-for-body-analysis"
    }

    fn run(&self, module: &mut ir::Module) -> Result<(), ir::PassError> {

        let mut pairs: Vec<((usize, usize), (usize, usize))> = Vec::new();
        let mut stack: Vec<(usize, usize)> = Vec::new();
        for (li, line) in module.lines.iter().enumerate() {
            for (si, stmt) in line.stmts.iter().enumerate() {
                match stmt {
                    Stmt::For { .. } => stack.push((li, si)),
                    Stmt::Next { vars } => {
                        // Multi-var NEXT (`NEXT I, J, K`) closes one
                        // open FOR per listed var.
                        for _ in vars {
                            if let Some(open) = stack.pop() {
                                pairs.push((open, (li, si)));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        // Step 2: scan each body for the safety flag and for whether it
        // ever reads the loop variable. Held in two HashSets so we can
        // mutate the IR safely afterwards. The routine-write map lets a
        // GOSUB in the body stay int-safe when the called routine never
        // writes the counter.
        let routines = build_routine_writes(module);
        // A Float counter cannot share its private integer slot with the variable's five-byte home.
        let counter_writes = reachable_for_counter_writes(module);
        let unmatched_positions: HashSet<(usize, usize)> = stack.into_iter().collect();
        let mut unsafe_positions = HashSet::new();
        for &(fli, fsi) in &unmatched_positions {
            let Stmt::For { var, .. } = &module.lines[fli].stmts[fsi] else {
                unreachable!("position came from FOR scan")
            };
            let safe = module.lines.iter().enumerate().all(|(li, line)| {
                line.stmts.iter().enumerate().all(|(si, stmt)| {
                    (li, si) == (fli, fsi)
                        || unmatched_counter_stmt_is_safe(stmt, var, &routines)
                })
            });
            if !safe {
                unsafe_positions.insert((fli, fsi));
            }
        }
        let mut unread_positions: HashSet<(usize, usize)> = HashSet::new();
        for ((fli, fsi), (nli, nsi)) in &pairs {
            let (loop_var, for_lowering, induction_const) = match &module.lines[*fli].stmts[*fsi] {
                Stmt::For {
                    var,
                    start,
                    end,
                    step,
                    induction_const,
                    ..
                } => (
                    var.clone(),
                    classify_for_lowering(var, start, end, step),
                    *induction_const,
                ),
                _ => unreachable!("position came from FOR scan"),
            };
            let body = collect_body(module, *fli, *fsi, *nli, *nsi);
            if !body
                .iter()
                .all(|s| stmt_is_int_safe_with(s, &loop_var, Some(&routines)))
            {
                unsafe_positions.insert((*fli, *fsi));
            }
            // V_var doesn't need to be synced with the int counter
            // when no body read of `loop_var` would route through
            // FAC. For the u8-FOR + Float counter shape this also
            // unlocks codegen's int-island for bare Var(loop_var)
            // reads (the Speed-only gate on the Var arm of
            // is_int_island_addsub_only is overridden when this
            // flag is clear), so all the reads classified as
            // int-routable here actually get routed through the
            // FU_ slot at codegen time. Without that codegen
            // feedback the analysis would lie — Default profile
            // would still drop to FAC and read stale V_var (bench
            // section 4 caught exactly this as `a%(150)=0`).
            //
            // The induction_const, when set by LoopInductionDetect,
            // tells us codegen will swap matching `Var(loop_var) *
            // K` shapes for a MOVFM from the per-FOR FB_ slot, so
            // those reads don't need V_var either.
            if !body
                .iter()
                .any(|s| stmt_loop_var_needs_fac(s, &loop_var, induction_const, for_lowering))
            {
                unread_positions.insert((*fli, *fsi));
            }
        }

        // Step 3: apply both flags to each FOR.
        for (li, line) in module.lines.iter_mut().enumerate() {
            for (si, stmt) in line.stmts.iter_mut().enumerate() {
                if let Stmt::For {
                    body_int_safe,
                    body_reads_loop_var,
                    ..
                } = stmt
                {
                    if unsafe_positions.contains(&(li, si)) {
                        *body_int_safe = false;
                    }
                    if unmatched_positions.contains(&(li, si)) {
                        *body_reads_loop_var = true;
                    } else if unread_positions.contains(&(li, si)) {
                        *body_reads_loop_var = false;
                    }
                }
            }
        }

        // Any FOR counter written outside its header, of any kind. Codegen
        // exits on equality, so jumping past the limit wraps.
        struct MutableCounters<'a>(&'a HashSet<VarName>);
        impl crate::visit::MutVisitor for MutableCounters<'_> {
            fn visit_stmt_mut(&mut self, line: u16, stmt: &mut Stmt) {
                if let Stmt::For {
                    var,
                    body_int_safe,
                    body_reads_loop_var,
                    ..
                } = stmt
                {
                    if self.0.contains(var) {
                        *body_int_safe = false;
                        *body_reads_loop_var = true;
                    }
                }
                crate::visit::walk_stmt_mut(self, line, stmt);
            }
        }
        crate::visit::MutVisitor::visit_module_mut(
            &mut MutableCounters(&counter_writes),
            module,
        );
        Ok(())
    }
}

/// A body with no textual end can visit any line.
fn unmatched_counter_stmt_is_safe(stmt: &Stmt, var: &VarName, routines: &RoutineWrites) -> bool {
    // The general body predicate only rejects FN at expression leaves.
    use crate::visit::Visitor;
    struct OpaqueCalls(bool);
    impl crate::visit::Visitor for OpaqueCalls {
        fn visit_expr(&mut self, expr: &Expr) {
            if matches!(expr, Expr::FnCall(_, _) | Expr::Usr(_)) {
                self.0 = true;
            }
            crate::visit::walk_expr(self, expr);
        }
    }
    let mut calls = OpaqueCalls(false);
    calls.visit_stmt(0, stmt);
    if calls.0 {
        return false;
    }
    let admitted = matches!(
        stmt,
        Stmt::Let { .. }
            | Stmt::LetStr { .. }
            | Stmt::ArrayLet { .. }
            | Stmt::ArrayLetStr { .. }
            | Stmt::Read(_)
            | Stmt::Input { .. }
            | Stmt::KeyGet { .. }
            | Stmt::Get { .. }
            | Stmt::For { .. }
            | Stmt::Next { .. }
            | Stmt::Print { .. }
            | Stmt::If { .. }
            | Stmt::IfElse { .. }
            | Stmt::Rcomp { .. }
            | Stmt::OnBranch {
                kind: OnBranchKind::Goto,
                ..
            }
            | Stmt::Goto { .. }
            | Stmt::ComputedGoto { .. }
            | Stmt::GoSub { .. }
            | Stmt::Return
            | Stmt::End
            | Stmt::Stop
            | Stmt::Run(_)
            | Stmt::Data(_)
            | Stmt::Restore
            | Stmt::DefFn { .. }
            | Stmt::Rem(_)
            | Stmt::LineMark(_)
    );
    if !admitted || !stmt_is_int_safe_with(stmt, var, Some(routines)) {
        return false;
    }
    let branch_safe = |branch: &ThenIr| match branch {
        ThenIr::Goto(_) => true,
        ThenIr::Stmts(inner) => inner
            .iter()
            .all(|s| unmatched_counter_stmt_is_safe(s, var, routines)),
    };
    match stmt {
        Stmt::If { then, .. } => branch_safe(then),
        Stmt::IfElse { then, else_then, .. } => branch_safe(then) && branch_safe(else_then),
        Stmt::Rcomp { then, else_then } => {
            branch_safe(then) && else_then.as_ref().is_none_or(branch_safe)
        }
        _ => true,
    }
}

/// Combine a FOR loop that writes a constant value to a contiguous address range.
pub struct PokeLoopFusion;

impl ir::Pass for PokeLoopFusion {
    fn name(&self) -> &'static str {
        "poke-loop-fusion"
    }

    fn run(&self, module: &mut ir::Module) -> Result<(), ir::PassError> {

        let reads = count_var_reads(module);
        for line in &mut module.lines {
            fuse_poke_loops(&mut line.stmts, &reads);
        }
        Ok(())
    }
}

/// Census of variable READS (expression positions only) across the whole module.
fn count_var_reads(module: &ir::Module) -> HashMap<VarName, u32> {
    use crate::visit::Visitor;
    struct Census {
        counts: HashMap<VarName, u32>,
    }
    impl Visitor for Census {
        fn visit_var_read(&mut self, v: &VarName) {
            *self.counts.entry(v.clone()).or_insert(0) += 1;
        }
    }
    let mut c = Census {
        counts: HashMap::new(),
    };
    c.visit_module(module);
    c.counts
}

fn fuse_poke_loops(stmts: &mut Vec<Stmt>, reads: &HashMap<VarName, u32>) {
    let mut i = 0;
    while i + 2 < stmts.len() {
        if let Some(fused) = match_poke_fill_triplet(&stmts[i..i + 3], reads) {
            // Replace 3 stmts with 1 PokeFill at position i.
            stmts[i] = Stmt::PokeFill {
                dst_start: fused.dst_start,
                dst_end: fused.dst_end,
                value: fused.value,
            };
            stmts.drain(i + 1..i + 3);
            if let Some((var, exit)) = fused.loop_var_exit {
                stmts.insert(i + 1, Stmt::Let {
                    var,
                    value: Expr::Number(exit),
                });
            }
            // Don't advance i — a previous fusion could have left
            // another candidate adjacent (e.g., two consecutive
            // FOR/POKE/NEXT on one line). Re-check from i.
            continue;
        }
        i += 1;
    }
}

/// Value the loop variable holds once a `FOR I=s TO e STEP 1` loop has finished, or `None` when
/// the endpoints aren't both literals.
fn poke_fill_loop_var_exit(start: &Expr, end: &Expr) -> Option<f64> {
    let s = expr_as_f64_literal(start)?;
    let e = expr_as_f64_literal(end)?;
    if !s.is_finite() || !e.is_finite() {
        return None;
    }
    let iters = ((e - s).floor() + 1.0).max(1.0);
    let exit = s + iters;
    if !exit.is_finite() {
        return None;
    }
    Some(exit)
}

/// What a matched triplet lowers to.
struct FusedPokeFill {
    dst_start: Expr,
    dst_end: Expr,
    value: Expr,
    /// `Some` when the loop variable is read elsewhere and therefore needs an explicit
    /// exit-value store after the fill.
    loop_var_exit: Option<(VarName, f64)>,
}

fn expr_contains_memory_read(e: &Expr) -> bool {
    match e {
        Expr::Peek(_) | Expr::MemPeek(_) => true,
        Expr::Neg(i) | Expr::Not(i) => expr_contains_memory_read(i),
        Expr::Bin(_, l, r) => expr_contains_memory_read(l) || expr_contains_memory_read(r),
        Expr::Func1(_, a)
        | Expr::FnCall(_, a)
        | Expr::Pos(a)
        | Expr::Fre(a)
        | Expr::Usr(a)
        | Expr::Joy(a)
        | Expr::Pot(a) => expr_contains_memory_read(a),
        Expr::At(a, b) | Expr::Test(a, b) => {
            expr_contains_memory_read(a) || expr_contains_memory_read(b)
        }
        Expr::ArrayRef(_, idx) => idx.iter().any(expr_contains_memory_read),
        Expr::Check { first, second } => {
            expr_contains_memory_read(first)
                || second.as_deref().is_some_and(expr_contains_memory_read)
        }
        _ => false,
    }
}

fn match_poke_fill_triplet(
    stmts: &[Stmt],
    reads: &HashMap<VarName, u32>,
) -> Option<FusedPokeFill> {
    if stmts.len() != 3 {
        return None;
    }
    let Stmt::For {
        var: loop_var,
        start,
        end,
        step,
        ..
    } = &stmts[0]
    else {
        return None;
    };
    // STEP must fold to literal 1.
    let step_lit = expr_as_f64_literal(step)?;
    if step_lit != 1.0 {
        return None;
    }
    let Stmt::Poke { addr, value } = &stmts[1] else {
        return None;
    };
    let offset = match_loop_var_with_offset(addr, loop_var)?;
    // Value can't read the loop variable (would change every
    // iteration — not foldable to a constant fill).
    if expr_reads_var(value, loop_var) {
        return None;
    }

    // Fusing N stores into one memset evaluates `value` once, so it must be
    // side-effect free and read the same every iteration (not `TI`).
    if !expr_is_pure(value)
        || !expr_reads_are_repeatable(value)
        || expr_contains_memory_read(value)
    {
        return None;
    }
    // Start/end can't read the loop variable either (the BASIC v2
    // FOR header captures both at loop entry, so a self-reference
    // would be a corner case we don't need to optimise).
    if expr_reads_var(start, loop_var) || expr_reads_var(end, loop_var) {
        return None;
    }
    let Stmt::Next { vars } = &stmts[2] else {
        return None;
    };
    // NEXT must close exactly this loop — either bare `NEXT` or
    // `NEXT I`.
    match vars.as_slice() {
        [None] => {}
        [Some(v)] if v == loop_var => {}
        _ => return None,
    }

    let read_elsewhere = reads.get(loop_var).copied().unwrap_or(0) > 1;
    let loop_var_exit = if read_elsewhere {
        let exit = poke_fill_loop_var_exit(start, end)?;
        // An int-promoted counter can only hold the exit value if it fits i16; `B+1` at the top
        // of the range does not.
        if loop_var.kind == VarKind::Integer && !(-32768.0..=32767.0).contains(&exit) {
            return None;
        }
        Some((loop_var.clone(), exit))
    } else {
        None
    };
    let (dst_start, dst_end) = if offset == 0 {
        (start.clone(), end.clone())
    } else {
        let off_expr = Expr::Number(offset as f64);
        (
            Expr::Bin(
                BinOp::Add,
                Box::new(start.clone()),
                Box::new(off_expr.clone()),
            ),
            Expr::Bin(BinOp::Add, Box::new(end.clone()), Box::new(off_expr)),
        )
    };
    Some(FusedPokeFill {
        dst_start,
        dst_end,
        value: value.clone(),
        loop_var_exit,
    })
}

/// Recognise `Var(loop_var)`, `Var(loop_var) + K`, `K + Var(loop_var)`,
/// or `Var(loop_var) - K` for a literal `K`. Returns the signed
/// offset (`+K` or `-K`); returns `None` for anything else, including
/// expressions that mention `loop_var` more than once or wrap it in
/// other operations.
fn match_loop_var_with_offset(e: &Expr, loop_var: &VarName) -> Option<i32> {
    match e {
        Expr::Var(v) if v == loop_var => Some(0),
        Expr::Bin(BinOp::Add, l, r) => {
            if let (Expr::Var(v), Some(k)) = (l.as_ref(), expr_as_f64_literal(r))
                && v == loop_var
                && k.fract() == 0.0
                && (-32768.0..=65535.0).contains(&k)
            {
                return Some(k as i32);
            }
            if let (Some(k), Expr::Var(v)) = (expr_as_f64_literal(l), r.as_ref())
                && v == loop_var
                && k.fract() == 0.0
                && (-32768.0..=65535.0).contains(&k)
            {
                return Some(k as i32);
            }
            None
        }
        Expr::Bin(BinOp::Sub, l, r) => {
            if let (Expr::Var(v), Some(k)) = (l.as_ref(), expr_as_f64_literal(r))
                && v == loop_var
                && k.fract() == 0.0
                && (-32768.0..=65535.0).contains(&k)
            {
                return Some(-(k as i32));
            }
            None
        }
        _ => None,
    }
}

/// Loop-Invariant Code Motion. For each FOR/NEXT pair, finds
/// subexpressions in the body whose value is the same on every
/// iteration, and hoists them to a fresh `LET __LICM_<n> = expr`
/// just before the FOR. Body uses are rewritten to read the temp.
///
/// For BASIC v2 the win is FAC arithmetic: `A(I) = X*Y + I`
/// re-evaluates `X*Y` on every iteration even though X and Y are
/// loop-invariant. One float multiplication costs ~200 cycles,
/// so hoisting saves ~200·N cycles for an N-iteration loop.
///
/// Conservative scope (v1):
///   * Loop body must contain only "safe" statements — no
///     GOSUB/USR/FN/INPUT/READ/RUN/CLR/RESUME/ON ERROR/etc.
///     Anything that could escape the loop or invoke arbitrary
///     user code disqualifies the loop.
///   * Hoist only expressions with at least one `Bin` or `Func1`
///     node (a bare `Var` is already trivially cheap).
///   * Operators considered safe to hoist: Add, Sub, Mul, And, Or,
///     Xor, comparisons. **Excluded** because they can trap or
///     change observable state when the loop runs zero times:
///     Div (?/0), Pow (overflow on edge cases).
///   * Functions considered safe: Abs, Int, Sgn. **Excluded**:
///     Sqr (negative arg traps), Log/Exp (domain/overflow), Sin/
///     Cos/Tan/Atn (expensive, but more importantly can underflow
///     and we want bug-compat with FAC behaviour), Rnd (stateful).
///   * Memory-reading exprs (PEEK, ArrayRef) are excluded because
///     we'd have to prove no POKE/ArrayLet writes the same address
///     in the body — expensive analysis for v1.
///   * String exprs are excluded (heap state).
///
/// The pass runs *after* `IntPromote` so the candidate expressions
/// have stable kinds, and *before* `LoopInductionDetect` so
/// induction analysis sees the post-hoist body shape.
pub struct LoopInvariantCodeMotion;

impl ir::Pass for LoopInvariantCodeMotion {
    fn name(&self) -> &'static str {
        "loop-invariant-code-motion"
    }

    fn run(&self, module: &mut ir::Module) -> Result<(), ir::PassError> {
        // Hoisted LETs sit at the FOR. A jump into the body from outside
        // skips them, so collect branch sources for `try_hoist_loop`.
        let entries = licm_collect_entry_edges(module);
        let opaque_entry = licm_has_opaque_entry(module);
        let pairs = collect_for_next_pairs(module);
        // Process pairs in reverse so insertions in earlier lines
        // (or earlier statements on the same line) don't shift the
        // indices of as-yet-unprocessed pairs.
        let mut next_id: usize = 0;
        for ((fli, fsi), (nli, nsi)) in pairs.iter().rev() {
            if opaque_entry {
                break;
            }
            try_hoist_loop(module, *fli, *fsi, *nli, *nsi, &mut next_id, &entries);
        }
        Ok(())
    }
}

/// Every branch in the module as `(source line index, target line number)`.
/// A jump from inside a loop body is fine: the hoisted temp ran with the FOR.
/// Only an edge from outside the body can skip it.
fn licm_collect_entry_edges(module: &ir::Module) -> Vec<(usize, u16)> {
    let mut out = Vec::new();
    for (li, line) in module.lines.iter().enumerate() {
        let mut targets: HashSet<u16> = HashSet::new();
        for stmt in &line.stmts {
            collect_goto_targets(stmt, &mut targets);
            licm_collect_gosub_targets(stmt, &mut targets);
            // ON ERROR is entered from wherever the trap fired. Record it as
            // an edge from `usize::MAX`, i.e. from outside every loop.
            if let Stmt::OnError {
                target: Some(t), ..
            } = stmt
            {
                out.push((usize::MAX, *t));
            }
        }
        for t in targets {
            out.push((li, t));
        }
    }
    out
}

/// GOSUB targets that `collect_goto_targets` skips. A GOSUB into the body
/// skips the hoisted LET the same way a GOTO would.
fn licm_collect_gosub_targets(stmt: &Stmt, targets: &mut HashSet<u16>) {
    match stmt {
        Stmt::GoSub { target, .. } => {
            targets.insert(*target);
        }
        Stmt::OnBranch {
            kind: OnBranchKind::GoSub,
            targets: branch_targets,
            ..
        } => {
            targets.extend(branch_targets.iter().copied());
        }
        Stmt::If {
            then: ThenIr::Stmts(inner),
            ..
        } => {
            for s in inner {
                licm_collect_gosub_targets(s, targets);
            }
        }
        _ => {}
    }
}

/// True if a transfer can land inside a loop body at an unknown line.
/// `ComputedGoto` can reach any line; `RESUME` returns to the trapped line.
/// `RUN` restarts at line 1; `ON ERROR GOTO` is a known entry edge.
fn licm_has_opaque_entry(module: &ir::Module) -> bool {
    fn stmt_is_opaque(stmt: &Stmt) -> bool {
        match stmt {
            Stmt::ComputedGoto { .. } | Stmt::Resume { .. } => true,
            Stmt::If {
                then: ThenIr::Stmts(inner),
                ..
            } => inner.iter().any(stmt_is_opaque),
            _ => false,
        }
    }
    module
        .lines
        .iter()
        .any(|l| l.stmts.iter().any(stmt_is_opaque))
}

/// Try to hoist invariant subexpressions out of the FOR at
/// `(fli, fsi)` whose matching NEXT is at `(nli, nsi)`.
fn try_hoist_loop(
    module: &mut ir::Module,
    fli: usize,
    fsi: usize,
    nli: usize,
    nsi: usize,
    next_id: &mut usize,
    entries: &[(usize, u16)],
) {
    // Skip the hoist if a jump can enter the body without passing the FOR.
    // The FOR line itself is safe: hoisted LETs sit at the FOR statement.
    if fli < nli {
        let body_lines: HashSet<u16> = module.lines[fli + 1..=nli]
            .iter()
            .map(|l| l.number)
            .collect();
        let entered_from_outside = entries
            .iter()
            .any(|(src, tgt)| body_lines.contains(tgt) && (*src < fli || *src > nli));
        if entered_from_outside {
            return;
        }
    }
    // Pull the loop variable out of the FOR header. Sanity check
    // that we still see a FOR there (a previous pass could have
    // mutated the IR).
    let Stmt::For { var: loop_var, .. } = &module.lines[fli].stmts[fsi] else {
        return;
    };
    let loop_var = loop_var.clone();

    // Walk the body: bail out if any unsafe statement appears, and
    // collect the set of vars written anywhere in the body.
    let mut blocked: HashSet<VarName> = HashSet::new();
    blocked.insert(loop_var.clone());
    if !licm_body_safe(module, fli, fsi, nli, nsi, &mut blocked) {
        return;
    }

    // Find candidate invariant subexpressions. We collect each unique
    // candidate once even if it appears in multiple expressions —
    // hoisting once and reusing the temp is the whole point.
    let mut candidates: Vec<Expr> = Vec::new();
    licm_collect_candidates(module, fli, fsi, nli, nsi, &blocked, &mut candidates);
    if candidates.is_empty() {
        return;
    }

    // For each candidate, allocate a temp and rewrite. We rewrite
    // body uses BEFORE inserting the hoisted LET so the LET's RHS
    // sees the original (un-rewritten) candidate expression.
    let mut hoisted: Vec<Stmt> = Vec::new();
    for expr in &candidates {
        let temp_var = VarName {
            base: format!("__LICM_{}", *next_id),
            kind: licm_infer_kind(expr),
        };
        *next_id += 1;
        let temp_expr = Expr::Var(temp_var.clone());
        licm_replace_in_body(module, fli, fsi, nli, nsi, expr, &temp_expr);
        hoisted.push(Stmt::Let {
            var: temp_var,
            value: expr.clone(),
        });
    }

    // Insert the hoisted LETs at the FOR position, pushing the FOR
    // (and everything after it on the same line) right.
    for (i, stmt) in hoisted.into_iter().enumerate() {
        module.lines[fli].stmts.insert(fsi + i, stmt);
    }
}

/// Walks the loop body. Returns `false` for any statement that
/// disqualifies LICM (GOSUB, USR, FN call, INPUT, READ, etc.).
/// Otherwise records every var written in `blocked`.
fn licm_body_safe(
    module: &ir::Module,
    fli: usize,
    fsi: usize,
    nli: usize,
    nsi: usize,
    blocked: &mut HashSet<VarName>,
) -> bool {
    licm_walk_body(module, fli, fsi, nli, nsi, |stmt| {
        licm_stmt_safe(stmt, blocked)
    })
}

/// Returns `true` if `stmt` is safe inside a LICM loop body. Records
/// any vars written by the statement in `blocked`.
fn licm_stmt_safe(stmt: &Stmt, blocked: &mut HashSet<VarName>) -> bool {
    match stmt {
        // Safe stmts that may write a known set of vars.
        Stmt::Let { var, value } => {
            if licm_expr_has_unsafe(value) {
                return false;
            }
            blocked.insert(var.clone());
            true
        }
        Stmt::ArrayLet {
            name,
            indices,
            value,
        } => {
            if licm_expr_has_unsafe(value) || indices.iter().any(licm_expr_has_unsafe) {
                return false;
            }
            blocked.insert(name.clone());
            true
        }
        Stmt::Poke { addr, value } | Stmt::Dpoke { addr, value } => {
            !licm_expr_has_unsafe(addr) && !licm_expr_has_unsafe(value)
        }
        Stmt::PokeFill {
            dst_start,
            dst_end,
            value,
        } => {
            !licm_expr_has_unsafe(dst_start)
                && !licm_expr_has_unsafe(dst_end)
                && !licm_expr_has_unsafe(value)
        }
        Stmt::Print { items, .. } => items.iter().all(licm_print_piece_safe),
        Stmt::If { cond, then } => {
            if licm_expr_has_unsafe(cond) {
                return false;
            }
            match then {
                ThenIr::Goto(_) => true,
                ThenIr::Stmts(inner) => inner.iter().all(|s| licm_stmt_safe(s, blocked)),
            }
        }
        // Unconditional flow within the loop is fine — Goto inside
        // the body is rare in well-formed BASIC, but if it stays
        // inside the body we can still hoist. We approximate by
        // refusing GOTO entirely (it's hard to prove the target is
        // inside the body region).
        Stmt::Goto { .. } => false,
        // Counter increments / NEXT closures are part of the loop
        // header; we never see them inside the body when collected
        // by collect_for_next_pairs.
        Stmt::For { .. } | Stmt::Next { .. } => false,
        // REM/Data/Restore are no-ops at runtime for LICM purposes.
        // RESET is only as safe as its numeric expression.
        Stmt::Rem(_) | Stmt::Data(_) | Stmt::Restore => true,
        Stmt::Reset { target } => !licm_expr_has_unsafe(target),
        // Everything else: be conservative. GOSUB/USR/FN can call
        // arbitrary code; INPUT/READ write vars unpredictably;
        // OnError/Resume/Run/Clr alter control flow; graphics and
        // sound statements touch I/O state. Refuse them all.
        _ => false,
    }
}

fn licm_print_piece_safe(piece: &PrintPiece) -> bool {
    match piece {
        PrintPiece::Expr(e)
        | PrintPiece::CharOut(e)
        | PrintPiece::TabTo(e)
        | PrintPiece::Spc(e) => !licm_expr_has_unsafe(e),
        PrintPiece::StrExpr(_) => false, // strings touch heap
        _ => true,
    }
}

/// True if the expression contains anything that disqualifies LICM
/// (PEEK, ArrayRef, FN, USR, INKEY, RND, etc.) — NOT the same as
/// "loop-invariant", which is a stronger property checked separately
/// in `licm_expr_invariant`.
fn licm_expr_has_unsafe(e: &Expr) -> bool {
    use crate::visit::Visitor;
    struct Det {
        unsafe_seen: bool,
    }
    impl Visitor for Det {
        fn visit_expr(&mut self, e: &Expr) {
            if self.unsafe_seen {
                return;
            }
            match e {
                Expr::Peek(_)
                | Expr::ArrayRef(_, _)
                | Expr::FnCall(_, _)
                | Expr::Usr(_)
                | Expr::Inkey
                | Expr::Joy(_)
                | Expr::Pot(_)
                | Expr::Lin
                | Expr::At(_, _)
                | Expr::Test(_, _)
                | Expr::Check { .. }
                | Expr::Inst { .. }
                | Expr::Asc(_)
                | Expr::Len(_)
                | Expr::Val(_)
                | Expr::Nrm(_)
                | Expr::Pos(_)
                | Expr::Fre(_)
                | Expr::StrCompare(_, _, _)
                | Expr::String(_) => {
                    self.unsafe_seen = true;
                }
                Expr::Func1(crate::ast::Func1::Rnd, _) => {
                    self.unsafe_seen = true;
                }
                // TI/ST/ER/EL: the runtime writes them, not the program.
                // Hoisting `TI > N` out of a wait loop freezes the clock.
                Expr::Var(v) if var_is_volatile(v) => {
                    self.unsafe_seen = true;
                }
                _ => crate::visit::walk_expr(self, e),
            }
        }

        // `TI$` is a `StrExpr::Var`, so the `Expr::Var` arm above never sees it.
        fn visit_str_expr(&mut self, s: &StrExpr) {
            if self.unsafe_seen {
                return;
            }
            if !str_reads_are_repeatable(s) {
                self.unsafe_seen = true;
                return;
            }
            crate::visit::walk_str_expr(self, s);
        }
    }
    let mut d = Det { unsafe_seen: false };
    d.visit_expr(e);
    d.unsafe_seen
}

/// Walk every statement in the loop body (from the position right
/// after the FOR up to and not including the NEXT) and call
/// `f(stmt)`. Returns `false` immediately if any call returns
/// `false`. The body may span multiple lines.
fn licm_walk_body(
    module: &ir::Module,
    fli: usize,
    fsi: usize,
    nli: usize,
    nsi: usize,
    mut f: impl FnMut(&Stmt) -> bool,
) -> bool {
    if fli == nli {
        for stmt in &module.lines[fli].stmts[fsi + 1..nsi] {
            if !f(stmt) {
                return false;
            }
        }
        return true;
    }
    for stmt in &module.lines[fli].stmts[fsi + 1..] {
        if !f(stmt) {
            return false;
        }
    }
    for line in &module.lines[fli + 1..nli] {
        for stmt in &line.stmts {
            if !f(stmt) {
                return false;
            }
        }
    }
    for stmt in &module.lines[nli].stmts[..nsi] {
        if !f(stmt) {
            return false;
        }
    }
    true
}

/// Walk every expression in the body, looking for maximal invariant
/// subexpressions (i.e. don't descend into a node that's already
/// invariant — hoist the whole thing). De-duplicates so each unique
/// candidate is hoisted to one temp.
fn licm_collect_candidates(
    module: &ir::Module,
    fli: usize,
    fsi: usize,
    nli: usize,
    nsi: usize,
    blocked: &HashSet<VarName>,
    out: &mut Vec<Expr>,
) {
    // FOR always runs the body at least once, so the top is safe to hoist.
    // After a branch-away, only hoist expressions that cannot trap.
    let mut speculative = false;
    licm_walk_body(module, fli, fsi, nli, nsi, |stmt| {
        licm_collect_in_stmt(stmt, blocked, out, speculative);
        if licm_stmt_can_branch_away(stmt) {
            speculative = true;
        }
        true
    });
}

/// True if this body statement can skip the rest of the body.
/// `licm_stmt_safe` already refused a bare `GOTO`, so this is `IF ... THEN <line>`.
fn licm_stmt_can_branch_away(stmt: &Stmt) -> bool {
    matches!(
        stmt,
        Stmt::If {
            then: ThenIr::Goto(_),
            ..
        }
    )
}

fn licm_collect_in_stmt(
    stmt: &Stmt,
    blocked: &HashSet<VarName>,
    out: &mut Vec<Expr>,
    speculative: bool,
) {
    match stmt {
        Stmt::Let { value, .. } => licm_collect_in_expr(value, blocked, out, speculative),
        Stmt::ArrayLet { indices, value, .. } => {
            for e in indices {
                licm_collect_in_expr(e, blocked, out, speculative);
            }
            licm_collect_in_expr(value, blocked, out, speculative);
        }
        Stmt::Poke { addr, value } | Stmt::Dpoke { addr, value } => {
            licm_collect_in_expr(addr, blocked, out, speculative);
            licm_collect_in_expr(value, blocked, out, speculative);
        }
        Stmt::PokeFill {
            dst_start,
            dst_end,
            value,
        } => {
            licm_collect_in_expr(dst_start, blocked, out, speculative);
            licm_collect_in_expr(dst_end, blocked, out, speculative);
            licm_collect_in_expr(value, blocked, out, speculative);
        }
        Stmt::Print { items, .. } => {
            for p in items {
                if let PrintPiece::Expr(e)
                | PrintPiece::CharOut(e)
                | PrintPiece::TabTo(e)
                | PrintPiece::Spc(e) = p
                {
                    licm_collect_in_expr(e, blocked, out, speculative);
                }
            }
        }
        Stmt::If { cond, then } => {
            // The condition runs whenever the IF is reached. The THEN body does not.
            licm_collect_in_expr(cond, blocked, out, speculative);
            if let ThenIr::Stmts(inner) = then {
                for s in inner {
                    licm_collect_in_stmt(s, blocked, out, true);
                }
            }
        }
        _ => {}
    }
}

fn licm_collect_in_expr(
    e: &Expr,
    blocked: &HashSet<VarName>,
    out: &mut Vec<Expr>,
    speculative: bool,
) {
    if licm_expr_invariant(e, blocked)
        && licm_expr_worth_hoisting(e)
        && (!speculative || expr_cannot_trap(e))
    {
        // Maximal: don't descend. Dedupe so identical sub-expressions
        // share one temp.
        if !out.iter().any(|x| x == e) {
            out.push(e.clone());
        }
        return;
    }
    // Descend into children to find smaller invariant candidates.
    match e {
        Expr::Neg(inner) | Expr::Not(inner) => {
            licm_collect_in_expr(inner, blocked, out, speculative);
        }
        Expr::Bin(_, l, r) => {
            licm_collect_in_expr(l, blocked, out, speculative);
            licm_collect_in_expr(r, blocked, out, speculative);
        }
        Expr::Func1(_, arg) => {
            licm_collect_in_expr(arg, blocked, out, speculative);
        }
        Expr::Peek(addr) | Expr::MemPeek(addr) => {
            licm_collect_in_expr(addr, blocked, out, speculative);
        }
        Expr::ArrayRef(_, idx) => {
            for e in idx {
                licm_collect_in_expr(e, blocked, out, speculative);
            }
        }
        _ => {}
    }
}

/// True iff every leaf in `e` is either a literal or a Var that's
/// not in `blocked`, AND every operator/function in `e` is in the
/// safe-to-hoist subset, AND `e` contains no memory/stateful nodes.
fn licm_expr_invariant(e: &Expr, blocked: &HashSet<VarName>) -> bool {
    if licm_expr_has_unsafe(e) {
        return false;
    }
    licm_expr_uses_blocked(e, blocked).is_none()
}

/// Returns the first blocked var found in `e`, or `None` if every var
/// reference is permitted.
fn licm_expr_uses_blocked<'a>(e: &'a Expr, blocked: &'a HashSet<VarName>) -> Option<&'a VarName> {
    match e {
        Expr::Number(_) => None,
        Expr::Var(v) => {
            if blocked.contains(v) {
                Some(v)
            } else {
                None
            }
        }
        Expr::Neg(inner) | Expr::Not(inner) => licm_expr_uses_blocked(inner, blocked),
        Expr::Bin(op, l, r) => {
            if !licm_binop_safe(*op) {
                // Pretend a fake var blocks this so the caller skips
                // hoisting through an unsafe operator.
                return licm_sentinel(blocked);
            }
            licm_expr_uses_blocked(l, blocked).or_else(|| licm_expr_uses_blocked(r, blocked))
        }
        Expr::Func1(f, arg) => {
            if !licm_func1_safe(*f) {
                return licm_sentinel(blocked);
            }
            licm_expr_uses_blocked(arg, blocked)
        }
        // Anything we don't explicitly recognise as pure: refuse.
        _ => licm_sentinel(blocked),
    }
}

/// Helper: returns a reference to any element in `blocked` (we just
/// need a non-`None` to signal "not invariant"). When `blocked` is
/// empty we still need to refuse — fall back to the loop var slot
/// the caller stuffed in. The caller always inserts the loop var,
/// so this never panics in practice.
fn licm_sentinel(blocked: &HashSet<VarName>) -> Option<&VarName> {
    blocked.iter().next()
}

fn licm_binop_safe(op: BinOp) -> bool {
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

fn licm_func1_safe(f: crate::ast::Func1) -> bool {
    use crate::ast::Func1;
    matches!(f, Func1::Abs | Func1::Int | Func1::Sgn)
}

/// "Worth hoisting" gate: at least one Bin or Func1 node. A bare
/// Var or Number is already cheap; hoisting it would just add a LET.
fn licm_expr_worth_hoisting(e: &Expr) -> bool {
    match e {
        Expr::Number(_) | Expr::Var(_) => false,
        Expr::Bin(_, _, _) | Expr::Func1(_, _) => true,
        Expr::Neg(inner) | Expr::Not(inner) => licm_expr_worth_hoisting(inner),
        _ => false,
    }
}

/// Pick the right `VarKind` for the temp. If every leaf is integer-
/// kinded and every op preserves int (which `licm_binop_safe`
/// guarantees), the result is integer; else float.
fn licm_infer_kind(e: &Expr) -> VarKind {
    // Integer leaves are not enough: `A% * B%` can exceed i16, and an Integer
    // temp would raise ?ILLEGAL QUANTITY. Bound the value instead.
    const MIN: f64 = -32768.0;
    const MAX: f64 = 32767.0;

    /// Conservative value range, or `None` if not provably integral.
    fn range(e: &Expr) -> Option<(f64, f64)> {
        match e {
            Expr::Number(n) if n.is_finite() && n.fract() == 0.0 && (MIN..=MAX).contains(n) => {
                Some((*n, *n))
            }
            Expr::Var(v) if v.kind == VarKind::Integer && !var_is_volatile(v) => Some((MIN, MAX)),
            Expr::Neg(inner) => range(inner).map(|(lo, hi)| (-hi, -lo)),
            // BASIC NOT is bitwise i16 complement: -(x+1), always in range.
            Expr::Not(inner) => range(inner).map(|_| (MIN, MAX)),
            Expr::Bin(op, l, r) => {
                let (a, b) = range(l)?;
                let (c, d) = range(r)?;
                match op {
                    // FCOMP yields 0 or -1.
                    BinOp::Eq
                    | BinOp::Ne
                    | BinOp::Lt
                    | BinOp::Le
                    | BinOp::Gt
                    | BinOp::Ge => Some((-1.0, 0.0)),
                    // AYINT-ed operands, bitwise result: always an i16.
                    BinOp::And | BinOp::Or | BinOp::Xor => Some((MIN, MAX)),
                    BinOp::Add => Some((a + c, b + d)),
                    BinOp::Sub => Some((a - d, b - c)),
                    BinOp::Mul => {
                        let p = [a * c, a * d, b * c, b * d];
                        Some((
                            p.iter().copied().fold(f64::INFINITY, f64::min),
                            p.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                        ))
                    }
                    _ => None,
                }
            }
            Expr::Func1(f, arg) => {
                let (lo, hi) = range(arg)?;
                match f {
                    // ABS(-32768) is 32768, one past i16.
                    crate::ast::Func1::Abs => {
                        Some((0.0, lo.abs().max(hi.abs())))
                    }
                    crate::ast::Func1::Int => Some((lo, hi)),
                    crate::ast::Func1::Sgn => Some((-1.0, 1.0)),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    match range(e) {
        Some((lo, hi)) if lo >= MIN && hi <= MAX => VarKind::Integer,
        _ => VarKind::Float,
    }
}

/// Walk the body and replace every occurrence of `target` with
/// `replacement`. Replacement happens at the deepest occurrence
/// first via post-order recursion.
fn licm_replace_in_body(
    module: &mut ir::Module,
    fli: usize,
    fsi: usize,
    nli: usize,
    nsi: usize,
    target: &Expr,
    replacement: &Expr,
) {
    if fli == nli {
        for stmt in &mut module.lines[fli].stmts[fsi + 1..nsi] {
            licm_replace_in_stmt(stmt, target, replacement);
        }
        return;
    }
    for stmt in &mut module.lines[fli].stmts[fsi + 1..] {
        licm_replace_in_stmt(stmt, target, replacement);
    }
    let body_lines = (fli + 1)..nli;
    for line in &mut module.lines[body_lines] {
        for stmt in &mut line.stmts {
            licm_replace_in_stmt(stmt, target, replacement);
        }
    }
    for stmt in &mut module.lines[nli].stmts[..nsi] {
        licm_replace_in_stmt(stmt, target, replacement);
    }
}

fn licm_replace_in_stmt(stmt: &mut Stmt, target: &Expr, replacement: &Expr) {
    match stmt {
        Stmt::Let { value, .. } => licm_replace_in_expr(value, target, replacement),
        Stmt::ArrayLet { indices, value, .. } => {
            for e in indices {
                licm_replace_in_expr(e, target, replacement);
            }
            licm_replace_in_expr(value, target, replacement);
        }
        Stmt::Poke { addr, value } | Stmt::Dpoke { addr, value } => {
            licm_replace_in_expr(addr, target, replacement);
            licm_replace_in_expr(value, target, replacement);
        }
        Stmt::PokeFill {
            dst_start,
            dst_end,
            value,
        } => {
            licm_replace_in_expr(dst_start, target, replacement);
            licm_replace_in_expr(dst_end, target, replacement);
            licm_replace_in_expr(value, target, replacement);
        }
        Stmt::Print { items, .. } => {
            for p in items.iter_mut() {
                if let PrintPiece::Expr(e)
                | PrintPiece::CharOut(e)
                | PrintPiece::TabTo(e)
                | PrintPiece::Spc(e) = p
                {
                    licm_replace_in_expr(e, target, replacement);
                }
            }
        }
        Stmt::If { cond, then } => {
            licm_replace_in_expr(cond, target, replacement);
            if let ThenIr::Stmts(inner) = then {
                for s in inner {
                    licm_replace_in_stmt(s, target, replacement);
                }
            }
        }
        _ => {}
    }
}

fn licm_replace_in_expr(e: &mut Expr, target: &Expr, replacement: &Expr) {
    if e == target {
        *e = replacement.clone();
        return;
    }
    match e {
        Expr::Neg(inner) | Expr::Not(inner) => licm_replace_in_expr(inner, target, replacement),
        Expr::Bin(_, l, r) => {
            licm_replace_in_expr(l, target, replacement);
            licm_replace_in_expr(r, target, replacement);
        }
        Expr::Func1(_, arg) => licm_replace_in_expr(arg, target, replacement),
        Expr::Peek(addr) | Expr::MemPeek(addr) => licm_replace_in_expr(addr, target, replacement),
        Expr::ArrayRef(_, idx) => {
            for e in idx {
                licm_replace_in_expr(e, target, replacement);
            }
        }
        _ => {}
    }
}

pub struct LoopInductionDetect;

impl ir::Pass for LoopInductionDetect {
    fn name(&self) -> &'static str {
        "loop-induction-detect"
    }

    fn run(&self, module: &mut ir::Module) -> Result<(), ir::PassError> {
        let pairs = collect_for_next_pairs(module);
        let mut assignments: Vec<((usize, usize), f64)> = Vec::new();
        for ((fli, fsi), (nli, nsi)) in &pairs {
            let (loop_var, step_literal) = match &module.lines[*fli].stmts[*fsi] {
                Stmt::For { var, step, .. } => (var.clone(), expr_as_f64_literal(step)),
                _ => unreachable!("position came from FOR scan"),
            };

            let Some(step_value) = step_literal else {
                continue;
            };
            // Sanity: skip FORs whose body is structurally unsound
            // for induction (writes to the loop var, GOSUB, FN
            // calls, nested FOR with same var). These mirror
            // `stmt_is_int_safe` so a body that can't take int-FOR
            // also won't get an induction slot.
            let body_for_safety = collect_body(module, *fli, *fsi, *nli, *nsi);
            if !body_for_safety
                .iter()
                .all(|s| stmt_is_int_safe(s, &loop_var))
            {
                continue;
            }
            let body = collect_body(module, *fli, *fsi, *nli, *nsi);
            let mut counts: Vec<(f64, usize)> = Vec::new();
            for stmt in &body {
                collect_induction_candidates(stmt, &loop_var, &mut counts);
            }
            if counts.is_empty() {
                continue;
            }
            // Keep only `K` values worth strength-reducing (skip the
            // shapes codegen already handles cheaply).
            counts.retain(|(k, _)| induction_worth_it(*k, step_value));
            if counts.is_empty() {
                continue;
            }
            // Pick the most-used `K`, ties broken by first seen.
            let best = counts
                .iter()
                .enumerate()
                .max_by_key(|(idx, (_, c))| (*c, std::cmp::Reverse(*idx)))
                .map(|(_, (k, _))| *k)
                .unwrap();
            assignments.push(((*fli, *fsi), best));
        }
        for ((fli, fsi), k) in assignments {
            if let Stmt::For {
                induction_const, ..
            } = &mut module.lines[fli].stmts[fsi]
            {
                *induction_const = Some(k);
            }
        }
        Ok(())
    }
}

/// Detects per-FOR array-pointer induction opportunities. For each
/// eligible shape, codegen materialises a 2-byte `AP_<n>` ZP-pointer
/// slot pre-computed to the FOR-start element at the header, advanced
/// by `step * axis_stride * elem_size` at NEXT, and read in place of
/// the per-iteration multiply chain at every matching access.
///
/// Conservative: requires `body_int_safe` (loop var can't be mutated
/// mid-body), a literal STEP (so the per-iteration advance folds), and
/// every access to the candidate array in the body must use one loop-var
/// axis (`loop_var + const`) while all other axes are literals. A single
/// mismatching access disqualifies the whole array.
pub struct ArrayPtrInductionDetect;

impl ir::Pass for ArrayPtrInductionDetect {
    fn name(&self) -> &'static str {
        "array-ptr-induction-detect"
    }

    fn run(&self, module: &mut ir::Module) -> Result<(), ir::PassError> {
        let pairs = collect_for_next_pairs(module);
        let mut assignments: Vec<((usize, usize), Vec<ArrayInduction>)> = Vec::new();
        for ((fli, fsi), (nli, nsi)) in &pairs {
            let (loop_var, step_is_literal) = match &module.lines[*fli].stmts[*fsi] {
                Stmt::For { var, step, .. } => (var.clone(), expr_as_f64_literal(step).is_some()),
                _ => unreachable!("position came from FOR scan"),
            };
            if !step_is_literal {
                continue;
            }
            // Loop var must not be mutated by the body — same gate
            // `LoopInductionDetect` uses, mirrors the int-FOR fast
            // path's body safety check.
            let body = collect_body(module, *fli, *fsi, *nli, *nsi);
            if !body.iter().all(|s| stmt_is_int_safe(s, &loop_var)) {
                continue;
            }
            // Per-array access classification: every `A(...)` ref or
            // write in the body either matches a supported induction
            // shape or disqualifies that array. Multiple literal
            // shapes for the same 2D array can each get their own AP.
            let mut classifier = ArrayPtrClassifier {
                loop_var: &loop_var,
                state: HashMap::new(),
                dimmed: HashSet::new(),
            };
            for stmt in &body {
                classifier.visit_stmt(stmt);
            }
            let mut safe: Vec<ArrayInduction> = Vec::new();
            for (arr, st) in classifier.state {
                if classifier.dimmed.contains(&arr) {
                    continue;
                }
                let ArrayPtrAccess::Matches(counts) = st else {
                    continue;
                };
                for (indices, n) in counts {
                    if array_ptr_break_even(&arr, n) {
                        safe.push(ArrayInduction {
                            name: arr.clone(),
                            indices,
                        });
                    }
                }
            }
            if safe.is_empty() {
                continue;
            }

            safe.sort_by(|a, b| {
                a.name
                    .label()
                    .cmp(&b.name.label())
                    .then_with(|| a.indices.cmp(&b.indices))
            });
            assignments.push(((*fli, *fsi), safe));
        }
        for ((fli, fsi), arrs) in assignments {
            if let Stmt::For {
                array_inductions, ..
            } = &mut module.lines[fli].stmts[fsi]
            {
                *array_inductions = arrs;
            }
        }
        Ok(())
    }
}

/// Break-even gate per array kind. The optimisation pays a per-FOR
/// constant cost (~26 bytes for init + per-NEXT advance) and saves a
/// per-access amount that depends on the OLD path's footprint and
/// how well peephole-factored it already is. The shared cost model
/// owns the final threshold.
fn array_ptr_break_even(arr: &VarName, n: u32) -> bool {
    let _ = arr;
    crate::opt_model::CostModel::default().array_ptr_induction_worth_it(n)
}

enum ArrayPtrAccess {
    /// All accesses so far use supported induction shapes. Counters
    /// track how many accesses share each exact base shape, which
    /// gates the constant init/NEXT overhead per AP slot.
    Matches(HashMap<Vec<ArrayInductionIndex>, u32>),
    /// At least one access used a different shape.
    Disqualified,
}

struct ArrayPtrClassifier<'a> {
    loop_var: &'a VarName,
    state: HashMap<VarName, ArrayPtrAccess>,
    /// Arrays redimensioned inside the loop body — never safe to
    /// pre-compute a pointer for them.
    dimmed: HashSet<VarName>,
}

impl<'a> ArrayPtrClassifier<'a> {
    fn note(&mut self, arr: &VarName, indices: &[Expr]) {
        let shape = array_ptr_indices_shape(indices, self.loop_var);
        let entry = self
            .state
            .entry(arr.clone())
            .or_insert_with(|| ArrayPtrAccess::Matches(HashMap::new()));
        match (entry, shape) {
            (ArrayPtrAccess::Disqualified, _) => {}
            (ArrayPtrAccess::Matches(counts), Some(shape)) => {
                *counts.entry(shape).or_insert(0) += 1;
            }
            (slot @ ArrayPtrAccess::Matches(_), None) => {
                *slot = ArrayPtrAccess::Disqualified;
            }
        }
    }

    fn visit_stmt(&mut self, stmt: &Stmt) {
        // Walk every expression in the statement to catch nested
        // ArrayRefs, plus per-stmt array-write shapes that don't
        // appear as expressions.
        match stmt {
            Stmt::ArrayLet {
                name,
                indices,
                value,
            } => {
                self.note(name, indices);
                for e in indices {
                    self.visit_expr(e);
                }
                self.visit_expr(value);
            }
            Stmt::ArrayLetStr {
                name,
                indices,
                value,
            } => {
                self.note(name, indices);
                for e in indices {
                    self.visit_expr(e);
                }
                self.visit_str_expr(value);
            }
            Stmt::Read(targets) | Stmt::Input { targets, .. } => {
                for t in targets {
                    if let ir::ReadTarget::Array { name, indices } = t {
                        self.note(name, indices);
                        for e in indices {
                            self.visit_expr(e);
                        }
                    }
                }
            }
            Stmt::InputFile { file_num, targets } => {
                self.visit_expr(file_num);
                for t in targets {
                    if let ir::ReadTarget::Array { name, indices } = t {
                        self.note(name, indices);
                        for e in indices {
                            self.visit_expr(e);
                        }
                    }
                }
            }
            Stmt::Dim(specs) => {
                for spec in specs {
                    self.dimmed.insert(spec.name.clone());
                }
            }
            Stmt::If { cond, then } => {
                self.visit_expr(cond);
                if let ThenIr::Stmts(inner) = then {
                    for s in inner {
                        self.visit_stmt(s);
                    }
                }
            }
            other => {
                use crate::visit::Visitor;
                let mut wrapped = ArrayPtrExprWalker { inner: self };
                wrapped.visit_stmt(0, other);
            }
        }
    }

    fn visit_expr(&mut self, e: &Expr) {
        use crate::visit::Visitor;
        let mut wrapped = ArrayPtrExprWalker { inner: self };
        wrapped.visit_expr(e);
    }

    fn visit_str_expr(&mut self, s: &StrExpr) {
        use crate::visit::Visitor;
        let mut wrapped = ArrayPtrExprWalker { inner: self };
        wrapped.visit_str_expr(s);
    }
}

fn array_ptr_indices_shape(
    indices: &[Expr],
    loop_var: &VarName,
) -> Option<Vec<ArrayInductionIndex>> {
    if indices.is_empty() {
        return None;
    }
    let mut loop_axes = 0_u8;
    let mut shape = Vec::with_capacity(indices.len());
    for index in indices {
        if array_ptr_index_offset(index, loop_var).is_some() {
            loop_axes += 1;
            shape.push(ArrayInductionIndex::LoopVar);
        } else if let Some(n) = expr_as_i16_literal(index) {
            shape.push(ArrayInductionIndex::Const(n));
        } else {
            return None;
        }
    }
    if loop_axes == 1 { Some(shape) } else { None }
}

struct ArrayPtrExprWalker<'a, 'b> {
    inner: &'b mut ArrayPtrClassifier<'a>,
}

impl<'a, 'b> crate::visit::Visitor for ArrayPtrExprWalker<'a, 'b> {
    fn visit_expr(&mut self, e: &Expr) {
        if let Expr::ArrayRef(name, indices) = e {
            self.inner.note(name, indices);
        }
        crate::visit::walk_expr(self, e);
    }
    fn visit_str_expr(&mut self, s: &StrExpr) {
        if let StrExpr::ArrayRef(name, indices) = s {
            self.inner.note(name, indices);
        }
        crate::visit::walk_str_expr(self, s);
    }
}

/// Mirrors `IntForBodyAnalysis::run`'s pairing logic. Returns
/// `((for_line, for_stmt), (next_line, next_stmt))` pairs in source
/// order, one per matched FOR/NEXT.
fn collect_for_next_pairs(module: &ir::Module) -> Vec<((usize, usize), (usize, usize))> {
    let mut pairs = Vec::new();
    let mut stack: Vec<(usize, usize)> = Vec::new();
    for (li, line) in module.lines.iter().enumerate() {
        for (si, stmt) in line.stmts.iter().enumerate() {
            match stmt {
                Stmt::For { .. } => stack.push((li, si)),
                Stmt::Next { vars } => {
                    for _ in vars {
                        if let Some(open) = stack.pop() {
                            pairs.push((open, (li, si)));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    pairs
}

fn collect_induction_candidates(stmt: &Stmt, target: &VarName, counts: &mut Vec<(f64, usize)>) {
    use crate::visit::Visitor;
    let mut det = InductionVisitor { target, counts };
    // line number is irrelevant for the visitor we're using.
    det.visit_stmt(0, stmt);
}

struct InductionVisitor<'a> {
    target: &'a VarName,
    counts: &'a mut Vec<(f64, usize)>,
}

impl<'a> crate::visit::Visitor for InductionVisitor<'a> {
    fn visit_expr(&mut self, e: &Expr) {
        if let Expr::Bin(BinOp::Mul, l, r) = e {
            let pair = match (l.as_ref(), r.as_ref()) {
                (Expr::Var(v), Expr::Number(k)) if v == self.target => Some(*k),
                (Expr::Number(k), Expr::Var(v)) if v == self.target => Some(*k),
                _ => None,
            };
            if let Some(k) = pair {
                if let Some(slot) = self.counts.iter_mut().find(|(kk, _)| {
                    // Distinguish by exact f64 bits to avoid matching
                    // `1.5` and `1.5000001` as the same constant.
                    kk.to_bits() == k.to_bits()
                }) {
                    slot.1 += 1;
                } else {
                    self.counts.push((k, 1));
                }
            }
        }
        crate::visit::walk_expr(self, e);
    }
}

/// True iff strength-reducing `var * K` is both profitable and sound.
fn induction_worth_it(k: f64, step: f64) -> bool {
    crate::opt_model::CostModel::default().loop_induction_worth_it(k, step)
}

fn stmt_reads_var(stmt: &Stmt, var: &VarName) -> bool {
    match stmt {
        Stmt::Print { items, .. } => items.iter().any(|p| match p {
            PrintPiece::Expr(e)
            | PrintPiece::CharOut(e)
            | PrintPiece::TabTo(e)
            | PrintPiece::Spc(e) => expr_reads_var(e, var),
            PrintPiece::StrExpr(s) => str_reads_var(s, var),
            _ => false,
        }),
        Stmt::Let { value, .. } => expr_reads_var(value, var),
        Stmt::LetStr { value, .. } => str_reads_var(value, var),
        Stmt::ArrayLet { indices, value, .. } => {
            indices.iter().any(|e| expr_reads_var(e, var)) || expr_reads_var(value, var)
        }
        Stmt::ArrayLetStr { indices, value, .. } => {
            indices.iter().any(|e| expr_reads_var(e, var)) || str_reads_var(value, var)
        }
        Stmt::If { cond, then } => expr_reads_var(cond, var) || then_ir_reads_var(then, var),
        Stmt::IfElse {
            cond,
            then,
            else_then,
        } => {
            expr_reads_var(cond, var)
                || then_ir_reads_var(then, var)
                || then_ir_reads_var(else_then, var)
        }
        Stmt::DoIf { cond } | Stmt::Until { cond } => expr_reads_var(cond, var),
        Stmt::ExitLoop { cond } => cond.as_ref().map_or(false, |e| expr_reads_var(e, var)),
        Stmt::ComputedGoto { target } => expr_reads_var(target, var),
        Stmt::Reset { target } => expr_reads_var(target, var),
        Stmt::Rcomp { then, else_then } => {
            then_ir_reads_var(then, var)
                || else_then
                    .as_ref()
                    .map_or(false, |branch| then_ir_reads_var(branch, var))
        }
        Stmt::OnKey { keys, .. } => str_reads_var(keys, var),
        Stmt::For {
            start, end, step, ..
        } => expr_reads_var(start, var) || expr_reads_var(end, var) || expr_reads_var(step, var),
        Stmt::Poke { addr, value } => expr_reads_var(addr, var) || expr_reads_var(value, var),
        Stmt::PokeFill {
            dst_start,
            dst_end,
            value,
        } => {
            expr_reads_var(dst_start, var)
                || expr_reads_var(dst_end, var)
                || expr_reads_var(value, var)
        }
        Stmt::Sys {
            addr,
            regs,
            param_vars,
            ..
        } => {
            expr_reads_var(addr, var)
                || regs.iter().any(|e| expr_reads_var(e, var))
                // A variable in the parameter text is read by the callee through the ROM's parsers.
                || param_vars.iter().any(|p| expr_reads_var(&p.value, var))
        }
        Stmt::Wait { addr, mask, eor } => {
            expr_reads_var(addr, var)
                || expr_reads_var(mask, var)
                || eor.as_ref().map_or(false, |e| expr_reads_var(e, var))
        }
        Stmt::Open {
            file_num,
            device,
            secondary,
            filename,
        } => {
            expr_reads_var(file_num, var)
                || device.as_ref().map_or(false, |e| expr_reads_var(e, var))
                || secondary.as_ref().map_or(false, |e| expr_reads_var(e, var))
                || filename.as_ref().map_or(false, |s| str_reads_var(s, var))
        }
        Stmt::Close { file_num } => expr_reads_var(file_num, var),
        Stmt::PrintFile {
            file_num, items, ..
        } => {
            expr_reads_var(file_num, var)
                || items.iter().any(|p| match p {
                    PrintPiece::Expr(e)
                    | PrintPiece::CharOut(e)
                    | PrintPiece::TabTo(e)
                    | PrintPiece::Spc(e) => expr_reads_var(e, var),
                    PrintPiece::StrExpr(s) => str_reads_var(s, var),
                    _ => false,
                })
        }
        Stmt::GetFile { file_num, .. } => expr_reads_var(file_num, var),
        Stmt::InputFile { file_num, .. } => expr_reads_var(file_num, var),
        Stmt::Cmd {
            file_num, items, ..
        } => {
            expr_reads_var(file_num, var)
                || items.iter().any(|p| match p {
                    PrintPiece::Expr(e)
                    | PrintPiece::CharOut(e)
                    | PrintPiece::TabTo(e)
                    | PrintPiece::Spc(e) => expr_reads_var(e, var),
                    PrintPiece::StrExpr(s) => str_reads_var(s, var),
                    _ => false,
                })
        }
        Stmt::Load {
            filename,
            device,
            secondary,
            load_addr,
        } => {
            str_reads_var(filename, var)
                || device.as_ref().map_or(false, |e| expr_reads_var(e, var))
                || secondary.as_ref().map_or(false, |e| expr_reads_var(e, var))
                || load_addr.as_ref().map_or(false, |e| expr_reads_var(e, var))
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
            str_reads_var(filename, var)
                || device.as_ref().map_or(false, |e| expr_reads_var(e, var))
                || secondary.as_ref().map_or(false, |e| expr_reads_var(e, var))
        }
        Stmt::Disk { command } => str_reads_var(command, var),
        Stmt::OnBranch { value, .. } => expr_reads_var(value, var),
        Stmt::Dpoke { addr, value } => expr_reads_var(addr, var) || expr_reads_var(value, var),
        Stmt::ScreenRect {
            row,
            col,
            width,
            height,
            ch,
            color,
            ..
        } => {
            [row, col, width, height]
                .iter()
                .any(|e| expr_reads_var(e, var))
                || ch.as_ref().map_or(false, |e| expr_reads_var(e, var))
                || color.as_ref().map_or(false, |e| expr_reads_var(e, var))
        }
        Stmt::ScreenMove {
            row,
            col,
            width,
            height,
            dest_row,
            dest_col,
        } => [row, col, width, height, dest_row, dest_col]
            .iter()
            .any(|e| expr_reads_var(e, var)),
        Stmt::ScreenScroll {
            row,
            col,
            width,
            height,
            ..
        } => [row, col, width, height]
            .iter()
            .any(|e| expr_reads_var(e, var)),
        Stmt::Color {
            border,
            background,
            pen,
        } => {
            border.as_ref().map_or(false, |e| expr_reads_var(e, var))
                || background
                    .as_ref()
                    .map_or(false, |e| expr_reads_var(e, var))
                || pen.as_ref().map_or(false, |e| expr_reads_var(e, var))
        }
        Stmt::MobEnable { index, .. } => expr_reads_var(index, var),
        Stmt::Cmob { color1, color2 } => expr_reads_var(color1, var) || expr_reads_var(color2, var),
        Stmt::Bckgnds {
            color0,
            color1,
            color2,
            color3,
        } => {
            expr_reads_var(color0, var)
                || expr_reads_var(color1, var)
                || expr_reads_var(color2, var)
                || expr_reads_var(color3, var)
        }
        Stmt::Cset { mode } => expr_reads_var(mode, var),
        Stmt::Pause { ticks, .. } => expr_reads_var(ticks, var),
        // Pure markers / control statements with no expressions.
        // (Repeat / Loop / EndLoop / Done / Else / Do / DoNull /
        //  Disable / Resume / Nrm / Return / Rem / End / Stop /
        //  Restore / DefFn / Run / Data / Dim / EndProc / ProcDef /
        //  ProcCall / Clr.)
        _ => false,
    }
}

fn then_ir_reads_var(then: &ThenIr, var: &VarName) -> bool {
    match then {
        ThenIr::Goto(_) => false,
        ThenIr::Stmts(inner) => inner.iter().any(|s| stmt_reads_var(s, var)),
    }
}

/// True iff any expression reads `var` — used by DeadLineElim to
/// keep lines that are still needed (the var being read on the
/// retained line means a write somewhere else still feeds it).
fn expr_reads_var(e: &Expr, var: &VarName) -> bool {
    use crate::visit::Visitor;
    let mut det = ReadsVar {
        target: var,
        found: false,
    };
    det.visit_expr(e);
    det.found
}

fn str_reads_var(s: &StrExpr, var: &VarName) -> bool {
    use crate::visit::Visitor;
    let mut det = ReadsVar {
        target: var,
        found: false,
    };
    det.visit_str_expr(s);
    det.found
}

struct ReadsVar<'a> {
    target: &'a VarName,
    found: bool,
}

impl<'a> crate::visit::Visitor for ReadsVar<'a> {
    fn visit_var_read(&mut self, v: &VarName) {
        if v == self.target {
            self.found = true;
        }
    }
}

/// True iff at codegen time some read of `target` inside `stmt` would
/// route through FAC instead of the int-FOR counter slot. Used by
/// `IntForBodyAnalysis` to decide whether the body needs V_var to be
/// kept in sync with the int counter — when the answer is "no FAC
/// reads", the per-iteration sync (and the one-shot setup sync at
/// the FOR header) can both be skipped.
///
/// Conservative on purpose: every shape this code isn't sure about
/// falls back to "needs FAC" so we never strand the body with a
/// stale V_var. The cases recognized as int-routable mirror the
/// codegen contexts where `int_for_counter_label` is consulted (array
/// indices, POKE/SYS/WAIT args, integer LET RHS, IF int-compare,
/// PRINT CharOut/TabTo/Spc, etc.).
fn stmt_loop_var_needs_fac(
    stmt: &Stmt,
    target: &VarName,
    induction: Option<f64>,
    for_lowering: ForLowering,
) -> bool {
    match stmt {
        Stmt::Let { var, value } => {
            // Integer-typed LHS routes the value through the int
            // island. Float LHS goes through FAC; even when a
            // shadow-int is selected, that's a dynamic codegen
            // decision so we stay conservative here.
            let in_int = var.kind == VarKind::Integer && var.base != "TI" && var.base != "ST";
            expr_loop_var_needs_fac(value, target, in_int, induction)
        }
        Stmt::LetStr { value, .. } => str_reads_var(value, target),
        Stmt::ArrayLet {
            indices,
            value,
            name,
        } => {
            // Indices are always evaluated in the int island. The
            // RHS routes through int when the array element is
            // integer-typed; for Float arrays the value flows
            // through FAC.
            let value_in_int = name.kind == VarKind::Integer;
            indices
                .iter()
                .any(|e| expr_loop_var_needs_fac(e, target, true, induction))
                || expr_loop_var_needs_fac(value, target, value_in_int, induction)
        }
        Stmt::ArrayLetStr { indices, value, .. } => {
            indices
                .iter()
                .any(|e| expr_loop_var_needs_fac(e, target, true, induction))
                || str_reads_var(value, target)
        }
        Stmt::Poke { addr, value } => {
            expr_loop_var_needs_fac(addr, target, true, induction)
                || expr_loop_var_needs_fac(value, target, true, induction)
        }
        Stmt::PokeFill {
            dst_start,
            dst_end,
            value,
        } => {
            expr_loop_var_needs_fac(dst_start, target, true, induction)
                || expr_loop_var_needs_fac(dst_end, target, true, induction)
                || expr_loop_var_needs_fac(value, target, true, induction)
        }
        Stmt::Sys {
            addr,
            regs,
            param_vars,
            ..
        } => {
            expr_loop_var_needs_fac(addr, target, true, induction)
                || regs
                    .iter()
                    .any(|e| expr_loop_var_needs_fac(e, target, true, induction))
                // Rendered with FOUT, which reads FAC.
                || param_vars
                    .iter()
                    .any(|p| expr_loop_var_needs_fac(&p.value, target, true, induction))
        }
        Stmt::Wait { addr, mask, eor } => {
            expr_loop_var_needs_fac(addr, target, true, induction)
                || expr_loop_var_needs_fac(mask, target, true, induction)
                || eor.as_ref().map_or(false, |e| {
                    expr_loop_var_needs_fac(e, target, true, induction)
                })
        }
        Stmt::If { cond, then } => {
            // GOTO-as-THEN can leave the loop with V_var stale — any
            // post-loop read sees the pre-FOR value (BSS init or
            // whatever was there). Force per-iteration sync by
            // reporting "needs FAC" so the codegen keeps V_var fresh
            // at every potential exit point. Stmts variants get
            // recursively scanned: a contained Goto/GoSub triggers
            // the same conservative path.
            if matches!(then, ThenIr::Goto(_)) {
                return true;
            }
            expr_loop_var_needs_fac(cond, target, true, induction)
                || matches!(
                    then,
                    ThenIr::Stmts(inner)
                        if inner.iter().any(|s| stmt_loop_var_needs_fac(
                            s,
                            target,
                            induction,
                            for_lowering,
                        ))
                )
        }
        Stmt::IfElse {
            cond,
            then,
            else_then,
        } => {
            expr_loop_var_needs_fac(cond, target, true, induction)
                || then_ir_loop_var_needs_fac(then, target, induction, for_lowering)
                || then_ir_loop_var_needs_fac(else_then, target, induction, for_lowering)
        }
        Stmt::DoIf { cond } | Stmt::Until { cond } => {
            expr_loop_var_needs_fac(cond, target, true, induction)
        }
        Stmt::ExitLoop { cond } => cond.as_ref().map_or(false, |cond| {
            expr_loop_var_needs_fac(cond, target, true, induction)
        }),
        Stmt::ComputedGoto { target: expr } => {
            let _ = expr_loop_var_needs_fac(expr, target, true, induction);
            true
        }
        Stmt::Rcomp { then, else_then } => {
            then_ir_loop_var_needs_fac(then, target, induction, for_lowering)
                || else_then.as_ref().map_or(false, |branch| {
                    then_ir_loop_var_needs_fac(branch, target, induction, for_lowering)
                })
        }
        Stmt::OnKey { keys, .. } => str_reads_var(keys, target),
        // primitives whose every operand evaluates in byte / int
        // context (int-island byte-eval or int16). Bare loop-var
        // reads inside these expressions DON'T need V_var sync —
        // the FU_/FI_ counter slot is the source of truth in the
        // body. Route through `expr_loop_var_needs_fac` with
        // `in_int=true` so the loop-var read on the int-island
        // path correctly returns `false` for FAC need.
        Stmt::Dpoke { addr, value } => {
            expr_loop_var_needs_fac(addr, target, true, induction)
                || expr_loop_var_needs_fac(value, target, true, induction)
        }
        Stmt::Color {
            border,
            background,
            pen,
        } => {
            border.as_ref().map_or(false, |e| {
                expr_loop_var_needs_fac(e, target, true, induction)
            }) || background.as_ref().map_or(false, |e| {
                expr_loop_var_needs_fac(e, target, true, induction)
            }) || pen.as_ref().map_or(false, |e| {
                expr_loop_var_needs_fac(e, target, true, induction)
            })
        }
        Stmt::MobEnable { index, .. } => expr_loop_var_needs_fac(index, target, true, induction),
        Stmt::Cmob { color1, color2 } => {
            expr_loop_var_needs_fac(color1, target, true, induction)
                || expr_loop_var_needs_fac(color2, target, true, induction)
        }
        Stmt::Bckgnds {
            color0,
            color1,
            color2,
            color3,
        } => {
            expr_loop_var_needs_fac(color0, target, true, induction)
                || expr_loop_var_needs_fac(color1, target, true, induction)
                || expr_loop_var_needs_fac(color2, target, true, induction)
                || expr_loop_var_needs_fac(color3, target, true, induction)
        }
        Stmt::Cset { mode } => expr_loop_var_needs_fac(mode, target, true, induction),
        Stmt::Pause { ticks, .. } => expr_loop_var_needs_fac(ticks, target, true, induction),
        // Bare GOTO/GOSUB/RETURN/ON-BRANCH inside the body:
        // conservatively force V_var sync so a jump out lands with
        // the loop var at its current value (not the pre-FOR stale
        // value). OnBranch is handled below for the value-expr's FAC
        // routing; we layer the sync requirement on top.
        Stmt::Goto { .. } | Stmt::GoSub { .. } | Stmt::Return => true,
        Stmt::For {
            start, end, step, ..
        } => {
            expr_loop_var_needs_fac(start, target, false, induction)
                || expr_loop_var_needs_fac(end, target, false, induction)
                || expr_loop_var_needs_fac(step, target, false, induction)
        }
        Stmt::Print { items, .. } => items
            .iter()
            .any(|p| print_piece_needs_fac(p, target, induction, for_lowering)),
        Stmt::PrintFile {
            file_num, items, ..
        }
        | Stmt::Cmd {
            file_num, items, ..
        } => {
            expr_loop_var_needs_fac(file_num, target, true, induction)
                || items
                    .iter()
                    .any(|p| print_piece_needs_fac(p, target, induction, for_lowering))
        }
        Stmt::OnBranch { value, .. } => {
            // Same out-of-loop concern as bare GOTO/GOSUB: the
            // dispatched target may sit outside the FOR and read the
            // loop var. Force sync regardless of how the value-expr
            // routes.
            let _ = expr_loop_var_needs_fac(value, target, true, induction);
            true
        }
        _ => stmt_reads_var(stmt, target),
    }
}

fn then_ir_loop_var_needs_fac(
    then: &ThenIr,
    target: &VarName,
    induction: Option<f64>,
    for_lowering: ForLowering,
) -> bool {
    match then {
        ThenIr::Goto(_) => true,
        ThenIr::Stmts(inner) => inner
            .iter()
            .any(|s| stmt_loop_var_needs_fac(s, target, induction, for_lowering)),
    }
}

/// Mirror of the path selection in `Codegen::emit_for`. We only care
/// about which slot codegen lowers the FOR-counter to (a 16-bit
/// integer slot, an 8-bit slot, or the float V_var slot itself), so
/// the analysis can predict whether a bare `Var(loop_var)` read in
/// the body routes through the int slot in any profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ForLowering {
    Int,
    U8,
    Float,
}

impl ForLowering {
    /// True iff a bare `Var(loop_var)` read inside the body always
    /// routes through the FOR's int slot in any profile (and so does
    /// not need V_var sync). False for the u8-FOR + Float-counter
    /// shape, where codegen's int-island gate is Speed-only and
    /// Default profile falls back to FAC (reading V_var directly).
    ///
    /// Currently consulted only by tests; kept here so the routing
    /// rule lives next to the enum it interprets.
    #[allow(dead_code)]
    fn bare_target_var_routes_through_int(self) -> bool {
        match self {
            // int-FOR: int_for_counter_label is consulted in
            // `is_int_island_addsub_only` with no profile gate.
            ForLowering::Int => true,
            // u8-FOR: the Var arm of `is_int_island_addsub_only`
            // gates u8-FOR-counter through `Profile::Speed`. In
            // Default the bare-Var read drops to FAC. Conservative:
            // be safe across both profiles.
            ForLowering::U8 => false,
            // Float-FOR: V_var IS the source of truth. The bare
            // read path is FAC-only, but V_var IS up to date by
            // construction (the float counter slot mirrors V_var),
            // so the optimization is irrelevant.
            ForLowering::Float => false,
        }
    }
}

fn classify_for_lowering(var: &VarName, start: &Expr, end: &Expr, step: &Expr) -> ForLowering {
    // u8-FOR with Integer-kind target reads through V_var via
    // 1-byte stores (kind already routes int), so the asymmetry is
    // moot — collapse into the int-FOR class.
    let is_integer_target = var.kind == VarKind::Integer && var.base != "TI" && var.base != "ST";
    let s = expr_as_i16_literal(start);
    let e = expr_as_i16_literal(end);
    let st = expr_as_f64_literal(step).and_then(f64_to_i16_lit);
    let (s, e, st) = match (s, e, st) {
        (Some(s), Some(e), Some(st)) => (s, e, st),
        _ => return ForLowering::Float,
    };
    if st == 0 {
        return ForLowering::Float;
    }
    let u8_eligible = u8_for_literal_params(s, e, st);
    if u8_eligible && !is_integer_target {
        ForLowering::U8
    } else {
        ForLowering::Int
    }
}

fn u8_for_literal_params(start: i16, end: i16, step: i16) -> bool {
    if step == 0
        || !(0..=255).contains(&start)
        || !(0..=255).contains(&end)
        || (step as i32).abs() > u8::MAX as i32
    {
        return false;
    }

    if step > 0 {
        if start > end {
            return false;
        }
        let span = end - start;
        let last = start + (span / step) * step;
        last + step <= u8::MAX as i16
    } else {
        if start < end {
            return false;
        }
        let span = start - end;
        let mag = -step;
        let last = start - (span / mag) * mag;
        last + step >= 0
    }
}

fn expr_as_i16_literal(e: &Expr) -> Option<i16> {
    if let Expr::Number(n) = e {
        return f64_to_i16_lit(*n);
    }
    None
}

fn array_ptr_index_offset(index: &Expr, loop_var: &VarName) -> Option<i16> {
    match index {
        Expr::Var(v) if v == loop_var => Some(0),
        Expr::Bin(BinOp::Add, l, r) => match (l.as_ref(), r.as_ref()) {
            (Expr::Var(v), Expr::Number(n)) if v == loop_var => f64_to_i16_lit(*n),
            (Expr::Number(n), Expr::Var(v)) if v == loop_var => f64_to_i16_lit(*n),
            _ => None,
        },
        Expr::Bin(BinOp::Sub, l, r) => match (l.as_ref(), r.as_ref()) {
            (Expr::Var(v), Expr::Number(n)) if v == loop_var => f64_to_i16_lit(*n)?.checked_neg(),
            _ => None,
        },
        _ => None,
    }
}

fn expr_as_f64_literal(e: &Expr) -> Option<f64> {
    match e {
        Expr::Number(n) => Some(*n),
        Expr::Neg(inner) => match inner.as_ref() {
            Expr::Number(n) => Some(-*n),
            _ => None,
        },
        _ => None,
    }
}

fn f64_to_i16_lit(n: f64) -> Option<i16> {
    if n.is_finite() && n.fract() == 0.0 && (-32768.0..=32767.0).contains(&n) {
        Some(n as i16)
    } else {
        None
    }
}

fn print_piece_needs_fac(
    piece: &PrintPiece,
    target: &VarName,
    induction: Option<f64>,
    for_lowering: ForLowering,
) -> bool {
    match piece {
        // A bare PRINT of an int/u8 FOR counter can be emitted via
        // __PRINT_INT16 from FI_/FU_, so V_var needn't be synced.
        PrintPiece::Expr(Expr::Var(v))
            if v == target && matches!(for_lowering, ForLowering::Int | ForLowering::U8) =>
        {
            false
        }
        // Other numeric PRINT expressions go through FAC.
        PrintPiece::Expr(e) => expr_loop_var_needs_fac(e, target, false, induction),
        // CharOut/Tab/Spc expect a byte — int island all the way.
        PrintPiece::CharOut(e) | PrintPiece::TabTo(e) | PrintPiece::Spc(e) => {
            expr_loop_var_needs_fac(e, target, true, induction)
        }
        PrintPiece::StrExpr(s) => str_reads_var(s, target),
        _ => false,
    }
}

/// True iff some read of `target` inside `e` would route through
/// FAC. `in_int` records whether the parent context is an int sink;
/// operators that the int island can keep flat (Add/Sub, plus
/// power-of-two Mul) propagate the flag, others clear it. Anything
/// we don't model explicitly (Func1, FnCall, Pow, comparisons with
/// non-leaf operands, ...) routes its children through FAC.
/// True when codegen will be forced to materialise `e` in FAC for
/// any Add/Sub it participates in — the other operand then has to
/// route through FAC too. Excludes the loop `target` itself: even
/// when the target Var has `kind=Float` in the IR, IntPromote may
/// store it in an integer slot, and codegen handles those reads via
/// the int island. Conservatively flags OTHER Float Vars (LICM
/// intermediates, unrelated scalars), fractional literals, and
/// arithmetic shapes codegen always lowers via FAC.
fn forces_fac(e: &Expr, target: &VarName) -> bool {
    match e {
        Expr::Var(v) => v.kind == VarKind::Float && v.base != "TI" && v.base != "ST" && v != target,
        Expr::Number(n) => !n.is_finite() || n.fract() != 0.0 || !(-32768.0..=32767.0).contains(n),
        Expr::Neg(inner) | Expr::Not(inner) => forces_fac(inner, target),
        Expr::Bin(BinOp::Add | BinOp::Sub, l, r) => forces_fac(l, target) || forces_fac(r, target),
        Expr::Bin(BinOp::Mul, l, r) => {
            // Pot2 multiplications stay in the int island as
            // ASL/ROL chains regardless of operand kinds — those
            // truly never read V_var via FAC.
            let pot2 = is_pot2_literal(l) || is_pot2_literal(r);
            if pot2 {
                return false;
            }
            // Number*Number folds at compile time — no runtime
            // FAC, no V_var read.
            if matches!(l.as_ref(), Expr::Number(_)) && matches!(r.as_ref(), Expr::Number(_)) {
                return false;
            }
            // Anything else with a Var operand: codegen frequently
            // routes the multiplication through FMULT (FAC), even
            // for "leaf * leaf" shapes — `Y1 * 6` lands on the
            // `__FMUL_Y1` stub once the (Var, Mul) use-count
            // threshold trips, which loads V_Y1 as a 5-byte float
            // and pulls the surrounding Add/Sub into FAC. The
            // earlier `leaf_leaf → not-forced` rule was too
            // optimistic: for FOR-counter-loop vars in scope, it
            // left V_var unsynced even though the body actually
            // read it. Keep the float mirror synced when a helper
            // pulls the expression out of the integer island.
            true
        }
        // Comparisons, bitwise AND/OR/XOR — int-island shapes.
        // FN call / Func1 / Pow / Div / string things → FAC.
        _ => true,
    }
}

fn expr_loop_var_needs_fac(
    e: &Expr,
    target: &VarName,
    in_int: bool,
    induction: Option<f64>,
) -> bool {
    // Loop induction sub-tree: when `e` is exactly the
    // `Var(target) * K` shape that codegen will swap for a single
    // MOVFM from the strength-reduced FB_ slot, the multiplication
    // and the `target` read disappear before they ever touch FAC,
    // so this sub-tree contributes no FAC dependency on V_var.
    if let Some(k) = induction {
        if let Expr::Bin(BinOp::Mul, l, r) = e {
            let matches = match (l.as_ref(), r.as_ref()) {
                (Expr::Var(v), Expr::Number(kk)) => v == target && kk.to_bits() == k.to_bits(),
                (Expr::Number(kk), Expr::Var(v)) => v == target && kk.to_bits() == k.to_bits(),
                _ => false,
            };
            if matches {
                return false;
            }
        }
    }
    match e {
        Expr::Var(v) if v == target => !in_int,
        Expr::Var(_) | Expr::Number(_) | Expr::String(_) => false,
        Expr::Neg(inner) => expr_loop_var_needs_fac(inner, target, in_int, induction),
        Expr::Bin(BinOp::Add, l, r) | Expr::Bin(BinOp::Sub, l, r) => {
            // If either operand evaluates as a Float (LICM-hoisted
            // intermediate, Float scalar, fractional literal),
            // codegen lowers the Add/Sub through FAC and pulls the
            // other side along — so target reads inside that other
            // side go through FAC too.
            let inner = in_int && !forces_fac(l, target) && !forces_fac(r, target);
            expr_loop_var_needs_fac(l, target, inner, induction)
                || expr_loop_var_needs_fac(r, target, inner, induction)
        }
        Expr::Bin(BinOp::And, l, r) | Expr::Bin(BinOp::Or, l, r) | Expr::Bin(BinOp::Xor, l, r) => {
            // Bitwise AND/OR stay in the int island when both
            // operands are int-island-eligible. Codegen lowers to
            // direct AND/ORA on the int slot (no FAC roundtrip),
            // so the target read inside follows the same routing
            // rules as Add/Sub.
            expr_loop_var_needs_fac(l, target, in_int, induction)
                || expr_loop_var_needs_fac(r, target, in_int, induction)
        }
        Expr::Not(inner) => expr_loop_var_needs_fac(inner, target, in_int, induction),
        Expr::Bin(BinOp::Mul, l, r) => {
            // Pot2 mul stays in the int island as an ASL/ROL chain;
            // leaf*leaf mul lowers to native MUL16 (which codegen
            // enables in every profile when both sides are int
            // leaves). Anything else drops into FAC FMULT or
            // profile-gated MUL16, so we conservatively clear
            // `in_int`.
            let pot2 = is_pot2_literal(l) || is_pot2_literal(r);
            let leaf_leaf = is_int_leaf_for_body(l, target) && is_int_leaf_for_body(r, target);
            let inner = in_int && (pot2 || leaf_leaf);
            expr_loop_var_needs_fac(l, target, inner, induction)
                || expr_loop_var_needs_fac(r, target, inner, induction)
        }
        Expr::Peek(addr) | Expr::MemPeek(addr) => {
            expr_loop_var_needs_fac(addr, target, true, induction)
        }
        Expr::ArrayRef(_, indices) => indices
            .iter()
            .any(|i| expr_loop_var_needs_fac(i, target, true, induction)),
        Expr::Bin(BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge, l, r) => {
            // Codegen takes the int-compare path only when both
            // operands fit the int island. When that holds, target
            // reads in either operand route through the int slot;
            // otherwise the compare drops to FAC and reads V_var.
            let int_compare =
                is_int_island_eligible(l, target) && is_int_island_eligible(r, target);
            let inner = in_int && int_compare;
            expr_loop_var_needs_fac(l, target, inner, induction)
                || expr_loop_var_needs_fac(r, target, inner, induction)
        }
        // Catch-all: any read of `target` here needs FAC. Captures
        // Func1, FnCall, Pos/Fre/Usr, And/Or, Pow, Not, Len/Asc/Val,
        // StrCompare.
        _ => expr_reads_var(e, target),
    }
}

/// Mirror of `Codegen::is_int_island_addsub_only` restricted to the
/// shapes we can statically verify from a FOR-body perspective.
/// Returns true when codegen will lower `e` entirely through the int
/// island (no FAC fallback) inside this FOR's body.
fn is_int_island_eligible(e: &Expr, target: &VarName) -> bool {
    match e {
        Expr::Number(n) => n.is_finite() && n.fract() == 0.0 && (-32768.0..=32767.0).contains(n),
        Expr::Var(v) => {
            v == target || (v.kind == VarKind::Integer && v.base != "TI" && v.base != "ST")
        }
        Expr::Neg(inner) => match inner.as_ref() {
            Expr::Number(n) => {
                let neg = -*n;
                neg.is_finite() && neg.fract() == 0.0 && (-32768.0..=32767.0).contains(&neg)
            }
            _ => false,
        },
        Expr::Bin(BinOp::Add | BinOp::Sub, l, r) => {
            is_int_island_eligible(l, target) && is_int_island_eligible(r, target)
        }
        Expr::Bin(BinOp::Mul, l, r) => {
            (is_pot2_literal(l) && is_int_island_eligible(r, target))
                || (is_pot2_literal(r) && is_int_island_eligible(l, target))
                || (is_int_leaf_for_body(l, target) && is_int_leaf_for_body(r, target))
        }
        _ => false,
    }
}

fn is_pot2_literal(e: &Expr) -> bool {
    if let Expr::Number(n) = e {
        if *n > 0.0 && n.fract() == 0.0 && *n <= u32::MAX as f64 {
            let v = *n as u32;
            return v.is_power_of_two();
        }
    }
    false
}

/// True iff `e` looks like an int leaf from the perspective of a FOR
/// body: an i16 literal, an integer-typed var, or the FOR's own
/// counter variable. Mirrors the cases `Codegen::int16_leaf` returns
/// `Some(_)` for inside an active FOR body — except for shadow-int
/// promotion, which is a dynamic decision we can't observe here, so
/// we err on the side of "not an int leaf".
fn is_int_leaf_for_body(e: &Expr, target: &VarName) -> bool {
    match e {
        Expr::Var(v) => {
            v == target || (v.kind == VarKind::Integer && v.base != "TI" && v.base != "ST")
        }
        Expr::Number(n) => n.is_finite() && n.fract() == 0.0 && (-32768.0..=32767.0).contains(n),
        Expr::Neg(inner) => match inner.as_ref() {
            Expr::Number(n) => {
                let neg = -*n;
                neg.is_finite() && neg.fract() == 0.0 && (-32768.0..=32767.0).contains(&neg)
            }
            _ => false,
        },
        _ => false,
    }
}

fn collect_body<'a>(
    module: &'a ir::Module,
    fli: usize,
    fsi: usize,
    nli: usize,
    nsi: usize,
) -> Vec<&'a Stmt> {
    let mut out = Vec::new();
    if fli == nli {
        for s in &module.lines[fli].stmts[fsi + 1..nsi] {
            out.push(s);
        }
    } else {
        for s in &module.lines[fli].stmts[fsi + 1..] {
            out.push(s);
        }
        for li in (fli + 1)..nli {
            for s in &module.lines[li].stmts {
                out.push(s);
            }
        }
        for s in &module.lines[nli].stmts[..nsi] {
            out.push(s);
        }
    }
    out
}

fn scan_for_counters_with_unsafe_body(module: &ir::Module) -> HashSet<VarName> {
    let literal_ranges = literal_scalar_i16_ranges(module);
    // Header safety applies to every FOR, including an inline conditional header and a header whose runtime NEXT precedes it in the source.
    struct Headers<'a> {
        ranges: &'a HashMap<VarName, crate::dataflow::IntRange>,
        unsafe_counters: HashSet<VarName>,
    }
    impl crate::visit::Visitor for Headers<'_> {
        fn visit_stmt(&mut self, line: u16, stmt: &Stmt) {
            if let Stmt::For {
                var, start, end, step, ..
            } = stmt
            {
                let end_range = expr_as_i16_literal(end)
                    .map(|e| crate::dataflow::IntRange::singleton(e as i32))
                    .or_else(|| match end {
                        Expr::Var(v) => self.ranges.get(v).copied(),
                        _ => None,
                    });
                let exit_fits = match (
                    expr_as_i16_literal(start),
                    end_range,
                    expr_as_i16_literal(step),
                ) {
                    (Some(s), Some(e), Some(st)) if st != 0 => {
                        crate::codegen::int_for_exit_fits_i16(
                            (s as i32).min(e.min),
                            (s as i32).max(e.max),
                            st as i32,
                        )
                    }
                    _ => false,
                };
                if !exit_fits || crate::codegen::for_bounds_read_loop_var(var, end, step) {
                    self.unsafe_counters.insert(var.clone());
                }
            }
            crate::visit::walk_stmt(self, line, stmt);
        }
    }
    let mut headers = Headers {
        ranges: &literal_ranges,
        unsafe_counters: unproved_mutated_for_counters(module, &literal_ranges),
    };
    crate::visit::Visitor::visit_module(&mut headers, module);
    let mut unsafe_counters = headers.unsafe_counters;
    let mut pairs: Vec<((usize, usize), (usize, usize))> = Vec::new();
    let mut stack: Vec<(usize, usize)> = Vec::new();
    for (li, line) in module.lines.iter().enumerate() {
        for (si, stmt) in line.stmts.iter().enumerate() {
            match stmt {
                Stmt::For { .. } => stack.push((li, si)),
                Stmt::Next { vars } => {
                    for _ in vars {
                        if let Some(open) = stack.pop() {
                            pairs.push((open, (li, si)));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    let routines = build_routine_writes(module);
    for ((fli, fsi), (nli, nsi)) in pairs {
        let Stmt::For { var: loop_var, .. } = &module.lines[fli].stmts[fsi] else {
            continue;
        };
        let body = collect_body(module, fli, fsi, nli, nsi);
        if !body
            .iter()
            .all(|s| stmt_is_int_safe_with(s, loop_var, Some(&routines)))
        {
            unsafe_counters.insert(loop_var.clone());
        }
    }
    unsafe_counters
}

/// Explicit scalar writes to any FOR variable, wherever control flow places them.
fn explicit_for_counter_writes(module: &ir::Module) -> HashSet<VarName> {
    use crate::visit::Visitor;
    struct Writes(HashSet<VarName>);
    impl Visitor for Writes {
        fn visit_stmt(&mut self, line: u16, stmt: &Stmt) {
            if let Stmt::Let { var, .. } = stmt {
                self.0.insert(var.clone());
            } else if !matches!(stmt, Stmt::For { .. }) {
                self.0.extend(stmt_counter_defs(stmt));
            }
            crate::visit::walk_stmt(self, line, stmt);
        }
    }
    let counters = collect_for_counter_vars(module);
    let mut writes = Writes(HashSet::new());
    writes.visit_module(module);
    writes.0.retain(|v| counters.contains(v));
    writes.0
}

/// Writes before initialization or after a proved Static loop completes cannot mutate its live private counter.
fn reachable_for_counter_writes(module: &ir::Module) -> HashSet<VarName> {
    let mut writes = explicit_for_counter_writes(module);
    if writes.is_empty()
        || module.has_on_key()
        || module.has_error_continuation()
        || module.lines.iter().any(|line| {
            line.stmts.iter().any(|stmt| effect_summary_for_stmt(stmt).opaque_write)
        })
    {
        return writes;
    }
    let lifetimes = crate::for_next_runtime::analyze(module);
    let cfg = crate::cfg::Cfg::build_with(module, &lifetimes);
    writes.retain(|var| {
        let headers = (0..cfg.nodes.len())
            .filter(|id| matches!(cfg.stmt_at(*id, module), Stmt::For { var: v, .. } if v == var))
            .collect::<Vec<_>>();
        for header in headers {
            let node = &cfg.nodes[header];
            let header_id = crate::for_next_runtime::StmtId {
                line_no: node.line_no,
                line_idx: node.stmt.line_idx,
                path: node.stmt.path.clone(),
            };
            let mut completed_at = HashSet::new();
            if lifetimes.for_tier(node.line_no, node.stmt.line_idx, &node.stmt.path)
                == crate::for_next_runtime::ForTier::Static
            {
                for (id, next) in cfg.nodes.iter().enumerate() {
                    let next_id = crate::for_next_runtime::StmtId {
                        line_no: next.line_no,
                        line_idx: next.stmt.line_idx,
                        path: next.stmt.path.clone(),
                    };
                    if lifetimes.static_pairs.get(&next_id).is_some_and(|partners| {
                        partners.iter().any(|partner| partner.as_ref() == Some(&header_id))
                    }) {
                        completed_at.insert(id);
                    }
                }
            }
            let mut pending = vec![header];
            let mut seen = HashSet::new();
            while let Some(id) = pending.pop() {

                if completed_at.contains(&id) || !seen.insert(id) {
                    continue;
                }
                let stmt = cfg.stmt_at(id, module);
                if matches!(stmt, Stmt::Let { var: v, .. } if v == var)
                    || (!matches!(stmt, Stmt::For { .. }) && stmt_counter_defs(stmt).contains(var))
                {
                    return true;
                }
                pending.extend(cfg.nodes[id].successors.iter().copied());
            }
        }
        false
    });
    writes
}

fn unproved_mutated_for_counters(
    module: &ir::Module,
    literal_ranges: &HashMap<VarName, crate::dataflow::IntRange>,
) -> HashSet<VarName> {
    let mut candidates = explicit_for_counter_writes(module);
    candidates.retain(|v| v.kind == VarKind::Float);
    if candidates.is_empty() {
        return candidates;
    }
    let cfg = crate::cfg::Cfg::build(module);
    candidates.retain(|var| !mutated_for_counter_domain_fits(module, &cfg, var, literal_ranges));
    candidates
}

fn mutated_for_counter_domain_fits(
    module: &ir::Module,
    cfg: &crate::cfg::Cfg,
    var: &VarName,
    literal_ranges: &HashMap<VarName, crate::dataflow::IntRange>,
) -> bool {
    let bound = |expr: &Expr| {
        expr_as_i16_literal(expr)
            .map(|n| PromRange::new(n as i64, n as i64))
            .or_else(|| match expr {
                Expr::Var(v) => literal_ranges
                    .get(v)
                    .map(|r| PromRange::new(r.min as i64, r.max as i64)),
                _ => None,
            })
    };
    let mut domain = PromRange::new(0, 0);
    let mut headers = Vec::new();
    let mut steps = HashMap::new();
    for id in 0..cfg.nodes.len() {
        match cfg.stmt_at(id, module) {
            Stmt::For {
                var: v, start, end, step, ..
            } if v == var => {
                let (Some(s), Some(e), Some(st)) =
                    (bound(start), bound(end), expr_as_i16_literal(step))
                else {
                    return false;
                };
                if st == 0 || crate::codegen::for_bounds_read_loop_var(var, end, step) {
                    return false;
                }
                domain = domain.join(s).join(e);
                headers.push((id, st as i64));
            }
            Stmt::Let { var: v, value } if v == var => match classify_counter_write(var, value) {
                Some(CounterWrite::Init(r)) => domain = domain.join(r),
                Some(CounterWrite::Step { k, .. }) => {
                    steps.insert(id, k);
                }
                None => return false,
            },
            stmt if !matches!(stmt, Stmt::For { .. }) && stmt_counter_defs(stmt).contains(var) => {
                return false;
            }
            _ => {}
        }
    }
    let mut covered_steps = HashSet::new();
    for (header, step) in headers {
        let mut total = 0_i64;
        if !steps.is_empty() {
            // Admit a straight control-flow path to NEXT, including GOTOs outside the lexical body.
            let mut region = HashSet::new();
            let mut node = header;
            loop {
                let [next] = cfg.nodes[node].successors.as_slice() else {
                    return false;
                };
                node = *next;
                if node == header || !region.insert(node) {
                    return false;
                }
                let stmt = cfg.stmt_at(node, module);
                if let Stmt::Next { vars } = stmt {
                    if vars.len() != 1 || vars[0].as_ref().is_some_and(|v| v != var) {
                        return false;
                    }
                    break;
                }
                if matches!(stmt, Stmt::For { .. } | Stmt::GoSub { .. } | Stmt::Return)
                    || effect_summary_for_stmt(stmt).opaque_write
                {
                    return false;
                }
                if let Some(k) = steps.get(&node) {
                    if k.signum() != step.signum() {
                        return false;
                    }
                    total += k;
                    covered_steps.insert(node);
                }
            }
            if region.iter().any(|id| {
                cfg.nodes[*id].predecessors.iter()
                    .any(|pred| *pred != header && !region.contains(pred))
            }) {
                return false;
            }
        }
        let extra = total + step;
        let crossing = if extra > 0 {
            PromRange::new(domain.min, domain.max + extra)
        } else {
            PromRange::new(domain.min + extra, domain.max)
        };
        if !crossing.fits_i16() {
            return false;
        }
    }
    steps.keys().all(|id| covered_steps.contains(id))
}

/// A whole-program range for scalars written only with i16 literals.
pub(crate) fn literal_scalar_i16_ranges(
    module: &ir::Module,
) -> HashMap<VarName, crate::dataflow::IntRange> {
    if module.has_on_key()
        || module.has_error_continuation()
        || module.lines.iter().any(|line| {
            line.stmts
                .iter()
                // CLR only restores the zero already present in every hull.
                .any(|stmt| !matches!(stmt, Stmt::Clr) && effect_summary_for_stmt(stmt).opaque_write)
        })
    {
        return HashMap::new();
    }
    let mut writes = WriteCollector {
        defs: HashMap::new(),
        bad: HashSet::new(),
        scalar_used: HashSet::new(),
        array_used: HashSet::new(),
    };
    crate::visit::walk_module(&mut writes, module);
    writes.bad.extend(collect_for_counter_vars(module));
    writes
        .defs
        .into_iter()
        .filter(|(v, _)| {
            v.kind != VarKind::String
                && !matches!(v.base.as_str(), "TI" | "ST" | "ER" | "EL")
                && !writes.bad.contains(v)
                && !writes.array_used.contains(v)
        })
        .filter_map(|(var, values)| {
            let mut range = crate::dataflow::IntRange::singleton(0);
            for value in values {
                let n = expr_as_i16_literal(&value)?;
                range = range.join(crate::dataflow::IntRange::singleton(n as i32));
            }
            Some((var, range))
        })
        .collect()
}

/// Per-GOSUB-target summary: the variables a routine may write, or
/// `Unknown` when control flow escapes what we can follow (computed
/// GOTO, RESUME, CLR, recursion, a missing target line, an opaque
/// `FnCall`, etc.). Over-approximating the write set is sound for the
/// "does this routine touch the loop counter" question — extra entries
/// only make a GOSUB look unsafe, never the reverse.
#[derive(Clone)]
enum RoutineEffect {
    Writes(HashSet<VarName>),
    Unknown,
}

type RoutineWrites = HashMap<u16, RoutineEffect>;

enum RoutineCf {
    FallThrough,
    Stop,
    Escaped,
}

/// Build a write-set summary for every line referenced as a GOSUB
/// target. Lets `stmt_is_int_safe` resolve a GOSUB instead of always
/// bailing: a GOSUB is safe for an int-FOR counter when the routine it
/// (transitively) reaches never writes that counter.
fn build_routine_writes(module: &ir::Module) -> RoutineWrites {
    let line_index: HashMap<u16, usize> = module
        .lines
        .iter()
        .enumerate()
        .map(|(i, l)| (l.number, i))
        .collect();
    let mut targets: HashSet<u16> = HashSet::new();
    for line in &module.lines {
        for s in &line.stmts {
            gather_routine_gosub_targets(s, &mut targets);
        }
    }
    let mut memo: RoutineWrites = HashMap::new();
    for t in targets {
        let mut visiting = HashSet::new();
        routine_effect(t, module, &line_index, &mut memo, &mut visiting);
    }
    memo
}

fn gather_routine_gosub_targets(stmt: &Stmt, out: &mut HashSet<u16>) {
    match stmt {
        Stmt::GoSub { target } => {
            out.insert(*target);
        }
        Stmt::OnBranch {
            kind: OnBranchKind::GoSub,
            targets,
            ..
        } => out.extend(targets.iter().copied()),
        Stmt::If { then, .. } => gather_routine_gosub_targets_in_then(then, out),
        Stmt::IfElse {
            then, else_then, ..
        } => {
            gather_routine_gosub_targets_in_then(then, out);
            gather_routine_gosub_targets_in_then(else_then, out);
        }
        Stmt::Rcomp { then, else_then } => {
            gather_routine_gosub_targets_in_then(then, out);
            if let Some(b) = else_then {
                gather_routine_gosub_targets_in_then(b, out);
            }
        }
        _ => {}
    }
}

fn gather_routine_gosub_targets_in_then(then: &ThenIr, out: &mut HashSet<u16>) {
    if let ThenIr::Stmts(inner) = then {
        for s in inner {
            gather_routine_gosub_targets(s, out);
        }
    }
}

fn routine_effect(
    target: u16,
    module: &ir::Module,
    line_index: &HashMap<u16, usize>,
    memo: &mut RoutineWrites,
    visiting: &mut HashSet<u16>,
) -> RoutineEffect {
    if let Some(e) = memo.get(&target) {
        return e.clone();
    }
    if !visiting.insert(target) {
        return RoutineEffect::Unknown; // recursion — bail conservatively
    }
    let result = compute_routine_effect(target, module, line_index, memo, visiting);
    visiting.remove(&target);
    memo.insert(target, result.clone());
    result
}

fn compute_routine_effect(
    target: u16,
    module: &ir::Module,
    line_index: &HashMap<u16, usize>,
    memo: &mut RoutineWrites,
    visiting: &mut HashSet<u16>,
) -> RoutineEffect {
    let Some(&start) = line_index.get(&target) else {
        return RoutineEffect::Unknown;
    };
    let mut writes = HashSet::new();
    let mut seen: HashSet<usize> = HashSet::new();
    let mut stack = vec![start];
    while let Some(li) = stack.pop() {
        if !seen.insert(li) {
            continue;
        }
        let stmts = &module.lines[li].stmts;
        match scan_routine_stmts(
            stmts, false, module, line_index, memo, visiting, &mut writes, &mut stack,
        ) {
            RoutineCf::Escaped => return RoutineEffect::Unknown,
            RoutineCf::FallThrough => {
                if li + 1 < module.lines.len() {
                    stack.push(li + 1);
                }
            }
            RoutineCf::Stop => {}
        }
    }
    RoutineEffect::Writes(writes)
}

/// Walk one line's statements (or an inline IF body when `conditional`),
/// collecting writes and pushing reachable line indices. A synthetic
/// probe var detects statement-level hazards that don't depend on a
/// specific variable (opaque `FnCall`, etc.): `stmt_is_int_safe` against
/// a name no real code writes is false only for those.
fn scan_routine_stmts(
    stmts: &[Stmt],
    conditional: bool,
    module: &ir::Module,
    line_index: &HashMap<u16, usize>,
    memo: &mut RoutineWrites,
    visiting: &mut HashSet<u16>,
    writes: &mut HashSet<VarName>,
    stack: &mut Vec<usize>,
) -> RoutineCf {
    let probe = VarName {
        base: "\u{1}probe".to_string(),
        kind: VarKind::Float,
    };
    let push_target = |t: &u16, stack: &mut Vec<usize>| -> bool {
        match line_index.get(t) {
            Some(&idx) => {
                stack.push(idx);
                true
            }
            None => false,
        }
    };
    for stmt in stmts {
        match stmt {
            Stmt::Return | Stmt::End | Stmt::Stop | Stmt::RaiseError(_) | Stmt::Hang => {
                if conditional {
                    continue;
                }
                return RoutineCf::Stop;
            }
            Stmt::Goto { target } => {
                if !push_target(target, stack) {
                    return RoutineCf::Escaped;
                }
                if conditional {
                    continue;
                }
                return RoutineCf::Stop;
            }
            Stmt::GoSub { target } => match routine_effect(*target, module, line_index, memo, visiting) {
                RoutineEffect::Writes(s) => writes.extend(s),
                RoutineEffect::Unknown => return RoutineCf::Escaped,
            },
            Stmt::OnBranch { kind, targets, value } => {
                if !expr_is_int_safe(value, &probe) {
                    return RoutineCf::Escaped;
                }
                match kind {
                    OnBranchKind::Goto => {
                        // ON x GOTO falls through when x is out of range,
                        // so keep scanning after pushing the targets.
                        for t in targets {
                            if !push_target(t, stack) {
                                return RoutineCf::Escaped;
                            }
                        }
                    }
                    OnBranchKind::GoSub => {
                        for t in targets {
                            match routine_effect(*t, module, line_index, memo, visiting) {
                                RoutineEffect::Writes(s) => writes.extend(s),
                                RoutineEffect::Unknown => return RoutineCf::Escaped,
                            }
                        }
                    }
                }
            }
            Stmt::If { cond, then } => {
                if !expr_is_int_safe(cond, &probe) {
                    return RoutineCf::Escaped;
                }
                if let RoutineCf::Escaped =
                    scan_routine_then(then, module, line_index, memo, visiting, writes, stack)
                {
                    return RoutineCf::Escaped;
                }
            }
            Stmt::IfElse {
                cond,
                then,
                else_then,
            } => {
                if !expr_is_int_safe(cond, &probe) {
                    return RoutineCf::Escaped;
                }
                if let RoutineCf::Escaped =
                    scan_routine_then(then, module, line_index, memo, visiting, writes, stack)
                {
                    return RoutineCf::Escaped;
                }
                if let RoutineCf::Escaped =
                    scan_routine_then(else_then, module, line_index, memo, visiting, writes, stack)
                {
                    return RoutineCf::Escaped;
                }
            }
            Stmt::Rcomp { then, else_then } => {
                if let RoutineCf::Escaped =
                    scan_routine_then(then, module, line_index, memo, visiting, writes, stack)
                {
                    return RoutineCf::Escaped;
                }
                if let Some(b) = else_then
                    && let RoutineCf::Escaped =
                        scan_routine_then(b, module, line_index, memo, visiting, writes, stack)
                {
                    return RoutineCf::Escaped;
                }
            }
            // Constructs we can't follow or that touch every variable.
            Stmt::ComputedGoto { .. }
            | Stmt::Resume { .. }
            | Stmt::OnError { .. }
            | Stmt::OnKey { .. }
            | Stmt::ErrorRaise { .. }
            | Stmt::Disable
            | Stmt::Clr
            | Stmt::Run(_) => return RoutineCf::Escaped,
            // Any other statement: a leftover statement-level hazard
            // (opaque FnCall, etc.) poisons the routine; otherwise just
            // record the variables it writes.
            other => {
                if !stmt_is_int_safe(other, &probe) {
                    return RoutineCf::Escaped;
                }
                collect_stmt_direct_writes(other, writes);
            }
        }
    }
    RoutineCf::FallThrough
}

fn scan_routine_then(
    then: &ThenIr,
    module: &ir::Module,
    line_index: &HashMap<u16, usize>,
    memo: &mut RoutineWrites,
    visiting: &mut HashSet<u16>,
    writes: &mut HashSet<VarName>,
    stack: &mut Vec<usize>,
) -> RoutineCf {
    match then {
        ThenIr::Goto(t) => match line_index.get(t) {
            Some(&idx) => {
                stack.push(idx);
                RoutineCf::FallThrough
            }
            None => RoutineCf::Escaped,
        },
        ThenIr::Stmts(inner) => {
            scan_routine_stmts(inner, true, module, line_index, memo, visiting, writes, stack)
        }
    }
}

/// Variables a single statement writes directly (not following any
/// control transfer). Used by the routine-write analysis. Must be
/// complete for soundness: a missed write would let a GOSUB look safe
/// for a counter it actually clobbers.
fn collect_stmt_direct_writes(stmt: &Stmt, out: &mut HashSet<VarName>) {
    let note_target = |t: &ir::ReadTarget, out: &mut HashSet<VarName>| match t {
        ir::ReadTarget::Scalar(v) => {
            out.insert(v.clone());
        }
        ir::ReadTarget::Array { name, .. } => {
            out.insert(name.clone());
        }
    };
    match stmt {
        Stmt::Let { var, .. } | Stmt::LetStr { var, .. } => {
            out.insert(var.clone());
        }
        Stmt::ArrayLet { name, .. } | Stmt::ArrayLetStr { name, .. } => {
            out.insert(name.clone());
        }
        Stmt::For { var, .. } => {
            out.insert(var.clone());
        }
        Stmt::Next { vars } => out.extend(vars.iter().flatten().cloned()),
        Stmt::Get { var, .. } | Stmt::KeyGet { var } => {
            out.insert(var.clone());
        }
        Stmt::GetFile { vars, .. } => out.extend(vars.iter().cloned()),
        Stmt::Fetch { target, .. } => {
            out.insert(target.clone());
        }
        Stmt::SwapStr { lhs, rhs } => {
            out.insert(lhs.clone());
            out.insert(rhs.clone());
        }
        Stmt::Read(targets) | Stmt::Input { targets, .. } | Stmt::InputFile { targets, .. } => {
            for t in targets {
                note_target(t, out);
            }
        }
        _ => {}
    }
}

fn stmt_is_int_safe(stmt: &Stmt, loop_var: &VarName) -> bool {
    stmt_is_int_safe_with(stmt, loop_var, None)
}

fn stmt_is_int_safe_with(
    stmt: &Stmt,
    loop_var: &VarName,
    routines: Option<&RoutineWrites>,
) -> bool {
    match stmt {
        // GOSUB / ON ... GOSUB: safe only when the routine's write set
        // is known and excludes the loop counter. Without a routine map
        // (None) we keep the old conservative bail.
        Stmt::GoSub { target } => match routines.and_then(|r| r.get(target)) {
            Some(RoutineEffect::Writes(s)) => !s.contains(loop_var),
            _ => false,
        },
        Stmt::OnBranch {
            kind: OnBranchKind::GoSub,
            targets,
            ..
        } => match routines {
            Some(r) => targets.iter().all(|t| {
                matches!(r.get(t), Some(RoutineEffect::Writes(s)) if !s.contains(loop_var))
            }),
            None => false,
        },
        // Direct writes to the loop variable.
        Stmt::Let { var, value } => {
            var != loop_var && expr_is_int_safe(value, loop_var)
        }
        Stmt::LetStr { value, .. } => str_is_int_safe(value, loop_var),
        Stmt::ArrayLet { indices, value, .. } => {
            indices.iter().all(|e| expr_is_int_safe(e, loop_var))
                && expr_is_int_safe(value, loop_var)
        }
        Stmt::ArrayLetStr { indices, value, .. } => {
            indices.iter().all(|e| expr_is_int_safe(e, loop_var))
                && str_is_int_safe(value, loop_var)
        }
        Stmt::Read(targets) => targets.iter().all(|t| match t {
            ir::ReadTarget::Scalar(v) => v != loop_var,
            // READ A(I) writes an array element — can't directly write
            // the loop var, but indices may read it: that's fine.
            ir::ReadTarget::Array { indices, .. } => {
                indices.iter().all(|e| expr_is_int_safe(e, loop_var))
            }
        }),
        Stmt::Input { targets, .. } => targets.iter().all(|t| match t {
            ir::ReadTarget::Scalar(v) => v != loop_var,
            ir::ReadTarget::Array { indices, .. } => {
                indices.iter().all(|e| expr_is_int_safe(e, loop_var))
            }
        }),
        Stmt::KeyGet { var } => var != loop_var,
        // `GET A(I)` also reads the subscripts, so they have to clear the same int-safety bar
        // the INPUT array form does.
        Stmt::Get { var, indices } => {
            var != loop_var && indices.iter().all(|e| expr_is_int_safe(e, loop_var))
        }
        Stmt::Fetch {
            control,
            max_len,
            target,
            target_indices,
            force,
            position,
        } => {
            target != loop_var
                && str_is_int_safe(control, loop_var)
                && expr_is_int_safe(max_len, loop_var)
                && target_indices.iter().all(|e| expr_is_int_safe(e, loop_var))
                && force
                    .as_ref()
                    .map_or(true, |e| expr_is_int_safe(e, loop_var))
                && position.as_ref().map_or(true, |(r, c)| {
                    expr_is_int_safe(r, loop_var) && expr_is_int_safe(c, loop_var)
                })
        }
        // Nested FOR using the same variable — shadowing breaks our
        // counter bookkeeping.
        Stmt::For { var, start, end, step, .. } if var == loop_var => {
            let _ = (start, end, step);
            false
        }
        // Nested FOR with a different variable: fine, but its body must
        // also be scanned for writes to OUR loop var.
        Stmt::For { start, end, step, .. } => {
            expr_is_int_safe(start, loop_var)
                && expr_is_int_safe(end, loop_var)
                && expr_is_int_safe(step, loop_var)
        }
        // IF with inline body: scan recursively.
        Stmt::If { cond, then } => {
            expr_is_int_safe(cond, loop_var)
                && match then {
                    ThenIr::Goto(_) => true,
                    ThenIr::Stmts(inner) => {
                        inner.iter().all(|s| stmt_is_int_safe_with(s, loop_var, routines))
                    }
                }
        }
        Stmt::IfElse { cond, then, else_then } => {
            expr_is_int_safe(cond, loop_var)
                && match then {
                    ThenIr::Goto(_) => true,
                    ThenIr::Stmts(inner) => {
                        inner.iter().all(|s| stmt_is_int_safe_with(s, loop_var, routines))
                    }
                }
                && match else_then {
                    ThenIr::Goto(_) => true,
                    ThenIr::Stmts(inner) => {
                        inner.iter().all(|s| stmt_is_int_safe_with(s, loop_var, routines))
                    }
                }
        }
        Stmt::DoIf { cond } | Stmt::Until { cond } => expr_is_int_safe(cond, loop_var),
        Stmt::ExitLoop { cond } => cond.as_ref().map_or(true, |c| expr_is_int_safe(c, loop_var)),
        Stmt::ComputedGoto { target } => expr_is_int_safe(target, loop_var),
        Stmt::Reset { target } => expr_is_int_safe(target, loop_var),
        Stmt::Rcomp { then, else_then } => {
            (match then {
                ThenIr::Goto(_) => true,
                ThenIr::Stmts(inner) => inner.iter().all(|s| stmt_is_int_safe_with(s, loop_var, routines)),
            }) && else_then.as_ref().map_or(true, |branch| match branch {
                ThenIr::Goto(_) => true,
                ThenIr::Stmts(inner) => inner.iter().all(|s| stmt_is_int_safe_with(s, loop_var, routines)),
            })
        }
        Stmt::Print { items, .. } => items.iter().all(|p| match p {
            PrintPiece::Expr(e)
            | PrintPiece::CharOut(e)
            | PrintPiece::TabTo(e)
            | PrintPiece::Spc(e) => expr_is_int_safe(e, loop_var),
            PrintPiece::StrExpr(s) => str_is_int_safe(s, loop_var),
            _ => true,
        }),
        Stmt::UseFormat { at, values, .. } => {
            at.iter().all(|(r, c)| {
                expr_is_int_safe(r, loop_var) && expr_is_int_safe(c, loop_var)
            }) && values.iter().all(|e| expr_is_int_safe(e, loop_var))
        }
        Stmt::UseFormatDyn {
            at, ctrl, values, ..
        } => {
            at.iter().all(|(r, c)| {
                expr_is_int_safe(r, loop_var) && expr_is_int_safe(c, loop_var)
            }) && str_is_int_safe(ctrl, loop_var)
                && values.iter().all(|e| expr_is_int_safe(e, loop_var))
        }
        Stmt::Poke { addr, value } => {
            expr_is_int_safe(addr, loop_var) && expr_is_int_safe(value, loop_var)
        }
        Stmt::Dpoke { addr, value } => {
            expr_is_int_safe(addr, loop_var) && expr_is_int_safe(value, loop_var)
        }
        Stmt::PokeFill {
            dst_start,
            dst_end,
            value,
        } => {
            expr_is_int_safe(dst_start, loop_var)
                && expr_is_int_safe(dst_end, loop_var)
                && expr_is_int_safe(value, loop_var)
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
            [row, col, width, height]
                .iter()
                .all(|e| expr_is_int_safe(e, loop_var))
                && ch.as_ref().map_or(true, |e| expr_is_int_safe(e, loop_var))
                && color
                    .as_ref()
                    .map_or(true, |e| expr_is_int_safe(e, loop_var))
        }
        Stmt::ScreenMove {
            row,
            col,
            width,
            height,
            dest_row,
            dest_col,
        } => [row, col, width, height, dest_row, dest_col]
            .iter()
            .all(|e| expr_is_int_safe(e, loop_var)),
        Stmt::ScreenScroll {
            row,
            col,
            width,
            height,
            ..
        } => [row, col, width, height]
            .iter()
            .all(|e| expr_is_int_safe(e, loop_var)),
        Stmt::Color {
            border,
            background,
            pen,
        } => {
            border.as_ref().map_or(true, |e| expr_is_int_safe(e, loop_var))
                && background
                    .as_ref()
                    .map_or(true, |e| expr_is_int_safe(e, loop_var))
                && pen.as_ref().map_or(true, |e| expr_is_int_safe(e, loop_var))
        }
        Stmt::MobEnable { index, .. } => expr_is_int_safe(index, loop_var),
        Stmt::Multi { .. } | Stmt::HiCol | Stmt::Hires { .. } => true,
        Stmt::MultiColors { c1, c2, c3 } => {
            [c1, c2, c3].iter().all(|e| expr_is_int_safe(e, loop_var))
        }
        Stmt::Border { color } => expr_is_int_safe(color, loop_var),
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
            [x1, y1, x2, y2]
                .iter()
                .all(|e| expr_is_int_safe(e, loop_var))
                && mode
                    .as_ref()
                    .is_none_or(|e| expr_is_int_safe(e, loop_var))
        }
        Stmt::Rec {
            x,
            y,
            width,
            height,
            mode,
        } => {
            [x, y, width, height]
                .iter()
                .all(|e| expr_is_int_safe(e, loop_var))
                && mode
                    .as_ref()
                    .is_none_or(|e| expr_is_int_safe(e, loop_var))
        }
        Stmt::Draw { x, y, mode }
        | Stmt::DrawTo { x, y, mode }
        | Stmt::Paint { x, y, mode } => {
            expr_is_int_safe(x, loop_var)
                && expr_is_int_safe(y, loop_var)
                && mode
                    .as_ref()
                    .is_none_or(|e| expr_is_int_safe(e, loop_var))
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
            expr_is_int_safe(cx, loop_var)
                && expr_is_int_safe(cy, loop_var)
                && expr_is_int_safe(radius, loop_var)
                && [ry, start, end, step, mode].iter().all(|opt| {
                    opt.as_ref()
                        .is_none_or(|e| expr_is_int_safe(e, loop_var))
                })
        }
        Stmt::Char {
            x,
            y,
            code,
            mode,
            zoom,
        } => {
            expr_is_int_safe(x, loop_var)
                && expr_is_int_safe(y, loop_var)
                && expr_is_int_safe(code, loop_var)
                && mode
                    .as_ref()
                    .is_none_or(|e| expr_is_int_safe(e, loop_var))
                && zoom
                    .as_ref()
                    .is_none_or(|e| expr_is_int_safe(e, loop_var))
        }
        Stmt::Text {
            x,
            y,
            mode,
            zoom,
            kerning,
            ..
        } => {
            expr_is_int_safe(x, loop_var)
                && expr_is_int_safe(y, loop_var)
                && mode
                    .as_ref()
                    .is_none_or(|e| expr_is_int_safe(e, loop_var))
                && zoom
                    .as_ref()
                    .is_none_or(|e| expr_is_int_safe(e, loop_var))
                && kerning
                    .as_ref()
                    .is_none_or(|e| expr_is_int_safe(e, loop_var))
        }
        Stmt::Rot { direction, length } => {
            expr_is_int_safe(direction, loop_var)
                && length
                    .as_ref()
                    .is_none_or(|l| expr_is_int_safe(l, loop_var))
        }
        Stmt::DrawString { x, y, .. } => {
            expr_is_int_safe(x, loop_var) && expr_is_int_safe(y, loop_var)
        }
        Stmt::Angl {
            cx,
            cy,
            angle,
            rx,
            ry,
            mode,
        } => {
            [cx, cy, angle, rx]
                .iter()
                .all(|e| expr_is_int_safe(e, loop_var))
                && [ry, mode].iter().all(|opt| {
                    opt.as_ref()
                        .is_none_or(|e| expr_is_int_safe(e, loop_var))
                })
        }
        Stmt::Sound { voice, freq } => {
            expr_is_int_safe(voice, loop_var) && expr_is_int_safe(freq, loop_var)
        }
        Stmt::Envelope {
            voice,
            attack,
            decay,
            sustain,
            release,
        } => [voice, attack, decay, sustain, release]
            .iter()
            .all(|e| expr_is_int_safe(e, loop_var)),
        Stmt::Wave {
            voice,
            control,
            pulse,
        } => {
            expr_is_int_safe(voice, loop_var)
                && expr_is_int_safe(control, loop_var)
                && pulse.as_ref().map_or(true, |e| expr_is_int_safe(e, loop_var))
        }
        Stmt::Music { tempo, tune } => {
            expr_is_int_safe(tempo, loop_var) && str_is_int_safe(tune, loop_var)
        }
        Stmt::Play { mode } => expr_is_int_safe(mode, loop_var),
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
        } => [speed, color1, color2].iter().all(|opt| {
            opt.as_ref()
                .is_none_or(|e| expr_is_int_safe(e, loop_var))
        }),
        Stmt::LowCol {
            color1,
            color2,
            color3,
        } => {
            expr_is_int_safe(color1, loop_var)
                && expr_is_int_safe(color2, loop_var)
                && color3
                    .as_ref()
                    .map_or(true, |e| expr_is_int_safe(e, loop_var))
        }
        Stmt::Mod { ink, paper } => {
            expr_is_int_safe(ink, loop_var) && expr_is_int_safe(paper, loop_var)
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
            [src_x, src_y, width, height, dst_x, dst_y]
                .iter()
                .all(|e| expr_is_int_safe(e, loop_var))
                && [mode, zoom].iter().all(|opt| {
                    opt.as_ref()
                        .is_none_or(|e| expr_is_int_safe(e, loop_var))
                })
        }
        Stmt::Copy { src, dst, len } => {
            expr_is_int_safe(src, loop_var)
                && expr_is_int_safe(dst, loop_var)
                && expr_is_int_safe(len, loop_var)
        }
        Stmt::ScrSave { addr, mode } | Stmt::ScrLoad { addr, mode } => {
            addr.as_ref()
                .is_none_or(|e| expr_is_int_safe(e, loop_var))
                && mode
                    .as_ref()
                    .is_none_or(|e| expr_is_int_safe(e, loop_var))
        }
        Stmt::ScrDef { addr, mode, .. } => {
            expr_is_int_safe(addr, loop_var)
                && mode
                    .as_ref()
                    .is_none_or(|e| expr_is_int_safe(e, loop_var))
        }
        Stmt::ScrRestore { .. } => true,
        Stmt::MemClr { addr, len, value } => {
            expr_is_int_safe(addr, loop_var)
                && expr_is_int_safe(len, loop_var)
                && value
                    .as_ref()
                    .is_none_or(|e| expr_is_int_safe(e, loop_var))
        }
        Stmt::MemTransfer { .. } => true,
        Stmt::MemDef {
            len,
            c64_addr,
            reu_addr,
            reu_bank,
            auto_inc,
            fixed,
        } => {
            expr_is_int_safe(len, loop_var)
                && [c64_addr, reu_addr, reu_bank, auto_inc, fixed]
                    .iter()
                    .all(|opt| {
                        opt.as_ref()
                            .is_none_or(|e| expr_is_int_safe(e, loop_var))
                    })
        }
        Stmt::MemLen { len } => expr_is_int_safe(len, loop_var),
        Stmt::MemC64Addr { addr } => expr_is_int_safe(addr, loop_var),
        Stmt::MemReuPos { addr, bank } => {
            expr_is_int_safe(addr, loop_var) && expr_is_int_safe(bank, loop_var)
        }
        Stmt::MemRestore { auto_inc } => expr_is_int_safe(auto_inc, loop_var),
        Stmt::MemCont { mode } => expr_is_int_safe(mode, loop_var),
        Stmt::Design { addr, bytes } => {
            expr_is_int_safe(addr, loop_var)
                && bytes.iter().all(|e| expr_is_int_safe(e, loop_var))
        }
        Stmt::Mmob { index, x, y } => {
            expr_is_int_safe(index, loop_var)
                && expr_is_int_safe(x, loop_var)
                && expr_is_int_safe(y, loop_var)
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
            [index, sx, sy, ex, ey]
                .iter()
                .all(|e| expr_is_int_safe(e, loop_var))
                && size.as_ref().map_or(true, |e| expr_is_int_safe(e, loop_var))
                && speed.as_ref().map_or(true, |e| expr_is_int_safe(e, loop_var))
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
            [index, block, color, priority, multicolor]
                .iter()
                .all(|e| expr_is_int_safe(e, loop_var))
                && size.as_ref().map_or(true, |e| expr_is_int_safe(e, loop_var))
                && speed.as_ref().map_or(true, |e| expr_is_int_safe(e, loop_var))
        }
        Stmt::Rlocmob {
            index,
            dx,
            dy,
            speed,
        } => {
            expr_is_int_safe(index, loop_var)
                && expr_is_int_safe(dx, loop_var)
                && expr_is_int_safe(dy, loop_var)
                && speed.as_ref().map_or(true, |e| expr_is_int_safe(e, loop_var))
        }
        Stmt::Detect { mode } => expr_is_int_safe(mode, loop_var),
        Stmt::Cmob { color1, color2 } => {
            expr_is_int_safe(color1, loop_var) && expr_is_int_safe(color2, loop_var)
        }
        Stmt::Bckgnds {
            color0,
            color1,
            color2,
            color3,
        } => {
            expr_is_int_safe(color0, loop_var)
                && expr_is_int_safe(color1, loop_var)
                && expr_is_int_safe(color2, loop_var)
                && expr_is_int_safe(color3, loop_var)
        }
        Stmt::Cset { mode } => expr_is_int_safe(mode, loop_var),
        Stmt::Nrm | Stmt::HardCopy | Stmt::MemModeOn | Stmt::Cold | Stmt::Retrace => true,
        Stmt::Pause { ticks, .. } => expr_is_int_safe(ticks, loop_var),
        Stmt::Sys {
            addr,
            regs,
            param_vars,
            ..
        } => {
            expr_is_int_safe(addr, loop_var)
                && regs.iter().all(|e| expr_is_int_safe(e, loop_var))

                && param_vars
                    .iter()
                    .all(|p| expr_is_int_safe(&p.value, loop_var))
        }
        Stmt::Wait { addr, mask, eor } => {
            expr_is_int_safe(addr, loop_var)
                && expr_is_int_safe(mask, loop_var)
                && eor.as_ref().map_or(true, |e| expr_is_int_safe(e, loop_var))
        }
        Stmt::Open { file_num, device, secondary, filename } => {
            expr_is_int_safe(file_num, loop_var)
                && device.as_ref().map_or(true, |e| expr_is_int_safe(e, loop_var))
                && secondary.as_ref().map_or(true, |e| expr_is_int_safe(e, loop_var))
                && filename.as_ref().map_or(true, |s| str_is_int_safe(s, loop_var))
        }
        Stmt::Close { file_num } => expr_is_int_safe(file_num, loop_var),
        Stmt::PrintFile { file_num, items, .. } => {
            expr_is_int_safe(file_num, loop_var)
                && items.iter().all(|p| match p {
                    PrintPiece::Expr(e)
                    | PrintPiece::CharOut(e)
                    | PrintPiece::TabTo(e)
                    | PrintPiece::Spc(e) => expr_is_int_safe(e, loop_var),
                    PrintPiece::StrExpr(s) => str_is_int_safe(s, loop_var),
                    _ => true,
                })
        }
        Stmt::GetFile { file_num, vars } => {
            expr_is_int_safe(file_num, loop_var) && vars.iter().all(|v| v != loop_var)
        }
        Stmt::InputFile { file_num, targets } => {
            expr_is_int_safe(file_num, loop_var)
                && targets.iter().all(|t| match t {
                    ir::ReadTarget::Scalar(v) => v != loop_var,
                    ir::ReadTarget::Array { name, indices } => {
                        name != loop_var
                            && indices.iter().all(|e| expr_is_int_safe(e, loop_var))
                    }
                })
        }
        Stmt::Cmd { file_num, items, .. } => {
            expr_is_int_safe(file_num, loop_var)
                && items.iter().all(|p| match p {
                    PrintPiece::Expr(e)
                    | PrintPiece::CharOut(e)
                    | PrintPiece::TabTo(e)
                    | PrintPiece::Spc(e) => expr_is_int_safe(e, loop_var),
                    PrintPiece::StrExpr(s) => str_is_int_safe(s, loop_var),
                    _ => true,
                })
        }
        Stmt::Load {
            filename,
            device,
            secondary,
            load_addr,
        } => {
            str_is_int_safe(filename, loop_var)
                && device.as_ref().map_or(true, |e| expr_is_int_safe(e, loop_var))
                && secondary.as_ref().map_or(true, |e| expr_is_int_safe(e, loop_var))
                && load_addr.as_ref().map_or(true, |e| expr_is_int_safe(e, loop_var))
        }
        Stmt::Verify { filename, device, secondary }
        | Stmt::Save { filename, device, secondary } => {
            str_is_int_safe(filename, loop_var)
                && device.as_ref().map_or(true, |e| expr_is_int_safe(e, loop_var))
                && secondary.as_ref().map_or(true, |e| expr_is_int_safe(e, loop_var))
        }
        Stmt::Disk { command } => str_is_int_safe(command, loop_var),
        Stmt::OnBranch { value, .. } => expr_is_int_safe(value, loop_var),
        Stmt::OnKey { keys, .. } => str_is_int_safe(keys, loop_var),
        Stmt::KeySet { index, text } => {
            expr_is_int_safe(index, loop_var) && str_is_int_safe(text, loop_var)
        }
        Stmt::DisplayKeys => true,
        Stmt::SwapStr { .. } => true,
        Stmt::InsertBox {
            pattern,
            row,
            col,
            width,
            height,
            color,
        } => {
            str_is_int_safe(pattern, loop_var)
                && [row, col, width, height, color]
                    .iter()
                    .all(|e| expr_is_int_safe(e, loop_var))
        }
        // Pure declarations / control statements — no side effects on
        // the loop variable.
        Stmt::Goto { .. }
        | Stmt::Return
        | Stmt::Next { .. }
        | Stmt::Do
        | Stmt::DoNull
        | Stmt::Done
        | Stmt::Else
        | Stmt::Repeat
        | Stmt::Loop
        | Stmt::EndLoop
        | Stmt::Disable
        | Stmt::Rem(_)
        | Stmt::LineMark(_)
        | Stmt::End
        | Stmt::Stop
        | Stmt::RaiseError(_)
        | Stmt::Hang
        // RUN abandons the loop entirely (jumps to L<first>) so it
        // doesn't matter whether the int-counter and float V_var are
        // in sync at the time of the jump.
        | Stmt::Run(_)
        | Stmt::Data(_)
        | Stmt::Restore
        | Stmt::Dim(_)
        | Stmt::DefFn { .. } => true,
        // CLR wipes the loop variable along with everything else, so
        // the int-counter would diverge from V_var. Force the float-
        // FOR path so the loop reads its counter from V_var (which
        // CLR also wipes — matching ROM semantics).
        Stmt::Clr
        | Stmt::Resume { .. }
        | Stmt::OnError { .. }
        | Stmt::ErrorRaise { .. } => false,
    }
}

/// True iff every expression in the body of a FOR loop is safe to
/// run with the integer-counter fast path active. Conservative:
/// FnCall is opaque (could clobber the loop var via its param), so
/// we bail. Loop_var itself shows up in the signature for parity
/// with the previous walker but isn't actually consulted — the
/// "could touch loop_var" question is captured by stmt-level checks
/// in IntForBodyAnalysis::collect_body, not at the leaf level.
fn expr_is_int_safe(e: &Expr, _loop_var: &VarName) -> bool {
    use crate::visit::Visitor;
    let mut det = ExprIntSafetyChecker { unsafe_seen: false };
    det.visit_expr(e);
    !det.unsafe_seen
}

fn str_is_int_safe(s: &StrExpr, _loop_var: &VarName) -> bool {
    use crate::visit::Visitor;
    let mut det = ExprIntSafetyChecker { unsafe_seen: false };
    det.visit_str_expr(s);
    !det.unsafe_seen
}

struct ExprIntSafetyChecker {
    unsafe_seen: bool,
}

impl crate::visit::Visitor for ExprIntSafetyChecker {
    fn visit_expr(&mut self, e: &Expr) {
        if matches!(e, Expr::FnCall(_, _)) {
            self.unsafe_seen = true;
        }
        crate::visit::walk_expr(self, e);
    }
}

// ===== GOTO/GOSUB chain folding =====

/// Folds GOTO/GOSUB targets that point at lines whose only statement
/// is itself another GOTO. `100 GOTO 200`, `200 GOTO 300` →
/// rewrite the original `GOTO 100` (or `GOTO 200`) as `GOTO 300`.
/// Saves the trampoline jump every time the line is reached.
///
/// GOSUB chains fold the same way: the eventual subroutine still
/// RETURNs to the original caller because the intermediate hop is a
/// pure jump (no PHA/PHP). ON GOTO/GOSUB targets and IF-THEN-Goto
/// participate in the fold too.
pub struct GotoChainFold;

impl ir::Pass for GotoChainFold {
    fn name(&self) -> &'static str {
        "goto-chain-fold"
    }

    fn run(&self, module: &mut ir::Module) -> Result<(), ir::PassError> {
        let defined: HashSet<u16> = module.lines.iter().map(|l| l.number).collect();

        let mut redirect: HashMap<u16, u16> = HashMap::new();
        for line in &module.lines {
            if line.stmts.len() == 1 {
                if let Stmt::Goto { target: m } = &line.stmts[0] {
                    if defined.contains(m) {
                        redirect.insert(line.number, *m);
                    }
                }
            }
        }
        if redirect.is_empty() {
            return Ok(());
        }
        // Take transitive closure with cycle detection (a malformed
        // program with `100 GOTO 100` should not loop the compiler).
        let resolve = |start: u16| -> u16 {
            let mut current = start;
            let mut seen: HashSet<u16> = HashSet::new();
            seen.insert(current);
            while let Some(&next) = redirect.get(&current) {
                if !seen.insert(next) {
                    break;
                } // cycle — keep last unique
                current = next;
            }
            current
        };
        // Rewrite every Goto/GoSub target across the module.
        for line in &mut module.lines {
            for stmt in &mut line.stmts {
                fold_goto_targets(stmt, &resolve);
            }
        }
        Ok(())
    }
}

fn fold_goto_targets<F: Fn(u16) -> u16>(stmt: &mut Stmt, resolve: &F) {
    match stmt {
        Stmt::Goto { target } => *target = resolve(*target),
        Stmt::GoSub { target } => *target = resolve(*target),
        Stmt::Run(Some(target)) => *target = resolve(*target),
        Stmt::OnBranch { targets, .. } => {
            for t in targets {
                *t = resolve(*t);
            }
        }
        Stmt::If { then, .. } => fold_then_targets(then, resolve),
        Stmt::IfElse {
            then, else_then, ..
        } => {
            fold_then_targets(then, resolve);
            fold_then_targets(else_then, resolve);
        }
        Stmt::Rcomp { then, else_then } => {
            fold_then_targets(then, resolve);
            if let Some(else_then) = else_then {
                fold_then_targets(else_then, resolve);
            }
        }
        _ => {}
    }
}

fn fold_then_targets<F: Fn(u16) -> u16>(then: &mut ThenIr, resolve: &F) {
    match then {
        ThenIr::Goto(n) => *n = resolve(*n),
        ThenIr::Stmts(inner) => {
            for s in inner {
                fold_goto_targets(s, resolve);
            }
        }
    }
}

// ===== Tail GOSUB rewrite =====

/// Rewrite `GOSUB target: RETURN` to `GOTO target`. The callee's
/// RETURN then pops the caller's original GOSUB frame directly, so
/// the extra JSR/RTS pair disappears.
pub struct TailGosubRewrite;

impl ir::Pass for TailGosubRewrite {
    fn name(&self) -> &'static str {
        "tail-gosub-rewrite"
    }

    fn run(&self, module: &mut ir::Module) -> Result<(), ir::PassError> {
        let inline_bodies = tail_gosub_inline_bodies(module);
        // A `GOSUB` also pushes a marker that FNDFOR stops at.
        let mut direct: HashMap<u16, u32> = HashMap::new();
        let mut on_targets: HashSet<u16> = HashSet::new();
        for line in &module.lines {
            for stmt in &line.stmts {
                collect_gosub_uses(stmt, &mut direct, &mut on_targets);
            }
        }
        let keep_marker = crate::for_next_runtime::gosub_targets_needing_marker(
            module,
            direct.keys().chain(on_targets.iter()).copied(),
        );

        let mut sites: Vec<(u16, u16)> = Vec::new();
        for line in &module.lines {
            collect_gosub_call_sites(line.number, &line.stmts, &mut sites);
        }
        let recursive = crate::for_next_runtime::recursive_gosub_call_lines(module, sites);
        for line in &mut module.lines {
            let no = line.number;
            rewrite_tail_gosubs_in_stmts(
                &mut line.stmts,
                &inline_bodies,
                false,
                &keep_marker,
                no,
                &recursive,
            );
        }
        Ok(())
    }
}

/// Every `(line, target)` a direct `GOSUB` in `stmts` names, including under a `THEN`.
fn collect_gosub_call_sites(line: u16, stmts: &[Stmt], out: &mut Vec<(u16, u16)>) {
    fn from_then(line: u16, then: &ThenIr, out: &mut Vec<(u16, u16)>) {
        if let ThenIr::Stmts(inner) = then {
            collect_gosub_call_sites(line, inner, out);
        }
    }
    for stmt in stmts {
        match stmt {
            Stmt::GoSub { target } => out.push((line, *target)),
            Stmt::If { then, .. } => from_then(line, then, out),
            Stmt::IfElse {
                then, else_then, ..
            } => {
                from_then(line, then, out);
                from_then(line, else_then, out);
            }
            Stmt::Rcomp { then, else_then } => {
                from_then(line, then, out);
                if let Some(else_then) = else_then {
                    from_then(line, else_then, out);
                }
            }
            _ => {}
        }
    }
}

fn tail_gosub_inline_bodies(module: &ir::Module) -> HashMap<u16, Vec<Stmt>> {
    let mut direct_gosubs: HashMap<u16, u32> = HashMap::new();
    let mut on_gosub_targets = HashSet::new();
    let mut entry_targets = ordinary_entry_targets(module);
    for line in &module.lines {
        for stmt in &line.stmts {
            collect_gosub_uses(stmt, &mut direct_gosubs, &mut on_gosub_targets);
            collect_goto_targets(stmt, &mut entry_targets);
        }
    }
    let mut out = HashMap::new();
    for line in &module.lines {
        if direct_gosubs.get(&line.number) != Some(&1)
            || on_gosub_targets.contains(&line.number)
            || entry_targets.contains(&line.number)
        {
            continue;
        }
        if let Some(body) = gosub_inline_body(&line.stmts) {
            out.insert(line.number, body);
        }
    }
    out
}

fn rewrite_tail_gosubs_in_stmts(
    stmts: &mut Vec<Stmt>,
    inline_bodies: &HashMap<u16, Vec<Stmt>>,
    allow_inline: bool,
    keep_marker: &HashSet<u16>,
    line: u16,
    recursive: &HashSet<(u16, u16)>,
) {
    for stmt in stmts.iter_mut() {
        match stmt {
            Stmt::If {
                then: ThenIr::Stmts(inner),
                ..
            } => {
                rewrite_tail_gosubs_in_stmts(
                    inner,
                    inline_bodies,
                    true,
                    keep_marker,
                    line,
                    recursive,
                );
            }
            Stmt::IfElse {
                then, else_then, ..
            } => {
                rewrite_tail_gosubs_in_then(then, inline_bodies, keep_marker, line, recursive);
                rewrite_tail_gosubs_in_then(else_then, inline_bodies, keep_marker, line, recursive);
            }
            Stmt::Rcomp { then, else_then } => {
                rewrite_tail_gosubs_in_then(then, inline_bodies, keep_marker, line, recursive);
                if let Some(else_then) = else_then {
                    rewrite_tail_gosubs_in_then(
                        else_then,
                        inline_bodies,
                        keep_marker,
                        line,
                        recursive,
                    );
                }
            }
            _ => {}
        }
    }
    let mut i = 0;
    while i + 1 < stmts.len() {
        let target = match (&stmts[i], &stmts[i + 1]) {
            (Stmt::GoSub { target }, Stmt::Return)
                if !keep_marker.contains(target) && !recursive.contains(&(line, *target)) =>
            {
                Some(*target)
            }
            _ => None,
        };
        if let Some(target) = target {
            if allow_inline && let Some(body) = inline_bodies.get(&target) {
                let len = body.len();
                stmts.splice(i..=i, body.clone());
                i += len + 1;
            } else {
                stmts[i] = Stmt::Goto { target };
                stmts.remove(i + 1);
            }
        } else {
            i += 1;
        }
    }
}

fn rewrite_tail_gosubs_in_then(
    then: &mut ThenIr,
    inline_bodies: &HashMap<u16, Vec<Stmt>>,
    keep_marker: &HashSet<u16>,
    line: u16,
    recursive: &HashSet<(u16, u16)>,
) {
    if let ThenIr::Stmts(inner) = then {
        rewrite_tail_gosubs_in_stmts(inner, inline_bodies, true, keep_marker, line, recursive);
    }
}

// ===== Single-use GOSUB inlining =====

/// Inline direct GOSUB calls whose target has exactly one direct use
/// and is not reachable as a normal line entry. This removes one
/// JSR/RTS pair and exposes the subroutine body to local passes that
/// run after it.
pub struct GosubSingleUseInline;

impl ir::Pass for GosubSingleUseInline {
    fn name(&self) -> &'static str {
        "gosub-single-use-inline"
    }

    fn run(&self, module: &mut ir::Module) -> Result<(), ir::PassError> {
        let mut direct_gosubs: HashMap<u16, u32> = HashMap::new();
        let mut on_gosub_targets: HashSet<u16> = HashSet::new();
        let mut entry_targets = ordinary_entry_targets(module);
        for line in &module.lines {
            for stmt in &line.stmts {
                collect_gosub_uses(stmt, &mut direct_gosubs, &mut on_gosub_targets);
                collect_goto_targets(stmt, &mut entry_targets);
            }
        }

        let mut inline_bodies: HashMap<u16, Vec<Stmt>> = HashMap::new();
        for line in &module.lines {
            if direct_gosubs.get(&line.number) != Some(&1)
                || on_gosub_targets.contains(&line.number)
                || entry_targets.contains(&line.number)
            {
                continue;
            }
            if let Some(body) = gosub_inline_body(&line.stmts) {
                inline_bodies.insert(line.number, body);
            }
        }
        if inline_bodies.is_empty() {
            return Ok(());
        }
        for line in &mut module.lines {
            inline_gosubs_in_stmts(&mut line.stmts, &inline_bodies, line.number);
        }
        Ok(())
    }
}

/// Inline short GOSUB targets at *every* direct call site, even when
/// the target has multiple callers. The single-call variant
/// ([`GosubSingleUseInline`]) only fires when there's exactly one
/// caller; this pass extends inlining to N-caller subroutines on the
/// theory that, for very small bodies, the per-call JSR/RTS overhead
/// (12 cycles + 3 bytes call site) costs more than the code-growth
/// from copying a 1-2 stmt body to every call site.
///
/// Cost-benefit per inlined call: save 12 cycles/3 bytes (JSR+RTS),
/// pay max(0, body_size - 3) bytes per extra call site. For bodies
/// of 1-2 statements this is a few bytes net growth for measurable
/// speed on hot paths.
///
/// Gates (same as the single-call variant, plus a length cap):
///   * Target line is not reachable as a normal line entry (would
///     change semantics on fall-through).
///   * Target line is not the target of an `ON GOSUB` (those rely on
///     the JSR/RTS semantics for selecting a caller).
///   * Body matches `gosub_inline_body` (no GOTO/RUN/FOR/NEXT/etc.,
///     ends with RETURN).
///   * Body has at most `SHORT_BODY_MAX_STMTS` statements after
///     stripping the trailing RETURN.
pub struct GosubShortBodyInline;

const SHORT_BODY_MAX_STMTS: usize = 2;

impl ir::Pass for GosubShortBodyInline {
    fn name(&self) -> &'static str {
        "gosub-short-body-inline"
    }

    fn run(&self, module: &mut ir::Module) -> Result<(), ir::PassError> {
        let mut direct_gosubs: HashMap<u16, u32> = HashMap::new();
        let mut on_gosub_targets: HashSet<u16> = HashSet::new();
        let mut entry_targets = ordinary_entry_targets(module);
        for line in &module.lines {
            for stmt in &line.stmts {
                collect_gosub_uses(stmt, &mut direct_gosubs, &mut on_gosub_targets);
                collect_goto_targets(stmt, &mut entry_targets);
            }
        }

        let mut inline_bodies: HashMap<u16, Vec<Stmt>> = HashMap::new();
        for line in &module.lines {
            let calls = direct_gosubs.get(&line.number).copied().unwrap_or(0);
            // ≥2 callers: this pass's reason to exist. (Single-caller
            // already handled by GosubSingleUseInline; let it own
            // those.)
            if calls < 2 {
                continue;
            }
            if on_gosub_targets.contains(&line.number) || entry_targets.contains(&line.number) {
                continue;
            }
            if let Some(body) = gosub_inline_body(&line.stmts)
                && body.len() <= SHORT_BODY_MAX_STMTS
            {
                inline_bodies.insert(line.number, body);
            }
        }
        if inline_bodies.is_empty() {
            return Ok(());
        }
        for line in &mut module.lines {
            inline_gosubs_in_stmts(&mut line.stmts, &inline_bodies, line.number);
        }
        Ok(())
    }
}

fn ordinary_entry_targets(module: &ir::Module) -> HashSet<u16> {
    let mut targets = HashSet::new();
    if module_has_computed_goto(module) {
        targets.extend(module.lines.iter().map(|line| line.number));
        return targets;
    }
    if let Some(first) = module.lines.first() {
        targets.insert(first.number);
    }
    for pair in module.lines.windows(2) {
        let prev = &pair[0];
        let next = &pair[1];
        if prev
            .stmts
            .last()
            .map_or(true, |stmt| !stmt_is_unconditional_transfer(stmt))
        {
            targets.insert(next.number);
        }
    }
    targets
}

fn collect_gosub_uses(stmt: &Stmt, direct: &mut HashMap<u16, u32>, on_targets: &mut HashSet<u16>) {
    match stmt {
        Stmt::GoSub { target } => {
            *direct.entry(*target).or_insert(0) += 1;
        }
        Stmt::OnBranch {
            kind: OnBranchKind::GoSub,
            targets,
            ..
        } => {
            on_targets.extend(targets.iter().copied());
        }
        Stmt::If {
            then: ThenIr::Stmts(inner),
            ..
        } => {
            for s in inner {
                collect_gosub_uses(s, direct, on_targets);
            }
        }
        Stmt::IfElse {
            then, else_then, ..
        } => {
            collect_gosub_uses_in_then(then, direct, on_targets);
            collect_gosub_uses_in_then(else_then, direct, on_targets);
        }
        Stmt::Rcomp { then, else_then } => {
            collect_gosub_uses_in_then(then, direct, on_targets);
            if let Some(else_then) = else_then {
                collect_gosub_uses_in_then(else_then, direct, on_targets);
            }
        }
        _ => {}
    }
}

fn collect_gosub_uses_in_then(
    then: &ThenIr,
    direct: &mut HashMap<u16, u32>,
    on_targets: &mut HashSet<u16>,
) {
    if let ThenIr::Stmts(inner) = then {
        for s in inner {
            collect_gosub_uses(s, direct, on_targets);
        }
    }
}

fn collect_goto_targets(stmt: &Stmt, targets: &mut HashSet<u16>) {
    match stmt {
        Stmt::Goto { target } => {
            targets.insert(*target);
        }
        // `RUN <line>` enters the target as a fresh program start —
        // not a GOSUB, so the GosubSingleUseInline gate counts it
        // as another non-GOSUB entry that disqualifies inlining.
        Stmt::Run(Some(target)) => {
            targets.insert(*target);
        }
        Stmt::OnBranch {
            kind: OnBranchKind::Goto,
            targets: branch_targets,
            ..
        } => {
            targets.extend(branch_targets.iter().copied());
        }
        Stmt::If {
            then: ThenIr::Goto(target),
            ..
        } => {
            targets.insert(*target);
        }
        Stmt::If {
            then: ThenIr::Stmts(inner),
            ..
        } => {
            for s in inner {
                collect_goto_targets(s, targets);
            }
        }
        Stmt::IfElse {
            then, else_then, ..
        } => {
            collect_goto_targets_in_then(then, targets);
            collect_goto_targets_in_then(else_then, targets);
        }
        Stmt::Rcomp { then, else_then } => {
            collect_goto_targets_in_then(then, targets);
            if let Some(else_then) = else_then {
                collect_goto_targets_in_then(else_then, targets);
            }
        }
        _ => {}
    }
}

fn collect_goto_targets_in_then(then: &ThenIr, targets: &mut HashSet<u16>) {
    match then {
        ThenIr::Goto(target) => {
            targets.insert(*target);
        }
        ThenIr::Stmts(inner) => {
            for s in inner {
                collect_goto_targets(s, targets);
            }
        }
    }
}

/// Why a line cannot serve as an inlinable `GOSUB` body.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum GosubRefusal {
    /// The line has no statements at all, so there is no trailing `RETURN`.
    EmptyLine,
    /// The line does not end in `RETURN`: the subroutine body runs on into the following line.
    NoTrailingReturn,

    Branch,
    /// A nested `GOSUB`.
    NestedGosub,
    /// A second `RETURN` before the final one ; an early exit.
    EarlyReturn,
    /// `FOR` or `NEXT` in the body.
    ForFrame,
    /// `END`, `STOP`, or `RUN` ; the body can terminate or restart, so the `RETURN` is not the only exit.
    Terminator,
    /// `CLR`, which resets BASIC's stack pointer and turns the body's own `RETURN` into `?RETURN WITHOUT GOSUB` on the subroutine's line.
    Clr,
    /// `ON KEY`, `RESUME`, `ON ERROR`, or an explicit error raise: these read or write interpreter error state that is keyed to position.
    ErrorState,
    /// `DIM`, `DATA`, or `DEF FN` ; declarations whose meaning depends on where they sit, not on when they run.
    Declaration,
}

impl GosubRefusal {

    pub fn slug(self) -> &'static str {
        match self {
            GosubRefusal::EmptyLine => "empty-line",
            GosubRefusal::NoTrailingReturn => "no-trailing-return",
            GosubRefusal::Branch => "branch",
            GosubRefusal::NestedGosub => "nested-gosub",
            GosubRefusal::EarlyReturn => "early-return",
            GosubRefusal::ForFrame => "for-frame",
            GosubRefusal::Terminator => "terminator",
            GosubRefusal::Clr => "clr",
            GosubRefusal::ErrorState => "error-state",
            GosubRefusal::Declaration => "declaration",
        }
    }
}

/// The first reason `stmts` cannot be inlined as a `GOSUB` body, or `None` when it can.
pub fn gosub_inline_refusal(stmts: &[Stmt]) -> Option<GosubRefusal> {
    let Some((last, body)) = stmts.split_last() else {
        return Some(GosubRefusal::EmptyLine);
    };
    if !matches!(last, Stmt::Return) {
        return Some(GosubRefusal::NoTrailingReturn);
    }
    body.iter().find_map(stmt_gosub_inline_refusal)
}

fn gosub_inline_body(stmts: &[Stmt]) -> Option<Vec<Stmt>> {
    if gosub_inline_refusal(stmts).is_some() {
        return None;
    }
    // `gosub_inline_refusal` returned `None`, so `split_last` succeeded there and the last statement is the `RETURN` this strips.
    Some(stmts[..stmts.len() - 1].to_vec())
}

fn stmt_gosub_inline_refusal(stmt: &Stmt) -> Option<GosubRefusal> {
    match stmt {
        Stmt::Goto { .. } | Stmt::ComputedGoto { .. } | Stmt::OnBranch { .. } => {
            Some(GosubRefusal::Branch)
        }
        Stmt::GoSub { .. } => Some(GosubRefusal::NestedGosub),
        Stmt::Return => Some(GosubRefusal::EarlyReturn),
        Stmt::For { .. } | Stmt::Next { .. } => Some(GosubRefusal::ForFrame),
        Stmt::End | Stmt::Stop | Stmt::Run(_) => Some(GosubRefusal::Terminator),
        // `CLR` resets BASIC's interpreter stack pointer, which throws away the caller's GOSUB
        // frame along with every FOR frame.
        Stmt::Clr => Some(GosubRefusal::Clr),
        Stmt::OnKey { .. }
        | Stmt::Resume { .. }
        | Stmt::OnError { .. }
        | Stmt::ErrorRaise { .. } => Some(GosubRefusal::ErrorState),
        Stmt::Dim(_) | Stmt::Data(_) | Stmt::DefFn { .. } => Some(GosubRefusal::Declaration),
        Stmt::If {
            then: ThenIr::Goto(_),
            ..
        } => Some(GosubRefusal::Branch),
        Stmt::If {
            then: ThenIr::Stmts(inner),
            ..
        } => inner.iter().find_map(stmt_gosub_inline_refusal),
        Stmt::IfElse {
            then, else_then, ..
        } => then_ir_gosub_inline_refusal(then).or_else(|| then_ir_gosub_inline_refusal(else_then)),
        Stmt::Rcomp { then, else_then } => then_ir_gosub_inline_refusal(then)
            .or_else(|| else_then.as_ref().and_then(then_ir_gosub_inline_refusal)),
        _ => None,
    }
}

fn then_ir_gosub_inline_refusal(then: &ThenIr) -> Option<GosubRefusal> {
    match then {
        ThenIr::Goto(_) => Some(GosubRefusal::Branch),
        ThenIr::Stmts(inner) => inner.iter().find_map(stmt_gosub_inline_refusal),
    }
}

fn stmt_is_unconditional_transfer(stmt: &Stmt) -> bool {
    matches!(
        stmt,
        Stmt::Goto { .. }
            | Stmt::ComputedGoto { .. }
            | Stmt::Return
            | Stmt::End
            | Stmt::Stop
            | Stmt::Run(_)
    )
}

/// Splice inlinable GOSUB bodies into their call sites.
fn inline_gosubs_in_stmts(stmts: &mut Vec<Stmt>, bodies: &HashMap<u16, Vec<Stmt>>, here: u16) {
    let mut i = 0;
    while i < stmts.len() {
        match &mut stmts[i] {
            Stmt::GoSub { target } => {
                if let Some(body) = bodies.get(target) {
                    let mut spliced = Vec::with_capacity(body.len() + 2);
                    spliced.push(Stmt::LineMark(*target));
                    spliced.extend(body.iter().cloned());
                    spliced.push(Stmt::LineMark(here));
                    let len = spliced.len();
                    stmts.splice(i..=i, spliced);
                    i += len;
                } else {
                    i += 1;
                }
            }
            Stmt::If {
                then: ThenIr::Stmts(inner),
                ..
            } => {
                inline_gosubs_in_stmts(inner, bodies, here);
                i += 1;
            }
            Stmt::IfElse {
                then, else_then, ..
            } => {
                inline_gosubs_in_then(then, bodies, here);
                inline_gosubs_in_then(else_then, bodies, here);
                i += 1;
            }
            Stmt::Rcomp { then, else_then } => {
                inline_gosubs_in_then(then, bodies, here);
                if let Some(else_then) = else_then {
                    inline_gosubs_in_then(else_then, bodies, here);
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
}

fn inline_gosubs_in_then(then: &mut ThenIr, bodies: &HashMap<u16, Vec<Stmt>>, here: u16) {
    if let ThenIr::Stmts(inner) = then {
        inline_gosubs_in_stmts(inner, bodies, here);
    }
}

// ===== Dead code after transfer-of-control =====

/// Trim every statement that follows a GOTO/RETURN/END/STOP/RUN
/// within a stmt list — both at line top-level and inside IF THEN
/// bodies. Common pattern: `100 GOTO 200: REM dead` or
/// `IF X THEN GOTO L1: A=5` where the trailing assignment can never
/// run because the THEN body either jumps away (GOTO fired) or never
/// entered the body at all (cond was false).
pub struct DeadCodeAfterTransfer;

impl ir::Pass for DeadCodeAfterTransfer {
    fn name(&self) -> &'static str {
        "dead-code-after-transfer"
    }

    fn run(&self, module: &mut ir::Module) -> Result<(), ir::PassError> {
        for line in &mut module.lines {
            trim_after_transfer(&mut line.stmts);
        }
        Ok(())
    }
}

fn trim_after_transfer(stmts: &mut Vec<Stmt>) {
    // Recurse into IF then-bodies first so we trim from the inside out.
    for stmt in stmts.iter_mut() {
        match stmt {
            Stmt::If {
                then: ThenIr::Stmts(inner),
                ..
            } => trim_after_transfer(inner),
            Stmt::IfElse {
                then, else_then, ..
            } => {
                trim_transfer_in_then(then);
                trim_transfer_in_then(else_then);
            }
            Stmt::Rcomp { then, else_then } => {
                trim_transfer_in_then(then);
                if let Some(else_then) = else_then {
                    trim_transfer_in_then(else_then);
                }
            }
            _ => {}
        }
    }
    // Stop at the first transfer-of-control. GOSUB returns, so it
    // does NOT terminate the flow. ON-GOTO/GOSUB selects at runtime
    // and may fall through (out-of-range selector), so it doesn't
    // either. CLR clears state but continues. RUN restarts the
    // program — anything after it is dead.
    if let Some(idx) = stmts.iter().position(is_transfer) {
        // `DATA` is a declaration, not a statement.
        let kept: Vec<Stmt> = stmts
            .drain(idx + 1..)
            .filter(|s| matches!(s, Stmt::Data(_)))
            .collect();
        stmts.extend(kept);
    }
}

fn is_transfer(s: &Stmt) -> bool {
    matches!(
        s,
        Stmt::Goto { .. }
            | Stmt::ComputedGoto { .. }
            | Stmt::Return
            | Stmt::End
            | Stmt::Stop
            | Stmt::Run(_)
    )
}

fn trim_transfer_in_then(then: &mut ThenIr) {
    if let ThenIr::Stmts(inner) = then {
        trim_after_transfer(inner);
    }
}

// ===== Constant variable propagation =====

/// Single-iteration constant-variable propagation. Returns `true`
/// when at least one substitution was made.
///
/// A variable is treated as constant when:
///   * Exactly one `LET v = literal` in the entire module (counted at
///     top-level only — assignments inside IF/FOR/DEF FN bodies don't
///     qualify, since those branches may be skipped at runtime).
///   * The RHS is a numeric literal (after constant folding).
///   * The variable is never written by INPUT, READ, GET, FOR, DIM, or
///     ArrayLet — those are assumed to be runtime-driven.
///   * The line containing the assignment has a number ≤ every line
///     that reads the variable. Catches the very common 80s pattern of
///     setting "constants" at the top of the program (`V=53248`,
///     `Q=1024`, `S=54272`, etc.) without risking pathological
///     backward-GOTO programs.
///
/// Substitution leaves the original LET in place — DeadLineElim can't
/// safely strip it (mid-line statements aren't independently
/// addressable), but the wasted slot store is tiny compared to the
/// savings at every use site (each PEEK/POKE/array-index expression
/// constant-folds further once its `Var` becomes `Number`).
///
/// The caller drives a fixpoint loop because chained constant chains
/// (`a=5: b=a*2: c=b+1: dim x(c)`) only fully fold once each layer in
/// the chain has been promoted. Alternate this with a constant-fold
/// pass and stop when nothing changes.
/// String version of `run_const_var_prop`. Finds `LET v$ = "..."`
/// (or any string expression that folds to a literal via
/// `try_fold_str`) where `v$` has exactly one top-level write and
/// every read happens at or after the LET line, then substitutes
/// the literal at every read site. Drops the now-dead LET so the
/// runtime heap allocation goes away too.
///
/// Motivation: programs build INSERT box patterns by
/// concatenating constant chars onto literals (`"..." + RL$ + "..."`)
/// where `RL$` was set once with `CHR$(ASC("...") OR $40)`. The
/// INSERT codegen needs the pattern as a 9-char literal at compile
/// time; without string propagation, the `Var(RL$)` in the concat
/// blocks the fold and INSERT errors out.
pub fn run_str_const_var_prop(module: &mut ir::Module) -> bool {
    let fre_observable = module_uses_fre(module);
    let mut scanner = StrConstVarScanner {
        fre_observable,
        ..StrConstVarScanner::default()
    };
    crate::visit::walk_module(&mut scanner, module);
    if scanner.saw_var_reset {
        return false;
    }
    let in_sub = compute_in_sub_lines(module);
    let mut consts: HashMap<VarName, Vec<u8>> = HashMap::new();
    for (var, vi) in &scanner.info {
        if var.kind != VarKind::String {
            continue;
        }
        if var.base == "TI" || var.base == "ST" {
            // Reserved system vars: `TI$ = "..."` resets the jiffy
            // clock, reads return live values. Never fold.
            continue;
        }
        if vi.let_count != 1 || vi.has_nonlet_write {
            continue;
        }
        let Some(value) = &vi.let_value else { continue };
        if vi.let_line > vi.first_use_line {
            continue;
        }
        if in_sub.contains(&vi.let_line) {
            continue;
        }
        consts.insert(var.clone(), value.clone());
    }
    if consts.is_empty() {
        return false;
    }
    // Text order does not establish execution order: an earlier GOSUB can read a later subroutine's variable before its sole LET runs.
    retain_initialized_constants(module, &mut consts);
    if consts.is_empty() {
        return false;
    }
    let mut substituter = StrConstVarSubstituter {
        consts: &consts,
        fre_observable,
    };
    crate::visit::walk_module_mut(&mut substituter, module);
    retain_top_level_except_floor_traps(module, |s| is_promoted_str_const_let(s, &consts));
    true
}

#[derive(Default)]
struct StrConstVarScanner {
    info: HashMap<VarName, StrVarInfo>,
    nested: bool,
    current_line: u16,
    /// See `ConstVarScanner::saw_var_reset`.
    saw_var_reset: bool,
    /// See `str_binding_fold_is_fre_visible`.
    fre_observable: bool,
}

#[derive(Default)]
struct StrVarInfo {
    let_count: u32,
    let_value: Option<Vec<u8>>,
    let_line: u16,
    has_nonlet_write: bool,
    first_use_line: u16,
}

impl StrVarInfo {
    fn new() -> Self {
        Self {
            first_use_line: u16::MAX,
            ..Default::default()
        }
    }
}

impl StrConstVarScanner {
    fn note_use(&mut self, v: &VarName) {
        let entry = self.info.entry(v.clone()).or_insert_with(StrVarInfo::new);
        if self.current_line < entry.first_use_line {
            entry.first_use_line = self.current_line;
        }
    }
    fn mark_nonlet_write(&mut self, v: &VarName) {
        self.info
            .entry(v.clone())
            .or_insert_with(StrVarInfo::new)
            .has_nonlet_write = true;
    }
}

impl crate::visit::Visitor for StrConstVarScanner {
    fn visit_str_expr(&mut self, s: &StrExpr) {
        if let StrExpr::Var(v) = s {
            self.note_use(v);
        }
        crate::visit::walk_str_expr(self, s);
    }

    fn visit_stmt(&mut self, line_no: u16, stmt: &Stmt) {
        let saved_line = self.current_line;
        self.current_line = line_no;
        match stmt {
            Stmt::LetStr { var, value } => {
                // RHS use-collection first (in case it reads `var`
                // itself in a self-concat).
                self.visit_str_expr(value);
                if self.nested {
                    self.mark_nonlet_write(var);
                } else {
                    let entry = self.info.entry(var.clone()).or_insert_with(StrVarInfo::new);
                    entry.let_count = entry.let_count.saturating_add(1);
                    entry.let_line = line_no;
                    entry.let_value = if str_binding_fold_is_fre_visible(value, self.fre_observable)
                    {
                        None
                    } else {
                        try_fold_str(value)
                    };
                }
            }
            Stmt::Let { var, .. } => {
                // Numeric LET — counts as a non-LET write for any
                // identically-named string var (shouldn't happen
                // because the parser distinguishes them, but the
                // VarName is the carrier; safer to mark).
                if var.kind == VarKind::String {
                    self.mark_nonlet_write(var);
                }
                crate::visit::walk_stmt(self, line_no, stmt);
            }
            Stmt::ArrayLetStr { name, .. } | Stmt::ArrayLet { name, .. } => {
                self.mark_nonlet_write(name);
                crate::visit::walk_stmt(self, line_no, stmt);
            }
            Stmt::If { cond, then } => {
                self.visit_expr(cond);
                if let crate::ir::ThenIr::Stmts(inner) = then {
                    let saved = self.nested;
                    self.nested = true;
                    for s in inner {
                        self.visit_stmt(line_no, s);
                    }
                    self.nested = saved;
                }
            }
            Stmt::Read(targets) | Stmt::Input { targets, .. } => {
                for t in targets {
                    if let crate::ir::ReadTarget::Scalar(v) = t {
                        self.mark_nonlet_write(v);
                    }
                }
            }
            Stmt::InputFile { file_num, targets } => {
                self.visit_expr(file_num);
                for t in targets {
                    if let crate::ir::ReadTarget::Scalar(v) = t {
                        self.mark_nonlet_write(v);
                    }
                }
            }
            Stmt::Get { var, .. } | Stmt::KeyGet { var } => self.mark_nonlet_write(var),
            Stmt::GetFile { file_num, vars } => {
                self.visit_expr(file_num);
                for v in vars {
                    self.mark_nonlet_write(v);
                }
            }

            Stmt::Clr | Stmt::Run(_) => {
                self.saw_var_reset = true;
            }
            _ => crate::visit::walk_stmt(self, line_no, stmt),
        }
        self.current_line = saved_line;
    }
}

struct StrConstVarSubstituter<'a> {
    consts: &'a HashMap<VarName, Vec<u8>>,
    fre_observable: bool,
}

impl<'a> crate::visit::MutVisitor for StrConstVarSubstituter<'a> {
    fn visit_expr_mut(&mut self, e: &mut Expr) {

        if let Expr::Var(v) = e {
            if let Some(bytes) = self.consts.get(v) {
                *e = Expr::String(bytes.clone());
                return;
            }
        }
        crate::visit::walk_expr_mut(self, e);
    }

    fn visit_stmt_mut(&mut self, line_no: u16, stmt: &mut Stmt) {
        match stmt {
            Stmt::LetStr { value, .. }
                if str_binding_fold_is_fre_visible(value, self.fre_observable) =>
            {
                crate::visit::walk_str_expr_mut(self, value);
            }
            Stmt::ArrayLetStr { indices, value, .. }
                if str_binding_fold_is_fre_visible(value, self.fre_observable) =>
            {
                for e in indices.iter_mut() {
                    self.visit_expr_mut(e);
                }
                crate::visit::walk_str_expr_mut(self, value);
            }
            _ => crate::visit::walk_stmt_mut(self, line_no, stmt),
        }
    }

    fn visit_str_expr_mut(&mut self, s: &mut StrExpr) {
        if let StrExpr::Var(v) = s {
            if let Some(bytes) = self.consts.get(v) {
                *s = StrExpr::Literal(bytes.clone());
                return;
            }
        }
        crate::visit::walk_str_expr_mut(self, s);
        if let Some(folded) = try_fold_str(s) {
            *s = StrExpr::Literal(folded);
        }
    }
}

fn is_promoted_str_const_let(stmt: &Stmt, consts: &HashMap<VarName, Vec<u8>>) -> bool {
    matches!(
        stmt,
        Stmt::LetStr { var, value } if consts.contains_key(var)
            && matches!(value, StrExpr::Literal(_))
    )
}

pub fn run_const_var_prop(module: &mut ir::Module) -> bool {
    // Step 1: scan via Visitor — gather per-var LET counts, the
    // literal value if the single LET folded to one, whether any
    // "non-LET write" mechanism touches it, and use-line range.
    let mut scanner = ConstVarScanner::default();
    crate::visit::walk_module(&mut scanner, module);
    if scanner.saw_var_reset {
        return false;
    }
    let info = scanner.info;
    // Pre-compute the set of line numbers that may be reached only
    // while inside a GOSUB body — those subs aren't guaranteed to
    // run before some other call site reads the var, so a literal
    // assignment inside one isn't a sound program-wide constant.
    // A low-numbered subroutine can assign the only literal value,
    // while the first read happens before that subroutine runs. In
    // that shape, propagating the literal would turn the later branch
    // into an unconditional jump.
    let in_sub = compute_in_sub_lines(module);
    // Step 2: build the substitution map.
    let mut consts: HashMap<VarName, f64> = HashMap::new();
    for (var, vi) in &info {
        if var.kind == VarKind::String {
            continue;
        }
        if vi.let_count != 1 {
            continue;
        }
        if vi.has_nonlet_write {
            continue;
        }
        let Some(value) = vi.let_value else {
            continue;
        };
        // Assignment must precede every read in line-number order.
        // first_use_line = u16::MAX when no reads were seen — that's
        // dead code, doesn't matter; substitute anyway (no-op).
        if vi.let_line > vi.first_use_line {
            continue;
        }
        // The line-order check is only sound when the LET runs on
        // every path that reaches a read. A LET inside a subroutine
        // body can be skipped at runtime if some caller reads the var
        // before that sub is ever entered. Disqualify those.
        if in_sub.contains(&vi.let_line) {
            continue;
        }
        // Skip vars whose only reads sit inside DIM size exprs.
        // Promoting them forces static array allocation (the
        // array bytes get baked into the PRG), which only pays
        // off for small arrays — large ones would bloat the
        // image past what the runtime-allocated path costs.
        //
        // The cap of 32 caps a worst-case `DIM A(32)` float
        // array at 33 * 5 = 165 bytes vs ~50 bytes of
        // allocator code, so even the worst float case
        // costs <120 bytes; small int/string arrays are
        // strictly cheaper. Above the cap we leave it as a
        // runtime DIM.
        if vi.non_dim_use_count == 0 && (!value.is_finite() || value < 0.0 || value > 32.0) {
            continue;
        }
        consts.insert(var.clone(), value);
    }
    // Step 3: substitute and re-fold via MutVisitor.
    if consts.is_empty() {
        return false;
    }
    // BASIC variables are initially zero.
    retain_initialized_constants(module, &mut consts);
    if consts.is_empty() {
        return false;
    }
    let mut substituter = ConstVarSubstituter { consts: &consts };
    crate::visit::walk_module_mut(&mut substituter, module);

    retain_top_level_except_floor_traps(module, |s| is_promoted_const_let(s, &consts));
    true
}

/// A candidate has one unconditional literal definition and no other writes.
fn retain_initialized_constants<T>(module: &ir::Module, consts: &mut HashMap<VarName, T>) {
    if module.has_on_key() || module.has_error_continuation() {
        consts.clear();
        return;
    }
    let cfg = crate::cfg::Cfg::build(module);
    if cfg.nodes.is_empty() { return; }
    let fns = crate::dataflow::FnBodyReads::build(module);
    struct Reads<'a> {
        names: HashSet<VarName>,
        fns: &'a crate::dataflow::FnBodyReads,
    }
    impl crate::visit::Visitor for Reads<'_> {
        fn visit_var_read(&mut self, v: &VarName) { self.names.insert(v.clone()); }
        fn visit_expr(&mut self, expr: &Expr) {
            if let Expr::FnCall(name, _) = expr {
                self.names.extend(self.fns.free_vars(name).iter().cloned());
            }
            crate::visit::walk_expr(self, expr);
        }
    }
    let reads: Vec<_> = (0..cfg.nodes.len()).map(|id| {
        let mut r = Reads { names: HashSet::new(), fns: &fns };
        crate::visit::Visitor::visit_stmt(&mut r, cfg.nodes[id].line_no, cfg.stmt_at(id, module));
        r.names
    }).collect();
    consts.retain(|var, _| {
        let mut pending = vec![cfg.entry];
        let mut visited = vec![false; cfg.nodes.len()];
        while let Some(id) = pending.pop() {
            if std::mem::replace(&mut visited[id], true) { continue; }
            if reads[id].contains(var) { return false; }
            if matches!(cfg.stmt_at(id, module),
                Stmt::Let { var: target, .. } | Stmt::LetStr { var: target, .. } if target == var)
            { continue; }
            pending.extend(cfg.nodes[id].successors.iter().copied());
        }
        true
    });
}

fn retain_top_level_except_floor_traps(module: &mut ir::Module, drop_it: impl Fn(&Stmt) -> bool) {
    let traps = stack_floor_trap_positions(module);

    if traps.is_empty() {
        for line in &mut module.lines {
            line.stmts.retain(|s| !drop_it(s));
        }
        return;
    }
    for (line_idx, line) in module.lines.iter_mut().enumerate() {
        let mut idx = 0usize;
        line.stmts.retain(|s| {
            let here = idx;
            idx += 1;
            !drop_it(s) || traps.contains(&(line_idx, vec![here]))
        });
    }
}

/// Returns the set of line numbers that may be reached while
/// executing inside a GOSUB body. Computed by flood-filling forward
/// from every GOSUB target and following sequential fall-through and
/// GOTO/IF-THEN-line/ON-GOTO edges, stopping at RETURN. GOSUBs nested
/// inside subs queue their own targets too. Conservative: treats any
/// reachable line as "in a sub" — false positives only block
/// propagation, never unsoundly enable it.
fn compute_in_sub_lines(module: &ir::Module) -> HashSet<u16> {
    use std::collections::VecDeque;
    let mut line_idx: HashMap<u16, usize> = HashMap::new();
    for (i, line) in module.lines.iter().enumerate() {
        line_idx.insert(line.number, i);
    }
    // Collect every GOSUB target — both bare GOSUB and ON ... GOSUB.
    let mut targets: Vec<u16> = Vec::new();
    for line in &module.lines {
        for stmt in &line.stmts {
            collect_gosub_targets(stmt, &mut targets);
        }
    }
    let mut visited: HashSet<u16> = HashSet::new();
    let mut queue: VecDeque<usize> = VecDeque::new();
    for t in targets {
        if let Some(&idx) = line_idx.get(&t) {
            queue.push_back(idx);
        }
    }
    while let Some(idx) = queue.pop_front() {
        let line_no = module.lines[idx].number;
        if !visited.insert(line_no) {
            continue;
        }
        let line = &module.lines[idx];
        let mut hits_return = false;
        let mut transfers = false;
        for stmt in &line.stmts {
            collect_intra_sub_jumps(stmt, &mut |t| {
                if let Some(&i) = line_idx.get(&t) {
                    queue.push_back(i);
                }
            });
            // Nested GOSUBs ALSO put us inside the called sub's
            // body — re-enter the flood fill from those targets.
            collect_gosub_targets(stmt, &mut Vec::new()); // no-op placeholder
            match stmt {
                Stmt::GoSub { target } => {
                    if let Some(&i) = line_idx.get(target) {
                        queue.push_back(i);
                    }
                }
                Stmt::OnBranch {
                    kind: crate::ast::OnBranchKind::GoSub,
                    targets,
                    ..
                } => {
                    for t in targets {
                        if let Some(&i) = line_idx.get(t) {
                            queue.push_back(i);
                        }
                    }
                }
                _ => {}
            }
            if matches!(stmt, Stmt::Return | Stmt::Resume { .. }) {
                hits_return = true;
            }
            if matches!(
                stmt,
                Stmt::Return
                    | Stmt::End
                    | Stmt::Stop
                    | Stmt::Goto { .. }
                    | Stmt::Run(_)
                    | Stmt::Resume { .. }
            ) {
                transfers = true;
                break;
            }
        }
        if !hits_return && !transfers && idx + 1 < module.lines.len() {
            queue.push_back(idx + 1);
        }
    }
    visited
}

/// Push the targets of bare GOSUB and ON ... GOSUB statements, plus
/// any nested inside an IF THEN body, into `out`. Used to seed the
/// flood-fill in [`compute_in_sub_lines`].
fn collect_gosub_targets(stmt: &Stmt, out: &mut Vec<u16>) {
    match stmt {
        Stmt::GoSub { target } => out.push(*target),
        Stmt::OnBranch {
            kind: crate::ast::OnBranchKind::GoSub,
            targets,
            ..
        } => out.extend(targets.iter().copied()),
        Stmt::If {
            then: crate::ir::ThenIr::Stmts(inner),
            ..
        } => {
            for s in inner {
                collect_gosub_targets(s, out);
            }
        }
        Stmt::IfElse {
            then: crate::ir::ThenIr::Stmts(inner),
            else_then,
            ..
        } => {
            for s in inner {
                collect_gosub_targets(s, out);
            }
            if let crate::ir::ThenIr::Stmts(inner) = else_then {
                for s in inner {
                    collect_gosub_targets(s, out);
                }
            }
        }
        _ => {}
    }
}

/// Push the targets of intra-sub jumps (GOTO, IF-THEN-Line, ON GOTO,
/// IF-THEN body's GOTO) into `push`. Used by the sub flood-fill so
/// jumps within a sub stay tracked.
fn collect_intra_sub_jumps(stmt: &Stmt, push: &mut impl FnMut(u16)) {
    match stmt {
        Stmt::Goto { target } => push(*target),
        Stmt::OnBranch {
            kind: crate::ast::OnBranchKind::Goto,
            targets,
            ..
        } => {
            for t in targets {
                push(*t);
            }
        }
        Stmt::If {
            then: crate::ir::ThenIr::Goto(t),
            ..
        } => push(*t),
        Stmt::If {
            then: crate::ir::ThenIr::Stmts(inner),
            ..
        } => {
            for s in inner {
                collect_intra_sub_jumps(s, push);
            }
        }
        Stmt::IfElse {
            then, else_then, ..
        } => {
            if let crate::ir::ThenIr::Goto(t) = then {
                push(*t);
            }
            if let crate::ir::ThenIr::Stmts(inner) = then {
                for s in inner {
                    collect_intra_sub_jumps(s, push);
                }
            }
            if let crate::ir::ThenIr::Goto(t) = else_then {
                push(*t);
            }
            if let crate::ir::ThenIr::Stmts(inner) = else_then {
                for s in inner {
                    collect_intra_sub_jumps(s, push);
                }
            }
        }
        _ => {}
    }
}

#[derive(Default)]
struct ConstVarScanner {
    info: HashMap<VarName, VarInfo>,
    /// True when traversal is inside an IF THEN body, FOR body, or DEF
    /// FN body. Nested LETs are disqualified — those branches may be
    /// skipped at runtime so the value isn't guaranteed-set.
    nested: bool,
    /// Snapshot of the current line number (set in visit_stmt).
    current_line: u16,
    /// True while walking a DIM size expression. Reads here aren't
    /// counted as "non-DIM uses" — see `VarInfo::non_dim_use_count`.
    in_dim_expr: bool,
    /// Whether CLR or RUN can reset variables.
    saw_var_reset: bool,
}

impl ConstVarScanner {
    fn note_use(&mut self, v: &VarName) {
        let entry = self.info.entry(v.clone()).or_insert_with(VarInfo::new);
        if self.current_line < entry.first_use_line {
            entry.first_use_line = self.current_line;
        }
        if !self.in_dim_expr {
            entry.non_dim_use_count = entry.non_dim_use_count.saturating_add(1);
        }
    }
    fn mark_nonlet_write(&mut self, v: &VarName) {
        self.info
            .entry(v.clone())
            .or_insert_with(VarInfo::new)
            .has_nonlet_write = true;
    }
}

impl crate::visit::Visitor for ConstVarScanner {
    fn visit_var_read(&mut self, v: &VarName) {
        self.note_use(v);
    }

    fn visit_stmt(&mut self, line_no: u16, stmt: &Stmt) {
        let saved_line = self.current_line;
        self.current_line = line_no;
        match stmt {
            Stmt::Let { var, value } => {
                // Use-sites in the RHS first — important for the case
                // of a LET that REFERENCES the same var on its RHS
                // (counter increment). The use must be recorded before
                // the LET is counted as the source.
                self.visit_expr(value);
                if self.nested {
                    self.mark_nonlet_write(var);
                } else {
                    let entry = self.info.entry(var.clone()).or_insert_with(VarInfo::new);
                    entry.let_count = entry.let_count.saturating_add(1);
                    entry.let_line = line_no;
                    let raw_value = match value {
                        Expr::Number(n) if n.is_finite() => Some(*n),
                        Expr::Neg(inner) => match inner.as_ref() {
                            Expr::Number(n) if n.is_finite() => Some(-n),
                            _ => None,
                        },
                        _ => None,
                    };

                    entry.let_value = raw_value.and_then(|v| narrow_for_int_store(var, v));
                }
            }
            Stmt::LetStr { var, .. } => {
                self.mark_nonlet_write(var);
                crate::visit::walk_stmt(self, line_no, stmt);
            }
            Stmt::ArrayLet { name, .. } | Stmt::ArrayLetStr { name, .. } => {
                self.mark_nonlet_write(name);
                crate::visit::walk_stmt(self, line_no, stmt);
            }
            Stmt::If { cond, then } => {
                self.visit_expr(cond);
                if let crate::ir::ThenIr::Stmts(inner) = then {
                    let saved = self.nested;
                    self.nested = true;
                    for s in inner {
                        self.visit_stmt(line_no, s);
                    }
                    self.nested = saved;
                }
            }
            Stmt::For { var, .. } => {
                // FOR writes the loop counter — disqualify, then walk
                // the start/end/step expressions for use-sites.
                self.mark_nonlet_write(var);
                crate::visit::walk_stmt(self, line_no, stmt);
            }
            Stmt::Next { vars } => {
                for v in vars.iter().flatten() {
                    self.mark_nonlet_write(v);
                }
            }
            Stmt::Read(targets) | Stmt::Input { targets, .. } => {
                for t in targets {
                    match t {
                        crate::ir::ReadTarget::Scalar(v) => self.mark_nonlet_write(v),
                        crate::ir::ReadTarget::Array { indices, .. } => {
                            for e in indices {
                                self.visit_expr(e);
                            }
                        }
                    }
                }
            }
            Stmt::InputFile { file_num, targets } => {
                self.visit_expr(file_num);
                for t in targets {
                    match t {
                        crate::ir::ReadTarget::Scalar(v) => self.mark_nonlet_write(v),
                        crate::ir::ReadTarget::Array { indices, .. } => {
                            for e in indices {
                                self.visit_expr(e);
                            }
                        }
                    }
                }
            }
            Stmt::Get { var, .. } | Stmt::KeyGet { var } => self.mark_nonlet_write(var),
            Stmt::GetFile { file_num, vars } => {
                self.visit_expr(file_num);
                for v in vars {
                    self.mark_nonlet_write(v);
                }
            }

            Stmt::Clr | Stmt::Run(_) => {
                self.saw_var_reset = true;
            }
            Stmt::Dim(specs) => {
                for spec in specs {
                    self.mark_nonlet_write(&spec.name);
                    // Walk DIM sizes with `in_dim_expr` set so reads
                    // here don't tip a var into the "promote me" set
                    // by themselves. The ConstVarSubstituter still
                    // walks DIMs and folds the literal in for vars
                    // that *also* have non-DIM uses; a var read only
                    // in DIM stays as a runtime load.
                    let saved = self.in_dim_expr;
                    self.in_dim_expr = true;
                    for d in &spec.dims {
                        self.visit_expr(d);
                    }
                    self.in_dim_expr = saved;
                }
            }
            Stmt::DefFn { param, body, .. } => {
                self.mark_nonlet_write(param);
                self.visit_expr(body);
            }
            // Everything else: walk normally for use-collection.
            _ => crate::visit::walk_stmt(self, line_no, stmt),
        }
        self.current_line = saved_line;
    }
}

struct ConstVarSubstituter<'a> {
    consts: &'a HashMap<VarName, f64>,
}

impl<'a> crate::visit::MutVisitor for ConstVarSubstituter<'a> {
    fn visit_expr_mut(&mut self, e: &mut Expr) {
        if let Expr::Var(v) = e {
            if let Some(&n) = self.consts.get(v) {
                *e = Expr::Number(n);
                return;
            }
        }
        crate::visit::walk_expr_mut(self, e);
        // Re-fold this node: substitution may have made it constant.
        if let Some(folded) = try_fold(e) {
            *e = Expr::Number(folded);
            return;
        }
        // Algebraic identity rewrite: catches `x + 0`, `x * 1`,
        // `x XOR 0`, etc. that ConstVarProp synthesizes when one
        // operand is foldable to a special constant. Without this
        // codegen emits the dead arithmetic.
        if let Some(simplified) = try_simplify_identity(e) {
            *e = simplified;
        }
    }

    fn visit_stmt_mut(&mut self, line_no: u16, stmt: &mut Stmt) {
        crate::visit::walk_stmt_mut(self, line_no, stmt);
    }
}

fn is_promoted_const_let(stmt: &Stmt, consts: &HashMap<VarName, f64>) -> bool {
    match stmt {
        Stmt::Let { var, value } => {
            // Only the top-level LET that originally produced the
            // constant — value must be a literal (or unary-neg
            // literal) and the var must be in the consts map. Don't
            // touch nested LETs (the scan disqualified them above).
            if !consts.contains_key(var) {
                return false;
            }
            matches!(value, Expr::Number(_))
                || matches!(
                    value,
                    Expr::Neg(inner) if matches!(inner.as_ref(), Expr::Number(_))
                )
        }
        _ => false,
    }
}

#[derive(Default)]
struct VarInfo {
    let_count: u32,
    let_value: Option<f64>,
    let_line: u16,
    has_nonlet_write: bool,
    first_use_line: u16,
    /// Reads of v that don't sit inside a DIM size expression. Used
    /// to gate promotion: substituting a var that's *only* read in
    /// DIM forces static array allocation (array bytes baked into
    /// the PRG) without paying back via simpler use sites elsewhere.
    /// Better to leave the LET running so DIM keeps the runtime
    /// allocation path.
    non_dim_use_count: u32,
}

impl VarInfo {
    fn new() -> Self {
        Self {
            first_use_line: u16::MAX,
            ..Default::default()
        }
    }
}

// ===== Local constant propagation =====

/// Per-basic-block constant propagation. Unlike `ConstVarProp`, this
/// does not require a single global assignment and does not remove any
/// stores. It only substitutes scalar reads that are known from earlier
/// statements in the same line or THEN-body; control-flow joins and
/// opaque calls clear or kill facts conservatively.
pub struct LocalConstProp;

impl ir::Pass for LocalConstProp {
    fn name(&self) -> &'static str {
        "local-const-prop"
    }

    fn run(&self, module: &mut ir::Module) -> Result<(), ir::PassError> {
        let fre_observable = module_uses_fre(module);
        for line in &mut module.lines {
            let mut env = HashMap::new();
            local_const_prop_stmts(&mut line.stmts, &mut env, fre_observable);
        }
        Ok(())
    }
}

fn local_const_prop_stmts(
    stmts: &mut [Stmt],
    env: &mut HashMap<VarName, f64>,
    fre_observable: bool,
) {
    for stmt in stmts {
        match stmt {
            Stmt::If { cond, then } => {
                local_const_expr(cond, env);
                match then {
                    ThenIr::Goto(_) => {}
                    ThenIr::Stmts(inner) => {
                        let mut inner_env = env.clone();
                        local_const_prop_stmts(inner, &mut inner_env, fre_observable);
                        let effects = local_effects(inner);
                        if effects.opaque {
                            env.clear();
                        } else {
                            for var in effects.writes {
                                env.remove(&var);
                            }
                        }
                    }
                }
            }
            Stmt::IfElse {
                cond,
                then,
                else_then,
            } => {
                local_const_expr(cond, env);
                for branch in [then, else_then] {
                    if let ThenIr::Stmts(inner) = branch {
                        let mut inner_env = env.clone();
                        local_const_prop_stmts(inner, &mut inner_env, fre_observable);
                        let effects = local_effects(inner);
                        if effects.opaque {
                            env.clear();
                        } else {
                            for var in effects.writes {
                                env.remove(&var);
                            }
                        }
                    }
                }
            }
            Stmt::Rcomp { then, else_then } => {
                for branch in std::iter::once(then).chain(else_then.iter_mut()) {
                    if let ThenIr::Stmts(inner) = branch {
                        let mut inner_env = env.clone();
                        local_const_prop_stmts(inner, &mut inner_env, fre_observable);
                        let effects = local_effects(inner);
                        if effects.opaque {
                            env.clear();
                        } else {
                            for var in effects.writes {
                                env.remove(&var);
                            }
                        }
                    }
                }
            }
            _ => {
                local_rewrite_stmt_exprs(stmt, env, fre_observable);
                local_update_env(stmt, env);
            }
        }
    }
}

fn local_rewrite_stmt_exprs(stmt: &mut Stmt, env: &HashMap<VarName, f64>, fre_observable: bool) {
    match stmt {
        Stmt::RaiseError(_) | Stmt::Hang => {}
        Stmt::Let { value, .. } => local_const_expr(value, env),
        // Binding RHS: fold the sub-trees but leave an allocating outermost node alone when
        // `FRE` can see the heap.
        Stmt::LetStr { value, .. } => {
            if str_binding_fold_is_fre_visible(value, fre_observable) {
                local_const_str_children(value, env);
            } else {
                local_const_str(value, env);
            }
        }
        Stmt::ArrayLet { indices, value, .. } => {
            for e in indices {
                local_const_expr(e, env);
            }
            local_const_expr(value, env);
        }
        Stmt::ArrayLetStr { indices, value, .. } => {
            for e in indices {
                local_const_expr(e, env);
            }
            if str_binding_fold_is_fre_visible(value, fre_observable) {
                local_const_str_children(value, env);
            } else {
                local_const_str(value, env);
            }
        }
        Stmt::For {
            start, end, step, ..
        } => {
            local_const_expr(start, env);
            local_const_expr(end, env);
            local_const_expr(step, env);
        }
        Stmt::Read(targets) | Stmt::Input { targets, .. } => {
            for t in targets {
                local_const_read_target(t, env);
            }
        }
        Stmt::InputFile { file_num, targets } => {
            local_const_expr(file_num, env);
            for t in targets {
                local_const_read_target(t, env);
            }
        }
        Stmt::Poke { addr, value } => {
            local_const_expr(addr, env);
            local_const_expr(value, env);
        }
        Stmt::Dpoke { addr, value } => {
            local_const_expr(addr, env);
            local_const_expr(value, env);
        }
        Stmt::PokeFill {
            dst_start,
            dst_end,
            value,
        } => {
            local_const_expr(dst_start, env);
            local_const_expr(dst_end, env);
            local_const_expr(value, env);
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
            for e in [row, col, width, height] {
                local_const_expr(e, env);
            }
            if let Some(e) = ch {
                local_const_expr(e, env);
            }
            if let Some(e) = color {
                local_const_expr(e, env);
            }
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
                local_const_expr(e, env);
            }
        }
        Stmt::ScreenScroll {
            row,
            col,
            width,
            height,
            ..
        } => {
            for e in [row, col, width, height] {
                local_const_expr(e, env);
            }
        }
        Stmt::Color {
            border,
            background,
            pen,
        } => {
            if let Some(e) = border {
                local_const_expr(e, env);
            }
            if let Some(e) = background {
                local_const_expr(e, env);
            }
            if let Some(e) = pen {
                local_const_expr(e, env);
            }
        }
        Stmt::MobEnable { index, .. } => local_const_expr(index, env),
        Stmt::Multi { .. } | Stmt::HiCol | Stmt::Hires { .. } => {}
        Stmt::MultiColors { c1, c2, c3 } => {
            local_const_expr(c1, env);
            local_const_expr(c2, env);
            local_const_expr(c3, env);
        }
        Stmt::Border { color } => local_const_expr(color, env),
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
                local_const_expr(e, env);
            }
            if let Some(e) = mode {
                local_const_expr(e, env);
            }
        }
        Stmt::Rec {
            x,
            y,
            width,
            height,
            mode,
        } => {
            for e in [x, y, width, height] {
                local_const_expr(e, env);
            }
            if let Some(e) = mode {
                local_const_expr(e, env);
            }
        }
        Stmt::Draw { x, y, mode } | Stmt::DrawTo { x, y, mode } | Stmt::Paint { x, y, mode } => {
            local_const_expr(x, env);
            local_const_expr(y, env);
            if let Some(e) = mode {
                local_const_expr(e, env);
            }
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
            local_const_expr(cx, env);
            local_const_expr(cy, env);
            local_const_expr(radius, env);
            for opt in [ry, start, end, step, mode] {
                if let Some(e) = opt {
                    local_const_expr(e, env);
                }
            }
        }
        Stmt::Char {
            x,
            y,
            code,
            mode,
            zoom,
        } => {
            local_const_expr(x, env);
            local_const_expr(y, env);
            local_const_expr(code, env);
            if let Some(e) = mode {
                local_const_expr(e, env);
            }
            if let Some(e) = zoom {
                local_const_expr(e, env);
            }
        }
        Stmt::Text {
            x,
            y,
            mode,
            zoom,
            kerning,
            ..
        } => {
            local_const_expr(x, env);
            local_const_expr(y, env);
            if let Some(e) = mode {
                local_const_expr(e, env);
            }
            if let Some(e) = zoom {
                local_const_expr(e, env);
            }
            if let Some(e) = kerning {
                local_const_expr(e, env);
            }
        }
        Stmt::Rot { direction, length } => {
            local_const_expr(direction, env);
            if let Some(l) = length {
                local_const_expr(l, env);
            }
        }
        Stmt::DrawString { x, y, .. } => {
            local_const_expr(x, env);
            local_const_expr(y, env);
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
                local_const_expr(e, env);
            }
            for opt in [ry, mode] {
                if let Some(e) = opt {
                    local_const_expr(e, env);
                }
            }
        }
        Stmt::Sound { voice, freq } => {
            local_const_expr(voice, env);
            local_const_expr(freq, env);
        }
        Stmt::Envelope {
            voice,
            attack,
            decay,
            sustain,
            release,
        } => {
            for e in [voice, attack, decay, sustain, release] {
                local_const_expr(e, env);
            }
        }
        Stmt::Wave {
            voice,
            control,
            pulse,
        } => {
            local_const_expr(voice, env);
            local_const_expr(control, env);
            if let Some(e) = pulse {
                local_const_expr(e, env);
            }
        }
        Stmt::Music { tempo, tune } => {
            local_const_expr(tempo, env);
            local_const_str(tune, env);
        }
        Stmt::Play { mode } => local_const_expr(mode, env),
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
            for opt in [speed, color1, color2] {
                if let Some(e) = opt {
                    local_const_expr(e, env);
                }
            }
        }
        Stmt::LowCol {
            color1,
            color2,
            color3,
        } => {
            local_const_expr(color1, env);
            local_const_expr(color2, env);
            if let Some(e) = color3 {
                local_const_expr(e, env);
            }
        }
        Stmt::Mod { ink, paper } => {
            local_const_expr(ink, env);
            local_const_expr(paper, env);
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
                local_const_expr(e, env);
            }
            for opt in [mode, zoom] {
                if let Some(e) = opt {
                    local_const_expr(e, env);
                }
            }
        }
        Stmt::Copy { src, dst, len } => {
            local_const_expr(src, env);
            local_const_expr(dst, env);
            local_const_expr(len, env);
        }
        Stmt::ScrSave { addr, mode } | Stmt::ScrLoad { addr, mode } => {
            if let Some(e) = addr {
                local_const_expr(e, env);
            }
            if let Some(e) = mode {
                local_const_expr(e, env);
            }
        }
        Stmt::ScrDef { addr, mode, .. } => {
            local_const_expr(addr, env);
            if let Some(e) = mode {
                local_const_expr(e, env);
            }
        }
        Stmt::ScrRestore { .. } => {}
        Stmt::MemClr { addr, len, value } => {
            local_const_expr(addr, env);
            local_const_expr(len, env);
            if let Some(e) = value {
                local_const_expr(e, env);
            }
        }
        Stmt::MemTransfer { .. } => {}
        Stmt::MemDef {
            len,
            c64_addr,
            reu_addr,
            reu_bank,
            auto_inc,
            fixed,
        } => {
            local_const_expr(len, env);
            for opt in [c64_addr, reu_addr, reu_bank, auto_inc, fixed] {
                if let Some(e) = opt {
                    local_const_expr(e, env);
                }
            }
        }
        Stmt::MemLen { len } => local_const_expr(len, env),
        Stmt::MemC64Addr { addr } => local_const_expr(addr, env),
        Stmt::MemReuPos { addr, bank } => {
            local_const_expr(addr, env);
            local_const_expr(bank, env);
        }
        Stmt::MemRestore { auto_inc } => local_const_expr(auto_inc, env),
        Stmt::MemCont { mode } => local_const_expr(mode, env),
        Stmt::Design { addr, bytes } => {
            local_const_expr(addr, env);
            for e in bytes {
                local_const_expr(e, env);
            }
        }
        Stmt::SwapStr { .. } => {}
        Stmt::InsertBox {
            pattern,
            row,
            col,
            width,
            height,
            color,
        } => {
            local_const_str(pattern, env);
            for e in [row, col, width, height, color] {
                local_const_expr(e, env);
            }
        }
        Stmt::Mmob { index, x, y } => {
            local_const_expr(index, env);
            local_const_expr(x, env);
            local_const_expr(y, env);
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
                local_const_expr(e, env);
            }
            if let Some(e) = size {
                local_const_expr(e, env);
            }
            if let Some(e) = speed {
                local_const_expr(e, env);
            }
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
                local_const_expr(e, env);
            }
            if let Some(e) = size {
                local_const_expr(e, env);
            }
            if let Some(e) = speed {
                local_const_expr(e, env);
            }
        }
        Stmt::Rlocmob {
            index,
            dx,
            dy,
            speed,
        } => {
            local_const_expr(index, env);
            local_const_expr(dx, env);
            local_const_expr(dy, env);
            if let Some(e) = speed {
                local_const_expr(e, env);
            }
        }
        Stmt::Detect { mode } => local_const_expr(mode, env),
        Stmt::Cmob { color1, color2 } => {
            local_const_expr(color1, env);
            local_const_expr(color2, env);
        }
        Stmt::Bckgnds {
            color0,
            color1,
            color2,
            color3,
        } => {
            local_const_expr(color0, env);
            local_const_expr(color1, env);
            local_const_expr(color2, env);
            local_const_expr(color3, env);
        }
        Stmt::Cset { mode } => local_const_expr(mode, env),
        Stmt::Pause { ticks, .. } => local_const_expr(ticks, env),
        Stmt::Sys {
            addr,
            regs,
            param_vars,
            ..
        } => {
            local_const_expr(addr, env);
            for r in regs {
                local_const_expr(r, env);
            }

            for p in param_vars {
                local_const_expr(&mut p.value, env);
            }
        }
        Stmt::Wait { addr, mask, eor } => {
            local_const_expr(addr, env);
            local_const_expr(mask, env);
            if let Some(e) = eor {
                local_const_expr(e, env);
            }
        }
        Stmt::Open {
            file_num,
            device,
            secondary,
            filename,
        } => {
            local_const_expr(file_num, env);
            if let Some(e) = device {
                local_const_expr(e, env);
            }
            if let Some(e) = secondary {
                local_const_expr(e, env);
            }
            if let Some(s) = filename {
                local_const_str(s, env);
            }
        }
        Stmt::Close { file_num } | Stmt::GetFile { file_num, .. } => {
            local_const_expr(file_num, env);
        }
        Stmt::Print { items, .. } | Stmt::PrintFile { items, .. } | Stmt::Cmd { items, .. } => {
            for item in items {
                local_const_print_piece(item, env);
            }
        }
        Stmt::UseFormat { at, values, .. } => {
            if let Some((r, c)) = at {
                local_const_expr(r, env);
                local_const_expr(c, env);
            }
            for value in values {
                local_const_expr(value, env);
            }
        }
        Stmt::UseFormatDyn {
            at, ctrl, values, ..
        } => {
            if let Some((r, c)) = at {
                local_const_expr(r, env);
                local_const_expr(c, env);
            }
            local_const_str(ctrl, env);
            for value in values {
                local_const_expr(value, env);
            }
        }
        Stmt::Load {
            filename,
            device,
            secondary,
            load_addr,
        } => {
            local_const_str(filename, env);
            if let Some(e) = device {
                local_const_expr(e, env);
            }
            if let Some(e) = secondary {
                local_const_expr(e, env);
            }
            if let Some(e) = load_addr {
                local_const_expr(e, env);
            }
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
            local_const_str(filename, env);
            if let Some(e) = device {
                local_const_expr(e, env);
            }
            if let Some(e) = secondary {
                local_const_expr(e, env);
            }
        }
        Stmt::Disk { command } => local_const_str(command, env),
        Stmt::OnBranch { value, .. } => local_const_expr(value, env),
        Stmt::DoIf { cond } | Stmt::Until { cond } => local_const_expr(cond, env),
        Stmt::ExitLoop { cond } => {
            if let Some(cond) = cond {
                local_const_expr(cond, env);
            }
        }
        Stmt::ComputedGoto { target } => local_const_expr(target, env),
        Stmt::Reset { target } => local_const_expr(target, env),
        Stmt::OnKey { keys, .. } => local_const_str(keys, env),
        Stmt::Fetch {
            control,
            max_len,
            force,
            position,
            ..
        } => {
            local_const_str(control, env);
            local_const_expr(max_len, env);
            if let Some(e) = force {
                local_const_expr(e, env);
            }
            if let Some((r, c)) = position {
                local_const_expr(r, env);
                local_const_expr(c, env);
            }
        }
        Stmt::KeySet { index, text } => {
            local_const_expr(index, env);
            local_const_str(text, env);
        }
        Stmt::Dim(specs) => {
            for spec in specs {
                for dim in &mut spec.dims {
                    local_const_expr(dim, env);
                }
            }
        }
        Stmt::DefFn { .. } => {
            // DEF FN bodies are evaluated later, using the then-current
            // global variables, so local declaration-time facts must
            // not be captured into the body.
        }
        Stmt::Goto { .. }
        | Stmt::GoSub { .. }
        | Stmt::Return
        | Stmt::Next { .. }
        | Stmt::Do
        | Stmt::DoNull
        | Stmt::Done
        | Stmt::Else
        | Stmt::Repeat
        | Stmt::Loop
        | Stmt::EndLoop
        | Stmt::Disable
        | Stmt::Resume { .. }
        | Stmt::OnError { .. }
        | Stmt::ErrorRaise { .. }
        | Stmt::Nrm
        | Stmt::HardCopy
        | Stmt::Cold
        | Stmt::Retrace
        | Stmt::MemModeOn
        | Stmt::Rem(_)
        | Stmt::LineMark(_)
        | Stmt::End
        | Stmt::Stop
        | Stmt::Run(_)
        | Stmt::Clr
        | Stmt::Data(_)
        | Stmt::Restore
        | Stmt::Get { .. }
        | Stmt::KeyGet { .. }
        | Stmt::DisplayKeys => {}
        Stmt::If { .. } | Stmt::IfElse { .. } | Stmt::Rcomp { .. } => {
            unreachable!("branch handled by local_const_prop_stmts")
        }
    }
}

fn local_update_env(stmt: &Stmt, env: &mut HashMap<VarName, f64>) {
    match stmt {
        Stmt::RaiseError(_) | Stmt::Hang => {}
        Stmt::Let { var, value } => {
            if var.kind != VarKind::String && expr_is_pure(value) {
                if let Some(n) = local_expr_literal(value) {
                    // The fact recorded here is what the *variable* holds afterwards, not what
                    // the RHS evaluated to.
                    if let Some(narrowed) = narrow_for_int_store(var, n) {
                        env.insert(var.clone(), narrowed);
                        return;
                    }
                }
            }
            env.remove(var);
        }
        Stmt::LetStr { var, .. } => {
            env.remove(var);
        }
        Stmt::ArrayLet { name, .. } | Stmt::ArrayLetStr { name, .. } => {
            env.remove(name);
        }
        Stmt::Read(targets) | Stmt::Input { targets, .. } => {
            local_kill_read_targets(targets, env);
        }
        Stmt::InputFile { targets, .. } => {
            local_kill_read_targets(targets, env);
        }
        Stmt::Get { var, .. } | Stmt::KeyGet { var } => {
            env.remove(var);
        }
        Stmt::GetFile { vars, .. } => {
            for v in vars {
                env.remove(v);
            }
        }
        Stmt::Fetch { target, .. } => {
            env.remove(target);
        }
        Stmt::SwapStr { lhs, rhs } => {
            env.remove(lhs);
            env.remove(rhs);
        }
        Stmt::Dim(specs) => {
            for spec in specs {
                env.remove(&spec.name);
            }
        }
        Stmt::For { .. }
        | Stmt::Next { .. }
        | Stmt::GoSub { .. }
        | Stmt::Sys { .. }
        | Stmt::Run(_)
        | Stmt::Clr
        | Stmt::Return
        | Stmt::End
        | Stmt::Stop
        | Stmt::ComputedGoto { .. }
        | Stmt::Resume { .. }
        | Stmt::OnError { .. }
        | Stmt::ErrorRaise { .. }
        | Stmt::OnKey { .. } => {
            env.clear();
        }
        Stmt::Poke { .. }
        | Stmt::Dpoke { .. }
        | Stmt::PokeFill { .. }
        | Stmt::ScreenRect { .. }
        | Stmt::ScreenMove { .. }
        | Stmt::ScreenScroll { .. }
        | Stmt::Color { .. }
        | Stmt::MobEnable { .. }
        | Stmt::Multi { .. }
        | Stmt::MultiColors { .. }
        | Stmt::Sound { .. }
        | Stmt::Envelope { .. }
        | Stmt::Wave { .. }
        | Stmt::Music { .. }
        | Stmt::Play { .. }
        | Stmt::Flash { .. }
        | Stmt::Bflash { .. }
        | Stmt::HiCol
        | Stmt::Hires { .. }
        | Stmt::Border { .. }
        | Stmt::Line { .. }
        | Stmt::Draw { .. }
        | Stmt::Rec { .. }
        | Stmt::Block { .. }
        | Stmt::Circle { .. }
        | Stmt::Char { .. }
        | Stmt::Text { .. }
        | Stmt::DrawTo { .. }
        | Stmt::Rot { .. }
        | Stmt::DrawString { .. }
        | Stmt::Paint { .. }
        | Stmt::Angl { .. }
        | Stmt::LowCol { .. }
        | Stmt::Mod { .. }
        | Stmt::Dup { .. }
        | Stmt::Copy { .. }
        | Stmt::ScrSave { .. }
        | Stmt::ScrLoad { .. }
        | Stmt::ScrDef { .. }
        | Stmt::ScrRestore { .. }
        | Stmt::MemClr { .. }
        | Stmt::MemTransfer { .. }
        | Stmt::MemDef { .. }
        | Stmt::MemLen { .. }
        | Stmt::MemC64Addr { .. }
        | Stmt::MemReuPos { .. }
        | Stmt::MemRestore { .. }
        | Stmt::MemCont { .. }
        | Stmt::Design { .. }
        | Stmt::KeySet { .. }
        | Stmt::DisplayKeys
        | Stmt::InsertBox { .. }
        | Stmt::Mmob { .. }
        | Stmt::MmobGlide { .. }
        | Stmt::MobSet { .. }
        | Stmt::Rlocmob { .. }
        | Stmt::Detect { .. }
        | Stmt::Cmob { .. }
        | Stmt::Bckgnds { .. }
        | Stmt::Nrm
        | Stmt::HardCopy
        | Stmt::Cold
        | Stmt::Retrace
        | Stmt::MemModeOn
        | Stmt::Cset { .. }
        | Stmt::Pause { .. } => {
            local_apply_effect_summary(stmt, env);
        }
        Stmt::OnBranch {
            kind: OnBranchKind::GoSub,
            ..
        } => {
            env.clear();
        }
        Stmt::Goto { .. }
        | Stmt::OnBranch {
            kind: OnBranchKind::Goto,
            ..
        }
        | Stmt::Rem(_)
        | Stmt::LineMark(_)
        | Stmt::Wait { .. }
        | Stmt::Open { .. }
        | Stmt::Close { .. }
        | Stmt::Disk { .. }
        | Stmt::Print { .. }
        | Stmt::UseFormat { .. }
        | Stmt::UseFormatDyn { .. }
        | Stmt::PrintFile { .. }
        | Stmt::Cmd { .. }
        | Stmt::Load { .. }
        | Stmt::Verify { .. }
        | Stmt::Save { .. }
        | Stmt::Data(_)
        | Stmt::Restore
        | Stmt::Reset { .. }
        | Stmt::DefFn { .. } => {}
        // TSB `LOOP`, `REPEAT`, and `DO` are back-edge targets. Facts from
        // before the loop are only valid on the first iteration, so clear them.
        Stmt::Do | Stmt::Repeat | Stmt::Loop => env.clear(),
        Stmt::DoIf { .. }
        | Stmt::DoNull
        | Stmt::Done
        | Stmt::Else
        | Stmt::Until { .. }
        | Stmt::EndLoop
        | Stmt::ExitLoop { .. }
        | Stmt::Disable => {}
        Stmt::If { .. } | Stmt::IfElse { .. } | Stmt::Rcomp { .. } => {
            unreachable!("branch handled by local_const_prop_stmts")
        }
    }
}

fn local_apply_effect_summary(stmt: &Stmt, env: &mut HashMap<VarName, f64>) {
    let effects = effect_summary_for_stmt(stmt);
    if effects.opaque_write
        || effects.writes.contains(&EffectRegion::ProgramState)
        || effects.writes.contains(&EffectRegion::SysMemUnknown)
    {
        env.clear();
        return;
    }
    env.retain(|var, _| {
        let region = match var.kind {
            VarKind::Float => EffectRegion::ScalarFloat(var.clone()),
            VarKind::Integer => EffectRegion::ScalarInteger(var.clone()),
            VarKind::String => EffectRegion::ScalarString(var.clone()),
        };
        !effects.writes_region(&region)
    });
}

fn local_const_expr(e: &mut Expr, env: &HashMap<VarName, f64>) {
    match e {
        Expr::Var(v) => {
            if let Some(&n) = env.get(v) {
                *e = Expr::Number(n);
            }
            return;
        }
        Expr::Neg(inner)
        | Expr::Not(inner)
        | Expr::Func1(_, inner)
        | Expr::Peek(inner)
        | Expr::MemPeek(inner)
        | Expr::FnCall(_, inner)
        | Expr::Pos(inner)
        | Expr::Fre(inner)
        | Expr::Usr(inner)
        | Expr::Joy(inner)
        | Expr::Pot(inner) => local_const_expr(inner, env),
        Expr::Bin(_, l, r) => {
            local_const_expr(l, env);
            local_const_expr(r, env);
        }
        Expr::ArrayRef(_, indices) => {
            for idx in indices {
                local_const_expr(idx, env);
            }
        }
        Expr::Len(s) | Expr::Asc(s) | Expr::Val(s) | Expr::Nrm(s) => local_const_str(s, env),
        Expr::StrCompare(_, l, r) => {
            local_const_str(l, env);
            local_const_str(r, env);
        }
        Expr::At(row, col) => {
            local_const_expr(row, env);
            local_const_expr(col, env);
        }
        Expr::Test(x, y) => {
            local_const_expr(x, env);
            local_const_expr(y, env);
        }
        Expr::Check { first, second } => {
            local_const_expr(first, env);
            if let Some(e) = second {
                local_const_expr(e, env);
            }
        }
        Expr::Inst {
            haystack,
            needle,
            start,
        } => {
            local_const_str(haystack, env);
            local_const_str(needle, env);
            if let Some(e) = start {
                local_const_expr(e, env);
            }
        }
        Expr::Number(_) | Expr::String(_) | Expr::Inkey | Expr::Lin => {}
    }
    if let Some(folded) = try_fold(e) {
        *e = Expr::Number(folded);
        return;
    }
    if let Some(simplified) = try_simplify_identity(e) {
        *e = simplified;
    }
}

fn local_const_str(s: &mut StrExpr, env: &HashMap<VarName, f64>) {
    local_const_str_children(s, env);
    if let Some(folded) = try_fold_str(s) {
        *s = StrExpr::Literal(folded);
    }
}

/// Propagate and fold string children while preserving the outer allocation.
fn local_const_str_children(s: &mut StrExpr, env: &HashMap<VarName, f64>) {
    match s {
        StrExpr::Chr(e) | StrExpr::Str(e) | StrExpr::HexFmt(e) | StrExpr::BinFmt(e) => {
            local_const_expr(e, env)
        }
        StrExpr::Concat(l, r) => {
            local_const_str(l, env);
            local_const_str(r, env);
        }
        StrExpr::Left(inner, n) | StrExpr::Right(inner, n) => {
            local_const_str(inner, env);
            local_const_expr(n, env);
        }
        StrExpr::Mid(inner, start, len) => {
            local_const_str(inner, env);
            local_const_expr(start, env);
            if let Some(len) = len {
                local_const_expr(len, env);
            }
        }
        StrExpr::Dup(inner, n) => {
            local_const_str(inner, env);
            local_const_expr(n, env);
        }
        StrExpr::Insert(s, t, pos) => {
            local_const_str(s, env);
            local_const_str(t, env);
            local_const_expr(pos, env);
        }
        StrExpr::ArrayRef(_, indices) => {
            for idx in indices {
                local_const_expr(idx, env);
            }
        }
        StrExpr::Literal(_) | StrExpr::Var(_) | StrExpr::GetKey => {}
    }
}

fn local_const_print_piece(piece: &mut PrintPiece, env: &HashMap<VarName, f64>) {
    match piece {
        PrintPiece::Expr(e)
        | PrintPiece::CharOut(e)
        | PrintPiece::TabTo(e)
        | PrintPiece::Spc(e) => local_const_expr(e, env),
        PrintPiece::StrExpr(s) => local_const_str(s, env),
        PrintPiece::PositionAt(r, c) => {
            local_const_expr(r, env);
            local_const_expr(c, env);
        }
        PrintPiece::LiteralString(_) | PrintPiece::Tab => {}
    }
}

fn local_const_read_target(target: &mut ir::ReadTarget, env: &HashMap<VarName, f64>) {
    if let ir::ReadTarget::Array { indices, .. } = target {
        for idx in indices {
            local_const_expr(idx, env);
        }
    }
}

fn local_kill_read_targets(targets: &[ir::ReadTarget], env: &mut HashMap<VarName, f64>) {
    for target in targets {
        match target {
            ir::ReadTarget::Scalar(v) => {
                env.remove(v);
            }
            ir::ReadTarget::Array { name, .. } => {
                env.remove(name);
            }
        }
    }
}

fn local_expr_literal(e: &Expr) -> Option<f64> {
    match e {
        Expr::Number(n) if n.is_finite() => Some(*n),
        Expr::Neg(inner) => match inner.as_ref() {
            Expr::Number(n) if n.is_finite() => Some(-*n),
            _ => None,
        },
        _ => None,
    }
}

#[derive(Default)]
struct LocalEffects {
    writes: HashSet<VarName>,
    opaque: bool,
}

fn local_effects(stmts: &[Stmt]) -> LocalEffects {
    let mut effects = LocalEffects::default();
    for stmt in stmts {
        local_note_effects(stmt, &mut effects);
    }
    effects
}

fn local_note_effects(stmt: &Stmt, effects: &mut LocalEffects) {
    match stmt {
        Stmt::RaiseError(_) | Stmt::Hang => {}
        Stmt::Let { var, .. }
        | Stmt::LetStr { var, .. }
        | Stmt::Get { var, .. }
        | Stmt::KeyGet { var } => {
            effects.writes.insert(var.clone());
        }
        Stmt::Fetch { target, .. } => {
            effects.writes.insert(target.clone());
        }
        Stmt::SwapStr { lhs, rhs } => {
            effects.writes.insert(lhs.clone());
            effects.writes.insert(rhs.clone());
        }
        Stmt::GetFile { vars, .. } => {
            for v in vars {
                effects.writes.insert(v.clone());
            }
        }
        Stmt::ArrayLet { name, .. } | Stmt::ArrayLetStr { name, .. } => {
            effects.writes.insert(name.clone());
        }
        Stmt::Dim(specs) => {
            for spec in specs {
                effects.writes.insert(spec.name.clone());
            }
        }
        Stmt::Read(targets) | Stmt::Input { targets, .. } | Stmt::InputFile { targets, .. } => {
            for target in targets {
                match target {
                    ir::ReadTarget::Scalar(v) => {
                        effects.writes.insert(v.clone());
                    }
                    ir::ReadTarget::Array { name, .. } => {
                        effects.writes.insert(name.clone());
                    }
                }
            }
        }
        Stmt::For { var, .. } => {
            effects.writes.insert(var.clone());
            effects.opaque = true;
        }
        Stmt::Next { vars } => {
            for var in vars.iter().flatten() {
                effects.writes.insert(var.clone());
            }
            effects.opaque = true;
        }
        Stmt::GoSub { .. }
        | Stmt::Sys { .. }
        | Stmt::Poke { .. }
        | Stmt::Dpoke { .. }
        | Stmt::PokeFill { .. }
        | Stmt::Copy { .. }
        | Stmt::ScrSave { .. }
        | Stmt::ScrLoad { .. }
        | Stmt::ScrDef { .. }
        | Stmt::ScrRestore { .. }
        | Stmt::MemClr { .. }
        | Stmt::MemTransfer { .. }
        | Stmt::MemDef { .. }
        | Stmt::MemLen { .. }
        | Stmt::MemC64Addr { .. }
        | Stmt::MemReuPos { .. }
        | Stmt::MemRestore { .. }
        | Stmt::MemCont { .. }
        | Stmt::Design { .. }
        | Stmt::Run(_)
        | Stmt::Clr
        | Stmt::Return
        | Stmt::End
        | Stmt::Stop
        | Stmt::ComputedGoto { .. }
        | Stmt::Resume { .. }
        | Stmt::OnError { .. }
        | Stmt::ErrorRaise { .. }
        | Stmt::OnKey { .. }
        | Stmt::OnBranch {
            kind: OnBranchKind::GoSub,
            ..
        } => {
            effects.opaque = true;
        }
        Stmt::If {
            then: ThenIr::Stmts(inner),
            ..
        } => {
            let inner_effects = local_effects(inner);
            effects.writes.extend(inner_effects.writes);
            effects.opaque |= inner_effects.opaque;
        }
        Stmt::IfElse {
            then, else_then, ..
        } => {
            for branch in [then, else_then] {
                if let ThenIr::Stmts(inner) = branch {
                    let inner_effects = local_effects(inner);
                    effects.writes.extend(inner_effects.writes);
                    effects.opaque |= inner_effects.opaque;
                }
            }
        }
        Stmt::Rcomp { then, else_then } => {
            for branch in std::iter::once(then).chain(else_then.iter()) {
                if let ThenIr::Stmts(inner) = branch {
                    let inner_effects = local_effects(inner);
                    effects.writes.extend(inner_effects.writes);
                    effects.opaque |= inner_effects.opaque;
                }
            }
        }
        Stmt::Goto { .. }
        | Stmt::If {
            then: ThenIr::Goto(_),
            ..
        }
        | Stmt::OnBranch {
            kind: OnBranchKind::Goto,
            ..
        }
        | Stmt::DoIf { .. }
        | Stmt::Do
        | Stmt::DoNull
        | Stmt::Done
        | Stmt::Else
        | Stmt::Repeat
        | Stmt::Until { .. }
        | Stmt::Loop
        | Stmt::EndLoop
        | Stmt::ExitLoop { .. }
        | Stmt::Disable
        | Stmt::Rem(_)
        | Stmt::LineMark(_)
        | Stmt::Wait { .. }
        | Stmt::ScreenRect { .. }
        | Stmt::ScreenMove { .. }
        | Stmt::ScreenScroll { .. }
        | Stmt::Color { .. }
        | Stmt::MobEnable { .. }
        | Stmt::Multi { .. }
        | Stmt::MultiColors { .. }
        | Stmt::Sound { .. }
        | Stmt::Envelope { .. }
        | Stmt::Wave { .. }
        | Stmt::Music { .. }
        | Stmt::Play { .. }
        | Stmt::Flash { .. }
        | Stmt::Bflash { .. }
        | Stmt::HiCol
        | Stmt::Hires { .. }
        | Stmt::Border { .. }
        | Stmt::Line { .. }
        | Stmt::Draw { .. }
        | Stmt::Rec { .. }
        | Stmt::Block { .. }
        | Stmt::Circle { .. }
        | Stmt::Char { .. }
        | Stmt::Text { .. }
        | Stmt::DrawTo { .. }
        | Stmt::Rot { .. }
        | Stmt::DrawString { .. }
        | Stmt::Paint { .. }
        | Stmt::Angl { .. }
        | Stmt::LowCol { .. }
        | Stmt::Mod { .. }
        | Stmt::Dup { .. }
        | Stmt::InsertBox { .. }
        | Stmt::Mmob { .. }
        | Stmt::MmobGlide { .. }
        | Stmt::MobSet { .. }
        | Stmt::Rlocmob { .. }
        | Stmt::Detect { .. }
        | Stmt::Cmob { .. }
        | Stmt::Bckgnds { .. }
        | Stmt::Nrm
        | Stmt::HardCopy
        | Stmt::Cold
        | Stmt::Retrace
        | Stmt::MemModeOn
        | Stmt::Cset { .. }
        | Stmt::Pause { .. }
        | Stmt::KeySet { .. }
        | Stmt::DisplayKeys
        | Stmt::Open { .. }
        | Stmt::Close { .. }
        | Stmt::Disk { .. }
        | Stmt::Print { .. }
        | Stmt::UseFormat { .. }
        | Stmt::UseFormatDyn { .. }
        | Stmt::PrintFile { .. }
        | Stmt::Cmd { .. }
        | Stmt::Load { .. }
        | Stmt::Verify { .. }
        | Stmt::Save { .. }
        | Stmt::Data(_)
        | Stmt::Restore
        | Stmt::Reset { .. }
        | Stmt::DefFn { .. } => {}
    }
}

// ===== Integer promotion =====

/// Demote float-typed scalar variables to `VarKind::Integer` when
/// every value ever stored in them is provably an i16-range integer.
/// The codegen automatically switches to 2-byte storage and the
/// inline int16 fast paths once a var's kind flips.
///
/// Conservative v1 rules:
///   * Var must be `VarKind::Float` (already-int doesn't need work).
///   * Skip `TI` / `ST` (system vars) and DEF FN parameters.
///   * No INPUT, READ, or GET writes — those bring runtime-unknown
///     values that could exceed i16 range.
///   * Every LET RHS and every FOR start/end/step must be
///     "int-stayable" — see `int_stayable`. Recursive: a value can
///     reference another promoted var.
///
/// The set is computed iteratively: start optimistic (all float vars
/// are candidates), then drop any whose definitions don't pass
/// `int_stayable` against the current candidate set. Repeat until
/// the set stabilises.
/// Promote a Float numeric array to Integer (2-byte) storage when
/// every value ever stored in it is provably an integer within i16
/// range. Bubble-sort-class programs that `DIM` a default (float)
/// array but only ever hold small integers — DATA loads, element
/// copies, integer literals — get 2-byte elements instead of 5-byte
/// MFLPT floats: the hot loop's element loads, compares and swaps drop
/// from ROM FAC routines to int ops, and the per-element stride
/// shrinks from 5 to 2. Reuses the existing `A%()` integer-array
/// codegen by flipping the array's `VarName` kind in the IR.
///
/// Conservative by construction. An array stays Float if any write to
/// it is: arithmetic or a function call (could overflow i16 or be
/// fractional), a non-integer literal, INPUT/GET (runtime-unknown,
/// possibly fractional), a READ when DATA isn't all integer-in-i16, or
/// a reference to a value not itself proven integer. Reads are always
/// safe — an integer widens to float exactly.
///
/// Soundness rests on a coupled fixpoint over arrays AND scalars: a
/// write like `A(J)=T` is only safe once `T` is proven integer, and
/// `T=A(J)` is only safe once `A` is. We start optimistic (every Float
/// array/scalar is a candidate) and drop any whose writes don't hold
/// under the current candidate set until it stabilises, then promote
/// the surviving arrays.
pub struct IntArrayPromote;

#[derive(Clone)]
enum WriteReq {
    /// The written value must be an integer-safe expression.
    Expr(Expr),
    /// A READ — safe iff every numeric DATA value is integer in i16.
    Data,
    /// A FOR header — safe iff start, end and step are all int-safe.
    ForBounds(Expr, Expr, Expr),
}

#[derive(Default)]
struct ArrayPromoteFacts {
    float_scalars: HashSet<VarName>,
    float_arrays: HashSet<VarName>,
    disq_scalars: HashSet<VarName>,
    disq_arrays: HashSet<VarName>,
    scalar_writes: HashMap<VarName, Vec<WriteReq>>,
    array_writes: HashMap<VarName, Vec<WriteReq>>,
    /// Array names that have a separate integer-typed form.
    int_array_bases: HashSet<String>,
}

impl ArrayPromoteFacts {
    /// Record an integer-typed array name.
    fn note_array_name(&mut self, name: &VarName) {
        if name.kind == VarKind::Integer {
            self.int_array_bases.insert(name.base.clone());
        }
    }
}

impl crate::visit::Visitor for ArrayPromoteFacts {
    fn visit_expr(&mut self, e: &Expr) {
        match e {
            Expr::Var(v) if v.kind == VarKind::Float => {
                self.float_scalars.insert(v.clone());
            }
            Expr::ArrayRef(a, _) => {
                if a.kind == VarKind::Float {
                    self.float_arrays.insert(a.clone());
                }
                self.note_array_name(a);
            }
            _ => {}
        }
        crate::visit::walk_expr(self, e);
    }

    fn visit_stmt(&mut self, line_no: u16, stmt: &Stmt) {
        match stmt {
            Stmt::ArrayLet { name, .. } | Stmt::ArrayLetStr { name, .. } => {
                self.note_array_name(name)
            }
            Stmt::Dim(specs) => {
                for spec in specs {
                    self.note_array_name(&spec.name);
                }
            }
            Stmt::Read(targets) | Stmt::Input { targets, .. } | Stmt::InputFile { targets, .. } => {
                for t in targets {
                    if let ir::ReadTarget::Array { name, .. } = t {
                        self.note_array_name(name);
                    }
                }
            }
            Stmt::Get { var, indices } if !indices.is_empty() => self.note_array_name(var),
            Stmt::Fetch {
                target,
                target_indices,
                ..
            } if !target_indices.is_empty() => self.note_array_name(target),
            _ => {}
        }
        match stmt {
            Stmt::Let { var, value } if var.kind == VarKind::Float => {
                self.float_scalars.insert(var.clone());
                self.scalar_writes
                    .entry(var.clone())
                    .or_default()
                    .push(WriteReq::Expr(value.clone()));
            }
            Stmt::ArrayLet { name, value, .. } if name.kind == VarKind::Float => {
                self.float_arrays.insert(name.clone());
                self.array_writes
                    .entry(name.clone())
                    .or_default()
                    .push(WriteReq::Expr(value.clone()));
            }
            Stmt::For {
                var,
                start,
                end,
                step,
                ..
            } if var.kind == VarKind::Float => {
                self.float_scalars.insert(var.clone());
                self.scalar_writes
                    .entry(var.clone())
                    .or_default()
                    .push(WriteReq::ForBounds(
                        start.clone(),
                        end.clone(),
                        step.clone(),
                    ));
            }
            Stmt::Read(targets) => {
                for t in targets {
                    match t {
                        ir::ReadTarget::Scalar(v) if v.kind == VarKind::Float => {
                            self.float_scalars.insert(v.clone());
                            self.scalar_writes
                                .entry(v.clone())
                                .or_default()
                                .push(WriteReq::Data);
                        }
                        ir::ReadTarget::Array { name, .. } if name.kind == VarKind::Float => {
                            self.float_arrays.insert(name.clone());
                            self.array_writes
                                .entry(name.clone())
                                .or_default()
                                .push(WriteReq::Data);
                        }
                        _ => {}
                    }
                }
            }
            // Runtime-unknown inputs disqualify their targets — a user
            // or file could supply a fractional or out-of-range value.
            Stmt::Input { targets, .. } | Stmt::InputFile { targets, .. } => {
                for t in targets {
                    match t {
                        ir::ReadTarget::Scalar(v) if v.kind == VarKind::Float => {
                            self.disq_scalars.insert(v.clone());
                        }
                        ir::ReadTarget::Array { name, .. } if name.kind == VarKind::Float => {
                            self.disq_arrays.insert(name.clone());
                        }
                        _ => {}
                    }
                }
            }
            // GET's value is runtime-unknown, so its target can never be promoted.
            Stmt::Get { var, indices } if var.kind == VarKind::Float => {
                if indices.is_empty() {
                    self.disq_scalars.insert(var.clone());
                } else {
                    self.disq_arrays.insert(var.clone());
                }
            }
            Stmt::KeyGet { var } if var.kind == VarKind::Float => {
                self.disq_scalars.insert(var.clone());
            }
            Stmt::GetFile { vars, .. } => {
                for v in vars {
                    if v.kind == VarKind::Float {
                        self.disq_scalars.insert(v.clone());
                    }
                }
            }

            Stmt::Fetch {
                target,
                target_indices,
                ..
            } if target.kind == VarKind::Float => {
                if target_indices.is_empty() {
                    self.disq_scalars.insert(target.clone());
                } else {
                    self.disq_arrays.insert(target.clone());
                }
            }
            _ => {}
        }
        crate::visit::walk_stmt(self, line_no, stmt);
    }
}

/// True iff every numeric DATA value in the module is an integer that
/// fits i16 — the condition under which a READ into an integer array
/// can't store a fractional or out-of-range value.
fn data_all_integer_i16(module: &ir::Module) -> bool {
    let mut any = false;
    for line in &module.lines {
        for stmt in &line.stmts {
            if let Stmt::Data(values) = stmt {
                for value in values {

                    let Some(f) = crate::codegen::data_item_numeric_value(value) else {
                        continue;
                    };
                    any = true;
                    if !f.is_finite()
                        || f.fract() != 0.0
                        || f < i16::MIN as f64
                        || f > i16::MAX as f64
                    {
                        return false;
                    }
                }
            }
        }
    }
    // No DATA at all: vacuously fine (no READ can pull a bad value).
    let _ = any;
    true
}

/// Is `e` an integer-valued expression within i16, given the current
/// candidate sets? Conservative: only literals, references to
/// already-integer or candidate scalars/arrays, and their negation
/// count. Any arithmetic, function, PEEK, etc. fails — those could
/// overflow i16 or be fractional.
fn array_rhs_int_safe(
    e: &Expr,
    scalar_cand: &HashSet<VarName>,
    array_cand: &HashSet<VarName>,
) -> bool {
    match e {
        Expr::Number(n) => {
            n.is_finite() && n.fract() == 0.0 && (i16::MIN as f64..=i16::MAX as f64).contains(n)
        }
        Expr::Neg(inner) => array_rhs_int_safe(inner, scalar_cand, array_cand),
        Expr::Var(v) => v.kind == VarKind::Integer || scalar_cand.contains(v),
        Expr::ArrayRef(a, _) => a.kind == VarKind::Integer || array_cand.contains(a),
        _ => false,
    }
}

/// MutVisitor that flips the kind of promoted arrays from Float to
/// Integer everywhere they appear — array reads, writes, DIMs, and
/// READ targets. Scalar `Var` references (which may share the array's
/// base name) are deliberately left untouched.
struct ArrayKindRewriter {
    promote: HashSet<VarName>,
}

impl ArrayKindRewriter {
    fn fix(&self, name: &mut VarName) {
        if name.kind == VarKind::Float && self.promote.contains(name) {
            name.kind = VarKind::Integer;
        }
    }
}

impl crate::visit::MutVisitor for ArrayKindRewriter {
    fn visit_expr_mut(&mut self, e: &mut Expr) {
        if let Expr::ArrayRef(name, _) = e {
            self.fix(name);
        }
        crate::visit::walk_expr_mut(self, e);
    }

    fn visit_stmt_mut(&mut self, line_no: u16, stmt: &mut Stmt) {
        match stmt {
            Stmt::ArrayLet { name, .. } => self.fix(name),
            Stmt::Dim(specs) => {
                for spec in specs.iter_mut() {
                    self.fix(&mut spec.name);
                }
            }
            Stmt::Read(targets) | Stmt::Input { targets, .. } | Stmt::InputFile { targets, .. } => {
                for t in targets.iter_mut() {
                    if let ir::ReadTarget::Array { name, .. } = t {
                        self.fix(name);
                    }
                }
            }
            _ => {}
        }
        crate::visit::walk_stmt_mut(self, line_no, stmt);
    }
}

impl ir::Pass for IntArrayPromote {
    fn name(&self) -> &'static str {
        "int-array-promote"
    }

    fn run(&self, module: &mut ir::Module) -> Result<(), ir::PassError> {
        let mut facts = ArrayPromoteFacts::default();
        crate::visit::walk_module(&mut facts, module);

        let data_ok = data_all_integer_i16(module);

        // Optimistic start: every non-disqualified Float scalar/array
        // is a candidate.
        let mut scalar_cand: HashSet<VarName> = facts
            .float_scalars
            .difference(&facts.disq_scalars)
            .cloned()
            .collect();
        let mut array_cand: HashSet<VarName> = facts
            .float_arrays
            .difference(&facts.disq_arrays)
            // BASIC v2 gives `A()` and `A%()` separate storage and separate DIM bounds.
            .filter(|a| !facts.int_array_bases.contains(&a.base))
            .cloned()
            .collect();

        let write_ok = |reqs: Option<&Vec<WriteReq>>,
                        scalar_cand: &HashSet<VarName>,
                        array_cand: &HashSet<VarName>| {
            let Some(reqs) = reqs else { return true }; // no writes ⇒ value stays 0
            reqs.iter().all(|req| match req {
                WriteReq::Expr(e) => array_rhs_int_safe(e, scalar_cand, array_cand),
                WriteReq::Data => data_ok,
                WriteReq::ForBounds(s, e, st) => {
                    array_rhs_int_safe(s, scalar_cand, array_cand)
                        && array_rhs_int_safe(e, scalar_cand, array_cand)
                        && array_rhs_int_safe(st, scalar_cand, array_cand)
                }
            })
        };

        loop {
            // Compute drops by reading the sets immutably, then apply —
            // avoids a self-borrow while iterating.
            let drop_scalars: Vec<VarName> = scalar_cand
                .iter()
                .filter(|v| !write_ok(facts.scalar_writes.get(*v), &scalar_cand, &array_cand))
                .cloned()
                .collect();
            let drop_arrays: Vec<VarName> = array_cand
                .iter()
                .filter(|a| !write_ok(facts.array_writes.get(*a), &scalar_cand, &array_cand))
                .cloned()
                .collect();
            if drop_scalars.is_empty() && drop_arrays.is_empty() {
                break;
            }
            for v in drop_scalars {
                scalar_cand.remove(&v);
            }
            for a in drop_arrays {
                array_cand.remove(&a);
            }
        }

        if array_cand.is_empty() {
            return Ok(());
        }

        let mut rewriter = ArrayKindRewriter {
            promote: array_cand,
        };
        crate::visit::walk_module_mut(&mut rewriter, module);
        Ok(())
    }
}

pub struct IntPromote;

impl ir::Pass for IntPromote {
    fn name(&self) -> &'static str {
        "int-promote"
    }

    fn run(&self, module: &mut ir::Module) -> Result<(), ir::PassError> {
        let promote = compute_int_promotable(module);
        if promote.is_empty() {
            return Ok(());
        }
        let mut promoter = VarKindPromoter { promote: &promote };
        promoter.rewrite_module(module);
        Ok(())
    }
}

/// Compute the set of vars that are safe to demote to integer.
/// Returns `HashSet<VarName>` keyed by the original (Float) names.
fn compute_int_promotable(module: &ir::Module) -> HashSet<VarName> {
    // Step 1: gather every "structured write" RHS per var, plus
    // disqualify any var touched by INPUT/READ/GET/DEF-FN.
    let mut info = WriteCollector {
        defs: HashMap::new(),
        bad: HashSet::new(),
        scalar_used: HashSet::new(),
        array_used: HashSet::new(),
    };
    crate::visit::walk_module(&mut info, module);
    let WriteCollector {
        defs,
        mut bad,
        scalar_used,
        array_used,
    } = info;

    bad.extend(scan_for_counters_with_unsafe_body(module));
    // V2 IR shares VarName between scalar `A` and array `A()`, but at
    // codegen they're separate slots — promoting one to Integer flips
    // the kind everywhere, and the codegen would then read the other
    // through int-typed paths even if it was written with floats.
    // Until we tag scalars and arrays separately in the IR, just
    // disqualify any var used in both contexts.
    for v in scalar_used.intersection(&array_used) {
        bad.insert(v.clone());
    }
    disqualify_percent_twins(&scalar_used, &array_used, &mut bad);

    // Step 2: seed candidates. Any Float var that has at least one
    // structured write and no bad writes is in the running.
    let mut candidates: HashSet<VarName> = defs
        .keys()
        .filter(|v| {
            v.kind == VarKind::Float
                && v.base != "TI"
                && v.base != "ST"
                && v.base != "ER"
                && v.base != "EL"
                && !bad.contains(v)
        })
        .cloned()
        .collect();
    if candidates.is_empty() {
        return candidates;
    }

    let for_counters = collect_for_counter_vars(module);
    // Bounds for hand-rolled counters, from their loop-exit tests.
    let mut guarded = guarded_counter_bounds(module);
    let mut bounded_families = HashSet::new();
    let mut counted: HashSet<VarName> = HashSet::new();
    // Unproved accumulators are unsafe only on cycles that are not FOR/NEXT.
    let uncounted_cycle = module_has_uncounted_cycle(module);
    for (var, range) in bounded_accumulator_ranges(module) {
        // Both are independent whole-program bounds.
        if var.kind == VarKind::Float && range.fits_i16() {
            bounded_families.insert(split_family_base(&var).to_owned());
            counted.insert(var.clone());
            guarded.entry(var).or_insert(range);
        }
    }

    let trace = std::env::var("INTPROMOTE_DEBUG").is_ok();
    if trace {
        let mut seed: Vec<&str> = candidates.iter().map(|v| v.base.as_str()).collect();
        seed.sort_unstable();
        let mut badv: Vec<&str> = bad.iter().map(|v| v.base.as_str()).collect();
        badv.sort_unstable();
        badv.dedup();
        eprintln!("[intpromote] seed candidates: {seed:?}");
        eprintln!("[intpromote] disqualified before seeding: {badv:?}");
    }
    loop {
        let mut to_drop: Vec<VarName> = Vec::new();
        for v in &candidates {
            let Some(values) = defs.get(v) else { continue };
            for value in values {
                if !int_stayable(value, &candidates) {
                    if trace {
                        eprintln!(
                            "[intpromote] DROP {} (not int-stayable: one definition is {:?})",
                            v.base, value
                        );
                    }
                    to_drop.push(v.clone());
                    break;
                }
            }
        }
        if to_drop.is_empty() {

            to_drop =
                int_range_escapees(
                    &defs,
                    &candidates,
                    &for_counters,
                    &guarded,
                    &counted,
                    uncounted_cycle,
                );
            if to_drop.is_empty() {
                break;
            }
        }
        for v in to_drop {
            candidates.remove(&v);
        }
    }
    if candidates.is_empty() {
        return candidates;
    }

    // Step 4: per-var cost/benefit weighing. Each candidate's reads
    // and writes are classified by the role the expression plays
    // in its containing statement. The weights below are tuned
    // against regression workloads — vars that net out positive promote,
    // others stay float.
    //
    // Big wins for promotion:
    //   * loop counter (`FOR I = ...`)         — saved FAC math/iter
    //   * loop bound (`TO X` / `STEP X`)       — saved per-iter convert
    //   * POKE / SYS / WAIT address operand    — direct LDA/STA
    //   * array index                          — saved FAC→u16 step
    //   * arithmetic operand (Add/Sub/Mul/Div)
    //
    // Costs that often dominate:
    //   * PRINT item                           — JSR GIVAYF + FOUT
    //   * Transcendental arg (SQR/LOG/SIN…)   — must convert to FAC
    //   * String/file ops (rare path)
    let mut classifier = UseClassifier {
        counts: HashMap::new(),
        promote: &candidates,
    };
    classifier.classify_module(module);
    let counts = classifier.counts;
    // Optional gate-trace: set INTPROMOTE_DEBUG to dump per-var
    // role counts and the kept/dropped decision. Intended for
    // tuning the weights below against new regression cases.
    if std::env::var("INTPROMOTE_DEBUG").is_ok() {
        let mut keys: Vec<&VarName> = candidates.iter().collect();
        keys.sort_by(|a, b| a.base.cmp(&b.base));
        for v in keys {
            let c = counts.get(v).copied().unwrap_or_default();
            eprintln!("[intpromote] {} kind={:?} counts={c:?}", v.base, v.kind);
        }
    }

    let mut family_prices: HashMap<&str, (u64, u64)> = HashMap::new();
    for v in &candidates {
        let c = counts.get(v).copied().unwrap_or_default();
        let family = split_family_base(v);
        let key = if bounded_families.contains(family) { family } else { &v.base };
        let price = family_prices.entry(key).or_default();
        price.0 += u64::from(c.for_var) * 8
            + u64::from(c.for_bound) * 4
            + u64::from(c.addr) * 4
            + u64::from(c.index) * 3
            + u64::from(c.arith) * 2
            + u64::from(c.cond)
            + u64::from(c.let_rhs);
        price.1 += u64::from(c.print) * 4
            + u64::from(c.transcend) * 6
            + u64::from(c.print_arg);
    }
    let drop_after_cost: Vec<VarName> = candidates
        .iter()
        .filter(|v| {
            // Per-role weights tuned against regression workloads +
            // bubble sort. `arith` and `cond` are net positive
            // when the var sits in a tight loop (each iteration
            // saves the FAC-load + FCOMP/FADD round-trip). The
            // weights below intentionally bias towards promotion
            // — vars used at all in arith/cond/index contexts
            // beat the cost gate even with a single PRINT site,
            // which gives counter-style flag/loop vars (bubble's
            // `s` / `m` / `x`) the int path they need to keep
            // the sort at 6s rather than the 10s float fallback.
            //
            // Programs that score net-negative are typically
            // text-adventure dispatcher vars (one PRINT or VAL
            // dominating), which are correctly left float.
            let family = split_family_base(v);
            let key = if bounded_families.contains(family) { family } else { &v.base };
            let (benefit, cost) = family_prices[key];
            benefit < cost
        })
        .cloned()
        .collect();
    if std::env::var("INTPROMOTE_DEBUG").is_ok() {
        for v in &drop_after_cost {
            eprintln!("[intpromote] DROP {} (cost-gate)", v.base);
        }
    }
    for v in drop_after_cost {
        candidates.remove(&v);
    }

    candidates
}

/// Per-var classification of how each read is consumed. Drives the
/// cost/benefit gate in `compute_int_promotable`.
#[derive(Default, Clone, Copy, Debug)]
struct UseCounts {
    /// Reads inside a numeric `PRINT` item that produce the
    /// printed value directly (`PRINT V`, `PRINT V+1`). Each
    /// promoted-int → FAC convert here costs ~10 cycles + 6 bytes
    /// before FOUT.
    print: u32,
    /// Argument inside a PRINT but in a FAC-needing sub-position
    /// (TAB/SPC/CharOut/etc.). Cheaper than `print` but still a
    /// per-use FAC convert.
    print_arg: u32,
    /// Argument of SQR/LOG/SIN/COS/TAN/ATN/EXP — float-only ROM
    /// routines that demand FAC.
    transcend: u32,
    /// Operand of a numeric BinOp (Add/Sub/Mul/Div). When both
    /// operands stay int the codegen reaches the int-island fast
    /// path; even when one side has to convert, the int half is
    /// still cheaper to load.
    arith: u32,
    /// Used as an array subscript. The address compute is u16, so
    /// int storage is a clean win — no FAC roundtrip.
    index: u32,
    /// Used as the address of POKE/SYS/WAIT/PEEK/DPEEK. The
    /// codegen lowers these directly to a u16 store/load with no
    /// FAC involvement.
    addr: u32,
    /// FOR loop variable. Promoting unlocks the integer-counter
    /// path which is by far the biggest single source of speedup
    /// on counted loops.
    for_var: u32,
    /// Read in a FOR start/end/step expression. Same int-counter
    /// motivation as `for_var`.
    for_bound: u32,
    /// Used in an IF/UNTIL/EXIT condition (bare or compared).
    /// Int-int compare avoids the FCOMP roundtrip.
    cond: u32,
    /// Right-hand side of a `LET v = ...` (other than the var
    /// being scored). Roughly neutral — same conversion cost
    /// either way — but a small benefit because a downstream
    /// reader likely benefits from the int form.
    let_rhs: u32,
}

#[derive(Clone, Copy)]
enum UseRole {
    Print,
    PrintArg,
    Transcend,
    Arith,
    Index,
    Addr,
    ForVar,
    ForBound,
    Cond,
    LetRhs,
    /// Neutral context — read happens but doesn't favour either
    /// representation (e.g. inside a no-op REM-equivalent). Don't
    /// score.
    Skip,
}

struct UseClassifier<'a> {
    counts: HashMap<VarName, UseCounts>,
    promote: &'a HashSet<VarName>,
}

impl<'a> UseClassifier<'a> {
    fn classify_module(&mut self, module: &ir::Module) {
        for line in &module.lines {
            for stmt in &line.stmts {
                self.classify_stmt(stmt);
            }
        }
    }

    fn note(&mut self, v: &VarName, role: UseRole) {
        // Only score reads that BELONG to a candidate — vars
        // already disqualified shouldn't drag the budget, and we
        // never need to store counts for them.
        if !self.promote.contains(v) {
            return;
        }
        let c = self.counts.entry(v.clone()).or_default();
        match role {
            UseRole::Print => c.print += 1,
            UseRole::PrintArg => c.print_arg += 1,
            UseRole::Transcend => c.transcend += 1,
            UseRole::Arith => c.arith += 1,
            UseRole::Index => c.index += 1,
            UseRole::Addr => c.addr += 1,
            UseRole::ForVar => c.for_var += 1,
            UseRole::ForBound => c.for_bound += 1,
            UseRole::Cond => c.cond += 1,
            UseRole::LetRhs => c.let_rhs += 1,
            UseRole::Skip => {}
        }
    }

    /// Walk an expression noting every Var/ArrayRef read with the
    /// given role. The role propagates through unary ops but
    /// switches when we descend into a transcendental call (the
    /// inner arg always pays the float-convert cost) or an
    /// arithmetic op (operands score as `Arith`).
    fn walk_expr(&mut self, e: &Expr, role: UseRole) {
        match e {
            Expr::Var(v) => self.note(v, role),
            Expr::ArrayRef(name, idx) => {
                // The array reference itself plays `role`; the
                // indices always pay the index path.
                self.note(name, role);
                for i in idx {
                    self.walk_expr(i, UseRole::Index);
                }
            }
            Expr::Number(_) | Expr::Inkey | Expr::Lin => {}
            Expr::String(_) => {}
            Expr::Neg(inner) | Expr::Not(inner) => self.walk_expr(inner, role),
            Expr::Bin(op, l, r) => {
                // Comparison BinOps (Eq/Ne/Lt/Le/Gt/Ge) feed an
                // IF/UNTIL test — score the operands as Cond so an
                // int-int compare is preferred. Arithmetic ops
                // score as Arith.
                let inner_role = match op {
                    BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                        UseRole::Cond
                    }
                    _ => UseRole::Arith,
                };
                self.walk_expr(l, inner_role);
                self.walk_expr(r, inner_role);
            }
            Expr::Func1(func, arg) => {
                let inner = match func {
                    Func1::Sqr
                    | Func1::Sin
                    | Func1::Cos
                    | Func1::Tan
                    | Func1::Atn
                    | Func1::Log
                    | Func1::Exp
                    | Func1::Rnd => UseRole::Transcend,
                    // ABS/INT/SGN keep the parent role — they're
                    // cheap and don't force a float-only path.
                    _ => role,
                };
                self.walk_expr(arg, inner);
            }
            Expr::Peek(addr) | Expr::MemPeek(addr) => self.walk_expr(addr, UseRole::Addr),
            Expr::Pos(arg) | Expr::Joy(arg) | Expr::Pot(arg) | Expr::Fre(arg) => {
                self.walk_expr(arg, UseRole::Arith);
            }
            Expr::Usr(arg) => self.walk_expr(arg, UseRole::Transcend),
            Expr::Asc(s) | Expr::Len(s) => self.walk_str(s, role),
            Expr::Val(s) | Expr::Nrm(s) => self.walk_str(s, UseRole::Transcend),
            Expr::StrCompare(_, l, r) => {
                self.walk_str(l, UseRole::Cond);
                self.walk_str(r, UseRole::Cond);
            }
            Expr::At(row, col) => {
                self.walk_expr(row, UseRole::Addr);
                self.walk_expr(col, UseRole::Addr);
            }
            Expr::Test(x, y) => {
                self.walk_expr(x, UseRole::Addr);
                self.walk_expr(y, UseRole::Addr);
            }
            Expr::Check { first, second } => {
                self.walk_expr(first, role);
                if let Some(s) = second {
                    self.walk_expr(s, role);
                }
            }
            Expr::Inst {
                haystack,
                needle,
                start,
            } => {
                self.walk_str(haystack, role);
                self.walk_str(needle, role);
                if let Some(s) = start {
                    self.walk_expr(s, UseRole::Arith);
                }
            }
            Expr::FnCall(_, arg) => self.walk_expr(arg, UseRole::Transcend),
        }
    }

    fn walk_str(&mut self, s: &StrExpr, role: UseRole) {
        match s {
            StrExpr::Literal(_) | StrExpr::Var(_) | StrExpr::GetKey => {}
            StrExpr::ArrayRef(name, idx) => {
                self.note(name, role);
                for i in idx {
                    self.walk_expr(i, UseRole::Index);
                }
            }
            StrExpr::Concat(l, r) => {
                self.walk_str(l, role);
                self.walk_str(r, role);
            }
            StrExpr::Left(s, n) | StrExpr::Right(s, n) | StrExpr::Dup(s, n) => {
                self.walk_str(s, role);
                self.walk_expr(n, UseRole::Arith);
            }
            StrExpr::Mid(s, start, len) => {
                self.walk_str(s, role);
                self.walk_expr(start, UseRole::Arith);
                if let Some(l) = len {
                    self.walk_expr(l, UseRole::Arith);
                }
            }
            StrExpr::Insert(haystack, frag, at) => {
                self.walk_str(haystack, role);
                self.walk_str(frag, role);
                self.walk_expr(at, UseRole::Arith);
            }
            StrExpr::Chr(arg) => self.walk_expr(arg, UseRole::PrintArg),
            StrExpr::Str(arg) => self.walk_expr(arg, UseRole::Print),
            StrExpr::HexFmt(arg) | StrExpr::BinFmt(arg) => self.walk_expr(arg, UseRole::Arith),
        }
    }

    /// Top-level dispatch: each statement kind has its own
    /// "shape" that decides which role each operand plays. We
    /// also handle nested IF bodies recursively. Anything not
    /// listed scores nothing — those statements either reference
    /// no candidate vars or are already disqualified.
    fn classify_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Let { value, .. } => self.walk_expr(value, UseRole::LetRhs),
            Stmt::LetStr { .. } => {}
            Stmt::ArrayLet {
                name,
                indices,
                value,
            } => {
                self.note(name, UseRole::LetRhs);
                for i in indices {
                    self.walk_expr(i, UseRole::Index);
                }
                self.walk_expr(value, UseRole::LetRhs);
            }
            Stmt::ArrayLetStr { name, indices, .. } => {
                self.note(name, UseRole::LetRhs);
                for i in indices {
                    self.walk_expr(i, UseRole::Index);
                }
            }
            Stmt::For {
                var,
                start,
                end,
                step,
                ..
            } => {
                self.note(var, UseRole::ForVar);
                self.walk_expr(start, UseRole::ForBound);
                self.walk_expr(end, UseRole::ForBound);
                self.walk_expr(step, UseRole::ForBound);
            }
            Stmt::Next { vars } => {
                for v in vars.iter().flatten() {
                    self.note(v, UseRole::ForVar);
                }
            }
            Stmt::Print { items, .. } => self.walk_print_items(items),
            Stmt::PrintFile {
                file_num, items, ..
            } => {
                self.walk_expr(file_num, UseRole::Addr);
                self.walk_print_items(items);
            }
            Stmt::Cmd {
                file_num, items, ..
            } => {
                self.walk_expr(file_num, UseRole::Addr);
                self.walk_print_items(items);
            }
            Stmt::Poke { addr, value } | Stmt::Dpoke { addr, value } => {
                self.walk_expr(addr, UseRole::Addr);
                self.walk_expr(value, UseRole::Arith);
            }
            Stmt::If { cond, then } => {
                self.walk_expr(cond, UseRole::Cond);
                if let crate::ir::ThenIr::Stmts(inner) = then {
                    for s in inner {
                        self.classify_stmt(s);
                    }
                }
            }
            Stmt::IfElse {
                cond,
                then,
                else_then,
            } => {
                self.walk_expr(cond, UseRole::Cond);
                for branch in [then, else_then] {
                    if let crate::ir::ThenIr::Stmts(inner) = branch {
                        for s in inner {
                            self.classify_stmt(s);
                        }
                    }
                }
            }
            Stmt::Rcomp { then, else_then } => {
                if let crate::ir::ThenIr::Stmts(inner) = then {
                    for s in inner {
                        self.classify_stmt(s);
                    }
                }
                if let Some(crate::ir::ThenIr::Stmts(inner)) = else_then {
                    for s in inner {
                        self.classify_stmt(s);
                    }
                }
            }
            Stmt::DoIf { cond } | Stmt::Until { cond } => {
                self.walk_expr(cond, UseRole::Cond);
            }
            Stmt::ExitLoop { cond } => {
                if let Some(c) = cond {
                    self.walk_expr(c, UseRole::Cond);
                }
            }
            Stmt::ComputedGoto { target } => self.walk_expr(target, UseRole::Addr),
            Stmt::Reset { target } => self.walk_expr(target, UseRole::Addr),
            Stmt::OnBranch { value, .. } => self.walk_expr(value, UseRole::Cond),
            Stmt::Sys {
                addr,
                regs,
                param_vars,
                ..
            } => {
                self.walk_expr(addr, UseRole::Addr);
                for r in regs {
                    self.walk_expr(r, UseRole::Addr);
                }
                // FOUT wants the value in FAC, so this is an arithmetic use, not an address one.
                for p in param_vars {
                    self.walk_expr(&p.value, UseRole::Arith);
                }
            }
            Stmt::Wait { addr, mask, eor } => {
                self.walk_expr(addr, UseRole::Addr);
                self.walk_expr(mask, UseRole::Arith);
                if let Some(e) = eor {
                    self.walk_expr(e, UseRole::Arith);
                }
            }
            // Everything else: don't score. Most extensions
            // already disqualify their target var via
            // WriteCollector::bad, and the remaining read sites
            // are too rare to influence the budget meaningfully.
            _ => {}
        }
    }

    fn walk_print_items(&mut self, items: &[crate::ir::PrintPiece]) {
        for item in items {
            match item {
                crate::ir::PrintPiece::Expr(e) => self.walk_expr(e, UseRole::Print),
                crate::ir::PrintPiece::CharOut(e)
                | crate::ir::PrintPiece::TabTo(e)
                | crate::ir::PrintPiece::Spc(e) => self.walk_expr(e, UseRole::PrintArg),
                crate::ir::PrintPiece::StrExpr(s) => self.walk_str(s, UseRole::Skip),
                _ => {}
            }
        }
    }
}

/// Visitor that records every structured write to a scalar var
/// (`LET v = expr`, `FOR v = start TO end STEP step`) plus every
/// "bad" write (INPUT/READ/GET/DEF-FN-param) that disqualifies the
/// var from promotion.
struct WriteCollector {
    defs: HashMap<VarName, Vec<Expr>>,
    bad: HashSet<VarName>,
    /// Names referenced as scalars (Var/Let). When the same name is
    /// also referenced as an array, we can't promote either side
    /// because the IR shares a single VarName.
    scalar_used: HashSet<VarName>,
    array_used: HashSet<VarName>,
}

impl WriteCollector {
    fn note_scalar(&mut self, v: &VarName) {
        self.scalar_used.insert(v.clone());
    }
    fn note_array(&mut self, v: &VarName) {
        self.array_used.insert(v.clone());
    }
}

impl crate::visit::Visitor for WriteCollector {
    fn visit_var_read(&mut self, v: &VarName) {
        self.note_scalar(v);
    }

    fn visit_expr(&mut self, e: &Expr) {
        if let Expr::ArrayRef(name, _) = e {
            self.note_array(name);
        }
        crate::visit::walk_expr(self, e);
    }

    fn visit_str_expr(&mut self, s: &StrExpr) {
        if let StrExpr::ArrayRef(name, _) = s {
            self.note_array(name);
        }
        crate::visit::walk_str_expr(self, s);
    }

    fn visit_stmt(&mut self, line_no: u16, stmt: &Stmt) {
        match stmt {
            Stmt::Let { var, value } => {
                self.note_scalar(var);
                self.defs
                    .entry(var.clone())
                    .or_default()
                    .push(value.clone());
            }
            Stmt::LetStr { var, .. } => {
                // LetStr stores a string pointer — never int-stayable.
                self.note_scalar(var);
                self.bad.insert(var.clone());
            }
            // BASIC v2 puts scalars and arrays in the same name
            // namespace (`A` and `A()` share `VarName`). We track
            // array writes in the same map: if any array element
            // assignment isn't int-stayable, the whole var is bad.
            Stmt::ArrayLet { name, value, .. } => {
                self.note_array(name);
                self.defs
                    .entry(name.clone())
                    .or_default()
                    .push(value.clone());
            }
            Stmt::ArrayLetStr { name, .. } => {
                self.note_array(name);
                self.bad.insert(name.clone());
            }
            Stmt::For {
                var,
                start,
                end,
                step,
                ..
            } => {
                self.note_scalar(var);
                let entry = self.defs.entry(var.clone()).or_default();
                // start is the actual stored value; end/step are read
                // every iteration in the int-FOR exit test, so they
                // also need to fit i16 for promotion to be sound.
                entry.push(start.clone());
                entry.push(end.clone());
                entry.push(step.clone());
            }

            Stmt::Next { vars } => {
                for v in vars.iter().flatten() {
                    self.note_scalar(v);
                }
            }
            Stmt::Get { var, indices } => {
                if indices.is_empty() {
                    self.note_scalar(var);
                } else {
                    self.note_array(var);
                }
                self.bad.insert(var.clone());
            }
            Stmt::KeyGet { var } => {
                self.note_scalar(var);
                self.bad.insert(var.clone());
            }
            Stmt::GetFile { vars, .. } => {
                for v in vars {
                    self.note_scalar(v);
                    self.bad.insert(v.clone());
                }
            }
            Stmt::Read(targets) | Stmt::Input { targets, .. } => {
                for t in targets {
                    match t {
                        crate::ir::ReadTarget::Scalar(v) => {
                            self.note_scalar(v);
                            self.bad.insert(v.clone());
                        }
                        crate::ir::ReadTarget::Array { name, .. } => {
                            self.note_array(name);
                            self.bad.insert(name.clone());
                        }
                    }
                }
            }
            Stmt::InputFile { targets, .. } => {
                for t in targets {
                    match t {
                        crate::ir::ReadTarget::Scalar(v) => {
                            self.note_scalar(v);
                            self.bad.insert(v.clone());
                        }
                        crate::ir::ReadTarget::Array { name, .. } => {
                            self.note_array(name);
                            self.bad.insert(name.clone());
                        }
                    }
                }
            }
            Stmt::DefFn { param, .. } => {
                self.note_scalar(param);
                self.bad.insert(param.clone());
            }
            Stmt::Dim(specs) => {
                for spec in specs {
                    self.note_array(&spec.name);
                }
            }

            Stmt::Fetch {
                target,
                target_indices,
                ..
            } => {
                if target_indices.is_empty() {
                    self.note_scalar(target);
                } else {
                    self.note_array(target);
                }
                self.bad.insert(target.clone());
            }
            _ => {}
        }
        crate::visit::walk_stmt(self, line_no, stmt);
    }
}

/// Prevent promotion from merging distinct float and integer variables or arrays.
fn disqualify_percent_twins(
    scalar_used: &HashSet<VarName>,
    array_used: &HashSet<VarName>,
    bad: &mut HashSet<VarName>,
) {
    for used in [scalar_used, array_used] {
        let int_bases: HashSet<&str> = used
            .iter()
            .filter(|v| v.kind == VarKind::Integer)
            .map(|v| v.base.as_str())
            .collect();
        if int_bases.is_empty() {
            continue;
        }
        for v in used.iter().filter(|v| v.kind == VarKind::Float) {
            if int_bases.contains(v.base.as_str()) {
                bad.insert(v.clone());
            }
        }
    }
}

/// Interval bounds for the range half of the promotion gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PromRange {
    min: i64,
    max: i64,
}

impl PromRange {
    fn new(min: i64, max: i64) -> Self {
        Self { min, max }
    }
    fn i16() -> Self {
        Self::new(i16::MIN as i64, i16::MAX as i64)
    }
    fn u8() -> Self {
        Self::new(0, 255)
    }
    fn join(self, other: Self) -> Self {
        Self::new(self.min.min(other.min), self.max.max(other.max))
    }
    fn contains(self, other: Self) -> bool {
        self.min <= other.min && self.max >= other.max
    }
    fn fits_i16(self) -> bool {
        self.min >= i16::MIN as i64 && self.max <= i16::MAX as i64
    }
}

/// Maximum interval-propagation rounds before treating a changing range as unbounded.
const PROM_RANGE_ROUNDS: usize = 24;

fn split_family_base(var: &VarName) -> &str {
    var.base.rsplit_once("__")
        .filter(|(base, suffix)| !base.is_empty() && !suffix.is_empty()
            && suffix.bytes().all(|b| b.is_ascii_digit()))
        .map_or(var.base.as_str(), |(base, _)| base)
}

/// True if the program has a cycle that is not a `FOR`/`NEXT` pair.
/// Those can run forever, so an unproved accumulator is not safe to promote.
/// Orphan NEXT, oversized CFG, or a NEXT that misses its header: true.
fn module_has_uncounted_cycle(module: &ir::Module) -> bool {
    let orphans = crate::for_next_runtime::analyze(module);
    if orphans.needs_runtime_stack() {
        return true;
    }
    let cfg = crate::cfg::Cfg::build_with(module, &orphans);
    if cfg.nodes.len() > REGION_NODE_LIMIT {
        return true;
    }
    let mut stack: Vec<(VarName, usize)> = Vec::new();
    let mut back_edges: HashSet<(usize, usize)> = HashSet::new();
    for (id, node) in cfg.nodes.iter().enumerate() {
        match cfg.stmt_at(id, module) {
            Stmt::For { var, .. } => {
                if node.stmt.path.len() != 1 || node.successors.as_slice() != [id + 1] {
                    return true;
                }
                stack.push((var.clone(), id));
            }
            Stmt::Next { vars } => {
                if node.stmt.path.len() != 1 || vars.len() != 1 {
                    return true;
                }
                let Some((var, header)) = stack.pop() else {
                    return true;
                };
                if vars[0].as_ref().is_some_and(|v| v != &var)
                    || !node.successors.contains(&(header + 1))
                {
                    return true;
                }
                back_edges.insert((id, header + 1));
            }
            _ => {}
        }
    }
    if !stack.is_empty() {
        return true;
    }
    cfg.nodes.iter().enumerate().any(|(id, node)| {
        node.successors
            .iter()
            .any(|&to| to <= id && !back_edges.contains(&(id, to)))
    })
}

fn bounded_accumulator_ranges(module: &ir::Module) -> HashMap<VarName, PromRange> {
    fn prove(module: &ir::Module) -> Option<HashMap<VarName, PromRange>> {
        struct Safe(bool);
        impl crate::visit::Visitor for Safe {
            fn visit_stmt(&mut self, line: u16, stmt: &Stmt) {
                // Explicit allow-list: new control-flow or machine-code features do not silently gain an execution-count proof.
                self.0 &= matches!(stmt,
                    Stmt::Let { .. } | Stmt::LetStr { .. }
                    | Stmt::ArrayLet { .. } | Stmt::ArrayLetStr { .. }
                    | Stmt::Print { .. } | Stmt::Poke { .. } | Stmt::Dpoke { .. }
                    | Stmt::For { .. } | Stmt::Next { .. }
                    | Stmt::If { .. } | Stmt::IfElse { .. } | Stmt::Goto { .. }
                    | Stmt::Read(_) | Stmt::Data(_) | Stmt::Restore | Stmt::Dim(_)
                    | Stmt::Rem(_) | Stmt::End | Stmt::Stop | Stmt::LineMark(_));
                if self.0 {
                    crate::visit::walk_stmt(self, line, stmt);
                }
            }
            fn visit_expr(&mut self, expr: &Expr) {
                self.0 &= !matches!(expr, Expr::FnCall(..) | Expr::Usr(_));
                if self.0 {
                    crate::visit::walk_expr(self, expr);
                }
            }
        }
        let mut safe = Safe(true);
        crate::visit::walk_module(&mut safe, module);
        if !safe.0 { return None; }
        let orphans = crate::for_next_runtime::analyze(module);
        if orphans.needs_runtime_stack() { return None; }
        let cfg = crate::cfg::Cfg::build_with(module, &orphans);
        if cfg.nodes.len() > REGION_NODE_LIMIT { return None; }

        fn literal(expr: &Expr) -> Option<i64> {
            let value = match expr {
                Expr::Number(n) => *n,
                Expr::Neg(inner) => -literal(inner)? as f64,
                _ => return None,
            };
            (value.is_finite() && value.fract() == 0.0
                && (-32768.0..=32767.0).contains(&value)).then_some(value as i64)
        }
        let mut stack: Vec<(VarName, usize, i64)> = Vec::new();
        let mut multiplier = 1i64;
        let mut counts = vec![1i64; cfg.nodes.len()];
        let mut back_edges = HashSet::new();
        for (id, node) in cfg.nodes.iter().enumerate() {
            let stmt = cfg.stmt_at(id, module);
            counts[id] = multiplier;
            match stmt {
                Stmt::For { var, start, end, step, .. } => {
                    if node.stmt.path.len() != 1
                        || stack.iter().any(|(active, _, _)| active == var)
                        || node.successors.as_slice() != [id + 1]
                    { return None; }
                    let (start, end, step) = (literal(start)?, literal(end)?, literal(step)?);
                    if step == 0 { return None; }
                    // BASIC's FOR is do-while: even reversed bounds run once.
                    let distance = if step > 0 { end - start } else { start - end };
                    let trips = if distance < 0 { 1 } else { distance / step.abs() + 1 };
                    let exit = start.checked_add(trips.checked_mul(step)?)?;
                    if !(i16::MIN as i64..=i16::MAX as i64).contains(&exit) { return None; }
                    stack.push((var.clone(), id, multiplier));
                    multiplier = multiplier.checked_mul(trips)?;
                }
                Stmt::Next { vars } => {
                    if node.stmt.path.len() != 1 || vars.len() != 1 { return None; }
                    let (var, header, previous) = stack.pop()?;
                    if vars[0].as_ref().is_some_and(|v| v != &var)
                        || !node.successors.contains(&(header + 1))
                    { return None; }
                    back_edges.insert((id, header + 1));
                    multiplier = previous;
                }
                _ => {
                    // A body assignment can extend a nominally bounded FOR forever.
                    if stack.iter().any(|(v, _, _)| stmt_writes_var(stmt, v)) {
                        return None;
                    }
                }
            }
        }
        if !stack.is_empty() { return None; }
        for (id, node) in cfg.nodes.iter().enumerate() {
            if node.successors.iter().any(|&to| to <= id && !back_edges.contains(&(id, to))) {
                return None;
            }
        }

        #[derive(Default)]
        struct Excursion {
            names: HashSet<VarName>,
            init_min: i64,
            init_max: i64,
            negative: i64,
            positive: i64,
            stepped: bool,
            bad: bool,
        }
        let family_key = |v: &VarName| VarName {
            base: split_family_base(v).to_owned(), kind: v.kind,
        };
        let mut families: HashMap<VarName, Excursion> = HashMap::new();
        for (id, _) in cfg.nodes.iter().enumerate() {
            let stmt = cfg.stmt_at(id, module);
            let Stmt::Let { var, value } = stmt else {
                for v in stmt_counter_defs(stmt) {
                    families.entry(family_key(&v)).or_default().bad = true;
                }
                continue;
            };
            if var.kind == VarKind::String { continue; }
            let family = split_family_base(var);
            let entry = families.entry(family_key(var)).or_default();
            entry.names.insert(var.clone());
            if let Some(init) = literal(value) {
                entry.init_min = entry.init_min.min(init);
                entry.init_max = entry.init_max.max(init);
                continue;
            }
            let same_family = |expr: &Expr| matches!(expr,
                Expr::Var(v) if v.kind == var.kind && split_family_base(v) == family);
            let step = match value {
                Expr::Bin(BinOp::Add, l, r) if same_family(l) => literal(r),
                Expr::Bin(BinOp::Add, l, r) if same_family(r) => literal(l),
                Expr::Bin(BinOp::Sub, l, r) if same_family(l) => literal(r).map(|n| -n),
                _ => None,
            };
            if let Some(step) = step {
                let delta = step.checked_mul(counts[id])?;
                entry.negative = entry.negative.checked_add(delta.min(0))?;
                entry.positive = entry.positive.checked_add(delta.max(0))?;
                entry.stepped = true;
            } else {
                entry.bad = true;
            }
        }
        let mut out = HashMap::new();
        for (_, family) in families {
            if family.bad || !family.stepped { continue; }
            let range = PromRange::new(
                family.init_min.checked_add(family.negative)?,
                family.init_max.checked_add(family.positive)?,
            );
            for name in family.names { out.insert(name, range); }
        }
        Some(out)
    }
    prove(module).unwrap_or_default()
}

/// Recheck the final IR, after promotion and the loop transforms.
pub(crate) fn bounded_integer_accumulators(module: &ir::Module) -> HashSet<VarName> {
    bounded_accumulator_ranges(module).into_iter()
        .filter(|(v, range)| v.kind == VarKind::Integer && range.fits_i16())
        .map(|(v, _)| v)
        .collect()
}

#[cfg(test)]
#[path = "bounded_accumulator_tests.rs"]
mod bounded_accumulator_tests;

/// Every variable that some `FOR` header controls.
fn collect_for_counter_vars(module: &ir::Module) -> HashSet<VarName> {
    use crate::visit::Visitor;
    struct Collect {
        out: HashSet<VarName>,
    }
    impl Visitor for Collect {
        fn visit_stmt(&mut self, line_no: u16, stmt: &Stmt) {
            if let Stmt::For { var, .. } = stmt {
                self.out.insert(var.clone());
            }
            crate::visit::walk_stmt(self, line_no, stmt);
        }
    }
    let mut c = Collect {
        out: HashSet::new(),
    };
    c.visit_module(module);
    c.out
}

/// Candidates whose value cannot be proved to stay inside i16.
fn int_range_escapees(
    defs: &HashMap<VarName, Vec<Expr>>,
    candidates: &HashSet<VarName>,
    for_counters: &HashSet<VarName>,
    guarded: &HashMap<VarName, PromRange>,
    // `guarded` entries whose bound came from execution count, not a loop-exit
    // test. Only the count proof bounds the value at the store.
    counted: &HashSet<VarName>,
    // True when a cycle is not a FOR/NEXT pair, so an accumulator can run forever.
    uncounted_cycle: bool,
) -> Vec<VarName> {
    let mut ranges: HashMap<VarName, PromRange> = HashMap::new();
    let mut unstable: HashSet<VarName> = HashSet::new();
    for round in 0..=PROM_RANGE_ROUNDS {
        let mut changed = false;
        // Deterministic order: the fixpoint result does not depend on it, but the escapee list
        // handed back does.
        let mut names: Vec<&VarName> = candidates.iter().collect();
        names.sort_by_key(|v| prom_sort_key(v));
        for v in names {
            let Some(values) = defs.get(v) else { continue };
            let mut acc: Option<PromRange> = None;
            for value in values {
                let Some(r) = prom_range_of(value, candidates, &ranges) else {
                    continue;
                };
                acc = Some(match acc {
                    Some(a) => a.join(r),
                    None => r,
                });
            }
            let Some(next) = acc else { continue };
            match ranges.get(v) {
                Some(&old) if old.contains(next) => {}
                Some(&old) => {
                    ranges.insert(v.clone(), old.join(next));
                    changed = true;
                    if round == PROM_RANGE_ROUNDS
                        && !for_counters.contains(v)
                        && !guarded.contains_key(v)
                    {
                        unstable.insert(v.clone());
                    }
                }
                None => {
                    ranges.insert(v.clone(), next);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    // A def that mentions itself. Needs a proven bound before promotion.
    let accumulator = |v: &VarName| {
        defs.get(v)
            .is_some_and(|values| values.iter().any(|e| expr_reads_var(e, v)))
    };
    let mut out: Vec<VarName> = candidates
        .iter()
        .filter(|v| {

            // Range still widening after the last fixpoint round. Check this
            // first so an accumulator that never settled is still dropped.
            if unstable.contains(*v) {
                return true;
            }
            if uncounted_cycle && accumulator(v) {
                // A loop-exit test bounds the value at the test, not the store.
                // Keep only if the execution-count proof or the fixpoint covers it.
                return !(counted.contains(*v)
                    || ranges.get(*v).is_some_and(|r| r.fits_i16()));
            }
            if let Some(b) = guarded.get(*v) {
                return !b.fits_i16();
            }
            ranges.get(*v).is_some_and(|r| !r.fits_i16())
        })
        .cloned()
        .collect();
    out.sort_by_key(prom_sort_key);
    // Gate-trace.
    if std::env::var("INTPROMOTE_DEBUG").is_ok() {
        for v in &out {
            let why = if unstable.contains(v) {
                format!(
                    "unstable after {PROM_RANGE_ROUNDS} rounds (range still widening: \
                     {:?}) -- unguarded self-recurrence",
                    ranges.get(v)
                )
            } else {
                format!("range {:?} leaves i16", ranges.get(v))
            };
            eprintln!("[intpromote] DROP {} (range-gate: {why})", v.base);
        }
    }
    out
}

const GUARD_WALK_LIMIT: usize = 8;
/// Cap on the loop-region BFS, so a pathological CFG cannot make this analysis quadratic.
const REGION_NODE_LIMIT: usize = 4096;
/// Rounds of the bound fixpoint.
const BOUND_ROUNDS: usize = 8;

/// What a single write to a candidate does to it.
enum CounterWrite {

    Step { node: usize, k: i64 },
    /// A write that does not read V, with a computable interval.
    Init(PromRange),
}

fn guarded_counter_bounds(module: &ir::Module) -> HashMap<VarName, PromRange> {
    let cfg = crate::cfg::Cfg::build(module);

    let mut writes: HashMap<VarName, Vec<CounterWrite>> = HashMap::new();
    let mut bad: HashSet<VarName> = HashSet::new();
    let mut init_nodes: HashMap<VarName, HashSet<usize>> = HashMap::new();
    for id in 0..cfg.nodes.len() {
        let stmt = cfg.stmt_at(id, module);
        if let Stmt::Let { var, value } = stmt {
            if var.kind != VarKind::Float {
                continue;
            }
            match classify_counter_write(var, value) {
                Some(CounterWrite::Step { k, .. }) => {
                    writes
                        .entry(var.clone())
                        .or_default()
                        .push(CounterWrite::Step { node: id, k });
                }
                Some(CounterWrite::Init(r)) => {
                    writes.entry(var.clone()).or_default().push(CounterWrite::Init(r));
                    init_nodes.entry(var.clone()).or_default().insert(id);
                }
                None => {
                    bad.insert(var.clone());
                }
            }
            continue;
        }
        for v in stmt_counter_defs(stmt) {
            bad.insert(v);
        }
    }

    let mut proven: HashMap<VarName, PromRange> = HashMap::new();
    for _ in 0..BOUND_ROUNDS {
        let mut changed = false;
        let mut names: Vec<&VarName> = writes.keys().filter(|v| !bad.contains(*v)).collect();
        names.sort_by_key(|v| prom_sort_key(v));
        for v in names {
            if proven.contains_key(v) {
                continue;
            }
            if let Some(r) = prove_counter_bound(module, &cfg, v, &writes[v], &init_nodes, &proven)
            {
                proven.insert(v.clone(), r);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    proven
}

/// Variables a non-LET statement writes, for the disqualification scan.
fn stmt_counter_defs(stmt: &Stmt) -> Vec<VarName> {
    let mut out = Vec::new();
    match stmt {
        Stmt::For { var, .. } => out.push(var.clone()),
        Stmt::Get { indices, .. } if !indices.is_empty() => {}
        Stmt::Get { var, .. } | Stmt::KeyGet { var } => out.push(var.clone()),
        Stmt::GetFile { vars, .. } => out.extend(vars.iter().cloned()),
        Stmt::Fetch { target, .. } => out.push(target.clone()),
        Stmt::Read(targets) | Stmt::Input { targets, .. } | Stmt::InputFile { targets, .. } => {
            for t in targets {
                if let crate::ir::ReadTarget::Scalar(v) = t {
                    out.push(v.clone());
                }
            }
        }
        _ => {}
    }
    out
}

fn classify_counter_write(var: &VarName, value: &Expr) -> Option<CounterWrite> {
    let as_step = |l: &Expr, r: &Expr, sign: i64| -> Option<i64> {
        let is_self = matches!(l, Expr::Var(v) if v == var);
        if !is_self {
            return None;
        }
        let Expr::Number(n) = r else { return None };
        if !n.is_finite() || n.fract() != 0.0 || *n == 0.0 || n.abs() > 1024.0 {
            return None;
        }
        Some(sign * (*n as i64))
    };
    match value {
        Expr::Bin(BinOp::Add, l, r) => {
            if let Some(k) = as_step(l, r, 1).or_else(|| as_step(r, l, 1)) {
                return Some(CounterWrite::Step { node: 0, k });
            }
        }
        Expr::Bin(BinOp::Sub, l, r) => {
            if let Some(k) = as_step(l, r, -1) {
                return Some(CounterWrite::Step { node: 0, k });
            }
        }
        _ => {}
    }
    if expr_reads_var_named(value, var) {

        return None;
    }
    let r = prom_range_of(value, &HashSet::new(), &HashMap::new())?;
    Some(CounterWrite::Init(r))
}

fn expr_reads_var_named(e: &Expr, var: &VarName) -> bool {
    struct Look<'a> {
        var: &'a VarName,
        found: bool,
    }
    impl crate::visit::Visitor for Look<'_> {
        fn visit_var_read(&mut self, v: &VarName) {
            if v == self.var {
                self.found = true;
            }
        }
    }
    let mut l = Look { var, found: false };
    crate::visit::Visitor::visit_expr(&mut l, e);
    l.found
}

/// Prove a bound for one candidate, or return `None`.
fn prove_counter_bound(
    module: &ir::Module,
    cfg: &crate::cfg::Cfg,
    var: &VarName,
    writes: &[CounterWrite],
    init_nodes: &HashMap<VarName, HashSet<usize>>,
    proven: &HashMap<VarName, PromRange>,
) -> Option<PromRange> {
    let mut init: Option<PromRange> = None;
    let mut steps: Vec<(usize, i64)> = Vec::new();
    for w in writes {
        match w {
            CounterWrite::Step { node, k } => steps.push((*node, *k)),
            CounterWrite::Init(r) => {
                init = Some(match init {
                    Some(a) => a.join(*r),
                    None => *r,
                })
            }
        }
    }
    if steps.is_empty() {
        return None;
    }
    // A BASIC scalar reads as 0 before its first assignment, so an un-initialised counter still has a known starting value.
    let init = init.unwrap_or_else(|| PromRange::new(0, 0));

    let dir = steps[0].1.signum();
    if steps.iter().any(|(_, k)| k.signum() != dir) {
        return None;
    }
    let kmax = steps.iter().map(|(_, k)| k.abs()).max().unwrap_or(1);
    let empty = HashSet::new();
    let mut acc: Option<PromRange> = None;
    for &(node, k) in &steps {
        let (cont_op, b_expr) = guard_for_step(module, cfg, var, node, init_nodes)?;
        let b = prom_range_of(b_expr, &empty, proven)?;
        let bound = if matches!(cont_op, BinOp::Ne) {
            // Equality exit.
            if k.abs() != 1 {
                return None;
            }
            if dir > 0 {
                if init.max > b.min {
                    return None;
                }
                PromRange::new(init.min, b.max)
            } else {
                if init.min < b.max {
                    return None;
                }
                PromRange::new(b.min, init.max)
            }
        } else {
            // Inequality exit.
            match (dir > 0, cont_op) {
                (true, BinOp::Lt | BinOp::Le) => {
                    PromRange::new(init.min, b.max.saturating_add(kmax))
                }
                (false, BinOp::Gt | BinOp::Ge) => {
                    PromRange::new(b.min.saturating_sub(kmax), init.max)
                }
                _ => return None,
            }
        };
        acc = Some(match acc {
            Some(a) => a.join(bound),
            None => bound,
        });
    }
    Some(acc?.join(init))
}

fn guard_for_step<'m>(
    module: &'m ir::Module,
    cfg: &crate::cfg::Cfg,
    var: &VarName,
    step_node: usize,
    init_nodes: &HashMap<VarName, HashSet<usize>>,
) -> Option<(BinOp, &'m Expr)> {
    // Walk single-successor edges to the guard, checking on the way that nothing rewrites the counter before the test sees it.
    let mut cur = step_node;
    let mut hops = 0;
    let guard_id = loop {
        let succ = &cfg.nodes[cur].successors;
        if succ.len() != 1 {
            return None;
        }
        cur = succ[0];
        hops += 1;
        if hops > GUARD_WALK_LIMIT {
            return None;
        }
        if matches!(cfg.stmt_at(cur, module), Stmt::If { .. }) && cfg.nodes[cur].successors.len() == 2
        {
            break cur;
        }
        // Any statement between the step and the guard that writes the counter breaks the induction.
        if stmt_writes_var(cfg.stmt_at(cur, module), var) {
            return None;
        }
    };
    let Stmt::If { cond, then, .. } = cfg.stmt_at(guard_id, module) else {
        return None;
    };
    // Only the `IF <cmp> THEN <line>` form.
    let crate::ir::ThenIr::Goto(target) = then else {
        return None;
    };
    let Expr::Bin(op, l, r) = cond else {
        return None;
    };
    // Normalise to `V <op2> B`.
    let (op2, b_expr): (BinOp, &Expr) = if matches!(l.as_ref(), Expr::Var(v) if v == var) {
        (*op, r.as_ref())
    } else if matches!(r.as_ref(), Expr::Var(v) if v == var) {
        (mirror_relop(*op)?, l.as_ref())
    } else {
        return None;
    };
    if !matches!(
        op2,
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
    ) {
        return None;
    }
    // Identify the true edge by its target line.
    let succs = &cfg.nodes[guard_id].successors;
    let on_target: Vec<usize> = succs
        .iter()
        .copied()
        .filter(|&s| cfg.nodes[s].line_no == *target)
        .collect();
    if on_target.len() != 1 {
        return None;
    }
    let true_edge = on_target[0];
    let false_edge = *succs.iter().find(|&&s| s != true_edge)?;

    // Which edge continues the recurrence?
    let empty = HashSet::new();
    let inits = init_nodes.get(var).unwrap_or(&empty);
    let t_reaches = region_reaches(cfg, true_edge, guard_id, inits, step_node);
    let f_reaches = region_reaches(cfg, false_edge, guard_id, inits, step_node);
    let (cont_edge, cont_is_true) = match (t_reaches, f_reaches) {
        (Some(true), Some(false)) => (true_edge, true),
        (Some(false), Some(true)) => (false_edge, false),
        // Both continue: the guard does not exit the recurrence.
        _ => return None,
    };
    // The bound has to hold still while the loop runs.
    let b_vars = expr_var_set(b_expr);
    if !b_vars.is_empty()
        && region_writes_any(cfg, module, cont_edge, guard_id, inits, &b_vars)
    {
        return None;
    }
    let cont_op = if cont_is_true { op2 } else { negate_relop(op2) };
    Some((cont_op, b_expr))
}

/// `a op b` rewritten as `b op' a`.
fn mirror_relop(op: BinOp) -> Option<BinOp> {
    Some(match op {
        BinOp::Eq => BinOp::Eq,
        BinOp::Ne => BinOp::Ne,
        BinOp::Lt => BinOp::Gt,
        BinOp::Le => BinOp::Ge,
        BinOp::Gt => BinOp::Lt,
        BinOp::Ge => BinOp::Le,
        _ => return None,
    })
}

fn negate_relop(op: BinOp) -> BinOp {
    match op {
        BinOp::Eq => BinOp::Ne,
        BinOp::Ne => BinOp::Eq,
        BinOp::Lt => BinOp::Ge,
        BinOp::Le => BinOp::Gt,
        BinOp::Gt => BinOp::Le,
        BinOp::Ge => BinOp::Lt,
        other => other,
    }
}

/// Can `start` reach `target` without passing through `stop` or any node in `skip`?
fn region_reaches(
    cfg: &crate::cfg::Cfg,
    start: usize,
    stop: usize,
    skip: &HashSet<usize>,
    target: usize,
) -> Option<bool> {
    let mut seen: HashSet<usize> = HashSet::new();
    let mut work = vec![start];
    while let Some(n) = work.pop() {
        if n == stop || skip.contains(&n) {
            continue;
        }
        if !seen.insert(n) {
            continue;
        }
        if seen.len() > REGION_NODE_LIMIT {
            return None;
        }
        if n == target {
            return Some(true);
        }
        work.extend(cfg.nodes[n].successors.iter().copied());
    }
    Some(false)
}

fn region_writes_any(
    cfg: &crate::cfg::Cfg,
    module: &ir::Module,
    start: usize,
    stop: usize,
    skip: &HashSet<usize>,
    vars: &HashSet<VarName>,
) -> bool {
    let mut seen: HashSet<usize> = HashSet::new();
    let mut work = vec![start];
    while let Some(n) = work.pop() {
        if n == stop || skip.contains(&n) {
            continue;
        }
        if !seen.insert(n) {
            continue;
        }
        if seen.len() > REGION_NODE_LIMIT {
            // Could not finish the scan ; assume the worst.
            return true;
        }
        let stmt = cfg.stmt_at(n, module);
        if vars.iter().any(|v| stmt_writes_var(stmt, v)) {
            return true;
        }
        work.extend(cfg.nodes[n].successors.iter().copied());
    }
    false
}

fn stmt_writes_var(stmt: &Stmt, var: &VarName) -> bool {
    if let Stmt::Let { var: v, .. } = stmt
        && v == var
    {
        return true;
    }
    stmt_counter_defs(stmt).iter().any(|v| v == var)
}

fn expr_var_set(e: &Expr) -> HashSet<VarName> {
    struct Collect {
        out: HashSet<VarName>,
    }
    impl crate::visit::Visitor for Collect {
        fn visit_var_read(&mut self, v: &VarName) {
            self.out.insert(v.clone());
        }
    }
    let mut c = Collect {
        out: HashSet::new(),
    };
    crate::visit::Visitor::visit_expr(&mut c, e);
    c.out
}

/// Stable variable order for interval propagation.
fn prom_sort_key(v: &VarName) -> (String, u8) {
    let k = match v.kind {
        VarKind::Float => 0u8,
        VarKind::Integer => 1,
        VarKind::String => 2,
    };
    (v.base.clone(), k)
}

/// Interval of `e` under the current partial solution.
fn prom_range_of(
    e: &Expr,
    candidates: &HashSet<VarName>,
    ranges: &HashMap<VarName, PromRange>,
) -> Option<PromRange> {
    let bin = |l: &Expr, r: &Expr| -> Option<(PromRange, PromRange)> {
        Some((
            prom_range_of(l, candidates, ranges)?,
            prom_range_of(r, candidates, ranges)?,
        ))
    };
    Some(match e {
        Expr::Number(n) => {
            if !n.is_finite() || n.fract() != 0.0 {
                return None;
            }
            PromRange::new(*n as i64, *n as i64)
        }
        Expr::Var(v) => {
            if v.kind == VarKind::Integer {
                PromRange::i16()
            } else {
                // A candidate with no interval yet is bottom.
                *ranges.get(v)?
            }
        }
        Expr::ArrayRef(a, _) => {
            if a.kind == VarKind::Integer {
                PromRange::i16()
            } else {
                *ranges.get(a)?
            }
        }
        Expr::Neg(inner) => {
            let r = prom_range_of(inner, candidates, ranges)?;
            PromRange::new(-r.max, -r.min)
        }
        // NOT / AND / OR / EOR all narrow through AYINT and produce an i16 result by
        // construction.
        Expr::Not(_) => PromRange::i16(),
        Expr::Bin(op, l, r) => match op {
            BinOp::Add => {
                let (a, b) = bin(l, r)?;
                PromRange::new(a.min.saturating_add(b.min), a.max.saturating_add(b.max))
            }
            BinOp::Sub => {
                let (a, b) = bin(l, r)?;
                PromRange::new(a.min.saturating_sub(b.max), a.max.saturating_sub(b.min))
            }
            BinOp::Mul => {
                let (a, b) = bin(l, r)?;
                let p = [
                    a.min.saturating_mul(b.min),
                    a.min.saturating_mul(b.max),
                    a.max.saturating_mul(b.min),
                    a.max.saturating_mul(b.max),
                ];
                PromRange::new(*p.iter().min().unwrap(), *p.iter().max().unwrap())
            }
            BinOp::Eq
            | BinOp::Ne
            | BinOp::Lt
            | BinOp::Le
            | BinOp::Gt
            | BinOp::Ge => PromRange::new(-1, 0),
            BinOp::And | BinOp::Or | BinOp::Xor => PromRange::i16(),
            _ => return None,
        },
        Expr::Func1(f, arg) => match f {
            Func1::Abs => {
                let r = prom_range_of(arg, candidates, ranges)?;
                let hi = r.min.saturating_abs().max(r.max.saturating_abs());
                PromRange::new(0, hi)
            }

            Func1::Int => prom_range_of(arg, candidates, ranges)?,
            Func1::Sgn => PromRange::new(-1, 1),
            _ => return None,
        },
        Expr::Peek(_) | Expr::MemPeek(_) | Expr::Asc(_) | Expr::Len(_) => PromRange::u8(),
        // Everything `int_stayable` accepts without being able to narrow it further.
        Expr::Pos(_)
        | Expr::Fre(_)
        | Expr::Joy(_)
        | Expr::Pot(_)
        | Expr::Inkey
        | Expr::Lin
        | Expr::At(_, _)
        | Expr::Test(_, _)
        | Expr::Check { .. }
        | Expr::Inst { .. }
        | Expr::StrCompare(_, _, _) => PromRange::i16(),
        _ => return None,
    })
}

/// Predicate: does evaluating `e` always produce an i16-range integer at runtime?
fn int_stayable(e: &Expr, int_vars: &HashSet<VarName>) -> bool {
    match e {
        Expr::Number(n) => n.is_finite() && n.fract() == 0.0 && (-32768.0..=32767.0).contains(n),
        Expr::Var(v) => v.kind == VarKind::Integer || int_vars.contains(v),
        Expr::Neg(inner) => int_stayable(inner, int_vars),
        Expr::Not(inner) => int_stayable(inner, int_vars),
        Expr::Bin(op, l, r) => {
            // Add/Sub/Mul preserve int (with overflow risk).
            // Compare ops produce -1 / 0 — int.
            // Bitwise And/Or operate on i16 — int.
            // Div / Pow generally introduce fractions — bail.
            matches!(
                op,
                BinOp::Add
                    | BinOp::Sub
                    | BinOp::Mul
                    | BinOp::Eq
                    | BinOp::Ne
                    | BinOp::Lt
                    | BinOp::Le
                    | BinOp::Gt
                    | BinOp::Ge
                    | BinOp::And
                    | BinOp::Or
                    | BinOp::Xor
            ) && int_stayable(l, int_vars)
                && int_stayable(r, int_vars)
        }
        Expr::Func1(f, arg) => match f {
            // ABS/INT/SGN preserve int when input is int.
            Func1::Abs | Func1::Int | Func1::Sgn => int_stayable(arg, int_vars),
            _ => false,
        },
        // PEEK returns 0..255 — always int.
        Expr::Peek(_) | Expr::MemPeek(_) => true,
        // ASC/LEN return 0..255 — always int.
        Expr::Asc(_) | Expr::Len(_) => true,
        // POS / FRE return small ints. USR returns whatever the user
        // routine left in FAC — opaque, so disqualify.
        Expr::Pos(_) => true,
        Expr::Fre(_) => true,
        Expr::Usr(_) => false,
        Expr::Joy(_) => true,
        Expr::Pot(_) => true,
        Expr::Inkey => true,
        Expr::Lin => true,
        Expr::At(row, col) => int_stayable(row, int_vars) && int_stayable(col, int_vars),
        Expr::Test(x, y) => int_stayable(x, int_vars) && int_stayable(y, int_vars),
        Expr::Check { first, second } => {
            int_stayable(first, int_vars)
                && second.as_ref().map_or(true, |e| int_stayable(e, int_vars))
        }
        Expr::Inst { start, .. } => start.as_ref().map_or(true, |e| int_stayable(e, int_vars)),
        // ArrayRef on an integer (or promoted) array is int; on a
        // float array we'd be reading a float so bail.
        Expr::ArrayRef(name, idx) => {
            (name.kind == VarKind::Integer || int_vars.contains(name))
                && idx.iter().all(|e| int_stayable(e, int_vars))
        }
        // Compares between two strings yield -1/0 — int.
        Expr::StrCompare(_, _, _) => true,
        // VAL returns a float that may have a fractional part.
        Expr::Val(_) | Expr::Nrm(_) => false,
        // FnCall is opaque: param substitution + arbitrary body.
        Expr::FnCall(_, _) => false,
        Expr::String(_) => false,
    }
}

/// Walk the IR rewriting every VarName position whose name is in the
/// promoted set: change kind from Float to Integer. Touches LET
/// targets, FOR vars, every Var/ArrayRef in expressions, and the
/// scalar-target list inside READ/INPUT (those vars were already
/// disqualified during scanning, so the rewrite is a no-op there
/// for promoted vars — but we visit them anyway for completeness).
struct VarKindPromoter<'a> {
    promote: &'a HashSet<VarName>,
}

impl<'a> VarKindPromoter<'a> {
    fn rewrite_module(&mut self, module: &mut ir::Module) {
        for line in &mut module.lines {
            for stmt in &mut line.stmts {
                self.rewrite_stmt(stmt);
            }
        }
    }

    fn maybe_promote(&self, v: &mut VarName) {
        if v.kind == VarKind::Float && self.promote.contains(v) {
            v.kind = VarKind::Integer;
        }
    }

    fn rewrite_stmt(&mut self, stmt: &mut Stmt) {
        match stmt {
            Stmt::Let { var, value } => {
                self.maybe_promote(var);
                self.rewrite_expr(value);
            }
            Stmt::LetStr { var: _, value } => {
                // LetStr's var was disqualified, but rewrite RHS in
                // case it mentions other promoted vars.
                self.rewrite_str(value);
            }
            Stmt::ArrayLet {
                name,
                indices,
                value,
            } => {
                self.maybe_promote(name);
                for e in indices.iter_mut() {
                    self.rewrite_expr(e);
                }
                self.rewrite_expr(value);
            }
            Stmt::ArrayLetStr {
                name,
                indices,
                value,
            } => {
                self.maybe_promote(name);
                for e in indices.iter_mut() {
                    self.rewrite_expr(e);
                }
                self.rewrite_str(value);
            }
            Stmt::If { cond, then } => {
                self.rewrite_expr(cond);
                if let ThenIr::Stmts(inner) = then {
                    for s in inner.iter_mut() {
                        self.rewrite_stmt(s);
                    }
                }
            }
            Stmt::IfElse {
                cond,
                then,
                else_then,
            } => {
                self.rewrite_expr(cond);
                for branch in [then, else_then] {
                    if let ThenIr::Stmts(inner) = branch {
                        for s in inner.iter_mut() {
                            self.rewrite_stmt(s);
                        }
                    }
                }
            }
            Stmt::Rcomp { then, else_then } => {
                if let ThenIr::Stmts(inner) = then {
                    for s in inner.iter_mut() {
                        self.rewrite_stmt(s);
                    }
                }
                if let Some(ThenIr::Stmts(inner)) = else_then {
                    for s in inner.iter_mut() {
                        self.rewrite_stmt(s);
                    }
                }
            }
            // Loop conditions also reference vars (`REPEAT … UNTIL X>N`,
            // `EXITIF X=Y`, `DO … LOOP X<N`) — without rewriting these
            // a demoted Float→Integer var keeps its stale `V_<base>`
            // float slot here while every write lands in `VI_<base>`.
            Stmt::DoIf { cond } | Stmt::Until { cond } => self.rewrite_expr(cond),
            Stmt::ExitLoop { cond } => {
                if let Some(cond) = cond {
                    self.rewrite_expr(cond);
                }
            }
            Stmt::ComputedGoto { target } => self.rewrite_expr(target),
            Stmt::Reset { target } => self.rewrite_expr(target),
            Stmt::For {
                var,
                start,
                end,
                step,
                ..
            } => {
                self.maybe_promote(var);
                self.rewrite_expr(start);
                self.rewrite_expr(end);
                self.rewrite_expr(step);
            }
            Stmt::Next { vars } => {
                for v in vars.iter_mut().flatten() {
                    self.maybe_promote(v);
                }
            }
            Stmt::Print { items, .. } | Stmt::PrintFile { items, .. } | Stmt::Cmd { items, .. } => {
                for it in items.iter_mut() {
                    self.rewrite_print(it);
                }
            }
            Stmt::Poke { addr, value } => {
                self.rewrite_expr(addr);
                self.rewrite_expr(value);
            }
            Stmt::Dpoke { addr, value } => {
                self.rewrite_expr(addr);
                self.rewrite_expr(value);
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
                for e in [row, col, width, height] {
                    self.rewrite_expr(e);
                }
                if let Some(e) = ch {
                    self.rewrite_expr(e);
                }
                if let Some(e) = color {
                    self.rewrite_expr(e);
                }
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
                    self.rewrite_expr(e);
                }
            }
            Stmt::ScreenScroll {
                row,
                col,
                width,
                height,
                ..
            } => {
                for e in [row, col, width, height] {
                    self.rewrite_expr(e);
                }
            }
            Stmt::Color {
                border,
                background,
                pen,
            } => {
                for e in border
                    .iter_mut()
                    .chain(background.iter_mut())
                    .chain(pen.iter_mut())
                {
                    self.rewrite_expr(e);
                }
            }
            Stmt::MobEnable { index, .. } => self.rewrite_expr(index),
            Stmt::Multi { .. } | Stmt::HiCol | Stmt::Hires { .. } => {}
            Stmt::MultiColors { c1, c2, c3 } => {
                self.rewrite_expr(c1);
                self.rewrite_expr(c2);
                self.rewrite_expr(c3);
            }
            Stmt::Border { color } => self.rewrite_expr(color),
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
                    self.rewrite_expr(e);
                }
                if let Some(e) = mode {
                    self.rewrite_expr(e);
                }
            }
            Stmt::Rec {
                x,
                y,
                width,
                height,
                mode,
            } => {
                for e in [x, y, width, height] {
                    self.rewrite_expr(e);
                }
                if let Some(e) = mode {
                    self.rewrite_expr(e);
                }
            }
            Stmt::Draw { x, y, mode }
            | Stmt::DrawTo { x, y, mode }
            | Stmt::Paint { x, y, mode } => {
                self.rewrite_expr(x);
                self.rewrite_expr(y);
                if let Some(e) = mode {
                    self.rewrite_expr(e);
                }
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
                self.rewrite_expr(cx);
                self.rewrite_expr(cy);
                self.rewrite_expr(radius);
                for opt in [ry, start, end, step, mode] {
                    if let Some(e) = opt {
                        self.rewrite_expr(e);
                    }
                }
            }
            Stmt::Char {
                x,
                y,
                code,
                mode,
                zoom,
            } => {
                self.rewrite_expr(x);
                self.rewrite_expr(y);
                self.rewrite_expr(code);
                if let Some(e) = mode {
                    self.rewrite_expr(e);
                }
                if let Some(e) = zoom {
                    self.rewrite_expr(e);
                }
            }
            Stmt::Text {
                x,
                y,
                text,
                mode,
                zoom,
                kerning,
            } => {
                self.rewrite_expr(x);
                self.rewrite_expr(y);
                self.rewrite_str(text);
                if let Some(e) = mode {
                    self.rewrite_expr(e);
                }
                if let Some(e) = zoom {
                    self.rewrite_expr(e);
                }
                if let Some(e) = kerning {
                    self.rewrite_expr(e);
                }
            }
            Stmt::Rot { direction, length } => {
                self.rewrite_expr(direction);
                if let Some(l) = length {
                    self.rewrite_expr(l);
                }
            }
            Stmt::DrawString { code, x, y, mode } => {
                self.rewrite_str(code);
                self.rewrite_expr(x);
                self.rewrite_expr(y);
                if let Some(e) = mode {
                    self.rewrite_expr(e);
                }
            }
            Stmt::Design { addr, bytes } => {
                // `DESIGN type, addr` keeps the target address live,
                // often through a FOR-counter or small-int variable.
                // Without rewriting it, codegen reads the (never-
                // synced) float mirror `V_x` instead of `VI_x`, so
                // `BL`(=11) was seen as 0 and the sprite shape landed
                // at `BASE` instead of `BASE+11*64`.
                self.rewrite_expr(addr);
                for e in bytes.iter_mut() {
                    self.rewrite_expr(e);
                }
            }
            Stmt::InsertBox {
                pattern,
                row,
                col,
                width,
                height,
                color,
            } => {
                self.rewrite_str(pattern);
                self.rewrite_expr(row);
                self.rewrite_expr(col);
                self.rewrite_expr(width);
                self.rewrite_expr(height);
                self.rewrite_expr(color);
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
                    self.rewrite_expr(e);
                }
                for opt in [ry, mode] {
                    if let Some(e) = opt {
                        self.rewrite_expr(e);
                    }
                }
            }
            Stmt::Sound { voice, freq } => {
                self.rewrite_expr(voice);
                self.rewrite_expr(freq);
            }
            Stmt::Envelope {
                voice,
                attack,
                decay,
                sustain,
                release,
            } => {
                for e in [voice, attack, decay, sustain, release] {
                    self.rewrite_expr(e);
                }
            }
            Stmt::Wave {
                voice,
                control,
                pulse,
            } => {
                self.rewrite_expr(voice);
                self.rewrite_expr(control);
                if let Some(e) = pulse {
                    self.rewrite_expr(e);
                }
            }
            Stmt::LowCol {
                color1,
                color2,
                color3,
            } => {
                self.rewrite_expr(color1);
                self.rewrite_expr(color2);
                if let Some(e) = color3 {
                    self.rewrite_expr(e);
                }
            }
            Stmt::Mod { ink, paper } => {
                self.rewrite_expr(ink);
                self.rewrite_expr(paper);
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
                    self.rewrite_expr(e);
                }
                for opt in [mode, zoom] {
                    if let Some(e) = opt {
                        self.rewrite_expr(e);
                    }
                }
            }
            Stmt::Mmob { index, x, y } => {
                self.rewrite_expr(index);
                self.rewrite_expr(x);
                self.rewrite_expr(y);
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
                    self.rewrite_expr(e);
                }
                if let Some(e) = size {
                    self.rewrite_expr(e);
                }
                if let Some(e) = speed {
                    self.rewrite_expr(e);
                }
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
                    self.rewrite_expr(e);
                }
                if let Some(e) = size {
                    self.rewrite_expr(e);
                }
                if let Some(e) = speed {
                    self.rewrite_expr(e);
                }
            }
            Stmt::Rlocmob {
                index,
                dx,
                dy,
                speed,
            } => {
                self.rewrite_expr(index);
                self.rewrite_expr(dx);
                self.rewrite_expr(dy);
                if let Some(e) = speed {
                    self.rewrite_expr(e);
                }
            }
            Stmt::Detect { mode } => self.rewrite_expr(mode),
            Stmt::Cmob { color1, color2 } => {
                self.rewrite_expr(color1);
                self.rewrite_expr(color2);
            }
            Stmt::Bckgnds {
                color0,
                color1,
                color2,
                color3,
            } => {
                for e in [color0, color1, color2, color3] {
                    self.rewrite_expr(e);
                }
            }
            Stmt::Cset { mode } => self.rewrite_expr(mode),
            Stmt::Pause { ticks, .. } => self.rewrite_expr(ticks),
            Stmt::Sys {
                addr,
                regs,
                param_vars,
                ..
            } => {
                self.rewrite_expr(addr);
                for r in regs {
                    self.rewrite_expr(r);
                }
                for p in param_vars {
                    self.rewrite_expr(&mut p.value);
                }
            }
            Stmt::Wait { addr, mask, eor } => {
                self.rewrite_expr(addr);
                self.rewrite_expr(mask);
                if let Some(e) = eor {
                    self.rewrite_expr(e);
                }
            }
            Stmt::Open {
                file_num,
                device,
                secondary,
                filename,
            } => {
                self.rewrite_expr(file_num);
                if let Some(e) = device {
                    self.rewrite_expr(e);
                }
                if let Some(e) = secondary {
                    self.rewrite_expr(e);
                }
                if let Some(s) = filename {
                    self.rewrite_str(s);
                }
            }
            Stmt::Close { file_num } | Stmt::GetFile { file_num, .. } => {
                self.rewrite_expr(file_num)
            }
            Stmt::InputFile { file_num, targets } => {
                self.rewrite_expr(file_num);
                for t in targets.iter_mut() {
                    if let crate::ir::ReadTarget::Array { indices, .. } = t {
                        for e in indices.iter_mut() {
                            self.rewrite_expr(e);
                        }
                    }
                }
            }
            Stmt::Read(targets) | Stmt::Input { targets, .. } => {
                for t in targets.iter_mut() {
                    if let crate::ir::ReadTarget::Array { indices, .. } = t {
                        for e in indices.iter_mut() {
                            self.rewrite_expr(e);
                        }
                    }
                }
            }
            Stmt::Load {
                device, secondary, ..
            }
            | Stmt::Verify {
                device, secondary, ..
            }
            | Stmt::Save {
                device, secondary, ..
            } => {
                if let Some(e) = device {
                    self.rewrite_expr(e);
                }
                if let Some(e) = secondary {
                    self.rewrite_expr(e);
                }
            }
            Stmt::OnBranch { value, .. } => self.rewrite_expr(value),
            Stmt::DefFn { body, .. } => self.rewrite_expr(body),
            Stmt::Dim(specs) => {
                // Promote the array name itself so the DIM statement
                // names the same kind that ArrayLet/ArrayRef will use
                // after this pass — otherwise codegen registers the
                // DIM under the Float key and the auto-DIM path
                // re-creates a stub Integer entry with the BASIC v2
                // default (DIM 10), shrinking the runtime allocation.
                for spec in specs.iter_mut() {
                    self.maybe_promote(&mut spec.name);
                    for e in spec.dims.iter_mut() {
                        self.rewrite_expr(e);
                    }
                }
            }
            other => {
                // Rewrite reads in every remaining statement via the generic
                // walk, so a promoted var is not left reading the old Float slot.
                struct Reads<'p, 'a>(&'p VarKindPromoter<'a>);
                impl crate::visit::MutVisitor for Reads<'_, '_> {
                    fn visit_expr_mut(&mut self, e: &mut Expr) {
                        self.0.rewrite_expr(e);
                    }
                    fn visit_str_expr_mut(&mut self, s: &mut StrExpr) {
                        self.0.rewrite_str(s);
                    }
                }
                crate::visit::walk_stmt_mut(&mut Reads(self), 0, other);
            }
        }
    }

    fn rewrite_expr(&self, e: &mut Expr) {
        match e {
            Expr::Var(v) => {
                if v.kind == VarKind::Float && self.promote.contains(v) {
                    v.kind = VarKind::Integer;
                }
            }
            Expr::ArrayRef(name, idx) => {
                if name.kind == VarKind::Float && self.promote.contains(name) {
                    name.kind = VarKind::Integer;
                }
                for e in idx.iter_mut() {
                    self.rewrite_expr(e);
                }
            }
            Expr::Neg(inner) | Expr::Not(inner) => self.rewrite_expr(inner),
            Expr::Bin(_, l, r) => {
                self.rewrite_expr(l);
                self.rewrite_expr(r);
            }
            Expr::Func1(_, arg)
            | Expr::Peek(arg)
            | Expr::MemPeek(arg)
            | Expr::FnCall(_, arg)
            | Expr::Pos(arg)
            | Expr::Fre(arg)
            | Expr::Usr(arg)
            | Expr::Joy(arg)
            | Expr::Pot(arg) => self.rewrite_expr(arg),
            Expr::Len(s) | Expr::Asc(s) | Expr::Val(s) | Expr::Nrm(s) => self.rewrite_str(s),
            Expr::StrCompare(_, l, r) => {
                self.rewrite_str(l);
                self.rewrite_str(r);
            }
            Expr::At(row, col) => {
                self.rewrite_expr(row);
                self.rewrite_expr(col);
            }
            Expr::Test(x, y) => {
                self.rewrite_expr(x);
                self.rewrite_expr(y);
            }
            Expr::Check { first, second } => {
                self.rewrite_expr(first);
                if let Some(e) = second {
                    self.rewrite_expr(e);
                }
            }
            Expr::Inst {
                haystack,
                needle,
                start,
            } => {
                self.rewrite_str(haystack);
                self.rewrite_str(needle);
                if let Some(e) = start {
                    self.rewrite_expr(e);
                }
            }
            Expr::Number(_) | Expr::String(_) | Expr::Inkey | Expr::Lin => {}
        }
    }

    fn rewrite_str(&self, s: &mut StrExpr) {
        match s {
            StrExpr::Chr(e) | StrExpr::Str(e) | StrExpr::HexFmt(e) | StrExpr::BinFmt(e) => {
                self.rewrite_expr(e)
            }
            StrExpr::Concat(a, b) => {
                self.rewrite_str(a);
                self.rewrite_str(b);
            }
            StrExpr::Left(s, n) | StrExpr::Right(s, n) => {
                self.rewrite_str(s);
                self.rewrite_expr(n);
            }
            StrExpr::Mid(s, st, n) => {
                self.rewrite_str(s);
                self.rewrite_expr(st);
                if let Some(b) = n {
                    self.rewrite_expr(b);
                }
            }
            StrExpr::Dup(s, n) => {
                self.rewrite_str(s);
                self.rewrite_expr(n);
            }
            StrExpr::Insert(s, t, pos) => {
                self.rewrite_str(s);
                self.rewrite_str(t);
                self.rewrite_expr(pos);
            }
            StrExpr::ArrayRef(_, idx) => {
                for e in idx.iter_mut() {
                    self.rewrite_expr(e);
                }
            }
            StrExpr::Literal(_) | StrExpr::Var(_) | StrExpr::GetKey => {}
        }
    }

    fn rewrite_print(&self, p: &mut PrintPiece) {
        match p {
            PrintPiece::Expr(e)
            | PrintPiece::CharOut(e)
            | PrintPiece::TabTo(e)
            | PrintPiece::Spc(e) => {
                self.rewrite_expr(e);
            }
            PrintPiece::StrExpr(s) => self.rewrite_str(s),
            PrintPiece::PositionAt(r, c) => {
                self.rewrite_expr(r);
                self.rewrite_expr(c);
            }
            PrintPiece::LiteralString(_) | PrintPiece::Tab => {}
        }
    }
}

// ===== Dead-store elimination =====

/// Remove `LET x = expr` statements whose target `x` isn't live afterward AND whose RHS is
/// provably side-effect free.
pub struct ResolveBareNext;

impl ir::Pass for ResolveBareNext {
    fn name(&self) -> &'static str {
        "resolve-bare-next"
    }

    fn run(&self, module: &mut ir::Module) -> Result<(), ir::PassError> {
        // Computed up front, on the un-renamed module, so the ids line up with the walk below.
        let orphans = crate::for_next_runtime::analyze(module);
        let mut st = BareNextState::default();
        for (line_idx, line) in module.lines.iter_mut().enumerate() {
            let line_no = line.number;
            for (top_idx, stmt) in line.stmts.iter_mut().enumerate() {
                resolve_bare_next_stmt(
                    stmt,
                    &mut st,
                    &orphans,
                    line_no,
                    line_idx,
                    &[top_idx],
                );
            }
        }
        Ok(())
    }
}

/// Walk state for `ResolveBareNext`.
#[derive(Default)]
struct BareNextState {
    for_stack: Vec<VarName>,
    /// A FOR frame opened only on a conditional path.
    conditional_open: bool,
}

impl BareNextState {
    /// Model BASIC's FNDFOR discard at `FOR V`: drop the newest live frame for `V` together
    /// with everything stacked on top of it.
    fn discard_same_var(&mut self, var: &VarName) {
        if let Some(pos) = self.for_stack.iter().rposition(|v| v == var) {
            self.for_stack.truncate(pos);
        }
    }

    fn pop_frame(&mut self) -> Option<VarName> {
        let top = self.for_stack.pop();
        if self.for_stack.is_empty() {
            // Back to an empty stack: whatever a conditional branch left open cannot be the
            // innermost frame of any loop opened from here on, so the ambiguity is resolved.
            self.conditional_open = false;
        }
        top
    }
}

fn resolve_bare_next_stmt(
    stmt: &mut Stmt,
    st: &mut BareNextState,
    orphans: &crate::for_next_runtime::OrphanAnalysis,
    line_no: u16,
    line_idx: usize,
    path: &[usize],
) {
    match stmt {
        Stmt::For { var, .. } => {

            st.discard_same_var(var);
            st.for_stack.push(var.clone());
        }
        Stmt::Next { vars } => {
            // Routed through the runtime dispatcher: the innermost frame is a runtime fact, so
            // leave the NEXT bare and let the dispatcher take the top of the real stack.
            let dispatched = orphans.is_orphan_next(line_no, line_idx, path);
            for v in vars.iter_mut() {
                match v {
                    None => {
                        // Only name the loop when the compile-time stack really is the runtime
                        // stack.
                        let ambiguous = st.conditional_open || dispatched;
                        let top = st.pop_frame();
                        if !ambiguous && let Some(top) = top {
                            *v = Some(top);
                        }
                    }
                    Some(name) => {
                        while let Some(top) = st.for_stack.last() {
                            if top == name {
                                st.pop_frame();
                                break;
                            }
                            st.pop_frame();
                        }
                    }
                }
            }
        }
        Stmt::If { then, .. } => {
            // Same snapshot discipline as IfElse: a NEXT under a
            // condition only closes the FOR when the condition holds,
            // so the fall-through path still has it open and its own
            // `NEXT` must still resolve to that loop variable.
            resolve_bare_next_branch(then, st, orphans, line_no, line_idx, path);
        }
        Stmt::IfElse {
            then, else_then, ..
        } => {
            // Snapshot so a NEXT in one branch doesn't drain the
            // sibling branch's view of the stack.
            resolve_bare_next_branch(then, st, orphans, line_no, line_idx, path);
            resolve_bare_next_branch(else_then, st, orphans, line_no, line_idx, path);
        }
        Stmt::Rcomp { then, else_then } => {
            resolve_bare_next_branch(then, st, orphans, line_no, line_idx, path);
            if let Some(et) = else_then {
                resolve_bare_next_branch(et, st, orphans, line_no, line_idx, path);
            }
        }
        _ => {}
    }
}

/// Walk one conditional branch.
fn resolve_bare_next_branch(
    then: &mut crate::ir::ThenIr,
    st: &mut BareNextState,
    orphans: &crate::for_next_runtime::OrphanAnalysis,
    line_no: u16,
    line_idx: usize,
    path: &[usize],
) {
    let snapshot = st.for_stack.clone();
    let depth = snapshot.len();
    resolve_bare_next_then(then, st, orphans, line_no, line_idx, path);
    if st.for_stack.len() > depth {
        st.conditional_open = true;
    }
    st.for_stack = snapshot;
}

fn resolve_bare_next_then(
    then: &mut crate::ir::ThenIr,
    st: &mut BareNextState,
    orphans: &crate::for_next_runtime::OrphanAnalysis,
    line_no: u16,
    line_idx: usize,
    path: &[usize],
) {
    if let crate::ir::ThenIr::Stmts(inner) = then {
        // Per-arm 0-based indices, matching `for_next_runtime`'s `recurse_then` and codegen's
        // `emit_then_body`.
        for (idx, s) in inner.iter_mut().enumerate() {
            let mut child = path.to_vec();
            child.push(idx);
            resolve_bare_next_stmt(s, st, orphans, line_no, line_idx, &child);
        }
    }
}

pub struct DeadStoreElim;

impl ir::Pass for DeadStoreElim {
    fn name(&self) -> &'static str {
        "dead-store-elim"
    }

    fn run(&self, module: &mut ir::Module) -> Result<(), ir::PassError> {
        // Build the CFG + live-vars analysis once. Both go through
        // the registry so other passes downstream can reuse them.
        let mut reg = crate::analysis::Registry::new();
        let cfg = reg.get(module, &crate::cfg::CfgBuild).clone();
        let live = reg.get(module, &crate::dataflow::LiveVars).clone();

        // When the program never calls FRE, heap-allocating string
        // RHSs (Concat, CHR$, STR$, Left/Right/Mid, Dup, Insert) are
        // unobservable apart from the binding they create — dropping
        // a dead one just lets the heap fill slower and the GC fire
        // less often. With FRE present we keep the conservative
        // `str_is_pure` gate so the FRE return value doesn't change.
        let fre_observable = module_uses_fre(module);

        let fretop_observable = module_reads_fretop(module);

        // Positions where the statement being there at all is the observable effect ; see `stack_floor_trap_positions`.
        let floor_traps = stack_floor_trap_positions(module);

        // For each top-level LET, find its CfgNode and check if the
        // var is live at the OUT side of that node. If not — and
        // the RHS is pure — mark the line+stmt for removal.
        let mut to_remove: HashSet<(usize, Vec<usize>)> = HashSet::new();
        for (id, node) in cfg.nodes.iter().enumerate() {
            let stmt = cfg.stmt_at(id, module);
            let drop = match stmt {
                Stmt::Let { var, value } => {
                    let live_out = &live.per_node[id].1;

                    !live_out.contains(var)
                        && expr_is_pure(value)
                        && expr_cannot_trap(value)
                        && int_store_cannot_trap(var, value)
                }
                Stmt::LetStr { var, value } => {
                    let live_out = &live.per_node[id].1;

                    !live_out.contains(var)
                        && str_store_cannot_trap(var)
                        && str_is_pure_for_dead_store(value, fre_observable)
                        && !(fretop_observable && is_str_self_assign(var, value))
                }
                _ => false,
            };
            if drop
                && (floor_traps.is_empty()
                    || !floor_traps.contains(&(node.stmt.line_idx, node.stmt.path.clone())))
            {
                to_remove.insert((node.stmt.line_idx, node.stmt.path.clone()));
            }
        }
        if to_remove.is_empty() {
            return Ok(());
        }

        // Apply: walk lines and drop the marked stmts. Path-based
        // removal so we can hit nested THEN-body LETs too.
        for (line_idx, line) in module.lines.iter_mut().enumerate() {
            drop_paths_in(&mut line.stmts, &[], line_idx, &to_remove);
        }
        Ok(())
    }
}

/// Statement positions where evaluating anything at all is fatal, so deleting the statement deletes a `?OUT OF MEMORY`.
fn stack_floor_trap_positions(module: &ir::Module) -> HashSet<(usize, Vec<usize>)> {
    let mut out: HashSet<(usize, Vec<usize>)> = HashSet::new();
    let mut depth = 0usize;
    let mut path: Vec<usize> = Vec::new();
    for (line_idx, line) in module.lines.iter().enumerate() {
        walk_stack_floor(&line.stmts, &mut path, 0, line_idx, &mut depth, &mut out);
        debug_assert!(path.is_empty());
    }
    out
}

/// One nesting level of [`stack_floor_trap_positions`].
fn walk_stack_floor(
    stmts: &[Stmt],
    path: &mut Vec<usize>,
    path_offset: usize,
    line_idx: usize,
    depth: &mut usize,
    out: &mut HashSet<(usize, Vec<usize>)>,
) {
    for (i, stmt) in stmts.iter().enumerate() {
        path.push(path_offset + i);

        if *depth > crate::codegen::FOR_NEST_EXPR_MAX
            && crate::codegen::stmt_evaluates_expression(stmt)
        {
            out.insert((line_idx, path.clone()));
        }
        match stmt {
            Stmt::For { .. } => *depth += 1,
            Stmt::Next { vars } => {
                *depth = depth.saturating_sub(vars.len().max(1));
            }
            Stmt::If {
                then: ThenIr::Stmts(inner),
                ..
            } => {
                let before = *depth;
                walk_stack_floor(inner, path, 0, line_idx, depth, out);
                *depth = (*depth).max(before);
            }
            Stmt::IfElse {
                then, else_then, ..
            } => {
                let before = *depth;
                let then_len = then_branch_len(then);
                walk_stack_floor_then(then, path, 0, line_idx, depth, out);
                *depth = (*depth).max(before);
                let after_then = *depth;
                walk_stack_floor_then(else_then, path, then_len, line_idx, depth, out);
                *depth = (*depth).max(after_then);
            }
            Stmt::Rcomp { then, else_then } => {
                let before = *depth;
                let then_len = then_branch_len(then);
                walk_stack_floor_then(then, path, 0, line_idx, depth, out);
                *depth = (*depth).max(before);
                let after_then = *depth;
                if let Some(else_then) = else_then {
                    walk_stack_floor_then(else_then, path, then_len, line_idx, depth, out);
                    *depth = (*depth).max(after_then);
                }
            }
            _ => {}
        }
        path.pop();
    }
}

fn walk_stack_floor_then(
    then: &ThenIr,
    path: &mut Vec<usize>,
    path_offset: usize,
    line_idx: usize,
    depth: &mut usize,
    out: &mut HashSet<(usize, Vec<usize>)>,
) {
    if let ThenIr::Stmts(inner) = then {
        walk_stack_floor(inner, path, path_offset, line_idx, depth, out);
    }
}

fn drop_paths_in(
    stmts: &mut Vec<Stmt>,
    prefix: &[usize],
    line_idx: usize,
    to_remove: &HashSet<(usize, Vec<usize>)>,
) {
    drop_paths_in_offset(stmts, prefix, line_idx, to_remove, 0);
}

fn drop_paths_in_offset(
    stmts: &mut Vec<Stmt>,
    prefix: &[usize],
    line_idx: usize,
    to_remove: &HashSet<(usize, Vec<usize>)>,
    path_offset: usize,
) {
    // First recurse into nested branch bodies (deepest first) so nested
    // path indices stay stable as we don't mutate parent arrays
    // before child arrays.
    for (i, stmt) in stmts.iter_mut().enumerate() {
        let mut p = prefix.to_vec();
        p.push(path_offset + i);
        match stmt {
            Stmt::If {
                then: ThenIr::Stmts(inner),
                ..
            } => {
                drop_paths_in_offset(inner, &p, line_idx, to_remove, 0);
            }
            Stmt::IfElse {
                then, else_then, ..
            } => {
                let then_len = then_branch_len(then);
                drop_paths_in_then(then, &p, line_idx, to_remove, 0);
                drop_paths_in_then(else_then, &p, line_idx, to_remove, then_len);
            }
            Stmt::Rcomp { then, else_then } => {
                let then_len = then_branch_len(then);
                drop_paths_in_then(then, &p, line_idx, to_remove, 0);
                if let Some(else_then) = else_then {
                    drop_paths_in_then(else_then, &p, line_idx, to_remove, then_len);
                }
            }
            _ => {}
        }
    }
    // Now drop matching stmts at this level. Build a closure that
    // checks each stmt's full path against the removal set.
    let prefix = prefix.to_vec();
    let mut idx = 0;
    stmts.retain(|_| {
        let mut p = prefix.clone();
        p.push(path_offset + idx);
        idx += 1;
        !to_remove.contains(&(line_idx, p))
    });
}

fn drop_paths_in_then(
    then: &mut ThenIr,
    prefix: &[usize],
    line_idx: usize,
    to_remove: &HashSet<(usize, Vec<usize>)>,
    path_offset: usize,
) {
    if let ThenIr::Stmts(inner) = then {
        drop_paths_in_offset(inner, prefix, line_idx, to_remove, path_offset);
    }
}

fn then_branch_len(then: &ThenIr) -> usize {
    match then {
        ThenIr::Goto(_) => 0,
        ThenIr::Stmts(inner) => inner.len(),
    }
}

/// True iff evaluating `e` provably cannot raise a BASIC error.
pub(crate) fn expr_cannot_trap(e: &Expr) -> bool {
    match e {
        Expr::Number(n) => n.is_finite() && !crate::codegen::mflpt_overflows(*n),
        // A plain slot read converts nothing and traps on nothing.
        Expr::Var(_) | Expr::String(_) => true,
        // Sign flip cannot leave the representable range.
        Expr::Neg(inner) => expr_cannot_trap(inner),
        Expr::Bin(op, l, r) => match op {
            // FCOMP yields -1/0 and cannot overflow.
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                expr_cannot_trap(l) && expr_cannot_trap(r)
            }
            // FADD/FSUB/FMULT can overflow, and there is nothing in the IR that bounds a
            // variable's magnitude.
            BinOp::Add | BinOp::Sub | BinOp::Mul => match (l.as_ref(), r.as_ref()) {
                (Expr::Number(a), Expr::Number(b)) => fold_bin(*op, *a, *b).is_some(),
                _ => false,
            },
            _ => false,
        },

        Expr::Func1(f, arg) => {
            matches!(f, Func1::Abs | Func1::Int | Func1::Sgn) && expr_cannot_trap(arg)
        }

        Expr::Peek(_) | Expr::MemPeek(_) => true,
        // String compare walks two descriptors byte by byte and drops -1/0 in FAC ($B02E).
        Expr::StrCompare(_, l, r) => str_is_pure(l) && str_is_pure(r),
        _ => false,
    }
}

/// True iff evaluating `e` has no observable side effects on memory, the string heap or the
/// RNG.
fn int_store_cannot_trap(var: &VarName, value: &Expr) -> bool {
    if var.kind != VarKind::Integer || var.base == "TI" || var.base == "ST" {
        return true;
    }
    if let Some(n) = as_simple_literal(value) {
        return narrow_for_int_store(var, n).is_some();
    }
    expr_is_ayint_narrowed(value)
}

fn str_store_cannot_trap(var: &VarName) -> bool {
    !(var.kind == VarKind::String && var.base == "TI")
}

/// Runtime-written pseudo-variables: `TI`/`TI$`, `ST`, `ER`/`EL`. Name test, not kind.
/// Distinct from [`expr_is_pure`]: reading `TI` has no side effect, it just
/// does not give the same answer twice.
pub(crate) fn var_is_volatile(v: &VarName) -> bool {
    matches!(v.base.as_str(), "TI" | "ST" | "ER" | "EL")
}

/// True if evaluating `e` again later yields the same value. Whitelist: unknown
/// forms are refused (`PEEK`, `POS`, `FRE`, `RND`, `USR`, `FN`, I/O, strings).
/// `ArrayRef` covers subscripts only; element stability is the caller's problem.
pub(crate) fn expr_reads_are_repeatable(e: &Expr) -> bool {
    match e {
        Expr::Number(n) => n.is_finite(),
        Expr::Var(v) => !var_is_volatile(v),
        Expr::ArrayRef(name, idx) => {
            !var_is_volatile(name) && idx.iter().all(expr_reads_are_repeatable)
        }
        Expr::Neg(inner) | Expr::Not(inner) => expr_reads_are_repeatable(inner),
        Expr::Bin(_, l, r) => expr_reads_are_repeatable(l) && expr_reads_are_repeatable(r),
        // RND advances the seed. Other one-arg functions are pure in the argument.
        Expr::Func1(crate::ast::Func1::Rnd, _) => false,
        Expr::Func1(_, arg) => expr_reads_are_repeatable(arg),
        _ => false,
    }
}

/// String side of [`expr_reads_are_repeatable`]: would evaluating `s` again
/// later yield the same characters? `TI$` is a `StrExpr::Var`, so the numeric
/// predicate never sees it. Value only: callers that care about allocation ask [`str_is_pure`].
pub(crate) fn str_reads_are_repeatable(s: &StrExpr) -> bool {
    match s {
        StrExpr::Literal(_) => true,
        StrExpr::Var(v) => !var_is_volatile(v),
        StrExpr::ArrayRef(name, idx) => {
            !var_is_volatile(name) && idx.iter().all(expr_reads_are_repeatable)
        }
        // GET advances the keyboard buffer, so the next read is a different key.
        StrExpr::GetKey => false,
        StrExpr::Chr(e) | StrExpr::HexFmt(e) | StrExpr::BinFmt(e) | StrExpr::Str(e) => {
            expr_reads_are_repeatable(e)
        }
        StrExpr::Concat(a, b) => str_reads_are_repeatable(a) && str_reads_are_repeatable(b),
        StrExpr::Left(s, n) | StrExpr::Right(s, n) | StrExpr::Dup(s, n) => {
            str_reads_are_repeatable(s) && expr_reads_are_repeatable(n)
        }
        StrExpr::Mid(s, from, len) => {
            str_reads_are_repeatable(s)
                && expr_reads_are_repeatable(from)
                && len.as_deref().is_none_or(expr_reads_are_repeatable)
        }
        StrExpr::Insert(a, b, n) => {
            str_reads_are_repeatable(a)
                && str_reads_are_repeatable(b)
                && expr_reads_are_repeatable(n)
        }
    }
}

pub(crate) fn expr_is_pure(e: &Expr) -> bool {
    match e {
        Expr::Number(n) => n.is_finite(),
        // READST for RS-232 returns $0297 and clears it, so a discarded `X=ST`
        // is a discarded side effect. A discarded `TI` read is not.
        Expr::Var(v) if v.kind == VarKind::Float && v.base == "ST" => false,
        Expr::Var(_) => true,
        Expr::String(_) => true,
        Expr::Neg(inner) | Expr::Not(inner) => expr_is_pure(inner),

        Expr::Bin(op, l, r) => {
            matches!(
                op,
                BinOp::Add
                    | BinOp::Sub
                    | BinOp::Mul
                    | BinOp::Eq
                    | BinOp::Ne
                    | BinOp::Lt
                    | BinOp::Le
                    | BinOp::Gt
                    | BinOp::Ge
                    | BinOp::And
                    | BinOp::Or
                    | BinOp::Xor
            ) && expr_is_pure(l)
                && expr_is_pure(r)
        }
        // Most one-arg numeric funcs are pure on their domain — but
        // SQR/LOG/ATN can raise on negative/zero args, RND modifies
        // the RNG stream (observable through later RND calls).
        Expr::Func1(f, arg) => match f {
            Func1::Abs | Func1::Int | Func1::Sgn => expr_is_pure(arg),
            // Bail on the rest: domain errors or stream effects.
            _ => false,
        },
        // PEEK is impure when the address is in I/O space ($D000–$DFFF):
        // many VIC-II / CIA registers are read-clear or read-affecting
        // (e.g. $D01E/$D01F sprite collision latches, $D019 raster IRQ
        // ack, $DC0D/$DD0D CIA IRQ, CIA timer counters). Dropping such a
        // PEEK silently changes hardware state.
        // Only allow DCE when we can prove the address is a literal
        // outside I/O space.
        Expr::Peek(addr) | Expr::MemPeek(addr) => {
            if !expr_is_pure(addr) {
                return false;
            }
            match peek_const_addr(addr) {
                Some(a) => !(0xD000..=0xDFFF).contains(&a),
                None => false,
            }
        }
        // ASC raises ?ILLEGAL QUANTITY for empty string; LEN is pure.
        Expr::Asc(_) => false,
        Expr::Len(s) => str_is_pure(s),
        // VAL parses a string — runtime behaviour depends on the
        // bytes, including possible parse errors. Bail.
        Expr::Val(_) | Expr::Nrm(_) => false,
        // POS reads cursor column — pure.
        Expr::Pos(_) => true,
        // FRE invokes string GC, which has observable timing
        // (Rnd-style stream effect on the heap layout but not on
        // any user-visible value). Conservatively impure.
        Expr::Fre(_) => false,
        // USR jumps to user-supplied native code — opaque.
        Expr::Usr(_) => false,
        Expr::Joy(_) => false,
        Expr::Pot(_) => false,
        Expr::At(_, _) => false,
        Expr::Test(_, _) => false,
        Expr::Check { .. } => false,
        Expr::Inst { .. } => false,
        Expr::Inkey => false,
        Expr::Lin => false,
        // FN body is opaque to us; assume side-effecting.
        Expr::FnCall(_, _) => false,
        // Array access can raise ?BAD SUBSCRIPT.
        Expr::ArrayRef(_, _) => false,
        // String compare reads memory and runs the strcmp helper —
        // pure as long as the operands are pure.
        Expr::StrCompare(_, l, r) => str_is_pure(l) && str_is_pure(r),
    }
}

/// Best-effort constant fold of a PEEK address. Recognises bare
/// numbers and the `Number ± Number` shape that ConstantFold leaves
/// behind for things like `V+31` where V was already substituted.
/// Returns `None` for anything that depends on a runtime variable —
/// the caller treats that as "could be I/O" and bails on DCE.
fn peek_const_addr(e: &Expr) -> Option<u16> {
    let v = peek_const_addr_f64(e)?;
    if !v.is_finite() {
        return None;
    }
    let i = v as i64;
    if (0..=0xFFFF).contains(&i) {
        Some(i as u16)
    } else {
        None
    }
}

fn peek_const_addr_f64(e: &Expr) -> Option<f64> {
    match e {
        Expr::Number(n) => Some(*n),
        Expr::Neg(inner) => peek_const_addr_f64(inner).map(|v| -v),
        Expr::Bin(op, l, r) => {
            let lv = peek_const_addr_f64(l)?;
            let rv = peek_const_addr_f64(r)?;
            match op {
                BinOp::Add => Some(lv + rv),
                BinOp::Sub => Some(lv - rv),
                BinOp::Mul => Some(lv * rv),
                _ => None,
            }
        }
        _ => None,
    }
}

fn str_is_pure(s: &StrExpr) -> bool {
    match s {
        StrExpr::Literal(_) | StrExpr::Var(_) => true,
        // CHR$/STR$ allocate from the string heap (observable via
        // FRE) — bail.
        StrExpr::Chr(_) | StrExpr::Str(_) | StrExpr::HexFmt(_) | StrExpr::BinFmt(_) => false,
        StrExpr::Concat(_, _) => false,
        StrExpr::Left(_, _)
        | StrExpr::Right(_, _)
        | StrExpr::Mid(_, _, _)
        | StrExpr::Dup(_, _)
        | StrExpr::Insert(_, _, _) => false,
        StrExpr::ArrayRef(_, _) => false,
        // GET-key advances the keyboard buffer — observable.
        StrExpr::GetKey => false,
    }
}

/// Looser variant of [`str_is_pure`] used by `DeadStoreElim`. When the
/// program never calls `FRE`, heap allocation is unobservable by user
/// code: the only effect of allocating a string chunk is that the heap
/// pointer advances and (later) GC fires. Dropping a dead chunk just
/// pushes both effects further into the future — no semantic change.
///
/// `GetKey` and `ArrayRef` are still rejected because their side
/// effects (advancing the keyboard buffer, possibly raising ?BAD
/// SUBSCRIPT) are observable regardless of FRE.
fn str_is_pure_for_dead_store(s: &StrExpr, fre_observable: bool) -> bool {
    // Volatile string reads (`TI$`) move FRE even if the store is unused.
    if fre_observable && !str_reads_are_repeatable(s) {
        return false;
    }
    if str_is_pure(s) {
        return true;
    }
    if fre_observable {
        return false;
    }
    match s {
        StrExpr::Chr(_) | StrExpr::Str(_) | StrExpr::HexFmt(_) | StrExpr::BinFmt(_) => true,
        StrExpr::Concat(l, r) => {
            str_is_pure_for_dead_store(l, fre_observable)
                && str_is_pure_for_dead_store(r, fre_observable)
        }
        StrExpr::Left(s, _) | StrExpr::Right(s, _) | StrExpr::Dup(s, _) => {
            str_is_pure_for_dead_store(s, fre_observable)
        }
        StrExpr::Mid(s, _, _) => str_is_pure_for_dead_store(s, fre_observable),
        StrExpr::Insert(a, b, _) => {
            str_is_pure_for_dead_store(a, fre_observable)
                && str_is_pure_for_dead_store(b, fre_observable)
        }
        StrExpr::ArrayRef(_, _) | StrExpr::GetKey => false,
        _ => false,
    }
}

/// True iff any expression in `module` invokes `FRE`. Used by
/// `DeadStoreElim` to decide whether heap-allocating string RHSs
/// have observable side effects.
fn module_uses_fre(module: &ir::Module) -> bool {
    struct FreFinder {
        found: bool,
    }
    impl crate::visit::Visitor for FreFinder {
        fn visit_expr(&mut self, e: &Expr) {
            if self.found {
                return;
            }
            if matches!(e, Expr::Fre(_)) {
                self.found = true;
                return;
            }
            crate::visit::walk_expr(self, e);
        }
    }
    let mut f = FreFinder { found: false };
    crate::visit::walk_module(&mut f, module);
    f.found
}

pub(crate) fn is_str_self_assign(var: &VarName, value: &StrExpr) -> bool {
    matches!(value, StrExpr::Var(_)) && var.kind == VarKind::String
}

/// True iff the module reads BASIC's FRETOP pair through a `PEEK` with a literal address ; `PEEK(51)` or `PEEK(52)`.
pub(crate) fn module_reads_fretop(module: &ir::Module) -> bool {
    struct FretopFinder {
        found: bool,
    }
    impl crate::visit::Visitor for FretopFinder {
        fn visit_expr(&mut self, e: &Expr) {
            if self.found {
                return;
            }
            if let Expr::Peek(arg) = e
                && let Expr::Number(n) = &**arg
                && (*n == f64::from(crate::runtime::FRETOP_LO)
                    || *n == f64::from(crate::runtime::FRETOP_HI))
            {
                self.found = true;
                return;
            }
            crate::visit::walk_expr(self, e);
        }
    }
    let mut f = FretopFinder { found: false };
    crate::visit::walk_module(&mut f, module);
    f.found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{BinOp, VarKind, VarName};
    use crate::ir::Pass;
    use crate::ir::{ArrayInductionIndex, Expr, Line, Module, Stmt, ThenIr};

    fn guarded_bounds_from_source(source: &str) -> HashMap<VarName, PromRange> {
        let bytes = crate::tokenize_program(source).unwrap();
        let prg = crate::prg::Program::parse(&bytes).unwrap();
        let ast = crate::parse::program(&prg).unwrap();
        guarded_counter_bounds(&crate::ir::lower(&ast).unwrap())
    }

    #[test]
    fn guarded_counter_bounds_prove_both_bubble_inductions() {
        let bounds = guarded_bounds_from_source(
            "10 M=240\n20 M=M-1:IF M=0 THEN 80\n30 X=-1\n40 X=X+1:IF X=M THEN 20\n50 POKE 1024+X,1\n60 GOTO 40\n80 END\n",
        );
        let m = bounds.get(&fvar("M")).expect("down-counter bound");
        let x = bounds.get(&fvar("X")).expect("inner up-counter bound");
        assert_eq!((m.min, m.max), (0, 240));
        assert_eq!((x.min, x.max), (-1, 240));
    }

    #[test]
    fn guarded_counter_bounds_reject_equality_step_over_and_wrong_side() {
        for (init, step) in [("M=240:X=1", 2), ("M=10:X=20", 1)] {
            let source = format!("10 {init}\n20 X=X+{step}:IF X=M THEN 40\n25 IF X>300 THEN 40\n30 GOTO 20\n40 PRINT X\n");
            let bounds = guarded_bounds_from_source(&source);
            assert!(!bounds.contains_key(&fvar("X")), "{source}: {bounds:?}");
        }
    }

    #[test]
    fn fold_refuses_results_outside_the_mflpt_range() {
        assert_eq!(fold_bin(BinOp::Mul, 1e38, 1e38), None);
        assert_eq!(fold_bin(BinOp::Add, 1.7e38, 1.7e38), None);
        assert!(fold_bin(BinOp::Mul, 1e19, 1e19).is_some());
    }

    /// The ROM keeps a 32-bit mantissa and rounds after every op, so `0.1+0.2` and `0.3` really
    /// are the same value on a C64.
    #[test]
    fn fold_rounds_to_c64_precision_after_each_operation() {
        assert_eq!(fold_bin(BinOp::Sub, 0.1 + 0.2, 0.3), Some(0.0));
        let sum = fold_bin(BinOp::Add, 0.1, 0.2).unwrap();
        assert_eq!(fold_bin(BinOp::Eq, sum, 0.3), Some(-1.0));
        // (2^32 + 1) - 2^32 is 2 on a C64: the sum rounds up.
        let inc = fold_bin(BinOp::Add, 4294967296.0, 1.0).unwrap();
        assert_eq!(fold_bin(BinOp::Sub, inc, 4294967296.0), Some(2.0));
    }

    /// AND/OR/XOR narrow through AYINT, which floors and rejects out-of-range operands.
    #[test]
    fn fold_logic_floors_and_declines_out_of_range() {
        assert_eq!(fold_bin(BinOp::And, -1.5, 3.0), Some(2.0));
        assert_eq!(fold_bin(BinOp::And, 40000.0, 1.0), None);
        assert_eq!(fold_bin(BinOp::Or, 4.0, 1.0), Some(5.0));
    }

    /// Raising a BASIC error is observable, so a dead store whose RHS can raise one must
    /// survive DCE.
    #[test]
    fn trapping_expressions_are_not_droppable() {
        let a = Expr::Var(fvar("A"));
        let mul = Expr::Bin(BinOp::Mul, Box::new(a.clone()), Box::new(a.clone()));
        assert!(expr_is_pure(&mul), "purity is unchanged");
        assert!(!expr_cannot_trap(&mul), "FMULT can raise ?OVERFLOW");
        // Reads and comparisons stay droppable.
        assert!(expr_cannot_trap(&a));
        assert!(expr_cannot_trap(&Expr::Bin(
            BinOp::Lt,
            Box::new(a.clone()),
            Box::new(Expr::Number(3.0))
        )));
        // AND narrows through AYINT, which can raise ?ILLEGAL QUANTITY.
        assert!(!expr_cannot_trap(&Expr::Bin(
            BinOp::And,
            Box::new(a),
            Box::new(Expr::Number(3.0))
        )));
    }

    /// `str_reads_are_repeatable` refuses `TI$`, including nested string ops.
    #[test]
    fn a_volatile_string_read_is_not_repeatable() {
        let ti = VarName {
            base: "TI".to_string(),
            kind: VarKind::String,
        };
        let a = VarName {
            base: "A".to_string(),
            kind: VarKind::String,
        };

        // Name test: string-typed TI is volatile, ordinary A$ is not.
        assert!(var_is_volatile(&ti));
        assert!(!var_is_volatile(&a));

        assert!(!str_reads_are_repeatable(&StrExpr::Var(ti.clone())));
        assert!(str_reads_are_repeatable(&StrExpr::Var(a.clone())));
        assert!(str_reads_are_repeatable(&StrExpr::Literal(b"HI".to_vec())));

        // GET advances the keyboard buffer, so the next read is a different key.
        assert!(!str_reads_are_repeatable(&StrExpr::GetKey));

        // Nested uses of `TI$` are still not repeatable.
        for nested in [
            StrExpr::Left(
                Box::new(StrExpr::Var(ti.clone())),
                Box::new(Expr::Number(2.0)),
            ),
            StrExpr::Concat(
                Box::new(StrExpr::Literal(b"T=".to_vec())),
                Box::new(StrExpr::Var(ti.clone())),
            ),
            StrExpr::Mid(
                Box::new(StrExpr::Var(ti.clone())),
                Box::new(Expr::Number(3.0)),
                Some(Box::new(Expr::Number(2.0))),
            ),
        ] {
            assert!(
                !str_reads_are_repeatable(&nested),
                "a volatile read stays volatile inside {nested:?}"
            );
        }

        // The same shapes on an ordinary string variable are repeatable.
        assert!(str_reads_are_repeatable(&StrExpr::Left(
            Box::new(StrExpr::Var(a.clone())),
            Box::new(Expr::Number(2.0))
        )));
        assert!(str_reads_are_repeatable(&StrExpr::Concat(
            Box::new(StrExpr::Literal(b"T=".to_vec())),
            Box::new(StrExpr::Var(a.clone()))
        )));

        // A numeric argument can be volatile too.
        let ti_num = VarName {
            base: "TI".to_string(),
            kind: VarKind::Float,
        };
        assert!(!str_reads_are_repeatable(&StrExpr::Str(Box::new(
            Expr::Var(ti_num)
        ))));
        assert!(str_reads_are_repeatable(&StrExpr::Str(Box::new(
            Expr::Number(1.0)
        ))));
    }

    /// LICM must refuse a `TI$` read, even if it starts hoisting other strings.
    #[test]
    fn licm_refuses_a_volatile_string_read_whatever_the_blanket_does() {
        let ti = VarName {
            base: "TI".to_string(),
            kind: VarKind::String,
        };
        for e in [
            Expr::Len(Box::new(StrExpr::Var(ti.clone()))),
            Expr::Asc(Box::new(StrExpr::Var(ti.clone()))),
            Expr::Val(Box::new(StrExpr::Var(ti.clone()))),
            Expr::StrCompare(
                crate::ast::BinOp::Eq,
                StrExpr::Var(ti.clone()),
                StrExpr::Literal(b"000000".to_vec()),
            ),
        ] {
            assert!(
                licm_expr_has_unsafe(&e),
                "a read of TI$ must never be hoistable: {e:?}"
            );
        }
    }

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

    fn array_kind_in(m: &Module, base: &str) -> Option<VarKind> {
        for line in &m.lines {
            for st in &line.stmts {
                match st {
                    Stmt::ArrayLet { name, .. } if name.base == base => return Some(name.kind),
                    Stmt::Dim(specs) => {
                        for s in specs {
                            if s.name.base == base {
                                return Some(s.name.kind);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        None
    }

    /// Find an array's type from writes, independent of DIM declarations.
    fn array_let_kind_in(m: &Module, base: &str) -> Option<VarKind> {
        for line in &m.lines {
            for st in &line.stmts {
                if let Stmt::ArrayLet { name, .. } = st
                    && name.base == base
                {
                    return Some(name.kind);
                }
            }
        }
        None
    }

    fn aref(base: &str, idx: f64) -> Expr {
        Expr::ArrayRef(fvar(base), vec![Expr::Number(idx)])
    }

    fn dim_line(n: u16, base: &str, size: f64) -> Line {
        Line {
            number: n,
            stmts: vec![Stmt::Dim(vec![crate::ir::DimSpec {
                name: fvar(base),
                dims: vec![Expr::Number(size)],
            }])],
        }
    }

    fn aset(n: u16, base: &str, idx: f64, value: Expr) -> Line {
        Line {
            number: n,
            stmts: vec![Stmt::ArrayLet {
                name: fvar(base),
                indices: vec![Expr::Number(idx)],
                value,
            }],
        }
    }

    #[test]
    fn int_array_promote_promotes_integer_only_array() {
        // DIM A(5): A(0)=3: A(1)=A(0)  — only int literals + self-copy.
        let mut m = Module {
            lines: vec![
                dim_line(10, "A", 5.0),
                aset(20, "A", 0.0, Expr::Number(3.0)),
                aset(30, "A", 1.0, aref("A", 0.0)),
            ],
        };
        IntArrayPromote.run(&mut m).unwrap();
        assert_eq!(array_kind_in(&m, "A"), Some(VarKind::Integer));
    }

    #[test]
    fn int_array_promote_skips_fractional_literal() {
        // A(0)=3.5 — must stay float or the value would be truncated.
        let mut m = Module {
            lines: vec![
                dim_line(10, "A", 5.0),
                aset(20, "A", 0.0, Expr::Number(3.5)),
            ],
        };
        IntArrayPromote.run(&mut m).unwrap();
        assert_eq!(array_kind_in(&m, "A"), Some(VarKind::Float));
    }

    #[test]
    fn int_array_promote_skips_arithmetic_rhs() {
        // A(0)=A(1)+A(2) — arithmetic could overflow i16; stay float.
        let mut m = Module {
            lines: vec![
                dim_line(10, "A", 5.0),
                aset(
                    20,
                    "A",
                    0.0,
                    Expr::Bin(
                        BinOp::Add,
                        Box::new(aref("A", 1.0)),
                        Box::new(aref("A", 2.0)),
                    ),
                ),
            ],
        };
        IntArrayPromote.run(&mut m).unwrap();
        assert_eq!(array_kind_in(&m, "A"), Some(VarKind::Float));
    }

    #[test]
    fn int_array_promote_skips_read_of_fractional_data() {
        // READ A(0) with fractional DATA — the int store would truncate.
        let mut m = Module {
            lines: vec![
                dim_line(10, "A", 2.0),
                Line {
                    number: 20,
                    stmts: vec![Stmt::Read(vec![crate::ir::ReadTarget::Array {
                        name: fvar("A"),
                        indices: vec![Expr::Number(0.0)],
                    }])],
                },
                Line {
                    number: 30,
                    stmts: vec![Stmt::Data(vec![crate::ast::DataValue::Float(1.5)])],
                },
            ],
        };
        IntArrayPromote.run(&mut m).unwrap();
        assert_eq!(array_kind_in(&m, "A"), Some(VarKind::Float));
    }

    #[test]
    fn int_array_promote_promotes_read_of_integer_data() {
        // READ A(0) with all-integer DATA — safe to promote.
        let mut m = Module {
            lines: vec![
                dim_line(10, "A", 2.0),
                Line {
                    number: 20,
                    stmts: vec![Stmt::Read(vec![crate::ir::ReadTarget::Array {
                        name: fvar("A"),
                        indices: vec![Expr::Number(0.0)],
                    }])],
                },
                Line {
                    number: 30,
                    stmts: vec![Stmt::Data(vec![
                        crate::ast::DataValue::Float(5.0),
                        crate::ast::DataValue::Float(8.0),
                    ])],
                },
            ],
        };
        IntArrayPromote.run(&mut m).unwrap();
        assert_eq!(array_kind_in(&m, "A"), Some(VarKind::Integer));
    }

    #[test]
    fn int_array_promote_skips_array_with_percent_twin() {
        let mut m = Module {
            lines: vec![
                Line {
                    number: 10,
                    stmts: vec![Stmt::Dim(vec![crate::ir::DimSpec {
                        name: ivar("A"),
                        dims: vec![Expr::Number(2.0)],
                    }])],
                },
                aset(20, "A", 5.0, Expr::Number(1.0)),
            ],
        };
        IntArrayPromote.run(&mut m).unwrap();

        assert_eq!(array_let_kind_in(&m, "A"), Some(VarKind::Float));
    }

    #[test]
    fn int_array_promote_skips_array_with_fetch_percent_twin() {
        let mut m = Module {
            lines: vec![
                aset(10, "A", 0.0, Expr::Number(4.0)),
                Line {
                    number: 20,
                    stmts: vec![Stmt::Fetch {
                        control: StrExpr::Literal(b"N".to_vec()),
                        max_len: Expr::Number(2.0),
                        target: ivar("A"),
                        target_indices: vec![Expr::Number(0.0)],
                        force: None,
                        position: None,
                    }],
                },
            ],
        };
        IntArrayPromote.run(&mut m).unwrap();
        assert_eq!(array_kind_in(&m, "A"), Some(VarKind::Float));
    }

    #[test]
    fn int_array_promote_ignores_percent_scalar_twin() {
        let mut m = Module {
            lines: vec![
                Line {
                    number: 10,
                    stmts: vec![Stmt::Let {
                        var: ivar("A"),
                        value: Expr::Number(7.0),
                    }],
                },
                aset(20, "A", 0.0, Expr::Number(4.0)),
            ],
        };
        IntArrayPromote.run(&mut m).unwrap();
        assert_eq!(array_kind_in(&m, "A"), Some(VarKind::Integer));
    }

    #[test]
    fn structured_loop_markers_preserve_int_for_safety() {
        let i = fvar("I");
        let read_i = Expr::Var(i.clone());

        assert!(stmt_is_int_safe(&Stmt::Repeat, &i));
        assert!(stmt_is_int_safe(&Stmt::Loop, &i));
        assert!(stmt_is_int_safe(&Stmt::EndLoop, &i));
        assert!(stmt_is_int_safe(
            &Stmt::Until {
                cond: read_i.clone()
            },
            &i
        ));
        assert!(stmt_is_int_safe(
            &Stmt::ExitLoop {
                cond: Some(read_i.clone())
            },
            &i
        ));

        assert!(!stmt_loop_var_needs_fac(
            &Stmt::Until {
                cond: read_i.clone()
            },
            &i,
            None,
            ForLowering::Int,
        ));
        assert!(!stmt_loop_var_needs_fac(
            &Stmt::ExitLoop { cond: Some(read_i) },
            &i,
            None,
            ForLowering::Int,
        ));
    }

    #[test]
    fn computed_goto_keeps_for_counter_synced() {
        let i = fvar("I");
        let stmt = Stmt::ComputedGoto {
            target: Expr::Var(i.clone()),
        };

        assert!(stmt_is_int_safe(&stmt, &i));
        assert!(stmt_loop_var_needs_fac(&stmt, &i, None, ForLowering::Int));
    }

    fn add(l: Expr, r: Expr) -> Expr {
        Expr::Bin(BinOp::Add, Box::new(l), Box::new(r))
    }

    fn mul(l: Expr, r: Expr) -> Expr {
        Expr::Bin(BinOp::Mul, Box::new(l), Box::new(r))
    }

    fn cmp(op: BinOp, l: Expr, r: Expr) -> Expr {
        Expr::Bin(op, Box::new(l), Box::new(r))
    }

    #[test]
    fn array_ptr_detects_2d_loop_axis_with_const_axis() {
        let i = fvar("I");
        let a = fvar("A");
        let mut body = Vec::new();
        for _ in 0..5 {
            body.push(Stmt::ArrayLet {
                name: a.clone(),
                indices: vec![Expr::Var(i.clone()), Expr::Number(1.0)],
                value: Expr::Number(0.0),
            });
        }
        let mut module = Module {
            lines: vec![
                Line {
                    number: 10,
                    stmts: vec![Stmt::For {
                        var: i.clone(),
                        start: Expr::Number(0.0),
                        end: Expr::Number(9.0),
                        step: Expr::Number(1.0),
                        body_int_safe: true,
                        body_reads_loop_var: true,
                        induction_const: None,
                        array_inductions: Vec::new(),
                    }],
                },
                Line {
                    number: 20,
                    stmts: body,
                },
                Line {
                    number: 30,
                    stmts: vec![Stmt::Next {
                        vars: vec![Some(i)],
                    }],
                },
            ],
        };

        ArrayPtrInductionDetect.run(&mut module).unwrap();

        let Stmt::For {
            array_inductions, ..
        } = &module.lines[0].stmts[0]
        else {
            panic!("line 10 should still be FOR");
        };
        assert_eq!(array_inductions.len(), 1);
        assert_eq!(array_inductions[0].name, a);
        assert_eq!(
            array_inductions[0].indices,
            vec![ArrayInductionIndex::LoopVar, ArrayInductionIndex::Const(1)]
        );
    }

    #[test]
    fn array_ptr_detects_3d_loop_axis_with_const_axes() {
        let i = fvar("I");
        let a = fvar("A");
        let mut body = Vec::new();
        for _ in 0..5 {
            body.push(Stmt::ArrayLet {
                name: a.clone(),
                indices: vec![Expr::Var(i.clone()), Expr::Number(1.0), Expr::Number(2.0)],
                value: Expr::Number(0.0),
            });
        }
        let mut module = Module {
            lines: vec![
                Line {
                    number: 10,
                    stmts: vec![Stmt::For {
                        var: i.clone(),
                        start: Expr::Number(0.0),
                        end: Expr::Number(9.0),
                        step: Expr::Number(1.0),
                        body_int_safe: true,
                        body_reads_loop_var: true,
                        induction_const: None,
                        array_inductions: Vec::new(),
                    }],
                },
                Line {
                    number: 20,
                    stmts: body,
                },
                Line {
                    number: 30,
                    stmts: vec![Stmt::Next {
                        vars: vec![Some(i)],
                    }],
                },
            ],
        };

        ArrayPtrInductionDetect.run(&mut module).unwrap();

        let Stmt::For {
            array_inductions, ..
        } = &module.lines[0].stmts[0]
        else {
            panic!("line 10 should still be FOR");
        };
        assert_eq!(array_inductions.len(), 1);
        assert_eq!(array_inductions[0].name, a);
        assert_eq!(
            array_inductions[0].indices,
            vec![
                ArrayInductionIndex::LoopVar,
                ArrayInductionIndex::Const(1),
                ArrayInductionIndex::Const(2),
            ]
        );
    }

    #[test]
    fn identity_simplifier_drops_add_zero() {
        // x + 0 → x, with x being a Var sub-tree (the case
        // ConstVarProp synthesizes when k% = 0 is propagated).
        let mut module = Module {
            lines: vec![
                Line {
                    number: 10,
                    stmts: vec![Stmt::Let {
                        var: ivar("K"),
                        value: Expr::Number(0.0),
                    }],
                },
                Line {
                    number: 20,
                    stmts: vec![Stmt::Let {
                        var: ivar("B"),
                        value: Expr::Bin(
                            BinOp::Add,
                            Box::new(Expr::Var(ivar("A"))),
                            Box::new(Expr::Var(ivar("K"))),
                        ),
                    }],
                },
            ],
        };
        ConstantFold.run(&mut module).unwrap();
        run_const_var_prop(&mut module);
        ConstantFold.run(&mut module).unwrap();
        // After ConstVarProp + re-fold, the K reference becomes
        // a literal 0, and the algebraic-identity simplifier
        // collapses Add(Var(A), Number(0)) -> Var(A).
        let line20 = &module.lines.iter().find(|l| l.number == 20).unwrap();
        if let Stmt::Let { value, .. } = &line20.stmts[0] {
            assert!(
                matches!(value, Expr::Var(v) if v == &ivar("A")),
                "expected bare Var(A), got {value:?}"
            );
        } else {
            panic!("line 20 not a Let");
        }
    }

    #[test]
    fn identity_simplifier_drops_mul_one() {
        let mut e = Expr::Bin(
            BinOp::Mul,
            Box::new(Expr::Var(ivar("X"))),
            Box::new(Expr::Number(1.0)),
        );
        let simplified = try_simplify_identity(&e).unwrap();
        assert!(matches!(&simplified, Expr::Var(v) if v == &ivar("X")));
        e = Expr::Bin(
            BinOp::Mul,
            Box::new(Expr::Number(1.0)),
            Box::new(Expr::Var(ivar("X"))),
        );
        let simplified = try_simplify_identity(&e).unwrap();
        assert!(matches!(&simplified, Expr::Var(v) if v == &ivar("X")));
    }

    #[test]
    fn identity_simplifier_drops_xor_zero() {
        let e = Expr::Bin(
            BinOp::Xor,
            Box::new(Expr::Var(ivar("X"))),
            Box::new(Expr::Number(0.0)),
        );
        let simplified = try_simplify_identity(&e).unwrap();
        assert!(matches!(&simplified, Expr::Var(v) if v == &ivar("X")));
    }

    /// The bitwise identities are only identities on values AYINT leaves alone.
    #[test]
    fn identity_simplifier_keeps_bitwise_ops_on_unnarrowed_operands() {
        for (op, lit) in [(BinOp::And, -1.0), (BinOp::Or, 0.0), (BinOp::Xor, 0.0)] {
            let float_operand = Expr::Bin(
                op,
                Box::new(Expr::Var(fvar("X"))),
                Box::new(Expr::Number(lit)),
            );
            assert!(
                try_simplify_identity(&float_operand).is_none(),
                "{op:?} with a float var must keep its AYINT narrowing"
            );

            let float_operand_rhs = Expr::Bin(
                op,
                Box::new(Expr::Number(lit)),
                Box::new(Expr::Var(fvar("X"))),
            );
            assert!(
                try_simplify_identity(&float_operand_rhs).is_none(),
                "{op:?} with a float var on the right must keep its narrowing"
            );

            let out_of_range = Expr::Bin(
                op,
                Box::new(Expr::Number(40000.0)),
                Box::new(Expr::Number(lit)),
            );
            assert!(
                try_simplify_identity(&out_of_range).is_none(),
                "{op:?} with an out-of-range literal must not simplify"
            );
        }
    }

    #[test]
    fn identity_simplifier_still_drops_bitwise_ops_on_narrowed_operands() {
        for (op, lit) in [(BinOp::And, -1.0), (BinOp::Or, 0.0), (BinOp::Xor, 0.0)] {
            let e = Expr::Bin(
                op,
                Box::new(Expr::Var(ivar("X"))),
                Box::new(Expr::Number(lit)),
            );
            let simplified = try_simplify_identity(&e)
                .unwrap_or_else(|| panic!("{op:?} with a % var should simplify"));
            assert!(matches!(&simplified, Expr::Var(v) if v == &ivar("X")));
            // A comparison result is 0/-1, likewise narrow.
            let cmp = Expr::Bin(
                op,
                Box::new(Expr::Number(lit)),
                Box::new(Expr::Bin(
                    BinOp::Eq,
                    Box::new(Expr::Var(fvar("A"))),
                    Box::new(Expr::Number(1.0)),
                )),
            );
            assert!(
                try_simplify_identity(&cmp).is_some(),
                "{op:?} with a comparison operand should simplify"
            );
        }
    }

    #[test]
    fn identity_simplifier_handles_negative_literal() {
        // x * -1 → -x ; tests Neg(Number(1)) recognition path
        let e = Expr::Bin(
            BinOp::Mul,
            Box::new(Expr::Var(ivar("X"))),
            Box::new(Expr::Neg(Box::new(Expr::Number(1.0)))),
        );
        let simplified = try_simplify_identity(&e).unwrap();
        assert!(matches!(&simplified, Expr::Neg(_)));
    }

    #[test]
    fn proc_inline_single_call_inlines_body() {
        // Single EXEC call: cost model says always inline.
        // Body becomes part of the caller's line; PROC body lines
        // are emptied to Rem.
        use crate::ast::{
            Expr as AstExpr, Line as AstLine, ProcName, Program as AstProgram,
            Statement as AstStmt, VarKind as AstKind, VarName as AstVar,
        };
        let mut prog = AstProgram {
            skipped: Vec::new(),
            traps: Vec::new(),
            deferred_fn_bodies: Vec::new(),
            lines: vec![
                AstLine {
                    number: 10,
                    statements: vec![AstStmt::ProcCall(ProcName(b"FOO".to_vec()))],
                },
                AstLine {
                    number: 20,
                    statements: vec![AstStmt::End],
                },
                AstLine {
                    number: 30,
                    statements: vec![AstStmt::ProcDef(ProcName(b"FOO".to_vec()))],
                },
                AstLine {
                    number: 40,
                    statements: vec![AstStmt::Let {
                        name: AstVar {
                            base: "X".to_string(),
                            kind: AstKind::Integer,
                        },
                        value: AstExpr::Number(5.0),
                    }],
                },
                AstLine {
                    number: 50,
                    statements: vec![AstStmt::EndProc],
                },
            ],
        };
        crate::passes::inline_procs_ast(&mut prog);
        // Line 10 should now contain the inlined Let (was ProcCall).
        let line10 = &prog.lines[0];
        assert_eq!(line10.number, 10);
        assert!(
            matches!(&line10.statements[0], AstStmt::Let { .. }),
            "expected inlined Let at line 10, got {:?}",
            line10.statements[0]
        );
        // PROC body lines should be empty Rem.
        let line30 = prog.lines.iter().find(|l| l.number == 30).unwrap();
        assert!(matches!(&line30.statements[0], AstStmt::Rem(_)));
        let line40 = prog.lines.iter().find(|l| l.number == 40).unwrap();
        assert!(matches!(&line40.statements[0], AstStmt::Rem(_)));
    }

    #[test]
    fn proc_inline_skips_when_too_many_callers() {
        // 11 callers + body of 1 stmt: cost model says skip
        // (11+ never inlines under user's policy).
        use crate::ast::{
            Expr as AstExpr, Line as AstLine, ProcName, Program as AstProgram,
            Statement as AstStmt, VarKind as AstKind, VarName as AstVar,
        };
        let mut lines = Vec::new();
        for n in 1..=11 {
            lines.push(AstLine {
                number: n * 10,
                statements: vec![AstStmt::ProcCall(ProcName(b"INC".to_vec()))],
            });
        }
        lines.push(AstLine {
            number: 200,
            statements: vec![AstStmt::End],
        });
        lines.push(AstLine {
            number: 210,
            statements: vec![AstStmt::ProcDef(ProcName(b"INC".to_vec()))],
        });
        lines.push(AstLine {
            number: 220,
            statements: vec![AstStmt::Let {
                name: AstVar {
                    base: "X".to_string(),
                    kind: AstKind::Integer,
                },
                value: AstExpr::Number(0.0),
            }],
        });
        lines.push(AstLine {
            number: 230,
            statements: vec![AstStmt::EndProc],
        });
        let mut prog = AstProgram {
            lines,
            skipped: Vec::new(),
            traps: Vec::new(),
            deferred_fn_bodies: Vec::new(),
        };
        crate::passes::inline_procs_ast(&mut prog);
        // None of the call sites should have been inlined.
        let line10 = &prog.lines[0];
        assert!(
            matches!(&line10.statements[0], AstStmt::ProcCall(_)),
            "expected ProcCall preserved, got {:?}",
            line10.statements[0]
        );
        // PROC body lines should still be intact.
        let line210 = prog.lines.iter().find(|l| l.number == 210).unwrap();
        assert!(matches!(&line210.statements[0], AstStmt::ProcDef(_)));
    }

    #[test]
    fn localize_renames_local_var_inside_proc_body() {
        // PROC FOO with `LOCAL X` should mangle every reference to X
        // inside the body to `FOO__X`. References outside the body
        // stay untouched.
        use crate::ast::{
            Expr as AstExpr, Line as AstLine, ProcName, Program as AstProgram,
            Statement as AstStmt, VarKind as AstKind, VarName as AstVar,
        };
        let xfloat = AstVar {
            base: "X".to_string(),
            kind: AstKind::Float,
        };
        let mut prog = AstProgram {
            skipped: Vec::new(),
            traps: Vec::new(),
            deferred_fn_bodies: Vec::new(),
            lines: vec![
                // Outside body — must NOT be mangled.
                AstLine {
                    number: 10,
                    statements: vec![AstStmt::Let {
                        name: xfloat.clone(),
                        value: AstExpr::Number(1.0),
                    }],
                },
                AstLine {
                    number: 20,
                    statements: vec![AstStmt::ProcDef(ProcName(b"FOO".to_vec()))],
                },
                AstLine {
                    number: 30,
                    statements: vec![AstStmt::Local {
                        vars: vec![xfloat.clone()],
                    }],
                },
                AstLine {
                    number: 40,
                    statements: vec![AstStmt::Let {
                        name: xfloat.clone(),
                        value: AstExpr::Var(xfloat.clone()),
                    }],
                },
                AstLine {
                    number: 50,
                    statements: vec![AstStmt::EndProc],
                },
            ],
        };
        crate::passes::localize_proc_vars(&mut prog);

        let outside = match &prog.lines[0].statements[0] {
            AstStmt::Let { name, .. } => name.clone(),
            other => panic!("expected Let outside body, got {other:?}"),
        };
        assert_eq!(outside.base, "X", "outside-body var must NOT be mangled");

        let inside = prog.lines.iter().find(|l| l.number == 40).unwrap();
        let (lhs, rhs) = match &inside.statements[0] {
            AstStmt::Let {
                name,
                value: AstExpr::Var(rhs),
            } => (name.clone(), rhs.clone()),
            other => panic!("expected Let inside body, got {other:?}"),
        };
        assert_eq!(lhs.base, "FOO__X", "LHS must be mangled to FOO__X");
        assert_eq!(rhs.base, "FOO__X", "RHS Var ref must also be mangled");
    }

    #[test]
    fn localize_global_overrides_local_in_same_body() {
        // `LOCAL Y: GLOBAL Y` strips Y from the local set so it stays
        // shared with the caller.
        use crate::ast::{
            Expr as AstExpr, Line as AstLine, ProcName, Program as AstProgram,
            Statement as AstStmt, VarKind as AstKind, VarName as AstVar,
        };
        let y = AstVar {
            base: "Y".to_string(),
            kind: AstKind::Float,
        };
        let mut prog = AstProgram {
            skipped: Vec::new(),
            traps: Vec::new(),
            deferred_fn_bodies: Vec::new(),
            lines: vec![
                AstLine {
                    number: 10,
                    statements: vec![AstStmt::ProcDef(ProcName(b"BAR".to_vec()))],
                },
                AstLine {
                    number: 20,
                    statements: vec![
                        AstStmt::Local {
                            vars: vec![y.clone()],
                        },
                        AstStmt::Global {
                            vars: vec![y.clone()],
                        },
                    ],
                },
                AstLine {
                    number: 30,
                    statements: vec![AstStmt::Let {
                        name: y.clone(),
                        value: AstExpr::Number(7.0),
                    }],
                },
                AstLine {
                    number: 40,
                    statements: vec![AstStmt::EndProc],
                },
            ],
        };
        crate::passes::localize_proc_vars(&mut prog);

        let assign = prog.lines.iter().find(|l| l.number == 30).unwrap();
        let lhs = match &assign.statements[0] {
            AstStmt::Let { name, .. } => name.clone(),
            other => panic!("expected Let, got {other:?}"),
        };
        assert_eq!(lhs.base, "Y", "GLOBAL must strip Y from the local set");
    }

    #[test]
    fn proc_inline_rejects_body_with_goto() {
        // PROC body containing GOTO is non-inlinable (control flow
        // doesn't compose under multiple call sites).
        use crate::ast::{Line as AstLine, ProcName, Program as AstProgram, Statement as AstStmt};
        let mut prog = AstProgram {
            skipped: Vec::new(),
            traps: Vec::new(),
            deferred_fn_bodies: Vec::new(),
            lines: vec![
                AstLine {
                    number: 10,
                    statements: vec![AstStmt::ProcCall(ProcName(b"BAD".to_vec()))],
                },
                AstLine {
                    number: 20,
                    statements: vec![AstStmt::End],
                },
                AstLine {
                    number: 30,
                    statements: vec![AstStmt::ProcDef(ProcName(b"BAD".to_vec()))],
                },
                AstLine {
                    number: 40,
                    statements: vec![AstStmt::Goto(99)],
                },
                AstLine {
                    number: 50,
                    statements: vec![AstStmt::EndProc],
                },
                AstLine {
                    number: 99,
                    statements: vec![AstStmt::End],
                },
            ],
        };
        crate::passes::inline_procs_ast(&mut prog);
        // ProcCall preserved (not inlined because body has GOTO).
        let line10 = &prog.lines[0];
        assert!(
            matches!(&line10.statements[0], AstStmt::ProcCall(_)),
            "non-inlinable body should leave ProcCall in place"
        );
    }

    #[test]
    fn dead_line_elim_keeps_on_key_target_line() {
        // ON KEY "Q" GOTO 200 reaches line 200 only via the keyboard
        // trap — no static control-flow edge in the surrounding
        // code. collect_jump_targets must mark it reachable so
        // DeadLineElim doesn't drop it.
        let mut module = Module {
            lines: vec![
                Line {
                    number: 10,
                    stmts: vec![Stmt::OnKey {
                        keys: ir::StrExpr::Literal(b"Q".to_vec()),
                        target: Some(crate::ast::OnKeyAction::Goto(200)),
                    }],
                },
                Line {
                    number: 20,
                    stmts: vec![Stmt::Goto { target: 20 }],
                },
                Line {
                    number: 200,
                    stmts: vec![Stmt::Print {
                        items: vec![PrintPiece::LiteralString(b"GOT".to_vec())],
                        newline: true,
                    }],
                },
            ],
        };
        DeadLineElim.run(&mut module).unwrap();
        assert!(
            module.lines.iter().any(|l| l.number == 200),
            "line 200 should survive — ON KEY ... GOTO 200 references it"
        );
    }

    #[test]
    fn dead_line_elim_keeps_run_target_line() {
        // `RUN <line>` is the only reference to line 100; without
        // collect_jump_targets recognising RUN, DeadLineElim would
        // drop line 100 and codegen's `JMP L100` would dangle at
        // assembly time.
        let mut module = Module {
            lines: vec![
                Line {
                    number: 10,
                    stmts: vec![Stmt::Run(Some(100))],
                },
                Line {
                    number: 20,
                    stmts: vec![Stmt::End],
                },
                Line {
                    number: 100,
                    stmts: vec![Stmt::Print {
                        items: vec![PrintPiece::LiteralString(b"HI".to_vec())],
                        newline: true,
                    }],
                },
            ],
        };
        DeadLineElim.run(&mut module).unwrap();
        assert!(
            module.lines.iter().any(|l| l.number == 100),
            "line 100 should survive — RUN 100 references it"
        );
    }

    #[test]
    fn if_condition_fold_false_goto_stops_target_from_staying_live() {
        let mut module = Module {
            lines: vec![
                Line {
                    number: 10,
                    stmts: vec![Stmt::If {
                        cond: Expr::Number(0.0),
                        then: ThenIr::Goto(100),
                    }],
                },
                Line {
                    number: 20,
                    stmts: vec![Stmt::End],
                },
                Line {
                    number: 100,
                    stmts: vec![Stmt::End],
                },
            ],
        };

        IfConditionFold.run(&mut module).unwrap();
        DeadLineElim.run(&mut module).unwrap();

        assert!(
            !module.lines.iter().any(|l| l.number == 100),
            "line 100 should be removable once IF 0 THEN GOTO 100 folds away"
        );
    }

    #[test]
    fn if_condition_fold_true_goto_exposes_same_line_dead_code() {
        let mut module = Module {
            lines: vec![
                Line {
                    number: 10,
                    stmts: vec![
                        Stmt::If {
                            cond: Expr::Number(-1.0),
                            then: ThenIr::Goto(100),
                        },
                        Stmt::Let {
                            var: fvar("A"),
                            value: Expr::Number(1.0),
                        },
                    ],
                },
                Line {
                    number: 100,
                    stmts: vec![Stmt::End],
                },
            ],
        };

        IfConditionFold.run(&mut module).unwrap();
        DeadCodeAfterTransfer.run(&mut module).unwrap();

        assert!(matches!(
            module.lines[0].stmts.as_slice(),
            [Stmt::Goto { target: 100 }]
        ));
    }

    #[test]
    fn if_condition_fold_skips_rcomp_modules() {
        let mut module = Module {
            lines: vec![Line {
                number: 10,
                stmts: vec![
                    Stmt::If {
                        cond: Expr::Number(0.0),
                        then: ThenIr::Goto(100),
                    },
                    Stmt::Rcomp {
                        then: ThenIr::Goto(200),
                        else_then: None,
                    },
                ],
            }],
        };

        IfConditionFold.run(&mut module).unwrap();

        assert!(
            matches!(module.lines[0].stmts[0], Stmt::If { .. }),
            "RCOMP observes __LAST_IF, so constant IFs must stay structural"
        );
    }

    #[test]
    fn goto_chain_fold_redirects_run_target() {
        // RUN 100 + 100 GOTO 200 should fold to RUN 200.
        let mut module = Module {
            lines: vec![
                Line {
                    number: 10,
                    stmts: vec![Stmt::Run(Some(100))],
                },
                Line {
                    number: 100,
                    stmts: vec![Stmt::Goto { target: 200 }],
                },
                Line {
                    number: 200,
                    stmts: vec![Stmt::End],
                },
            ],
        };
        GotoChainFold.run(&mut module).unwrap();
        match &module.lines[0].stmts[0] {
            Stmt::Run(Some(target)) => assert_eq!(*target, 200),
            other => panic!("expected RUN target folded to 200, got {other:?}"),
        }
    }

    /// A trampoline whose own target does not exist is not folded through: the `?UNDEF'D STATEMENT` belongs to the trampoline line.
    #[test]
    fn goto_chain_fold_stops_at_a_chain_that_ends_nowhere() {
        let mut module = Module {
            lines: vec![
                Line {
                    number: 30,
                    stmts: vec![Stmt::OnBranch {
                        value: Expr::Number(1.0),
                        kind: crate::ast::OnBranchKind::Goto,
                        targets: vec![6, 35],
                    }],
                },
                Line {
                    number: 35,
                    stmts: vec![Stmt::Goto { target: 999 }],
                },
            ],
        };
        GotoChainFold.run(&mut module).unwrap();
        match &module.lines[0].stmts[0] {
            Stmt::OnBranch { targets, .. } => assert_eq!(
                targets,
                &[6, 35],
                "a GOTO to a line that does not exist is a trap on line 35, \
                 not a jump the caller may absorb"
            ),
            other => panic!("expected ON GOTO, got {other:?}"),
        }
    }

    #[test]
    fn goto_chain_fold_still_folds_a_chain_that_lands_somewhere() {
        let mut module = Module {
            lines: vec![
                Line {
                    number: 30,
                    stmts: vec![Stmt::OnBranch {
                        value: Expr::Number(1.0),
                        kind: crate::ast::OnBranchKind::Goto,
                        targets: vec![35],
                    }],
                },
                Line {
                    number: 35,
                    stmts: vec![Stmt::Goto { target: 40 }],
                },
                Line {
                    number: 40,
                    stmts: vec![Stmt::Goto { target: 50 }],
                },
                Line {
                    number: 50,
                    stmts: vec![Stmt::End],
                },
            ],
        };
        GotoChainFold.run(&mut module).unwrap();
        match &module.lines[0].stmts[0] {
            Stmt::OnBranch { targets, .. } => assert_eq!(targets, &[50]),
            other => panic!("expected ON GOTO, got {other:?}"),
        }
    }

    #[test]
    fn gosub_single_use_inlines_isolated_return_tail() {
        let a = fvar("A");
        let mut module = Module {
            lines: vec![
                Line {
                    number: 10,
                    stmts: vec![Stmt::GoSub { target: 100 }, Stmt::Goto { target: 200 }],
                },
                Line {
                    number: 100,
                    stmts: vec![
                        Stmt::Let {
                            var: a.clone(),
                            value: Expr::Number(7.0),
                        },
                        Stmt::Return,
                    ],
                },
                Line {
                    number: 200,
                    stmts: vec![Stmt::End],
                },
            ],
        };

        GosubSingleUseInline.run(&mut module).unwrap();

        assert!(matches!(module.lines[0].stmts[0], Stmt::LineMark(100)));
        assert!(matches!(module.lines[0].stmts[1], Stmt::Let { .. }));
        assert!(matches!(module.lines[0].stmts[2], Stmt::LineMark(10)));
        assert!(matches!(
            module.lines[0].stmts[3],
            Stmt::Goto { target: 200 }
        ));
        assert!(
            !module.lines[0]
                .stmts
                .iter()
                .any(|s| matches!(s, Stmt::GoSub { .. }))
        );
    }

    #[test]
    fn gosub_short_body_inlines_at_multiple_call_sites() {
        // Two callers + a one-statement body: each `GOSUB 100` should
        // be replaced by the body in place; line 100 has nothing
        // that still references it as a fall-through entry, so
        // future DeadLineElim can drop it.
        let a = fvar("A");
        let mut module = Module {
            lines: vec![
                Line {
                    number: 10,
                    stmts: vec![Stmt::GoSub { target: 100 }, Stmt::End],
                },
                Line {
                    number: 20,
                    stmts: vec![Stmt::GoSub { target: 100 }, Stmt::End],
                },
                Line {
                    number: 100,
                    stmts: vec![
                        Stmt::Let {
                            var: a.clone(),
                            value: Expr::Number(7.0),
                        },
                        Stmt::Return,
                    ],
                },
            ],
        };

        GosubShortBodyInline.run(&mut module).unwrap();

        for (idx, line_no) in [(0usize, 10), (1, 20)] {
            assert_eq!(module.lines[idx].number, line_no);
            assert!(
                matches!(module.lines[idx].stmts[0], Stmt::LineMark(100)),
                "line {line_no} should open the inlined body with a mark:\n{:#?}",
                module.lines[idx].stmts
            );
            assert!(
                matches!(module.lines[idx].stmts[1], Stmt::Let { .. }),
                "line {line_no} should be `LET a=7; END` after inline:\n{:#?}",
                module.lines[idx].stmts
            );
            assert!(matches!(module.lines[idx].stmts[2], Stmt::LineMark(n) if n == line_no));
            assert!(matches!(module.lines[idx].stmts[3], Stmt::End));
        }
    }

    #[test]
    fn gosub_short_body_skips_long_bodies() {
        // 3-stmt body exceeds SHORT_BODY_MAX_STMTS (2) — must NOT inline.
        // GOSUB call sites stay as GOSUB; the line stays.
        let a = fvar("A");
        let mut module = Module {
            lines: vec![
                Line {
                    number: 10,
                    stmts: vec![Stmt::GoSub { target: 100 }, Stmt::End],
                },
                Line {
                    number: 20,
                    stmts: vec![Stmt::GoSub { target: 100 }, Stmt::End],
                },
                Line {
                    number: 100,
                    stmts: vec![
                        Stmt::Let {
                            var: a.clone(),
                            value: Expr::Number(1.0),
                        },
                        Stmt::Let {
                            var: a.clone(),
                            value: Expr::Number(2.0),
                        },
                        Stmt::Let {
                            var: a.clone(),
                            value: Expr::Number(3.0),
                        },
                        Stmt::Return,
                    ],
                },
            ],
        };

        GosubShortBodyInline.run(&mut module).unwrap();

        assert!(matches!(
            module.lines[0].stmts[0],
            Stmt::GoSub { target: 100 }
        ));
        assert!(matches!(
            module.lines[1].stmts[0],
            Stmt::GoSub { target: 100 }
        ));
    }

    #[test]
    fn tail_gosub_rewrite_turns_gosub_return_into_goto() {
        let mut module = Module {
            lines: vec![Line {
                number: 100,
                stmts: vec![Stmt::GoSub { target: 200 }, Stmt::Return],
            }],
        };

        TailGosubRewrite.run(&mut module).unwrap();

        assert!(matches!(
            module.lines[0].stmts.as_slice(),
            [Stmt::Goto { target: 200 }]
        ));
    }

    #[test]
    fn tail_gosub_inlines_single_use_if_body() {
        let a = fvar("A");
        let mut module = Module {
            lines: vec![
                Line {
                    number: 10,
                    stmts: vec![
                        Stmt::If {
                            cond: Expr::Number(1.0),
                            then: ThenIr::Stmts(vec![Stmt::GoSub { target: 100 }, Stmt::Return]),
                        },
                        Stmt::End,
                    ],
                },
                Line {
                    number: 100,
                    stmts: vec![
                        Stmt::Let {
                            var: a,
                            value: Expr::Number(7.0),
                        },
                        Stmt::Return,
                    ],
                },
            ],
        };

        TailGosubRewrite.run(&mut module).unwrap();

        let Stmt::If {
            then: ThenIr::Stmts(inner),
            ..
        } = &module.lines[0].stmts[0]
        else {
            panic!("expected IF with stmt body");
        };
        assert!(matches!(inner.as_slice(), [Stmt::Let { .. }, Stmt::Return]));
    }

    #[test]
    fn local_const_prop_substitutes_within_line() {
        let a = fvar("A");
        let mut module = Module {
            lines: vec![Line {
                number: 10,
                stmts: vec![
                    Stmt::Let {
                        var: a.clone(),
                        value: Expr::Number(53280.0),
                    },
                    Stmt::Poke {
                        addr: Expr::Var(a),
                        value: Expr::Number(0.0),
                    },
                ],
            }],
        };

        LocalConstProp.run(&mut module).unwrap();

        let Stmt::Poke { addr, .. } = &module.lines[0].stmts[1] else {
            panic!("second stmt should be POKE");
        };
        assert_eq!(*addr, Expr::Number(53280.0));
    }

    #[test]
    fn local_const_prop_preserves_constants_across_known_poke() {
        let a = fvar("A");
        let mut module = Module {
            lines: vec![Line {
                number: 10,
                stmts: vec![
                    Stmt::Let {
                        var: a.clone(),
                        value: Expr::Number(1024.0),
                    },
                    Stmt::Poke {
                        addr: Expr::Number(53280.0),
                        value: Expr::Number(0.0),
                    },
                    Stmt::Poke {
                        addr: Expr::Var(a),
                        value: Expr::Number(1.0),
                    },
                ],
            }],
        };

        LocalConstProp.run(&mut module).unwrap();

        let Stmt::Poke { addr, .. } = &module.lines[0].stmts[2] else {
            panic!("third stmt should be POKE");
        };
        assert_eq!(*addr, Expr::Number(1024.0));
    }

    #[test]
    fn local_const_prop_kills_conditional_write() {
        let a = fvar("A");
        let x = fvar("X");
        let mut module = Module {
            lines: vec![Line {
                number: 10,
                stmts: vec![
                    Stmt::Let {
                        var: a.clone(),
                        value: Expr::Number(1.0),
                    },
                    Stmt::If {
                        cond: Expr::Var(x),
                        then: ThenIr::Stmts(vec![Stmt::Let {
                            var: a.clone(),
                            value: Expr::Number(2.0),
                        }]),
                    },
                    Stmt::Poke {
                        addr: Expr::Var(a.clone()),
                        value: Expr::Number(0.0),
                    },
                ],
            }],
        };

        LocalConstProp.run(&mut module).unwrap();

        let Stmt::Poke { addr, .. } = &module.lines[0].stmts[2] else {
            panic!("third stmt should be POKE");
        };
        assert_eq!(*addr, Expr::Var(a));
    }

    #[test]
    fn array_index_only_read_routes_through_int() {
        // A%(I) = 0 — I read only as int-routable index.
        let i = fvar("I");
        let a = ivar("A");
        let stmt = Stmt::ArrayLet {
            name: a.clone(),
            indices: vec![Expr::Var(i.clone())],
            value: Expr::Number(0.0),
        };
        assert!(!stmt_loop_var_needs_fac(&stmt, &i, None, ForLowering::Int));
    }

    #[test]
    fn leaf_squared_into_int_array_stays_int() {
        // A%(I) = I*I — both leaves are the FOR counter, MUL16 path.
        let i = fvar("I");
        let a = ivar("A");
        let stmt = Stmt::ArrayLet {
            name: a.clone(),
            indices: vec![Expr::Var(i.clone())],
            value: mul(Expr::Var(i.clone()), Expr::Var(i.clone())),
        };
        assert!(!stmt_loop_var_needs_fac(&stmt, &i, None, ForLowering::Int));
    }

    #[test]
    fn float_array_value_with_loop_var_needs_fac() {
        // A(I) = I + 1 where A is Float — value goes through FAC.
        let i = fvar("I");
        let a = fvar("A");
        let stmt = Stmt::ArrayLet {
            name: a.clone(),
            indices: vec![Expr::Var(i.clone())],
            value: add(Expr::Var(i.clone()), Expr::Number(1.0)),
        };
        assert!(stmt_loop_var_needs_fac(&stmt, &i, None, ForLowering::Int));
    }

    #[test]
    fn integer_compare_with_goto_then_forces_sync() {
        // IF I > 250 THEN GOTO 100 — even though the int-island routes
        // the compare without FAC, the GOTO can leave the FOR with
        // V_var stale (the post-loop code reads V_var). The
        // analysis now reports "needs FAC" so codegen keeps V_var
        // in sync at every iteration.
        let i = fvar("I");
        let stmt = Stmt::If {
            cond: cmp(BinOp::Gt, Expr::Var(i.clone()), Expr::Number(250.0)),
            then: ThenIr::Goto(100),
        };
        assert!(stmt_loop_var_needs_fac(&stmt, &i, None, ForLowering::Int));
    }

    #[test]
    fn float_compare_falls_back_to_fac() {
        // IF I < 1.5 THEN GOTO 100 — RHS isn't int-eligible.
        let i = fvar("I");
        let stmt = Stmt::If {
            cond: cmp(BinOp::Lt, Expr::Var(i.clone()), Expr::Number(1.5)),
            then: ThenIr::Goto(100),
        };
        assert!(stmt_loop_var_needs_fac(&stmt, &i, None, ForLowering::Int));
    }

    #[test]
    fn func1_argument_forces_fac() {
        // Y = SQR(I) — SQR routes through FAC.
        let i = fvar("I");
        let y = fvar("Y");
        let stmt = Stmt::Let {
            var: y.clone(),
            value: Expr::Func1(crate::ast::Func1::Sqr, Box::new(Expr::Var(i.clone()))),
        };
        assert!(stmt_loop_var_needs_fac(&stmt, &i, None, ForLowering::Int));
    }

    #[test]
    fn integer_let_with_addsub_stays_int() {
        // C% = C% + I — RHS is integer-typed, Add propagates int.
        let i = fvar("I");
        let c = ivar("C");
        let stmt = Stmt::Let {
            var: c.clone(),
            value: add(Expr::Var(c.clone()), Expr::Var(i.clone())),
        };
        assert!(!stmt_loop_var_needs_fac(&stmt, &i, None, ForLowering::Int));
    }

    #[test]
    fn poke_loop_var_routes_through_int() {
        // POKE 1024+I, 32 — both addr and value are int-context.
        let i = fvar("I");
        let stmt = Stmt::Poke {
            addr: add(Expr::Number(1024.0), Expr::Var(i.clone())),
            value: Expr::Number(32.0),
        };
        assert!(!stmt_loop_var_needs_fac(&stmt, &i, None, ForLowering::Int));
    }

    #[test]
    fn print_expr_of_float_loop_var_needs_fac() {
        // Float-FOR PRINT I still routes through FAC.
        let i = fvar("I");
        let stmt = Stmt::Print {
            items: vec![crate::ir::PrintPiece::Expr(Expr::Var(i.clone()))],
            newline: true,
        };
        assert!(stmt_loop_var_needs_fac(&stmt, &i, None, ForLowering::Float));
    }

    #[test]
    fn print_expr_of_u8_loop_var_routes_through_counter() {
        // u8/int-FOR PRINT I now uses __PRINT_INT16 directly from
        // FU_/FI_, so V_var does not need per-iteration sync.
        let i = fvar("I");
        let stmt = Stmt::Print {
            items: vec![crate::ir::PrintPiece::Expr(Expr::Var(i.clone()))],
            newline: true,
        };
        assert!(!stmt_loop_var_needs_fac(&stmt, &i, None, ForLowering::U8));
    }

    #[test]
    fn classify_for_lowering_picks_u8() {
        // FOR I = 0 TO 150 STEP 1 — fits u8 with Float counter.
        let i = fvar("I");
        let lowering = classify_for_lowering(
            &i,
            &Expr::Number(0.0),
            &Expr::Number(150.0),
            &Expr::Number(1.0),
        );
        assert_eq!(lowering, ForLowering::U8);
        assert!(!lowering.bare_target_var_routes_through_int());
    }

    #[test]
    fn classify_for_lowering_picks_int() {
        // FOR I = 1 TO 500 STEP 1 — too wide for u8, drops to int.
        let i = fvar("I");
        let lowering = classify_for_lowering(
            &i,
            &Expr::Number(1.0),
            &Expr::Number(500.0),
            &Expr::Number(1.0),
        );
        assert_eq!(lowering, ForLowering::Int);
        // int-FOR Float counter is int-routable in any profile.
        assert!(lowering.bare_target_var_routes_through_int());
    }

    #[test]
    fn classify_for_lowering_integer_target_collapses_to_int() {
        // FOR I% = 0 TO 150 — Integer target, no asymmetry to worry
        // about. Treat as int-FOR for the routing predicate.
        let i = ivar("I");
        let lowering = classify_for_lowering(
            &i,
            &Expr::Number(0.0),
            &Expr::Number(150.0),
            &Expr::Number(1.0),
        );
        assert_eq!(lowering, ForLowering::Int);
        assert!(lowering.bare_target_var_routes_through_int());
    }

    #[test]
    fn classify_for_lowering_dynamic_endpoints_drops_to_float() {
        // FOR I = 1 TO N — N isn't a literal so codegen takes the
        // float-FOR path and V_var IS the counter.
        let i = fvar("I");
        let n = fvar("N");
        let lowering =
            classify_for_lowering(&i, &Expr::Number(1.0), &Expr::Var(n), &Expr::Number(1.0));
        assert_eq!(lowering, ForLowering::Float);
    }

    fn for_stmt(var: VarName, start: f64, end: f64, step: f64) -> Stmt {
        Stmt::For {
            var,
            start: Expr::Number(start),
            end: Expr::Number(end),
            step: Expr::Number(step),
            body_int_safe: true,
            body_reads_loop_var: false,
            induction_const: None,
            array_inductions: Vec::new(),
        }
    }

    fn for_body_int_safe(module: &Module) -> bool {
        module
            .lines
            .iter()
            .flat_map(|l| &l.stmts)
            .find_map(|s| match s {
                Stmt::For { body_int_safe, .. } => Some(*body_int_safe),
                _ => None,
            })
            .expect("module has a FOR")
    }

    #[test]
    fn unmatched_for_opaque_expressions_cannot_prove_counter_preservation() {
        // The earlier predicate skipped DIM and ON GOSUB selectors, and PRINT AT/USE fields.
        for call in [
            Expr::FnCall(crate::ast::FnName("F".into()), Box::new(Expr::Number(1.0))),
            Expr::Usr(Box::new(Expr::Number(1.0))),
        ] {
            let a = fvar("A");
            let str_call = StrExpr::Str(Box::new(call.clone()));
            let print = |p| Stmt::Print { items: vec![p], newline: true };
            let cases = vec![
                Stmt::Let { var: a.clone(), value: call.clone() },
                Stmt::LetStr { var: a.clone(), value: str_call.clone() },
                Stmt::ArrayLet { name: a.clone(), indices: vec![call.clone()], value: Expr::Number(0.0) },
                Stmt::ArrayLet { name: a.clone(), indices: vec![Expr::Number(0.0)], value: call.clone() },
                Stmt::ArrayLetStr { name: a.clone(), indices: vec![call.clone()], value: StrExpr::Literal(vec![]) },
                Stmt::ArrayLetStr { name: a.clone(), indices: vec![Expr::Number(0.0)], value: str_call.clone() },
                Stmt::Read(vec![ir::ReadTarget::Array { name: a.clone(), indices: vec![call.clone()] }]),
                Stmt::Get { var: a.clone(), indices: vec![call.clone()] },
                print(PrintPiece::Expr(call.clone())),
                print(PrintPiece::CharOut(call.clone())),
                print(PrintPiece::TabTo(call.clone())),
                print(PrintPiece::Spc(call.clone())),
                print(PrintPiece::StrExpr(str_call)),
                print(PrintPiece::PositionAt(call.clone(), Expr::Number(0.0))),
                print(PrintPiece::PositionAt(Expr::Number(0.0), call.clone())),
                Stmt::If { cond: call.clone(), then: ThenIr::Goto(10) },
                Stmt::IfElse { cond: call.clone(), then: ThenIr::Goto(10), else_then: ThenIr::Goto(20) },
                Stmt::ComputedGoto { target: call.clone() },
                Stmt::OnBranch { kind: OnBranchKind::Goto, value: call.clone(), targets: vec![10] },
                Stmt::OnBranch { kind: OnBranchKind::GoSub, value: call.clone(), targets: vec![100] },
                Stmt::Dim(vec![ir::DimSpec { name: a, dims: vec![call] }]),
            ];
            for stmt in cases {
                let mut module = Module { lines: vec![
                    Line { number: 10, stmts: vec![Stmt::Next { vars: vec![None] }] },
                    Line { number: 20, stmts: vec![for_stmt(fvar("I"), 1.0, 3.0, 1.0), stmt.clone()] },
                    Line { number: 100, stmts: vec![Stmt::Return] },
                ] };
                IntForBodyAnalysis.run(&mut module).unwrap();
                assert!(!for_body_int_safe(&module), "opaque expression in {stmt:?}");
            }
        }
    }

    /// `FOR I … : GOSUB 100 … : NEXT` where line 100 itself does
    /// `GOSUB 200`, and neither routine writes I. The interprocedural
    /// routine-write analysis should keep the body int-safe.
    fn module_for_nested_gosub(callee_writes_counter: bool) -> Module {
        let i = fvar("I");
        Module {
            lines: vec![
                Line {
                    number: 10,
                    stmts: vec![
                        for_stmt(i.clone(), 1.0, 10.0, 1.0),
                        Stmt::GoSub { target: 100 },
                        Stmt::Poke {
                            addr: Expr::Var(i.clone()),
                            value: Expr::Var(i.clone()),
                        },
                        Stmt::Next {
                            vars: vec![Some(i.clone())],
                        },
                    ],
                },
                Line { number: 20, stmts: vec![Stmt::End] },
                Line {
                    number: 100,
                    stmts: vec![Stmt::GoSub { target: 200 }, Stmt::Return],
                },
                Line {
                    number: 200,
                    stmts: vec![
                        Stmt::Let {
                            // Writes the counter only in the "unsafe" variant.
                            var: if callee_writes_counter { i } else { fvar("Q") },
                            value: Expr::Number(5.0),
                        },
                        Stmt::Return,
                    ],
                },
            ],
        }
    }

    #[test]
    fn int_for_body_clean_nested_gosub_stays_safe() {
        let mut module = module_for_nested_gosub(false);
        IntForBodyAnalysis.run(&mut module).unwrap();
        assert!(
            for_body_int_safe(&module),
            "nested GOSUB whose routines never write the counter must stay int-safe"
        );
    }

    #[test]
    fn int_for_body_nested_gosub_writing_counter_is_unsafe() {
        let mut module = module_for_nested_gosub(true);
        IntForBodyAnalysis.run(&mut module).unwrap();
        assert!(
            !for_body_int_safe(&module),
            "a transitively-reached routine that writes the counter must mark the body unsafe"
        );
    }

    #[test]
    fn poke_loop_fusion_collapses_classic_clear_screen() {
        let i = fvar("I");
        let mut module = Module {
            lines: vec![Line {
                number: 10,
                stmts: vec![
                    for_stmt(i.clone(), 1024.0, 2023.0, 1.0),
                    Stmt::Poke {
                        addr: Expr::Var(i.clone()),
                        value: Expr::Number(32.0),
                    },
                    Stmt::Next {
                        vars: vec![Some(i.clone())],
                    },
                ],
            }],
        };
        PokeLoopFusion.run(&mut module).unwrap();
        let stmts = &module.lines[0].stmts;
        assert_eq!(stmts.len(), 1);
        let Stmt::PokeFill {
            dst_start,
            dst_end,
            value,
        } = &stmts[0]
        else {
            panic!("expected PokeFill, got {:?}", stmts[0]);
        };
        assert_eq!(expr_as_f64_literal(dst_start), Some(1024.0));
        assert_eq!(expr_as_f64_literal(dst_end), Some(2023.0));
        assert_eq!(expr_as_f64_literal(value), Some(32.0));
    }

    #[test]
    fn poke_loop_fusion_folds_constant_offset_into_endpoints() {
        // FOR I=0 TO 9: POKE 1024+I, 32: NEXT  -->  fill [1024..1033]
        let i = ivar("I");
        let mut module = Module {
            lines: vec![Line {
                number: 10,
                stmts: vec![
                    for_stmt(i.clone(), 0.0, 9.0, 1.0),
                    Stmt::Poke {
                        addr: Expr::Bin(
                            BinOp::Add,
                            Box::new(Expr::Number(1024.0)),
                            Box::new(Expr::Var(i.clone())),
                        ),
                        value: Expr::Number(32.0),
                    },
                    Stmt::Next { vars: vec![None] },
                ],
            }],
        };
        PokeLoopFusion.run(&mut module).unwrap();
        let stmts = &module.lines[0].stmts;
        assert_eq!(stmts.len(), 1);
        let Stmt::PokeFill {
            dst_start, dst_end, ..
        } = &stmts[0]
        else {
            panic!("expected PokeFill");
        };
        // Endpoints carry the +1024 offset as `start + 1024`,
        // `end + 1024`. ConstantFold (run later in the pipeline)
        // collapses these to literals at codegen time.
        let mut folder = ConstantFolder {
            fre_observable: false,
        };
        let mut start = dst_start.clone();
        let mut end = dst_end.clone();
        crate::visit::MutVisitor::visit_expr_mut(&mut folder, &mut start);
        crate::visit::MutVisitor::visit_expr_mut(&mut folder, &mut end);
        assert_eq!(expr_as_f64_literal(&start), Some(1024.0));
        assert_eq!(expr_as_f64_literal(&end), Some(1033.0));
    }

    fn fuses_with_value(lo: f64, hi: f64, value: Expr) -> bool {
        let i = ivar("I");
        let mut module = Module {
            lines: vec![Line {
                number: 10,
                stmts: vec![
                    for_stmt(i.clone(), lo, hi, 1.0),
                    Stmt::Poke {
                        addr: Expr::Var(i.clone()),
                        value,
                    },
                    Stmt::Next {
                        vars: vec![Some(i.clone())],
                    },
                ],
            }],
        };
        PokeLoopFusion.run(&mut module).unwrap();
        let stmts = &module.lines[0].stmts;
        match stmts.len() {
            1 => matches!(stmts[0], Stmt::PokeFill { .. }),
            3 => false,
            n => panic!("unexpected statement count {n}"),
        }
    }

    #[test]
    fn poke_loop_fusion_skips_io_peek_value() {
        assert!(
            !fuses_with_value(
                1024.0,
                2023.0,
                Expr::Peek(Box::new(Expr::Number(53266.0)))
            ),
            "a $D012 read must not be hoisted out of the fill"
        );
    }

    #[test]
    fn poke_loop_fusion_skips_value_peeking_into_the_fill_range() {
        let value = Expr::Bin(
            BinOp::Add,
            Box::new(Expr::Peek(Box::new(Expr::Number(49500.0)))),
            Box::new(Expr::Number(1.0)),
        );
        assert!(
            !fuses_with_value(49152.0, 50151.0, value),
            "a PEEK of an address inside the fill range must not be hoisted"
        );
    }

    #[test]
    fn poke_loop_fusion_skips_peek_outside_the_fill_range_too() {
        assert!(
            !fuses_with_value(
                1024.0,
                2023.0,
                Expr::Peek(Box::new(Expr::Number(4096.0)))
            ),
            "the gate refuses every memory read in the value"
        );
    }

    #[test]
    fn poke_loop_fusion_still_fuses_a_literal_value() {
        assert!(
            fuses_with_value(1024.0, 2023.0, Expr::Number(81.0)),
            "the ordinary constant fill must still fuse"
        );
    }

    #[test]
    fn poke_loop_fusion_still_fuses_a_variable_expression() {
        let value = Expr::Bin(
            BinOp::Mul,
            Box::new(Expr::Var(fvar("V"))),
            Box::new(Expr::Number(3.0)),
        );
        assert!(
            fuses_with_value(49152.0, 50151.0, value),
            "a pure scalar expression must still fuse"
        );
    }

    #[test]
    fn poke_loop_fusion_skips_step_other_than_one() {
        let i = ivar("I");
        let mut module = Module {
            lines: vec![Line {
                number: 10,
                stmts: vec![
                    for_stmt(i.clone(), 0.0, 10.0, 2.0),
                    Stmt::Poke {
                        addr: Expr::Var(i.clone()),
                        value: Expr::Number(0.0),
                    },
                    Stmt::Next { vars: vec![None] },
                ],
            }],
        };
        PokeLoopFusion.run(&mut module).unwrap();
        // STEP 2 → no fusion. Triplet must still be a FOR/POKE/NEXT.
        let stmts = &module.lines[0].stmts;
        assert_eq!(stmts.len(), 3);
        assert!(matches!(stmts[0], Stmt::For { .. }));
        assert!(matches!(stmts[1], Stmt::Poke { .. }));
        assert!(matches!(stmts[2], Stmt::Next { .. }));
    }

    #[test]
    fn poke_loop_fusion_skips_value_that_reads_loop_var() {
        // POKE I, I — value depends on the iteration, not foldable
        // to a single fill byte.
        let i = ivar("I");
        let mut module = Module {
            lines: vec![Line {
                number: 10,
                stmts: vec![
                    for_stmt(i.clone(), 0.0, 9.0, 1.0),
                    Stmt::Poke {
                        addr: Expr::Var(i.clone()),
                        value: Expr::Var(i.clone()),
                    },
                    Stmt::Next { vars: vec![None] },
                ],
            }],
        };
        PokeLoopFusion.run(&mut module).unwrap();
        assert_eq!(module.lines[0].stmts.len(), 3);
        assert!(matches!(module.lines[0].stmts[0], Stmt::For { .. }));
    }

    #[test]
    fn licm_hoists_invariant_subexpression_from_for_body() {
        // FOR I=1 TO N: A=X*K+I: NEXT
        // X and K are loop-invariant; the X*K subtree should be
        // hoisted to a fresh LET __LICM_0 = X*K immediately
        // before the FOR.
        let i = fvar("I");
        let x = fvar("X");
        let k = fvar("K");
        let n = fvar("N");
        let a = fvar("A");
        let mut module = Module {
            lines: vec![Line {
                number: 10,
                stmts: vec![
                    Stmt::For {
                        var: i.clone(),
                        start: Expr::Number(1.0),
                        end: Expr::Var(n),
                        step: Expr::Number(1.0),
                        body_int_safe: false,
                        body_reads_loop_var: true,
                        induction_const: None,
                        array_inductions: Vec::new(),
                    },
                    Stmt::Let {
                        var: a,
                        value: Expr::Bin(
                            BinOp::Add,
                            Box::new(Expr::Bin(
                                BinOp::Mul,
                                Box::new(Expr::Var(x.clone())),
                                Box::new(Expr::Var(k.clone())),
                            )),
                            Box::new(Expr::Var(i.clone())),
                        ),
                    },
                    Stmt::Next {
                        vars: vec![Some(i)],
                    },
                ],
            }],
        };

        LoopInvariantCodeMotion.run(&mut module).unwrap();

        let stmts = &module.lines[0].stmts;
        assert_eq!(stmts.len(), 4, "expected hoisted LET + original 3 stmts");

        // Position 0: hoisted LET __LICM_0 = X*K
        let Stmt::Let { var, value } = &stmts[0] else {
            panic!("expected hoisted LET at position 0, got {:?}", stmts[0]);
        };
        assert!(var.base.starts_with("__LICM_"));
        assert!(matches!(
            value,
            Expr::Bin(BinOp::Mul, l, r)
                if matches!(l.as_ref(), Expr::Var(v) if v == &x)
                && matches!(r.as_ref(), Expr::Var(v) if v == &k)
        ));

        // Body's `A = X*K + I` should now read `A = __LICM_0 + I`.
        let Stmt::Let { value, .. } = &stmts[2] else {
            panic!("expected LET A at position 2");
        };
        let Expr::Bin(BinOp::Add, l, _r) = value else {
            panic!("expected Add at top of body RHS");
        };
        let Expr::Var(licm) = l.as_ref() else {
            panic!("expected hoisted Var on lhs of Add, got {:?}", l);
        };
        assert!(licm.base.starts_with("__LICM_"));
    }

    #[test]
    fn licm_skips_when_body_has_gosub() {
        // FOR I=1 TO N: GOSUB 100: NEXT  — GOSUB could mutate
        // anything, so LICM must refuse the loop.
        let i = fvar("I");
        let n = fvar("N");
        let mut module = Module {
            lines: vec![Line {
                number: 10,
                stmts: vec![
                    Stmt::For {
                        var: i.clone(),
                        start: Expr::Number(1.0),
                        end: Expr::Var(n),
                        step: Expr::Number(1.0),
                        body_int_safe: false,
                        body_reads_loop_var: true,
                        induction_const: None,
                        array_inductions: Vec::new(),
                    },
                    Stmt::GoSub { target: 100 },
                    Stmt::Next {
                        vars: vec![Some(i)],
                    },
                ],
            }],
        };

        LoopInvariantCodeMotion.run(&mut module).unwrap();
        // Untouched: still 3 stmts, no hoisted LET.
        assert_eq!(module.lines[0].stmts.len(), 3);
    }

    #[test]
    fn licm_skips_when_subexpr_uses_loop_var() {
        // FOR I=1 TO N: A=X*I: NEXT  — X*I is NOT invariant.
        let i = fvar("I");
        let x = fvar("X");
        let n = fvar("N");
        let a = fvar("A");
        let mut module = Module {
            lines: vec![Line {
                number: 10,
                stmts: vec![
                    Stmt::For {
                        var: i.clone(),
                        start: Expr::Number(1.0),
                        end: Expr::Var(n),
                        step: Expr::Number(1.0),
                        body_int_safe: false,
                        body_reads_loop_var: true,
                        induction_const: None,
                        array_inductions: Vec::new(),
                    },
                    Stmt::Let {
                        var: a,
                        value: Expr::Bin(
                            BinOp::Mul,
                            Box::new(Expr::Var(x)),
                            Box::new(Expr::Var(i.clone())),
                        ),
                    },
                    Stmt::Next {
                        vars: vec![Some(i)],
                    },
                ],
            }],
        };

        LoopInvariantCodeMotion.run(&mut module).unwrap();
        assert_eq!(module.lines[0].stmts.len(), 3);
    }

    /// `A=PEEK($D01F)` clears the VIC sprite-collision latch. Even
    /// when `A` is dead, the read must happen for its side effect.
    #[test]
    fn dead_store_keeps_peek_of_io_register() {
        let v = fvar("V");
        let a = fvar("A");
        let mut module = Module {
            lines: vec![Line {
                number: 10,
                stmts: vec![
                    Stmt::Let {
                        var: v.clone(),
                        value: Expr::Number(53248.0),
                    },
                    // A = PEEK(V+31) — V+31 = $D01F (sprite-sprite
                    // collision, read-clear). A is never read again.
                    Stmt::Let {
                        var: a,
                        value: Expr::Peek(Box::new(Expr::Bin(
                            BinOp::Add,
                            Box::new(Expr::Var(v)),
                            Box::new(Expr::Number(31.0)),
                        ))),
                    },
                ],
            }],
        };
        // Constant-fold first so V+31 reduces to 53279 — that's what
        // the real pipeline does before DeadStoreElim runs.
        ConstantFold.run(&mut module).unwrap();
        DeadStoreElim.run(&mut module).unwrap();
        assert_eq!(
            module.lines[0].stmts.len(),
            2,
            "PEEK of $D01F must NOT be DCE'd — the read clears the \
             VIC collision latch:\n{:#?}",
            module.lines[0].stmts
        );
    }

    /// Companion: PEEK of plain RAM is still safe to drop when the
    /// destination is dead, so we don't regress the optimisation for
    /// the common case.
    #[test]
    fn dead_store_drops_peek_of_plain_ram() {
        let a = fvar("A");
        let mut module = Module {
            lines: vec![Line {
                number: 10,
                stmts: vec![Stmt::Let {
                    var: a,
                    value: Expr::Peek(Box::new(Expr::Number(2048.0))),
                }],
            }],
        };
        ConstantFold.run(&mut module).unwrap();
        DeadStoreElim.run(&mut module).unwrap();
        assert!(
            module.lines[0].stmts.is_empty(),
            "PEEK($0800) is a plain RAM read with no side effect — \
             should drop when target is dead:\n{:#?}",
            module.lines[0].stmts
        );
    }

    /// Generate nested FOR loops with one statement per BASIC line.
    fn for_nest_module(n: usize, body: Vec<Stmt>) -> Module {
        let mut lines = Vec::new();
        let mut number = 10u16;
        let names: Vec<VarName> = (0..n).map(|i| fvar(&format!("{}", (b'A' + i as u8) as char))).collect();
        for v in &names {
            lines.push(Line {
                number,
                stmts: vec![Stmt::For {
                    var: v.clone(),
                    start: Expr::Number(1.0),
                    end: Expr::Number(1.0),
                    step: Expr::Number(1.0),
                    body_int_safe: false,
                    body_reads_loop_var: true,
                    induction_const: None,
                    array_inductions: Vec::new(),
                }],
            });
            number += 10;
        }
        lines.push(Line {
            number,
            stmts: body,
        });
        number += 10;
        for v in names.iter().rev() {
            lines.push(Line {
                number,
                stmts: vec![Stmt::Next {
                    vars: vec![Some(v.clone())],
                }],
            });
            number += 10;
        }
        Module { lines }
    }

    #[test]
    fn the_page_one_ceiling_protects_only_positions_past_it() {
        let body = vec![Stmt::Let {
            var: fvar("Z"),
            value: Expr::Number(1.0),
        }];
        let deep = for_nest_module(10, body.clone());
        assert_eq!(
            stack_floor_trap_positions(&deep),
            [(10usize, vec![0usize])].into_iter().collect(),
            "line index 10 is the `Z=1` line, sitting on ten live frames"
        );
        assert!(
            stack_floor_trap_positions(&for_nest_module(9, body)).is_empty(),
            "nine frames leave room for an expression — nothing to protect"
        );
    }

    #[test]
    fn const_var_prop_keeps_a_promoted_let_past_the_ceiling() {
        let mut module = for_nest_module(
            10,
            vec![Stmt::Let {
                var: fvar("Z"),
                value: Expr::Number(1.0),
            }],
        );
        run_const_var_prop(&mut module);
        assert_eq!(
            module.lines[10].stmts.len(),
            1,
            "the trap has to have a statement to sit in front of:\n{:#?}",
            module.lines[10].stmts
        );
    }

    #[test]
    fn const_var_prop_still_drops_a_promoted_let_under_the_ceiling() {
        let mut module = for_nest_module(
            9,
            vec![Stmt::Let {
                var: fvar("Z"),
                value: Expr::Number(1.0),
            }],
        );
        run_const_var_prop(&mut module);
        assert!(
            module.lines[9].stmts.is_empty(),
            "still droppable nine frames deep:\n{:#?}",
            module.lines[9].stmts
        );
    }

    #[test]
    fn dead_store_elim_keeps_a_dead_store_past_the_ceiling() {
        let z = fvar("Z");
        let mut module = for_nest_module(
            10,
            vec![
                Stmt::Let {
                    var: z.clone(),
                    value: Expr::Number(1.0),
                },
                Stmt::Let {
                    var: z,
                    value: Expr::Number(2.0),
                },
            ],
        );
        DeadStoreElim.run(&mut module).unwrap();
        assert_eq!(
            module.lines[10].stmts.len(),
            2,
            "both sit past the ceiling; the first is where the ROM stops:\n{:#?}",
            module.lines[10].stmts
        );
    }

    #[test]
    fn dead_store_elim_keeps_the_ti_clock_write() {
        let ti = svar("TI");
        let a = svar("A");
        let mut module = Module {
            lines: vec![Line {
                number: 10,
                stmts: vec![
                    Stmt::LetStr {
                        var: ti.clone(),
                        value: crate::ir::StrExpr::Literal(b"000000".to_vec()),
                    },
                    Stmt::LetStr {
                        var: a,
                        value: crate::ir::StrExpr::Literal(b"000000".to_vec()),
                    },
                ],
            }],
        };
        DeadStoreElim.run(&mut module).unwrap();
        let kept: Vec<&VarName> = module.lines[0]
            .stmts
            .iter()
            .filter_map(|s| match s {
                Stmt::LetStr { var, .. } => Some(var),
                _ => None,
            })
            .collect();
        assert_eq!(
            kept,
            vec![&ti],
            "TI$ is the clock write and must stay; the ordinary A$ store \
             next to it is genuinely dead and must go:\n{:#?}",
            module.lines[0].stmts
        );
    }

    #[test]
    fn str_store_trap_guard_is_scoped_to_the_string_ti() {
        assert!(
            !str_store_cannot_trap(&svar("TI")),
            "TI$ stores can trap and must block the drop"
        );
        assert!(
            str_store_cannot_trap(&svar("TA")),
            "an ordinary string var must stay droppable"
        );
        assert!(
            str_store_cannot_trap(&fvar("TI")),
            "the guard is about the string TI$, not the base"
        );
    }

    fn svar(base: &str) -> VarName {
        VarName {
            base: base.to_string(),
            kind: VarKind::String,
        }
    }

    #[test]
    fn dead_store_drops_heap_concat_when_no_fre() {
        // `A$=A$+"X" : A$="reset"` — without FRE in the program, the
        // first allocation is unobservable. The relaxed dead-store
        // gate should drop it.
        let a = svar("A");
        let mut module = Module {
            lines: vec![
                Line {
                    number: 10,
                    stmts: vec![Stmt::LetStr {
                        var: a.clone(),
                        value: crate::ir::StrExpr::Concat(
                            Box::new(crate::ir::StrExpr::Var(a.clone())),
                            Box::new(crate::ir::StrExpr::Literal(b"X".to_vec())),
                        ),
                    }],
                },
                Line {
                    number: 20,
                    stmts: vec![Stmt::LetStr {
                        var: a.clone(),
                        value: crate::ir::StrExpr::Literal(b"reset".to_vec()),
                    }],
                },
                Line {
                    number: 30,
                    stmts: vec![Stmt::Print {
                        items: vec![crate::ir::PrintPiece::StrExpr(crate::ir::StrExpr::Var(a))],
                        newline: true,
                    }],
                },
            ],
        };
        DeadStoreElim.run(&mut module).unwrap();
        assert!(
            module.lines[0].stmts.is_empty(),
            "Concat to A$ is dead (A$ is overwritten before being read) — \
             should drop because no FRE in program:\n{:#?}",
            module.lines[0].stmts
        );
    }

    #[test]
    fn dead_store_keeps_heap_concat_when_fre_present() {
        // Same shape, but the program contains a FRE call. The heap
        // allocation IS observable now — keep it.
        let a = svar("A");
        let b = fvar("B");
        let mut module = Module {
            lines: vec![
                Line {
                    number: 10,
                    stmts: vec![Stmt::LetStr {
                        var: a.clone(),
                        value: crate::ir::StrExpr::Concat(
                            Box::new(crate::ir::StrExpr::Var(a.clone())),
                            Box::new(crate::ir::StrExpr::Literal(b"X".to_vec())),
                        ),
                    }],
                },
                Line {
                    number: 15,
                    stmts: vec![Stmt::Let {
                        var: b,
                        value: Expr::Fre(Box::new(Expr::Number(0.0))),
                    }],
                },
                Line {
                    number: 20,
                    stmts: vec![Stmt::LetStr {
                        var: a.clone(),
                        value: crate::ir::StrExpr::Literal(b"reset".to_vec()),
                    }],
                },
                Line {
                    number: 30,
                    stmts: vec![Stmt::Print {
                        items: vec![crate::ir::PrintPiece::StrExpr(crate::ir::StrExpr::Var(a))],
                        newline: true,
                    }],
                },
            ],
        };
        DeadStoreElim.run(&mut module).unwrap();
        assert!(
            !module.lines[0].stmts.is_empty(),
            "FRE makes heap state observable — must NOT drop the Concat:\n{:#?}",
            module.lines[0].stmts
        );
    }
}

// ===== Multi-type scalar var splitting (SSA-style) ========================
//
// A scalar var with disjoint def-use lifetimes is split into one
// variable per lifetime so each can be typed independently. The
// canonical case is UBL's `U`: line 9500 does `U=RND(0)` (float),
// then later code uses U as an integer index in POKE / array reads.
// The float lifetime never feeds the int reads, but YAB's shadow-int
// gate previously rejected the whole var because one write was float.
//
// Algorithm: per scalar Float var, run forward reaching-defs on the
// CFG, then union-find on writes by shared readers. If the writes
// fall into multiple disjoint clusters, each cluster gets its own
// renamed variable. Reads see only one cluster by construction (the
// union-find merges any cluster that shares a reader).
//
// Renamed bases use a double underscore (`U__1`, `U__2`); BASIC v2's
// parser rejects `_` so the new names can't collide with anything
// the user typed.

pub struct SplitMultiTypeVars;

impl ir::Pass for SplitMultiTypeVars {
    fn name(&self) -> &'static str {
        "split-multi-type-vars"
    }

    fn run(&self, module: &mut ir::Module) -> Result<(), ir::PassError> {
        let cfg = crate::cfg::Cfg::build(module);        let (writes_by_var, reads_by_var) = collect_var_io(module, &cfg);        // FOR/NEXT counters can't be split: the FOR-init write, the
// NEXT-step write, AND any body LET to the loop var all share
// one storage slot at runtime, and codegen pairs NEXT to FOR
// by name. A program that writes the loop var inside its own
// body (`120 FOR I=1 TO 5: 130 I=I+1: 140 NEXT I`) needs the
// body's write to land in the same slot the FOR reads from
// next iteration — splitting it would let the loop run with
// a stale counter. Conservatively skip any var that ever
// appears as a FOR counter or a NEXT-named counter.
        let mut skip: HashSet<VarName> = HashSet::new();
        for line in &module.lines {
            scan_skip_vars(&line.stmts, &mut skip);
        }

        let mut candidates: Vec<(&VarName, &Vec<usize>, &Vec<usize>)> = Vec::new();
        for (var, writes) in &writes_by_var {
            if var.kind != VarKind::Float {
                continue;
            }
            if var.base == "TI" || var.base == "ST" {
                continue;
            }
            if skip.contains(var) {
                continue;
            }
            if writes.len() < 2 {
                continue;
            }
            // Skip vars that are also used as arrays — splitting the
            // scalar would still leave the array slot, but the var
            // identity used by `intern_var` is the same shape so we
            // play it safe and stay clear.
            let Some(reads) = reads_by_var.get(var) else {
                continue;
            };
            candidates.push((var, writes, reads));
        }
        candidates.sort_by(|a, b| a.0.base.cmp(&b.0.base));
        // ONE reaching-defs solve for every candidate at once.
        let reach = reaching_defs(&cfg, &candidates);        let mut rename_writes: HashMap<(VarName, usize), VarName> = HashMap::new();
        let mut rename_reads: HashMap<(VarName, usize), VarName> = HashMap::new();
        for (var_slot, &(var, writes, reads)) in candidates.iter().enumerate() {
            // Each write starts as its own partition. Union by shared
            // reader: if a read sees writes {a, b}, union(a, b).
            let mut uf = UnionFind::new(writes.len());
            for &r in reads {
                let mut first: Option<usize> = None;
                for w in reach.iter_in(r, var_slot) {
                    match first {
                        None => first = Some(w),
                        Some(f) => uf.union(f, w),
                    }
                }
            }
            let partitions = uf.partitions();
            if partitions.len() <= 1 {
                continue;
            }
            // Stable cluster ordering: the cluster containing the
            // first write keeps the original name, others get a
            // `__<n>` suffix in source order.
            let mut clusters: Vec<Vec<usize>> = partitions
                .into_iter()
                .map(|set| {
                    let mut v: Vec<usize> = set.into_iter().collect();
                    v.sort_unstable();
                    v
                })
                .collect();
            clusters.sort_by_key(|c| c[0]);
            for (cluster_idx, cluster) in clusters.iter().enumerate() {
                let new_var = if cluster_idx == 0 {
                    var.clone()
                } else {
                    VarName {
                        base: format!("{}__{}", var.base, cluster_idx),
                        kind: VarKind::Float,
                    }
                };
                for &write_local in cluster {
                    let write_node = writes[write_local];
                    rename_writes.insert((var.clone(), write_node), new_var.clone());
                }
                let cluster_mask = reach.mask_of(var_slot, cluster);
                for &r in reads {
                    if reach.intersects(r, &cluster_mask) {
                        rename_reads.insert((var.clone(), r), new_var.clone());
                    }
                }
            }
        }        if rename_writes.is_empty() && rename_reads.is_empty() {
            return Ok(());
        }
        apply_var_split_renames(module, &cfg, &rename_writes, &rename_reads);
        Ok(())
    }
}

/// Tiny union-find specialised for the cluster pass.
struct UnionFind {
    parent: Vec<usize>,
    rank: Vec<u8>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
            rank: vec![0; n],
        }
    }

    fn find(&mut self, mut x: usize) -> usize {
        while self.parent[x] != x {
            self.parent[x] = self.parent[self.parent[x]];
            x = self.parent[x];
        }
        x
    }

    fn union(&mut self, a: usize, b: usize) {
        let ra = self.find(a);
        let rb = self.find(b);
        if ra == rb {
            return;
        }
        let (small, big) = if self.rank[ra] < self.rank[rb] {
            (ra, rb)
        } else {
            (rb, ra)
        };
        self.parent[small] = big;
        if self.rank[small] == self.rank[big] {
            self.rank[big] += 1;
        }
    }

    fn partitions(mut self) -> Vec<HashSet<usize>> {
        let n = self.parent.len();
        let roots: Vec<usize> = (0..n).map(|i| self.find(i)).collect();
        let mut groups: HashMap<usize, HashSet<usize>> = HashMap::new();
        for (i, root) in roots.iter().enumerate() {
            groups.entry(*root).or_default().insert(i);
        }
        groups.into_values().collect()
    }
}

/// Walk every CFG node and collect, per scalar var, which nodes write and which nodes read it.
fn collect_var_io(
    module: &ir::Module,
    cfg: &crate::cfg::Cfg,
) -> (HashMap<VarName, Vec<usize>>, HashMap<VarName, Vec<usize>>) {
    let mut writes: HashMap<VarName, Vec<usize>> = HashMap::new();
    let mut reads: HashMap<VarName, Vec<usize>> = HashMap::new();
    for (node_id, node) in cfg.nodes.iter().enumerate() {
        let stmt = cfg.stmt_at(node_id, module);
        let mut node_writes: HashSet<VarName> = HashSet::new();
        let mut node_reads: HashSet<VarName> = HashSet::new();
        collect_stmt_writes_reads(stmt, &mut node_writes, &mut node_reads);
        for v in node_writes {
            writes.entry(v).or_default().push(node_id);
        }
        for v in node_reads {
            reads.entry(v).or_default().push(node_id);
        }
        let _ = node;
    }
    (writes, reads)
}

/// Exclude variables whose names are needed for loop pairing or function binding.
fn scan_skip_vars(stmts: &[Stmt], skip: &mut HashSet<VarName>) {
    for s in stmts {
        match s {
            Stmt::For { var, .. } => {
                skip.insert(var.clone());
            }
            Stmt::Next { vars } => {
                for v in vars.iter().flatten() {
                    skip.insert(v.clone());
                }
            }

            Stmt::Input { targets, .. } => {
                for t in targets {
                    if let crate::ir::ReadTarget::Scalar(v) = t {
                        skip.insert(v.clone());
                    }
                }
            }
            Stmt::DefFn { param, body, .. } => {
                skip.insert(param.clone());
                collect_expr_reads(body, skip);
            }
            Stmt::If {
                then: ThenIr::Stmts(inner),
                ..
            } => scan_skip_vars(inner, skip),
            Stmt::IfElse {
                then,
                else_then,
                ..
            } => {
                if let ThenIr::Stmts(inner) = then {
                    scan_skip_vars(inner, skip);
                }
                if let ThenIr::Stmts(inner) = else_then {
                    scan_skip_vars(inner, skip);
                }
            }
            _ => {}
        }
    }
}

fn collect_stmt_writes_reads(
    stmt: &Stmt,
    writes: &mut HashSet<VarName>,
    reads: &mut HashSet<VarName>,
) {
    match stmt {
        Stmt::Let { var, value } => {
            writes.insert(var.clone());
            collect_expr_reads(value, reads);
        }
        Stmt::For {
            var, start, end, step, ..
        } => {
            writes.insert(var.clone());
            collect_expr_reads(start, reads);
            collect_expr_reads(end, reads);
            collect_expr_reads(step, reads);
        }
        Stmt::Read(targets) | Stmt::Input { targets, .. } => {
            for t in targets {
                match t {
                    ir::ReadTarget::Scalar(v) => {
                        writes.insert(v.clone());
                    }
                    // An array target's subscripts READ the scalars they mention: `READ
                    // A$(B*2)` reads B.
                    ir::ReadTarget::Array { indices, .. } => {
                        for e in indices {
                            collect_expr_reads(e, reads);
                        }
                    }
                }
            }
        }
        // `GET A(I)` writes an array element.
        Stmt::Get { var, indices } => {
            if indices.is_empty() {
                writes.insert(var.clone());
            } else {
                for e in indices {
                    collect_expr_reads(e, reads);
                }
            }
        }
        Stmt::GetFile { vars, file_num, .. } => {
            for v in vars {
                writes.insert(v.clone());
            }
            collect_expr_reads(file_num, reads);
        }
        Stmt::If { cond, then } => {
            collect_expr_reads(cond, reads);
            if let ThenIr::Stmts(_) = then {
                // body stmts get their own CFG nodes
            }
        }
        Stmt::Poke { addr, value } | Stmt::Dpoke { addr, value } => {
            collect_expr_reads(addr, reads);
            collect_expr_reads(value, reads);
        }
        Stmt::Sys {
            addr,
            regs,
            param_vars,
            ..
        } => {
            collect_expr_reads(addr, reads);
            for e in regs {
                collect_expr_reads(e, reads);
            }
            for p in param_vars {
                collect_expr_reads(&p.value, reads);
            }
        }
        Stmt::OnBranch { value, .. } => collect_expr_reads(value, reads),
        Stmt::Print { items, .. } => {
            for piece in items {
                collect_print_piece_reads(piece, reads);
            }
        }
        Stmt::Cmd { items, file_num, .. } => {
            collect_expr_reads(file_num, reads);
            for piece in items {
                collect_print_piece_reads(piece, reads);
            }
        }
        Stmt::LetStr { value, .. } => collect_str_expr_reads(value, reads),
        Stmt::ArrayLet { indices, value, .. } => {
            for e in indices {
                collect_expr_reads(e, reads);
            }
            collect_expr_reads(value, reads);
        }
        Stmt::ArrayLetStr { indices, value, .. } => {
            for e in indices {
                collect_expr_reads(e, reads);
            }
            collect_str_expr_reads(value, reads);
        }
        _ => {
            // Generic read-side fallback: traverse expressions via
            // the visitor. We don't need a perfectly exhaustive list
            // for correctness — missing reads are conservative
            // (they look like "unused def" → potentially over-split,
            // which still produces correct code).
            struct R<'a> {
                reads: &'a mut HashSet<VarName>,
            }
            impl<'a> crate::visit::Visitor for R<'a> {
                fn visit_var_read(&mut self, v: &VarName) {
                    self.reads.insert(v.clone());
                }
            }
            let mut r = R { reads };
            crate::visit::walk_stmt(&mut r, 0, stmt);
        }
    }
}

fn collect_expr_reads(e: &Expr, reads: &mut HashSet<VarName>) {
    use Expr::*;
    match e {
        Var(v) => {
            reads.insert(v.clone());
        }
        Number(_) | String(_) => {}
        Neg(inner) | Not(inner) | Func1(_, inner) | Peek(inner) | MemPeek(inner)
        | Pos(inner) | Fre(inner) | Usr(inner) | Joy(inner)
        | Pot(inner) => collect_expr_reads(inner, reads),
        Bin(_, l, r) => {
            collect_expr_reads(l, reads);
            collect_expr_reads(r, reads);
        }
        ArrayRef(_, idx) => {
            for x in idx {
                collect_expr_reads(x, reads);
            }
        }
        _ => collect_expr_reads_fallback(e, reads),
    }
}

fn collect_print_piece_reads(piece: &PrintPiece, reads: &mut HashSet<VarName>) {
    match piece {
        PrintPiece::Expr(e)
        | PrintPiece::CharOut(e)
        | PrintPiece::TabTo(e)
        | PrintPiece::Spc(e) => collect_expr_reads(e, reads),
        PrintPiece::PositionAt(a, b) => {
            collect_expr_reads(a, reads);
            collect_expr_reads(b, reads);
        }
        PrintPiece::StrExpr(s) => collect_str_expr_reads(s, reads),
        PrintPiece::LiteralString(_) | PrintPiece::Tab => {}
    }
}

/// Walk a StrExpr collecting Var reads from its numeric sub-trees.
/// String-typed Var leaves are out of scope — the splitter targets
/// numeric scalars only.
fn collect_str_expr_reads(s: &StrExpr, reads: &mut HashSet<VarName>) {
    match s {
        StrExpr::Literal(_) | StrExpr::Var(_) | StrExpr::GetKey => {}
        StrExpr::Chr(e) | StrExpr::Str(e) | StrExpr::HexFmt(e) | StrExpr::BinFmt(e) => {
            collect_expr_reads(e, reads);
        }
        StrExpr::Concat(a, b) => {
            collect_str_expr_reads(a, reads);
            collect_str_expr_reads(b, reads);
        }
        StrExpr::Left(s, n) | StrExpr::Right(s, n) => {
            collect_str_expr_reads(s, reads);
            collect_expr_reads(n, reads);
        }
        StrExpr::Mid(s, st, n) => {
            collect_str_expr_reads(s, reads);
            collect_expr_reads(st, reads);
            if let Some(boxed) = n {
                collect_expr_reads(boxed, reads);
            }
        }
        StrExpr::Dup(s, n) => {
            collect_str_expr_reads(s, reads);
            collect_expr_reads(n, reads);
        }
        StrExpr::Insert(s, t, pos) => {
            collect_str_expr_reads(s, reads);
            collect_str_expr_reads(t, reads);
            collect_expr_reads(pos, reads);
        }
        StrExpr::ArrayRef(_, idx) => {
            for e in idx {
                collect_expr_reads(e, reads);
            }
        }
    }
}

fn collect_expr_reads_fallback(e: &Expr, reads: &mut HashSet<VarName>) {
    struct R<'a> {
        reads: &'a mut HashSet<VarName>,
    }
    impl<'a> crate::visit::Visitor for R<'a> {
        fn visit_var_read(&mut self, v: &VarName) {
            self.reads.insert(v.clone());
        }
    }
    let mut r = R { reads };
    crate::visit::walk_expr(&mut r, e);
}

/// Forward reaching definitions for every candidate variable at once, as one bitset per CFG node.
struct ReachingDefs {
    /// `u64` words per node.
    words: usize,
    /// `in`-sets, `words` entries per node, indexed by node id.
    in_: Vec<u64>,
    /// First bit owned by candidate `v`.
    bit_base: Vec<usize>,
}

impl ReachingDefs {

    fn iter_in(&self, node: usize, var_slot: usize) -> impl Iterator<Item = usize> + '_ {
        let lo = self.bit_base[var_slot];
        let hi = self.bit_base[var_slot + 1];
        let base = node * self.words;
        // Only the words this variable's run touches can contribute.
        let (w0, w1) = (lo / 64, hi.div_ceil(64));
        self.in_[base + w0..base + w1]
            .iter()
            .enumerate()
            .flat_map(move |(i, &bits)| {
                let w = w0 + i;
                std::iter::successors((bits != 0).then_some(bits), |b| {
                    let rest = b & (b - 1);
                    (rest != 0).then_some(rest)
                })
                .map(move |b| w * 64 + b.trailing_zeros() as usize)
            })
            .filter(move |&bit| bit >= lo && bit < hi)
            .map(move |bit| bit - lo)
    }

    fn mask_of(&self, var_slot: usize, locals: &[usize]) -> Vec<u64> {
        let lo = self.bit_base[var_slot];
        let mut mask = vec![0u64; self.words];
        for &i in locals {
            let bit = lo + i;
            mask[bit / 64] |= 1u64 << (bit % 64);
        }
        mask
    }

    /// True when any write in `mask` reaches `node` on entry.
    fn intersects(&self, node: usize, mask: &[u64]) -> bool {
        let base = node * self.words;
        self.in_[base..base + self.words]
            .iter()
            .zip(mask)
            .any(|(a, b)| a & b != 0)
    }
}

/// `candidates` is `(var, writes, reads)` per variable; only `writes` is read here.
fn reaching_defs(
    cfg: &crate::cfg::Cfg,
    candidates: &[(&VarName, &Vec<usize>, &Vec<usize>)],
) -> ReachingDefs {
    let n = cfg.nodes.len();
    // `bit_base[v] .. bit_base[v + 1]` is candidate `v`'s bit run; the extra trailing entry is the total bit count.
    let mut bit_base: Vec<usize> = Vec::with_capacity(candidates.len() + 1);
    let mut total = 0usize;
    for (_, writes, _) in candidates {
        bit_base.push(total);
        total += writes.len();
    }
    bit_base.push(total);
    let words = total.div_ceil(64).max(1);

    // Per node: the bits it GENs, and the variables whose runs it KILLs.
    let mut gen_bits: Vec<Vec<u32>> = vec![Vec::new(); n];
    let mut kill_slots: Vec<Vec<u32>> = vec![Vec::new(); n];
    for (slot, (_, writes, _)) in candidates.iter().enumerate() {
        for (k, &node) in writes.iter().enumerate() {
            if node >= n {
                continue;
            }
            gen_bits[node].push((bit_base[slot] + k) as u32);
            kill_slots[node].push(slot as u32);
        }
    }
    // Kill masks are per variable and reused across nodes.
    let mut kill_mask: Vec<Vec<u64>> = Vec::with_capacity(candidates.len());
    for slot in 0..candidates.len() {
        let mut m = vec![0u64; words];
        for bit in bit_base[slot]..bit_base[slot + 1] {
            m[bit / 64] |= 1u64 << (bit % 64);
        }
        kill_mask.push(m);
    }

    let mut in_: Vec<u64> = vec![0; n * words];
    let mut out: Vec<u64> = vec![0; n * words];
    let mut scratch: Vec<u64> = vec![0; words];

    let mut dirty: Vec<bool> = vec![true; n];
    loop {
        let mut any_changed = false;
        for node in 0..n {
            if !dirty[node] {
                continue;
            }
            dirty[node] = false;
            scratch.fill(0);
            for &pred in &cfg.nodes[node].predecessors {
                let pbase = pred * words;
                for w in 0..words {
                    scratch[w] |= out[pbase + w];
                }
            }
            let base = node * words;
            if in_[base..base + words] != scratch[..] {
                in_[base..base + words].copy_from_slice(&scratch);
            }

            for &slot in &kill_slots[node] {
                let m = &kill_mask[slot as usize];
                for w in 0..words {
                    scratch[w] &= !m[w];
                }
            }
            for &bit in &gen_bits[node] {
                scratch[bit as usize / 64] |= 1u64 << (bit as usize % 64);
            }
            if out[base..base + words] != scratch[..] {
                out[base..base + words].copy_from_slice(&scratch);
                // Successors read `out` and nothing else, so a node whose `in` moved but whose `out` did not cannot change any of them.
                for &succ in &cfg.nodes[node].successors {
                    dirty[succ] = true;
                }
                any_changed = true;
            }
        }
        if !any_changed {
            break;
        }
    }
    ReachingDefs {
        words,
        in_,
        bit_base,
    }
}

/// Rewrite var references in the IR according to the per-(var, node)
/// rename maps produced by `SplitMultiTypeVars::run`. Writes lookup
/// `rename_writes`; reads lookup `rename_reads`.
fn apply_var_split_renames(
    module: &mut ir::Module,
    cfg: &crate::cfg::Cfg,
    rename_writes: &HashMap<(VarName, usize), VarName>,
    rename_reads: &HashMap<(VarName, usize), VarName>,
) {
    // CFG nodes carry StmtPaths; resolve each to the actual Stmt
    // and mutate it. We do this without a generic mut visitor
    // because the rename is per-node, not whole-program.
    for (node_id, node) in cfg.nodes.iter().enumerate() {
        let path = &node.stmt;
        let stmt = stmt_at_mut(module, path);
        rename_stmt(stmt, node_id, rename_writes, rename_reads);
    }
}

/// Mutable variant of `Cfg::stmt_at`.
fn stmt_at_mut<'a>(module: &'a mut ir::Module, path: &crate::cfg::StmtPath) -> &'a mut Stmt {
    let mut s = &mut module.lines[path.line_idx].stmts[path.path[0]];
    for &idx in &path.path[1..] {
        match s {
            Stmt::If {
                then: ThenIr::Stmts(inner),
                ..
            } => s = &mut inner[idx],
            Stmt::IfElse {
                then,
                else_then,
                ..
            } => {
                let then_len = match then {
                    ThenIr::Stmts(inner) => {
                        if idx < inner.len() {
                            s = &mut inner[idx];
                            continue;
                        }
                        inner.len()
                    }
                    _ => 0,
                };
                if let ThenIr::Stmts(inner) = else_then {
                    s = &mut inner[idx - then_len];
                } else {
                    unreachable!("invalid IF/ELSE path");
                }
            }
            Stmt::Rcomp { then, else_then } => {
                let then_len = match then {
                    ThenIr::Stmts(inner) => {
                        if idx < inner.len() {
                            s = &mut inner[idx];
                            continue;
                        }
                        inner.len()
                    }
                    _ => 0,
                };
                if let Some(ThenIr::Stmts(inner)) = else_then {
                    s = &mut inner[idx - then_len];
                } else {
                    unreachable!("invalid RCOMP path");
                }
            }
            _ => panic!("split-pass StmtPath traversed non-IF/Stmts statement"),
        }
    }
    s
}

/// Rewrite both write-side and read-side var references in `stmt`
/// according to the per-(var, node) rename maps. Writes are handled
/// per-variant; reads use a generic MutVisitor that walks every
/// numeric expression in the stmt and rewrites Var leaves.
fn rename_stmt(
    stmt: &mut Stmt,
    node_id: usize,
    rename_writes: &HashMap<(VarName, usize), VarName>,
    rename_reads: &HashMap<(VarName, usize), VarName>,
) {
    // Write-side: only LHS of writing statements.
    match stmt {
        Stmt::Let { var, .. } => {
            if let Some(new) = rename_writes.get(&(var.clone(), node_id)) {
                *var = new.clone();
            }
        }
        Stmt::For { var, .. } => {
            if let Some(new) = rename_writes.get(&(var.clone(), node_id)) {
                *var = new.clone();
            }
        }
        Stmt::Read(targets) | Stmt::Input { targets, .. } => {
            for t in targets {
                if let ir::ReadTarget::Scalar(v) = t {
                    if let Some(new) = rename_writes.get(&(v.clone(), node_id)) {
                        *v = new.clone();
                    }
                }
            }
        }

        Stmt::Get { var, indices } if indices.is_empty() => {
            if let Some(new) = rename_writes.get(&(var.clone(), node_id)) {
                *var = new.clone();
            }
        }
        Stmt::GetFile { vars, .. } => {
            for v in vars {
                if let Some(new) = rename_writes.get(&(v.clone(), node_id)) {
                    *v = new.clone();
                }
            }
        }
        Stmt::Next { vars } => {
            for v in vars.iter_mut().flatten() {
                if let Some(new) = rename_writes.get(&(v.clone(), node_id)) {
                    *v = new.clone();
                }
            }
        }
        _ => {}
    }
    // Read-side: walk every Expr / StrExpr in the stmt via a
    // MutVisitor and rewrite Var leaves. The IF-body recursion is
    // OFF because each inner stmt has its own CFG node and is
    // processed separately by the outer caller.
    struct Renamer<'a> {
        node_id: usize,
        rename_reads: &'a HashMap<(VarName, usize), VarName>,
    }
    impl<'a> crate::visit::MutVisitor for Renamer<'a> {
        fn visit_expr_mut(&mut self, e: &mut Expr) {
            if let Expr::Var(v) = e {
                if let Some(new) = self.rename_reads.get(&(v.clone(), self.node_id)) {
                    *v = new.clone();
                }
                return;
            }
            crate::visit::walk_expr_mut(self, e);
        }
        fn visit_str_expr_mut(&mut self, s: &mut StrExpr) {
            crate::visit::walk_str_expr_mut(self, s);
        }
        fn visit_stmt_mut(&mut self, _line_no: u16, _stmt: &mut Stmt) {
            // Stop at stmt boundary — caller drives the per-node
            // dispatch; nested-if bodies are separate CFG nodes
            // and will be visited with their own rename maps.
        }
    }
    let mut r = Renamer {
        node_id,
        rename_reads,
    };
    // Walk only the immediate expressions of this stmt, NOT inner
    // stmts. We achieve that by using the dedicated expression-
    // visitor entries for each variant.
    walk_stmt_exprs_mut(stmt, &mut r);
}

/// Walk only the EXPRESSIONS directly held by `stmt` (skip inner
/// statement bodies). Mirrors `walk_stmt_mut` minus the
/// recursion into IF/IFELSE/RCOMP bodies.
fn walk_stmt_exprs_mut<V: crate::visit::MutVisitor>(stmt: &mut Stmt, v: &mut V) {
    use Stmt::*;
    match stmt {
        Let { value, .. } => v.visit_expr_mut(value),
        LetStr { value, .. } => v.visit_str_expr_mut(value),
        ArrayLet { indices, value, .. } => {
            for e in indices {
                v.visit_expr_mut(e);
            }
            v.visit_expr_mut(value);
        }
        ArrayLetStr { indices, value, .. } => {
            for e in indices {
                v.visit_expr_mut(e);
            }
            v.visit_str_expr_mut(value);
        }
        If { cond, .. } => v.visit_expr_mut(cond),
        IfElse { cond, .. } => v.visit_expr_mut(cond),
        For { start, end, step, .. } => {
            v.visit_expr_mut(start);
            v.visit_expr_mut(end);
            v.visit_expr_mut(step);
        }
        DoIf { cond } | Until { cond } => v.visit_expr_mut(cond),
        ExitLoop { cond } => {
            if let Some(c) = cond {
                v.visit_expr_mut(c);
            }
        }
        ComputedGoto { target } => v.visit_expr_mut(target),
        Reset { target } => v.visit_expr_mut(target),
        Poke { addr, value } | Dpoke { addr, value } => {
            v.visit_expr_mut(addr);
            v.visit_expr_mut(value);
        }
        Sys {
            addr,
            regs,
            param_vars,
            ..
        } => {
            v.visit_expr_mut(addr);
            for e in regs {
                v.visit_expr_mut(e);
            }
            for p in param_vars {
                v.visit_expr_mut(&mut p.value);
            }
        }
        OnBranch { value, .. } => v.visit_expr_mut(value),
        Print { items, .. } => {
            for piece in items {
                match piece {
                    PrintPiece::Expr(e)
                    | PrintPiece::CharOut(e)
                    | PrintPiece::TabTo(e)
                    | PrintPiece::Spc(e) => v.visit_expr_mut(e),
                    PrintPiece::PositionAt(a, b) => {
                        v.visit_expr_mut(a);
                        v.visit_expr_mut(b);
                    }
                    PrintPiece::StrExpr(s) => v.visit_str_expr_mut(s),
                    _ => {}
                }
            }
        }
        GetFile { file_num, .. } => v.visit_expr_mut(file_num),
        InputFile { file_num, .. } => v.visit_expr_mut(file_num),
        Cmd { file_num, items, .. } => {
            v.visit_expr_mut(file_num);
            for piece in items {
                match piece {
                    PrintPiece::Expr(e)
                    | PrintPiece::CharOut(e)
                    | PrintPiece::TabTo(e)
                    | PrintPiece::Spc(e) => v.visit_expr_mut(e),
                    PrintPiece::PositionAt(a, b) => {
                        v.visit_expr_mut(a);
                        v.visit_expr_mut(b);
                    }
                    PrintPiece::StrExpr(s) => v.visit_str_expr_mut(s),
                    _ => {}
                }
            }
        }
        _ => {
            // Long-tail variants: many wrap expressions in odd
            // ways. Fall back to the generic walker which DOES
            // visit inner stmts, but the renamer's
            // `visit_stmt_mut` is a no-op so the recursion is
            // harmless — only this stmt's own expressions get
            // visited.
            crate::visit::walk_stmt_mut(v, 0, stmt);
        }
    }
}
