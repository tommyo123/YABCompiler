//! Intermediate representation and the optimization-pass pipeline.
//!
//! The IR is currently a near-mirror of the AST, but it lives behind its
//! own type so that:
//!   1. Codegen has a stable contract (IR shape doesn't shift when we
//!      extend the source-level AST), and
//!   2. Optimization passes mutate IR, not AST — they don't need to know
//!      anything about the original BASIC syntax.
//!
//! Add a pass by implementing `Pass` and pushing it into `Pipeline`.
//! Passes run in registration order; each one sees the previous pass's
//! output. Returning `Ok` with the unchanged IR is fine (no-op pass).

use crate::ast::{
    self, BinOp, FnName, Func1, OnBranchKind, ProcName, ScreenRectOp, ScreenScrollOp, ThenBranch,
    VarName,
};

/// Mirror of `ast::ReadTarget` — separate so the array-index variant
/// holds the IR-level `Expr` rather than the AST one.
#[derive(Debug, Clone)]
pub enum ReadTarget {
    Scalar(VarName),
    Array { name: VarName, indices: Vec<Expr> },
}

/// IR-level DIM spec — mirrors AST shape but holds IR Expr nodes so
/// the constant-fold pass operates on them in place.
#[derive(Debug, Clone, PartialEq)]
pub struct DimSpec {
    pub name: VarName,
    pub dims: Vec<Expr>,
}

#[derive(Debug, Clone)]
pub struct Module {
    pub lines: Vec<Line>,
}

#[derive(Debug, Clone)]
pub struct Line {
    pub number: u16,
    pub stmts: Vec<Stmt>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemTransferOp {
    Save,
    Load,
    Read,
}

/// Array-pointer induction shape for one array in one FOR body.
/// `indices` mirrors the array reference rank and marks which axis
/// follows the FOR loop variable. Non-loop axes are literal indexes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ArrayInduction {
    pub name: VarName,
    pub indices: Vec<ArrayInductionIndex>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ArrayInductionIndex {
    LoopVar,
    Const(i16),
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Print {
        items: Vec<PrintPiece>,
        newline: bool,
    },
    Goto {
        target: u16,
    },
    GoSub {
        target: u16,
    },
    Return,
    Let {
        var: VarName,
        value: Expr,
    },
    /// `IF cond` with two branches: `then_*` runs when cond is true,
    /// otherwise control falls through past the IF.
    If {
        cond: Expr,
        then: ThenIr,
    },
    IfElse {
        cond: Expr,
        then: ThenIr,
        else_then: ThenIr,
    },
    DoIf {
        cond: Expr,
    },
    Do,
    DoNull,
    Done,
    Else,
    Repeat,
    Until {
        cond: Expr,
    },
    Loop,
    EndLoop,
    ExitLoop {
        cond: Option<Expr>,
    },
    ComputedGoto {
        target: Expr,
    },
    Rcomp {
        then: ThenIr,
        else_then: Option<ThenIr>,
    },
    /// FOR header. END and STEP are stored in per-FOR float slots so the
    /// loop body can re-read them from memory at every NEXT.
    For {
        var: VarName,
        start: Expr,
        end: Expr,
        /// STEP expression. Codegen tries to fold it to a literal float
        /// (enabling the int-FOR fast path and a compile-time exit-sign
        /// branch); when it doesn't fold, codegen falls back to a
        /// dynamic-step path that evaluates the expression at FOR
        /// header time and stamps a sign byte for NEXT to read.
        step: Expr,
        /// True iff the body is safe for the int-FOR fast path — i.e.,
        /// nothing in it can modify the loop variable or call code we
        /// can't reason about. Set to true at lowering and cleared by
        /// `IntForBodyAnalysis` when it spots an unsafe construct.
        body_int_safe: bool,
        /// True iff the body actually reads the loop variable. When
        /// false, codegen can skip the per-iteration V_var sync — only
        /// stamp V_var once at loop exit so post-loop reads still see
        /// the right value. Same pass that fills `body_int_safe` sets
        /// this; default is true (conservative).
        body_reads_loop_var: bool,
        /// When `Some(K)`, the body contains at least one
        /// occurrence of `var * K` (or `K * var`) for the literal
        /// constant `K`. Codegen materialises a per-FOR float slot
        /// that holds the running value of `var * K`: initialised
        /// at the FOR header, advanced by `step * K` inside NEXT,
        /// and read in place of the multiplication everywhere it
        /// appears in the body. Set by `LoopInductionDetect`;
        /// `None` disables induction.
        induction_const: Option<f64>,
        /// Array access shapes whose `loop_var + const` dimension can
        /// be routed through a per-loop running pointer instead of
        /// recomputing the slot address per iteration. Codegen
        /// allocates an `AP_<n>` ZP-pointer slot for each entry,
        /// initialises it at the FOR header to the base element for
        /// that shape, advances it by `step*axis_stride` at NEXT, and
        /// rewrites matching access sites to copy the pointer into
        /// ARRAY_ADDR_LO/HI. Set by `ArrayPtrInductionDetect`; empty
        /// disables.
        array_inductions: Vec<ArrayInduction>,
    },
    Next {
        vars: Vec<Option<VarName>>,
    },
    /// `REM` — comment text bytes are kept on the IR node so future
    /// passes (e.g. a `--listing` mode that echoes source) can read
    /// them without re-parsing. Codegen ignores them.
    Rem(#[allow(dead_code)] Vec<u8>),
    End,
    Stop,
    /// `RUN [line]` — restart the program. See
    /// `ast::Statement::Run`.
    Run(Option<u16>),
    /// `CLR` — clear all variables/arrays/heap/DATA pointer and
    /// continue. See `ast::Statement::Clr`.
    Clr,
    Poke {
        addr: Expr,
        value: Expr,
    },
    Dpoke {
        addr: Expr,
        value: Expr,
    },
    /// Fused `FOR I=start TO end: POKE I,value: NEXT` body — fills the
    /// inclusive byte range `[start, end]` with `value`. Synthesised by
    /// the `PokeLoopFusion` pass when it spots that exact pattern with
    /// `STEP 1` (or omitted), an unused-elsewhere loop variable, and a
    /// value expression that doesn't depend on the loop variable.
    /// Codegen lowers it to a tight memory-fill loop.
    PokeFill {
        dst_start: Expr,
        dst_end: Expr,
        value: Expr,
    },
    ScreenRect {
        op: ScreenRectOp,
        row: Expr,
        col: Expr,
        width: Expr,
        height: Expr,
        ch: Option<Expr>,
        color: Option<Expr>,
    },
    ScreenMove {
        row: Expr,
        col: Expr,
        width: Expr,
        height: Expr,
        dest_row: Expr,
        dest_col: Expr,
    },
    ScreenScroll {
        op: ScreenScrollOp,
        row: Expr,
        col: Expr,
        width: Expr,
        height: Expr,
    },
    Color {
        border: Option<Expr>,
        background: Option<Expr>,
        pen: Option<Expr>,
    },
    MobEnable {
        index: Expr,
        enabled: bool,
    },
    Multi {
        enabled: bool,
    },
    MultiColors {
        c1: Expr,
        c2: Expr,
        c3: Expr,
    },
    /// `HIRES [ink [, paper]]` — enter 320x200 hires bitmap
    /// mode and clear it. The colour pair seeds `$C000+` (high
    /// nibble = ink/pixel-on, low nibble = paper/pixel-off). None
    /// in either field falls back to the codegen default.
    Hires {
        ink: Option<Expr>,
        paper: Option<Expr>,
    },
    /// `BORDER expr` — write the border colour ($D020).
    Border {
        color: Expr,
    },
    /// `LINE x1,y1,x2,y2 [,mode]` — Bresenham line.
    Line {
        x1: Expr,
        y1: Expr,
        x2: Expr,
        y2: Expr,
        mode: Option<Expr>,
    },
    /// Single-pixel hires plot. `PLOT x,y[,mode]` and the compat
    /// `DRAW x,y[,mode]` shortcut both lower here.
    Draw {
        x: Expr,
        y: Expr,
        mode: Option<Expr>,
    },
    /// `REC x,y,width,height [,mode]` — rectangle outline.
    Rec {
        x: Expr,
        y: Expr,
        width: Expr,
        height: Expr,
        mode: Option<Expr>,
    },
    /// `BLOCK x1,y1,x2,y2 [,mode]` — filled rectangle.
    Block {
        x1: Expr,
        y1: Expr,
        x2: Expr,
        y2: Expr,
        mode: Option<Expr>,
    },
    /// `CIRCLE cx,cy,rx [,ry [,start [,end [,step [,mode]]]]]` —
    /// fast Bresenham when only `cx,cy,r` is given; parametric
    /// ellipse/arc when any optional arg is supplied.
    Circle {
        cx: Expr,
        cy: Expr,
        radius: Expr,
        ry: Option<Expr>,
        start: Option<Expr>,
        end: Option<Expr>,
        step: Option<Expr>,
        mode: Option<Expr>,
    },
    /// `CHAR x,y,code [,mode [,zoom]]` — render single
    /// char from char ROM with optional pixel op (0 = clear,
    /// 1 = set, 2 = invert) and optional 1x/2x scaling.
    Char {
        x: Expr,
        y: Expr,
        code: Expr,
        mode: Option<Expr>,
        zoom: Option<Expr>,
    },
    /// `TEXT x,y,s$ [,mode [,zoom [,kerning]]]` — render a
    /// string of chars; `mode`/`zoom` pass through to CHAR,
    /// `kerning` is the per-glyph X advance (default 8).
    Text {
        x: Expr,
        y: Expr,
        text: StrExpr,
        mode: Option<Expr>,
        zoom: Option<Expr>,
        kerning: Option<Expr>,
    },
    /// `DRAW TO x,y [,mode]` — line from cursor to (x,y),
    /// updating the cursor to the new endpoint.
    DrawTo {
        x: Expr,
        y: Expr,
        mode: Option<Expr>,
    },
    /// `ROT direction[,length]` — write the per-direction
    /// drawtabx/drawtaby tables used by `DRAW string,...` and
    /// remember the per-step pixel length.
    Rot {
        direction: Expr,
        length: Option<Expr>,
    },
    /// `DRAW code$, x, y [,mode]` — turtle-graphics
    /// interpreter driven by ROT's direction tables.
    DrawString {
        code: StrExpr,
        x: Expr,
        y: Expr,
        mode: Option<Expr>,
    },
    /// `PAINT x,y [,mode]` — flood-fill at (x,y).
    Paint {
        x: Expr,
        y: Expr,
        mode: Option<Expr>,
    },
    /// `ANGL cx,cy,angle,rx[,ry[,mode]]` — polar line with
    /// per-axis semi-radius and optional pixel mode.
    Angl {
        cx: Expr,
        cy: Expr,
        angle: Expr,
        rx: Expr,
        ry: Option<Expr>,
        mode: Option<Expr>,
    },
    Sound {
        voice: Expr,
        freq: Expr,
    },
    Envelope {
        voice: Expr,
        attack: Expr,
        decay: Expr,
        sustain: Expr,
        release: Expr,
    },
    Wave {
        voice: Expr,
        control: Expr,
        pulse: Option<Expr>,
    },
    Music {
        tempo: Expr,
        tune: StrExpr,
    },
    Play {
        mode: Expr,
    },
    Flash {
        enabled: Option<bool>,
        speed: Option<Expr>,
        color1: Option<Expr>,
        color2: Option<Expr>,
    },
    Bflash {
        enabled: Option<bool>,
        speed: Option<Expr>,
        color1: Option<Expr>,
        color2: Option<Expr>,
    },
    HiCol,
    LowCol {
        color1: Expr,
        color2: Expr,
        color3: Option<Expr>,
    },
    /// `MOD ink, paper` — fill HIRES cell-attribute RAM with the
    /// packed nibble pair.
    Mod {
        ink: Expr,
        paper: Expr,
    },
    /// `DUP src_x, src_y, width, height, dst_x, dst_y [,mode [,zoom]]`
    /// — copy a HIRES bitmap region with optional pixel op + zoom.
    Dup {
        src_x: Expr,
        src_y: Expr,
        width: Expr,
        height: Expr,
        dst_x: Expr,
        dst_y: Expr,
        mode: Option<Expr>,
        zoom: Option<Expr>,
    },
    Copy {
        src: Expr,
        dst: Expr,
        len: Expr,
    },
    ScrSave {
        addr: Option<Expr>,
        mode: Option<Expr>,
    },
    ScrLoad {
        addr: Option<Expr>,
        mode: Option<Expr>,
    },
    ScrDef {
        save: bool,
        addr: Expr,
        mode: Option<Expr>,
    },
    ScrRestore {
        save: bool,
    },
    MemClr {
        addr: Expr,
        len: Expr,
        value: Option<Expr>,
    },
    MemTransfer {
        op: MemTransferOp,
    },
    MemDef {
        len: Expr,
        c64_addr: Option<Expr>,
        reu_addr: Option<Expr>,
        reu_bank: Option<Expr>,
        auto_inc: Option<Expr>,
        fixed: Option<Expr>,
    },
    MemLen {
        len: Expr,
    },
    MemC64Addr {
        addr: Expr,
    },
    MemReuPos {
        addr: Expr,
        bank: Expr,
    },
    MemRestore {
        auto_inc: Expr,
    },
    MemCont {
        mode: Expr,
    },
    Design {
        addr: Expr,
        bytes: Vec<Expr>,
    },
    Mmob {
        index: Expr,
        x: Expr,
        y: Expr,
    },
    MmobGlide {
        index: Expr,
        sx: Expr,
        sy: Expr,
        ex: Expr,
        ey: Expr,
        size: Option<Expr>,
        speed: Option<Expr>,
    },
    MobSet {
        index: Expr,
        block: Expr,
        color: Expr,
        priority: Expr,
        multicolor: Expr,
        size: Option<Expr>,
        speed: Option<Expr>,
    },
    Rlocmob {
        index: Expr,
        dx: Expr,
        dy: Expr,
        speed: Option<Expr>,
    },
    Detect {
        mode: Expr,
    },
    Cmob {
        color1: Expr,
        color2: Expr,
    },
    Bckgnds {
        color0: Expr,
        color1: Expr,
        color2: Expr,
        color3: Expr,
    },
    Nrm,
    MemModeOn,
    Cset {
        mode: Expr,
    },
    Pause {
        message: Option<StrExpr>,
        ticks: Expr,
    },
    Sys {
        addr: Expr,
        /// Optional A / X / Y / SR pre-load values, in that order.
        /// See [`crate::ast::Statement::Sys`]; empty for plain SYS.
        regs: Vec<Expr>,
        /// Raw tokenised BASIC bytes that followed the address — see
        /// [`crate::ast::Statement::Sys::params`]. Empty for plain SYS.
        params: Vec<u8>,
    },
    Wait {
        addr: Expr,
        mask: Expr,
        eor: Option<Expr>,
    },
    Open {
        file_num: Expr,
        device: Option<Expr>,
        secondary: Option<Expr>,
        filename: Option<StrExpr>,
    },
    Close {
        file_num: Expr,
    },
    PrintFile {
        file_num: Expr,
        items: Vec<PrintPiece>,
        newline: bool,
    },
    GetFile {
        file_num: Expr,
        vars: Vec<VarName>,
    },
    InputFile {
        file_num: Expr,
        targets: Vec<ReadTarget>,
    },
    Cmd {
        file_num: Expr,
        items: Vec<PrintPiece>,
        newline: bool,
    },
    Load {
        filename: StrExpr,
        device: Option<Expr>,
        secondary: Option<Expr>,
        load_addr: Option<Expr>,
    },
    Verify {
        filename: StrExpr,
        device: Option<Expr>,
        secondary: Option<Expr>,
    },
    Save {
        filename: StrExpr,
        device: Option<Expr>,
        secondary: Option<Expr>,
    },
    Disk {
        command: StrExpr,
    },
    Data(Vec<ast::DataValue>),
    Read(Vec<ReadTarget>),
    Restore,
    Reset {
        line: u16,
    },
    Get {
        var: VarName,
    },
    KeyGet {
        var: VarName,
    },
    Fetch {
        control: StrExpr,
        max_len: Expr,
        target: VarName,
        target_indices: Vec<Expr>,
        force: Option<Expr>,
        position: Option<(Expr, Expr)>,
    },
    KeySet {
        index: Expr,
        text: StrExpr,
    },
    DisplayKeys,
    SwapStr {
        lhs: VarName,
        rhs: VarName,
    },
    InsertBox {
        pattern: StrExpr,
        row: Expr,
        col: Expr,
        width: Expr,
        height: Expr,
        color: Expr,
    },
    Dim(Vec<DimSpec>),
    ArrayLet {
        name: VarName,
        indices: Vec<Expr>,
        value: Expr,
    },
    ArrayLetStr {
        name: VarName,
        indices: Vec<Expr>,
        value: StrExpr,
    },
    LetStr {
        var: VarName,
        value: StrExpr,
    },
    OnBranch {
        value: Expr,
        kind: OnBranchKind,
        targets: Vec<u16>,
    },
    Input {
        prompt: Option<Vec<u8>>,
        targets: Vec<ReadTarget>,
    },
    /// `DEF FN F(X) = expr` — declaration, runtime no-op. Codegen
    /// collects every DefFn in a pre-pass and emits the body as a
    /// helper subroutine after the main code.
    DefFn {
        name: FnName,
        param: VarName,
        body: Expr,
    },
    OnKey {
        keys: StrExpr,
        target: Option<crate::ast::OnKeyAction>,
    },
    Disable,
    /// `RESUME` (Same), `RESUME NEXT`, or `RESUME <line>` —
    /// error-handler continuation.
    Resume {
        target: crate::ast::ResumeTarget,
    },
    /// `ON ERROR GOTO <line>` (Some) installs a handler at the
    /// given line; `ON ERROR` / `NO ERROR` (None) disable it.
    OnError {
        target: Option<u16>,
    },
    /// `ERROR <expr>` — explicitly raise a BASIC error code.
    ErrorRaise {
        code: Expr,
    },
}

#[derive(Debug, Clone)]
pub enum ThenIr {
    Goto(u16),
    Stmts(Vec<Stmt>),
}

#[derive(Debug, Clone)]
pub enum PrintPiece {
    LiteralString(Vec<u8>),
    /// Numeric expression — codegen evaluates it to FAC and calls FOUT.
    Expr(Expr),
    /// PRINT separators: codegen emits the right kerning for these once
    /// they're implemented. For now we accept Tab via comma but emit
    /// nothing (matches BASIC's behaviour of just a small column gap).
    Tab,
    /// `CHR$(expr)` byte sent straight to CHROUT.
    CharOut(Expr),
    /// `TAB(n)` — pad to absolute screen column `n`.
    TabTo(Expr),
    /// `SPC(n)` — emit `n` spaces.
    Spc(Expr),
    StrExpr(StrExpr),
    /// `AT(row, col)` cursor positioning prefix in PRINT /
    /// CENTER. Lowers to a KERNAL PLOT call (`CLC; LDX row; LDY col;
    /// JSR $FFF0`).
    PositionAt(Expr, Expr),
    /// `USE`-formatted-PRINT field — width-char `#`-run from
    /// the control string, consumes one numeric value. Codegen runs
    /// `JSR __USE_FIELD` after stashing the width.
    UseField {
        width: u8,
        value: Expr,
    },
}

/// IR mirror of `ast::StrExpr` — separate type so `Chr(Expr)` refers to
/// the IR's numeric expression rather than the AST's.
#[derive(Debug, Clone, PartialEq)]
pub enum StrExpr {
    Literal(Vec<u8>),
    Var(VarName),
    Chr(Box<Expr>),
    HexFmt(Box<Expr>),
    BinFmt(Box<Expr>),
    GetKey,
    Concat(Box<StrExpr>, Box<StrExpr>),
    Str(Box<Expr>),
    Left(Box<StrExpr>, Box<Expr>),
    Right(Box<StrExpr>, Box<Expr>),
    Mid(Box<StrExpr>, Box<Expr>, Option<Box<Expr>>),
    Dup(Box<StrExpr>, Box<Expr>),
    Insert(Box<StrExpr>, Box<StrExpr>, Box<Expr>),
    ArrayRef(VarName, Vec<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Number(f64),
    String(Vec<u8>),
    Var(VarName),
    Neg(Box<Expr>),
    Not(Box<Expr>),
    Bin(BinOp, Box<Expr>, Box<Expr>),
    Func1(Func1, Box<Expr>),
    Peek(Box<Expr>),
    MemPeek(Box<Expr>),
    Nrm(Box<StrExpr>),
    ArrayRef(VarName, Vec<Expr>),
    Len(Box<StrExpr>),
    Asc(Box<StrExpr>),
    StrCompare(BinOp, StrExpr, StrExpr),
    Val(Box<StrExpr>),
    FnCall(FnName, Box<Expr>),
    Pos(Box<Expr>),
    Fre(Box<Expr>),
    Usr(Box<Expr>),
    Joy(Box<Expr>),
    Pot(Box<Expr>),
    Inkey,
    Lin,
    At(Box<Expr>, Box<Expr>),
    Test(Box<Expr>, Box<Expr>),
    Check {
        first: Box<Expr>,
        second: Option<Box<Expr>>,
    },
    Inst {
        haystack: Box<StrExpr>,
        needle: Box<StrExpr>,
        start: Option<Box<Expr>>,
    },
}

pub fn lower(prog: &ast::Program) -> Result<Module, LowerError> {
    let proc_targets = collect_proc_targets(prog);
    let lines = prog
        .lines
        .iter()
        .map(|l| {
            Ok(Line {
                number: l.number,
                stmts: l
                    .statements
                    .iter()
                    .map(|s| lower_stmt(l.number, s, &proc_targets))
                    .collect::<Result<_, _>>()?,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Module { lines })
}

#[derive(Debug)]
pub enum LowerError {
    UndefinedProc(String),
}

impl std::fmt::Display for LowerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LowerError::UndefinedProc(name) => write!(f, "PROC {name} is not defined"),
        }
    }
}

impl std::error::Error for LowerError {}

fn collect_proc_targets(prog: &ast::Program) -> std::collections::HashMap<ProcName, u16> {
    let mut out = std::collections::HashMap::new();
    for line in &prog.lines {
        for stmt in &line.statements {
            if let ast::Statement::ProcDef(name) = stmt {
                out.entry(name.clone()).or_insert(line.number);
            }
        }
    }
    out
}

fn lower_then_branch(
    line_no: u16,
    then_branch: &ThenBranch,
    proc_targets: &std::collections::HashMap<ProcName, u16>,
) -> Result<ThenIr, LowerError> {
    Ok(match then_branch {
        ThenBranch::Goto(n) => ThenIr::Goto(*n),
        ThenBranch::Stmts(stmts) => ThenIr::Stmts(
            stmts
                .iter()
                .map(|s| lower_stmt(line_no, s, proc_targets))
                .collect::<Result<_, _>>()?,
        ),
    })
}

fn lower_stmt(
    line_no: u16,
    s: &ast::Statement,
    proc_targets: &std::collections::HashMap<ProcName, u16>,
) -> Result<Stmt, LowerError> {
    Ok(match s {
        ast::Statement::Print(p) => {
            let mut items = Vec::new();
            for it in &p.items {
                match it {
                    ast::PrintItem::String(b) => items.push(PrintPiece::LiteralString(b.clone())),
                    ast::PrintItem::Comma => items.push(PrintPiece::Tab),
                    ast::PrintItem::Semi => {} // pure separator, no effect on output
                    ast::PrintItem::Expr(e) => items.push(PrintPiece::Expr(lower_expr(e))),
                    ast::PrintItem::CharOut(e) => items.push(PrintPiece::CharOut(lower_expr(e))),
                    ast::PrintItem::Tab(e) => items.push(PrintPiece::TabTo(lower_expr(e))),
                    ast::PrintItem::Spc(e) => items.push(PrintPiece::Spc(lower_expr(e))),
                    ast::PrintItem::StrExpr(s) => items.push(PrintPiece::StrExpr(lower_str(s))),
                    ast::PrintItem::PositionAt(r, c) => {
                        items.push(PrintPiece::PositionAt(lower_expr(r), lower_expr(c)))
                    }
                    ast::PrintItem::UseField { width, value } => items.push(PrintPiece::UseField {
                        width: *width,
                        value: lower_expr(value),
                    }),
                }
            }
            Stmt::Print {
                items,
                newline: p.trailing_newline,
            }
        }
        ast::Statement::Goto(n) => Stmt::Goto { target: *n },
        ast::Statement::GoSub(n) => Stmt::GoSub { target: *n },
        ast::Statement::Return => Stmt::Return,
        ast::Statement::Let { name, value } => Stmt::Let {
            var: name.clone(),
            value: lower_expr(value),
        },
        ast::Statement::If { cond, then_branch } => {
            let then = lower_then_branch(line_no, then_branch, proc_targets)?;
            Stmt::If {
                cond: lower_expr(cond),
                then,
            }
        }
        ast::Statement::IfElse {
            cond,
            then_branch,
            else_branch,
        } => {
            let then = lower_then_branch(line_no, then_branch, proc_targets)?;
            let else_then = lower_then_branch(line_no, else_branch, proc_targets)?;
            Stmt::IfElse {
                cond: lower_expr(cond),
                then,
                else_then,
            }
        }
        ast::Statement::DoIf { cond } => Stmt::DoIf {
            cond: lower_expr(cond),
        },
        ast::Statement::Do => Stmt::Do,
        ast::Statement::DoNull => Stmt::DoNull,
        ast::Statement::Done => Stmt::Done,
        ast::Statement::Else => Stmt::Else,
        ast::Statement::Repeat => Stmt::Repeat,
        ast::Statement::Until { cond } => Stmt::Until {
            cond: lower_expr(cond),
        },
        ast::Statement::Loop => Stmt::Loop,
        ast::Statement::EndLoop => Stmt::EndLoop,
        ast::Statement::ExitLoop { cond } => Stmt::ExitLoop {
            cond: cond.as_ref().map(lower_expr),
        },
        ast::Statement::ComputedGoto { target } => Stmt::ComputedGoto {
            target: lower_expr(target),
        },
        ast::Statement::Rcomp {
            then_branch,
            else_branch,
        } => Stmt::Rcomp {
            then: lower_then_branch(line_no, then_branch, proc_targets)?,
            else_then: else_branch
                .as_ref()
                .map(|b| lower_then_branch(line_no, b, proc_targets))
                .transpose()?,
        },
        ast::Statement::ProcDef(_) => Stmt::Rem(Vec::new()),
        ast::Statement::ProcCall(name) => {
            // Real BASIC v2 listings (KingTut'sTomb, RenegadeF-16,
            // Snake.Wolf, …) often have a stray bare identifier
            // tail or a corrupt PROC-name fragment that's never
            // executed at runtime. Rejecting at compile time blocks
            // the whole program over dead text. Drop unresolved
            // calls to a Rem so the surrounding code still compiles
            // — anything that actually flows here would have hit
            // `?UNDEF'D STATEMENT` in the interpreter too.
            match proc_targets.get(name).copied() {
                Some(target) => Stmt::GoSub { target },
                None => Stmt::Rem(Vec::new()),
            }
        }
        ast::Statement::ProcTailCall(name) => {
            // `CALL name`: tail jump to the proc body. The
            // proc's `END PROC` returns to whatever called the
            // CURRENT proc, not to the statement after the CALL.
            // Lower to Goto so codegen emits JMP (not JSR) — the
            // current frame stays in place and the proc's RTS pops
            // the original return address. Same fallback as the
            // EXEC path for unresolved names.
            match proc_targets.get(name).copied() {
                Some(target) => Stmt::Goto { target },
                None => Stmt::Rem(Vec::new()),
            }
        }
        ast::Statement::EndProc => Stmt::Return,
        // LOCAL / GLOBAL are pure declarations consumed by the
        // `localize_proc_vars` AST pass; by the time we lower they
        // carry no executable semantics.
        ast::Statement::Local { .. } | ast::Statement::Global { .. } => Stmt::Rem(Vec::new()),
        ast::Statement::For {
            var,
            start,
            end,
            step,
        } => Stmt::For {
            var: var.clone(),
            start: lower_expr(start),
            end: lower_expr(end),
            step: lower_expr(step),
            // Optimistic default — the int-for-body-analysis pass
            // clears this if it spots anything unsafe.
            body_int_safe: true,
            // Conservative default — pass clears if body never
            // touches the loop variable.
            body_reads_loop_var: true,
            // Off by default; `LoopInductionDetect` fills this in.
            induction_const: None,
            // Off by default; `ArrayPtrInductionDetect` fills this in.
            array_inductions: Vec::new(),
        },
        ast::Statement::Next { vars } => Stmt::Next { vars: vars.clone() },
        ast::Statement::Rem(b) => Stmt::Rem(b.clone()),
        ast::Statement::End => Stmt::End,
        ast::Statement::Stop => Stmt::Stop,
        ast::Statement::Run(line) => Stmt::Run(*line),
        ast::Statement::Clr => Stmt::Clr,
        ast::Statement::Poke { addr, value } => Stmt::Poke {
            addr: lower_expr(addr),
            value: lower_expr(value),
        },
        ast::Statement::Dpoke { addr, value } => Stmt::Dpoke {
            addr: lower_expr(addr),
            value: lower_expr(value),
        },
        ast::Statement::ScreenRect {
            op,
            row,
            col,
            width,
            height,
            ch,
            color,
        } => Stmt::ScreenRect {
            op: *op,
            row: lower_expr(row),
            col: lower_expr(col),
            width: lower_expr(width),
            height: lower_expr(height),
            ch: ch.as_ref().map(lower_expr),
            color: color.as_ref().map(lower_expr),
        },
        ast::Statement::ScreenMove {
            row,
            col,
            width,
            height,
            dest_row,
            dest_col,
        } => Stmt::ScreenMove {
            row: lower_expr(row),
            col: lower_expr(col),
            width: lower_expr(width),
            height: lower_expr(height),
            dest_row: lower_expr(dest_row),
            dest_col: lower_expr(dest_col),
        },
        ast::Statement::ScreenScroll {
            op,
            row,
            col,
            width,
            height,
        } => Stmt::ScreenScroll {
            op: *op,
            row: lower_expr(row),
            col: lower_expr(col),
            width: lower_expr(width),
            height: lower_expr(height),
        },
        ast::Statement::Color {
            border,
            background,
            pen,
        } => Stmt::Color {
            border: border.as_ref().map(lower_expr),
            background: background.as_ref().map(lower_expr),
            pen: pen.as_ref().map(lower_expr),
        },
        ast::Statement::MobEnable { index, enabled } => Stmt::MobEnable {
            index: lower_expr(index),
            enabled: *enabled,
        },
        ast::Statement::Multi { enabled } => Stmt::Multi { enabled: *enabled },
        ast::Statement::MultiColors { c1, c2, c3 } => Stmt::MultiColors {
            c1: lower_expr(c1),
            c2: lower_expr(c2),
            c3: lower_expr(c3),
        },
        ast::Statement::Hires { ink, paper } => Stmt::Hires {
            ink: ink.as_ref().map(lower_expr),
            paper: paper.as_ref().map(lower_expr),
        },
        ast::Statement::Border { color } => Stmt::Border {
            color: lower_expr(color),
        },
        ast::Statement::Line {
            x1,
            y1,
            x2,
            y2,
            mode,
        } => Stmt::Line {
            x1: lower_expr(x1),
            y1: lower_expr(y1),
            x2: lower_expr(x2),
            y2: lower_expr(y2),
            mode: mode.as_ref().map(lower_expr),
        },
        ast::Statement::Draw { x, y, mode } => Stmt::Draw {
            x: lower_expr(x),
            y: lower_expr(y),
            mode: mode.as_ref().map(lower_expr),
        },
        ast::Statement::Rec {
            x,
            y,
            width,
            height,
            mode,
        } => Stmt::Rec {
            x: lower_expr(x),
            y: lower_expr(y),
            width: lower_expr(width),
            height: lower_expr(height),
            mode: mode.as_ref().map(lower_expr),
        },
        ast::Statement::Block {
            x1,
            y1,
            x2,
            y2,
            mode,
        } => Stmt::Block {
            x1: lower_expr(x1),
            y1: lower_expr(y1),
            x2: lower_expr(x2),
            y2: lower_expr(y2),
            mode: mode.as_ref().map(lower_expr),
        },
        ast::Statement::Circle {
            cx,
            cy,
            radius,
            ry,
            start,
            end,
            step,
            mode,
        } => Stmt::Circle {
            cx: lower_expr(cx),
            cy: lower_expr(cy),
            radius: lower_expr(radius),
            ry: ry.as_ref().map(lower_expr),
            start: start.as_ref().map(lower_expr),
            end: end.as_ref().map(lower_expr),
            step: step.as_ref().map(lower_expr),
            mode: mode.as_ref().map(lower_expr),
        },
        ast::Statement::Char {
            x,
            y,
            code,
            mode,
            zoom,
        } => Stmt::Char {
            x: lower_expr(x),
            y: lower_expr(y),
            code: lower_expr(code),
            mode: mode.as_ref().map(lower_expr),
            zoom: zoom.as_ref().map(lower_expr),
        },
        ast::Statement::Text {
            x,
            y,
            text,
            mode,
            zoom,
            kerning,
        } => Stmt::Text {
            x: lower_expr(x),
            y: lower_expr(y),
            text: lower_str(text),
            mode: mode.as_ref().map(lower_expr),
            zoom: zoom.as_ref().map(lower_expr),
            kerning: kerning.as_ref().map(lower_expr),
        },
        ast::Statement::DrawTo { x, y, mode } => Stmt::DrawTo {
            x: lower_expr(x),
            y: lower_expr(y),
            mode: mode.as_ref().map(lower_expr),
        },
        ast::Statement::Rot { direction, length } => Stmt::Rot {
            direction: lower_expr(direction),
            length: length.as_ref().map(lower_expr),
        },
        ast::Statement::DrawString { code, x, y, mode } => Stmt::DrawString {
            code: lower_str(code),
            x: lower_expr(x),
            y: lower_expr(y),
            mode: mode.as_ref().map(lower_expr),
        },
        ast::Statement::Paint { x, y, mode } => Stmt::Paint {
            x: lower_expr(x),
            y: lower_expr(y),
            mode: mode.as_ref().map(lower_expr),
        },
        ast::Statement::Angl {
            cx,
            cy,
            angle,
            rx,
            ry,
            mode,
        } => Stmt::Angl {
            cx: lower_expr(cx),
            cy: lower_expr(cy),
            angle: lower_expr(angle),
            rx: lower_expr(rx),
            ry: ry.as_ref().map(lower_expr),
            mode: mode.as_ref().map(lower_expr),
        },
        ast::Statement::Sound { voice, freq } => Stmt::Sound {
            voice: lower_expr(voice),
            freq: lower_expr(freq),
        },
        ast::Statement::Envelope {
            voice,
            attack,
            decay,
            sustain,
            release,
        } => Stmt::Envelope {
            voice: lower_expr(voice),
            attack: lower_expr(attack),
            decay: lower_expr(decay),
            sustain: lower_expr(sustain),
            release: lower_expr(release),
        },
        ast::Statement::Wave {
            voice,
            control,
            pulse,
        } => Stmt::Wave {
            voice: lower_expr(voice),
            control: lower_expr(control),
            pulse: pulse.as_ref().map(lower_expr),
        },
        ast::Statement::Music { tempo, tune } => Stmt::Music {
            tempo: lower_expr(tempo),
            tune: lower_str(tune),
        },
        ast::Statement::Play { mode } => Stmt::Play {
            mode: lower_expr(mode),
        },
        ast::Statement::Flash {
            enabled,
            speed,
            color1,
            color2,
        } => Stmt::Flash {
            enabled: *enabled,
            speed: speed.as_ref().map(lower_expr),
            color1: color1.as_ref().map(lower_expr),
            color2: color2.as_ref().map(lower_expr),
        },
        ast::Statement::Bflash {
            enabled,
            speed,
            color1,
            color2,
        } => Stmt::Bflash {
            enabled: *enabled,
            speed: speed.as_ref().map(lower_expr),
            color1: color1.as_ref().map(lower_expr),
            color2: color2.as_ref().map(lower_expr),
        },
        ast::Statement::HiCol => Stmt::HiCol,
        ast::Statement::LowCol {
            color1,
            color2,
            color3,
        } => Stmt::LowCol {
            color1: lower_expr(color1),
            color2: lower_expr(color2),
            color3: color3.as_ref().map(lower_expr),
        },
        ast::Statement::Mod { ink, paper } => Stmt::Mod {
            ink: lower_expr(ink),
            paper: lower_expr(paper),
        },
        ast::Statement::Dup {
            src_x,
            src_y,
            width,
            height,
            dst_x,
            dst_y,
            mode,
            zoom,
        } => Stmt::Dup {
            src_x: lower_expr(src_x),
            src_y: lower_expr(src_y),
            width: lower_expr(width),
            height: lower_expr(height),
            dst_x: lower_expr(dst_x),
            dst_y: lower_expr(dst_y),
            mode: mode.as_ref().map(lower_expr),
            zoom: zoom.as_ref().map(lower_expr),
        },
        ast::Statement::Copy { src, dst, len } => Stmt::Copy {
            src: lower_expr(src),
            dst: lower_expr(dst),
            len: lower_expr(len),
        },
        ast::Statement::ScrSave { addr, mode } => Stmt::ScrSave {
            addr: addr.as_ref().map(lower_expr),
            mode: mode.as_ref().map(lower_expr),
        },
        ast::Statement::ScrLoad { addr, mode } => Stmt::ScrLoad {
            addr: addr.as_ref().map(lower_expr),
            mode: mode.as_ref().map(lower_expr),
        },
        ast::Statement::ScrDef { save, addr, mode } => Stmt::ScrDef {
            save: *save,
            addr: lower_expr(addr),
            mode: mode.as_ref().map(lower_expr),
        },
        ast::Statement::ScrRestore { save } => Stmt::ScrRestore { save: *save },
        ast::Statement::MemClr { addr, len, value } => Stmt::MemClr {
            addr: lower_expr(addr),
            len: lower_expr(len),
            value: value.as_ref().map(lower_expr),
        },
        ast::Statement::MemTransfer { op } => Stmt::MemTransfer {
            op: match op {
                ast::MemTransferOp::Save => MemTransferOp::Save,
                ast::MemTransferOp::Load => MemTransferOp::Load,
                ast::MemTransferOp::Read => MemTransferOp::Read,
            },
        },
        ast::Statement::MemDef {
            len,
            c64_addr,
            reu_addr,
            reu_bank,
            auto_inc,
            fixed,
        } => Stmt::MemDef {
            len: lower_expr(len),
            c64_addr: c64_addr.as_ref().map(lower_expr),
            reu_addr: reu_addr.as_ref().map(lower_expr),
            reu_bank: reu_bank.as_ref().map(lower_expr),
            auto_inc: auto_inc.as_ref().map(lower_expr),
            fixed: fixed.as_ref().map(lower_expr),
        },
        ast::Statement::MemLen { len } => Stmt::MemLen {
            len: lower_expr(len),
        },
        ast::Statement::MemC64Addr { addr } => Stmt::MemC64Addr {
            addr: lower_expr(addr),
        },
        ast::Statement::MemReuPos { addr, bank } => Stmt::MemReuPos {
            addr: lower_expr(addr),
            bank: lower_expr(bank),
        },
        ast::Statement::MemRestore { auto_inc } => Stmt::MemRestore {
            auto_inc: lower_expr(auto_inc),
        },
        ast::Statement::MemCont { mode } => Stmt::MemCont {
            mode: lower_expr(mode),
        },
        ast::Statement::Design { addr, bytes } => Stmt::Design {
            addr: lower_expr(addr),
            bytes: bytes.iter().map(lower_expr).collect(),
        },
        // The design-group pass folds DesignRow into the preceding
        // Design's byte list, so any DesignRow that survives to
        // lowering is a row outside of a DESIGN block — treat as
        // a no-op (matches BASIC v2 behaviour for an unknown line
        // prefix in REM-style data).
        ast::Statement::DesignRow(_) => Stmt::Rem(Vec::new()),
        ast::Statement::Mmob { index, x, y } => Stmt::Mmob {
            index: lower_expr(index),
            x: lower_expr(x),
            y: lower_expr(y),
        },
        ast::Statement::MmobGlide {
            index,
            sx,
            sy,
            ex,
            ey,
            size,
            speed,
        } => Stmt::MmobGlide {
            index: lower_expr(index),
            sx: lower_expr(sx),
            sy: lower_expr(sy),
            ex: lower_expr(ex),
            ey: lower_expr(ey),
            size: size.as_ref().map(lower_expr),
            speed: speed.as_ref().map(lower_expr),
        },
        ast::Statement::MobSet {
            index,
            block,
            color,
            priority,
            multicolor,
            size,
            speed,
        } => Stmt::MobSet {
            index: lower_expr(index),
            block: lower_expr(block),
            color: lower_expr(color),
            priority: lower_expr(priority),
            multicolor: lower_expr(multicolor),
            size: size.as_ref().map(lower_expr),
            speed: speed.as_ref().map(lower_expr),
        },
        ast::Statement::Rlocmob {
            index,
            dx,
            dy,
            speed,
        } => Stmt::Rlocmob {
            index: lower_expr(index),
            dx: lower_expr(dx),
            dy: lower_expr(dy),
            speed: speed.as_ref().map(lower_expr),
        },
        ast::Statement::Detect { mode } => Stmt::Detect {
            mode: lower_expr(mode),
        },
        ast::Statement::Cmob { color1, color2 } => Stmt::Cmob {
            color1: lower_expr(color1),
            color2: lower_expr(color2),
        },
        ast::Statement::Bckgnds {
            color0,
            color1,
            color2,
            color3,
        } => Stmt::Bckgnds {
            color0: lower_expr(color0),
            color1: lower_expr(color1),
            color2: lower_expr(color2),
            color3: lower_expr(color3),
        },
        ast::Statement::Nrm => Stmt::Nrm,
        ast::Statement::MemModeOn => Stmt::MemModeOn,
        ast::Statement::Cset { mode } => Stmt::Cset {
            mode: lower_expr(mode),
        },
        ast::Statement::Pause { message, ticks } => Stmt::Pause {
            message: message.as_ref().map(lower_str),
            ticks: lower_expr(ticks),
        },
        ast::Statement::Sys {
            addr,
            regs,
            params,
        } => Stmt::Sys {
            addr: lower_expr(addr),
            regs: regs.iter().map(lower_expr).collect(),
            params: params.clone(),
        },
        ast::Statement::Wait { addr, mask, eor } => Stmt::Wait {
            addr: lower_expr(addr),
            mask: lower_expr(mask),
            eor: eor.as_ref().map(lower_expr),
        },
        ast::Statement::Open {
            file_num,
            device,
            secondary,
            filename,
        } => Stmt::Open {
            file_num: lower_expr(file_num),
            device: device.as_ref().map(lower_expr),
            secondary: secondary.as_ref().map(lower_expr),
            filename: filename.as_ref().map(lower_str),
        },
        ast::Statement::Close { file_num } => Stmt::Close {
            file_num: lower_expr(file_num),
        },
        ast::Statement::PrintFile { file_num, body } => {
            let mut items = Vec::new();
            for it in &body.items {
                match it {
                    ast::PrintItem::String(b) => items.push(PrintPiece::LiteralString(b.clone())),
                    ast::PrintItem::Comma => items.push(PrintPiece::Tab),
                    ast::PrintItem::Semi => {}
                    ast::PrintItem::Expr(e) => items.push(PrintPiece::Expr(lower_expr(e))),
                    ast::PrintItem::CharOut(e) => items.push(PrintPiece::CharOut(lower_expr(e))),
                    ast::PrintItem::Tab(e) => items.push(PrintPiece::TabTo(lower_expr(e))),
                    ast::PrintItem::Spc(e) => items.push(PrintPiece::Spc(lower_expr(e))),
                    ast::PrintItem::StrExpr(s) => items.push(PrintPiece::StrExpr(lower_str(s))),
                    ast::PrintItem::PositionAt(r, c) => {
                        items.push(PrintPiece::PositionAt(lower_expr(r), lower_expr(c)))
                    }
                    ast::PrintItem::UseField { width, value } => items.push(PrintPiece::UseField {
                        width: *width,
                        value: lower_expr(value),
                    }),
                }
            }
            Stmt::PrintFile {
                file_num: lower_expr(file_num),
                items,
                newline: body.trailing_newline,
            }
        }
        ast::Statement::GetFile { file_num, vars } => Stmt::GetFile {
            file_num: lower_expr(file_num),
            vars: vars.clone(),
        },
        ast::Statement::InputFile { file_num, targets } => Stmt::InputFile {
            file_num: lower_expr(file_num),
            targets: targets
                .iter()
                .map(|t| match t {
                    ast::ReadTarget::Scalar(v) => ReadTarget::Scalar(v.clone()),
                    ast::ReadTarget::Array { name, indices } => ReadTarget::Array {
                        name: name.clone(),
                        indices: indices.iter().map(lower_expr).collect(),
                    },
                })
                .collect(),
        },
        ast::Statement::Load {
            filename,
            device,
            secondary,
            load_addr,
        } => Stmt::Load {
            filename: lower_str(filename),
            device: device.as_ref().map(lower_expr),
            secondary: secondary.as_ref().map(lower_expr),
            load_addr: load_addr.as_ref().map(lower_expr),
        },
        ast::Statement::Verify {
            filename,
            device,
            secondary,
        } => Stmt::Verify {
            filename: lower_str(filename),
            device: device.as_ref().map(lower_expr),
            secondary: secondary.as_ref().map(lower_expr),
        },
        ast::Statement::Save {
            filename,
            device,
            secondary,
        } => Stmt::Save {
            filename: lower_str(filename),
            device: device.as_ref().map(lower_expr),
            secondary: secondary.as_ref().map(lower_expr),
        },
        ast::Statement::Disk { command } => Stmt::Disk {
            command: lower_str(command),
        },
        ast::Statement::Cmd { file_num, body } => {
            let mut items = Vec::new();
            for it in &body.items {
                match it {
                    ast::PrintItem::String(b) => items.push(PrintPiece::LiteralString(b.clone())),
                    ast::PrintItem::Comma => items.push(PrintPiece::Tab),
                    ast::PrintItem::Semi => {}
                    ast::PrintItem::Expr(e) => items.push(PrintPiece::Expr(lower_expr(e))),
                    ast::PrintItem::CharOut(e) => items.push(PrintPiece::CharOut(lower_expr(e))),
                    ast::PrintItem::Tab(e) => items.push(PrintPiece::TabTo(lower_expr(e))),
                    ast::PrintItem::Spc(e) => items.push(PrintPiece::Spc(lower_expr(e))),
                    ast::PrintItem::StrExpr(s) => items.push(PrintPiece::StrExpr(lower_str(s))),
                    ast::PrintItem::PositionAt(r, c) => {
                        items.push(PrintPiece::PositionAt(lower_expr(r), lower_expr(c)))
                    }
                    ast::PrintItem::UseField { width, value } => items.push(PrintPiece::UseField {
                        width: *width,
                        value: lower_expr(value),
                    }),
                }
            }
            Stmt::Cmd {
                file_num: lower_expr(file_num),
                items,
                newline: body.trailing_newline,
            }
        }
        ast::Statement::Data(values) => Stmt::Data(values.clone()),
        ast::Statement::Read(targets) => Stmt::Read(
            targets
                .iter()
                .map(|t| match t {
                    ast::ReadTarget::Scalar(v) => ReadTarget::Scalar(v.clone()),
                    ast::ReadTarget::Array { name, indices } => ReadTarget::Array {
                        name: name.clone(),
                        indices: indices.iter().map(lower_expr).collect(),
                    },
                })
                .collect(),
        ),
        ast::Statement::Restore => Stmt::Restore,
        ast::Statement::Reset { line } => Stmt::Reset { line: *line },
        ast::Statement::Get { var } => Stmt::Get { var: var.clone() },
        ast::Statement::KeyGet { var } => Stmt::KeyGet { var: var.clone() },
        ast::Statement::Fetch {
            control,
            max_len,
            target,
            target_indices,
            force,
            position,
        } => Stmt::Fetch {
            control: lower_str(control),
            max_len: lower_expr(max_len),
            target: target.clone(),
            target_indices: target_indices.iter().map(lower_expr).collect(),
            force: force.as_ref().map(lower_expr),
            position: position
                .as_ref()
                .map(|(r, c)| (lower_expr(r), lower_expr(c))),
        },
        ast::Statement::KeySet { index, text } => Stmt::KeySet {
            index: lower_expr(index),
            text: lower_str(text),
        },
        ast::Statement::DisplayKeys => Stmt::DisplayKeys,
        ast::Statement::SwapStr { lhs, rhs } => Stmt::SwapStr {
            lhs: lhs.clone(),
            rhs: rhs.clone(),
        },
        ast::Statement::InsertBox {
            pattern,
            row,
            col,
            width,
            height,
            color,
        } => Stmt::InsertBox {
            pattern: lower_str(pattern),
            row: lower_expr(row),
            col: lower_expr(col),
            width: lower_expr(width),
            height: lower_expr(height),
            color: lower_expr(color),
        },
        ast::Statement::Dim(specs) => Stmt::Dim(
            specs
                .iter()
                .map(|s| DimSpec {
                    name: s.name.clone(),
                    dims: s.dims.iter().map(lower_expr).collect(),
                })
                .collect(),
        ),
        ast::Statement::ArrayLet {
            name,
            indices,
            value,
        } => Stmt::ArrayLet {
            name: name.clone(),
            indices: indices.iter().map(lower_expr).collect(),
            value: lower_expr(value),
        },
        ast::Statement::ArrayLetStr {
            name,
            indices,
            value,
        } => Stmt::ArrayLetStr {
            name: name.clone(),
            indices: indices.iter().map(lower_expr).collect(),
            value: lower_str(value),
        },
        ast::Statement::LetStr { var, value } => Stmt::LetStr {
            var: var.clone(),
            value: lower_str(value),
        },
        ast::Statement::OnBranch {
            value,
            kind,
            targets,
        } => Stmt::OnBranch {
            value: lower_expr(value),
            kind: *kind,
            targets: targets.clone(),
        },
        ast::Statement::Input { prompt, targets } => Stmt::Input {
            prompt: prompt.clone(),
            targets: targets
                .iter()
                .map(|t| match t {
                    ast::ReadTarget::Scalar(v) => ReadTarget::Scalar(v.clone()),
                    ast::ReadTarget::Array { name, indices } => ReadTarget::Array {
                        name: name.clone(),
                        indices: indices.iter().map(lower_expr).collect(),
                    },
                })
                .collect(),
        },
        ast::Statement::DefFn { name, param, body } => Stmt::DefFn {
            name: name.clone(),
            param: param.clone(),
            body: lower_expr(body),
        },
        ast::Statement::OnKey { keys, target } => Stmt::OnKey {
            keys: lower_str(keys),
            target: *target,
        },
        ast::Statement::Disable => Stmt::Disable,
        ast::Statement::Resume { target } => Stmt::Resume { target: *target },
        ast::Statement::OnError { target } => Stmt::OnError { target: *target },
        ast::Statement::ErrorRaise { code } => Stmt::ErrorRaise {
            code: lower_expr(code),
        },
    })
}

fn lower_str(s: &ast::StrExpr) -> StrExpr {
    match s {
        ast::StrExpr::Literal(b) => StrExpr::Literal(b.clone()),
        ast::StrExpr::Var(v) => StrExpr::Var(v.clone()),
        ast::StrExpr::Chr(e) => StrExpr::Chr(Box::new(lower_expr(e))),
        ast::StrExpr::HexFmt(e) => StrExpr::HexFmt(Box::new(lower_expr(e))),
        ast::StrExpr::BinFmt(e) => StrExpr::BinFmt(Box::new(lower_expr(e))),
        ast::StrExpr::GetKey => StrExpr::GetKey,
        ast::StrExpr::Concat(a, b) => {
            StrExpr::Concat(Box::new(lower_str(a)), Box::new(lower_str(b)))
        }
        ast::StrExpr::Str(e) => StrExpr::Str(Box::new(lower_expr(e))),
        ast::StrExpr::Left(s, n) => StrExpr::Left(Box::new(lower_str(s)), Box::new(lower_expr(n))),
        ast::StrExpr::Right(s, n) => {
            StrExpr::Right(Box::new(lower_str(s)), Box::new(lower_expr(n)))
        }
        ast::StrExpr::Mid(s, start, n) => StrExpr::Mid(
            Box::new(lower_str(s)),
            Box::new(lower_expr(start)),
            n.as_ref().map(|e| Box::new(lower_expr(e))),
        ),
        ast::StrExpr::Dup(s, n) => StrExpr::Dup(Box::new(lower_str(s)), Box::new(lower_expr(n))),
        ast::StrExpr::Insert(s, t, pos) => StrExpr::Insert(
            Box::new(lower_str(s)),
            Box::new(lower_str(t)),
            Box::new(lower_expr(pos)),
        ),
        ast::StrExpr::ArrayRef(v, idx) => {
            StrExpr::ArrayRef(v.clone(), idx.iter().map(lower_expr).collect())
        }
    }
}

fn lower_expr(e: &ast::Expr) -> Expr {
    match e {
        ast::Expr::Number(n) => Expr::Number(*n),
        ast::Expr::String(s) => Expr::String(s.clone()),
        ast::Expr::Var(v) => Expr::Var(v.clone()),
        ast::Expr::Neg(inner) => Expr::Neg(Box::new(lower_expr(inner))),
        ast::Expr::Not(inner) => Expr::Not(Box::new(lower_expr(inner))),
        ast::Expr::Bin(op, l, r) => {
            Expr::Bin(*op, Box::new(lower_expr(l)), Box::new(lower_expr(r)))
        }
        ast::Expr::Func1(f, arg) => Expr::Func1(*f, Box::new(lower_expr(arg))),
        ast::Expr::Peek(addr) => Expr::Peek(Box::new(lower_expr(addr))),
        ast::Expr::MemPeek(addr) => Expr::MemPeek(Box::new(lower_expr(addr))),
        ast::Expr::Nrm(s) => Expr::Nrm(Box::new(lower_str(s))),
        ast::Expr::ArrayRef(v, idx) => {
            Expr::ArrayRef(v.clone(), idx.iter().map(lower_expr).collect())
        }
        ast::Expr::Len(s) => Expr::Len(Box::new(lower_str(s))),
        ast::Expr::Asc(s) => Expr::Asc(Box::new(lower_str(s))),
        ast::Expr::StrCompare(op, l, r) => Expr::StrCompare(*op, lower_str(l), lower_str(r)),
        ast::Expr::Val(s) => Expr::Val(Box::new(lower_str(s))),
        ast::Expr::FnCall(name, arg) => Expr::FnCall(name.clone(), Box::new(lower_expr(arg))),
        ast::Expr::Pos(arg) => Expr::Pos(Box::new(lower_expr(arg))),
        ast::Expr::Fre(arg) => Expr::Fre(Box::new(lower_expr(arg))),
        ast::Expr::Usr(arg) => Expr::Usr(Box::new(lower_expr(arg))),
        ast::Expr::Joy(arg) => Expr::Joy(Box::new(lower_expr(arg))),
        ast::Expr::Pot(arg) => Expr::Pot(Box::new(lower_expr(arg))),
        ast::Expr::Inkey => Expr::Inkey,
        ast::Expr::Lin => Expr::Lin,
        ast::Expr::At(row, col) => Expr::At(Box::new(lower_expr(row)), Box::new(lower_expr(col))),
        ast::Expr::Test(x, y) => Expr::Test(Box::new(lower_expr(x)), Box::new(lower_expr(y))),
        ast::Expr::Check { first, second } => Expr::Check {
            first: Box::new(lower_expr(first)),
            second: second.as_ref().map(|e| Box::new(lower_expr(e))),
        },
        ast::Expr::Inst {
            haystack,
            needle,
            start,
        } => Expr::Inst {
            haystack: Box::new(lower_str(haystack)),
            needle: Box::new(lower_str(needle)),
            start: start.as_ref().map(|e| Box::new(lower_expr(e))),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{self, DataValue, VarKind, VarName};

    fn fvar(base: &str) -> VarName {
        VarName {
            base: base.to_string(),
            kind: VarKind::Float,
        }
    }

    fn svar(base: &str) -> VarName {
        VarName {
            base: base.to_string(),
            kind: VarKind::String,
        }
    }

    #[test]
    fn lower_accepts_dynamic_for_step() {
        let i = fvar("I");
        let s = fvar("S");
        let prog = ast::Program {
            lines: vec![ast::Line {
                number: 10,
                statements: vec![ast::Statement::For {
                    var: i.clone(),
                    start: ast::Expr::Number(1.0),
                    end: ast::Expr::Number(10.0),
                    step: ast::Expr::Var(s.clone()),
                }],
            }],
        };

        let module = lower(&prog).unwrap();

        let Stmt::For { var, step, .. } = &module.lines[0].stmts[0] else {
            panic!("line 10 should lower to FOR");
        };
        assert_eq!(var, &i);
        assert_eq!(step, &Expr::Var(s));
    }

    #[test]
    fn lower_preserves_string_read_targets_and_mixed_data() {
        let n = fvar("N");
        let name = svar("NAME");
        let prog = ast::Program {
            lines: vec![
                ast::Line {
                    number: 10,
                    statements: vec![ast::Statement::Data(vec![
                        DataValue::Float(42.0),
                        DataValue::String(b"ALICE".to_vec()),
                    ])],
                },
                ast::Line {
                    number: 20,
                    statements: vec![ast::Statement::Read(vec![
                        ast::ReadTarget::Scalar(n.clone()),
                        ast::ReadTarget::Scalar(name.clone()),
                    ])],
                },
            ],
        };

        let module = lower(&prog).unwrap();

        let Stmt::Data(values) = &module.lines[0].stmts[0] else {
            panic!("line 10 should lower to DATA");
        };
        assert_eq!(
            values,
            &vec![DataValue::Float(42.0), DataValue::String(b"ALICE".to_vec())]
        );
        let Stmt::Read(targets) = &module.lines[1].stmts[0] else {
            panic!("line 20 should lower to READ");
        };
        assert!(matches!(&targets[0], ReadTarget::Scalar(v) if v == &n));
        assert!(matches!(&targets[1], ReadTarget::Scalar(v) if v == &name));
    }

    #[test]
    fn lower_resolves_tsb_proc_calls_to_gosub() {
        let proc = ast::ProcName(b"WORK".to_vec());
        let prog = ast::Program {
            lines: vec![
                ast::Line {
                    number: 10,
                    statements: vec![ast::Statement::ProcCall(proc.clone())],
                },
                ast::Line {
                    number: 100,
                    statements: vec![ast::Statement::ProcDef(proc)],
                },
                ast::Line {
                    number: 110,
                    statements: vec![ast::Statement::EndProc],
                },
            ],
        };

        let module = lower(&prog).unwrap();

        assert!(matches!(
            module.lines[0].stmts.as_slice(),
            [Stmt::GoSub { target: 100 }]
        ));
        assert!(matches!(module.lines[1].stmts.as_slice(), [Stmt::Rem(_)]));
        assert!(matches!(module.lines[2].stmts.as_slice(), [Stmt::Return]));
    }
}

#[derive(Debug)]
pub enum PassError {
    /// Reserved for passes that surface a contextual error message
    /// (none currently do — every existing pass is infallible).
    #[allow(dead_code)]
    Custom(String),
}

impl std::fmt::Display for PassError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PassError::Custom(s) => f.write_str(s),
        }
    }
}

impl std::error::Error for PassError {}

pub trait Pass {
    fn name(&self) -> &'static str;
    /// Primary entry point. Existing passes implement only this.
    fn run(&self, module: &mut Module) -> Result<(), PassError>;
    /// Modern entry point: opt-in for passes that want cached
    /// analysis results from the registry. The default ignores the
    /// registry so legacy passes work unchanged.
    fn run_with(
        &self,
        module: &mut Module,
        _registry: &mut crate::analysis::Registry,
    ) -> Result<(), PassError> {
        self.run(module)
    }
}

#[derive(Default)]
pub struct Pipeline {
    passes: Vec<Box<dyn Pass>>,
}

impl Pipeline {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn add<P: Pass + 'static>(&mut self, p: P) -> &mut Self {
        self.passes.push(Box::new(p));
        self
    }
    pub fn run(&self, module: &mut Module) -> Result<(), PassError> {
        let mut registry = crate::analysis::Registry::new();
        for p in &self.passes {
            p.run_with(module, &mut registry).map_err(|e| match e {
                PassError::Custom(s) => PassError::Custom(format!("{}: {s}", p.name())),
            })?;
            // Conservative: any pass might mutate the IR, so drop the
            // cache. Future work: declare per-pass invalidation.
            registry.invalidate();
        }
        Ok(())
    }
}
