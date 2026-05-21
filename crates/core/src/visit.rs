#![allow(dead_code)]
// framework module — mid-migration, several
// walkers/methods will only see use as we port
// remaining ad-hoc walkers in passes.rs.

//! Visitor framework for the IR.
//!
//! Two traits — `Visitor` for read-only walks (analyses) and
//! `MutVisitor` for in-place transformations (optimisations) — with
//! default methods that recursively walk every child node. Analyses
//! that only care about a specific node kind override that one method
//! and call `walk_*(self, node)` to keep descending; everything else
//! is free.
//!
//! The dual `walk_*` free functions are exposed so an overriding
//! impl can opt in to the default child-traversal — the same pattern
//! rustc and most compiler frameworks use.

use crate::ast::VarName;
use crate::ir::{Expr, Module, PrintPiece, Stmt, StrExpr, ThenIr};

// ===== Read-only visitor =====

/// Visitor for read-only IR walks. Override the methods for the node
/// kinds you care about; everything else is reached by the default
/// `walk_*` calls.
pub trait Visitor {
    fn visit_module(&mut self, m: &Module) {
        walk_module(self, m);
    }
    fn visit_line(&mut self, line_no: u16, stmts: &[Stmt]) {
        walk_line(self, line_no, stmts);
    }
    fn visit_stmt(&mut self, line_no: u16, stmt: &Stmt) {
        walk_stmt(self, line_no, stmt);
    }
    fn visit_expr(&mut self, e: &Expr) {
        walk_expr(self, e);
    }
    fn visit_str_expr(&mut self, s: &StrExpr) {
        walk_str_expr(self, s);
    }
    fn visit_print_piece(&mut self, p: &PrintPiece) {
        walk_print_piece(self, p);
    }
    /// Called for every variable READ during expression traversal.
    /// Hook here when you only need to enumerate var uses.
    fn visit_var_read(&mut self, _v: &VarName) {}
}

pub fn walk_module<V: Visitor + ?Sized>(v: &mut V, m: &Module) {
    for line in &m.lines {
        v.visit_line(line.number, &line.stmts);
    }
}

pub fn walk_line<V: Visitor + ?Sized>(v: &mut V, line_no: u16, stmts: &[Stmt]) {
    for stmt in stmts {
        v.visit_stmt(line_no, stmt);
    }
}

pub fn walk_stmt<V: Visitor + ?Sized>(v: &mut V, line_no: u16, stmt: &Stmt) {
    use Stmt::*;
    match stmt {
        Let { value, .. } => v.visit_expr(value),
        LetStr { value, .. } => v.visit_str_expr(value),
        ArrayLet { indices, value, .. } => {
            for e in indices {
                v.visit_expr(e);
            }
            v.visit_expr(value);
        }
        ArrayLetStr { indices, value, .. } => {
            for e in indices {
                v.visit_expr(e);
            }
            v.visit_str_expr(value);
        }
        If { cond, then } => {
            v.visit_expr(cond);
            if let ThenIr::Stmts(inner) = then {
                for s in inner {
                    v.visit_stmt(line_no, s);
                }
            }
        }
        IfElse {
            cond,
            then,
            else_then,
        } => {
            v.visit_expr(cond);
            if let ThenIr::Stmts(inner) = then {
                for s in inner {
                    v.visit_stmt(line_no, s);
                }
            }
            if let ThenIr::Stmts(inner) = else_then {
                for s in inner {
                    v.visit_stmt(line_no, s);
                }
            }
        }
        DoIf { cond } | Until { cond } => v.visit_expr(cond),
        ExitLoop { cond } => {
            if let Some(cond) = cond {
                v.visit_expr(cond);
            }
        }
        ComputedGoto { target } => v.visit_expr(target),
        Rcomp { then, else_then } => {
            if let ThenIr::Stmts(inner) = then {
                for s in inner {
                    v.visit_stmt(line_no, s);
                }
            }
            if let Some(ThenIr::Stmts(inner)) = else_then {
                for s in inner {
                    v.visit_stmt(line_no, s);
                }
            }
        }
        For {
            start, end, step, ..
        } => {
            v.visit_expr(start);
            v.visit_expr(end);
            v.visit_expr(step);
        }
        Read(targets) | Input { targets, .. } => {
            for t in targets {
                if let crate::ir::ReadTarget::Array { indices, .. } = t {
                    for e in indices {
                        v.visit_expr(e);
                    }
                }
            }
        }
        InputFile { file_num, targets } => {
            v.visit_expr(file_num);
            for t in targets {
                if let crate::ir::ReadTarget::Array { indices, .. } = t {
                    for e in indices {
                        v.visit_expr(e);
                    }
                }
            }
        }
        Poke { addr, value } => {
            v.visit_expr(addr);
            v.visit_expr(value);
        }
        Dpoke { addr, value } => {
            v.visit_expr(addr);
            v.visit_expr(value);
        }
        PokeFill {
            dst_start,
            dst_end,
            value,
        } => {
            v.visit_expr(dst_start);
            v.visit_expr(dst_end);
            v.visit_expr(value);
        }
        ScreenRect {
            row,
            col,
            width,
            height,
            ch,
            color,
            ..
        } => {
            v.visit_expr(row);
            v.visit_expr(col);
            v.visit_expr(width);
            v.visit_expr(height);
            if let Some(e) = ch {
                v.visit_expr(e);
            }
            if let Some(e) = color {
                v.visit_expr(e);
            }
        }
        ScreenMove {
            row,
            col,
            width,
            height,
            dest_row,
            dest_col,
        } => {
            v.visit_expr(row);
            v.visit_expr(col);
            v.visit_expr(width);
            v.visit_expr(height);
            v.visit_expr(dest_row);
            v.visit_expr(dest_col);
        }
        ScreenScroll {
            row,
            col,
            width,
            height,
            ..
        } => {
            v.visit_expr(row);
            v.visit_expr(col);
            v.visit_expr(width);
            v.visit_expr(height);
        }
        Color {
            border,
            background,
            pen,
        } => {
            if let Some(e) = border {
                v.visit_expr(e);
            }
            if let Some(e) = background {
                v.visit_expr(e);
            }
            if let Some(e) = pen {
                v.visit_expr(e);
            }
        }
        MobEnable { index, .. } => v.visit_expr(index),
        Multi { .. } | HiCol => {}
        MultiColors { c1, c2, c3 } => {
            v.visit_expr(c1);
            v.visit_expr(c2);
            v.visit_expr(c3);
        }
        Hires { ink, paper } => {
            if let Some(e) = ink {
                v.visit_expr(e);
            }
            if let Some(e) = paper {
                v.visit_expr(e);
            }
        }
        Border { color } => v.visit_expr(color),
        Line {
            x1,
            y1,
            x2,
            y2,
            mode,
        }
        | Block {
            x1,
            y1,
            x2,
            y2,
            mode,
        } => {
            v.visit_expr(x1);
            v.visit_expr(y1);
            v.visit_expr(x2);
            v.visit_expr(y2);
            if let Some(e) = mode {
                v.visit_expr(e);
            }
        }
        Rec {
            x,
            y,
            width,
            height,
            mode,
        } => {
            v.visit_expr(x);
            v.visit_expr(y);
            v.visit_expr(width);
            v.visit_expr(height);
            if let Some(e) = mode {
                v.visit_expr(e);
            }
        }
        Draw { x, y, mode } | DrawTo { x, y, mode } | Paint { x, y, mode } => {
            v.visit_expr(x);
            v.visit_expr(y);
            if let Some(e) = mode {
                v.visit_expr(e);
            }
        }
        Circle {
            cx,
            cy,
            radius,
            ry,
            start,
            end,
            step,
            mode,
        } => {
            v.visit_expr(cx);
            v.visit_expr(cy);
            v.visit_expr(radius);
            for opt in [ry, start, end, step, mode] {
                if let Some(e) = opt {
                    v.visit_expr(e);
                }
            }
        }
        Char {
            x,
            y,
            code,
            mode,
            zoom,
        } => {
            v.visit_expr(x);
            v.visit_expr(y);
            v.visit_expr(code);
            if let Some(e) = mode {
                v.visit_expr(e);
            }
            if let Some(e) = zoom {
                v.visit_expr(e);
            }
        }
        Text {
            x,
            y,
            text,
            mode,
            zoom,
            kerning,
        } => {
            v.visit_expr(x);
            v.visit_expr(y);
            v.visit_str_expr(text);
            if let Some(e) = mode {
                v.visit_expr(e);
            }
            if let Some(e) = zoom {
                v.visit_expr(e);
            }
            if let Some(e) = kerning {
                v.visit_expr(e);
            }
        }
        Rot { direction, length } => {
            v.visit_expr(direction);
            if let Some(l) = length {
                v.visit_expr(l);
            }
        }
        DrawString { code, x, y, mode } => {
            v.visit_str_expr(code);
            v.visit_expr(x);
            v.visit_expr(y);
            if let Some(e) = mode {
                v.visit_expr(e);
            }
        }
        Angl {
            cx,
            cy,
            angle,
            rx,
            ry,
            mode,
        } => {
            v.visit_expr(cx);
            v.visit_expr(cy);
            v.visit_expr(angle);
            v.visit_expr(rx);
            for opt in [ry, mode] {
                if let Some(e) = opt {
                    v.visit_expr(e);
                }
            }
        }
        Sound { voice, freq } => {
            v.visit_expr(voice);
            v.visit_expr(freq);
        }
        Envelope {
            voice,
            attack,
            decay,
            sustain,
            release,
        } => {
            v.visit_expr(voice);
            v.visit_expr(attack);
            v.visit_expr(decay);
            v.visit_expr(sustain);
            v.visit_expr(release);
        }
        Wave {
            voice,
            control,
            pulse,
        } => {
            v.visit_expr(voice);
            v.visit_expr(control);
            if let Some(e) = pulse {
                v.visit_expr(e);
            }
        }
        Music { tempo, tune } => {
            v.visit_expr(tempo);
            v.visit_str_expr(tune);
        }
        Play { mode } => v.visit_expr(mode),
        Flash {
            speed,
            color1,
            color2,
            ..
        }
        | Bflash {
            speed,
            color1,
            color2,
            ..
        } => {
            for opt in [speed, color1, color2] {
                if let Some(e) = opt {
                    v.visit_expr(e);
                }
            }
        }
        LowCol {
            color1,
            color2,
            color3,
        } => {
            v.visit_expr(color1);
            v.visit_expr(color2);
            if let Some(e) = color3 {
                v.visit_expr(e);
            }
        }
        Mod { ink, paper } => {
            v.visit_expr(ink);
            v.visit_expr(paper);
        }
        Dup {
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
                v.visit_expr(e);
            }
            for opt in [mode, zoom] {
                if let Some(e) = opt {
                    v.visit_expr(e);
                }
            }
        }
        Copy { src, dst, len } => {
            v.visit_expr(src);
            v.visit_expr(dst);
            v.visit_expr(len);
        }
        ScrSave { addr, mode } | ScrLoad { addr, mode } => {
            if let Some(e) = addr {
                v.visit_expr(e);
            }
            if let Some(e) = mode {
                v.visit_expr(e);
            }
        }
        ScrDef { addr, mode, .. } => {
            v.visit_expr(addr);
            if let Some(e) = mode {
                v.visit_expr(e);
            }
        }
        ScrRestore { .. } => {}
        MemClr { addr, len, value } => {
            v.visit_expr(addr);
            v.visit_expr(len);
            if let Some(e) = value {
                v.visit_expr(e);
            }
        }
        MemTransfer { .. } => {}
        MemDef {
            len,
            c64_addr,
            reu_addr,
            reu_bank,
            auto_inc,
            fixed,
        } => {
            v.visit_expr(len);
            for e in [c64_addr, reu_addr, reu_bank, auto_inc, fixed]
                .into_iter()
                .flatten()
            {
                v.visit_expr(e);
            }
        }
        MemLen { len } => v.visit_expr(len),
        MemC64Addr { addr } => v.visit_expr(addr),
        MemReuPos { addr, bank } => {
            v.visit_expr(addr);
            v.visit_expr(bank);
        }
        MemRestore { auto_inc } => v.visit_expr(auto_inc),
        MemCont { mode } => v.visit_expr(mode),
        Design { addr, bytes } => {
            v.visit_expr(addr);
            for e in bytes {
                v.visit_expr(e);
            }
        }
        Mmob { index, x, y } => {
            v.visit_expr(index);
            v.visit_expr(x);
            v.visit_expr(y);
        }
        MmobGlide {
            index,
            sx,
            sy,
            ex,
            ey,
            size,
            speed,
        } => {
            v.visit_expr(index);
            v.visit_expr(sx);
            v.visit_expr(sy);
            v.visit_expr(ex);
            v.visit_expr(ey);
            if let Some(e) = size {
                v.visit_expr(e);
            }
            if let Some(e) = speed {
                v.visit_expr(e);
            }
        }
        MobSet {
            index,
            block,
            color,
            priority,
            multicolor,
            size,
            speed,
        } => {
            v.visit_expr(index);
            v.visit_expr(block);
            v.visit_expr(color);
            v.visit_expr(priority);
            v.visit_expr(multicolor);
            if let Some(e) = size {
                v.visit_expr(e);
            }
            if let Some(e) = speed {
                v.visit_expr(e);
            }
        }
        Rlocmob {
            index,
            dx,
            dy,
            speed,
        } => {
            v.visit_expr(index);
            v.visit_expr(dx);
            v.visit_expr(dy);
            if let Some(e) = speed {
                v.visit_expr(e);
            }
        }
        Detect { mode } => v.visit_expr(mode),
        Cmob { color1, color2 } => {
            v.visit_expr(color1);
            v.visit_expr(color2);
        }
        Bckgnds {
            color0,
            color1,
            color2,
            color3,
        } => {
            v.visit_expr(color0);
            v.visit_expr(color1);
            v.visit_expr(color2);
            v.visit_expr(color3);
        }
        Cset { mode } => v.visit_expr(mode),
        Pause { message, ticks } => {
            if let Some(m) = message {
                v.visit_str_expr(m);
            }
            v.visit_expr(ticks);
        }
        Sys { addr, regs } => {
            v.visit_expr(addr);
            for r in regs {
                v.visit_expr(r);
            }
        }
        Wait { addr, mask, eor } => {
            v.visit_expr(addr);
            v.visit_expr(mask);
            if let Some(e) = eor {
                v.visit_expr(e);
            }
        }
        Open {
            file_num,
            device,
            secondary,
            filename,
        } => {
            v.visit_expr(file_num);
            if let Some(e) = device {
                v.visit_expr(e);
            }
            if let Some(e) = secondary {
                v.visit_expr(e);
            }
            if let Some(s) = filename {
                v.visit_str_expr(s);
            }
        }
        Close { file_num } | GetFile { file_num, .. } => v.visit_expr(file_num),
        Print { items, .. } | PrintFile { items, .. } | Cmd { items, .. } => {
            for it in items {
                v.visit_print_piece(it);
            }
        }
        Load {
            device,
            secondary,
            load_addr,
            ..
        } => {
            if let Some(e) = device {
                v.visit_expr(e);
            }
            if let Some(e) = secondary {
                v.visit_expr(e);
            }
            if let Some(e) = load_addr {
                v.visit_expr(e);
            }
        }
        Verify {
            device, secondary, ..
        }
        | Save {
            device, secondary, ..
        } => {
            if let Some(e) = device {
                v.visit_expr(e);
            }
            if let Some(e) = secondary {
                v.visit_expr(e);
            }
        }
        Disk { command } => v.visit_str_expr(command),
        OnBranch { value, .. } => v.visit_expr(value),
        DefFn { body, .. } => v.visit_expr(body),
        OnKey { keys, .. } => v.visit_str_expr(keys),
        Fetch {
            control,
            max_len,
            force,
            position,
            ..
        } => {
            v.visit_str_expr(control);
            v.visit_expr(max_len);
            if let Some(e) = force {
                v.visit_expr(e);
            }
            if let Some((r, c)) = position {
                v.visit_expr(r);
                v.visit_expr(c);
            }
        }
        KeySet { index, text } => {
            v.visit_expr(index);
            v.visit_str_expr(text);
        }
        Dim(specs) => {
            for spec in specs {
                for d in &spec.dims {
                    v.visit_expr(d);
                }
            }
        }
        // Leaf statements with no child expressions.
        Goto { .. }
        | GoSub { .. }
        | Return
        | Next { .. }
        | Do
        | DoNull
        | Done
        | Else
        | Repeat
        | Loop
        | EndLoop
        | Disable
        | Resume { .. }
        | OnError { .. }
        | Nrm
        | MemModeOn
        | Rem(_)
        | End
        | Stop
        | Run(_)
        | Clr
        | Data(_)
        | Get { .. }
        | KeyGet { .. }
        | DisplayKeys
        | SwapStr { .. }
        | Restore
        | Reset { .. } => {}
        InsertBox {
            pattern,
            row,
            col,
            width,
            height,
            color,
        } => {
            v.visit_str_expr(pattern);
            for e in [row, col, width, height, color] {
                v.visit_expr(e);
            }
        }
        ErrorRaise { code } => v.visit_expr(code),
    }
}

pub fn walk_expr<V: Visitor + ?Sized>(v: &mut V, e: &Expr) {
    use Expr::*;
    match e {
        Var(name) => v.visit_var_read(name),
        Number(_) | String(_) => {}
        Neg(inner) | Not(inner) => v.visit_expr(inner),
        Bin(_, l, r) => {
            v.visit_expr(l);
            v.visit_expr(r);
        }
        Func1(_, arg)
        | Peek(arg)
        | MemPeek(arg)
        | FnCall(_, arg)
        | Pos(arg)
        | Fre(arg)
        | Usr(arg)
        | Joy(arg)
        | Pot(arg) => v.visit_expr(arg),
        ArrayRef(_, idx) => {
            for e in idx {
                v.visit_expr(e);
            }
        }
        Len(s) | Asc(s) | Val(s) | Nrm(s) => v.visit_str_expr(s),
        StrCompare(_, l, r) => {
            v.visit_str_expr(l);
            v.visit_str_expr(r);
        }
        At(row, col) => {
            v.visit_expr(row);
            v.visit_expr(col);
        }
        Test(x, y) => {
            v.visit_expr(x);
            v.visit_expr(y);
        }
        Check { first, second } => {
            v.visit_expr(first);
            if let Some(e) = second {
                v.visit_expr(e);
            }
        }
        Inst {
            haystack,
            needle,
            start,
        } => {
            v.visit_str_expr(haystack);
            v.visit_str_expr(needle);
            if let Some(e) = start {
                v.visit_expr(e);
            }
        }
        Inkey | Lin => {}
    }
}

pub fn walk_str_expr<V: Visitor + ?Sized>(v: &mut V, s: &StrExpr) {
    use StrExpr::*;
    match s {
        Var(name) => v.visit_var_read(name),
        Literal(_) | GetKey => {}
        Chr(e) | Str(e) | HexFmt(e) | BinFmt(e) => v.visit_expr(e),
        Concat(a, b) => {
            v.visit_str_expr(a);
            v.visit_str_expr(b);
        }
        Left(s, n) | Right(s, n) => {
            v.visit_str_expr(s);
            v.visit_expr(n);
        }
        Mid(s, st, n) => {
            v.visit_str_expr(s);
            v.visit_expr(st);
            if let Some(boxed) = n {
                v.visit_expr(boxed);
            }
        }
        Dup(s, n) => {
            v.visit_str_expr(s);
            v.visit_expr(n);
        }
        Insert(s, t, pos) => {
            v.visit_str_expr(s);
            v.visit_str_expr(t);
            v.visit_expr(pos);
        }
        ArrayRef(_, idx) => {
            for e in idx {
                v.visit_expr(e);
            }
        }
    }
}

pub fn walk_print_piece<V: Visitor + ?Sized>(v: &mut V, p: &PrintPiece) {
    use PrintPiece::*;
    match p {
        Expr(e) | CharOut(e) | TabTo(e) | Spc(e) => v.visit_expr(e),
        StrExpr(s) => v.visit_str_expr(s),
        PositionAt(r, c) => {
            v.visit_expr(r);
            v.visit_expr(c);
        }
        UseField { value, .. } => v.visit_expr(value),
        LiteralString(_) | Tab => {}
    }
}

// ===== Mutating visitor =====

/// Visitor for in-place IR transformations. Same shape as `Visitor`
/// but every traversal node is `&mut`. Used by passes that rewrite
/// nodes (constant folding, variable substitution, GOTO chain
/// folding, etc.).
pub trait MutVisitor {
    fn visit_module_mut(&mut self, m: &mut Module) {
        walk_module_mut(self, m);
    }
    fn visit_line_mut(&mut self, line_no: u16, stmts: &mut Vec<Stmt>) {
        walk_line_mut(self, line_no, stmts);
    }
    fn visit_stmt_mut(&mut self, line_no: u16, stmt: &mut Stmt) {
        walk_stmt_mut(self, line_no, stmt);
    }
    fn visit_expr_mut(&mut self, e: &mut Expr) {
        walk_expr_mut(self, e);
    }
    fn visit_str_expr_mut(&mut self, s: &mut StrExpr) {
        walk_str_expr_mut(self, s);
    }
    fn visit_print_piece_mut(&mut self, p: &mut PrintPiece) {
        walk_print_piece_mut(self, p);
    }
}

pub fn walk_module_mut<V: MutVisitor + ?Sized>(v: &mut V, m: &mut Module) {
    for line in m.lines.iter_mut() {
        let line_no = line.number;
        v.visit_line_mut(line_no, &mut line.stmts);
    }
}

pub fn walk_line_mut<V: MutVisitor + ?Sized>(v: &mut V, line_no: u16, stmts: &mut Vec<Stmt>) {
    for stmt in stmts.iter_mut() {
        v.visit_stmt_mut(line_no, stmt);
    }
}

pub fn walk_stmt_mut<V: MutVisitor + ?Sized>(v: &mut V, line_no: u16, stmt: &mut Stmt) {
    use Stmt::*;
    match stmt {
        Let { value, .. } => v.visit_expr_mut(value),
        LetStr { value, .. } => v.visit_str_expr_mut(value),
        ArrayLet { indices, value, .. } => {
            for e in indices.iter_mut() {
                v.visit_expr_mut(e);
            }
            v.visit_expr_mut(value);
        }
        ArrayLetStr { indices, value, .. } => {
            for e in indices.iter_mut() {
                v.visit_expr_mut(e);
            }
            v.visit_str_expr_mut(value);
        }
        If { cond, then } => {
            v.visit_expr_mut(cond);
            if let ThenIr::Stmts(inner) = then {
                for s in inner.iter_mut() {
                    v.visit_stmt_mut(line_no, s);
                }
            }
        }
        IfElse {
            cond,
            then,
            else_then,
        } => {
            v.visit_expr_mut(cond);
            if let ThenIr::Stmts(inner) = then {
                for s in inner.iter_mut() {
                    v.visit_stmt_mut(line_no, s);
                }
            }
            if let ThenIr::Stmts(inner) = else_then {
                for s in inner.iter_mut() {
                    v.visit_stmt_mut(line_no, s);
                }
            }
        }
        DoIf { cond } | Until { cond } => v.visit_expr_mut(cond),
        ExitLoop { cond } => {
            if let Some(cond) = cond {
                v.visit_expr_mut(cond);
            }
        }
        ComputedGoto { target } => v.visit_expr_mut(target),
        Rcomp { then, else_then } => {
            if let ThenIr::Stmts(inner) = then {
                for s in inner.iter_mut() {
                    v.visit_stmt_mut(line_no, s);
                }
            }
            if let Some(ThenIr::Stmts(inner)) = else_then {
                for s in inner.iter_mut() {
                    v.visit_stmt_mut(line_no, s);
                }
            }
        }
        For {
            start, end, step, ..
        } => {
            v.visit_expr_mut(start);
            v.visit_expr_mut(end);
            v.visit_expr_mut(step);
        }
        Read(targets) | Input { targets, .. } => {
            for t in targets.iter_mut() {
                if let crate::ir::ReadTarget::Array { indices, .. } = t {
                    for e in indices.iter_mut() {
                        v.visit_expr_mut(e);
                    }
                }
            }
        }
        InputFile { file_num, targets } => {
            v.visit_expr_mut(file_num);
            for t in targets.iter_mut() {
                if let crate::ir::ReadTarget::Array { indices, .. } = t {
                    for e in indices.iter_mut() {
                        v.visit_expr_mut(e);
                    }
                }
            }
        }
        Poke { addr, value } => {
            v.visit_expr_mut(addr);
            v.visit_expr_mut(value);
        }
        Dpoke { addr, value } => {
            v.visit_expr_mut(addr);
            v.visit_expr_mut(value);
        }
        PokeFill {
            dst_start,
            dst_end,
            value,
        } => {
            v.visit_expr_mut(dst_start);
            v.visit_expr_mut(dst_end);
            v.visit_expr_mut(value);
        }
        ScreenRect {
            row,
            col,
            width,
            height,
            ch,
            color,
            ..
        } => {
            v.visit_expr_mut(row);
            v.visit_expr_mut(col);
            v.visit_expr_mut(width);
            v.visit_expr_mut(height);
            if let Some(e) = ch {
                v.visit_expr_mut(e);
            }
            if let Some(e) = color {
                v.visit_expr_mut(e);
            }
        }
        ScreenMove {
            row,
            col,
            width,
            height,
            dest_row,
            dest_col,
        } => {
            v.visit_expr_mut(row);
            v.visit_expr_mut(col);
            v.visit_expr_mut(width);
            v.visit_expr_mut(height);
            v.visit_expr_mut(dest_row);
            v.visit_expr_mut(dest_col);
        }
        ScreenScroll {
            row,
            col,
            width,
            height,
            ..
        } => {
            v.visit_expr_mut(row);
            v.visit_expr_mut(col);
            v.visit_expr_mut(width);
            v.visit_expr_mut(height);
        }
        Color {
            border,
            background,
            pen,
        } => {
            if let Some(e) = border {
                v.visit_expr_mut(e);
            }
            if let Some(e) = background {
                v.visit_expr_mut(e);
            }
            if let Some(e) = pen {
                v.visit_expr_mut(e);
            }
        }
        MobEnable { index, .. } => v.visit_expr_mut(index),
        Multi { .. } | HiCol => {}
        MultiColors { c1, c2, c3 } => {
            v.visit_expr_mut(c1);
            v.visit_expr_mut(c2);
            v.visit_expr_mut(c3);
        }
        Hires { ink, paper } => {
            if let Some(e) = ink {
                v.visit_expr_mut(e);
            }
            if let Some(e) = paper {
                v.visit_expr_mut(e);
            }
        }
        Border { color } => v.visit_expr_mut(color),
        Line {
            x1,
            y1,
            x2,
            y2,
            mode,
        }
        | Block {
            x1,
            y1,
            x2,
            y2,
            mode,
        } => {
            v.visit_expr_mut(x1);
            v.visit_expr_mut(y1);
            v.visit_expr_mut(x2);
            v.visit_expr_mut(y2);
            if let Some(e) = mode {
                v.visit_expr_mut(e);
            }
        }
        Rec {
            x,
            y,
            width,
            height,
            mode,
        } => {
            v.visit_expr_mut(x);
            v.visit_expr_mut(y);
            v.visit_expr_mut(width);
            v.visit_expr_mut(height);
            if let Some(e) = mode {
                v.visit_expr_mut(e);
            }
        }
        Draw { x, y, mode } | DrawTo { x, y, mode } | Paint { x, y, mode } => {
            v.visit_expr_mut(x);
            v.visit_expr_mut(y);
            if let Some(e) = mode {
                v.visit_expr_mut(e);
            }
        }
        Circle {
            cx,
            cy,
            radius,
            ry,
            start,
            end,
            step,
            mode,
        } => {
            v.visit_expr_mut(cx);
            v.visit_expr_mut(cy);
            v.visit_expr_mut(radius);
            for opt in [ry, start, end, step, mode] {
                if let Some(e) = opt {
                    v.visit_expr_mut(e);
                }
            }
        }
        Char {
            x,
            y,
            code,
            mode,
            zoom,
        } => {
            v.visit_expr_mut(x);
            v.visit_expr_mut(y);
            v.visit_expr_mut(code);
            if let Some(e) = mode {
                v.visit_expr_mut(e);
            }
            if let Some(e) = zoom {
                v.visit_expr_mut(e);
            }
        }
        Text {
            x,
            y,
            text,
            mode,
            zoom,
            kerning,
        } => {
            v.visit_expr_mut(x);
            v.visit_expr_mut(y);
            v.visit_str_expr_mut(text);
            if let Some(e) = mode {
                v.visit_expr_mut(e);
            }
            if let Some(e) = zoom {
                v.visit_expr_mut(e);
            }
            if let Some(e) = kerning {
                v.visit_expr_mut(e);
            }
        }
        Rot { direction, length } => {
            v.visit_expr_mut(direction);
            if let Some(l) = length {
                v.visit_expr_mut(l);
            }
        }
        DrawString { code, x, y, mode } => {
            v.visit_str_expr_mut(code);
            v.visit_expr_mut(x);
            v.visit_expr_mut(y);
            if let Some(e) = mode {
                v.visit_expr_mut(e);
            }
        }
        Angl {
            cx,
            cy,
            angle,
            rx,
            ry,
            mode,
        } => {
            v.visit_expr_mut(cx);
            v.visit_expr_mut(cy);
            v.visit_expr_mut(angle);
            v.visit_expr_mut(rx);
            for opt in [ry, mode] {
                if let Some(e) = opt {
                    v.visit_expr_mut(e);
                }
            }
        }
        Sound { voice, freq } => {
            v.visit_expr_mut(voice);
            v.visit_expr_mut(freq);
        }
        Envelope {
            voice,
            attack,
            decay,
            sustain,
            release,
        } => {
            v.visit_expr_mut(voice);
            v.visit_expr_mut(attack);
            v.visit_expr_mut(decay);
            v.visit_expr_mut(sustain);
            v.visit_expr_mut(release);
        }
        Wave {
            voice,
            control,
            pulse,
        } => {
            v.visit_expr_mut(voice);
            v.visit_expr_mut(control);
            if let Some(e) = pulse {
                v.visit_expr_mut(e);
            }
        }
        Music { tempo, tune } => {
            v.visit_expr_mut(tempo);
            v.visit_str_expr_mut(tune);
        }
        Play { mode } => v.visit_expr_mut(mode),
        Flash {
            speed,
            color1,
            color2,
            ..
        }
        | Bflash {
            speed,
            color1,
            color2,
            ..
        } => {
            for opt in [speed, color1, color2] {
                if let Some(e) = opt {
                    v.visit_expr_mut(e);
                }
            }
        }
        LowCol {
            color1,
            color2,
            color3,
        } => {
            v.visit_expr_mut(color1);
            v.visit_expr_mut(color2);
            if let Some(e) = color3 {
                v.visit_expr_mut(e);
            }
        }
        Mod { ink, paper } => {
            v.visit_expr_mut(ink);
            v.visit_expr_mut(paper);
        }
        Dup {
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
                v.visit_expr_mut(e);
            }
            for opt in [mode, zoom] {
                if let Some(e) = opt {
                    v.visit_expr_mut(e);
                }
            }
        }
        Copy { src, dst, len } => {
            v.visit_expr_mut(src);
            v.visit_expr_mut(dst);
            v.visit_expr_mut(len);
        }
        ScrSave { addr, mode } | ScrLoad { addr, mode } => {
            if let Some(e) = addr {
                v.visit_expr_mut(e);
            }
            if let Some(e) = mode {
                v.visit_expr_mut(e);
            }
        }
        ScrDef { addr, mode, .. } => {
            v.visit_expr_mut(addr);
            if let Some(e) = mode {
                v.visit_expr_mut(e);
            }
        }
        ScrRestore { .. } => {}
        MemClr { addr, len, value } => {
            v.visit_expr_mut(addr);
            v.visit_expr_mut(len);
            if let Some(e) = value {
                v.visit_expr_mut(e);
            }
        }
        MemTransfer { .. } => {}
        MemDef {
            len,
            c64_addr,
            reu_addr,
            reu_bank,
            auto_inc,
            fixed,
        } => {
            v.visit_expr_mut(len);
            for e in [c64_addr, reu_addr, reu_bank, auto_inc, fixed]
                .into_iter()
                .flatten()
            {
                v.visit_expr_mut(e);
            }
        }
        MemLen { len } => v.visit_expr_mut(len),
        MemC64Addr { addr } => v.visit_expr_mut(addr),
        MemReuPos { addr, bank } => {
            v.visit_expr_mut(addr);
            v.visit_expr_mut(bank);
        }
        MemRestore { auto_inc } => v.visit_expr_mut(auto_inc),
        MemCont { mode } => v.visit_expr_mut(mode),
        Design { addr, bytes } => {
            v.visit_expr_mut(addr);
            for e in bytes {
                v.visit_expr_mut(e);
            }
        }
        Mmob { index, x, y } => {
            v.visit_expr_mut(index);
            v.visit_expr_mut(x);
            v.visit_expr_mut(y);
        }
        MmobGlide {
            index,
            sx,
            sy,
            ex,
            ey,
            size,
            speed,
        } => {
            v.visit_expr_mut(index);
            v.visit_expr_mut(sx);
            v.visit_expr_mut(sy);
            v.visit_expr_mut(ex);
            v.visit_expr_mut(ey);
            if let Some(e) = size {
                v.visit_expr_mut(e);
            }
            if let Some(e) = speed {
                v.visit_expr_mut(e);
            }
        }
        MobSet {
            index,
            block,
            color,
            priority,
            multicolor,
            size,
            speed,
        } => {
            v.visit_expr_mut(index);
            v.visit_expr_mut(block);
            v.visit_expr_mut(color);
            v.visit_expr_mut(priority);
            v.visit_expr_mut(multicolor);
            if let Some(e) = size {
                v.visit_expr_mut(e);
            }
            if let Some(e) = speed {
                v.visit_expr_mut(e);
            }
        }
        Rlocmob {
            index,
            dx,
            dy,
            speed,
        } => {
            v.visit_expr_mut(index);
            v.visit_expr_mut(dx);
            v.visit_expr_mut(dy);
            if let Some(e) = speed {
                v.visit_expr_mut(e);
            }
        }
        Detect { mode } => v.visit_expr_mut(mode),
        Cmob { color1, color2 } => {
            v.visit_expr_mut(color1);
            v.visit_expr_mut(color2);
        }
        Bckgnds {
            color0,
            color1,
            color2,
            color3,
        } => {
            v.visit_expr_mut(color0);
            v.visit_expr_mut(color1);
            v.visit_expr_mut(color2);
            v.visit_expr_mut(color3);
        }
        Cset { mode } => v.visit_expr_mut(mode),
        Pause { message, ticks } => {
            if let Some(m) = message {
                v.visit_str_expr_mut(m);
            }
            v.visit_expr_mut(ticks);
        }
        Sys { addr, regs } => {
            v.visit_expr_mut(addr);
            for r in regs {
                v.visit_expr_mut(r);
            }
        }
        Wait { addr, mask, eor } => {
            v.visit_expr_mut(addr);
            v.visit_expr_mut(mask);
            if let Some(e) = eor {
                v.visit_expr_mut(e);
            }
        }
        Open {
            file_num,
            device,
            secondary,
            filename,
        } => {
            v.visit_expr_mut(file_num);
            if let Some(e) = device {
                v.visit_expr_mut(e);
            }
            if let Some(e) = secondary {
                v.visit_expr_mut(e);
            }
            if let Some(s) = filename {
                v.visit_str_expr_mut(s);
            }
        }
        Close { file_num } | GetFile { file_num, .. } => v.visit_expr_mut(file_num),
        Print { items, .. } | PrintFile { items, .. } | Cmd { items, .. } => {
            for it in items.iter_mut() {
                v.visit_print_piece_mut(it);
            }
        }
        Load {
            device,
            secondary,
            load_addr,
            ..
        } => {
            if let Some(e) = device {
                v.visit_expr_mut(e);
            }
            if let Some(e) = secondary {
                v.visit_expr_mut(e);
            }
            if let Some(e) = load_addr {
                v.visit_expr_mut(e);
            }
        }
        Verify {
            device, secondary, ..
        }
        | Save {
            device, secondary, ..
        } => {
            if let Some(e) = device {
                v.visit_expr_mut(e);
            }
            if let Some(e) = secondary {
                v.visit_expr_mut(e);
            }
        }
        Disk { command } => v.visit_str_expr_mut(command),
        OnBranch { value, .. } => v.visit_expr_mut(value),
        DefFn { body, .. } => v.visit_expr_mut(body),
        OnKey { keys, .. } => v.visit_str_expr_mut(keys),
        Fetch {
            control,
            max_len,
            force,
            position,
            ..
        } => {
            v.visit_str_expr_mut(control);
            v.visit_expr_mut(max_len);
            if let Some(e) = force {
                v.visit_expr_mut(e);
            }
            if let Some((r, c)) = position {
                v.visit_expr_mut(r);
                v.visit_expr_mut(c);
            }
        }
        KeySet { index, text } => {
            v.visit_expr_mut(index);
            v.visit_str_expr_mut(text);
        }
        Dim(specs) => {
            for spec in specs.iter_mut() {
                for d in spec.dims.iter_mut() {
                    v.visit_expr_mut(d);
                }
            }
        }
        Goto { .. }
        | GoSub { .. }
        | Return
        | Next { .. }
        | Do
        | DoNull
        | Done
        | Else
        | Repeat
        | Loop
        | EndLoop
        | Disable
        | Resume { .. }
        | OnError { .. }
        | Nrm
        | MemModeOn
        | Rem(_)
        | End
        | Stop
        | Run(_)
        | Clr
        | Data(_)
        | Get { .. }
        | KeyGet { .. }
        | DisplayKeys
        | SwapStr { .. }
        | Restore
        | Reset { .. } => {}
        InsertBox {
            pattern,
            row,
            col,
            width,
            height,
            color,
        } => {
            v.visit_str_expr_mut(pattern);
            for e in [row, col, width, height, color] {
                v.visit_expr_mut(e);
            }
        }
        ErrorRaise { code } => v.visit_expr_mut(code),
    }
}

pub fn walk_expr_mut<V: MutVisitor + ?Sized>(v: &mut V, e: &mut Expr) {
    use Expr::*;
    match e {
        Var(_) | Number(_) | String(_) => {}
        Neg(inner) | Not(inner) => v.visit_expr_mut(inner),
        Bin(_, l, r) => {
            v.visit_expr_mut(l);
            v.visit_expr_mut(r);
        }
        Func1(_, arg)
        | Peek(arg)
        | MemPeek(arg)
        | FnCall(_, arg)
        | Pos(arg)
        | Fre(arg)
        | Usr(arg)
        | Joy(arg)
        | Pot(arg) => v.visit_expr_mut(arg),
        ArrayRef(_, idx) => {
            for e in idx.iter_mut() {
                v.visit_expr_mut(e);
            }
        }
        Len(s) | Asc(s) | Val(s) | Nrm(s) => v.visit_str_expr_mut(s),
        StrCompare(_, l, r) => {
            v.visit_str_expr_mut(l);
            v.visit_str_expr_mut(r);
        }
        At(row, col) => {
            v.visit_expr_mut(row);
            v.visit_expr_mut(col);
        }
        Test(x, y) => {
            v.visit_expr_mut(x);
            v.visit_expr_mut(y);
        }
        Check { first, second } => {
            v.visit_expr_mut(first);
            if let Some(e) = second {
                v.visit_expr_mut(e);
            }
        }
        Inst {
            haystack,
            needle,
            start,
        } => {
            v.visit_str_expr_mut(haystack);
            v.visit_str_expr_mut(needle);
            if let Some(e) = start {
                v.visit_expr_mut(e);
            }
        }
        Inkey | Lin => {}
    }
}

pub fn walk_str_expr_mut<V: MutVisitor + ?Sized>(v: &mut V, s: &mut StrExpr) {
    use StrExpr::*;
    match s {
        Var(_) | Literal(_) | GetKey => {}
        Chr(e) | Str(e) | HexFmt(e) | BinFmt(e) => v.visit_expr_mut(e),
        Concat(a, b) => {
            v.visit_str_expr_mut(a);
            v.visit_str_expr_mut(b);
        }
        Left(s, n) | Right(s, n) => {
            v.visit_str_expr_mut(s);
            v.visit_expr_mut(n);
        }
        Mid(s, st, n) => {
            v.visit_str_expr_mut(s);
            v.visit_expr_mut(st);
            if let Some(boxed) = n {
                v.visit_expr_mut(boxed);
            }
        }
        Dup(s, n) => {
            v.visit_str_expr_mut(s);
            v.visit_expr_mut(n);
        }
        Insert(s, t, pos) => {
            v.visit_str_expr_mut(s);
            v.visit_str_expr_mut(t);
            v.visit_expr_mut(pos);
        }
        ArrayRef(_, idx) => {
            for e in idx.iter_mut() {
                v.visit_expr_mut(e);
            }
        }
    }
}

pub fn walk_print_piece_mut<V: MutVisitor + ?Sized>(v: &mut V, p: &mut PrintPiece) {
    use PrintPiece::*;
    match p {
        Expr(e) | CharOut(e) | TabTo(e) | Spc(e) => v.visit_expr_mut(e),
        StrExpr(s) => v.visit_str_expr_mut(s),
        PositionAt(r, c) => {
            v.visit_expr_mut(r);
            v.visit_expr_mut(c);
        }
        UseField { value, .. } => v.visit_expr_mut(value),
        LiteralString(_) | Tab => {}
    }
}
