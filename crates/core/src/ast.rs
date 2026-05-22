//! Abstract syntax tree for Commodore BASIC v2.
//!
//! The AST is the *frontend's* output: a typed, structural view of one
//! program. It deliberately keeps PETSCII string literals as raw byte
//! sequences (no transcoding), and keeps numbers in a representation wide
//! enough not to lose anything from BASIC v2's 5-byte float (we'll narrow
//! at codegen time when we know the target type).
//!
//! New BASIC statements are added by extending `Statement` and the parser.
//! Codegen falls back to a clear "unsupported" error for variants it
//! hasn't learned yet, so partial coverage is honest rather than silent.

#[derive(Debug, Clone)]
pub struct Program {
    pub lines: Vec<Line>,
}

#[derive(Debug, Clone)]
pub struct Line {
    pub number: u16,
    pub statements: Vec<Statement>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemTransferOp {
    Save,
    Load,
    Read,
}

#[derive(Debug, Clone)]
pub enum Statement {
    Print(PrintStmt),
    Goto(u16),
    GoSub(u16),
    Return,
    Let {
        name: VarName,
        value: Expr,
    },
    /// `IF cond THEN <then_body>`. THEN_LINE is a pure jump target form;
    /// THEN_STMTS are inline statements that run if cond is true.
    If {
        cond: Expr,
        then_branch: ThenBranch,
    },
    /// `IF cond THEN ... ELSE ...`. Kept separate from the V2 IF
    /// shape so existing V2-only optimizations keep their old match
    /// arms and we opt in to ELSE-aware handling where needed.
    IfElse {
        cond: Expr,
        then_branch: ThenBranch,
        else_branch: ThenBranch,
    },
    /// `IF cond THEN DO`: start of a multi-line conditional block,
    /// closed by `DONE` and optionally split by a standalone `ELSE`.
    DoIf {
        cond: Expr,
    },
    /// `DO` outside the `IF ... THEN DO` form. It is a structural
    /// marker in the interpreter; native code treats it as a no-op.
    Do,
    /// `DO NULL`: wait for one key press, then continue.
    DoNull,
    /// `DONE`: end of an `IF ... THEN DO` block.
    Done,
    /// standalone `ELSE` inside a `DO`/`DONE` block.
    Else,
    /// `REPEAT`: start of a post-test loop closed by `UNTIL`.
    Repeat,
    /// `UNTIL expr`: end of a `REPEAT` loop.
    Until {
        cond: Expr,
    },
    /// `LOOP`: start of an endless loop closed by `END LOOP`.
    Loop,
    /// `END LOOP`.
    EndLoop,
    /// `EXIT [IF expr]`: leave the innermost structured loop.
    ExitLoop {
        cond: Option<Expr>,
    },
    /// `CGOTO expr`: computed GOTO to a runtime-selected line.
    ComputedGoto {
        target: Expr,
    },
    /// `RCOMP THEN ...`: repeat the previous IF result.
    Rcomp {
        then_branch: ThenBranch,
        else_branch: Option<ThenBranch>,
    },
    /// `PROC name`: named procedure definition. Calls lower to
    /// GOSUB so existing subroutine passes can still see them.
    ProcDef(ProcName),
    /// `EXEC name` or omitted EXEC (`name` as a statement) —
    /// pushes a return address so `END PROC` returns to the next
    /// statement after the call.
    /// Lowers to GOSUB.
    ProcCall(ProcName),
    /// `CALL name` — tail call. It does not push a return address,
    /// so `END PROC` returns to the caller of the current procedure.
    /// Lowers to GOTO (the proc's first body line).
    ProcTailCall(ProcName),
    /// `END PROC`.
    EndProc,
    /// `LOCAL var [, var, ...]` — declare named variables as
    /// having storage scoped to the enclosing PROC body. The
    /// `localize_proc_vars` pass rewrites references inside the PROC
    /// body to mangled names so downstream passes (ConstVarProp,
    /// IntPromote, shadow-int, codegen) treat them as ordinary
    /// independent slots. The statement itself lowers to a no-op.
    Local {
        vars: Vec<VarName>,
    },
    /// `GLOBAL var [, var, ...]` — affirm that named variables
    /// refer to the program-wide slot rather than any enclosing
    /// `LOCAL` shadow. Default scope in BASIC v2 is global, so this
    /// is a no-op declaration; the localizer pass uses it to skip
    /// renaming inside a PROC body if the same name is also LOCAL.
    Global {
        vars: Vec<VarName>,
    },
    /// runtime interrupt/control commands. Native compilation can
    /// parse them and report a precise unsupported feature instead of
    /// failing on the raw token.
    OnKey {
        keys: StrExpr,
        /// Optional `GOTO`/`GOSUB <line>` action. `None` keeps the
        /// statement parseable on legacy listings that ended at the
        /// key string; codegen treats it as an immediate error so
        /// the user notices the missing target before runtime.
        target: Option<OnKeyAction>,
    },
    Disable,
    /// `RESUME` / `RESUME NEXT` / `RESUME <line>` — error
    /// handler continuation. The `Same` target re-executes the
    /// line that errored; `Next` skips to the line after; `Line`
    /// jumps to a specific line number.
    Resume {
        target: ResumeTarget,
    },
    /// `ON ERROR GOTO <line>` (Some(line)) installs a runtime
    /// error handler at the given line; bare `ON ERROR` (None) and
    /// `NO ERROR` both disable the handler. When no handler is
    /// installed, runtime errors fall through to the default
    /// BASIC ROM handler ("?XX ERROR IN line").
    OnError {
        target: Option<u16>,
    },
    /// `ERROR <expr>` — explicitly raise a BASIC error code.
    /// Routes through the same dispatcher as a real runtime error,
    /// so an installed `ON ERROR` handler can catch it.
    ErrorRaise {
        code: Expr,
    },
    /// `FOR var = start TO end [STEP step]`. STEP defaults to literal 1.0
    /// when absent; the parser resolves the default.
    For {
        var: VarName,
        start: Expr,
        end: Expr,
        step: Expr,
    },
    /// `NEXT [var]`. The variable is optional in BASIC v2; when absent we
    /// match the innermost open FOR at codegen time.
    /// `NEXT [var [, var ...]]` — one or more loop terminators on a
    /// single line. `vec![None]` represents bare `NEXT` (matches the
    /// innermost open FOR); each named var pops one frame in turn.
    Next {
        vars: Vec<Option<VarName>>,
    },
    Rem(Vec<u8>),
    End,
    /// Same as END except it prints `BREAK IN <line>` before returning
    /// to READY. Useful for marking debug halts.
    Stop,
    /// `RUN [line]` — restart from the first line (when None) or
    /// from `Some(line)`. Resets variables, arrays, the string heap,
    /// and the DATA pointer to their startup state in both cases.
    Run(Option<u16>),
    /// `CLR` — same state reset as RUN (variables, arrays, heap, DATA
    /// pointer) but without the program-counter jump. Execution
    /// continues at the statement after CLR.
    Clr,
    /// `POKE addr, value` — write `value` (low byte) to memory at `addr`.
    Poke {
        addr: Expr,
        value: Expr,
    },
    /// `D!POKE addr, value` — write a 16-bit word to addr/addr+1.
    Dpoke {
        addr: Expr,
        value: Expr,
    },
    /// Screen rectangle primitives. Coordinates are row/col
    /// in text cells; width/height are byte counts. `ch` is used by
    /// FCHR/FILL, `color` by FCOL/FILL, and INV needs neither.
    ScreenRect {
        op: ScreenRectOp,
        row: Expr,
        col: Expr,
        width: Expr,
        height: Expr,
        ch: Option<Expr>,
        color: Option<Expr>,
    },
    /// `MOVE row,col,w,h,to_row,to_col` — copy a text-screen
    /// rectangle, including color RAM, to another position.
    ScreenMove {
        row: Expr,
        col: Expr,
        width: Expr,
        height: Expr,
        dest_row: Expr,
        dest_col: Expr,
    },
    /// text-screen scroll primitives. `B` variants blank the
    /// exposed edge; `W` variants wrap it around.
    ScreenScroll {
        op: ScreenScrollOp,
        row: Expr,
        col: Expr,
        width: Expr,
        height: Expr,
    },
    /// `COLOR` / `COLOUR`. Missing operands are left untouched.
    Color {
        border: Option<Expr>,
        background: Option<Expr>,
        pen: Option<Expr>,
    },
    /// `MOB n ON/OFF` — set/clear the enable bit in $D015.
    MobEnable {
        index: Expr,
        enabled: bool,
    },
    /// `MULTI ON/OFF` — toggle the VIC-II multicolor bit
    /// ($D016 bit 4). In text mode this enables multicolor characters;
    /// after `HIRES` it switches between monochrome and multicolor
    /// bitmap rendering. The same bit serves both modes.
    Multi {
        enabled: bool,
    },
    /// `MULTI c1, c2, c3` — fills the bitmap screen-matrix
    /// with `(c1<<4)|c2`, colour RAM with `c3`, and enables
    /// multicolor mode. `%00` pixels keep `$D021`.
    MultiColors {
        c1: Expr,
        c2: Expr,
        c3: Expr,
    },
    /// `HIRES [ink [, paper]]` — switch to 320x200 monochrome
    /// bitmap mode. Clears the bitmap at $E000 and seeds the screen-
    /// RAM colour pairs at $C000 with `(ink << 4) | paper`. After
    /// HIRES the bitmap is addressable for `DRAW` / `LINE`. Use `NRM`
    /// to return to text mode. Without args defaults to light blue
    /// on white.
    Hires {
        ink: Option<Expr>,
        paper: Option<Expr>,
    },
    /// `BORDER expr` — write the border colour at $D020.
    /// Same single-byte semantics as `COLOR` for the border slot,
    /// kept as a dedicated statement so the literal-arg fast path
    /// can collapse to a single `LDA #imm / STA $D020`.
    Border {
        color: Expr,
    },
    /// `LINE x1,y1,x2,y2 [,mode]` — Bresenham line in the
    /// hires bitmap. The optional `mode` is the standard pixel
    /// op (0 = clear, 1 = set / c1, 2 = invert, 3 = c2 in MULTI,
    /// 4 = c3 in MULTI). When omitted, leaves the current draw
    /// mode untouched in the sticky draw-mode byte.
    Line {
        x1: Expr,
        y1: Expr,
        x2: Expr,
        y2: Expr,
        mode: Option<Expr>,
    },
    /// Single-pixel hires plot. `PLOT x,y[,mode]` and the compat
    /// shortcut `DRAW x,y[,mode]` both lower here. The full
    /// `DRAW s$,x,y,...` turtle form is a separate `DrawString`.
    Draw {
        x: Expr,
        y: Expr,
        mode: Option<Expr>,
    },
    /// `REC x,y,width,height [,mode]` — outline a rectangle
    /// by drawing its four edges (lowers to four `LINE`s).
    Rec {
        x: Expr,
        y: Expr,
        width: Expr,
        height: Expr,
        mode: Option<Expr>,
    },
    /// `BLOCK x1,y1,x2,y2 [,mode]` — fill the rectangle
    /// bounded by `(x1, y1)` and `(x2, y2)` (corners inclusive).
    /// Lowers to a runtime loop of horizontal `LINE` calls.
    Block {
        x1: Expr,
        y1: Expr,
        x2: Expr,
        y2: Expr,
        mode: Option<Expr>,
    },
    /// `CIRCLE cx,cy,r` — full circle outline using a
    /// `CIRCLE cx,cy,rx [,ry [,start [,end [,step]]]]`. Without any
    /// trailing args the renderer uses a midpoint-Bresenham circle
    /// (fast, pixel-perfect). When `ry`, `start`, `end`, or `step`
    /// is supplied, it falls back to a parametric loop using the
    /// shared sin/cos LUT — supporting ellipses (`ry != rx`) and
    /// arcs (`start..end` in 0..255 angle units, default step 16,
    /// default end = full revolution).
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
    /// `CHAR x,y,code [,mode [,zoom]]` — render one
    /// character cell from the character ROM into the bitmap.
    /// `mode` is the pixel op (0 = clear, 1 = set, 2 = invert;
    /// default 1). `zoom` is 1 (8x8) or 2 (16x16, both axes
    /// doubled; default 1). Anything past `zoom` is parsed and
    /// ignored.
    Char {
        x: Expr,
        y: Expr,
        code: Expr,
        mode: Option<Expr>,
        zoom: Option<Expr>,
    },
    /// `TEXT x,y,s$ [,mode [,zoom [,kerning]]]` — render
    /// a string by iterating CHAR over each PETSCII byte (after
    /// PETSCII → screen-code mapping). `mode` and `zoom` pass
    /// through to CHAR. `kerning` is the per-glyph X advance in
    /// pixels; zoom stretches the glyph vertically, not horizontally.
    Text {
        x: Expr,
        y: Expr,
        text: StrExpr,
        mode: Option<Expr>,
        zoom: Option<Expr>,
        kerning: Option<Expr>,
    },
    /// `DRAW TO x,y [,mode]` — line from the persistent
    /// graphics cursor to `(x, y)`, then update the cursor.
    DrawTo {
        x: Expr,
        y: Expr,
        mode: Option<Expr>,
    },
    /// `ROT direction[,length]` — set 8-direction draw state.
    /// The compiled helper writes the canonical `drawtabx` /
    /// `drawtaby` 4-byte delta tables used by `DRAW string,...`.
    /// `length` is accepted and stored as the per-step pixel count
    /// (default 1).
    Rot {
        direction: Expr,
        length: Option<Expr>,
    },
    /// full-form `DRAW code$, x, y [, mode]` — turtle-graphics
    /// interpreter. Each character of `code$` is a digit-encoded
    /// direction (0..3 pen-up, 5..8 pen-down) relative to the
    /// current `ROT` orientation. Cursor starts at `(x, y)`.
    DrawString {
        code: StrExpr,
        x: Expr,
        y: Expr,
        mode: Option<Expr>,
    },
    /// `PAINT x,y [,mode]` — flood-fill the contiguous area
    /// containing `(x, y)`. The mode arg picks pen colour (and is
    /// also used to clear/invert).
    Paint {
        x: Expr,
        y: Expr,
        mode: Option<Expr>,
    },
    /// `ANGL cx,cy,angle,rx[,ry[,mode]]` — polar line from
    /// `(cx, cy)` along `angle` (0..255 = 0°..360°). With `ry !=
    /// rx` the endpoint follows an ellipse-shaped offset; with
    /// `ry` omitted, equals `rx` (pure-circular segment). The
    /// `mode` arg picks the pixel op for the underlying line.
    Angl {
        cx: Expr,
        cy: Expr,
        angle: Expr,
        rx: Expr,
        ry: Option<Expr>,
        mode: Option<Expr>,
    },
    /// `SOUND voice,freq` — write SID voice frequency.
    Sound {
        voice: Expr,
        freq: Expr,
    },
    /// `ENVELOPE voice,a,d,s,r` — write SID ADSR nybbles.
    Envelope {
        voice: Expr,
        attack: Expr,
        decay: Expr,
        sustain: Expr,
        release: Expr,
    },
    /// `WAVE voice,ctrl[,pulse]` — write SID control and
    /// optional pulse width.
    Wave {
        voice: Expr,
        control: Expr,
        pulse: Option<Expr>,
    },
    /// `MUSIC tempo, tune$` — stage a compact note string for
    /// the native SID player used by `PLAY`.
    Music {
        tempo: Expr,
        tune: StrExpr,
    },
    /// `PLAY n` / `PLAY ON` / `PLAY OFF`. Mode 0 stops, 1 plays
    /// synchronously until the tune ends, and 2/ON starts line-polled
    /// background playback.
    Play {
        mode: Expr,
    },
    /// `FLASH` — lightweight background-colour flasher. The
    /// native compiler uses a line-entry poll rather than installing
    /// an IRQ, so it stays compatible with ON KEY and optimizer state.
    Flash {
        enabled: Option<bool>,
        speed: Option<Expr>,
        color1: Option<Expr>,
        color2: Option<Expr>,
    },
    /// `BFLASH` — border-colour flasher with the same native
    /// line-entry poll model as `FLASH`.
    Bflash {
        enabled: Option<bool>,
        speed: Option<Expr>,
        color1: Option<Expr>,
        color2: Option<Expr>,
    },
    /// `HI COL` — reset the internal low-colour graphics mode
    /// flag used by later graphics commands.
    HiCol,
    /// `LOW COL c1,c2[,c3]` — stage low-colour mode state for
    /// later graphics commands.
    LowCol {
        color1: Expr,
        color2: Expr,
        color3: Option<Expr>,
    },
    /// `MOD ink, paper` — bulk recolour the HIRES screen-RAM
    /// (cell-attribute) area with the packed nibble pair (ink<<4 |
    /// paper). Lets the user re-colour everything already drawn in
    /// HIRES without redrawing.
    Mod {
        ink: Expr,
        paper: Expr,
    },
    /// `DUP src_x, src_y, width, height, dst_x, dst_y
    /// [,mode [,zoom]]` — copy a HIRES bitmap region. For each
    /// source pixel that is set, the destination pixel is updated
    /// per `mode` (default 1 = set). `zoom` (default 1) scales each
    /// source pixel into a zoom×zoom block at the destination.
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
    /// Native utility form: `COPY src,dst,len`.
    /// Copies arbitrary memory and is useful for screen/bitmap blits
    /// that do not need DUP's pixel-level mode handling.
    Copy {
        src: Expr,
        dst: Expr,
        len: Expr,
    },
    /// Native snapshot form: `SCRSV [addr[,mode]]`. Mode 0 copies
    /// text screen + colour RAM, mode 1 copies the current HIRES
    /// bitmap from $E000. When `addr` is omitted, the current SCRSV
    /// default set by `SCRSV DEF` is used.
    ScrSave {
        addr: Option<Expr>,
        mode: Option<Expr>,
    },
    /// Native snapshot restore: `SCRLD [addr[,mode]]`. Uses the same
    /// modes as `SCRSV`; omitted `addr` uses the SCRLD default.
    ScrLoad {
        addr: Option<Expr>,
        mode: Option<Expr>,
    },
    /// Native default setup for bare `SCRSV` / `SCRLD`. Compiled code
    /// keeps the useful part of `DEF`: default snapshot address and mode.
    ScrDef {
        save: bool,
        addr: Expr,
        mode: Option<Expr>,
    },
    /// Restore the compiled SCRSV/SCRLD default to text-screen snapshot
    /// at $C000, mode 0.
    ScrRestore {
        save: bool,
    },
    /// Native memory manager subset: `MEM CLR addr,len[,value]`.
    MemClr {
        addr: Expr,
        len: Expr,
        value: Option<Expr>,
    },
    /// REU transfer commands: `MEMSAVE`, `MEMLOAD`, `MEMREAD`.
    MemTransfer {
        op: MemTransferOp,
    },
    /// REU register setup: `MEMDEF len[,c64[,reu[,bank[,auto[,fix]]]]]`.
    MemDef {
        len: Expr,
        c64_addr: Option<Expr>,
        reu_addr: Option<Expr>,
        reu_bank: Option<Expr>,
        auto_inc: Option<Expr>,
        fixed: Option<Expr>,
    },
    /// `MEMLEN len`.
    MemLen {
        len: Expr,
    },
    /// `MEMOR addr` — C64-side transfer address.
    MemC64Addr {
        addr: Expr,
    },
    /// `MEMPOS addr,bank` — REU-side transfer address.
    MemReuPos {
        addr: Expr,
        bank: Expr,
    },
    /// `MEMRESTORE flag` — REU auto-increment flag.
    MemRestore {
        auto_inc: Expr,
    },
    /// `MEMCONT mode` — REU address-hold mode.
    MemCont {
        mode: Expr,
    },
    /// Native runtime data definition helper. Two source shapes
    /// fold into this same variant:
    ///   * Legacy compiler-internal form: `DESIGN addr, b1, b2, ...`
    ///     with `bytes` populated from comma-separated literals.
    ///   * native form: `DESIGN type, addr` followed by 8 (or
    ///     21) `@`-prefixed bitmap rows. The post-parse design-group
    ///     pass converts those rows into the byte sequence and
    ///     rewrites this statement so the codegen path is identical.
    Design {
        addr: Expr,
        bytes: Vec<Expr>,
    },
    /// One `@`-prefixed bitmap row from a DESIGN block.
    /// Holds the raw row characters (`.`, ` `, `A`, `B`, `C`, `D`)
    /// so the design-group pass can decode them once it knows the
    /// type from the preceding DESIGN setup. Replaced by a no-op
    /// (Statement::Rem) once the rows are folded.
    DesignRow(Vec<u8>),
    /// `MMOB n,x,y` — simple absolute sprite positioning.
    /// The glide form takes a separate variant.
    Mmob {
        index: Expr,
        x: Expr,
        y: Expr,
    },
    /// `MMOB n, sx, sy, ex, ey [, size [, speed]]` — glide from
    /// `(sx, sy)` to `(ex, ey)`.
    /// `speed` overrides the sprite's `__MOB_SPEED[n]` entry; `size`
    /// is parsed but currently routed back through `MOB SET`-style
    /// handling at the codegen site.
    MmobGlide {
        index: Expr,
        sx: Expr,
        sy: Expr,
        ex: Expr,
        ey: Expr,
        size: Option<Expr>,
        speed: Option<Expr>,
    },
    /// `MOB SET n,block,color,priority,type[,size[,speed]]`.
    /// `speed` stores the glide delay used by MMOB/RLOCMOB animations.
    MobSet {
        index: Expr,
        block: Expr,
        color: Expr,
        priority: Expr,
        multicolor: Expr,
        size: Option<Expr>,
        speed: Option<Expr>,
    },
    /// `RLOCMOB n, x, y [, size [, speed]]` — despite the name,
    /// `x`/`y` are the *absolute* target (`befrlocm` seeds the start
    /// with the current VIC pos and glides to (x, y)).
    Rlocmob {
        index: Expr,
        dx: Expr,
        dy: Expr,
        speed: Option<Expr>,
    },
    /// `DETECT mode` — select collision register for CHECK().
    Detect {
        mode: Expr,
    },
    /// `CMOB c1,c2` — shared sprite multicolour registers.
    Cmob {
        color1: Expr,
        color2: Expr,
    },
    /// `BCKGNDS c0,c1,c2,c3` — background colour registers.
    Bckgnds {
        color0: Expr,
        color1: Expr,
        color2: Expr,
        color3: Expr,
    },
    /// `NRM` — restore normal text screen mode.
    Nrm,
    /// bare `MEM` — switch to MEM mode: copy char ROM
    /// $D000-$DFFF to $E000-$EFFF, switch VIC bank to 3, set
    /// $D018 so screen reads from $CC00 and chars from $E000,
    /// and update KERNAL `$0288` so CHROUT writes to $CC00.
    /// Required for programs that DESIGN custom chars at $E000+
    /// to actually have them appear on screen.
    MemModeOn,
    /// `CSET n` — select character set / graphics charset mode.
    Cset {
        mode: Expr,
    },
    /// `PAUSE [msg$,] n` — optionally display `msg$`, then wait
    /// roughly `n` jiffies. RETURN exits the wait early.
    Pause {
        message: Option<StrExpr>,
        ticks: Expr,
    },
    /// `SYS addr [, A [, X [, Y [, SR]]]]` — JSR to address (no
    /// return value). Optional trailing args follow the C128 BASIC
    /// 7.0 convention: stamped into `$030C-$030F` (SADRA / SADRX /
    /// SADRY / SADRS) before the call so the ML routine sees them
    /// in the corresponding CPU registers. Empty vec = bare SYS.
    Sys {
        addr: Expr,
        regs: Vec<Expr>,
    },
    /// `WAIT addr, mask [, eor]` — busy-poll memory at `addr` until
    /// `(byte XOR eor) AND mask` is non-zero. `eor` defaults to 0.
    Wait {
        addr: Expr,
        mask: Expr,
        eor: Option<Expr>,
    },
    /// `OPEN file [, device [, secondary [, filename]]]`. file_num is
    /// required; the rest default per BASIC v2 (device=1, secondary
    /// chosen by KERNAL, no filename). Filename can be any string
    /// expression — literal, variable, or composed via `+`.
    Open {
        file_num: Expr,
        device: Option<Expr>,
        secondary: Option<Expr>,
        filename: Option<StrExpr>,
    },
    /// `CLOSE file_num` — releases the logical file via KERNAL CLOSE.
    Close {
        file_num: Expr,
    },
    /// `PRINT# file_num, items` — like PRINT but writes to the
    /// channel for `file_num` instead of the screen.
    PrintFile {
        file_num: Expr,
        body: PrintStmt,
    },
    /// `GET# file_num, var [, var2, …]` — read one byte from the
    /// channel for `file_num` into each `var` in turn (PETSCII char
    /// if string-typed, numeric 0..255 if numeric). A comma-separated
    /// variable list emits one read per variable.
    GetFile {
        file_num: Expr,
        vars: Vec<VarName>,
    },
    /// `INPUT# file_num, var [, var ...]` — read CR-terminated line
    /// from the channel and parse into the vars (same parse path as
    /// regular INPUT for VAL/string-copy semantics).
    InputFile {
        file_num: Expr,
        targets: Vec<ReadTarget>,
    },
    /// `CMD file_num [, items]` — like PRINT# but leaves the output
    /// channel redirected after returning. Subsequent PRINTs go to the
    /// channel until an explicit CLOSE or CLRCHN restores defaults.
    Cmd {
        file_num: Expr,
        body: PrintStmt,
    },
    /// `LOAD "name" [, device [, secondary]]`. Calls KERNAL LOAD with
    /// .A=0. Default device 1 (cassette), default secondary 1 (use
    /// the load address from the file header). For raw-byte data
    /// loads from disk a typical invocation is `LOAD "name", 8, 1`.
    Load {
        filename: StrExpr,
        device: Option<Expr>,
        secondary: Option<Expr>,
        /// `LOAD ... USE,0,addr` — force KERNAL LOAD to place
        /// raw data at `addr` instead of using the file header.
        load_addr: Option<Expr>,
    },
    /// `VERIFY ...` — same shape as LOAD but with .A=1.
    Verify {
        filename: StrExpr,
        device: Option<Expr>,
        secondary: Option<Expr>,
    },
    /// `SAVE "name" [, device [, secondary]]`. Saves the program plus
    /// any heap data ($0801 .. __HEAP_PTR). Note: BASIC v2's SAVE
    /// targets the BASIC text area; in our compiled context we save
    /// the same physical range (which IS our compiled binary).
    Save {
        filename: StrExpr,
        device: Option<Expr>,
        secondary: Option<Expr>,
    },
    /// `DISK "cmd"` — send a command to drive 8 via logical
    /// file 15. This is the programmatic command-channel form.
    Disk {
        command: StrExpr,
    },
    /// `DATA <values>` — pure declaration; runtime no-op. Values are
    /// pooled across the whole program by the codegen pass. Numeric
    /// and string literals coexist; the parser picks the matching
    /// `DataValue` variant per item.
    Data(Vec<DataValue>),
    /// `READ tgt [, tgt ...]` — pull next value(s) from the DATA pool
    /// into the named scalar or array-element targets, advancing the
    /// read pointer.
    Read(Vec<ReadTarget>),
    /// `RESTORE` — reset the DATA pointer to the start of the pool.
    Restore,
    /// `RESET <line>` — set the DATA pointer to the stream
    /// starting at the named BASIC line.
    Reset {
        line: u16,
    },
    /// `GET var` — read one keystroke (PETSCII code) into the numeric
    /// variable. Non-blocking; sets the variable to 0 if no key.
    Get {
        var: VarName,
    },
    /// `KEYGET var` — blocking GET. Waits until a key is
    /// available, then applies regular BASIC GET assignment semantics.
    KeyGet {
        var: VarName,
    },
    /// `FETCH ctrl$,max,target$[,force]` — read an editable
    /// keyboard field into a string variable. The compiler stores
    /// the result in a runtime-owned buffer and points the target
    /// descriptor at it.
    Fetch {
        control: StrExpr,
        max_len: Expr,
        target: VarName,
        /// Optional array subscripts on `target` — empty for a
        /// scalar string var (`FETCH … , F$`), non-empty for an
        /// element write such as `FETCH ..., A$(I)`.
        target_indices: Vec<Expr>,
        force: Option<Expr>,
        /// Optional `AT(row, col)` cursor-positioning prefix.
        /// Lowers to a KERNAL `PLOT` before `__FETCH_READ` runs so
        /// the cursor block draws at the
        /// right spot.
        position: Option<(Expr, Expr)>,
    },
    /// `KEY n,text$` — set a 16-byte function-key table entry.
    KeySet {
        index: Expr,
        text: StrExpr,
    },
    /// `DISPLAY` — show the native function-key table.
    DisplayKeys,
    /// `AT(a$, b$)` statement: exchange two string variables.
    SwapStr {
        lhs: VarName,
        rhs: VarName,
    },
    /// `INSERT box$, row, col, width, height, color` screen-box
    /// command. The 9-byte pattern is interpreted as top/middle/bottom
    /// triples: left, fill, right.
    InsertBox {
        pattern: StrExpr,
        row: Expr,
        col: Expr,
        width: Expr,
        height: Expr,
        color: Expr,
    },
    /// `DIM A(N) [, B(M) ...]` — declare arrays. Sizes must be literal
    /// in this iteration; BASIC v2 allows expressions but those need a
    /// runtime allocator we haven't built yet.
    Dim(Vec<DimSpec>),
    /// `A(I[, J, ...]) = expr` — write one numeric array element.
    ArrayLet {
        name: VarName,
        indices: Vec<Expr>,
        value: Expr,
    },
    /// `A$(I[, J, ...]) = strexpr` — write one string array element.
    ArrayLetStr {
        name: VarName,
        indices: Vec<Expr>,
        value: StrExpr,
    },
    /// `A$ = <string-expr>` — assign to a string variable.
    LetStr {
        var: VarName,
        value: StrExpr,
    },
    /// `ON expr GOTO n1, n2, ...` (branch=Goto) or `ON expr GOSUB ...`
    /// (branch=GoSub). Evaluates `expr` to an integer; if 1..=N, takes
    /// the corresponding branch. Otherwise falls through.
    OnBranch {
        value: Expr,
        kind: OnBranchKind,
        targets: Vec<u16>,
    },
    /// `INPUT [prompt;] var [, var ...]` — print the prompt (or just
    /// `?`), read one keyboard line, and stash the result(s) into the
    /// variables. For string vars the line is heap-copied; for numeric
    /// vars it goes through `VAL`.
    Input {
        prompt: Option<Vec<u8>>,
        targets: Vec<ReadTarget>,
    },
    /// `DEF FN F(X) = expr` — declare a single-argument user function.
    /// Pure declaration with no runtime effect; the codegen pre-pass
    /// collects all DefFn statements into a name→body map and emits the
    /// bodies as helper subroutines. The parameter `X` is just an
    /// ordinary numeric variable — calls assign the argument into its
    /// slot before invoking the body, exactly mirroring BASIC v2's
    /// behaviour where calling FN F(5) leaves X = 5 visible to the
    /// rest of the program.
    DefFn {
        name: FnName,
        param: VarName,
        body: Expr,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenRectOp {
    Fchr,
    Fcol,
    Fill,
    Inv,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenScrollOp {
    UpBlank,
    UpWrap,
    LeftWrap,
    LeftBlank,
    DownBlank,
    DownWrap,
    RightBlank,
    RightWrap,
}

/// String-typed expression. The phase-1 surface is intentionally tiny
/// (literal or variable); concatenation and string functions land later
/// once the heap allocator is in place.
#[derive(Debug, Clone)]
pub enum StrExpr {
    /// PETSCII bytes between the quote marks (no length byte stored —
    /// the codegen prepends one when laying the literal into the pool).
    Literal(Vec<u8>),
    /// Reference to another string variable; codegen copies the pointer.
    Var(VarName),
    /// `CHR$(expr)` as a string-producing function.
    Chr(Box<Expr>),
    /// `$$<expr>` — convert int16 expression to a hex digit
    /// string. Width is 2 if the high byte is zero, 4 otherwise.
    HexFmt(Box<Expr>),
    /// `%%<expr>` — convert int16 expression to a binary
    /// digit string. Width is 8 if the high byte is zero, 16
    /// otherwise. Sibling of `HexFmt`.
    BinFmt(Box<Expr>),
    /// `GET` peeks the keyboard buffer; the result is naturally a 1-char
    /// (or empty) string. We model it as a string-expression source so
    /// `GET A$` can lower to `Stmt::LetStr { var: A$, value: GetKey }`.
    GetKey,
    /// `s$ + t$` — produce a new string by joining two. Codegen
    /// allocates the result on a bump-allocated heap.
    Concat(Box<StrExpr>, Box<StrExpr>),
    /// `STR$(n)` — number to its decimal string representation.
    Str(Box<Expr>),
    /// `LEFT$(s$, n)` — first `n` characters (clamped to length).
    Left(Box<StrExpr>, Box<Expr>),
    /// `RIGHT$(s$, n)` — last `n` characters (clamped to length).
    Right(Box<StrExpr>, Box<Expr>),
    /// `MID$(s$, start [, n])` — substring from `start` (1-indexed) of
    /// length `n` (or to the end if omitted).
    Mid(Box<StrExpr>, Box<Expr>, Option<Box<Expr>>),
    /// `DUP(s$, n)` — repeat `s$` `n` times.
    Dup(Box<StrExpr>, Box<Expr>),
    /// `INSERT(s$, t$, pos)` — insert `t$` into `s$` at the
    /// 1-based position `pos`.
    Insert(Box<StrExpr>, Box<StrExpr>, Box<Expr>),
    /// `A$(I[, J, ...])` — read one element from a string array.
    ArrayRef(VarName, Vec<Expr>),
}

#[derive(Debug, Clone)]
pub struct DimSpec {
    pub name: VarName,
    /// Highest valid index per dimension. `DIM A(10)` gives one entry,
    /// `DIM A(10, 5)` gives two. Each is parsed as a general expression
    /// to allow forms like `DIM A(N+1)`; the constant-fold pass collapses
    /// it to a literal `Number(...)` and codegen requires that literal to
    /// allocate storage statically. Truly dynamic sizing would need a
    /// runtime allocator and hasn't shipped yet.
    pub dims: Vec<Expr>,
}

/// One target in a `READ` statement: either a scalar variable or one
/// element of a DIMmed array.
#[derive(Debug, Clone)]
pub enum ReadTarget {
    Scalar(VarName),
    Array { name: VarName, indices: Vec<Expr> },
}

/// One value in a `DATA` statement. Numeric and string literals
/// coexist in the same pool; READ dispatches on the target variable's
/// kind (numeric → __VAL_HELPER on the entry's bytes; string → V_var
/// pointed straight at the entry, since pool strings are immutable).
#[derive(Debug, Clone, PartialEq)]
pub enum DataValue {
    Float(f64),
    String(Vec<u8>),
}

#[derive(Debug, Clone)]
pub enum ThenBranch {
    /// `IF cond THEN 100` — single line-number jump.
    Goto(u16),
    /// `IF cond THEN PRINT "x" : A=1` — a sequence of inline statements.
    Stmts(Vec<Statement>),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProcName(pub Vec<u8>);

impl ProcName {
    pub fn display_lossy(&self) -> String {
        String::from_utf8_lossy(&self.0).trim().to_string()
    }
}

/// Action to dispatch when an `ON KEY` trap fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnKeyAction {
    /// `ON KEY "..." GOTO <line>` — jump to `target` when the key
    /// is pressed; do not save a return address (the original
    /// program flow is abandoned).
    Goto(u16),
    /// `ON KEY "..." GOSUB <line>` — like GOTO but pushes a return
    /// address so a matching `RETURN` resumes the interrupted code.
    /// Currently parsed for compatibility; codegen treats it as GOTO.
    GoSub(u16),
}

impl OnKeyAction {
    pub fn target(&self) -> u16 {
        match self {
            OnKeyAction::Goto(n) | OnKeyAction::GoSub(n) => *n,
        }
    }
}

/// Continuation target for `RESUME`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeTarget {
    /// Plain `RESUME` — re-execute the line that errored.
    Same,
    /// `RESUME NEXT` — continue at the line after the errored one.
    Next,
    /// `RESUME <line>` — jump to a specific line.
    Line(u16),
}

#[derive(Debug, Clone)]
pub struct PrintStmt {
    pub items: Vec<PrintItem>,
    /// True iff PRINT should append a newline. False when the statement
    /// ends with `;` or `,` (matches BASIC v2 semantics).
    pub trailing_newline: bool,
}

#[derive(Debug, Clone)]
pub enum PrintItem {
    /// PETSCII bytes between the quote marks (no NUL terminator stored).
    /// Reserved for parser paths that bypass the StrExpr layer; the
    /// current parser routes literals through `StrExpr(Literal(_))`,
    /// but downstream consumers still match on this variant so we keep
    /// it around.
    #[allow(dead_code)]
    String(Vec<u8>),
    /// Suppresses any separator. In BASIC the trailing `;` also suppresses
    /// the newline; that's tracked on `PrintStmt::trailing_newline`.
    Semi,
    /// Tabs to the next 10-column field.
    Comma,
    /// Numeric / string expression.
    Expr(Expr),
    /// `CHR$(expr)` — emit one PETSCII character via CHROUT. Special-cased
    /// in PRINT so we don't need full string-variable infrastructure for
    /// the common patterns (`PRINT CHR$(147)` to clear screen, etc).
    CharOut(Expr),
    /// `TAB(n)` — emit spaces until the cursor reaches column `n`.
    /// Cursor column is read from PNTR ($D3). If already at or past `n`,
    /// the call is a no-op (matches BASIC v2: it never moves *backwards*).
    Tab(Expr),
    /// `SPC(n)` — emit `n` spaces unconditionally (relative spacing).
    Spc(Expr),
    /// String expression — literal, string-variable reference, or
    /// CHR$-style producer. Codegen routes through __STR_PRINT.
    StrExpr(StrExpr),
    /// `AT(row, col)` cursor positioning prefix in PRINT /
    /// CENTER. Lowers to `CLC; LDX row; LDY col; JSR $FFF0` (KERNAL
    /// PLOT). Distinct from `Expr::At(row, col)` (screen-RAM peek)
    /// because the syntactic context disambiguates: as a leading
    /// item in PRINT/CENTER it positions the cursor; in numeric
    /// context it reads a screen byte.
    PositionAt(Expr, Expr),
    /// `USE`-formatted-PRINT field — a `#`-run of `width`
    /// chars in the control string takes the next numeric var and
    /// prints it right-justified, space-padded, in that width.
    /// Emitted by the `USE [AT(r,c)] "...###...", v` parser; the
    /// surrounding literal chunks of the control string ride
    /// alongside as `StrExpr(Literal(_))` items.
    UseField { width: u8, value: Expr },
}

#[derive(Debug, Clone)]
pub enum Expr {
    /// Decimal-literal numeric. Stored as f64 because BASIC v2 numbers are
    /// 5-byte floats; a Rust `f64` is wider than necessary but lossless
    /// for everything BASIC can encode as a literal.
    Number(f64),
    /// PETSCII string literal.
    String(Vec<u8>),
    Var(VarName),
    Neg(Box<Expr>),
    /// `NOT n` — bitwise complement of the operand's signed 16-bit
    /// truncation. Combined with the BASIC-comparison convention
    /// (-1.0 = true, 0.0 = false), `NOT (a=b)` reads naturally as
    /// "a is not equal to b".
    Not(Box<Expr>),
    Bin(BinOp, Box<Expr>, Box<Expr>),
    /// Built-in one-argument numeric function — `ABS(x)`, `INT(x)`, etc.
    /// Each maps to a single C64 BASIC ROM call applied to FAC.
    Func1(Func1, Box<Expr>),
    /// `PEEK(addr)` — read one byte from memory at `addr`, returns 0..255.
    Peek(Box<Expr>),
    /// `NRM(s$)` function — parse the string as a number
    /// using auto-format detection:
    ///   * len ≥ 8: binary digits, optionally prefixed with `%`.
    ///     8 digits → byte (low 8 bits).
    ///     16 digits → word (16 bits).
    ///   * len < 8: hex digits, optionally prefixed with `$`.
    /// Different from the `NRM` *statement* which restores the
    /// normal text screen mode.
    Nrm(Box<StrExpr>),
    /// `MEMPEEK(addr)` — like PEEK but bank-switches char
    /// ROM in (clears `$01` bit 2 = CHAREN) before the read so
    /// addresses in the `$D000-$DFFF` range hit the character ROM
    /// instead of the I/O area. Falls back to PEEK behaviour for
    /// other addresses since the bank-switch is harmless there.
    MemPeek(Box<Expr>),
    /// `A(I[, J, ...])` — read one element from a numeric array.
    ArrayRef(VarName, Vec<Expr>),
    /// `LEN(s$)` — number of characters in a string.
    Len(Box<StrExpr>),
    /// `ASC(s$)` — PETSCII code of the first char (raises ?ILLEGAL
    /// QUANTITY at runtime if the string is empty, like BASIC v2).
    Asc(Box<StrExpr>),
    /// `s$ <op> t$` — string comparison yielding -1.0 (true) or 0.0
    /// (false). Phase 1 supports `=` and `<>` only; lexicographic
    /// ordering (`<`, `>`, `<=`, `>=`) is deferred.
    StrCompare(BinOp, StrExpr, StrExpr),
    /// `VAL(s$)` — parse the string as a number; 0 if it doesn't
    /// parse (BASIC v2 semantics: trailing garbage is silently
    /// ignored, e.g. `VAL("12abc")` is 12).
    Val(Box<StrExpr>),
    /// `FN F(arg)` — invoke a previously-declared user function. The
    /// argument is evaluated and stored into the function's parameter
    /// slot before the body runs, then the body's result is returned in
    /// FAC. Codegen rejects calls to undefined names.
    FnCall(FnName, Box<Expr>),
    /// `POS(x)` — current cursor column from KERNAL's PNTR ($D3). The
    /// argument is evaluated for side effects but its value is ignored.
    Pos(Box<Expr>),
    /// `FRE(x)` — bytes of free RAM between the heap top and MEMSIZ.
    /// Same arg-discard semantics as POS.
    Fre(Box<Expr>),
    /// `USR(x)` — call user machine code via the standard USR vector
    /// at $0311/$0312. The argument is evaluated to FAC; the user
    /// routine returns with its result also in FAC.
    Usr(Box<Expr>),
    /// `JOY(n)` — normalised joystick direction plus fire bit.
    Joy(Box<Expr>),
    /// `POT(n)` — paddle value from SID POTX/POTY.
    Pot(Box<Expr>),
    /// `INKEY` — non-blocking keyboard read.
    Inkey,
    /// `LIN` — current cursor row via KERNAL PLOT.
    Lin,
    /// `AT(row,col)` — screen code at row/column.
    At(Box<Expr>, Box<Expr>),
    /// `TEST(x,y)` — pixel sense in the HIRES bitmap.
    /// Returns 0 if the pixel at (x, y) is clear, non-zero if set.
    /// Out-of-bounds counts as "set" (matches PAINT's boundary
    /// handling, since the same `__PIXEL_TEST` helper is reused).
    Test(Box<Expr>, Box<Expr>),
    /// `CHECK(n[,m])` — collision test. Returns 0 on detected
    /// collision, 1 otherwise.
    Check {
        first: Box<Expr>,
        second: Option<Box<Expr>>,
    },
    /// `INST(haystack$, needle$[,start])` — 1-based substring
    /// search, 0 when not found.
    Inst {
        haystack: Box<StrExpr>,
        needle: Box<StrExpr>,
        start: Option<Box<Expr>>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Func1 {
    Abs,
    Int,
    Sgn,
    Sqr,
    Sin,
    Cos,
    Tan,
    Atn,
    Log,
    Exp,
    Rnd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    /// `lhs ^ rhs` — exponentiation. Lowers to BASIC ROM's FPWRT
    /// routine (FAC = ARG^FAC), so codegen has to load the base into
    /// ARG explicitly rather than using the standard memory-operand
    /// pattern shared by `+ - * /`.
    Pow,
    /// Comparison ops produce -1.0 (true) or 0.0 (false), per BASIC v2.
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    Xor,
}

/// A canonicalised BASIC variable name.
///
/// BASIC v2 only distinguishes the first 2 characters of a name and is
/// case-insensitive (in practice all source is upper-case PETSCII). The
/// type suffix (`%` integer, `$` string, none = float) selects a separate
/// variable namespace, so `A` and `A%` are different variables.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VarName {
    /// 1 or 2 upper-case ASCII characters.
    pub base: String,
    pub kind: VarKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VarKind {
    Float,
    Integer,
    String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnBranchKind {
    Goto,
    GoSub,
}

/// Name of a user-defined function (the `F` in `FN F(X)`). BASIC v2
/// treats FN names as a separate namespace from regular variables and
/// honours only the first 2 characters; we store them upper-case.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FnName(pub String);

impl FnName {
    pub fn label(&self) -> String {
        format!("__FN_{}", self.0)
    }
}

impl VarName {
    pub fn label(&self) -> String {
        // Kind goes in the prefix, not the suffix — a suffix-based
        // scheme collides whenever a Float base ends in `I` or `S`
        // and matches some Integer/String base + suffix. CavesOfIce
        // hit `V_II` (Float `II`) and `V_II` (Integer `I` + suffix
        // `I`) sharing the same BSS slot, with the float read
        // straddling the integer byte and the next variable.
        let prefix = match self.kind {
            VarKind::Float => "V_",
            VarKind::Integer => "VI_",
            VarKind::String => "VS_",
        };
        format!("{prefix}{}", self.base)
    }
}
