//! Parse a tokenized BASIC v2 line body into AST statements.
//!
//! Statements within one line are separated by `:`. We dispatch on the
//! leading keyword token, hand off to a per-statement parser, then expect
//! either end-of-line or a `:` separator (or, after IF, a special case
//! where the body of THEN extends to end of line).
//!
//! Whitespace is permissive: BASIC v2 allowed extra spaces almost anywhere
//! and the tokenizer preserved them, so we just skip them between
//! syntactic elements.
//!
//! Operator precedence (lowest → highest):
//!   1. OR
//!   2. AND
//!   3. comparisons: =  <>  <  <=  >  >=
//!   4. + -
//!   5. * /
//!   6. unary -
//!   7. atoms (literal, variable, parenthesised expr)
//!
//! Power (`^`) and NOT are intentionally *not* implemented yet — they'll
//! land when the codegen needs them.

use crate::ast::{
    BinOp, DimSpec, Expr, FnName, Func1, Line, MemTransferOp, OnBranchKind, PrintItem, PrintStmt,
    ProcName, Program, ResumeTarget, ScreenRectOp, ScreenScrollOp, SkippedStatement, Statement,
    StrExpr, ThenBranch, VarKind, VarName,
};
use crate::prg;

#[derive(Debug)]
pub enum ParseError {
    UnsupportedToken {
        line: u16,
        byte: u8,
    },
    UnsupportedFeature {
        line: u16,
        what: &'static str,
    },
    Unsupported {
        line: u16,
        name: &'static str,
    },
    /// A direct-mode / interactive keyword that has no useful semantics
    /// in compiled code (RUN, LIST, NEW, CONT). The reason is shown
    /// alongside the keyword so the user understands why.
    RejectedKeyword {
        line: u16,
        keyword: &'static str,
        reason: &'static str,
    },
    ExpectedLineNumber {
        line: u16,
    },
    ExpectedVar {
        line: u16,
    },
    ExpectedKeyword {
        line: u16,
        what: &'static str,
    },
    ExpectedExpr {
        line: u16,
    },
    LineNumberOverflow {
        line: u16,
        value: u32,
    },
    BadStatementBoundary {
        line: u16,
        byte: u8,
    },
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::UnsupportedToken { line, byte } => {
                write!(f, "line {line}: token ${byte:02X} is not yet supported")
            }
            ParseError::UnsupportedFeature { line, what } => {
                write!(f, "line {line}: {what} is not yet supported")
            }
            ParseError::Unsupported { line, name } => {
                write!(f, "line {line}: {name} is not yet supported")
            }
            ParseError::RejectedKeyword {
                line,
                keyword,
                reason,
            } => {
                write!(
                    f,
                    "line {line}: {keyword} is not supported in compiled programs ({reason})"
                )
            }
            ParseError::ExpectedLineNumber { line } => {
                write!(f, "line {line}: expected a line number")
            }
            ParseError::ExpectedVar { line } => write!(f, "line {line}: expected a variable name"),
            ParseError::ExpectedKeyword { line, what } => {
                write!(f, "line {line}: expected {what}")
            }
            ParseError::ExpectedExpr { line } => write!(f, "line {line}: expected an expression"),
            ParseError::LineNumberOverflow { line, value } => write!(
                f,
                "line {line}: line number {value} exceeds BASIC v2 max of 63999"
            ),
            ParseError::BadStatementBoundary { line, byte } => write!(
                f,
                "line {line}: unexpected byte ${byte:02X} where ':' or end of line was expected"
            ),
        }
    }
}

impl std::error::Error for ParseError {}

// BASIC v2 token bytes we care about.
const TOK_END: u8 = 0x80;
const TOK_STOP: u8 = 0x90;
const TOK_FOR: u8 = 0x81;
const TOK_NEXT: u8 = 0x82;
const TOK_DATA: u8 = 0x83;
const TOK_DIM: u8 = 0x86;
const TOK_READ: u8 = 0x87;
const TOK_INPUT: u8 = 0x85;
const TOK_LET: u8 = 0x88;
const TOK_GOTO: u8 = 0x89;
const TOK_IF: u8 = 0x8B;
const TOK_RESTORE: u8 = 0x8C;
const TOK_GOSUB: u8 = 0x8D;
const TOK_RETURN: u8 = 0x8E;
const TOK_REM: u8 = 0x8F;
const TOK_ON: u8 = 0x91;
const TOK_PRINT: u8 = 0x99;
const TOK_GET: u8 = 0xA1;
const TOK_TO: u8 = 0xA4;
const TOK_THEN: u8 = 0xA7;
const TOK_STEP: u8 = 0xA9;
const TOK_PLUS: u8 = 0xAA;
const TOK_MINUS: u8 = 0xAB;
const TOK_MUL: u8 = 0xAC;
const TOK_DIV: u8 = 0xAD;
/// `^` — exponentiation. Highest-precedence numeric operator, binds
/// tighter than unary minus (`-2^2` parses as `-(2^2)` = -4).
const TOK_POW: u8 = 0xAE;
const TOK_AND: u8 = 0xAF;
const TOK_OR: u8 = 0xB0;
/// `NOT` — unary operator, precedence between comparisons and AND.
const TOK_NOT: u8 = 0xA8;
const TOK_GT: u8 = 0xB1;
const TOK_EQ: u8 = 0xB2;
const TOK_LT: u8 = 0xB3;

// Numeric one-arg functions. Each maps to a Func1 variant.
const TOK_SGN: u8 = 0xB4;
const TOK_INT: u8 = 0xB5;
const TOK_ABS: u8 = 0xB6;
const TOK_SQR: u8 = 0xBA;
const TOK_RND: u8 = 0xBB;
const TOK_LOG: u8 = 0xBC;
const TOK_EXP: u8 = 0xBD;
const TOK_COS: u8 = 0xBE;
const TOK_SIN: u8 = 0xBF;
const TOK_TAN: u8 = 0xC0;
const TOK_ATN: u8 = 0xC1;
const TOK_PEEK: u8 = 0xC2;
const TOK_FRE: u8 = 0xB8;
const TOK_POS: u8 = 0xB9;
const TOK_USR: u8 = 0xB7;
const TOK_SYS: u8 = 0x9E;
const TOK_POKE: u8 = 0x97;
const TOK_WAIT: u8 = 0x92;
const TOK_OPEN: u8 = 0x9F;
const TOK_CLOSE: u8 = 0xA0;
// Direct-mode / interactive commands that have no useful semantics in
// a compiled program. Rejected explicitly with a clear message rather
// than failing as "unsupported token".
const TOK_RUN: u8 = 0x8A;
const TOK_LIST: u8 = 0x9B;
const TOK_NEW: u8 = 0xA2;
const TOK_CONT: u8 = 0x9A;
const TOK_CLR: u8 = 0x9C;
/// `GO` — leading half of the alternative `GO TO` / `GO SUB` form.
/// Bastext (and the C64 ROM tokeniser when keyword recognition fails)
/// emits this as $CB followed by either the `TO` token or the ASCII
/// letters `SUB`. The parser folds both shapes into the regular GOTO
/// / GOSUB IR statements.
const TOK_GO: u8 = 0xCB;
const TOK_PRINT_FILE: u8 = 0x98;
const TOK_INPUT_FILE: u8 = 0x84;
const TOK_CMD: u8 = 0x9D;
const TOK_LOAD: u8 = 0x93;
const TOK_SAVE: u8 = 0x94;
const TOK_VERIFY: u8 = 0x95;
// GET# is TOK_GET (0xA1) immediately followed by ASCII '#' (0x23).
const TOK_CHR: u8 = 0xC7;
/// `TAB(` — only valid inside PRINT. The open-paren is part of the token.
const TOK_TAB: u8 = 0xA3;
/// `SPC(` — same shape as TAB.
const TOK_SPC: u8 = 0xA6;
/// `DEF` — paired with FN to declare a user function.
const TOK_DEF: u8 = 0x96;
/// `FN` — used both in declarations (`DEF FN F(...)`) and call sites
/// (`FN F(arg)`). The function name follows as ordinary letters.
const TOK_FN: u8 = 0xA5;
const TOK_STR: u8 = 0xC4;
const TOK_VAL: u8 = 0xC5;
const TOK_LEN: u8 = 0xC3;
const TOK_ASC: u8 = 0xC6;
const TOK_LEFT: u8 = 0xC8;
const TOK_RIGHT: u8 = 0xC9;
const TOK_MID: u8 = 0xCA;
const TOK_COMMA: u8 = b',';

// Extended BASIC tokens. They are stored as a two-byte
// sequence: ASCII/PETSCII `$64`, followed by the logical token id.
const TOK_TSB_PREFIX: u8 = 0x64;
const TSB_HIRES: u8 = 0x01;
const TSB_PLOT: u8 = 0x02;
const TSB_LINE: u8 = 0x03;
const TSB_BLOCK: u8 = 0x04;
const TSB_REC: u8 = 0x08;
const TSB_ROT: u8 = 0x09;
const TSB_DRAW: u8 = 0x0A;
const TSB_CHAR: u8 = 0x0B;
const TSB_FCHR: u8 = 0x05;
const TSB_FCOL: u8 = 0x06;
const TSB_FILL: u8 = 0x07;
const TSB_HI_COL: u8 = 0x0C;
const TSB_INV: u8 = 0x0D;
const TSB_FRAC: u8 = 0x0E;
const TSB_MOVE: u8 = 0x0F;
const TSB_PLACE: u8 = 0x10;
const TSB_UPB: u8 = 0x11;
const TSB_UPW: u8 = 0x12;
const TSB_LEFTW: u8 = 0x13;
const TSB_LEFTB: u8 = 0x14;
const TSB_DOWNB: u8 = 0x15;
const TSB_DOWNW: u8 = 0x16;
const TSB_RIGHTB: u8 = 0x17;
const TSB_RIGHTW: u8 = 0x18;
const TSB_MULTI: u8 = 0x19;
const TSB_COLOR: u8 = 0x1A;
const TSB_MMOB: u8 = 0x1B;
const TSB_BFLASH: u8 = 0x1C;
const TSB_MOB_SET: u8 = 0x1D;
const TSB_MUSIC: u8 = 0x1E;
const TSB_FLASH: u8 = 0x1F;
const TSB_REPEAT: u8 = 0x20;
const TSB_PLAY: u8 = 0x21;
const TSB_DO: u8 = 0x22;
const TSB_CENTER: u8 = 0x23;
const TSB_ENVELOPE: u8 = 0x24;
const TSB_CGOTO: u8 = 0x25;
const TSB_WAVE: u8 = 0x26;
const TSB_FETCH: u8 = 0x27;
const TSB_AT: u8 = 0x28;
const TSB_UNTIL: u8 = 0x29;
const TSB_USE: u8 = 0x2C;
const TSB_GLOBAL: u8 = 0x2E;
#[allow(dead_code)]
const TSB_RESET: u8 = 0x30;
const TSB_PROC: u8 = 0x31;
const TSB_CALL: u8 = 0x32;
const TSB_EXEC: u8 = 0x33;
const TSB_END_PROC: u8 = 0x34;
const TSB_EXIT: u8 = 0x35;
const TSB_END_LOOP: u8 = 0x36;
const TSB_ON_KEY: u8 = 0x37;
const TSB_DISABLE: u8 = 0x38;
const TSB_RESUME: u8 = 0x39;
const TSB_LOOP: u8 = 0x3A;
const TSB_DELAY: u8 = 0x3B;
const TSB_CLS: u8 = 0x3C;
#[allow(dead_code)]
const TSB_X_BANG: u8 = 0x3D;
#[allow(dead_code)]
const TSB_MAP: u8 = 0x3E;
#[allow(dead_code)]
const TSB_SECURE: u8 = 0x40;
const TSB_MOBCOL: u8 = 0x41;
const TSB_CIRCLE: u8 = 0x42;
const TSB_ON_ERROR: u8 = 0x43;
const TSB_NO_ERROR: u8 = 0x44;
const TSB_LOCAL: u8 = 0x45;
const TSB_RCOMP: u8 = 0x46;
const TSB_ELSE: u8 = 0x47;
const TSB_RETRACE: u8 = 0x48;
const TSB_TRACE: u8 = 0x49;
#[allow(dead_code)]
const TSB_DIR: u8 = 0x4A;
const TSB_PAGE: u8 = 0x4B;
#[allow(dead_code)]
const TSB_DUMP: u8 = 0x4C;
#[allow(dead_code)]
const TSB_FIND: u8 = 0x4D;
const TSB_OPTION: u8 = 0x4E;
#[allow(dead_code)]
const TSB_AUTO: u8 = 0x4F;
#[allow(dead_code)]
const TSB_OLD: u8 = 0x50;
const TSB_JOY: u8 = 0x51;
const TSB_MOD: u8 = 0x52;
const TSB_DIV: u8 = 0x53;
const TSB_D_BANG: u8 = 0x54;
const TSB_DUP: u8 = 0x55;
const TSB_INKEY: u8 = 0x56;
const TSB_INST: u8 = 0x57;
const TSB_TEST: u8 = 0x58;
const TSB_LIN: u8 = 0x59;
const TSB_EXOR: u8 = 0x5A;
const TSB_INSERT: u8 = 0x5B;
const TSB_POT: u8 = 0x5C;
const TSB_PENX: u8 = 0x5D;
const TSB_PENY: u8 = 0x5F;
const TSB_SOUND: u8 = 0x60;
const TSB_GRAPHICS: u8 = 0x61;
const TSB_DESIGN: u8 = 0x62;
const TSB_RLOCMOB: u8 = 0x63;
const TSB_CMOB: u8 = 0x64;
const TSB_BCKGNDS: u8 = 0x65;
const TSB_PAUSE: u8 = 0x66;
const TSB_NRM: u8 = 0x67;
const TSB_MOB: u8 = 0x68;
const TSB_OFF: u8 = 0x69;
const TSB_ANGL: u8 = 0x6A;
const TSB_ARC: u8 = 0x6B;
#[allow(dead_code)]
const TSB_COLD: u8 = 0x6C;
const TSB_SCRSV: u8 = 0x6D;
const TSB_SCRLD: u8 = 0x6E;
const TSB_TEXT: u8 = 0x6F;
const TSB_CSET: u8 = 0x70;
const TSB_VOL: u8 = 0x71;
const TSB_DISK: u8 = 0x72;
#[allow(dead_code)]
const TSB_HRDCPY: u8 = 0x73;
const TSB_KEY: u8 = 0x74;
const TSB_PAINT: u8 = 0x75;
const TSB_LOW_COL: u8 = 0x76;
const TSB_COPY: u8 = 0x77;
#[allow(dead_code)]
const TSB_MERGE: u8 = 0x78;
#[allow(dead_code)]
const TSB_RENUMBER: u8 = 0x79;
const TSB_MEM: u8 = 0x7A;
const TSB_DETECT: u8 = 0x7B;
const TSB_CHECK: u8 = 0x7C;
const TSB_DISPLAY: u8 = 0x7D;
const TSB_ERR: u8 = 0x7E;
#[allow(dead_code)]
const TSB_OUT: u8 = 0x7F;

fn func1_for_token(b: u8) -> Option<Func1> {
    Some(match b {
        TOK_SGN => Func1::Sgn,
        TOK_INT => Func1::Int,
        TOK_ABS => Func1::Abs,
        TOK_SQR => Func1::Sqr,
        TOK_RND => Func1::Rnd,
        TOK_LOG => Func1::Log,
        TOK_EXP => Func1::Exp,
        TOK_COS => Func1::Cos,
        TOK_SIN => Func1::Sin,
        TOK_TAN => Func1::Tan,
        TOK_ATN => Func1::Atn,
        _ => return None,
    })
}

fn is_statement_start_byte(b: u8) -> bool {
    b >= 0x80 || b == TOK_TSB_PREFIX
}

fn peek_tsb(p: &Cursor<'_>, token: u8) -> bool {
    p.peek() == Some(TOK_TSB_PREFIX) && p.peek_at(1).map(normalize_tsb_token) == Some(token)
}

fn consume_tsb(p: &mut Cursor<'_>, token: u8) -> bool {
    if peek_tsb(p, token) {
        p.advance(2);
        true
    } else {
        false
    }
}

pub fn program(prg: &prg::Program) -> Result<Program, ParseError> {
    program_with_options(prg, ParseOptions::default())
}

/// Options recognised by [`program_with_options`].
#[derive(Default, Clone)]
pub struct ParseOptions {
    /// Accept source typos that v2 would only catch at runtime —
    /// `GOT1200` (=> GOTO 1200), `CLOSE n, sa, dev` (extra args),
    /// etc. Off by default; opt in via the CLI or GUI when the user
    /// knows the offending line is dead code.
    pub lenient_syntax: bool,
    /// Base names (canonicalised upper-case, 1-2 chars) that REM hints
    /// have declared as integer. The parser rewrites Float `VarName`s
    /// whose base appears here to Integer kind, so the rest of the
    /// pipeline sees them as if the user had written the `%` suffix.
    pub int_hint_vars: std::collections::HashSet<String>,
}

pub fn program_with_options(prg: &prg::Program, opts: ParseOptions) -> Result<Program, ParseError> {
    let mut lines = Vec::with_capacity(prg.lines.len());
    let mut skipped = Vec::new();
    for raw in &prg.lines {
        lines.push(Line {
            number: raw.number,
            statements: line_body_with_options(raw.number, &raw.body, &opts, &mut skipped)?,
        });
    }
    Ok(Program { lines, skipped })
}

#[cfg(test)]
fn line_body(line_no: u16, body: &[u8]) -> Result<Vec<Statement>, ParseError> {
    line_body_with_options(line_no, body, &ParseOptions::default(), &mut Vec::new())
}

fn line_body_with_options(
    line_no: u16,
    body: &[u8],
    opts: &ParseOptions,
    skipped: &mut Vec<SkippedStatement>,
) -> Result<Vec<Statement>, ParseError> {
    let mut p = Cursor::new(body, line_no);
    p.lenient_syntax = opts.lenient_syntax;
    p.int_hint_vars = &opts.int_hint_vars;
    let mut out = Vec::new();
    loop {
        p.skip_spaces();
        // Skip empty statements (`:` separators with nothing between
        // them, or a bare `:` line). Common 80s idiom: `9050 :` as a
        // visual spacer between routines.
        while let Some(b':') = p.peek() {
            p.advance(1);
            p.skip_spaces();
        }
        // Stray control bytes at the start of a statement are ignored
        // by BASIC v2, so we skip them here too.
        while matches!(p.peek(), Some(b) if (0x01..0x20).contains(&b)) {
            p.advance(1);
        }
        p.skip_spaces();
        if p.eof() {
            break;
        }
        let saved = p.pos;
        let stmt = match statement(&mut p) {
            Ok(s) => s,
            Err(err) => {
                // A keyword from another Commodore BASIC isn't a syntax
                // error on the target — v2 only looks at those bytes if
                // it executes them, and listings guard them by machine
                // (`IF BV=67 THEN SPRDEF ...`). Compile it away and let
                // the caller report it. Malformed syntax still aborts
                // unless --lenient-syntax is on.
                let offending = match err {
                    ParseError::UnsupportedToken { byte, .. } => Some(byte),
                    _ => None,
                };
                if offending.is_none() && !p.lenient_syntax {
                    return Err(err);
                }
                // Universal lenient fallback: any statement that fails
                // to parse becomes a REM, with control flow targets
                // (line numbers) preserved. v2 ROM tolerance is loose
                // enough that tokenized listings can contain bytes the
                // strict parser rejects (missing `)`, control bytes
                // smuggled between keyword and argument, `GOTO` whose
                // tail isn't a digit, etc.). With --lenient-syntax on
                // the user has opted into "any unparseable statement
                // becomes a no-op, the rest of the program still
                // compiles."
                p.pos = saved;
                // Stopping at the next `:` inside a conditional would
                // re-parse the THEN body as top-level statements, which
                // then run unconditionally.
                let owns_line = statement_owns_rest_of_line(&p);
                skipped.push(SkippedStatement {
                    line: line_no,
                    token: offending.or_else(|| p.peek()).unwrap_or(0),
                    whole_conditional: owns_line,
                });
                while let Some(b) = p.peek() {
                    if b == b':' && !owns_line {
                        break;
                    }
                    p.advance(1);
                }
                Statement::Rem(Vec::new())
            }
        };
        let unreturning = matches!(
            &stmt,
            Statement::Sys { .. }
                | Statement::End
                | Statement::Stop
                | Statement::Run(_)
                | Statement::Goto(_)
        );
        out.push(stmt);
        p.skip_spaces();
        // Stray control bytes ($00-$1F) at end of line are common in
        // hand-edited tokenised sources and the v2 interpreter just
        // ignores them once a statement has finished. Drop runs of
        // them so the boundary check below doesn't reject the line.
        while matches!(p.peek(), Some(b) if (0x01..0x20).contains(&b)) {
            p.advance(1);
        }
        // Trailing decoration after a statement that never returns to
        // the BASIC dispatcher. The interpreter never reaches it
        // because control transfers away first, so swallow until the
        // next `:` / line end.
        if unreturning {
            while let Some(b) = p.peek() {
                if b == b':' {
                    break;
                }
                p.advance(1);
            }
        }
        match p.peek() {
            None => break,
            // `:` is the proper separator; `;` here is V2 ROM laxity
            // — outside of PRINT/INPUT it's silently treated as a
            // no-op separator and is common as a stray byte at the
            // end of legacy program lines.
            Some(b':') | Some(b';') => {
                p.advance(1);
            }
            // A token byte ($80+) directly after a statement that ended
            // on a numeric argument starts the next statement with no
            // `:` separator. Accept it so legacy tokenised listings parse
            // without modification.
            Some(b) if is_statement_start_byte(b) => { /* fall through, no advance */ }
            Some(_) => {
                // Trailing-junk recovery. Stray bytes after a valid
                // statement do not affect runtime execution once the
                // tokenized line terminates. Skip to the next colon or
                // end-of-line and continue, rather than
                // refusing to compile the whole program over a
                // single bad byte.
                while let Some(b) = p.peek() {
                    if b == b':' || b == b';' {
                        p.advance(1);
                        break;
                    }
                    p.advance(1);
                }
            }
        }
    }
    Ok(out)
}

/// Whether the statement starting at the cursor takes the rest of its
/// line as its body. Only the conditionals do — v2 has no way to put an
/// unconditional statement after `IF ... THEN` on the same line.
fn statement_owns_rest_of_line(p: &Cursor<'_>) -> bool {
    match p.peek() {
        Some(TOK_IF) => true,
        Some(TOK_TSB_PREFIX) => p
            .peek_at(1)
            .map(normalize_tsb_token)
            .is_some_and(|t| t == TSB_RCOMP),
        _ => false,
    }
}

fn statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    let b = p.peek().expect("caller guarantees non-empty");
    // A statement that starts with a bare `"` is malformed BASIC v2 —
    // it's not a known statement keyword, no implicit LET shape fits,
    // and quoted-string literals only live inside expression context.
    // Real listings use it as a "comment" idiom anyway: Miser's House
    // has a credit block (`1 " M.J. LANSING`) on lines that GOTO
    // skips at runtime, and SecretOfKublaih has stray `:"text"`
    // sequences. Treat the whole tail of the line as a REM so the
    // surrounding code still compiles. Anything that did GOTO/GOSUB
    // here would still have hit `?SYNTAX ERROR` in the interpreter.
    if b == b'"' {
        // Swallow the entire rest of the line — `:` inside a credit
        // line (`" AS OF 81 AUG 23 1:25 BS`) isn't a statement
        // separator, the whole tail is decorative.
        let mut bytes = Vec::new();
        while let Some(c) = p.peek() {
            bytes.push(c);
            p.advance(1);
        }
        return Ok(Statement::Rem(bytes));
    }
    // `@`-prefixed bitmap row, used as data following a
    // `DESIGN type, addr` setup. The tokeniser leaves `@` ($40)
    // as a literal byte at the start of the line; capture all
    // pattern characters until `:` or end of line and let the
    // post-parse design-group pass fold the row into the
    // preceding DESIGN's byte list.
    if b == b'@' {
        p.advance(1);
        let mut row = Vec::new();
        while let Some(c) = p.peek() {
            if c == b':' {
                break;
            }
            row.push(c);
            p.advance(1);
        }
        return Ok(Statement::DesignRow(row));
    }
    // High-Speed-Graphics extension statement: `\` ($5C) +
    // a command letter. The tokeniser leaves `$5C` as a literal
    // byte; `parse_hsg_statement` dispatches it.
    if b == 0x5C {
        return parse_hsg_statement(p);
    }
    // `.NAME` at the start of a statement is an omitted EXEC.
    // The leading `.` is part of the procedure name, so the call site
    // and `PROC .NAME` header parse to the same `ProcName`. When no
    // matching `PROC` exists the call lowers to a no-op (ir.rs) —
    // which also covers decorative labels used as GOTO targets.
    //
    // A leading `.` qualifies when followed by an alphabetic ASCII
    // char OR a token byte ($80+) — the latter because the BASIC v2
    // tokeniser substitutes any keyword found mid-identifier (so
    // `.DEFINE` arrives as `$2E $96 'I' 'N' 'E'`, `.READ TABLE` as
    // `$2E $87 ' ' 'T' 'A' 'B' 'L' 'E'`).
    if b == b'.'
        && p.peek_at(1)
            .is_some_and(|c| c.is_ascii_alphabetic() || c >= 0x80)
    {
        return Ok(Statement::ProcCall(proc_name(p)?));
    }
    match b {
        TOK_PRINT => {
            p.advance(1);
            p.skip_spaces();
            if p.peek() == Some(b'#') {
                p.advance(1);
                return print_file_stmt(p);
            }
            Ok(Statement::Print(print_stmt(p)?))
        }
        TOK_GOTO => {
            p.advance(1);
            p.skip_spaces();
            if p.eof() || p.peek() == Some(b':') {
                // A handful of legacy tokenized listings contain a
                // bare trailing GOTO where the original target was
                // lost. Let the line compile by treating that corrupt
                // tail as an empty statement.
                return Ok(Statement::Rem(Vec::new()));
            }
            let target = line_number(p)?;
            skip_line_target_label(p);
            Ok(Statement::Goto(target))
        }
        TOK_GOSUB => {
            p.advance(1);
            p.skip_spaces();
            let target = line_number(p)?;
            skip_line_target_label(p);
            Ok(Statement::GoSub(target))
        }
        TOK_GO => {
            // `GO TO` (CB A4) or `GO SUB` (CB then ASCII "SUB"). In
            // lenient mode, also accept `GOT<digits>` / `GOS<digits>`
            // typos — v2's tokeniser stored those as `GO` + raw text
            // rather than a longer GOTO/GOSUB match.
            p.advance(1);
            p.skip_spaces();
            match p.peek() {
                Some(TOK_TO) => {
                    p.advance(1);
                    p.skip_spaces();
                    if p.eof() || p.peek() == Some(b':') {
                        return Ok(Statement::Rem(Vec::new()));
                    }
                    Ok(Statement::Goto(line_number(p)?))
                }
                Some(b'S') if p.peek_at(1) == Some(b'U') && p.peek_at(2) == Some(b'B') => {
                    p.advance(3);
                    p.skip_spaces();
                    Ok(Statement::GoSub(line_number(p)?))
                }
                Some(b's') if p.peek_at(1) == Some(b'u') && p.peek_at(2) == Some(b'b') => {
                    p.advance(3);
                    p.skip_spaces();
                    Ok(Statement::GoSub(line_number(p)?))
                }
                // Typo recovery: BASIC v2's tokeniser stores `GOTxxx`
                // / `GOSxxx` (missing the second 'O' or the closing
                // 'B') as `GO` + raw text rather than the proper
                // GOTO/GOSUB tokens. Accept
                // the shape unconditionally rather than requiring the
                // lenient-syntax flag: the alternative is rejecting
                // working programs over a single missing keystroke.
                Some(c)
                    if (c == b'S' || c == b's')
                        && p.peek_at(1).is_some_and(|b| b.is_ascii_digit()) =>
                {
                    p.advance(1);
                    Ok(Statement::GoSub(line_number(p)?))
                }
                Some(c)
                    if (c == b'T' || c == b't')
                        && p.peek_at(1).is_some_and(|b| b.is_ascii_digit()) =>
                {
                    p.advance(1);
                    Ok(Statement::Goto(line_number(p)?))
                }
                // `GOT` / `GOS` truncated at end of statement with no
                // line number following. Treat the tail as a no-op REM.
                Some(c)
                    if (c == b'T' || c == b't' || c == b'S' || c == b's')
                        && matches!(p.peek_at(1), None | Some(b':') | Some(b';')) =>
                {
                    Ok(Statement::Rem(Vec::new()))
                }
                None | Some(b':') | Some(b';') => Ok(Statement::Rem(Vec::new())),
                _ => Err(ParseError::ExpectedKeyword {
                    line: p.line,
                    what: "TO or SUB after GO",
                }),
            }
        }
        TOK_RETURN => {
            p.advance(1);
            Ok(Statement::Return)
        }
        TOK_LET => {
            p.advance(1);
            p.skip_spaces();
            let_assign(p)
        }
        TOK_IF => {
            p.advance(1);
            if_stmt(p)
        }
        TOK_FOR => {
            p.advance(1);
            for_stmt(p)
        }
        TOK_NEXT => {
            p.advance(1);
            next_stmt(p)
        }
        TOK_REM => {
            p.advance(1);
            // The rest of the line is raw PETSCII. REM has no inner ':'
            // structure — `:` after REM is just text per BASIC v2.
            let rest = p.take_rest().to_vec();
            Ok(Statement::Rem(rest))
        }
        TOK_END => {
            p.advance(1);
            Ok(Statement::End)
        }
        TOK_STOP => {
            p.advance(1);
            Ok(Statement::Stop)
        }
        TOK_POKE => {
            p.advance(1);
            let addr = expression(p)?;
            p.skip_spaces();
            if p.peek() != Some(TOK_COMMA) {
                return Err(ParseError::ExpectedKeyword {
                    line: p.line,
                    what: "',' in POKE",
                });
            }
            p.advance(1);
            let value = expression(p)?;
            Ok(Statement::Poke { addr, value })
        }
        TOK_SYS => {
            p.advance(1);
            let addr = expression(p)?;
            // Some BASIC dialects accept up to four
            // trailing comma-separated args after the address that
            // pre-load A / X / Y / SR before the JSR. Pure v2 syntax-
            // errors here, but extension-authored listings may use it.
            let mut regs = Vec::new();
            p.skip_spaces();
            while p.peek() == Some(TOK_COMMA) && regs.len() < 4 {
                p.advance(1);
                p.skip_spaces();
                regs.push(expression(p)?);
                p.skip_spaces();
            }
            // BASIC v2 SYS-with-parameters form (`SYS49152"text",8`):
            // any remaining tokens up to colon/end-of-statement become
            // the raw param byte string. Captured verbatim so codegen
            // can stage them at TXTPTR and let the ML target call ROM
            // parsers (CHRGOT, FRMEVL, ...) to consume them.
            let mut params = Vec::new();
            while let Some(b) = p.peek() {
                if b == b':' || b == b';' {
                    break;
                }
                params.push(b);
                p.advance(1);
            }
            // Trim trailing whitespace so an interpreter-style listing
            // with a stray space before the colon doesn't carry it.
            while matches!(params.last(), Some(b' ')) {
                params.pop();
            }
            Ok(Statement::Sys { addr, regs, params })
        }
        TOK_OPEN => {
            p.advance(1);
            open_stmt(p)
        }
        TOK_CLOSE => {
            p.advance(1);
            let file_num = expression(p)?;
            // BASIC v2 silently ignores any trailing OPEN-style
            // `, sa [, dev]` after CLOSE; the runtime simply uses
            // the file-table entry for `file_num`. Always swallow so
            // legacy listings parse without
            // requiring the lenient flag.
            p.skip_spaces();
            while p.peek() == Some(TOK_COMMA) {
                p.advance(1);
                let _ = expression(p)?;
                p.skip_spaces();
            }
            Ok(Statement::Close { file_num })
        }
        TOK_PRINT_FILE => {
            p.advance(1);
            print_file_stmt(p)
        }
        TOK_INPUT_FILE => {
            p.advance(1);
            input_file_stmt(p)
        }
        TOK_CMD => {
            p.advance(1);
            cmd_stmt(p)
        }
        TOK_LOAD => {
            p.advance(1);
            file_op_stmt(p, FileOp::Load)
        }
        TOK_SAVE => {
            p.advance(1);
            file_op_stmt(p, FileOp::Save)
        }
        TOK_VERIFY => {
            p.advance(1);
            file_op_stmt(p, FileOp::Verify)
        }
        TOK_WAIT => {
            p.advance(1);
            let addr = expression(p)?;
            p.skip_spaces();
            if p.peek() != Some(TOK_COMMA) {
                return Err(ParseError::ExpectedKeyword {
                    line: p.line,
                    what: "',' in WAIT",
                });
            }
            p.advance(1);
            let mask = expression(p)?;
            p.skip_spaces();
            let eor = if p.peek() == Some(TOK_COMMA) {
                p.advance(1);
                Some(expression(p)?)
            } else {
                None
            };
            Ok(Statement::Wait { addr, mask, eor })
        }
        TOK_DATA => {
            p.advance(1);
            data_stmt(p)
        }
        TOK_READ => {
            p.advance(1);
            read_stmt(p)
        }
        TOK_RESTORE => {
            p.advance(1);
            Ok(Statement::Restore)
        }
        TOK_INPUT => {
            p.advance(1);
            input_stmt(p)
        }
        TOK_GET => {
            p.advance(1);
            // GET# n, var — file form. The '#' immediately follows
            // the GET token (tokenization keeps GET as $A1 even when paired
            // with #, so we look for the literal '#').
            if p.peek() == Some(b'#') {
                p.advance(1);
                let file_num = expression(p)?;
                p.skip_spaces();
                if p.peek() != Some(TOK_COMMA) {
                    return Err(ParseError::ExpectedKeyword {
                        line: p.line,
                        what: "',' after GET# file number",
                    });
                }
                p.advance(1);
                let mut vars = vec![var_name(p)?];
                // `GET#n, A$, B$, C$` — one byte per var.
                loop {
                    p.skip_spaces();
                    if p.peek() != Some(TOK_COMMA) {
                        break;
                    }
                    p.advance(1);
                    vars.push(var_name(p)?);
                }
                return Ok(Statement::GetFile { file_num, vars });
            }
            let var = var_name(p)?;
            // GET A$ is a string assignment whose right-hand side is the
            // GetKey "expression" — folds into the regular LetStr path.
            if var.kind == VarKind::String {
                Ok(Statement::LetStr {
                    var,
                    value: StrExpr::GetKey,
                })
            } else {
                Ok(Statement::Get { var })
            }
        }
        TOK_DIM => {
            p.advance(1);
            dim_stmt(p)
        }
        TOK_ON => {
            p.advance(1);
            on_stmt(p)
        }
        TOK_DEF => {
            // Some programs use `DEF` as a decorative label-comment
            // (e.g. `2220 DEFINITIONEN`). In strict mode that's a
            // parse error; in lenient mode treat the whole rest of
            // the line as REM so the program still compiles.
            let saved = p.pos;
            p.advance(1);
            match def_fn_stmt(p) {
                Ok(stmt) => Ok(stmt),
                Err(err) => {
                    if p.lenient_syntax {
                        p.pos = saved;
                        while let Some(b) = p.peek() {
                            if b == b':' {
                                break;
                            }
                            p.advance(1);
                        }
                        Ok(Statement::Rem(Vec::new()))
                    } else {
                        Err(err)
                    }
                }
            }
        }
        TOK_CLR => {
            p.advance(1);
            Ok(Statement::Clr)
        }
        TOK_RUN => {
            p.advance(1);
            // `RUN` alone restarts from the first line; `RUN <line>`
            // restarts and jumps to <line> (a classic V2 idiom for
            // game/menu loops). Both share the state-reset path.
            p.skip_spaces();
            match p.peek() {
                None | Some(b':') => Ok(Statement::Run(None)),
                Some(b) if b.is_ascii_digit() => {
                    let line = line_number(p)?;
                    Ok(Statement::Run(Some(line)))
                }
                _ => Err(ParseError::ExpectedKeyword {
                    line: p.line,
                    what: "line number or end of statement after RUN",
                }),
            }
        }
        TOK_LIST => {
            // No source listing exists at runtime in compiled mode.
            // Swallow the rest as a REM-style comment so it parses.
            p.advance(1);
            let rest = p.take_until_statement_end().to_vec();
            Ok(Statement::Rem(rest))
        }
        TOK_NEW => {
            // Same reasoning as `LIST`: `NEW` would clear the live
            // BASIC program but is a no-op in compiled output. Accept
            // and discard rather than reject.
            p.advance(1);
            Ok(Statement::Rem(Vec::new()))
        }
        TOK_CONT => Err(ParseError::RejectedKeyword {
            line: p.line,
            keyword: "CONT",
            reason: "compiled programs cannot be resumed from STOP; use GOTO/IF for control flow",
        }),
        TOK_TSB_PREFIX => tsb_statement(p),
        b if b.is_ascii_alphabetic() => {
            if consume_ascii_stmt_word(p, b"FCHR") {
                return tsb_screen_rect_statement(p, ScreenRectOp::Fchr);
            }
            if consume_ascii_stmt_word(p, b"FCOL") {
                return tsb_screen_rect_statement(p, ScreenRectOp::Fcol);
            }
            if consume_ascii_stmt_word(p, b"FILL") {
                return tsb_screen_rect_statement(p, ScreenRectOp::Fill);
            }
            if consume_ascii_stmt_word(p, b"INV") {
                return tsb_screen_rect_statement(p, ScreenRectOp::Inv);
            }
            if consume_ascii_stmt_word(p, b"MOVE") {
                return tsb_screen_move_statement(p);
            }
            if let Some(op) = consume_ascii_screen_scroll_word(p) {
                return tsb_screen_scroll_statement(p, op);
            }
            if consume_ascii_stmt_word(p, b"CLS") {
                return Ok(cls_statement());
            }
            if consume_ascii_stmt_word(p, b"DIV") {
                return Ok(cls_statement());
            }
            if consume_ascii_color_command(p) {
                return tsb_color_statement(p);
            }
            if consume_ascii_stmt_word(p, b"CENTER") || consume_ascii_stmt_word(p, b"CENTRE") {
                return tsb_center_statement(p);
            }
            if consume_ascii_stmt_word(p, b"MOBCOL") {
                return tsb_mobcol_statement(p);
            }
            if consume_ascii_d_bang(p) {
                return tsb_d_bang_statement(p);
            }
            if consume_ascii_stmt_word(p, b"CMOB") {
                return tsb_cmob_statement(p);
            }
            if consume_ascii_stmt_word(p, b"BCKGNDS") {
                return tsb_bckgnds_statement(p);
            }
            if consume_ascii_stmt_word(p, b"NRM") {
                return Ok(Statement::Nrm);
            }
            if consume_ascii_stmt_word(p, b"CSET") {
                return Ok(Statement::Cset {
                    mode: expression(p)?,
                });
            }
            if consume_ascii_stmt_word(p, b"PAUSE") {
                return tsb_pause_statement(p);
            }
            if consume_ascii_stmt_word(p, b"PAGE")
                || consume_ascii_stmt_word(p, b"DELAY")
                || consume_ascii_stmt_word(p, b"OPTION")
                || consume_ascii_stmt_word(p, b"TRACE")
                || consume_ascii_stmt_word(p, b"RETRACE")
            {
                return Ok(tsb_noop_statement(p));
            }
            if consume_ascii_stmt_word(p, b"KEYGET") || consume_ascii_two_word(p, b"KEY", b"GET") {
                return tsb_keyget_statement(p);
            }
            if consume_ascii_stmt_word(p, b"DISK") {
                return tsb_disk_statement(p);
            }
            if consume_ascii_stmt_word(p, b"SOUND") {
                return tsb_sound_statement(p);
            }
            if consume_ascii_stmt_word(p, b"ENVELOPE") {
                return tsb_envelope_statement(p);
            }
            if consume_ascii_stmt_word(p, b"WAVE") {
                return tsb_wave_statement(p);
            }
            // `MUSIC` can appear as decorative label text on
            // otherwise-unreached lines. Save the cursor, try the
            // parse, and roll back to a REM in lenient mode if the
            // rest of the line is not `MUSIC <string-expr>, ...`.
            if peek_ascii_stmt_word(p, b"MUSIC") {
                let saved = p.pos;
                p.advance(5);
                match tsb_music_statement(p) {
                    Ok(stmt) => return Ok(stmt),
                    Err(err) => {
                        if p.lenient_syntax {
                            p.pos = saved;
                            while let Some(b) = p.peek() {
                                if b == b':' || b == b';' {
                                    break;
                                }
                                p.advance(1);
                            }
                            return Ok(Statement::Rem(Vec::new()));
                        }
                        return Err(err);
                    }
                }
            }
            if consume_ascii_stmt_word(p, b"PLAY") {
                return tsb_play_statement(p);
            }
            if consume_ascii_stmt_word(p, b"BFLASH") {
                return tsb_flash_statement(p, true);
            }
            if consume_ascii_stmt_word(p, b"FLASH") {
                return tsb_flash_statement(p, false);
            }
            if consume_ascii_stmt_word(p, b"FETCH") {
                return tsb_fetch_statement(p);
            }
            if consume_ascii_stmt_word(p, b"DISPLAY") {
                return tsb_display_statement(p);
            }
            if consume_ascii_stmt_word(p, b"KEY") {
                return tsb_key_statement(p);
            }
            if consume_ascii_stmt_word(p, b"MULTI") {
                return Ok(Statement::Multi {
                    enabled: parse_tsb_on_off(p)?,
                });
            }
            if consume_ascii_two_word(p, b"HI", b"COL") {
                return Ok(Statement::HiCol);
            }
            if consume_ascii_two_word(p, b"LOW", b"COL") {
                return tsb_low_col_statement(p);
            }
            if consume_ascii_stmt_word(p, b"MMOB") {
                return tsb_mmob_statement(p);
            }
            if consume_ascii_stmt_word(p, b"RLOCMOB") {
                return tsb_rlocmob_statement(p);
            }
            if consume_ascii_stmt_word(p, b"DETECT") {
                return tsb_detect_statement(p);
            }
            if consume_ascii_stmt_word(p, b"MOB") {
                return tsb_mob_statement(p);
            }
            if consume_ascii_stmt_word(p, b"VOL") {
                return tsb_vol_statement(p);
            }
            // AT/LIN/INST clash with user-defined arrays after BASIC
            // v2's two-char identifier truncation. Try the statement
            // form first, but backtrack to the implicit-LET
            // fall-through if the inner shape doesn't match.
            if peek_ascii_stmt_word(p, b"AT") {
                let saved = p.pos;
                p.advance(2);
                match tsb_at_statement(p) {
                    Ok(stmt) => return Ok(stmt),
                    Err(_) => p.pos = saved,
                }
            }
            if consume_ascii_stmt_word(p, b"PLACE") {
                return tsb_place_statement(p);
            }
            if consume_ascii_stmt_word(p, b"INSERT") {
                return tsb_insert_statement(p);
            }
            if peek_ascii_stmt_word(p, b"LIN") {
                let saved = p.pos;
                p.advance(3);
                match tsb_lin_statement(p) {
                    Ok(stmt) => return Ok(stmt),
                    Err(_) => p.pos = saved,
                }
            }
            if peek_ascii_stmt_word(p, b"INST") {
                let saved = p.pos;
                p.advance(4);
                match tsb_inst_statement(p) {
                    Ok(stmt) => return Ok(stmt),
                    Err(_) => p.pos = saved,
                }
            }
            if consume_ascii_stmt_word(p, b"LOCAL") {
                return tsb_local_statement(p);
            }
            if consume_ascii_stmt_word(p, b"GLOBAL") {
                return tsb_global_statement(p);
            }
            if consume_ascii_stmt_word(p, b"HIRES") {
                // Bare ASCII "HIRES" — ink/paper variant only matches
                // through the tokenised TSB_HIRES path with explicit
                // args. ASCII source uses the no-arg default.
                return Ok(Statement::Hires {
                    ink: None,
                    paper: None,
                });
            }
            // BORDER can arrive as `B` + TOK_OR + `DER` because BASIC
            // v2 tokenizes the embedded OR greedily. Accept both forms.
            if consume_ascii_stmt_word(p, b"BORDER")
                || (p.peek() == Some(b'B')
                    && p.peek_at(1) == Some(TOK_OR)
                    && p.peek_at(2) == Some(b'D')
                    && p.peek_at(3) == Some(b'E')
                    && p.peek_at(4) == Some(b'R')
                    && !p
                        .peek_at(5)
                        .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'$' | b'%'))
                    && {
                        p.advance(5);
                        true
                    })
            {
                p.skip_spaces();
                if at_statement_tail(p) {
                    // Bare `BORDER` — "reset border" no-op
                    // in our build (we don't track a saved-border
                    // state). Emit as REM so following statements
                    // still parse.
                    return Ok(Statement::Rem(Vec::new()));
                }
                return Ok(Statement::Border {
                    color: expression(p)?,
                });
            }
            if consume_ascii_stmt_word(p, b"LINE") {
                return tsb_line_statement(p);
            }
            if peek_ascii_stmt_word(p, b"DRAW") {
                let saved = p.pos;
                p.advance(4);
                match tsb_draw_statement(p) {
                    Ok(stmt) => return Ok(stmt),
                    Err(err) => {
                        if p.lenient_syntax {
                            // BASIC v2 programs sometimes use `DRAW` as
                            // a bare label or comment marker — e.g.
                            // `:DRAW GRID` to mark the start of a
                            // grid-drawing block. The ASCII match
                            // engaged the handler, which then
                            // wants a comma'd numeric arg list and
                            // bails. Roll back and treat the rest of
                            // the statement as decoration so the line
                            // (and the program) still compile.
                            p.pos = saved;
                            while let Some(b) = p.peek() {
                                if b == b':' || b == b';' {
                                    break;
                                }
                                p.advance(1);
                            }
                            return Ok(Statement::Rem(Vec::new()));
                        }
                        return Err(err);
                    }
                }
            }
            if consume_ascii_stmt_word(p, b"PLOT") {
                return tsb_plot_statement(p);
            }
            if consume_ascii_stmt_word(p, b"REC") {
                return tsb_rec_statement(p);
            }
            if consume_ascii_stmt_word(p, b"BLOCK") {
                return tsb_block_statement(p);
            }
            if consume_ascii_stmt_word(p, b"CIRCLE") {
                return tsb_circle_statement(p);
            }
            if consume_ascii_stmt_word(p, b"ARC") {
                return tsb_arc_statement(p);
            }
            if consume_ascii_stmt_word(p, b"DUP") {
                return tsb_dup_statement(p);
            }
            if consume_ascii_stmt_word(p, b"COPY") {
                return tsb_copy_statement(p);
            }
            if consume_ascii_stmt_word(p, b"SCRSV") {
                return tsb_scr_statement(p, true);
            }
            if consume_ascii_stmt_word(p, b"SCRLD") {
                return tsb_scr_statement(p, false);
            }
            if consume_ascii_stmt_word(p, b"MEMSAVE") {
                return tsb_mem_transfer_or_copy_statement(p, MemTransferOp::Save);
            }
            if consume_ascii_stmt_word(p, b"MEMLOAD") {
                return tsb_mem_transfer_or_copy_statement(p, MemTransferOp::Load);
            }
            if consume_ascii_stmt_word(p, b"MEMREAD") {
                return tsb_mem_transfer_or_copy_statement(p, MemTransferOp::Read);
            }
            if consume_ascii_stmt_word(p, b"MEMCLR") {
                return tsb_mem_clr_statement(p);
            }
            if consume_ascii_stmt_word(p, b"MEMRESTORE") {
                return tsb_mem_restore_statement(p);
            }
            if consume_ascii_stmt_word(p, b"MEMDEF") {
                return tsb_mem_def_statement(p);
            }
            if consume_ascii_stmt_word(p, b"MEMCONT") {
                return tsb_mem_cont_statement(p);
            }
            if consume_ascii_stmt_word(p, b"MEMOR") {
                return tsb_mem_c64_statement(p);
            }
            if consume_ascii_stmt_word(p, b"MEMPOS") {
                return tsb_mem_reu_pos_statement(p);
            }
            if consume_ascii_stmt_word(p, b"MEMLEN") {
                return tsb_mem_len_statement(p);
            }
            if consume_ascii_stmt_word(p, b"MEM") {
                return tsb_mem_statement(p);
            }
            if consume_ascii_stmt_word(p, b"DESIGN") {
                return tsb_design_statement(p);
            }
            if consume_ascii_stmt_word(p, b"RESET") {
                p.skip_spaces();
                let line = line_number(p)?;
                return Ok(Statement::Reset { line });
            }
            // CHAR followed by `(` is a v2 array reference, not the
            // `CHAR x, y` glyph-POKE statement. Skip the statement
            // match in that shape. For everything else, try the parse
            // and roll back to a REM on failure in lenient mode.
            if peek_ascii_stmt_word(p, b"CHAR") && p.bytes.get(p.pos + 4) != Some(&b'(') {
                let saved = p.pos;
                p.advance(4);
                match tsb_char_statement(p) {
                    Ok(stmt) => return Ok(stmt),
                    Err(err) => {
                        if p.lenient_syntax {
                            p.pos = saved;
                            while let Some(b) = p.peek() {
                                if b == b':' || b == b';' {
                                    break;
                                }
                                p.advance(1);
                            }
                            return Ok(Statement::Rem(Vec::new()));
                        }
                        return Err(err);
                    }
                }
            }
            if consume_ascii_stmt_word(p, b"TEXT") {
                return tsb_text_statement(p);
            }
            if consume_ascii_stmt_word(p, b"PAINT") {
                return tsb_paint_statement(p);
            }
            if consume_ascii_stmt_word(p, b"ROT") {
                return tsb_rot_statement(p);
            }
            if consume_ascii_stmt_word(p, b"ANGL") {
                return tsb_angl_statement(p);
            }
            if let Some(name) = consume_ascii_unsupported_tsb_statement(p) {
                // File-system / debugging commands that are safe to
                // treat as no-ops in compiled programs — the surrounding
                // code keeps running and the user sees a missing
                // operation but no compile error.
                const NOOP_OK: &[&str] = &[
                    "DIR", "DUMP", "MAP", "COLD", "OUT", "HRDCPY", "MERGE", "RENUMBER",
                ];
                if NOOP_OK.contains(&name) {
                    let mut bytes = Vec::new();
                    while let Some(c) = p.peek() {
                        if c == b':' {
                            break;
                        }
                        bytes.push(c);
                        p.advance(1);
                    }
                    return Ok(Statement::Rem(bytes));
                }
                return Err(ParseError::Unsupported { line: p.line, name });
            }
            // allows `EXEC` to be omitted for procedure calls. If
            // the identifier is not followed by an assignment/index,
            // keep it as a pending procedure name and let lowering
            // resolve it against collected PROC labels.
            if bare_identifier_statement(p) {
                let name = proc_name(p)?;
                Ok(Statement::ProcCall(name))
            } else {
                // Implicit LET: `A=5` is the same as `LET A=5` in BASIC v2.
                let_assign(p)
            }
        }
        // Statement-start bytes we don't recognise as keywords.
        // Save-disk noise can leave stray ASCII where BASIC v2 would
        // have stored raw text. Treat the byte and the rest of
        // the statement) as a no-op REM rather than failing the
        // whole compile.
        other if !is_known_keyword_byte(other) => {
            while let Some(b) = p.peek() {
                if b == b':' || b == b';' {
                    break;
                }
                p.advance(1);
            }
            Ok(Statement::Rem(Vec::new()))
        }
        other => Err(ParseError::UnsupportedToken {
            line: p.line,
            byte: other,
        }),
    }
}

/// True iff `b` is a byte that *could* legitimately begin a BASIC v2
/// statement once a higher-level dispatcher checked it. Used by the
/// fall-through arm in `statement()` to decide whether an unknown
/// byte is corrupt source (skip) or a token we forgot to wire up
/// (surface the error).
fn is_known_keyword_byte(b: u8) -> bool {
    // Operator-and-clause tokens ($A4..$B3) are never valid as the
    // *first* byte of a statement — they're TO, FN, SPC(, THEN, NOT,
    // STEP, +, -, *, /, ^, AND, OR, >, =, <. BASIC v2 stores them in
    // tokenised form even when they appear in decorative lines that
    // the program never reaches. Treat them as REM-style decoration.
    if (0xA4..=0xB3).contains(&b) {
        return false;
    }
    // Function tokens ($B4..$CA: SGN, INT, ABS, USR, FRE, POS, SQR,
    // RND, LOG, EXP, COS, SIN, TAN, ATN, PEEK, LEN, STR$, VAL, ASC,
    // CHR$, LEFT$, RIGHT$, MID$) are values, not statements. If a
    // line starts with one, treat it as a syntax-level typo or a
    // discarded value expression.
    // the program never actually evaluates. Treat them as REM fall-
    // through too so the rest of the program compiles. `$CB` (GO)
    // stays a keyword: it's the prefix of GO TO / GO SUB.
    if (0xB4..=0xCA).contains(&b) {
        return false;
    }
    // Any other tokenised keyword ($80..$CB excludes ASCII).
    if b >= 0x80 {
        return true;
    }
    // Letters can start a LET-implicit-target or an identifier.
    if b.is_ascii_alphabetic() {
        return true;
    }
    // The `?` shorthand for PRINT, the `@`-prefixed DESIGN row,
    // and the `\` HSG escape are all dispatched specifically.
    matches!(b, b'?' | b'@' | b'\\')
}

fn tsb_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    debug_assert_eq!(p.peek(), Some(TOK_TSB_PREFIX));
    p.advance(1);
    let Some(token) = p.peek() else {
        return Err(ParseError::UnsupportedToken {
            line: p.line,
            byte: TOK_TSB_PREFIX,
        });
    };
    p.advance(1);
    match normalize_tsb_token(token) {
        TSB_HIRES => {
            // `HIRES [ink, paper [, bg]]`. ink/paper seed the
            // bitmap colour-pair byte at $C000+ (high nibble = ink,
            // low nibble = paper). bg is accepted for source
            // compatibility; the current renderer does not model it
            // as a third MULTI colour. Without args, codegen uses the
            // light-blue/white default.
            p.skip_spaces();
            let mut ink = None;
            let mut paper = None;
            if !at_statement_tail(p) {
                ink = Some(expression(p)?);
                p.skip_spaces();
                if p.peek() == Some(TOK_COMMA) {
                    p.advance(1);
                    paper = Some(expression(p)?);
                    p.skip_spaces();
                    if p.peek() == Some(TOK_COMMA) {
                        p.advance(1);
                        let _bg = expression(p)?;
                    }
                }
            }
            Ok(Statement::Hires { ink, paper })
        }
        TSB_LINE => tsb_line_statement(p),
        TSB_DRAW => tsb_draw_statement(p),
        TSB_PLOT => tsb_plot_statement(p),
        TSB_REC => tsb_rec_statement(p),
        TSB_BLOCK => tsb_block_statement(p),
        TSB_CIRCLE => tsb_circle_statement(p),
        TSB_ARC => tsb_arc_statement(p),
        TSB_MOD => tsb_mod_statement(p),
        TSB_DUP => tsb_dup_statement(p),
        TSB_CHAR => tsb_char_statement(p),
        TSB_TEXT => tsb_text_statement(p),
        TSB_PAINT => tsb_paint_statement(p),
        TSB_ROT => tsb_rot_statement(p),
        TSB_ANGL => tsb_angl_statement(p),
        TSB_FCHR => tsb_screen_rect_statement(p, ScreenRectOp::Fchr),
        TSB_FCOL => tsb_screen_rect_statement(p, ScreenRectOp::Fcol),
        TSB_FILL => tsb_screen_rect_statement(p, ScreenRectOp::Fill),
        TSB_HI_COL => Ok(Statement::HiCol),
        TSB_INV => tsb_screen_rect_statement(p, ScreenRectOp::Inv),
        TSB_MOVE => tsb_screen_move_statement(p),
        TSB_UPB => tsb_screen_scroll_statement(p, ScreenScrollOp::UpBlank),
        TSB_UPW => tsb_screen_scroll_statement(p, ScreenScrollOp::UpWrap),
        TSB_LEFTW => tsb_screen_scroll_statement(p, ScreenScrollOp::LeftWrap),
        TSB_LEFTB => tsb_screen_scroll_statement(p, ScreenScrollOp::LeftBlank),
        TSB_DOWNB => tsb_screen_scroll_statement(p, ScreenScrollOp::DownBlank),
        TSB_DOWNW => tsb_screen_scroll_statement(p, ScreenScrollOp::DownWrap),
        TSB_RIGHTB => tsb_screen_scroll_statement(p, ScreenScrollOp::RightBlank),
        TSB_RIGHTW => tsb_screen_scroll_statement(p, ScreenScrollOp::RightWrap),
        TSB_MULTI => tsb_multi_statement(p),
        TSB_COLOR => tsb_color_statement(p),
        TSB_MMOB => tsb_mmob_statement(p),
        TSB_MOB_SET => tsb_mob_set_statement(p),
        TSB_REPEAT => Ok(Statement::Repeat),
        TSB_DO => tsb_do_statement(p),
        TSB_CENTER => tsb_center_statement(p),
        TSB_ENVELOPE => tsb_envelope_statement(p),
        TSB_CGOTO => Ok(Statement::ComputedGoto {
            target: expression(p)?,
        }),
        TSB_WAVE => tsb_wave_statement(p),
        TSB_UNTIL => Ok(Statement::Until {
            cond: parse_truthy_expression(p)?,
        }),
        TSB_PROC => Ok(Statement::ProcDef(proc_name(p)?)),
        TSB_CALL => tsb_call_statement(p),
        TSB_EXEC => Ok(Statement::ProcCall(proc_name(p)?)),
        TSB_END_PROC => Ok(Statement::EndProc),
        TSB_LOCAL => tsb_local_statement(p),
        TSB_GLOBAL => tsb_global_statement(p),
        TSB_EXIT => tsb_exit_statement(p),
        TSB_END_LOOP => Ok(Statement::EndLoop),
        TSB_ON_KEY => {
            let keys = string_expression(p)?;
            p.skip_spaces();
            let target = match p.peek() {
                Some(TOK_GOTO) => {
                    p.advance(1);
                    p.skip_spaces();
                    Some(crate::ast::OnKeyAction::Goto(line_number(p)?))
                }
                Some(TOK_GOSUB) => {
                    p.advance(1);
                    p.skip_spaces();
                    Some(crate::ast::OnKeyAction::GoSub(line_number(p)?))
                }
                _ => None,
            };
            Ok(Statement::OnKey { keys, target })
        }
        TSB_DISABLE => Ok(Statement::Disable),
        TSB_RESUME => tsb_resume_statement(p),
        TSB_ON_ERROR => tsb_on_error_statement(p),
        TSB_NO_ERROR => Ok(Statement::OnError { target: None }),
        TSB_ERR => {
            // Tokenized "ERROR" can arrive as ERR + OR. Consume the
            // OR so what remains is the error-code expression.
            if p.peek() == Some(TOK_OR) {
                p.advance(1);
            }
            tsb_error_raise_statement(p)
        }
        TSB_LOOP => Ok(Statement::Loop),
        TSB_CLS => Ok(cls_statement()),
        TSB_MOBCOL => tsb_mobcol_statement(p),
        TSB_RCOMP => rcomp_stmt(p),
        TSB_ELSE => Ok(Statement::Else),
        TSB_DIV => Ok(cls_statement()),
        TSB_D_BANG => tsb_d_bang_statement(p),
        TSB_DELAY | TSB_PAGE | TSB_OPTION | TSB_TRACE | TSB_RETRACE => Ok(tsb_noop_statement(p)),
        TSB_SOUND => tsb_sound_statement(p),
        TSB_BFLASH => tsb_flash_statement(p, true),
        TSB_MUSIC => tsb_music_statement(p),
        TSB_FLASH => tsb_flash_statement(p, false),
        TSB_PLAY => tsb_play_statement(p),
        TSB_FETCH => tsb_fetch_statement(p),
        TSB_DESIGN => tsb_design_statement(p),
        // `USE` as statement — three forms (drive switch,
        // formatted PRINT, file open). For now, swallow the rest
        // of the statement and treat as REM so the surrounding
        // program compiles. The drive-switch path is a no-op
        // anyway in our build (we always target drive 8); the
        // formatted-PRINT and file-open variants would need real
        // codegen. Programs that just `USE 0+I` to switch drives
        // run unchanged; ones that depend on USE for output get
        // visible behaviour reduced to "no PRINT happens here".
        TSB_USE => tsb_use_statement(p),
        TSB_RESET => {
            p.skip_spaces();
            let line = line_number(p)?;
            Ok(Statement::Reset { line })
        }
        TSB_SCRSV => tsb_scr_statement(p, true),
        TSB_SCRLD => tsb_scr_statement(p, false),
        TSB_COPY => tsb_copy_statement(p),
        TSB_MEM => tsb_mem_statement(p),
        TSB_DISPLAY => tsb_display_statement(p),
        TSB_AT => tsb_at_statement(p),
        TSB_PLACE => tsb_place_statement(p),
        TSB_INSERT => tsb_insert_statement(p),
        TSB_LIN => tsb_lin_statement(p),
        TSB_INST => tsb_inst_statement(p),
        TSB_RLOCMOB => tsb_rlocmob_statement(p),
        TSB_CMOB => tsb_cmob_statement(p),
        TSB_BCKGNDS => tsb_bckgnds_statement(p),
        TSB_NRM => Ok(Statement::Nrm),
        TSB_CSET => Ok(Statement::Cset {
            mode: expression(p)?,
        }),
        TSB_PAUSE => tsb_pause_statement(p),
        TSB_MOB => tsb_mob_statement(p),
        TSB_VOL => tsb_vol_statement(p),
        TSB_KEY => tsb_key_statement(p),
        TSB_DISK => tsb_disk_statement(p),
        TSB_LOW_COL => tsb_low_col_statement(p),
        TSB_DETECT => tsb_detect_statement(p),
        // Bare `CHECK` validates the interpreter runtime.
        // Compiled programs bring their own runtime, so it is a no-op.
        TSB_CHECK if at_statement_tail(p) => Ok(Statement::Rem(Vec::new())),
        // File-system / debug commands that are safe to no-op
        // when compiled — keeps the program flowing past calls
        // we can't actually honour (no disk I/O, no live BASIC
        // editor) but flags the missing operation visually as a
        // skipped REM in the asm.
        TSB_MAP | TSB_DIR | TSB_DUMP | TSB_COLD | TSB_OUT => Ok(tsb_noop_statement(p)),
        // `GRAPHICS` loads HSG support for the interpreter. Compiled
        // programs handle `\` HSG commands directly, so it is a no-op.
        TSB_GRAPHICS => Ok(tsb_noop_statement(p)),
        other => unsupported_tsb(p.line, other),
    }
}

fn normalize_tsb_token(token: u8) -> u8 {
    match token {
        // stores a few tokens as high-bit PETSCII bytes in some
        // builds. The docs map these back to their logical ids by XOR.
        0xB3 | 0xB2 | 0xB1 => token ^ 0x8F,
        other => other,
    }
}

fn cls_statement() -> Statement {
    Statement::Print(PrintStmt {
        items: vec![PrintItem::CharOut(Expr::Number(147.0))],
        trailing_newline: false,
    })
}

fn vol_statement(value: Expr) -> Statement {
    Statement::Poke {
        addr: Expr::Number(0xD418 as f64),
        value: Expr::Bin(BinOp::And, Box::new(value), Box::new(Expr::Number(15.0))),
    }
}

fn unsupported_tsb(line: u16, token: u8) -> Result<Statement, ParseError> {
    if let Some(name) = crate::tokens::tsb_keyword(token) {
        Err(ParseError::Unsupported { line, name })
    } else {
        Err(ParseError::UnsupportedToken { line, byte: token })
    }
}

fn unsupported_tsb_expr(line: u16, token: u8) -> Result<Expr, ParseError> {
    if let Some(name) = crate::tokens::tsb_keyword(token) {
        Err(ParseError::Unsupported { line, name })
    } else {
        Err(ParseError::UnsupportedToken { line, byte: token })
    }
}

fn tsb_noop_statement(p: &mut Cursor<'_>) -> Statement {
    let _ = p.take_until_statement_end();
    Statement::Rem(Vec::new())
}

/// `\<cmd>[,args]` — High-Speed-Graphics shorthand. Drawing commands
/// map to existing HIRES helpers; loader and interpreter-only commands
/// compile to no-ops. Entered with the cursor on `\` ($5C).
fn parse_hsg_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    p.advance(1); // the `\` ($5C)
    p.skip_spaces();
    let cmd = p.peek().ok_or(ParseError::ExpectedKeyword {
        line: p.line,
        what: "HSG command letter after `\\`",
    })?;
    p.advance(1);
    let norm = if cmd.is_ascii_alphabetic() {
        cmd | 0x20
    } else {
        cmd
    };
    // The command letter may be followed by one optional comma.
    p.skip_spaces();
    if p.peek() == Some(TOK_COMMA) {
        p.advance(1);
    }
    match norm {
        // \l x1,y1,x2,y2[,mode]  → LINE
        b'l' => {
            let x1 = expression(p)?;
            expect_comma(p, "',' in \\L")?;
            let y1 = expression(p)?;
            expect_comma(p, "',' in \\L")?;
            let x2 = expression(p)?;
            expect_comma(p, "',' in \\L")?;
            let y2 = expression(p)?;
            let mode = consume_trailing_mode_arg(p)?;
            Ok(Statement::Line {
                x1,
                y1,
                x2,
                y2,
                mode,
            })
        }
        // \b x1,y1,x2,y2[,mode]  → BLOCK (filled rectangle)
        b'b' => {
            let x1 = expression(p)?;
            expect_comma(p, "',' in \\B")?;
            let y1 = expression(p)?;
            expect_comma(p, "',' in \\B")?;
            let x2 = expression(p)?;
            expect_comma(p, "',' in \\B")?;
            let y2 = expression(p)?;
            let mode = consume_trailing_mode_arg(p)?;
            Ok(Statement::Block {
                x1,
                y1,
                x2,
                y2,
                mode,
            })
        }
        // \r x1,y1,x2,y2[,mode]  → REC outline from (x1,y1) to (x2,y2).
        b'r' => {
            let x1 = expression(p)?;
            expect_comma(p, "',' in \\R")?;
            let y1 = expression(p)?;
            expect_comma(p, "',' in \\R")?;
            let x2 = expression(p)?;
            expect_comma(p, "',' in \\R")?;
            let y2 = expression(p)?;
            let mode = consume_trailing_mode_arg(p)?;
            let width = Expr::Bin(BinOp::Sub, Box::new(x2), Box::new(x1.clone()));
            let height = Expr::Bin(BinOp::Sub, Box::new(y2), Box::new(y1.clone()));
            Ok(Statement::Rec {
                x: x1,
                y: y1,
                width,
                height,
                mode,
            })
        }
        // \k cx,cy,rx,ry[,mode]  → CIRCLE (ellipse)
        b'k' => {
            let cx = expression(p)?;
            expect_comma(p, "',' in \\K")?;
            let cy = expression(p)?;
            expect_comma(p, "',' in \\K")?;
            let rx = expression(p)?;
            expect_comma(p, "',' in \\K")?;
            let ry = expression(p)?;
            let mode = consume_trailing_mode_arg(p)?;
            Ok(Statement::Circle {
                cx,
                cy,
                radius: rx,
                ry: Some(ry),
                start: None,
                end: None,
                step: None,
                mode,
            })
        }
        // \p x,y[,mode]  → PLOT (single pixel, also sets the cursor)
        b'p' => {
            let x = expression(p)?;
            expect_comma(p, "',' in \\P")?;
            let y = expression(p)?;
            let mode = consume_trailing_mode_arg(p)?;
            Ok(Statement::Draw { x, y, mode })
        }
        // \d x,y[,mode]  → DRAW TO (line from the graphics cursor)
        b'd' => {
            let x = expression(p)?;
            expect_comma(p, "',' in \\D")?;
            let y = expression(p)?;
            let mode = consume_trailing_mode_arg(p)?;
            Ok(Statement::DrawTo { x, y, mode })
        }
        // \e [c1,c2]  → erase = HIRES
        b'e' => {
            p.skip_spaces();
            let (ink, paper) = if at_statement_tail(p) {
                (None, None)
            } else {
                let c1 = expression(p)?;
                expect_comma(p, "',' in \\E")?;
                let c2 = expression(p)?;
                (Some(c1), Some(c2))
            };
            Ok(Statement::Hires { ink, paper })
        }
        // \n  → normal screen = NRM
        b'n' => Ok(Statement::Nrm),
        // \h  → hires/gfx on = CSET 2
        b'h' => Ok(Statement::Cset {
            mode: Expr::Number(2.0),
        }),
        // \q combine, \i split, \m screen-select, \v string-eval,
        // \s set-cursor, \c colorize — interpreter/loader features
        // with no compiled equivalent. Swallow to the next `:`.
        _ => Ok(tsb_noop_statement(p)),
    }
}

fn tsb_key_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    p.skip_spaces();
    if p.peek() == Some(TOK_GET) {
        p.advance(1);
        return tsb_keyget_statement(p);
    }
    let index = expression(p)?;
    // `KEY <n>, "..."` is F-key binding. If there's no comma, the
    // program is not using the binding form, so treat the rest of the
    // statement as a no-op REM rather than refusing to compile.
    p.skip_spaces();
    if p.peek() != Some(TOK_COMMA) {
        return Ok(Statement::Rem(Vec::new()));
    }
    p.advance(1);
    let text = string_expression(p)?;
    Ok(Statement::KeySet { index, text })
}

fn tsb_keyget_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    p.skip_spaces();
    Ok(Statement::KeyGet { var: var_name(p)? })
}

fn print_expr_statement(expr: Expr) -> Statement {
    Statement::Print(PrintStmt {
        items: vec![PrintItem::Expr(expr)],
        trailing_newline: true,
    })
}

fn tsb_at_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    let _had_lparen = consume_optional_lparen(p);
    let lhs = var_name(p)?;
    if lhs.kind != VarKind::String {
        return Err(ParseError::ExpectedKeyword {
            line: p.line,
            what: "string variable as first AT argument",
        });
    }
    expect_comma(p, "',' in AT")?;
    let rhs = var_name(p)?;
    if rhs.kind != VarKind::String {
        return Err(ParseError::ExpectedKeyword {
            line: p.line,
            what: "string variable as second AT argument",
        });
    }
    expect_rparen(p, "')' after AT")?;
    Ok(Statement::SwapStr { lhs, rhs })
}

fn tsb_place_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    Ok(print_expr_statement(tsb_place_expr(p)?))
}

fn tsb_inst_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    p.skip_spaces();
    if p.peek() == Some(b'(') {
        Ok(print_expr_statement(tsb_inst_expr(p)?))
    } else {
        Err(ParseError::RejectedKeyword {
            line: p.line,
            keyword: "INST",
            reason: "the DOS-wedge installer has no runtime target in a standalone compiled program; use INST(...) as a function",
        })
    }
}

fn tsb_lin_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    if at_statement_tail(p) {
        Ok(print_expr_statement(Expr::Lin))
    } else {
        Err(ParseError::RejectedKeyword {
            line: p.line,
            keyword: "LIN",
            reason: "the line-save command manipulates BASIC source text; use LIN() as a function",
        })
    }
}

fn tsb_insert_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    let pattern = string_expression(p)?;
    expect_comma(p, "',' in INSERT")?;
    let row = expression(p)?;
    expect_comma(p, "',' in INSERT")?;
    let col = expression(p)?;
    expect_comma(p, "',' in INSERT")?;
    let width = expression(p)?;
    expect_comma(p, "',' in INSERT")?;
    let height = expression(p)?;
    expect_comma(p, "',' in INSERT")?;
    let color = expression(p)?;
    Ok(Statement::InsertBox {
        pattern,
        row,
        col,
        width,
        height,
        color,
    })
}

fn tsb_vol_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    p.skip_spaces();
    if let Some(enabled) = try_tsb_on_off(p)? {
        return Ok(vol_statement(Expr::Number(if enabled {
            10.0
        } else {
            0.0
        })));
    }
    Ok(vol_statement(expression(p)?))
}

fn at_statement_tail(p: &mut Cursor<'_>) -> bool {
    p.skip_spaces();
    matches!(p.peek(), None | Some(b':'))
}

fn try_tsb_on_off(p: &mut Cursor<'_>) -> Result<Option<bool>, ParseError> {
    let saved = p.pos;
    match parse_tsb_on_off(p) {
        Ok(value) => Ok(Some(value)),
        Err(ParseError::ExpectedKeyword { .. }) => {
            p.pos = saved;
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

fn tsb_multi_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    // `MULTI ON/OFF` toggles the mode bit; `MULTI c1, c2, c3`
    // sets bitmap-multicolor pens and enables multicolor mode.
    if let Some(enabled) = try_tsb_on_off(p)? {
        return Ok(Statement::Multi { enabled });
    }
    let c1 = expression(p)?;
    expect_comma(p, "',' in MULTI")?;
    let c2 = expression(p)?;
    expect_comma(p, "',' in MULTI")?;
    let c3 = expression(p)?;
    consume_trailing_numeric_args(p)?;
    Ok(Statement::MultiColors { c1, c2, c3 })
}

fn tsb_music_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    let tempo = expression(p)?;
    expect_comma(p, "',' in MUSIC")?;
    let tune = string_expression(p)?;
    Ok(Statement::Music { tempo, tune })
}

fn tsb_play_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    p.skip_spaces();
    if at_statement_tail(p) {
        return Ok(Statement::Play {
            mode: Expr::Number(2.0),
        });
    }
    if let Some(enabled) = try_tsb_on_off(p)? {
        return Ok(Statement::Play {
            mode: Expr::Number(if enabled { 2.0 } else { 0.0 }),
        });
    }
    Ok(Statement::Play {
        mode: expression(p)?,
    })
}

fn tsb_flash_statement(p: &mut Cursor<'_>, border: bool) -> Result<Statement, ParseError> {
    // FLASH and BFLASH share bare / ON / OFF tail forms, but their
    // argument shapes differ:
    //   FLASH                — toggle / enable
    //   FLASH ON | OFF
    //   FLASH color          — mark `color` for flashing (1 arg)
    //   FLASH color, speed   — same plus override animation speed
    //
    //   BFLASH               — toggle / enable
    //   BFLASH ON | OFF
    //   BFLASH speed, c1, c2 — full triple
    //
    // Our `Statement::Flash` / `Statement::Bflash` have the same
    // shape (`speed, color1, color2`), so for FLASH's 1- and 2-arg
    // shorthand we map `color` → `color1` and leave `color2` empty.
    p.skip_spaces();
    if at_statement_tail(p) {
        return Ok(make_flash_stmt(border, Some(true), None, None, None));
    }
    if let Some(enabled) = try_tsb_on_off(p)? {
        return Ok(make_flash_stmt(border, Some(enabled), None, None, None));
    }
    let first = expression(p)?;
    p.skip_spaces();
    if !border {
        // FLASH: 1 or 2 args. First arg is the colour to mark.
        let speed = if p.peek() == Some(TOK_COMMA) {
            p.advance(1);
            Some(expression(p)?)
        } else {
            None
        };
        return Ok(make_flash_stmt(false, Some(true), speed, Some(first), None));
    }
    // BFLASH: speed, c1, c2 — first arg is speed.
    expect_comma(p, "',' in FLASH/BFLASH")?;
    let color1 = expression(p)?;
    expect_comma(p, "',' in FLASH/BFLASH")?;
    let color2 = expression(p)?;
    Ok(make_flash_stmt(
        true,
        Some(true),
        Some(first),
        Some(color1),
        Some(color2),
    ))
}

fn make_flash_stmt(
    border: bool,
    enabled: Option<bool>,
    speed: Option<Expr>,
    color1: Option<Expr>,
    color2: Option<Expr>,
) -> Statement {
    if border {
        Statement::Bflash {
            enabled,
            speed,
            color1,
            color2,
        }
    } else {
        Statement::Flash {
            enabled,
            speed,
            color1,
            color2,
        }
    }
}

fn tsb_fetch_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    // Optional leading `AT(row, col)` positions the FETCH prompt.
    let position = parse_optional_position_at(p)?;
    let control = string_expression(p)?;
    expect_comma(p, "',' in FETCH")?;
    let max_len = expression(p)?;
    expect_comma(p, "',' in FETCH")?;
    let target = var_name(p)?;
    // FETCH may target a scalar or array element. Numeric targets read
    // typed digits through FIN; string targets receive the buffer.
    p.skip_spaces();
    let target_indices = if p.peek() == Some(b'(') {
        p.advance(1);
        let mut idx = Vec::new();
        loop {
            idx.push(expression(p)?);
            p.skip_spaces();
            if p.peek() != Some(TOK_COMMA) {
                break;
            }
            p.advance(1);
        }
        expect_rparen(p, "')' after FETCH array index")?;
        idx
    } else {
        Vec::new()
    };
    p.skip_spaces();
    let force = if p.peek() == Some(TOK_COMMA) {
        p.advance(1);
        Some(expression(p)?)
    } else {
        None
    };
    Ok(Statement::Fetch {
        control,
        max_len,
        target,
        target_indices,
        force,
        position,
    })
}

fn tsb_copy_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    let src = expression(p)?;
    expect_comma(p, "',' in COPY")?;
    let dst = expression(p)?;
    expect_comma(p, "',' in COPY")?;
    let len = expression(p)?;
    Ok(Statement::Copy { src, dst, len })
}

fn tsb_scr_statement(p: &mut Cursor<'_>, save: bool) -> Result<Statement, ParseError> {
    p.skip_spaces();
    if consume_mem_suffix(p, TOK_RESTORE, b"RESTORE") {
        return Ok(Statement::ScrRestore { save });
    }
    if consume_mem_suffix(p, TOK_DEF, b"DEF") {
        let addr = expression(p)?;
        p.skip_spaces();
        let mode = if p.peek() == Some(TOK_COMMA) {
            p.advance(1);
            Some(expression(p)?)
        } else {
            None
        };
        // DEF can take additional drive/secondary args (used by
        // programs that target a specific drive's REU). Swallow
        // them silently — our SCRLD/SCRSV codegen doesn't model
        // multi-drive targets but the surrounding code still
        // wants to compile.
        p.skip_spaces();
        while p.peek() == Some(TOK_COMMA) {
            p.advance(1);
            let _ = expression(p)?;
            p.skip_spaces();
        }
        return Ok(Statement::ScrDef { save, addr, mode });
    }
    if at_statement_tail(p) {
        return Ok(if save {
            Statement::ScrSave {
                addr: None,
                mode: None,
            }
        } else {
            Statement::ScrLoad {
                addr: None,
                mode: None,
            }
        });
    }
    let addr = expression(p)?;
    p.skip_spaces();
    let mode = if p.peek() == Some(TOK_COMMA) {
        p.advance(1);
        Some(expression(p)?)
    } else {
        None
    };
    // Tolerate trailing args (drive number, secondary address,
    // filename) — programs commonly write
    // `SCRLD 1, DR, 3, "FILE.SCR"` to load from a specific
    // drive. We don't honour the disk arguments yet; treat the
    // call as the equivalent local-buffer load so the program
    // proceeds to subsequent state setup.
    p.skip_spaces();
    while p.peek() == Some(TOK_COMMA) {
        p.advance(1);
        p.skip_spaces();
        if peek_is_string_atom(p) {
            let _ = string_expression(p)?;
        } else {
            let _ = expression(p)?;
        }
        p.skip_spaces();
    }
    Ok(if save {
        Statement::ScrSave {
            addr: Some(addr),
            mode,
        }
    } else {
        Statement::ScrLoad {
            addr: Some(addr),
            mode,
        }
    })
}

fn tsb_mem_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    p.skip_spaces();
    if consume_mem_suffix(p, TOK_SAVE, b"SAVE") {
        return tsb_mem_transfer_or_copy_statement(p, MemTransferOp::Save);
    }
    if consume_mem_suffix(p, TOK_LOAD, b"LOAD") {
        return tsb_mem_transfer_or_copy_statement(p, MemTransferOp::Load);
    }
    if consume_mem_suffix(p, TOK_READ, b"READ") {
        return tsb_mem_transfer_or_copy_statement(p, MemTransferOp::Read);
    }

    if consume_mem_suffix(p, TOK_CLR, b"CLR") {
        return tsb_mem_clr_statement(p);
    }

    if consume_mem_suffix(p, TOK_RESTORE, b"RESTORE") {
        return tsb_mem_restore_statement(p);
    }
    if consume_mem_suffix(p, TOK_DEF, b"DEF") {
        return tsb_mem_def_statement(p);
    }
    if consume_mem_suffix(p, TOK_CONT, b"CONT") {
        return tsb_mem_cont_statement(p);
    }
    if consume_mem_suffix(p, TOK_OR, b"OR") {
        return tsb_mem_c64_statement(p);
    }
    if consume_mem_suffix(p, TOK_POS, b"POS") {
        return tsb_mem_reu_pos_statement(p);
    }
    if consume_mem_suffix(p, TOK_LEN, b"LEN") {
        return tsb_mem_len_statement(p);
    }

    // Bare `MEM` (no suffix) is the mode-switch that
    // copies char ROM $D000-$DFFF to $E000-$EFFF, switches VIC to
    // bank 3, and points $D018 at screen $CC00 + char $E000. The
    // codegen emits `__MEM_INIT` (which also updates KERNAL
    // `$0288` so CHROUT writes to $CC00) and from then on CSET 0
    // / CSET 1 use the MEM-mode XOR'd $D018 values to select the
    // upper- or lower-case bank inside the relocated chars.
    // Programs that DESIGN custom chars at $E000+ rely on this —
    // without it those bitmaps are never read by VIC.
    if at_statement_tail(p) {
        return Ok(Statement::MemModeOn);
    }

    Err(ParseError::Unsupported {
        line: p.line,
        name: "MEM",
    })
}

fn consume_mem_suffix(p: &mut Cursor<'_>, token: u8, ascii: &[u8]) -> bool {
    p.skip_spaces();
    if p.peek() == Some(token) {
        p.advance(1);
        true
    } else {
        consume_ascii_command_word(p, ascii)
    }
}

fn tsb_mem_transfer_or_copy_statement(
    p: &mut Cursor<'_>,
    op: MemTransferOp,
) -> Result<Statement, ParseError> {
    p.skip_spaces();
    if at_statement_tail(p) {
        Ok(Statement::MemTransfer { op })
    } else {
        tsb_mem_copy_statement(p)
    }
}

fn tsb_mem_copy_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    p.skip_spaces();
    let src = expression(p)?;
    expect_comma(p, "',' in MEM copy")?;
    let dst = expression(p)?;
    expect_comma(p, "',' in MEM copy")?;
    let len = expression(p)?;
    Ok(Statement::Copy { src, dst, len })
}

fn tsb_mem_clr_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    p.skip_spaces();
    let addr = expression(p)?;
    expect_comma(p, "',' in MEM CLR")?;
    let len = expression(p)?;
    p.skip_spaces();
    let value = if p.peek() == Some(TOK_COMMA) {
        p.advance(1);
        Some(expression(p)?)
    } else {
        None
    };
    Ok(Statement::MemClr { addr, len, value })
}

fn tsb_mem_def_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    p.skip_spaces();
    let len = expression(p)?;
    let c64_addr = optional_comma_expr(p)?;
    let reu_addr = optional_comma_expr(p)?;
    let reu_bank = if reu_addr.is_some() {
        optional_comma_expr(p)?.or(Some(Expr::Number(0.0)))
    } else {
        None
    };
    let auto_inc = optional_comma_expr(p)?;
    let fixed = optional_comma_expr(p)?;
    Ok(Statement::MemDef {
        len,
        c64_addr,
        reu_addr,
        reu_bank,
        auto_inc,
        fixed,
    })
}

fn tsb_mem_len_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    Ok(Statement::MemLen {
        len: expression(p)?,
    })
}

fn tsb_mem_c64_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    Ok(Statement::MemC64Addr {
        addr: expression(p)?,
    })
}

fn tsb_mem_reu_pos_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    let addr = expression(p)?;
    p.skip_spaces();
    let bank = if p.peek() == Some(TOK_COMMA) {
        p.advance(1);
        expression(p)?
    } else {
        Expr::Number(0.0)
    };
    Ok(Statement::MemReuPos { addr, bank })
}

fn tsb_mem_restore_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    Ok(Statement::MemRestore {
        auto_inc: expression(p)?,
    })
}

fn tsb_mem_cont_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    Ok(Statement::MemCont {
        mode: expression(p)?,
    })
}

fn optional_comma_expr(p: &mut Cursor<'_>) -> Result<Option<Expr>, ParseError> {
    p.skip_spaces();
    if p.peek() == Some(TOK_COMMA) {
        p.advance(1);
        Ok(Some(expression(p)?))
    } else {
        Ok(None)
    }
}

fn tsb_design_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    let addr = expression(p)?;
    let mut bytes = Vec::new();
    loop {
        p.skip_spaces();
        if p.peek() != Some(TOK_COMMA) {
            break;
        }
        p.advance(1);
        bytes.push(expression(p)?);
    }
    // DESIGN consumes the rest of its BASIC line; bitmap bytes come
    // from the following `@` rows.
    while p.peek().is_some() {
        p.advance(1);
    }
    Ok(Statement::Design { addr, bytes })
}

fn tsb_display_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    p.skip_spaces();
    if p.peek() == Some(b'(') {
        consume_empty_call(p, "DISPLAY")?;
    }
    Ok(Statement::DisplayKeys)
}

fn tsb_disk_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    p.skip_spaces();
    Ok(Statement::Disk {
        command: string_expression(p)?,
    })
}

/// `USE [AT(r,c)] "ctrl$", var1, var2, … [;]` — formatted
/// PRINT. The control string holds literal bytes plus `#`-runs that
/// each consume the next numeric var, formatting it right-justified
/// (space-padded) in the run's width. Lowered into a regular `Print`
/// statement: each non-`#` chunk of the control string becomes a
/// `StrExpr(Literal)` item, each `#`-run becomes a `UseField`. An
/// optional leading `AT(r,c)` becomes a `PositionAt` item. A trailing
/// `;` suppresses the newline.
///
/// Forms we don't recognise (`USE n`, `USE #n,...`, or non-literal
/// control strings) fall back to a no-op. Without a trailing `;`, we
/// still emit a bare CR so the cursor reverse flag resets.
fn tsb_use_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    p.skip_spaces();

    // Optional `AT(r,c)` prefix.
    let mut items: Vec<PrintItem> = Vec::new();
    if matches!(p.peek(), Some(TOK_TSB_PREFIX))
        && p.peek_at(1).map(normalize_tsb_token) == Some(TSB_AT)
    {
        p.advance(2); // skip $64 $28 (= `AT(`)
        let row = expression(p)?;
        expect_comma(p, "',' in USE AT")?;
        let col = expression(p)?;
        expect_rparen(p, "')' after USE AT")?;
        items.push(PrintItem::PositionAt(row, col));
        p.skip_spaces();
    }

    // The control string must be a literal for our formatted-PRINT
    // path. Anything else (`USE n`, `USE #n,…`, runtime ctrl) falls
    // back to the no-op-with-conditional-CR behaviour.
    if p.peek() != Some(b'"') {
        return tsb_use_fallback(p);
    }
    p.advance(1);
    let mut ctrl: Vec<u8> = Vec::new();
    while let Some(c) = p.peek() {
        if c == b'"' {
            p.advance(1);
            break;
        }
        ctrl.push(c);
        p.advance(1);
    }

    // Variables follow, each after a comma.
    let mut vars: Vec<Expr> = Vec::new();
    loop {
        p.skip_spaces();
        if p.peek() != Some(TOK_COMMA) {
            break;
        }
        p.advance(1);
        vars.push(expression(p)?);
    }

    // Optional trailing `;` suppresses the newline.
    p.skip_spaces();
    let trailing_semicolon = p.peek() == Some(b';');
    if trailing_semicolon {
        p.advance(1);
    }

    // Walk the control string, emitting literal chunks and field
    // items. `#`-runs each consume the next var.
    let mut var_iter = vars.into_iter();
    let mut literal: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < ctrl.len() {
        if ctrl[i] == b'#' {
            if !literal.is_empty() {
                items.push(PrintItem::StrExpr(StrExpr::Literal(std::mem::take(
                    &mut literal,
                ))));
            }
            let start = i;
            while i < ctrl.len() && ctrl[i] == b'#' {
                i += 1;
            }
            let width = (i - start).min(255) as u8;
            // If there are fewer vars than `#` runs, treat remaining
            // runs as literal `#`s instead of failing at compile time.
            match var_iter.next() {
                Some(value) => items.push(PrintItem::UseField { width, value }),
                None => {
                    for _ in 0..(i - start) {
                        literal.push(b'#');
                    }
                }
            }
        } else {
            literal.push(ctrl[i]);
            i += 1;
        }
    }
    if !literal.is_empty() {
        items.push(PrintItem::StrExpr(StrExpr::Literal(literal)));
    }
    // Any unused vars get printed verbatim after the control string —
    // matches BASIC's behaviour where extra args run off the field
    // list without consuming format chars.
    for value in var_iter {
        items.push(PrintItem::UseField { width: 0, value });
    }

    Ok(Statement::Print(PrintStmt {
        items,
        trailing_newline: !trailing_semicolon,
    }))
}

/// Fallback for `USE` forms we don't model — swallow the rest of the
/// statement (respecting string literals so a `:` inside `"..."` isn't
/// the separator) and emit a bare CR when there is no trailing `;`.
fn tsb_use_fallback(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    let mut bytes = Vec::new();
    let mut in_string = false;
    while let Some(c) = p.peek() {
        if c == b'"' {
            in_string = !in_string;
        } else if c == b':' && !in_string {
            break;
        }
        bytes.push(c);
        p.advance(1);
    }
    let trailing_semicolon = bytes.last() == Some(&b';');
    if trailing_semicolon {
        Ok(Statement::Rem(bytes))
    } else {
        Ok(Statement::Print(PrintStmt {
            items: Vec::new(),
            trailing_newline: true,
        }))
    }
}

/// Parse a comma-separated variable list for `LOCAL`/`GLOBAL`.
/// Empty lists are treated as a no-op
/// when the line is just `LOCAL` with no operands).
fn tsb_var_list(p: &mut Cursor<'_>) -> Result<Vec<VarName>, ParseError> {
    let mut vars = Vec::new();
    p.skip_spaces();
    if matches!(p.peek(), None | Some(b':' | b';')) {
        return Ok(vars);
    }
    loop {
        p.skip_spaces();
        vars.push(var_name(p)?);
        p.skip_spaces();
        if p.peek() != Some(TOK_COMMA) {
            break;
        }
        p.advance(1);
    }
    Ok(vars)
}

fn tsb_local_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    Ok(Statement::Local {
        vars: tsb_var_list(p)?,
    })
}

fn tsb_global_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    Ok(Statement::Global {
        vars: tsb_var_list(p)?,
    })
}

fn tsb_line_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    let x1 = expression(p)?;
    expect_comma(p, "',' in LINE")?;
    let y1 = expression(p)?;
    expect_comma(p, "',' in LINE")?;
    let x2 = expression(p)?;
    expect_comma(p, "',' in LINE")?;
    let y2 = expression(p)?;
    let mode = consume_trailing_mode_arg(p)?;
    Ok(Statement::Line {
        x1,
        y1,
        x2,
        y2,
        mode,
    })
}

fn tsb_draw_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    p.skip_spaces();
    if p.peek() == Some(TOK_TO) || consume_ascii_command_word(p, b"TO") {
        if p.peek() == Some(TOK_TO) {
            p.advance(1);
        }
        let x = expression(p)?;
        expect_comma(p, "',' in DRAW TO")?;
        let y = expression(p)?;
        let mode = consume_trailing_mode_arg(p)?;
        return Ok(Statement::DrawTo { x, y, mode });
    }
    if peek_starts_string_expr(p) {
        // full form: `DRAW code$, x, y [, mode]`. The code
        // string is a sequence of digit-encoded turtle directions
        // interpreted relative to the current ROT orientation.
        let code = string_expression(p)?;
        expect_comma(p, "',' in DRAW")?;
        let x = expression(p)?;
        expect_comma(p, "',' in DRAW")?;
        let y = expression(p)?;
        let mode = consume_trailing_mode_arg(p)?;
        return Ok(Statement::DrawString { code, x, y, mode });
    }
    let x = expression(p)?;
    expect_comma(p, "',' in DRAW")?;
    let y = expression(p)?;
    let mode = consume_trailing_mode_arg(p)?;
    Ok(Statement::Draw { x, y, mode })
}

fn consume_trailing_numeric_args(p: &mut Cursor<'_>) -> Result<(), ParseError> {
    p.skip_spaces();
    while p.peek() == Some(TOK_COMMA) {
        p.advance(1);
        let _ignored = expression(p)?;
        p.skip_spaces();
    }
    Ok(())
}

/// Capture an optional trailing mode argument (the sticky `$f7`
/// byte) and swallow any further trailing args. Used by
/// graphics commands where the first argument after the
/// command-specific positional args is the pixel mode.
fn consume_trailing_mode_arg(p: &mut Cursor<'_>) -> Result<Option<Expr>, ParseError> {
    p.skip_spaces();
    let mode = if p.peek() == Some(TOK_COMMA) {
        p.advance(1);
        Some(expression(p)?)
    } else {
        None
    };
    consume_trailing_numeric_args(p)?;
    Ok(mode)
}

fn tsb_arc_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    // ARC uses the same parametric engine as CIRCLE but puts the
    // angle arguments first. Anything past `ry` is consumed.
    let cx = expression(p)?;
    expect_comma(p, "',' in ARC")?;
    let cy = expression(p)?;
    expect_comma(p, "',' in ARC")?;
    let start = expression(p)?;
    expect_comma(p, "',' in ARC")?;
    let end = expression(p)?;
    expect_comma(p, "',' in ARC")?;
    let step = expression(p)?;
    expect_comma(p, "',' in ARC")?;
    let radius = expression(p)?;
    p.skip_spaces();
    let ry = if p.peek() == Some(TOK_COMMA) {
        p.advance(1);
        Some(expression(p)?)
    } else {
        None
    };
    let mode = consume_trailing_mode_arg(p)?;
    Ok(Statement::Circle {
        cx,
        cy,
        radius,
        ry,
        start: Some(start),
        end: Some(end),
        step: Some(step),
        mode,
    })
}

fn tsb_circle_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    // `CIRCLE cx, cy, rx [, ry [, start [, end [, step [, mode]]]]]`.
    let cx = expression(p)?;
    expect_comma(p, "',' in CIRCLE")?;
    let cy = expression(p)?;
    expect_comma(p, "',' in CIRCLE")?;
    let radius = expression(p)?;
    let next = |p: &mut Cursor<'_>| -> Result<Option<Expr>, ParseError> {
        p.skip_spaces();
        if p.peek() == Some(TOK_COMMA) {
            p.advance(1);
            Ok(Some(expression(p)?))
        } else {
            Ok(None)
        }
    };
    let ry = next(p)?;
    let start = next(p)?;
    let end = next(p)?;
    let step = next(p)?;
    let mode = consume_trailing_mode_arg(p)?;
    Ok(Statement::Circle {
        cx,
        cy,
        radius,
        ry,
        start,
        end,
        step,
        mode,
    })
}

fn tsb_char_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    // `CHAR x, y, code [, mode [, zoom]]`. `mode` is the
    // pixel op (0/1/2); `zoom` is 1 or 2. Anything past `zoom`
    // is parsed and discarded.
    let x = expression(p)?;
    expect_comma(p, "',' in CHAR")?;
    let y = expression(p)?;
    expect_comma(p, "',' in CHAR")?;
    let code = expression(p)?;
    p.skip_spaces();
    let mode = if p.peek() == Some(TOK_COMMA) {
        p.advance(1);
        Some(expression(p)?)
    } else {
        None
    };
    p.skip_spaces();
    let zoom = if p.peek() == Some(TOK_COMMA) {
        p.advance(1);
        Some(expression(p)?)
    } else {
        None
    };
    consume_trailing_numeric_args(p)?;
    Ok(Statement::Char {
        x,
        y,
        code,
        mode,
        zoom,
    })
}

fn tsb_text_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    // `TEXT x, y, string$ [, mode [, zoom [, kerning]]]`. Mode and
    // zoom pass through to CHAR; kerning is the per-glyph X advance.
    // ZOOM only stretches vertically, so it does not change advance.
    let x = expression(p)?;
    expect_comma(p, "',' in TEXT")?;
    let y = expression(p)?;
    expect_comma(p, "',' in TEXT")?;
    let text = string_expression(p)?;
    p.skip_spaces();
    let mode = if p.peek() == Some(TOK_COMMA) {
        p.advance(1);
        Some(expression(p)?)
    } else {
        None
    };
    p.skip_spaces();
    let zoom = if p.peek() == Some(TOK_COMMA) {
        p.advance(1);
        Some(expression(p)?)
    } else {
        None
    };
    p.skip_spaces();
    let kerning = if p.peek() == Some(TOK_COMMA) {
        p.advance(1);
        Some(expression(p)?)
    } else {
        None
    };
    consume_trailing_numeric_args(p)?;
    Ok(Statement::Text {
        x,
        y,
        text,
        mode,
        zoom,
        kerning,
    })
}

fn tsb_paint_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    // `PAINT x, y [, mode]`.
    let x = expression(p)?;
    expect_comma(p, "',' in PAINT")?;
    let y = expression(p)?;
    let mode = consume_trailing_mode_arg(p)?;
    Ok(Statement::Paint { x, y, mode })
}

fn tsb_resume_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    // RESUME / RESUME NEXT / RESUME <line>. Bare RESUME re-runs the
    // line that errored; NEXT skips past it; <line> jumps anywhere.
    p.skip_spaces();
    if matches!(p.peek(), None | Some(b':')) {
        return Ok(Statement::Resume {
            target: ResumeTarget::Same,
        });
    }
    if p.peek() == Some(TOK_NEXT) {
        p.advance(1);
        return Ok(Statement::Resume {
            target: ResumeTarget::Next,
        });
    }
    if consume_ascii_command_word(p, b"NEXT") {
        return Ok(Statement::Resume {
            target: ResumeTarget::Next,
        });
    }
    Ok(Statement::Resume {
        target: ResumeTarget::Line(line_number(p)?),
    })
}

fn tsb_on_error_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    // `ON ERROR GOTO <line>` installs; `ON ERROR` (no target)
    // disables. `GOTO`/`GOSUB` is consumed if present so listings
    // with either form parse.
    p.skip_spaces();
    if matches!(p.peek(), None | Some(b':')) {
        return Ok(Statement::OnError { target: None });
    }
    if matches!(p.peek(), Some(TOK_GOTO | TOK_GOSUB)) {
        p.advance(1);
    } else {
        let _ = consume_ascii_command_word(p, b"GOTO") || consume_ascii_command_word(p, b"GOSUB");
    }
    p.skip_spaces();
    if matches!(p.peek(), None | Some(b':')) {
        return Ok(Statement::OnError { target: None });
    }
    Ok(Statement::OnError {
        target: Some(line_number(p)?),
    })
}

fn tsb_pause_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    // PAUSE accepts either `ticks` or `message$, ticks`.
    p.skip_spaces();
    let message = if peek_is_string_atom(p) {
        let m = string_expression(p)?;
        expect_comma(p, "',' after PAUSE message")?;
        Some(m)
    } else {
        None
    };
    Ok(Statement::Pause {
        message,
        ticks: expression(p)?,
    })
}

fn tsb_error_raise_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    // `ERROR <expr>` — explicitly raise BASIC error code <expr>.
    // A bare `ERROR` is malformed BASIC, but can appear in dead code.
    // Compile it as a no-op rather than failing the whole file.
    p.skip_spaces();
    if at_statement_tail(p) {
        return Ok(Statement::Rem(Vec::new()));
    }
    Ok(Statement::ErrorRaise {
        code: expression(p)?,
    })
}

fn tsb_mod_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    // `MOD ink, paper` — bulk recolour of HIRES screen RAM. Both
    // args are nibbles (0..15); the runtime packs them as
    // (ink<<4 | paper) and stamps every cell.
    let ink = expression(p)?;
    expect_comma(p, "',' in MOD")?;
    let paper = expression(p)?;
    consume_trailing_numeric_args(p)?;
    Ok(Statement::Mod { ink, paper })
}

fn tsb_dup_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    // `DUP src_x, src_y, width, height, dst_x, dst_y [,mode [,zoom]]`.
    let src_x = expression(p)?;
    expect_comma(p, "',' in DUP")?;
    let src_y = expression(p)?;
    expect_comma(p, "',' in DUP")?;
    let width = expression(p)?;
    expect_comma(p, "',' in DUP")?;
    let height = expression(p)?;
    expect_comma(p, "',' in DUP")?;
    let dst_x = expression(p)?;
    expect_comma(p, "',' in DUP")?;
    let dst_y = expression(p)?;
    p.skip_spaces();
    let mode = if p.peek() == Some(TOK_COMMA) {
        p.advance(1);
        Some(expression(p)?)
    } else {
        None
    };
    p.skip_spaces();
    let zoom = if p.peek() == Some(TOK_COMMA) {
        p.advance(1);
        Some(expression(p)?)
    } else {
        None
    };
    consume_trailing_numeric_args(p)?;
    Ok(Statement::Dup {
        src_x,
        src_y,
        width,
        height,
        dst_x,
        dst_y,
        mode,
        zoom,
    })
}

fn tsb_rot_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    let direction = expression(p)?;
    p.skip_spaces();
    let length = if p.peek() == Some(TOK_COMMA) {
        p.advance(1);
        Some(expression(p)?)
    } else {
        None
    };
    // Tolerate any further trailing args sources may carry.
    consume_trailing_numeric_args(p)?;
    Ok(Statement::Rot { direction, length })
}

fn tsb_angl_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    // `ANGL cx, cy, angle, rx [, ry [, mode]]` — separate semi-axes
    // give an elliptical polar offset; mode is the standard pixel op.
    let cx = expression(p)?;
    expect_comma(p, "',' in ANGL")?;
    let cy = expression(p)?;
    expect_comma(p, "',' in ANGL")?;
    let angle = expression(p)?;
    expect_comma(p, "',' in ANGL")?;
    let rx = expression(p)?;
    p.skip_spaces();
    let ry = if p.peek() == Some(TOK_COMMA) {
        p.advance(1);
        Some(expression(p)?)
    } else {
        None
    };
    let mode = consume_trailing_mode_arg(p)?;
    Ok(Statement::Angl {
        cx,
        cy,
        angle,
        rx,
        ry,
        mode,
    })
}

/// `PLOT x, y [, color]` — lets the third arg pick the
/// hi-color cell colour. HIRES uses a single colour pair seeded
/// at HIRES-entry, so the 3rd arg is accepted-and-ignored. The pixel
/// set then matches `DRAW` exactly.
fn tsb_plot_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    let x = expression(p)?;
    expect_comma(p, "',' in PLOT")?;
    let y = expression(p)?;
    let mode = consume_trailing_mode_arg(p)?;
    Ok(Statement::Draw { x, y, mode })
}

fn tsb_rec_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    let x = expression(p)?;
    expect_comma(p, "',' in REC")?;
    let y = expression(p)?;
    expect_comma(p, "',' in REC")?;
    let width = expression(p)?;
    expect_comma(p, "',' in REC")?;
    let height = expression(p)?;
    let mode = consume_trailing_mode_arg(p)?;
    Ok(Statement::Rec {
        x,
        y,
        width,
        height,
        mode,
    })
}

fn tsb_block_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    let x1 = expression(p)?;
    expect_comma(p, "',' in BLOCK")?;
    let y1 = expression(p)?;
    expect_comma(p, "',' in BLOCK")?;
    let x2 = expression(p)?;
    expect_comma(p, "',' in BLOCK")?;
    let y2 = expression(p)?;
    let mode = consume_trailing_mode_arg(p)?;
    Ok(Statement::Block {
        x1,
        y1,
        x2,
        y2,
        mode,
    })
}

fn expect_comma(p: &mut Cursor<'_>, context: &'static str) -> Result<(), ParseError> {
    p.skip_spaces();
    if p.peek() != Some(TOK_COMMA) {
        return Err(ParseError::ExpectedKeyword {
            line: p.line,
            what: context,
        });
    }
    p.advance(1);
    Ok(())
}

fn consume_ascii_d_bang(p: &mut Cursor<'_>) -> bool {
    match (p.peek(), p.peek_at(1)) {
        (Some(b'D' | b'd'), Some(b'!')) => {
            p.advance(2);
            true
        }
        _ => false,
    }
}

fn peek_ascii_command_word(p: &Cursor<'_>, word: &[u8]) -> bool {
    let rest = &p.bytes[p.pos..];
    if rest.len() < word.len() {
        return false;
    }
    if !rest[..word.len()]
        .iter()
        .zip(word.iter())
        .all(|(a, b)| a.to_ascii_uppercase() == *b)
    {
        return false;
    }
    let next = rest.get(word.len()).copied();
    !next.is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'$' | b'%'))
}

/// Same as `peek_ascii_command_word` but also rejects when the word
/// is immediately followed by `=` (TOK_EQ) — used at statement
/// dispatch to disambiguate `KEY=PEEK(197)` (BASIC-v2 variable "KE"
/// being assigned) from a `KEY` statement. Function-style
/// words like `LIN(`/`SOUND(` go through the regular peek so the
/// expression parser still recognises them.
fn peek_ascii_stmt_word(p: &Cursor<'_>, word: &[u8]) -> bool {
    if !peek_ascii_command_word(p, word) {
        return false;
    }
    let next = p.bytes.get(p.pos + word.len()).copied();
    !matches!(next, Some(TOK_EQ))
}

fn consume_ascii_stmt_word(p: &mut Cursor<'_>, word: &[u8]) -> bool {
    if !peek_ascii_stmt_word(p, word) {
        return false;
    }
    p.advance(word.len());
    true
}

fn consume_ascii_command_word(p: &mut Cursor<'_>, word: &[u8]) -> bool {
    if !peek_ascii_command_word(p, word) {
        return false;
    }
    p.advance(word.len());
    true
}

fn consume_ascii_unsupported_tsb_statement(p: &mut Cursor<'_>) -> Option<&'static str> {
    const WORDS: &[(&[u8], &str)] = &[
        (b"SECURE", "SECURE"),
        (b"DIR", "DIR"),
        (b"DUMP", "DUMP"),
        (b"FIND", "FIND"),
        (b"AUTO", "AUTO"),
        (b"OLD", "OLD"),
        (b"TEST", "TEST"),
        (b"LIN", "LIN"),
        (b"GRAPHICS", "GRAPHICS"),
        (b"ARC", "ARC"),
        (b"COLD", "COLD"),
        (b"HRDCPY", "HRDCPY"),
        (b"MERGE", "MERGE"),
        (b"RENUMBER", "RENUMBER"),
        (b"ERR", "ERR"),
        (b"OUT", "OUT"),
        (b"X!", "X!"),
        (b"MAP", "MAP"),
    ];
    for (word, name) in WORDS {
        let saved = p.pos;
        if consume_ascii_command_word(p, word) {
            p.skip_spaces();
            // Skip the diagnostic when the keyword is being used as a
            // BASIC v2 variable name — `KEY=...` (assignment) or
            // `KEY(...)` (array reference). The `=` may appear as the
            // tokenized form `$B2` (TOK_EQ), not just literal ASCII `=`.
            // Otherwise BASIC v2 programs that use these names as
            // variables would be rejected.
            if matches!(p.peek(), Some(b'=' | b'(' | TOK_EQ)) {
                p.pos = saved;
                continue;
            }
            return Some(*name);
        }
    }
    None
}

fn consume_ascii_screen_scroll_word(p: &mut Cursor<'_>) -> Option<ScreenScrollOp> {
    const WORDS: &[(&[u8], ScreenScrollOp)] = &[
        (b"UPB", ScreenScrollOp::UpBlank),
        (b"UPW", ScreenScrollOp::UpWrap),
        (b"LEFTW", ScreenScrollOp::LeftWrap),
        (b"LEFTB", ScreenScrollOp::LeftBlank),
        (b"DOWNB", ScreenScrollOp::DownBlank),
        (b"DOWNW", ScreenScrollOp::DownWrap),
        (b"RIGHTB", ScreenScrollOp::RightBlank),
        (b"RIGHTW", ScreenScrollOp::RightWrap),
    ];
    for (word, op) in WORDS {
        if consume_ascii_command_word(p, word) {
            return Some(*op);
        }
    }
    None
}

fn consume_ascii_two_word(p: &mut Cursor<'_>, first: &[u8], second: &[u8]) -> bool {
    let saved = p.pos;
    if !consume_ascii_command_word(p, first) {
        return false;
    }
    p.skip_spaces();
    if consume_ascii_command_word(p, second) {
        true
    } else {
        p.pos = saved;
        false
    }
}

fn consume_ascii_color_command(p: &mut Cursor<'_>) -> bool {
    // Some tokenisers split `COLOR` as ASCII `COL`
    // followed by BASIC v2's `OR` token. Treat that shape as the same
    // statement as the real `COLOUR` token.
    if p.peek().is_some_and(|b| b.eq_ignore_ascii_case(&b'C'))
        && p.peek_at(1).is_some_and(|b| b.eq_ignore_ascii_case(&b'O'))
        && p.peek_at(2).is_some_and(|b| b.eq_ignore_ascii_case(&b'L'))
        && p.peek_at(3) == Some(TOK_OR)
    {
        p.advance(4);
        return true;
    }
    consume_ascii_command_word(p, b"COLOR") || consume_ascii_command_word(p, b"COLOUR")
}

fn tsb_screen_rect_statement(
    p: &mut Cursor<'_>,
    op: ScreenRectOp,
) -> Result<Statement, ParseError> {
    let row = expression(p)?;
    expect_comma(p, "',' in screen rectangle command")?;
    let col = expression(p)?;
    expect_comma(p, "',' in screen rectangle command")?;
    let width = expression(p)?;
    expect_comma(p, "',' in screen rectangle command")?;
    let height = expression(p)?;
    let mut ch = None;
    let mut color = None;
    match op {
        ScreenRectOp::Fchr => {
            expect_comma(p, "',' in FCHR")?;
            ch = Some(expression(p)?);
        }
        ScreenRectOp::Fcol => {
            expect_comma(p, "',' in FCOL")?;
            color = Some(expression(p)?);
        }
        ScreenRectOp::Fill => {
            expect_comma(p, "',' in FILL")?;
            ch = Some(expression(p)?);
            expect_comma(p, "',' in FILL")?;
            color = Some(expression(p)?);
        }
        ScreenRectOp::Inv => {}
    }
    Ok(Statement::ScreenRect {
        op,
        row,
        col,
        width,
        height,
        ch,
        color,
    })
}

fn tsb_screen_move_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    let row = expression(p)?;
    expect_comma(p, "',' in MOVE")?;
    let col = expression(p)?;
    expect_comma(p, "',' in MOVE")?;
    let width = expression(p)?;
    expect_comma(p, "',' in MOVE")?;
    let height = expression(p)?;
    expect_comma(p, "',' in MOVE")?;
    let dest_row = expression(p)?;
    expect_comma(p, "',' in MOVE")?;
    let dest_col = expression(p)?;
    Ok(Statement::ScreenMove {
        row,
        col,
        width,
        height,
        dest_row,
        dest_col,
    })
}

fn tsb_screen_scroll_statement(
    p: &mut Cursor<'_>,
    op: ScreenScrollOp,
) -> Result<Statement, ParseError> {
    let row = expression(p)?;
    expect_comma(p, "',' in screen scroll command")?;
    let col = expression(p)?;
    expect_comma(p, "',' in screen scroll command")?;
    let width = expression(p)?;
    expect_comma(p, "',' in screen scroll command")?;
    let height = expression(p)?;
    Ok(Statement::ScreenScroll {
        op,
        row,
        col,
        width,
        height,
    })
}

fn tsb_sound_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    let voice = expression(p)?;
    expect_comma(p, "',' in SOUND")?;
    let freq = expression(p)?;
    Ok(Statement::Sound { voice, freq })
}

fn tsb_envelope_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    let voice = expression(p)?;
    expect_comma(p, "',' in ENVELOPE")?;
    let attack = expression(p)?;
    expect_comma(p, "',' in ENVELOPE")?;
    let decay = expression(p)?;
    expect_comma(p, "',' in ENVELOPE")?;
    let sustain = expression(p)?;
    expect_comma(p, "',' in ENVELOPE")?;
    let release = expression(p)?;
    Ok(Statement::Envelope {
        voice,
        attack,
        decay,
        sustain,
        release,
    })
}

fn tsb_wave_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    let voice = expression(p)?;
    expect_comma(p, "',' in WAVE")?;
    // WAVE accepts either an 8-digit binary switch panel or a regular
    // numeric expression.
    let control = parse_wave_control(p)?;
    p.skip_spaces();
    let pulse = if p.peek() == Some(TOK_COMMA) {
        p.advance(1);
        Some(expression(p)?)
    } else {
        None
    };
    Ok(Statement::Wave {
        voice,
        control,
        pulse,
    })
}

/// Parse an 8-character binary switch panel into a byte, or fall back
/// to a normal expression.
fn parse_wave_control(p: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    p.skip_spaces();
    let start = p.pos;
    let mut bits = 0u8;
    let mut count = 0;
    let mut i = start;
    while count < 8 && i < p.bytes.len() {
        match p.bytes[i] {
            b'0' => { /* zero — no bit */ }
            b'1' => bits |= 1 << (7 - count),
            _ => break,
        }
        i += 1;
        count += 1;
    }
    if count == 8 {
        // Commit to binary only when exactly 8 binary digits are present
        // and the run isn't extended by another digit / decimal point /
        // suffix. Anything else is a regular numeric literal.
        let extends = matches!(p.bytes.get(i), Some(b'0'..=b'9') | Some(b'.'));
        if !extends {
            p.pos = i;
            return Ok(Expr::Number(bits as f64));
        }
    }
    expression(p)
}

fn tsb_low_col_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    let color1 = expression(p)?;
    expect_comma(p, "',' in LOW COL")?;
    let color2 = expression(p)?;
    p.skip_spaces();
    let color3 = if p.peek() == Some(TOK_COMMA) {
        p.advance(1);
        Some(expression(p)?)
    } else {
        None
    };
    Ok(Statement::LowCol {
        color1,
        color2,
        color3,
    })
}

fn tsb_mmob_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    // `MMOB n, x, y` snaps; `MMOB n, sx, sy, ex, ey [, size [, speed]]`
    // glides. Decide based on whether a 4th argument follows.
    let index = expression(p)?;
    expect_comma(p, "',' in MMOB")?;
    let x = expression(p)?;
    expect_comma(p, "',' in MMOB")?;
    let y = expression(p)?;
    p.skip_spaces();
    if p.peek() != Some(TOK_COMMA) {
        return Ok(Statement::Mmob { index, x, y });
    }
    p.advance(1);
    let ex = expression(p)?;
    expect_comma(p, "',' in MMOB")?;
    let ey = expression(p)?;
    p.skip_spaces();
    let size = if p.peek() == Some(TOK_COMMA) {
        p.advance(1);
        Some(expression(p)?)
    } else {
        None
    };
    p.skip_spaces();
    let speed = if p.peek() == Some(TOK_COMMA) {
        p.advance(1);
        Some(expression(p)?)
    } else {
        None
    };
    // Tolerate any further trailing args (programs sometimes pad).
    p.skip_spaces();
    while p.peek() == Some(TOK_COMMA) {
        p.advance(1);
        let _ = expression(p)?;
        p.skip_spaces();
    }
    Ok(Statement::MmobGlide {
        index,
        sx: x,
        sy: y,
        ex,
        ey,
        size,
        speed,
    })
}

fn tsb_mob_set_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    let index = expression(p)?;
    expect_comma(p, "',' in MOB SET")?;
    let block = expression(p)?;
    expect_comma(p, "',' in MOB SET")?;
    let color = expression(p)?;
    expect_comma(p, "',' in MOB SET")?;
    let priority = expression(p)?;
    expect_comma(p, "',' in MOB SET")?;
    let multicolor = expression(p)?;
    p.skip_spaces();
    let size = if p.peek() == Some(TOK_COMMA) {
        p.advance(1);
        Some(expression(p)?)
    } else {
        None
    };
    // Optional trailing `, speed` is consumed later by MMOB/RLOCMOB
    // glides, so keep it in `__MOB_SPEED`.
    p.skip_spaces();
    let speed = if p.peek() == Some(TOK_COMMA) {
        p.advance(1);
        Some(expression(p)?)
    } else {
        None
    };
    // Tolerate any further trailing args.
    p.skip_spaces();
    while p.peek() == Some(TOK_COMMA) {
        p.advance(1);
        let _ = expression(p)?;
        p.skip_spaces();
    }
    Ok(Statement::MobSet {
        index,
        block,
        color,
        priority,
        multicolor,
        size,
        speed,
    })
}

fn tsb_rlocmob_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    let index = expression(p)?;
    expect_comma(p, "',' in RLOCMOB")?;
    let dx = expression(p)?;
    expect_comma(p, "',' in RLOCMOB")?;
    let dy = expression(p)?;
    // Optional trailing `, size [, speed]` per `befrlocm` → `smp2` →
    // `mobcont` → `setspeed`. We accept the size arg but discard it
    // (sprite size is owned by MOB SET in our model); speed overrides
    // `__MOB_SPEED[n]` during the glide.
    p.skip_spaces();
    let _size = if p.peek() == Some(TOK_COMMA) {
        p.advance(1);
        Some(expression(p)?)
    } else {
        None
    };
    p.skip_spaces();
    let speed = if p.peek() == Some(TOK_COMMA) {
        p.advance(1);
        Some(expression(p)?)
    } else {
        None
    };
    p.skip_spaces();
    while p.peek() == Some(TOK_COMMA) {
        p.advance(1);
        let _ = expression(p)?;
        p.skip_spaces();
    }
    Ok(Statement::Rlocmob {
        index,
        dx,
        dy,
        speed,
    })
}

fn tsb_detect_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    Ok(Statement::Detect {
        mode: expression(p)?,
    })
}

fn tsb_d_bang_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    p.skip_spaces();
    if p.peek() != Some(TOK_POKE) {
        return Err(ParseError::ExpectedKeyword {
            line: p.line,
            what: "POKE after D!",
        });
    }
    p.advance(1);
    let addr = expression(p)?;
    expect_comma(p, "',' in D!POKE")?;
    let value = expression(p)?;
    Ok(Statement::Dpoke { addr, value })
}

fn tsb_color_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    // `COLOR` shapes:
    //   `COLOR a`         → border = a
    //   `COLOR a, b`      → border = a, background = b
    //   `COLOR a, b, c`   → border = a, background = b, pen = c
    //   `COLOR , p`       → pen = p   (leading-comma shorthand)
    //
    // A leading comma updates only the pen. Otherwise operands fill
    // border / background / pen in order.
    p.skip_spaces();
    let mut border = None;
    let mut background = None;
    let mut pen = None;

    let at_end = |p: &Cursor<'_>| p.eof() || p.peek() == Some(b':');

    // Leading-comma shorthand: `COLOR , <pen>`. Tolerate a second
    // comma before the pen value too; `COLOR ,, X` is the same as
    // `COLOR , X`.
    if p.peek() == Some(TOK_COMMA) {
        while p.peek() == Some(TOK_COMMA) {
            p.advance(1);
            p.skip_spaces();
        }
        if !at_end(p) {
            pen = Some(expression(p)?);
        }
        return Ok(Statement::Color {
            border,
            background,
            pen,
        });
    }

    // Slot 1: border.
    if !at_end(p) {
        border = Some(expression(p)?);
        p.skip_spaces();
    }
    // Slot 2: background — only if the first comma is present.
    if p.peek() == Some(TOK_COMMA) {
        p.advance(1);
        p.skip_spaces();
        if !at_end(p) && p.peek() != Some(TOK_COMMA) {
            background = Some(expression(p)?);
            p.skip_spaces();
        }
        // Slot 3: pen — only if the second comma is present.
        if p.peek() == Some(TOK_COMMA) {
            p.advance(1);
            p.skip_spaces();
            if !at_end(p) {
                pen = Some(expression(p)?);
            }
        }
    }
    Ok(Statement::Color {
        border,
        background,
        pen,
    })
}

fn tsb_center_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    // Optional `AT(row, col)` prefix — moves the cursor before the
    // centered string is emitted. Common idiom: `CENTER AT(R,0) "..."`.
    p.skip_spaces();
    let position = parse_optional_position_at(p)?;
    p.skip_spaces();
    let text = string_expression(p)?;
    p.skip_spaces();
    let width = if p.peek() == Some(TOK_COMMA) {
        p.advance(1);
        expression(p)?
    } else {
        Expr::Number(40.0)
    };
    let spaces = Expr::Func1(
        Func1::Int,
        Box::new(Expr::Bin(
            BinOp::Div,
            Box::new(Expr::Bin(
                BinOp::Sub,
                Box::new(width),
                Box::new(Expr::Len(Box::new(text.clone()))),
            )),
            Box::new(Expr::Number(2.0)),
        )),
    );
    let mut items = Vec::new();
    if let Some((row, col)) = position {
        items.push(PrintItem::PositionAt(row, col));
    }
    items.push(PrintItem::Spc(spaces));
    items.push(PrintItem::StrExpr(text));
    // CENTER prints without a trailing carriage return. Adding one
    // would scroll when text lands on the last screen row.
    Ok(Statement::Print(PrintStmt {
        items,
        trailing_newline: false,
    }))
}

/// Recognise an `AT(row, col)` cursor-positioning prefix used by
/// `PRINT AT(...)` and `CENTER AT(...)`. Returns `Some((row,
/// col))` when consumed, `None` if the cursor isn't sitting on AT.
/// Both the tokenised form (TSB_AT byte) and the bare-ASCII form
/// ("AT" two letters in source) need handling — the tokeniser
/// emits one or the other depending on the toolchain.
fn parse_optional_position_at(p: &mut Cursor<'_>) -> Result<Option<(Expr, Expr)>, ParseError> {
    let saved_pos = p.pos;
    let had_tsb_token = matches!(p.peek(), Some(TOK_TSB_PREFIX)) && peek_tsb(p, TSB_AT);
    let consumed = if had_tsb_token {
        // TSB_AT tokenises with the '(' baked into the token
        // byte (`AT(` -> $64 $28). Subsequent `consume_optional_lparen`
        // is a no-op for this form.
        p.advance(2);
        true
    } else if consume_ascii_command_word(p, b"AT") {
        true
    } else {
        false
    };
    if !consumed {
        return Ok(None);
    }
    let had_lparen = consume_optional_lparen(p);
    // The ASCII "AT" form needs an explicit '(' or it would shadow a
    // BASIC variable named `AT`.
    if !had_tsb_token && !had_lparen {
        p.pos = saved_pos;
        return Ok(None);
    }
    // Disambiguate from the SwapStr `AT(A$, B$)` form: that
    // statement takes two string variables. Cursor positioning
    // takes numeric coordinates. If we see a string-flavoured atom
    // here, this isn't a position prefix — back out.
    if peek_is_string_atom(p) {
        p.pos = saved_pos;
        return Ok(None);
    }
    // The first sub-expression has to parse — if it doesn't, this
    // wasn't a position prefix and we should back out so the outer
    // PRINT-item parser can try `AT` as a user variable / array.
    let row = match expression(p) {
        Ok(e) => e,
        Err(_) => {
            p.pos = saved_pos;
            return Ok(None);
        }
    };
    // After the first arg, cursor positioning needs `,`. If we see
    // `)` instead, this is `AT(N)` — a user array access rather
    // than positioning. Back out so the outer expression parser
    // sees `AT` as a regular identifier (e.g. MONSTERS_&_MAGIC's
    // `PRINT ";AT(WM)`).
    p.skip_spaces();
    if p.peek() != Some(b',') {
        p.pos = saved_pos;
        return Ok(None);
    }
    p.advance(1);
    let col = expression(p)?;
    expect_rparen(p, "')' after AT")?;
    Ok(Some((row, col)))
}

fn tsb_mobcol_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    let index = expression(p)?;
    expect_comma(p, "',' in MOBCOL")?;
    let color = expression(p)?;
    Ok(Statement::Poke {
        addr: Expr::Bin(
            BinOp::Add,
            Box::new(Expr::Number(0xD027 as f64)),
            Box::new(index),
        ),
        value: color,
    })
}

fn tsb_cmob_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    let color1 = expression(p)?;
    expect_comma(p, "',' in CMOB")?;
    let color2 = expression(p)?;
    Ok(Statement::Cmob { color1, color2 })
}

fn tsb_bckgnds_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    let color0 = expression(p)?;
    expect_comma(p, "',' in BCKGNDS")?;
    let color1 = expression(p)?;
    expect_comma(p, "',' in BCKGNDS")?;
    let color2 = expression(p)?;
    expect_comma(p, "',' in BCKGNDS")?;
    let color3 = expression(p)?;
    Ok(Statement::Bckgnds {
        color0,
        color1,
        color2,
        color3,
    })
}

fn parse_tsb_on_off(p: &mut Cursor<'_>) -> Result<bool, ParseError> {
    p.skip_spaces();
    match p.peek() {
        Some(TOK_ON) => {
            p.advance(1);
            Ok(true)
        }
        Some(b'O' | b'o') if peek_ascii_command_word(p, b"ON") => {
            p.advance(2);
            Ok(true)
        }
        Some(b'O' | b'o') if peek_ascii_command_word(p, b"OFF") => {
            p.advance(3);
            Ok(false)
        }
        Some(TOK_TSB_PREFIX) if peek_tsb(p, TSB_OFF) => {
            p.advance(2);
            Ok(false)
        }
        _ => Err(ParseError::ExpectedKeyword {
            line: p.line,
            what: "ON or OFF",
        }),
    }
}

fn tsb_mob_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    p.skip_spaces();
    if consume_ascii_command_word(p, b"SET") {
        p.skip_spaces();
        return tsb_mob_set_statement(p);
    }
    if p.peek() == Some(TOK_ON) || peek_tsb(p, TSB_OFF) || peek_ascii_command_word(p, b"OFF") {
        let enabled = parse_tsb_on_off(p)?;
        p.skip_spaces();
        if p.peek() == Some(TOK_COMMA) {
            p.advance(1);
        }
        let index = expression(p)?;
        return Ok(Statement::MobEnable { index, enabled });
    }

    let index = expression(p)?;
    p.skip_spaces();
    if p.peek() == Some(TOK_COMMA) {
        p.advance(1);
    }
    let enabled = parse_tsb_on_off(p)?;
    Ok(Statement::MobEnable { index, enabled })
}

fn tsb_do_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    p.skip_spaces();
    if consume_ascii_word(p, b"NE") {
        return Ok(Statement::Done);
    }
    if consume_ascii_word(p, b"NULL") {
        return Ok(Statement::DoNull);
    }
    Ok(Statement::Do)
}

fn tsb_call_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    p.skip_spaces();
    match p.peek() {
        Some(b) if b.is_ascii_digit() || b == b'.' || b == b'(' || b == TOK_MINUS => {
            Ok(Statement::Sys {
                addr: expression(p)?,
                regs: Vec::new(),
                params: Vec::new(),
            })
        }
        // CALL is a tail call: it replaces the current PROC frame
        // instead of returning to the next statement.
        _ => Ok(Statement::ProcTailCall(proc_name(p)?)),
    }
}

fn tsb_exit_statement(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    p.skip_spaces();
    let cond = if p.peek() == Some(TOK_IF) {
        p.advance(1);
        Some(expression(p)?)
    } else if p.eof() || p.peek() == Some(b':') || p.peek() == Some(b';') {
        None
    } else {
        return Err(ParseError::ExpectedKeyword {
            line: p.line,
            what: "IF or end after EXIT",
        });
    };
    Ok(Statement::ExitLoop { cond })
}

fn rcomp_stmt(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    p.skip_spaces();
    if p.peek() == Some(TOK_THEN) {
        p.advance(1);
    } else if !consume_ascii_then(p) && p.peek() != Some(TOK_GOTO) {
        return Err(ParseError::ExpectedKeyword {
            line: p.line,
            what: "THEN or GOTO after RCOMP",
        });
    }
    let (then_branch, else_branch) = then_else_tail(p)?;
    Ok(Statement::Rcomp {
        then_branch,
        else_branch,
    })
}

fn proc_name(p: &mut Cursor<'_>) -> Result<ProcName, ParseError> {
    p.skip_spaces();
    let start = p.pos;
    while let Some(b) = p.peek() {
        if b == b':' || b == b';' {
            break;
        }
        p.advance(1);
    }
    let mut bytes = p.bytes[start..p.pos].to_vec();
    while bytes.last() == Some(&b' ') {
        bytes.pop();
    }
    if bytes.is_empty() {
        return Err(ParseError::ExpectedVar { line: p.line });
    }
    Ok(ProcName(bytes))
}

fn bare_identifier_statement(p: &Cursor<'_>) -> bool {
    let mut i = p.pos;
    if i >= p.bytes.len() || !p.bytes[i].is_ascii_alphabetic() {
        return false;
    }
    while i < p.bytes.len() && p.bytes[i].is_ascii_alphanumeric() {
        i += 1;
    }
    if i < p.bytes.len() && matches!(p.bytes[i], b'$' | b'%') {
        i += 1;
    }
    while i < p.bytes.len() && p.bytes[i] == b' ' {
        i += 1;
    }
    !matches!(p.bytes.get(i).copied(), Some(TOK_EQ | b'('))
}

fn consume_ascii_word(p: &mut Cursor<'_>, word: &[u8]) -> bool {
    let rest = &p.bytes[p.pos..];
    if rest.len() < word.len() {
        return false;
    }
    if rest[..word.len()]
        .iter()
        .zip(word.iter())
        .all(|(a, b)| a.to_ascii_uppercase() == *b)
    {
        p.advance(word.len());
        true
    } else {
        false
    }
}

fn print_stmt(p: &mut Cursor<'_>) -> Result<PrintStmt, ParseError> {
    let mut items = Vec::new();
    let mut trailing_newline = true;
    loop {
        p.skip_spaces();
        match p.peek() {
            None | Some(b':') => break,
            Some(TOK_TSB_PREFIX) if peek_tsb(p, TSB_ELSE) => break,
            // Statement-keyword tokens ($80-$A2) inside PRINT mark an
            // implicit statement boundary: `PRINT "x"; POKE 1024,1`
            // ends the PRINT at POKE rather than feeding $97 to the
            // expression parser. Function tokens ($A3+) like CHR$,
            // TAB, SPC stay inside the loop.
            Some(b) if (0x80..=0xA2).contains(&b) => break,
            Some(TOK_GO) => break,
            // Bare `"..."` falls into the string-expression branch below,
            // which handles both single literals and chains like
            // `"a" + "b" + var$`. A separate b'"' arm here would consume
            // only the leading literal and choke on the `+` that follows.
            Some(b';') => {
                p.advance(1);
                items.push(PrintItem::Semi);
                trailing_newline = false;
            }
            Some(b',') => {
                p.advance(1);
                items.push(PrintItem::Comma);
                trailing_newline = false;
            }
            Some(TOK_CHR) => {
                p.advance(1);
                let arg = paren_expr(p)?;
                items.push(PrintItem::CharOut(arg));
                trailing_newline = true;
            }
            // TAB( and SPC( bake the open-paren into the token byte
            // (verified via tokenizer output), so the expression starts
            // immediately and we expect a closing `)`.
            //
            // BASIC v2 ROM treats TAB/SPC as separators that suppress
            // the trailing CR (they return with carry set from their
            // dispatcher, and the PRINT loop's CR-or-not check honors
            // carry — see ROM $AAA0). So `PRINT SPC(3*I)` with no
            // trailing `;` does NOT add a newline. Mirror that by
            // clearing `trailing_newline`. 1x1-Kampf line 6300-6360
            // ("{home}", SPC, then a vertical `*{down}` strip) is the
            // canonical failure: the spurious CR shifts the strip one
            // row down so the loop walks off the bottom and scrolls.
            Some(TOK_TAB) => {
                p.advance(1);
                let arg = baked_paren_expr(p)?;
                items.push(PrintItem::Tab(arg));
                trailing_newline = false;
            }
            Some(TOK_SPC) => {
                p.advance(1);
                let arg = baked_paren_expr(p)?;
                items.push(PrintItem::Spc(arg));
                trailing_newline = false;
            }
            // `PRINT A$=B$` / `PRINT "X"<"Y"` etc. — a string atom
            // followed by a comparison operator produces a numeric
            // result (-1 / 0). Route through the numeric expression
            // path so `expression()` sees the whole compare. Without
            // this, `string_expression()` would consume only the
            // leading atom and the bare `=` left over would trip the
            // expression-fallback arm with "expected an expression".
            Some(_) if peek_is_string_compare(p) => {
                let e = expression(p)?;
                items.push(PrintItem::Expr(e));
                trailing_newline = true;
            }
            // `PRINT A$;`, `PRINT LEFT$(...)`, etc — anything that
            // produces a string routes through StrExpr instead of
            // falling into the numeric expression path.
            Some(_) if peek_is_string_atom(p) => {
                let s = string_expression(p)?;
                items.push(PrintItem::StrExpr(s));
                trailing_newline = true;
            }
            // `PRINT TAB(14)+"X"+A$(I)` — BASIC v2's PRINT loop
            // re-enters expression evaluation after TAB/SPC, and
            // FRMEVL accepts unary `+` as a no-op even before a
            // string atom. The resulting concat then yields a
            // string. Mirror that: when the next non-space byte
            // is `+` followed by a string atom, swallow the `+`
            // and route through string_expression so the operands
            // stay typed as strings.
            Some(TOK_PLUS) if peek_string_atom_after_plus(p) => {
                p.advance(1);
                let s = string_expression(p)?;
                items.push(PrintItem::StrExpr(s));
                trailing_newline = true;
            }
            _ => {
                // `PRINT AT(row, col) ...` — cursor
                // positioning prefix. Emitted as a PositionAt item
                // so subsequent pieces in the same PRINT print at
                // (row, col).
                if let Some((row, col)) = parse_optional_position_at(p)? {
                    items.push(PrintItem::PositionAt(row, col));
                    trailing_newline = true;
                } else {
                    let e = expression(p)?;
                    items.push(PrintItem::Expr(e));
                    trailing_newline = true;
                }
            }
        }
    }
    Ok(PrintStmt {
        items,
        trailing_newline,
    })
}

#[derive(Clone, Copy)]
enum FileOp {
    Load,
    Save,
    Verify,
}

/// Shared parser for LOAD / SAVE / VERIFY. All three share the form
/// `<keyword> "name" [, device [, secondary]]`. Filename is required
/// — bare `LOAD` (cassette next-program) isn't useful in compiled
/// programs and we reject it here with a clearer error.
fn file_op_stmt(p: &mut Cursor<'_>, op: FileOp) -> Result<Statement, ParseError> {
    p.skip_spaces();
    // The filename is the only string-typed argument here, so a leading
    // `(` can only be a parenthesised string expression — `LOAD (F$),8`.
    // (OPEN can't assume this, which is why the shared heuristic omits it.)
    if !peek_starts_string_expr(p) && p.peek() != Some(b'(') {
        // BASIC v2's bare `LOAD` (chain the next program from tape)
        // and the directory form `LOAD "$",8` aren't meaningful in a
        // compiled single-binary program. In lenient mode drop the
        // statement to a REM so the rest of the program still
        // compiles; strict mode reports it.
        if p.lenient_syntax {
            while let Some(b) = p.peek() {
                if b == b':' || b == b';' {
                    break;
                }
                p.advance(1);
            }
            return Ok(Statement::Rem(Vec::new()));
        }
        let _ = op;
        return Err(ParseError::ExpectedKeyword {
            line: p.line,
            what: "string filename after LOAD/SAVE/VERIFY",
        });
    }
    let filename = string_expression(p)?;
    let mut device = None;
    let mut secondary = None;
    let mut load_addr = None;
    p.skip_spaces();
    if p.peek() == Some(TOK_COMMA) {
        p.advance(1);
        // `LOAD "name", USE, bank, addr` designates a target
        // address. Compile it as a normal disk LOAD to device 8.
        p.skip_spaces();
        if consume_tsb_use_keyword(p) && matches!(op, FileOp::Load) {
            load_addr = Some(tsb_load_use_addr(p)?);
        } else {
            device = Some(expression(p)?);
            p.skip_spaces();
            if p.peek() == Some(TOK_COMMA) {
                p.advance(1);
                secondary = Some(expression(p)?);
                p.skip_spaces();
                // extension: the fourth LOAD argument is a
                // literal target address. SAVE/VERIFY do not take one.
                if p.peek() == Some(TOK_COMMA) && matches!(op, FileOp::Load) {
                    p.advance(1);
                    load_addr = Some(expression(p)?);
                }
            }
        }
    }
    p.skip_spaces();
    if consume_tsb_use_keyword(p) {
        if !matches!(op, FileOp::Load) {
            return Err(ParseError::UnsupportedFeature {
                line: p.line,
                what: "USE suffix is only supported on LOAD",
            });
        }
        load_addr = Some(tsb_load_use_addr(p)?);
    }
    Ok(match op {
        FileOp::Load => Statement::Load {
            filename,
            device,
            secondary,
            load_addr,
        },
        FileOp::Save => Statement::Save {
            filename,
            device,
            secondary,
        },
        FileOp::Verify => Statement::Verify {
            filename,
            device,
            secondary,
        },
    })
}

fn consume_tsb_use_keyword(p: &mut Cursor<'_>) -> bool {
    p.skip_spaces();
    consume_tsb(p, TSB_USE) || consume_ascii_command_word(p, b"USE")
}

fn tsb_load_use_addr(p: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    p.skip_spaces();
    if p.peek() == Some(TOK_COMMA) {
        p.advance(1);
    }
    // syntax is `USE,bank,addr`; C64 builds use bank 0.
    let _bank = expression(p)?;
    expect_comma(p, "',' in LOAD USE")?;
    expression(p)
}

/// `OPEN file [, device [, secondary [, "filename"]]]`. The shape
/// follows BASIC v2: file is required, the rest taper off as defaults.
fn open_stmt(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    let file_num = expression(p)?;
    let mut device = None;
    let mut secondary = None;
    let mut filename = None;
    p.skip_spaces();
    if p.peek() == Some(TOK_COMMA) {
        p.advance(1);
        device = Some(expression(p)?);
        p.skip_spaces();
        if p.peek() == Some(TOK_COMMA) {
            p.advance(1);
            // Either secondary OR filename. Filename is anything
            // string-typed; secondary is anything numeric. Look ahead
            // to see whether the next token starts a string expression.
            p.skip_spaces();
            if peek_starts_string_expr(p) {
                filename = Some(string_expression(p)?);
            } else {
                secondary = Some(expression(p)?);
                p.skip_spaces();
                if p.peek() == Some(TOK_COMMA) {
                    p.advance(1);
                    p.skip_spaces();
                    filename = Some(string_expression(p)?);
                }
            }
        }
    }
    Ok(Statement::Open {
        file_num,
        device,
        secondary,
        filename,
    })
}

/// Heuristic: does the next token begin a string-typed expression?
/// True for a quote, a string variable (`A$`), CHR$/STR$/LEFT$/RIGHT$/
/// MID$/TI$/GET-string. Used by OPEN to decide whether the next arg is
/// the filename or another numeric secondary.
fn peek_starts_string_expr(p: &Cursor<'_>) -> bool {
    match p.peek() {
        Some(b'"') => true,
        Some(TOK_CHR) | Some(TOK_STR) | Some(TOK_LEFT) | Some(TOK_RIGHT) | Some(TOK_MID) => true,
        Some(b) if b.is_ascii_alphabetic() => peek_is_string_var(p),
        _ => false,
    }
}

/// `CMD file_num [, items]` — same shape as PRINT# but lowers to
/// `Statement::Cmd` so codegen knows not to CLRCHN at the end.
fn cmd_stmt(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    let file_num = expression(p)?;
    p.skip_spaces();
    if p.peek() != Some(TOK_COMMA) && p.peek() != Some(b';') {
        return Ok(Statement::Cmd {
            file_num,
            body: PrintStmt {
                items: Vec::new(),
                trailing_newline: true,
            },
        });
    }
    p.advance(1);
    let body = print_stmt(p)?;
    Ok(Statement::Cmd { file_num, body })
}

/// `INPUT# file_num, target [, target ...]`. The INPUT# token is its
/// own keyword (separate from INPUT, which is $85). Targets are the
/// same shape as READ/INPUT — scalar or array element.
fn input_file_stmt(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    let file_num = expression(p)?;
    p.skip_spaces();
    if p.peek() != Some(TOK_COMMA) {
        return Err(ParseError::ExpectedKeyword {
            line: p.line,
            what: "',' after INPUT# file number",
        });
    }
    p.advance(1);
    let targets = parse_read_target_list(p)?;
    Ok(Statement::InputFile { file_num, targets })
}

/// `PRINT# file_num, items...`. The PRINT# token is its own keyword
/// (separate from PRINT), so we get here straight after consuming it.
fn print_file_stmt(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    let file_num = expression(p)?;
    p.skip_spaces();
    if p.peek() != Some(TOK_COMMA) && p.peek() != Some(b';') {
        // Bare `PRINT# n` with no items is allowed and just emits a
        // newline on the channel. Treat as empty body with newline.
        return Ok(Statement::PrintFile {
            file_num,
            body: PrintStmt {
                items: Vec::new(),
                trailing_newline: true,
            },
        });
    }
    p.advance(1); // skip the separator
    let body = print_stmt(p)?;
    Ok(Statement::PrintFile { file_num, body })
}

fn let_assign(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    let name = var_name(p)?;
    p.skip_spaces();
    // `A(I[, J, ...]) = expr` parses as an array-element write rather
    // than a scalar assignment. The element value is parsed in the
    // same type domain as the variable: string for `A$(I)=...`,
    // numeric otherwise.
    if p.peek() == Some(b'(') {
        p.advance(1);
        let indices = index_list(p)?;
        p.skip_spaces();
        if p.peek() != Some(TOK_EQ) {
            return Err(ParseError::ExpectedKeyword {
                line: p.line,
                what: "'=' after array index",
            });
        }
        p.advance(1);
        if name.kind == VarKind::String {
            let value = string_expression(p)?;
            return Ok(Statement::ArrayLetStr {
                name,
                indices,
                value,
            });
        }
        let value = expression(p)?;
        return Ok(Statement::ArrayLet {
            name,
            indices,
            value,
        });
    }
    if p.peek() != Some(TOK_EQ) {
        return Err(ParseError::ExpectedKeyword {
            line: p.line,
            what: "'=' after variable",
        });
    }
    p.advance(1);
    if name.kind == VarKind::String {
        let value = string_expression(p)?;
        return Ok(Statement::LetStr { var: name, value });
    }
    let value = expression(p)?;
    Ok(Statement::Let { name, value })
}

/// Parse a string expression: a sequence of string atoms joined by `+`.
fn string_expression(p: &mut Cursor<'_>) -> Result<StrExpr, ParseError> {
    let mut lhs = string_atom(p)?;
    loop {
        p.skip_spaces();
        if p.peek() == Some(TOK_PLUS) {
            p.advance(1);
            let rhs = string_atom(p)?;
            lhs = StrExpr::Concat(Box::new(lhs), Box::new(rhs));
        } else {
            break;
        }
    }
    Ok(lhs)
}

fn string_atom(p: &mut Cursor<'_>) -> Result<StrExpr, ParseError> {
    p.skip_spaces();
    // `$$<expr>` (hex string) and `%%<expr>` (binary
    // string) — both prefix operators that turn an int16 into a
    // PETSCII digit string. Each pair is two literal bytes in
    // the tokenized source. Width is hex 2 or 4, bin 8 or 16, depending
    // on whether the high byte is zero.
    if p.peek() == Some(b'$') && p.peek_at(1) == Some(b'$') {
        p.advance(2);
        let arg = expression(p)?;
        return Ok(StrExpr::HexFmt(Box::new(arg)));
    }
    if p.peek() == Some(b'%') && p.peek_at(1) == Some(b'%') {
        p.advance(2);
        let arg = expression(p)?;
        return Ok(StrExpr::BinFmt(Box::new(arg)));
    }
    match p.peek() {
        Some(b'"') => {
            p.advance(1);
            Ok(StrExpr::Literal(p.take_string_body()))
        }
        // Accept parens around any expression, including string ones.
        // Peel off the matching `)` after a recursive string_expression
        // so concat/operator chaining inside the parens still works.
        Some(b'(') => {
            p.advance(1);
            let inner = string_expression(p)?;
            p.skip_spaces();
            if p.peek() != Some(b')') {
                return Err(ParseError::ExpectedKeyword {
                    line: p.line,
                    what: "')' in parenthesised string expression",
                });
            }
            p.advance(1);
            Ok(inner)
        }
        Some(TOK_CHR) => {
            p.advance(1);
            let arg = paren_expr(p)?;
            Ok(StrExpr::Chr(Box::new(arg)))
        }
        Some(TOK_STR) => {
            p.advance(1);
            let arg = paren_expr(p)?;
            Ok(StrExpr::Str(Box::new(arg)))
        }
        Some(TOK_LEFT) => {
            p.advance(1);
            let (s, args) = paren_str_then_nums(p, 1)?;
            Ok(StrExpr::Left(
                Box::new(s),
                Box::new(args.into_iter().next().unwrap()),
            ))
        }
        Some(TOK_RIGHT) => {
            p.advance(1);
            let (s, args) = paren_str_then_nums(p, 1)?;
            Ok(StrExpr::Right(
                Box::new(s),
                Box::new(args.into_iter().next().unwrap()),
            ))
        }
        Some(TOK_MID) => {
            p.advance(1);
            // MID$(s$, start) or MID$(s$, start, n)
            p.skip_spaces();
            if p.peek() != Some(b'(') {
                return Err(ParseError::ExpectedKeyword {
                    line: p.line,
                    what: "'(' after MID$",
                });
            }
            p.advance(1);
            let s = string_expression(p)?;
            p.skip_spaces();
            if p.peek() != Some(TOK_COMMA) {
                return Err(ParseError::ExpectedKeyword {
                    line: p.line,
                    what: "',' in MID$",
                });
            }
            p.advance(1);
            let start = expression(p)?;
            p.skip_spaces();
            let length = if p.peek() == Some(TOK_COMMA) {
                p.advance(1);
                Some(Box::new(expression(p)?))
            } else {
                None
            };
            p.skip_spaces();
            if p.peek() != Some(b')') {
                return Err(ParseError::ExpectedKeyword {
                    line: p.line,
                    what: "')'",
                });
            }
            p.advance(1);
            Ok(StrExpr::Mid(Box::new(s), Box::new(start), length))
        }
        // The extension prefix byte is `$64`, which is also ASCII
        // `d`, so tokenized string functions must be checked before
        // the generic variable arm.
        Some(TOK_TSB_PREFIX)
            if p.peek_at(1)
                .map(normalize_tsb_token)
                .is_some_and(|t| matches!(t, TSB_DUP | TSB_INSERT | TSB_INST)) =>
        {
            tsb_str_atom(p)
        }
        Some(b'D' | b'd') if peek_ascii_command_word(p, b"DUP") => {
            p.advance(3);
            tsb_dup_str_expr(p)
        }
        Some(b'I' | b'i') if peek_ascii_command_word(p, b"INSERT") => {
            p.advance(6);
            tsb_insert_str_expr(p)
        }
        Some(b) if b.is_ascii_alphabetic() => {
            let v = var_name(p)?;
            if v.kind != VarKind::String {
                return Err(ParseError::UnsupportedFeature {
                    line: p.line,
                    what: "numeric variable in string context",
                });
            }
            p.skip_spaces();
            if p.peek() == Some(b'(') {
                p.advance(1);
                let indices = index_list(p)?;
                Ok(StrExpr::ArrayRef(v, indices))
            } else {
                Ok(StrExpr::Var(v))
            }
        }
        _ => Err(ParseError::ExpectedExpr { line: p.line }),
    }
}

fn tsb_str_atom(p: &mut Cursor<'_>) -> Result<StrExpr, ParseError> {
    debug_assert_eq!(p.peek(), Some(TOK_TSB_PREFIX));
    p.advance(1);
    let Some(token) = p.peek() else {
        return Err(ParseError::ExpectedExpr { line: p.line });
    };
    p.advance(1);
    match normalize_tsb_token(token) {
        TSB_DUP => tsb_dup_str_expr(p),
        TSB_INSERT => tsb_insert_str_expr(p),
        TSB_INST => tsb_inst_str_expr(p),
        other => {
            if let Some(name) = crate::tokens::tsb_keyword(other) {
                Err(ParseError::Unsupported { line: p.line, name })
            } else {
                Err(ParseError::UnsupportedToken {
                    line: p.line,
                    byte: other,
                })
            }
        }
    }
}

fn tsb_dup_str_expr(p: &mut Cursor<'_>) -> Result<StrExpr, ParseError> {
    p.skip_spaces();
    if p.peek() != Some(b'(') {
        return Err(ParseError::ExpectedKeyword {
            line: p.line,
            what: "'(' after DUP",
        });
    }
    p.advance(1);
    let source = string_expression(p)?;
    expect_comma(p, "',' in DUP")?;
    let count = expression(p)?;
    p.skip_spaces();
    if p.peek() != Some(b')') {
        return Err(ParseError::ExpectedKeyword {
            line: p.line,
            what: "')' after DUP",
        });
    }
    p.advance(1);
    Ok(StrExpr::Dup(Box::new(source), Box::new(count)))
}

fn tsb_insert_str_expr(p: &mut Cursor<'_>) -> Result<StrExpr, ParseError> {
    p.skip_spaces();
    if p.peek() != Some(b'(') {
        return Err(ParseError::ExpectedKeyword {
            line: p.line,
            what: "'(' after INSERT",
        });
    }
    p.advance(1);
    let source = string_expression(p)?;
    expect_comma(p, "',' in INSERT")?;
    let insertion = string_expression(p)?;
    expect_comma(p, "',' in INSERT")?;
    let pos = expression(p)?;
    p.skip_spaces();
    if p.peek() != Some(b')') {
        return Err(ParseError::ExpectedKeyword {
            line: p.line,
            what: "')' after INSERT",
        });
    }
    p.advance(1);
    Ok(StrExpr::Insert(
        Box::new(source),
        Box::new(insertion),
        Box::new(pos),
    ))
}

/// `INST(replacement$, target$, pos)` — return `target$` with
/// the `len(replacement$)`-char slice at the 1-based position `pos`
/// *overwritten* by `replacement$` (result length stays `len(target$)`,
/// not the lengthening insert that `INSERT(...)` does). Lowered to
/// `LEFT$(target$, pos-1) + replacement$ + MID$(target$, pos + len(replacement$))`,
/// which reuses the existing string-op codegen. A `pos` of 0 still
/// raises ?ILLEGAL QUANTITY via the negative `LEFT$` length.
fn tsb_inst_str_expr(p: &mut Cursor<'_>) -> Result<StrExpr, ParseError> {
    p.skip_spaces();
    if p.peek() != Some(b'(') {
        return Err(ParseError::ExpectedKeyword {
            line: p.line,
            what: "'(' after INST",
        });
    }
    p.advance(1);
    let replacement = string_expression(p)?;
    expect_comma(p, "',' in INST")?;
    let target = string_expression(p)?;
    expect_comma(p, "',' in INST")?;
    let pos = expression(p)?;
    expect_rparen(p, "')' after INST")?;
    let prefix_len = Expr::Bin(
        BinOp::Sub,
        Box::new(pos.clone()),
        Box::new(Expr::Number(1.0)),
    );
    let suffix_start = Expr::Bin(
        BinOp::Add,
        Box::new(pos),
        Box::new(Expr::Len(Box::new(replacement.clone()))),
    );
    Ok(StrExpr::Concat(
        Box::new(StrExpr::Concat(
            Box::new(StrExpr::Left(
                Box::new(target.clone()),
                Box::new(prefix_len),
            )),
            Box::new(replacement),
        )),
        Box::new(StrExpr::Mid(Box::new(target), Box::new(suffix_start), None)),
    ))
}

/// Parse `(<string-expr> , <num-expr> [, <num-expr>...])`. Returns the
/// string and the list of numeric arguments. `expected_nums` is the
/// number of trailing numeric arguments after the comma — used by
/// `LEFT$` and `RIGHT$` (1) which is constant.
fn paren_str_then_nums(
    p: &mut Cursor<'_>,
    expected_nums: usize,
) -> Result<(StrExpr, Vec<Expr>), ParseError> {
    p.skip_spaces();
    if p.peek() != Some(b'(') {
        return Err(ParseError::ExpectedKeyword {
            line: p.line,
            what: "'(' after function",
        });
    }
    p.advance(1);
    let s = string_expression(p)?;
    let mut nums = Vec::with_capacity(expected_nums);
    for _ in 0..expected_nums {
        p.skip_spaces();
        if p.peek() != Some(TOK_COMMA) {
            return Err(ParseError::ExpectedKeyword {
                line: p.line,
                what: "',' in function call",
            });
        }
        p.advance(1);
        nums.push(expression(p)?);
    }
    p.skip_spaces();
    if p.peek() != Some(b')') {
        return Err(ParseError::ExpectedKeyword {
            line: p.line,
            what: "')'",
        });
    }
    p.advance(1);
    Ok((s, nums))
}

fn dim_stmt(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    let mut specs = Vec::new();
    loop {
        let name = var_name(p)?;
        p.skip_spaces();
        if p.peek() != Some(b'(') {
            // extension: `DIM A, B$, C%(10), D` — bare names
            // declare scalar variables (zero-initialised at the
            // standard slot type) instead of arrays. Standard BASIC
            // v2 would refuse, but uses this as a scalar
            // declaration. Treat a missing `(` as a zero-dim entry;
            // codegen's array
            // emission turns a zero-dim spec into a regular scalar.
            specs.push(DimSpec {
                name,
                dims: Vec::new(),
            });
            if p.peek() == Some(TOK_COMMA) {
                p.advance(1);
                continue;
            }
            break;
        }
        p.advance(1);
        // Each dim is a numeric expression. BASIC v2 evaluates these
        // at runtime; we accept anything the parser does and require
        // the constant-fold pass to collapse them to a literal so we
        // can allocate storage statically.
        let mut dims = Vec::new();
        loop {
            p.skip_spaces();
            dims.push(expression(p)?);
            p.skip_spaces();
            if p.peek() == Some(TOK_COMMA) {
                p.advance(1);
            } else {
                break;
            }
        }
        if p.peek() != Some(b')') {
            return Err(ParseError::ExpectedKeyword {
                line: p.line,
                what: "')' in DIM",
            });
        }
        p.advance(1);
        specs.push(DimSpec { name, dims });
        p.skip_spaces();
        if p.peek() == Some(TOK_COMMA) {
            p.advance(1);
        } else {
            break;
        }
    }
    Ok(Statement::Dim(specs))
}

/// Parse `(<expr> [, <expr> ...])` index list. Caller has already
/// matched the opening `(`. Returns the list and advances past `)`.
fn index_list(p: &mut Cursor<'_>) -> Result<Vec<Expr>, ParseError> {
    let mut out = Vec::new();
    loop {
        out.push(expression(p)?);
        p.skip_spaces();
        if p.peek() == Some(TOK_COMMA) {
            p.advance(1);
        } else {
            break;
        }
    }
    if p.peek() != Some(b')') {
        return Err(ParseError::ExpectedKeyword {
            line: p.line,
            what: "')'",
        });
    }
    p.advance(1);
    Ok(out)
}

fn if_stmt(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    let cond = expression(p)?;
    p.skip_spaces();
    if p.peek() == Some(TOK_THEN) {
        p.advance(1);
        p.skip_spaces();
    } else if consume_ascii_then(p) {
        p.skip_spaces();
    } else {
        // BASIC v2 also allows `IF cond GOTO line` with no THEN. Accept
        // GOTO directly to be lenient.
        if p.peek() != Some(TOK_GOTO) {
            return Err(ParseError::ExpectedKeyword {
                line: p.line,
                what: "THEN",
            });
        }
    }
    // Double-THEN typo: skip the redundant token so the THEN body
    // still parses.
    while p.peek() == Some(TOK_THEN) {
        p.advance(1);
        p.skip_spaces();
    }
    if peek_tsb(p, TSB_DO) && !tsb_do_suffix_is_done_or_null(p) {
        p.advance(2);
        return Ok(Statement::DoIf { cond });
    }
    let (then_branch, else_branch) = then_else_tail(p)?;
    Ok(match else_branch {
        Some(else_branch) => Statement::IfElse {
            cond,
            then_branch,
            else_branch,
        },
        None => Statement::If { cond, then_branch },
    })
}

fn tsb_do_suffix_is_done_or_null(p: &Cursor<'_>) -> bool {
    let mut i = p.pos + 2;
    while i < p.bytes.len() && p.bytes[i] == b' ' {
        i += 1;
    }
    starts_ascii_ci(&p.bytes[i..], b"NE") || starts_ascii_ci(&p.bytes[i..], b"NULL")
}

fn then_else_tail(p: &mut Cursor<'_>) -> Result<(ThenBranch, Option<ThenBranch>), ParseError> {
    let then_branch = then_tail_until_else(p)?;
    p.skip_spaces();
    let else_branch = if consume_tsb(p, TSB_ELSE) {
        p.skip_spaces();
        Some(then_tail_to_end(p)?)
    } else {
        None
    };
    Ok((then_branch, else_branch))
}

fn then_tail_until_else(p: &mut Cursor<'_>) -> Result<ThenBranch, ParseError> {
    if matches!(p.peek(), Some(b) if b.is_ascii_digit()) {
        let n = line_number(p)?;
        skip_after_then_line_number(p);
        return Ok(ThenBranch::Goto(n));
    }
    if p.peek() == Some(TOK_GOTO) {
        p.advance(1);
        p.skip_spaces();
        let n = line_number(p)?;
        skip_after_then_line_number(p);
        return Ok(ThenBranch::Goto(n));
    }
    let stmts = inline_stmts(p, true)?;
    Ok(ThenBranch::Stmts(stmts))
}

/// `IF cond THEN <line>` consumes the rest of the line — BASIC v2
/// skips it on a false condition and never reaches it on a true one.
/// Drop everything until ELSE or end-of-line so trailing `:STMTS`
/// (the v2 dead-code idiom) and stray comment-like bytes (e.g. a
/// trailing `*`) don't confuse the outer line parser.
fn skip_after_then_line_number(p: &mut Cursor<'_>) {
    p.skip_spaces();
    while let Some(b) = p.peek() {
        if b == TOK_TSB_PREFIX && p.peek_at(1).map(normalize_tsb_token) == Some(TSB_ELSE) {
            return;
        }
        p.advance(1);
    }
}

fn then_tail_to_end(p: &mut Cursor<'_>) -> Result<ThenBranch, ParseError> {
    if matches!(p.peek(), Some(b) if b.is_ascii_digit()) {
        let n = line_number(p)?;
        return Ok(ThenBranch::Goto(n));
    }
    if p.peek() == Some(TOK_GOTO) {
        p.advance(1);
        p.skip_spaces();
        let n = line_number(p)?;
        return Ok(ThenBranch::Goto(n));
    }
    let stmts = inline_stmts(p, false)?;
    Ok(ThenBranch::Stmts(stmts))
}

fn inline_stmts(p: &mut Cursor<'_>, stop_at_else: bool) -> Result<Vec<Statement>, ParseError> {
    let mut stmts = Vec::new();
    loop {
        p.skip_spaces();
        while let Some(b':') = p.peek() {
            p.advance(1);
            p.skip_spaces();
        }
        if p.eof() || (stop_at_else && peek_tsb(p, TSB_ELSE)) {
            break;
        }
        stmts.push(statement(p)?);
        p.skip_spaces();
        match p.peek() {
            None => break,
            Some(b':') | Some(b';') => {
                p.advance(1);
            }
            Some(_) if stop_at_else && peek_tsb(p, TSB_ELSE) => {}
            Some(b) if is_statement_start_byte(b) => { /* implicit separator after numeric arg */ }
            Some(_) => {
                // Same trailing-junk recovery as `parse_line` —
                // skip a stray byte and continue. Frequently fires
                // for corrupt sources where a tail like `THEN…{$04}`
                // or `THEN…EADY.` (truncated READY.) bleeds onto
                // a THEN body.
                while let Some(b) = p.peek() {
                    if b == b':' || b == b';' {
                        p.advance(1);
                        break;
                    }
                    p.advance(1);
                }
            }
        }
    }
    Ok(stmts)
}

fn starts_ascii_ci(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.len() >= needle.len()
        && haystack[..needle.len()]
            .iter()
            .zip(needle.iter())
            .all(|(a, b)| a.to_ascii_uppercase() == *b)
}

fn consume_ascii_then(p: &mut Cursor<'_>) -> bool {
    let rest = &p.bytes[p.pos..];
    if rest.starts_with(b"THEN") {
        p.advance(4);
        return true;
    }
    if rest.starts_with(b"TH") {
        let after = rest.get(2).copied();
        if after.is_none() || matches!(after, Some(b' ' | b':' | b';')) {
            p.advance(2);
            return true;
        }
    }
    false
}

fn for_stmt(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    let var = var_name(p)?;
    if var.kind == VarKind::String {
        return Err(ParseError::UnsupportedFeature {
            line: p.line,
            what: "FOR with string counter",
        });
    }
    p.skip_spaces();
    if p.peek() != Some(TOK_EQ) {
        return Err(ParseError::ExpectedKeyword {
            line: p.line,
            what: "'=' in FOR",
        });
    }
    p.advance(1);
    let start = expression(p)?;
    p.skip_spaces();
    if p.peek() != Some(TOK_TO) {
        return Err(ParseError::ExpectedKeyword {
            line: p.line,
            what: "TO in FOR",
        });
    }
    p.advance(1);
    let end = expression(p)?;
    p.skip_spaces();
    let step = if p.peek() == Some(TOK_STEP) {
        p.advance(1);
        expression(p)?
    } else {
        Expr::Number(1.0)
    };
    Ok(Statement::For {
        var,
        start,
        end,
        step,
    })
}

fn data_stmt(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    use crate::ast::DataValue;
    let mut values = Vec::new();
    loop {
        p.skip_spaces();
        match p.peek() {
            None | Some(b':') => break,
            // Quoted string — terminates at the next `"` or end of
            // statement (BASIC v2 closes an unterminated DATA string
            // at the statement boundary).
            Some(b'"') => {
                p.advance(1);
                values.push(DataValue::String(p.take_string_body()));
            }
            // Optional unary minus on a numeric literal.
            Some(TOK_MINUS) => {
                let start = p.pos;
                p.advance(1);
                p.skip_spaces();
                let n = p.take_number();
                p.skip_spaces();
                // If the byte after the number isn't a separator,
                // BASIC v2 treats the whole thing as a string DATA
                // item (READ into a numeric var traps later). Re-
                // collect from the original `-` so the stored item
                // matches what RUN would have parsed.
                if !matches!(p.peek(), None | Some(TOK_COMMA) | Some(b':')) {
                    p.pos = start;
                    let bytes = take_unquoted_data_item(p);
                    values.push(DataValue::String(bytes));
                } else {
                    values.push(DataValue::Float(-n));
                }
            }
            // Real numeric literal: digit, or a leading `.` that's
            // immediately followed by a digit (so `.5` is `0.5`).
            // A bare `.` introducing other text — e.g. Compu-Tarot's
            // `DATA. OPTIMISM PRODUCES...` — is not a number; let it
            // fall through to the unquoted-string collector so the
            // whole item lands in DATA as a literal.
            Some(b)
                if b.is_ascii_digit()
                    || (b == b'.' && p.peek_at(1).is_some_and(|c| c.is_ascii_digit())) =>
            {
                let start = p.pos;
                let n = p.take_number();
                p.skip_spaces();
                if !matches!(p.peek(), None | Some(TOK_COMMA) | Some(b':')) {
                    // E.T's `DATA ...,3X3 SPRITES,...` — the digit
                    // run is followed by letters, so the whole token
                    // is a literal string, not a 3 with garbage.
                    p.pos = start;
                    let bytes = take_unquoted_data_item(p);
                    values.push(DataValue::String(bytes));
                } else {
                    values.push(DataValue::Float(n));
                }
            }
            // Bare token / identifier without quotes — BASIC v2 actually
            // accepts this and stores it as a string up to the next `,`
            // or `:`. Rather than re-parsing every keyword token, we
            // collect the raw bytes verbatim until the terminator.
            Some(_) => {
                let bytes = take_unquoted_data_item(p);
                values.push(DataValue::String(bytes));
            }
        }
        p.skip_spaces();
        if p.peek() == Some(TOK_COMMA) {
            p.advance(1);
        } else {
            break;
        }
    }
    Ok(Statement::Data(values))
}

/// Collect bytes for an unquoted DATA item up to the next `,` or `:`
/// or end-of-line. Trims one trailing space (BASIC v2 strips a single
/// trailing space because of how its tokeniser handles statement
/// boundaries; multiple trailing spaces are kept).
fn take_unquoted_data_item(p: &mut Cursor<'_>) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(b) = p.peek() {
        if b == TOK_COMMA || b == b':' {
            break;
        }
        out.push(b);
        p.advance(1);
    }
    while out.last() == Some(&b' ') {
        out.pop();
    }
    out
}

fn read_stmt(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    Ok(Statement::Read(parse_read_target_list(p)?))
}

fn input_stmt(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    p.skip_spaces();
    // Optional `"prompt";` before the variable list.
    let prompt = if p.peek() == Some(b'"') {
        p.advance(1);
        let bytes = p.take_string_body();
        p.skip_spaces();
        if p.peek() != Some(b';') {
            return Err(ParseError::ExpectedKeyword {
                line: p.line,
                what: "';' after INPUT prompt",
            });
        }
        p.advance(1);
        Some(bytes)
    } else {
        None
    };
    let targets = parse_read_target_list(p)?;
    Ok(Statement::Input { prompt, targets })
}

/// Parse a comma-separated list of `READ`/`INPUT` targets — each is
/// either a scalar variable name or an array element. Stops at the
/// next non-comma byte (typically end of statement).
fn parse_read_target_list(p: &mut Cursor<'_>) -> Result<Vec<crate::ast::ReadTarget>, ParseError> {
    use crate::ast::ReadTarget;
    let mut out = Vec::new();
    loop {
        let v = var_name(p)?;
        p.skip_spaces();
        let t = if p.peek() == Some(b'(') {
            p.advance(1);
            let mut indices = Vec::new();
            loop {
                indices.push(expression(p)?);
                p.skip_spaces();
                match p.peek() {
                    Some(TOK_COMMA) => {
                        p.advance(1);
                    }
                    Some(b')') => {
                        p.advance(1);
                        break;
                    }
                    _ => {
                        return Err(ParseError::ExpectedKeyword {
                            line: p.line,
                            what: "',' or ')' in array index",
                        });
                    }
                }
            }
            ReadTarget::Array { name: v, indices }
        } else {
            ReadTarget::Scalar(v)
        };
        out.push(t);
        p.skip_spaces();
        if p.peek() == Some(TOK_COMMA) {
            p.advance(1);
        } else {
            break;
        }
    }
    Ok(out)
}

fn on_stmt(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    let value = expression(p)?;
    p.skip_spaces();
    let kind = match p.peek() {
        Some(TOK_GOTO) => {
            p.advance(1);
            OnBranchKind::Goto
        }
        Some(TOK_GOSUB) => {
            p.advance(1);
            OnBranchKind::GoSub
        }
        _ => {
            return Err(ParseError::ExpectedKeyword {
                line: p.line,
                what: "GOTO or GOSUB after ON",
            });
        }
    };
    let mut targets = Vec::new();
    loop {
        p.skip_spaces();
        targets.push(line_number(p)?);
        p.skip_spaces();
        if p.peek() == Some(TOK_COMMA) {
            p.advance(1);
        } else {
            break;
        }
    }
    skip_line_target_label(p);
    Ok(Statement::OnBranch {
        value,
        kind,
        targets,
    })
}

fn next_stmt(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    let mut vars = Vec::new();
    p.skip_spaces();
    match p.peek() {
        Some(b) if b.is_ascii_alphabetic() => {
            vars.push(Some(var_name(p)?));
            // Comma-form: `NEXT I, J, K` is shorthand for three NEXTs
            // popping the FOR stack in order.
            loop {
                p.skip_spaces();
                if p.peek() != Some(TOK_COMMA) {
                    break;
                }
                p.advance(1);
                vars.push(Some(var_name(p)?));
            }
        }
        _ => vars.push(None),
    }
    Ok(Statement::Next { vars })
}

fn line_number(p: &mut Cursor<'_>) -> Result<u16, ParseError> {
    let start = p.pos;
    while let Some(b) = p.peek() {
        if !b.is_ascii_digit() {
            break;
        }
        p.advance(1);
    }
    if p.pos == start {
        return Err(ParseError::ExpectedLineNumber { line: p.line });
    }
    let s = std::str::from_utf8(&p.bytes[start..p.pos]).expect("ASCII digits");
    let n: u32 = s.parse().expect("digits parse");
    // BASIC v2's interactive editor caps at 63999, but the tokenised
    // format stores line numbers as u16 — and programs in the wild
    // (Cylon Zap line 490: `GOTO 65535`) use a high non-existent
    // line as a deliberate `?UNDEF'D STATEMENT ERROR` abort. Accept
    // anything that fits in u16; codegen routes unresolved targets
    // through the undefined-line error handler.
    if n > 65535 {
        return Err(ParseError::LineNumberOverflow {
            line: p.line,
            value: n,
        });
    }
    Ok(n as u16)
}

/// After a `GOTO`, `GOSUB` or `ON … GOTO/GOSUB` line-number target,
/// BASIC v2 ignores any characters baked onto the end of the number up
/// to the next statement separator. Renumber and label tools rely on
/// this to tag a target with a readable name (`GOSUB 500DEFGFX`), where
/// the tokeniser may even turn a leading keyword in the label into a
/// token byte. Skip that label so the leftover bytes are not taken as a
/// following statement. Stop at `:` / `;` (statements past those are
/// still reachable on return) and at the TSB `ELSE` that ends a `THEN`
/// body.
fn skip_line_target_label(p: &mut Cursor<'_>) {
    p.skip_spaces();
    while let Some(b) = p.peek() {
        if b == b':' || b == b';' {
            break;
        }
        if b == TOK_TSB_PREFIX && p.peek_at(1).map(normalize_tsb_token) == Some(TSB_ELSE) {
            break;
        }
        p.advance(1);
    }
}

fn var_name(p: &mut Cursor<'_>) -> Result<VarName, ParseError> {
    p.skip_spaces();
    let Some(first) = p.peek() else {
        return Err(ParseError::ExpectedVar { line: p.line });
    };
    if !first.is_ascii_alphabetic() {
        return Err(ParseError::ExpectedVar { line: p.line });
    }
    let mut base = String::new();
    base.push(first.to_ascii_uppercase() as char);
    p.advance(1);
    // BASIC v2's identifier parser ignores embedded spaces — `RN B(5)`
    // (FootballStrategy line 4460) is the same as `RNB(5)`, which
    // becomes array `RN` (truncated to 2 chars) indexed by 5. Skip
    // through spaces while accumulating alphanumeric chars; the
    // first two surviving chars are the canonical name.
    p.skip_spaces();
    if let Some(b) = p.peek() {
        if b.is_ascii_alphanumeric() {
            base.push(b.to_ascii_uppercase() as char);
            p.advance(1);
            // BASIC keeps only the first two characters; consume and
            // discard any trailing alphanumerics (and the spaces
            // between them) so the parser cursor ends up where the
            // user expects.
            loop {
                p.skip_spaces();
                match p.peek() {
                    Some(b) if b.is_ascii_alphanumeric() => p.advance(1),
                    _ => break,
                }
            }
        }
    }
    let mut kind = match p.peek() {
        Some(b'%') => {
            p.advance(1);
            VarKind::Integer
        }
        Some(b'$') => {
            p.advance(1);
            VarKind::String
        }
        _ => VarKind::Float,
    };
    // REM-hint promotion: a Float var whose base appears in the
    // active hint set was declared as integer via `REM@i=...` or
    // `REM@ \WORD ...`. Lift it to Integer so the rest of the
    // pipeline sees it as if the user had typed the `%` suffix.
    if kind == VarKind::Float && p.int_hint_vars.contains(&base) {
        kind = VarKind::Integer;
    }
    Ok(VarName { base, kind })
}

/// Parse a user-function name (the `F` in `FN F(...)`). Same shape as
/// `var_name` minus the type-suffix logic — FN names are always the
/// implicit float type and a `$` or `%` after them would be a syntax
/// error, not a different namespace.
fn fn_name(p: &mut Cursor<'_>) -> Result<FnName, ParseError> {
    p.skip_spaces();
    let Some(first) = p.peek() else {
        return Err(ParseError::ExpectedVar { line: p.line });
    };
    if !first.is_ascii_alphabetic() {
        return Err(ParseError::ExpectedVar { line: p.line });
    }
    let mut base = String::new();
    base.push(first.to_ascii_uppercase() as char);
    p.advance(1);
    if let Some(b) = p.peek() {
        if b.is_ascii_alphanumeric() {
            base.push(b.to_ascii_uppercase() as char);
            p.advance(1);
            while let Some(b) = p.peek() {
                if b.is_ascii_alphanumeric() {
                    p.advance(1);
                } else {
                    break;
                }
            }
        }
    }
    Ok(FnName(base))
}

/// `DEF FN F(X) = expr` — parsed entry point. The `DEF` token has
/// already been consumed; we expect `FN`, the function name, the
/// parameter in parens, `=`, and finally the body expression.
fn def_fn_stmt(p: &mut Cursor<'_>) -> Result<Statement, ParseError> {
    p.skip_spaces();
    if p.peek() != Some(TOK_FN) {
        return Err(ParseError::ExpectedKeyword {
            line: p.line,
            what: "FN after DEF",
        });
    }
    p.advance(1);
    let name = fn_name(p)?;
    p.skip_spaces();
    if p.peek() != Some(b'(') {
        return Err(ParseError::ExpectedKeyword {
            line: p.line,
            what: "'(' after DEF FN name",
        });
    }
    p.advance(1);
    let param = var_name(p)?;
    if param.kind != VarKind::Float {
        return Err(ParseError::UnsupportedFeature {
            line: p.line,
            what: "DEF FN with non-numeric parameter",
        });
    }
    p.skip_spaces();
    if p.peek() != Some(b')') {
        return Err(ParseError::ExpectedKeyword {
            line: p.line,
            what: "')' after DEF FN param",
        });
    }
    p.advance(1);
    p.skip_spaces();
    if p.peek() != Some(TOK_EQ) {
        return Err(ParseError::ExpectedKeyword {
            line: p.line,
            what: "'=' in DEF FN",
        });
    }
    p.advance(1);
    let body = expression(p)?;
    Ok(Statement::DefFn { name, param, body })
}

// ----- expression parser (recursive descent, precedence climbing) -----

fn expression(p: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    or_expr(p)
}

/// Parse an expression in a boolean context (UNTIL / IF / WHILE
/// later). programs commonly write `UNTIL X$` to wait for
/// a key — a bare string variable used as a truthy condition.
/// Standard BASIC v2 raises ?TYPE MISMATCH there, so the regular
/// `expression` parser rejects it. Here, when the next atom is a
/// string and no comparison operator follows, fall back to
/// `LEN(s$)` which produces 0 for empty / non-zero otherwise —
/// matching the natural "non-empty = truthy" reading.
fn parse_truthy_expression(p: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    p.skip_spaces();
    if peek_is_string_atom(p) {
        // A leading string in a condition is either a string
        // comparison (`BF$ = "0"`) or a bare string variable used as
        // a truthy flag (`REPEAT: GETX$: UNTIL X$` — non-empty =
        // truthy → wrap as LEN). Either way it sits at the cmp-expr
        // level, so an AND/OR chain may follow (`UNTIL BF$ = "0" OR
        // AB` ⇒ `(BF$="0") OR AB`). Build the first term, then
        // continue the boolean grammar.
        let s = string_expression(p)?;
        p.skip_spaces();
        let first = if is_compare_op_byte(p.peek()) {
            let op = parse_compare_op(p)?;
            let rhs = string_expression(p)?;
            Expr::StrCompare(op, s, rhs)
        } else {
            Expr::Len(Box::new(s))
        };
        return continue_boolean_expr(p, first);
    }
    // Numeric path: `expression` already routes through or_expr →
    // and_expr → not_expr → cmp_expr, so AND/OR/compares (including a
    // string comparison that surfaces in cmp_expr) all parse here.
    expression(p)
}

/// Continue an AND/OR boolean expression given a pre-parsed first
/// term that's already at the cmp-expr precedence level. AND binds
/// tighter than OR (matches `and_expr` / `or_expr`).
fn continue_boolean_expr(p: &mut Cursor<'_>, first: Expr) -> Result<Expr, ParseError> {
    let mut lhs = first;
    loop {
        p.skip_spaces();
        if p.peek() == Some(TOK_AND) {
            p.advance(1);
            let rhs = not_expr(p)?;
            lhs = Expr::Bin(BinOp::And, Box::new(lhs), Box::new(rhs));
        } else {
            break;
        }
    }
    loop {
        p.skip_spaces();
        if p.peek() == Some(TOK_OR) {
            p.advance(1);
            let rhs = and_expr(p)?;
            lhs = Expr::Bin(BinOp::Or, Box::new(lhs), Box::new(rhs));
        } else {
            break;
        }
    }
    Ok(lhs)
}

fn or_expr(p: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    let mut lhs = and_expr(p)?;
    loop {
        p.skip_spaces();
        if p.peek() == Some(TOK_OR) {
            p.advance(1);
            let rhs = and_expr(p)?;
            lhs = Expr::Bin(BinOp::Or, Box::new(lhs), Box::new(rhs));
        } else {
            break;
        }
    }
    Ok(lhs)
}

fn and_expr(p: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    let mut lhs = not_expr(p)?;
    loop {
        p.skip_spaces();
        if p.peek() == Some(TOK_AND) {
            p.advance(1);
            let rhs = not_expr(p)?;
            lhs = Expr::Bin(BinOp::And, Box::new(lhs), Box::new(rhs));
        } else {
            break;
        }
    }
    Ok(lhs)
}

/// `NOT cmp_expr` or just `cmp_expr`. Recurses into itself so chains
/// like `NOT NOT x` are allowed (and cancel as expected).
fn not_expr(p: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    p.skip_spaces();
    if p.peek() == Some(TOK_NOT) {
        p.advance(1);
        let inner = not_expr(p)?;
        return Ok(Expr::Not(Box::new(inner)));
    }
    cmp_expr(p)
}

fn cmp_expr(p: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    // Detect string comparisons before falling into the numeric path:
    // if the next atom is a string literal or string variable, treat
    // the whole comparison as string-typed.
    p.skip_spaces();
    if peek_is_string_atom(p) {
        let lhs = string_expression(p)?;
        p.skip_spaces();
        // String in numeric context with no comparison operator
        // following is a v2 runtime ?TYPE MISMATCH (e.g. typo'd
        // numeric-array assignment `NM(9)="DEAD PLANT"`). Emit
        // `VAL(string)` so the parse succeeds and the affected
        // line evaluates to a (likely zero) numeric value at run.
        if p.lenient_syntax && !is_compare_op_byte(p.peek()) {
            return Ok(Expr::Val(Box::new(lhs)));
        }
        let op = parse_compare_op(p)?;
        let rhs = string_expression(p)?;
        return Ok(Expr::StrCompare(op, lhs, rhs));
    }
    let mut lhs = add_expr(p)?;
    // BASIC permits 2-token sequences for compound compares. The ROM
    // accepts both orderings symmetrically: `<=` and `=<`, `>=` and
    // `=>`, `<>` and `><`. The tokenizer stores each as a separate
    // token, so we look at up to two. Chained compares parse
    // left-associatively as `(B=A1)=X`.
    loop {
        p.skip_spaces();
        let op = match p.peek() {
            Some(TOK_EQ) => {
                p.advance(1);
                match p.peek() {
                    Some(TOK_GT) => {
                        p.advance(1);
                        BinOp::Ge
                    }
                    Some(TOK_LT) => {
                        p.advance(1);
                        BinOp::Le
                    }
                    _ => BinOp::Eq,
                }
            }
            Some(TOK_LT) => {
                p.advance(1);
                match p.peek() {
                    Some(TOK_EQ) => {
                        p.advance(1);
                        BinOp::Le
                    }
                    Some(TOK_GT) => {
                        p.advance(1);
                        BinOp::Ne
                    }
                    _ => BinOp::Lt,
                }
            }
            Some(TOK_GT) => {
                p.advance(1);
                match p.peek() {
                    Some(TOK_EQ) => {
                        p.advance(1);
                        BinOp::Ge
                    }
                    Some(TOK_LT) => {
                        p.advance(1);
                        BinOp::Ne
                    }
                    _ => BinOp::Gt,
                }
            }
            _ => break,
        };
        let rhs = add_expr(p)?;
        lhs = Expr::Bin(op, Box::new(lhs), Box::new(rhs));
    }
    Ok(lhs)
}

fn add_expr(p: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    let mut lhs = mul_expr(p)?;
    loop {
        p.skip_spaces();
        let op = match p.peek() {
            Some(TOK_PLUS) => BinOp::Add,
            Some(TOK_MINUS) => BinOp::Sub,
            _ => break,
        };
        p.advance(1);
        let rhs = mul_expr(p)?;
        lhs = Expr::Bin(op, Box::new(lhs), Box::new(rhs));
    }
    Ok(lhs)
}

fn mul_expr(p: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    let mut lhs = unary_expr(p)?;
    loop {
        p.skip_spaces();
        let op = match p.peek() {
            Some(TOK_MUL) => BinOp::Mul,
            Some(TOK_DIV) => BinOp::Div,
            _ => break,
        };
        p.advance(1);
        let rhs = unary_expr(p)?;
        lhs = Expr::Bin(op, Box::new(lhs), Box::new(rhs));
    }
    Ok(lhs)
}

fn unary_expr(p: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    p.skip_spaces();
    if p.peek() == Some(TOK_MINUS) {
        p.advance(1);
        let inner = unary_expr(p)?;
        return Ok(Expr::Neg(Box::new(inner)));
    }
    // Unary plus is a no-op in BASIC v2, but real programs do write it
    // — TankAttack's `FOR Z=10 TO 0 STEP +.2` is the canonical case.
    // The interpreter accepts it because PLUS is one of the prefix
    // operators its expression evaluator scans for; we have to do the
    // same or the STEP expression dies with "expected an expression".
    if p.peek() == Some(TOK_PLUS) {
        p.advance(1);
        return unary_expr(p);
    }
    pow_expr(p)
}

/// `^` — left-associative exponentiation, matching C64 BASIC v2's
/// behaviour: `2^3^2` parses as `(2^3)^2 = 64`, not `2^(3^2)`. The RHS
/// of each `^` accepts a leading `-` (so `2^-2` is valid) but does NOT
/// recurse back through the full unary/pow chain — that's what makes
/// the operator left-associative.
fn pow_expr(p: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    let mut lhs = atom(p)?;
    loop {
        p.skip_spaces();
        if p.peek() != Some(TOK_POW) {
            break;
        }
        p.advance(1);
        p.skip_spaces();
        // Allow `2^-3`: pull off any number of unary minuses, then the
        // RHS atom. Each pair cancels, so the boolean toggle is enough.
        let mut neg = false;
        while p.peek() == Some(TOK_MINUS) {
            neg = !neg;
            p.advance(1);
            p.skip_spaces();
        }
        let mut rhs = atom(p)?;
        if neg {
            rhs = Expr::Neg(Box::new(rhs));
        }
        lhs = Expr::Bin(BinOp::Pow, Box::new(lhs), Box::new(rhs));
    }
    Ok(lhs)
}

fn atom(p: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    p.skip_spaces();
    match p.peek() {
        // C64 BASIC v2 stores `π` as a single-byte $FF token. The
        // ROM constant matches the float at $BAEC, with the same
        // 7-digit precision MFLPT carries.
        Some(0xFF) => {
            p.advance(1);
            Ok(Expr::Number(std::f64::consts::PI))
        }
        Some(b'(') => {
            p.advance(1);
            let e = expression(p)?;
            p.skip_spaces();
            if p.peek() != Some(b')') {
                return Err(ParseError::ExpectedKeyword {
                    line: p.line,
                    what: "')'",
                });
            }
            p.advance(1);
            Ok(e)
        }
        Some(b'"') => {
            p.advance(1);
            Ok(Expr::String(p.take_string_body()))
        }
        Some(b'$') => parse_prefixed_int_literal(p, b'$', 16),
        Some(b'%') => parse_prefixed_int_literal(p, b'%', 2),
        Some(b) if b.is_ascii_digit() || b == b'.' => Ok(Expr::Number(p.take_number())),
        Some(b'D' | b'd') if p.peek_at(1) == Some(b'!') => {
            p.advance(2);
            d_bang_peek_expr(p)
        }
        Some(b'M' | b'm') if peek_ascii_command_word(p, b"MEMPEEK") => {
            p.advance(7);
            Ok(Expr::MemPeek(Box::new(paren_expr(p)?)))
        }
        // The BASIC v2 tokeniser eagerly substitutes the literal
        // bytes "PEEK" with `TOK_PEEK` ($C2) anywhere it appears
        // in the source, so a typed `MEMPEEK` lands in the prog
        // as `M E M $C2`. Detect that shape too.
        Some(b'M' | b'm')
            if p.peek_at(1).is_some_and(|b| b == b'E' || b == b'e')
                && p.peek_at(2).is_some_and(|b| b == b'M' || b == b'm')
                && p.peek_at(3) == Some(TOK_PEEK) =>
        {
            p.advance(4);
            Ok(Expr::MemPeek(Box::new(paren_expr(p)?)))
        }
        Some(b'N' | b'n') if peek_ascii_command_word(p, b"NRM") => {
            p.advance(3);
            Ok(Expr::Nrm(Box::new(paren_str_expr(p)?)))
        }
        Some(b'U' | b'u') if peek_ascii_command_word(p, b"USE") => {
            p.advance(3);
            Ok(Expr::Number(8.0))
        }
        Some(b'M' | b'm') if peek_ascii_command_word(p, b"MOD") => {
            p.advance(3);
            tsb_mod_expr(p)
        }
        Some(b'D' | b'd') if peek_ascii_command_word(p, b"DIV") => {
            p.advance(3);
            tsb_div_expr(p)
        }
        Some(b'P' | b'p') if peek_ascii_command_word(p, b"POT") => {
            p.advance(3);
            Ok(Expr::Pot(Box::new(paren_expr(p)?)))
        }
        Some(b'I' | b'i') if peek_ascii_command_word(p, b"INKEY") => {
            p.advance(5);
            Ok(Expr::Inkey)
        }
        Some(b'L' | b'l') if peek_ascii_command_word(p, b"LIN") && p.peek_at(3) == Some(b'(') => {
            p.advance(3);
            consume_empty_call(p, "LIN")?;
            Ok(Expr::Lin)
        }
        Some(b'S' | b's') if peek_ascii_command_word(p, b"SOUND") && p.peek_at(5) == Some(b'(') => {
            p.advance(5);
            consume_empty_call(p, "SOUND")?;
            Ok(Expr::Number(0xD400 as f64))
        }
        Some(b'G' | b'g')
            if peek_ascii_command_word(p, b"GRAPHICS") && p.peek_at(8) == Some(b'(') =>
        {
            p.advance(8);
            consume_empty_call(p, "GRAPHICS")?;
            Ok(Expr::Number(0xD000 as f64))
        }
        Some(b'D' | b'd')
            if peek_ascii_command_word(p, b"DISPLAY") && p.peek_at(7) == Some(b'(') =>
        {
            p.advance(7);
            consume_empty_call(p, "DISPLAY")?;
            Ok(display_base_expr())
        }
        Some(b'P' | b'p') if peek_ascii_command_word(p, b"PLACE") && p.peek_at(5) == Some(b'(') => {
            p.advance(5);
            tsb_place_expr(p)
        }
        // ASCII forms of `AT(...)`, `CHECK(...)`, `INST(...)`
        // are intentionally NOT intercepted here — the names collide
        // with valid BASIC v2 variable identifiers such as `AT(P)` or
        // `ON AT GOTO ...`. Extended functions arrive tokenized via
        // the `TOK_TSB_PREFIX` byte (handled
        // in `tsb_expr_atom`), so the unambiguous form is reachable
        // without breaking the variable-name surface.
        // The extension prefix byte is `$64`, which is also ASCII
        // `d`, so this must be checked before the generic variable arm.
        Some(TOK_TSB_PREFIX) => tsb_expr_atom(p),
        Some(b) if b.is_ascii_alphabetic() => {
            let v = var_name(p)?;
            p.skip_spaces();
            if p.peek() == Some(b'(') {
                p.advance(1);
                let indices = index_list(p)?;
                Ok(Expr::ArrayRef(v, indices))
            } else {
                Ok(Expr::Var(v))
            }
        }
        Some(b) if func1_for_token(b).is_some() => {
            let f = func1_for_token(b).unwrap();
            p.advance(1);
            let arg = paren_expr(p)?;
            Ok(Expr::Func1(f, Box::new(arg)))
        }
        Some(TOK_PEEK) => {
            p.advance(1);
            let arg = paren_expr(p)?;
            Ok(Expr::Peek(Box::new(arg)))
        }
        Some(TOK_POS) => {
            p.advance(1);
            let arg = paren_expr(p)?;
            Ok(Expr::Pos(Box::new(arg)))
        }
        Some(TOK_FRE) => {
            p.advance(1);
            let arg = paren_expr(p)?;
            Ok(Expr::Fre(Box::new(arg)))
        }
        Some(TOK_USR) => {
            p.advance(1);
            let arg = paren_expr(p)?;
            Ok(Expr::Usr(Box::new(arg)))
        }
        Some(TOK_LEN) => {
            p.advance(1);
            let arg = paren_string_expr(p)?;
            Ok(Expr::Len(Box::new(arg)))
        }
        Some(TOK_ASC) => {
            p.advance(1);
            let arg = paren_string_expr(p)?;
            Ok(Expr::Asc(Box::new(arg)))
        }
        Some(TOK_VAL) => {
            p.advance(1);
            let arg = paren_string_expr(p)?;
            Ok(Expr::Val(Box::new(arg)))
        }
        Some(TOK_FN) => {
            p.advance(1);
            let name = fn_name(p)?;
            let arg = paren_expr(p)?;
            Ok(Expr::FnCall(name, Box::new(arg)))
        }
        _ => Err(ParseError::ExpectedExpr { line: p.line }),
    }
}

fn parse_prefixed_int_literal(
    p: &mut Cursor<'_>,
    prefix: u8,
    radix: u32,
) -> Result<Expr, ParseError> {
    debug_assert_eq!(p.peek(), Some(prefix));
    p.advance(1);
    if p.peek() == Some(prefix) {
        p.advance(1);
    }
    let mut value: u32 = 0;
    let mut digits = 0;
    while let Some(b) = p.peek() {
        let digit = match radix {
            16 => match b {
                b'0'..=b'9' => Some((b - b'0') as u32),
                b'A'..=b'F' => Some((b - b'A' + 10) as u32),
                b'a'..=b'f' => Some((b - b'a' + 10) as u32),
                _ => None,
            },
            2 => match b {
                b'0' | b'1' => Some((b - b'0') as u32),
                _ => None,
            },
            _ => unreachable!(),
        };
        let Some(digit) = digit else { break };
        value = value.saturating_mul(radix).saturating_add(digit);
        digits += 1;
        p.advance(1);
    }
    if digits == 0 {
        return Err(ParseError::ExpectedExpr { line: p.line });
    }
    if value > u16::MAX as u32 {
        return Err(ParseError::UnsupportedFeature {
            line: p.line,
            what: "integer literal larger than 16 bits",
        });
    }
    Ok(Expr::Number(value as f64))
}

fn tsb_expr_atom(p: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    debug_assert_eq!(p.peek(), Some(TOK_TSB_PREFIX));
    p.advance(1);
    let Some(token) = p.peek() else {
        return Err(ParseError::ExpectedExpr { line: p.line });
    };
    p.advance(1);
    match normalize_tsb_token(token) {
        TSB_FRAC => {
            let arg = paren_expr(p)?;
            Ok(Expr::Bin(
                BinOp::Sub,
                Box::new(arg.clone()),
                Box::new(Expr::Func1(Func1::Int, Box::new(arg))),
            ))
        }
        TSB_MOD => tsb_mod_expr(p),
        TSB_DIV => tsb_div_expr(p),
        TSB_D_BANG => d_bang_peek_expr(p),
        TSB_INKEY => Ok(Expr::Inkey),
        TSB_PLACE => tsb_place_expr(p),
        TSB_EXOR => {
            p.skip_spaces();
            if p.peek() != Some(b'(') {
                return Err(ParseError::ExpectedKeyword {
                    line: p.line,
                    what: "'(' after EXOR",
                });
            }
            p.advance(1);
            let lhs = expression(p)?;
            expect_comma(p, "',' in EXOR")?;
            let rhs = expression(p)?;
            p.skip_spaces();
            if p.peek() != Some(b')') {
                return Err(ParseError::ExpectedKeyword {
                    line: p.line,
                    what: "')' after EXOR",
                });
            }
            p.advance(1);
            Ok(Expr::Bin(BinOp::Xor, Box::new(lhs), Box::new(rhs)))
        }
        TSB_POT => {
            let arg = paren_expr(p)?;
            Ok(Expr::Pot(Box::new(arg)))
        }
        TSB_PENX => Ok(Expr::Peek(Box::new(Expr::Number(0xD013 as f64)))),
        TSB_PENY => Ok(Expr::Peek(Box::new(Expr::Number(0xD014 as f64)))),
        TSB_LIN => {
            consume_optional_empty_call(p, "LIN")?;
            Ok(Expr::Lin)
        }
        TSB_SOUND => {
            consume_optional_empty_call(p, "SOUND")?;
            Ok(Expr::Number(0xD400 as f64))
        }
        TSB_GRAPHICS => {
            consume_optional_empty_call(p, "GRAPHICS")?;
            Ok(Expr::Number(0xD000 as f64))
        }
        TSB_DISPLAY => {
            consume_optional_empty_call(p, "DISPLAY")?;
            Ok(display_base_expr())
        }
        TSB_MEM => tsb_mem_expr(p),
        TSB_NRM => {
            let arg = paren_str_expr(p)?;
            Ok(Expr::Nrm(Box::new(arg)))
        }
        // `USE` as expression returns the current default
        // drive number. Compiled programs can't really track a
        // mutable "current drive" state (we don't implement
        // `USE n` as a runtime statement), so hardcode the
        // canonical 8 — every `I=USE` reader gets a plausible
        // value and the program's `LOAD` paths still target the
        // canonical drive.
        TSB_USE => Ok(Expr::Number(8.0)),
        TSB_AT => tsb_at_expr(p),
        TSB_TEST => tsb_test_expr(p),
        TSB_CHECK => tsb_check_expr(p),
        TSB_INST => tsb_inst_expr(p),
        TSB_JOY => {
            let arg = paren_expr(p)?;
            Ok(Expr::Joy(Box::new(arg)))
        }
        TSB_ERR => {
            // ERR pseudo-variable in expression context. Tokenized
            // ERR and bare `ER` converge on the same IR shape.
            Ok(Expr::Var(VarName {
                base: "ER".to_string(),
                kind: VarKind::Float,
            }))
        }
        other => unsupported_tsb_expr(p.line, other),
    }
}

fn tsb_mem_expr(p: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    p.skip_spaces();
    if p.peek() == Some(TOK_PEEK) {
        p.advance(1);
        return Ok(Expr::MemPeek(Box::new(paren_expr(p)?)));
    }
    if consume_ascii_command_word(p, b"PEEK") {
        return Ok(Expr::MemPeek(Box::new(paren_expr(p)?)));
    }
    Err(ParseError::Unsupported {
        line: p.line,
        name: "MEM",
    })
}

fn consume_optional_lparen(p: &mut Cursor<'_>) -> bool {
    p.skip_spaces();
    if p.peek() == Some(b'(') {
        p.advance(1);
        true
    } else {
        false
    }
}

fn expect_rparen(p: &mut Cursor<'_>, context: &'static str) -> Result<(), ParseError> {
    p.skip_spaces();
    if p.peek() != Some(b')') {
        return Err(ParseError::ExpectedKeyword {
            line: p.line,
            what: context,
        });
    }
    p.advance(1);
    Ok(())
}

fn consume_empty_call(p: &mut Cursor<'_>, name: &'static str) -> Result<(), ParseError> {
    p.skip_spaces();
    if p.peek() != Some(b'(') {
        return Err(ParseError::ExpectedKeyword {
            line: p.line,
            what: "'(' after zero-argument function",
        });
    }
    p.advance(1);
    expect_rparen(
        p,
        match name {
            "LIN" => "')' after LIN",
            "SOUND" => "')' after SOUND",
            "GRAPHICS" => "')' after GRAPHICS",
            "DISPLAY" => "')' after DISPLAY",
            _ => "')' after function",
        },
    )
}

fn consume_optional_empty_call(p: &mut Cursor<'_>, name: &'static str) -> Result<(), ParseError> {
    p.skip_spaces();
    if p.peek() == Some(b'(') {
        consume_empty_call(p, name)?;
    }
    Ok(())
}

fn display_base_expr() -> Expr {
    Expr::Bin(
        BinOp::Mul,
        Box::new(Expr::Peek(Box::new(Expr::Number(648.0)))),
        Box::new(Expr::Number(256.0)),
    )
}

fn tsb_place_expr(p: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    p.skip_spaces();
    if p.peek() != Some(b'(') {
        return Err(ParseError::ExpectedKeyword {
            line: p.line,
            what: "'(' after PLACE",
        });
    }
    p.advance(1);
    let needle = string_expression(p)?;
    expect_comma(p, "',' in PLACE")?;
    let haystack = string_expression(p)?;
    expect_rparen(p, "')' after PLACE")?;
    Ok(Expr::Inst {
        haystack: Box::new(haystack),
        needle: Box::new(needle),
        start: Some(Box::new(Expr::Number(1.0))),
    })
}

fn tsb_at_expr(p: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    // tokenizes this as AT( in some toolchains, while ASCII
    // source naturally has a literal '(' after AT. Accept both forms.
    let _had_lparen = consume_optional_lparen(p);
    let row = expression(p)?;
    expect_comma(p, "',' in AT")?;
    let col = expression(p)?;
    expect_rparen(p, "')' after AT")?;
    Ok(Expr::At(Box::new(row), Box::new(col)))
}

fn tsb_test_expr(p: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    // `TEST(x, y)` — pixel sense in the HIRES bitmap.
    let _had_lparen = consume_optional_lparen(p);
    let x = expression(p)?;
    expect_comma(p, "',' in TEST")?;
    let y = expression(p)?;
    expect_rparen(p, "')' after TEST")?;
    Ok(Expr::Test(Box::new(x), Box::new(y)))
}

fn tsb_check_expr(p: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    let _had_lparen = consume_optional_lparen(p);
    let first = expression(p)?;
    p.skip_spaces();
    let second = if p.peek() == Some(TOK_COMMA) {
        p.advance(1);
        Some(Box::new(expression(p)?))
    } else {
        None
    };
    expect_rparen(p, "')' after CHECK")?;
    Ok(Expr::Check {
        first: Box::new(first),
        second,
    })
}

fn tsb_inst_expr(p: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    p.skip_spaces();
    if p.peek() != Some(b'(') {
        return Err(ParseError::ExpectedKeyword {
            line: p.line,
            what: "'(' after INST",
        });
    }
    p.advance(1);
    let haystack = string_expression(p)?;
    expect_comma(p, "',' in INST")?;
    let needle = string_expression(p)?;
    p.skip_spaces();
    let start = if p.peek() == Some(TOK_COMMA) {
        p.advance(1);
        Some(Box::new(expression(p)?))
    } else {
        None
    };
    expect_rparen(p, "')' after INST")?;
    Ok(Expr::Inst {
        haystack: Box::new(haystack),
        needle: Box::new(needle),
        start,
    })
}

fn tsb_binary_expr_args(
    p: &mut Cursor<'_>,
    name: &'static str,
) -> Result<(Expr, Expr), ParseError> {
    p.skip_spaces();
    if p.peek() != Some(b'(') {
        return Err(ParseError::ExpectedKeyword {
            line: p.line,
            what: "'(' after binary function",
        });
    }
    p.advance(1);
    let lhs = expression(p)?;
    expect_comma(p, name)?;
    let rhs = expression(p)?;
    p.skip_spaces();
    if p.peek() != Some(b')') {
        return Err(ParseError::ExpectedKeyword {
            line: p.line,
            what: "')' after binary function",
        });
    }
    p.advance(1);
    Ok((lhs, rhs))
}

fn tsb_div_expr(p: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    let (lhs, rhs) = tsb_binary_expr_args(p, "',' in DIV")?;
    Ok(Expr::Func1(
        Func1::Int,
        Box::new(Expr::Bin(BinOp::Div, Box::new(lhs), Box::new(rhs))),
    ))
}

fn tsb_mod_expr(p: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    let (lhs, rhs) = tsb_binary_expr_args(p, "',' in MOD")?;
    let quotient = Expr::Func1(
        Func1::Int,
        Box::new(Expr::Bin(
            BinOp::Div,
            Box::new(lhs.clone()),
            Box::new(rhs.clone()),
        )),
    );
    Ok(Expr::Bin(
        BinOp::Sub,
        Box::new(lhs),
        Box::new(Expr::Bin(BinOp::Mul, Box::new(rhs), Box::new(quotient))),
    ))
}

fn d_bang_peek_expr(p: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    p.skip_spaces();
    if p.peek() != Some(TOK_PEEK) {
        return Err(ParseError::ExpectedKeyword {
            line: p.line,
            what: "PEEK after D!",
        });
    }
    p.advance(1);
    let addr = paren_expr(p)?;
    let hi_addr = Expr::Bin(
        BinOp::Add,
        Box::new(addr.clone()),
        Box::new(Expr::Number(1.0)),
    );
    Ok(Expr::Bin(
        BinOp::Add,
        Box::new(Expr::Peek(Box::new(addr))),
        Box::new(Expr::Bin(
            BinOp::Mul,
            Box::new(Expr::Number(256.0)),
            Box::new(Expr::Peek(Box::new(hi_addr))),
        )),
    ))
}

/// Scan past one string atom starting at `i` (assumes
/// `peek_is_string_atom` already returned true). Returns the byte
/// index immediately after the atom. Conservative: handles literals
/// (`"..."`), `var$` references, and the simple string-builtin
/// tokens by stepping past their token byte plus a parenthesised
/// argument list. Falls back to returning `i` for shapes it can't
/// quickly skip — callers should treat that as "don't know".
fn skip_string_atom(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i] == b' ' {
        i += 1;
    }
    if i >= bytes.len() {
        return i;
    }
    match bytes[i] {
        b'"' => {
            i += 1;
            while i < bytes.len() && bytes[i] != b'"' && bytes[i] != 0 {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'"' {
                i += 1;
            }
            i
        }
        // Built-in string function tokens are followed by a
        // parenthesised argument list. Skip balanced parens.
        TOK_CHR | TOK_STR | TOK_LEFT | TOK_RIGHT | TOK_MID => {
            i += 1;
            while i < bytes.len() && bytes[i] == b' ' {
                i += 1;
            }
            if i >= bytes.len() || bytes[i] != b'(' {
                return i;
            }
            let mut depth = 0;
            while i < bytes.len() {
                match bytes[i] {
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            return i + 1;
                        }
                    }
                    b'"' => {
                        i += 1;
                        while i < bytes.len() && bytes[i] != b'"' && bytes[i] != 0 {
                            i += 1;
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            i
        }
        b if b.is_ascii_alphabetic() => {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_alphanumeric() {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'$' {
                i += 1;
            }
            i
        }
        _ => i,
    }
}

/// True if the next atom is a string-typed thing AND it's followed
/// by a comparison operator. Used by `print_stmt` to recognise
/// `PRINT A$=B$` / `PRINT "X"<"Y"` shapes — these belong on the
/// numeric expression path (string compare yields -1/0) rather than
/// the string-print path (which would consume only the leading
/// atom and choke on the operator).
fn peek_is_string_compare(p: &Cursor<'_>) -> bool {
    if !peek_is_string_atom(p) {
        return false;
    }
    let mut i = skip_string_atom(p.bytes, p.pos);
    while i < p.bytes.len() && p.bytes[i] == b' ' {
        i += 1;
    }
    if i >= p.bytes.len() {
        return false;
    }
    matches!(p.bytes[i], b'=' | b'<' | b'>' | TOK_EQ | TOK_LT | TOK_GT)
}

/// Used by `print_stmt` to detect the `+` <string-atom> shape that
/// comes from `PRINT TAB(14)+"X"+A$(I)`. BASIC v2's PRINT loop runs
/// FRMEVL after TAB/SPC, and FRMEVL silently swallows unary `+`
/// before a string atom — the program then concats normally.
fn peek_string_atom_after_plus(p: &Cursor<'_>) -> bool {
    debug_assert_eq!(p.peek(), Some(TOK_PLUS));
    peek_is_string_atom_at(p.bytes, p.pos + 1)
}

/// True if the next atom is a string-typed thing (literal, string
/// variable, CHR$). Used by `cmp_expr` to dispatch to string-compare
/// before committing to the numeric expression grammar.
fn peek_is_string_atom(p: &Cursor<'_>) -> bool {
    peek_is_string_atom_at(p.bytes, p.pos)
}

fn peek_is_string_atom_at(bytes: &[u8], start: usize) -> bool {
    let mut i = start;
    while i < bytes.len() && bytes[i] == b' ' {
        i += 1;
    }
    if i >= bytes.len() {
        return false;
    }
    // Anything that produces a string: literal, or any of the
    // string-typed function tokens.
    match bytes[i] {
        b'"' | TOK_CHR | TOK_STR | TOK_LEFT | TOK_RIGHT | TOK_MID => return true,
        TOK_TSB_PREFIX => {
            return bytes
                .get(i + 1)
                .copied()
                .map(normalize_tsb_token)
                .is_some_and(|t| matches!(t, TSB_DUP | TSB_INSERT | TSB_INST));
        }
        // `$$<expr>` / `%%<expr>` produce strings.
        b'$' if bytes.get(i + 1) == Some(&b'$') => return true,
        b'%' if bytes.get(i + 1) == Some(&b'%') => return true,
        _ => {}
    }
    if i + 3 <= bytes.len()
        && bytes[i..].len() >= 3
        && bytes[i..i + 3]
            .iter()
            .zip(b"DUP".iter())
            .all(|(a, b)| a.to_ascii_uppercase() == *b)
        && bytes
            .get(i + 3)
            .is_none_or(|b| !b.is_ascii_alphanumeric() && !matches!(b, b'$' | b'%'))
    {
        return true;
    }
    if i + 6 <= bytes.len()
        && bytes[i..i + 6]
            .iter()
            .zip(b"INSERT".iter())
            .all(|(a, b)| a.to_ascii_uppercase() == *b)
        && bytes
            .get(i + 6)
            .is_none_or(|b| !b.is_ascii_alphanumeric() && !matches!(b, b'$' | b'%'))
    {
        return true;
    }
    if bytes[i].is_ascii_alphabetic() {
        // Inline the alpha-then-$ check at offset `i`.
        let mut j = i + 1;
        while j < bytes.len() && bytes[j].is_ascii_alphanumeric() {
            j += 1;
        }
        return j < bytes.len() && bytes[j] == b'$';
    }
    false
}

fn is_compare_op_byte(b: Option<u8>) -> bool {
    matches!(b, Some(TOK_EQ | TOK_LT | TOK_GT))
}

/// Parse one of `=`, `<>`, `<`, `<=`, `>`, `>=` as a `BinOp`. ROM
/// also accepts `=<`, `=>`, `><` as synonyms — handled here.
fn parse_compare_op(p: &mut Cursor<'_>) -> Result<BinOp, ParseError> {
    p.skip_spaces();
    match p.peek() {
        Some(TOK_EQ) => {
            p.advance(1);
            match p.peek() {
                Some(TOK_GT) => {
                    p.advance(1);
                    Ok(BinOp::Ge)
                }
                Some(TOK_LT) => {
                    p.advance(1);
                    Ok(BinOp::Le)
                }
                _ => Ok(BinOp::Eq),
            }
        }
        Some(TOK_LT) => {
            p.advance(1);
            match p.peek() {
                Some(TOK_EQ) => {
                    p.advance(1);
                    Ok(BinOp::Le)
                }
                Some(TOK_GT) => {
                    p.advance(1);
                    Ok(BinOp::Ne)
                }
                _ => Ok(BinOp::Lt),
            }
        }
        Some(TOK_GT) => {
            p.advance(1);
            match p.peek() {
                Some(TOK_EQ) => {
                    p.advance(1);
                    Ok(BinOp::Ge)
                }
                Some(TOK_LT) => {
                    p.advance(1);
                    Ok(BinOp::Ne)
                }
                _ => Ok(BinOp::Gt),
            }
        }
        _ => Err(ParseError::ExpectedKeyword {
            line: p.line,
            what: "comparison operator",
        }),
    }
}

/// Look ahead from `p`'s current position to see whether the next
/// token sequence is `<alpha>[<alphanum>...]$` — i.e. a string-variable
/// reference. Used to decide whether PRINT should parse a string item
/// or fall back to numeric expression. Pure inspection: no advance.
fn peek_is_string_var(p: &Cursor<'_>) -> bool {
    let mut i = p.pos;
    let bytes = p.bytes;
    if i >= bytes.len() || !bytes[i].is_ascii_alphabetic() {
        return false;
    }
    i += 1;
    while i < bytes.len() && bytes[i].is_ascii_alphanumeric() {
        i += 1;
    }
    i < bytes.len() && bytes[i] == b'$'
}

/// Parse `(expr)` — common to all function-call forms (FUNC(arg) and PEEK(arg)).
/// Caller has already consumed the function token.
fn paren_expr(p: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    p.skip_spaces();
    if p.peek() != Some(b'(') {
        return Err(ParseError::ExpectedKeyword {
            line: p.line,
            what: "'(' after function",
        });
    }
    p.advance(1);
    let e = expression(p)?;
    p.skip_spaces();
    if p.peek() != Some(b')') {
        return Err(ParseError::ExpectedKeyword {
            line: p.line,
            what: "')'",
        });
    }
    p.advance(1);
    Ok(e)
}

/// Like `paren_expr` but the parenthesised body is a string
/// expression rather than a numeric one. Used by NRM(s$) and
/// similar extension-style functions that take a string argument
/// and return a number.
fn paren_str_expr(p: &mut Cursor<'_>) -> Result<StrExpr, ParseError> {
    p.skip_spaces();
    if p.peek() != Some(b'(') {
        return Err(ParseError::ExpectedKeyword {
            line: p.line,
            what: "'(' after function",
        });
    }
    p.advance(1);
    let s = string_expression(p)?;
    p.skip_spaces();
    if p.peek() != Some(b')') {
        return Err(ParseError::ExpectedKeyword {
            line: p.line,
            what: "')'",
        });
    }
    p.advance(1);
    Ok(s)
}

/// Like `paren_expr` but assumes the opening `(` has already been
/// consumed — used after tokens like `TAB(` and `SPC(` whose byte
/// representation includes the open-paren.
fn baked_paren_expr(p: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    let e = expression(p)?;
    p.skip_spaces();
    if p.peek() != Some(b')') {
        return Err(ParseError::ExpectedKeyword {
            line: p.line,
            what: "')'",
        });
    }
    p.advance(1);
    Ok(e)
}

/// Like `paren_expr` but expects a string-typed argument inside.
fn paren_string_expr(p: &mut Cursor<'_>) -> Result<StrExpr, ParseError> {
    p.skip_spaces();
    if p.peek() != Some(b'(') {
        return Err(ParseError::ExpectedKeyword {
            line: p.line,
            what: "'(' after function",
        });
    }
    p.advance(1);
    let e = string_expression(p)?;
    p.skip_spaces();
    if p.peek() != Some(b')') {
        return Err(ParseError::ExpectedKeyword {
            line: p.line,
            what: "')'",
        });
    }
    p.advance(1);
    Ok(e)
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
    line: u16,
    /// Accept obvious source typos that v2 would have errored on at
    /// runtime — e.g. `GOT1200` (=> GOTO 1200) or `CLOSE n, sa, dev`.
    /// Off by default so the parser refuses to compile broken code;
    /// the CLI/GUI surface a `--lenient-syntax` opt-in for the cases
    /// where the user knows the broken line never executes.
    lenient_syntax: bool,
    /// Base names that REM hints have declared as integer. `var_name`
    /// consults this to rewrite Float-kind vars whose base matches.
    int_hint_vars: &'a std::collections::HashSet<String>,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8], line: u16) -> Self {
        static EMPTY: std::sync::OnceLock<std::collections::HashSet<String>> =
            std::sync::OnceLock::new();
        Self {
            bytes,
            pos: 0,
            line,
            lenient_syntax: false,
            int_hint_vars: EMPTY.get_or_init(std::collections::HashSet::new),
        }
    }
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }
    fn peek_at(&self, offset: usize) -> Option<u8> {
        self.bytes.get(self.pos + offset).copied()
    }
    fn advance(&mut self, n: usize) {
        self.pos += n;
    }
    fn eof(&self) -> bool {
        self.pos >= self.bytes.len()
    }
    fn skip_spaces(&mut self) {
        while matches!(self.peek(), Some(b' ')) {
            self.pos += 1;
        }
    }
    fn take_rest(&mut self) -> &'a [u8] {
        let s = &self.bytes[self.pos..];
        self.pos = self.bytes.len();
        s
    }
    fn take_until_statement_end(&mut self) -> &'a [u8] {
        let start = self.pos;
        while let Some(b) = self.peek() {
            if b == b':' || b == b';' {
                break;
            }
            self.pos += 1;
        }
        &self.bytes[start..self.pos]
    }
    /// Caller has already consumed the opening `"`. Reads bytes until the
    /// closing `"` or end of line. Tokenized BASIC permits a string to run
    /// to EOL without a closing quote — we accept that silently.
    fn take_string_body(&mut self) -> Vec<u8> {
        let start = self.pos;
        while let Some(b) = self.peek() {
            if b == b'"' {
                let v = self.bytes[start..self.pos].to_vec();
                self.pos += 1; // consume closing quote
                return v;
            }
            self.pos += 1;
        }
        self.bytes[start..].to_vec()
    }
    fn take_number(&mut self) -> f64 {
        let start = self.pos;
        while let Some(b) = self.peek() {
            if b.is_ascii_digit() || b == b'.' {
                self.pos += 1;
            } else {
                break;
            }
        }
        // Optional scientific exponent: `E[+-]?digits` (case-insensitive).
        // Only consume the `E` when the exponent looks well-formed, else
        // leave it for the parser to handle as a separate token (matters
        // for stuck-together statements like `100E=...`). Tokenized
        // sources may carry the sign as TOK_PLUS/TOK_MINUS, so accept
        // both raw ASCII and tokenized forms.
        if matches!(self.peek(), Some(b'E') | Some(b'e')) {
            let exp_start = self.pos;
            self.pos += 1;
            // Splice over a token-form sign by stitching the literal
            // character back into the string we hand to f64::parse.
            let mut sign_char: Option<u8> = None;
            match self.peek() {
                Some(b'+') | Some(b'-') => {
                    self.pos += 1;
                }
                Some(TOK_PLUS) => {
                    sign_char = Some(b'+');
                    self.pos += 1;
                }
                Some(TOK_MINUS) => {
                    sign_char = Some(b'-');
                    self.pos += 1;
                }
                _ => {}
            }
            if matches!(self.peek(), Some(b) if b.is_ascii_digit()) {
                while let Some(b) = self.peek() {
                    if b.is_ascii_digit() {
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
                if let Some(s) = sign_char {
                    let mut buf = self.bytes[start..self.pos].to_vec();
                    // Replace the tokenised sign byte with its literal
                    // form so std::f64::from_str sees `2.5E-3` instead
                    // of `2.5E\xAB3`. The byte position to overwrite
                    // is `start + (exp_start - start) + 1` (the byte
                    // immediately after the `E`).
                    let sign_idx = (exp_start - start) + 1;
                    buf[sign_idx] = s;
                    return std::str::from_utf8(&buf)
                        .expect("ASCII")
                        .parse()
                        .unwrap_or(0.0);
                }
            } else {
                // Exponent never materialised — back off so the `E` stays
                // available as the next token.
                self.pos = exp_start;
            }
        }
        // Tokenized BASIC stores numbers as ASCII so parse always succeeds.
        std::str::from_utf8(&self.bytes[start..self.pos])
            .expect("ASCII")
            .parse()
            .unwrap_or(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn if_accepts_bare_goto_without_then() {
        let stmts = line_body(10, &[TOK_IF, b'1', b' ', TOK_GOTO, b'2', b'0']).unwrap();
        match &stmts[0] {
            Statement::If {
                then_branch: ThenBranch::Goto(20),
                ..
            } => {}
            other => panic!("unexpected parse result: {other:?}"),
        }
    }

    #[test]
    fn if_accepts_ascii_th_abbreviation_at_end_of_line() {
        let stmts = line_body(306, &[TOK_IF, b'1', b' ', b'T', b'H']).unwrap();
        match &stmts[0] {
            Statement::If {
                then_branch: ThenBranch::Stmts(inner),
                ..
            } => {
                assert!(inner.is_empty());
            }
            other => panic!("unexpected parse result: {other:?}"),
        }
    }

    /// `IF <false> THEN <BASIC 7.0 keyword>:RETURN` compiles away
    /// rather than failing the build, the trailing `RETURN` goes with
    /// the conditional it belonged to, and the drop is reported.
    #[test]
    fn unsupported_token_in_conditional_is_dropped_and_reported() {
        let mut skipped = Vec::new();
        // 10 IF 1 THEN <$FE $1D = SPRDEF> 1:RETURN
        let body = [
            TOK_IF, b'1', b' ', TOK_THEN, 0xFE, 0x1D, b'1', b':', TOK_RETURN,
        ];
        let stmts =
            line_body_with_options(10, &body, &ParseOptions::default(), &mut skipped).unwrap();
        assert!(
            !stmts.iter().any(|s| matches!(s, Statement::Return)),
            "THEN body escaped the conditional: {stmts:?}"
        );
        assert_eq!(skipped.len(), 1, "{skipped:?}");
        assert_eq!(skipped[0].token, 0xFE);
        assert!(skipped[0].whole_conditional);
    }

    #[test]
    fn trailing_bare_goto_is_accepted_as_empty_statement() {
        let stmts = line_body(3250, &[TOK_PRINT, b'"', b'X', b'"', b':', TOK_GOTO]).unwrap();
        assert_eq!(stmts.len(), 2);
        assert!(matches!(stmts[1], Statement::Rem(_)));
    }

    #[test]
    fn gosub_ignores_baked_label_after_target() {
        // `GOSUB 500DEFGFX` — the tokeniser turns the leading `DEF` of
        // the label into a token byte; v2 reads the 500 and ignores the
        // rest, so it must not be parsed as a `DEF` statement.
        let stmts = line_body(10, &[TOK_GOSUB, b'5', b'0', b'0', TOK_DEF, b'G', b'F', b'X']).unwrap();
        assert!(matches!(stmts.as_slice(), [Statement::GoSub(500)]));
    }

    #[test]
    fn gosub_label_stops_at_colon() {
        // The label is dropped but a `:`-separated statement still runs
        // when the subroutine returns.
        let stmts = line_body(
            10,
            &[
                TOK_GOSUB, b'5', b'0', b'0', TOK_DEF, b'G', b'F', b'X', b':', TOK_PRINT, b'"', b'X',
                b'"',
            ],
        )
        .unwrap();
        assert_eq!(stmts.len(), 2);
        assert!(matches!(stmts[0], Statement::GoSub(500)));
        assert!(matches!(stmts[1], Statement::Print(_)));
    }

    #[test]
    fn gosub_drops_trailing_text_to_end_of_line() {
        // v2 ignores everything after the line number, even when it would
        // tokenise to a valid statement; only `GOSUB 10000` survives.
        let stmts =
            line_body(10, &[TOK_GOSUB, b'1', b'0', b'0', b'0', b'0', TOK_PRINT, b'"', b'X', b'"'])
                .unwrap();
        assert!(matches!(stmts.as_slice(), [Statement::GoSub(10000)]));
    }

    #[test]
    fn gosub_label_inside_then_body() {
        let stmts = line_body(
            10,
            &[
                TOK_IF, b'1', b' ', TOK_THEN, TOK_GOSUB, b'5', b'0', b'0', TOK_DEF, b'G', b'F', b'X',
            ],
        )
        .unwrap();
        match &stmts[0] {
            Statement::If {
                then_branch: ThenBranch::Stmts(inner),
                ..
            } => assert!(matches!(inner.as_slice(), [Statement::GoSub(500)])),
            other => panic!("unexpected parse result: {other:?}"),
        }
    }

    #[test]
    fn on_gosub_ignores_label_after_last_target() {
        let stmts = line_body(
            10,
            &[
                TOK_ON, b'A', b' ', TOK_GOSUB, b'1', b'0', b'0', TOK_DEF, b'G', b'F', b'X',
            ],
        )
        .unwrap();
        match &stmts[0] {
            Statement::OnBranch { targets, kind, .. } => {
                assert_eq!(targets.as_slice(), &[100]);
                assert!(matches!(kind, OnBranchKind::GoSub));
            }
            other => panic!("unexpected parse result: {other:?}"),
        }
    }

    #[test]
    fn tsb_if_parses_inline_else() {
        let stmts = line_body(
            10,
            &[
                TOK_IF,
                b'1',
                b' ',
                TOK_THEN,
                TOK_PRINT,
                b'"',
                b'Y',
                b'"',
                TOK_TSB_PREFIX,
                TSB_ELSE,
                TOK_PRINT,
                b'"',
                b'N',
                b'"',
            ],
        )
        .unwrap();
        match &stmts[0] {
            Statement::IfElse {
                then_branch,
                else_branch,
                ..
            } => {
                assert!(matches!(then_branch, ThenBranch::Stmts(inner) if inner.len() == 1));
                assert!(matches!(else_branch, ThenBranch::Stmts(inner) if inner.len() == 1));
            }
            other => panic!("unexpected parse result: {other:?}"),
        }
    }

    #[test]
    fn tsb_do_done_and_do_null_are_distinct() {
        let done = line_body(10, &[TOK_TSB_PREFIX, TSB_DO, b'N', b'E']).unwrap();
        assert!(matches!(done.as_slice(), [Statement::Done]));

        let do_null =
            line_body(20, &[TOK_TSB_PREFIX, TSB_DO, b' ', b'N', b'U', b'L', b'L']).unwrap();
        assert!(matches!(do_null.as_slice(), [Statement::DoNull]));

        let plain = line_body(30, &[TOK_TSB_PREFIX, TSB_DO]).unwrap();
        assert!(matches!(plain.as_slice(), [Statement::Do]));
    }

    #[test]
    fn tsb_if_then_do_starts_do_block() {
        let stmts = line_body(10, &[TOK_IF, b'1', b' ', TOK_THEN, TOK_TSB_PREFIX, TSB_DO]).unwrap();
        assert!(matches!(stmts.as_slice(), [Statement::DoIf { .. }]));
    }

    #[test]
    fn tsb_repeat_until_and_exit_parse() {
        let stmts = line_body(
            10,
            &[
                TOK_TSB_PREFIX,
                TSB_REPEAT,
                b':',
                TOK_TSB_PREFIX,
                TSB_EXIT,
                b' ',
                TOK_IF,
                b'1',
                b':',
                TOK_TSB_PREFIX,
                TSB_UNTIL,
                b'0',
            ],
        )
        .unwrap();
        assert!(matches!(
            stmts.as_slice(),
            [
                Statement::Repeat,
                Statement::ExitLoop { cond: Some(_) },
                Statement::Until { .. }
            ]
        ));
    }

    #[test]
    fn tsb_low_hanging_commands_parse() {
        let stmts = line_body(
            10,
            &[
                TOK_TSB_PREFIX,
                TSB_CLS,
                b':',
                TOK_TSB_PREFIX,
                TSB_D_BANG,
                TOK_POKE,
                b'1',
                b'0',
                b'2',
                b'4',
                b',',
                b'5',
                b'1',
                b'3',
                b':',
                TOK_TSB_PREFIX,
                TSB_COLOR,
                b'6',
                b',',
                b'0',
                b',',
                b'1',
                b':',
                TOK_TSB_PREFIX,
                TSB_MOB,
                b'1',
                b' ',
                TOK_ON,
                b':',
                TOK_TSB_PREFIX,
                TSB_CMOB,
                b'2',
                b',',
                b'3',
                b':',
                TOK_TSB_PREFIX,
                TSB_PAUSE,
                b'5',
            ],
        )
        .unwrap();
        assert!(matches!(stmts[0], Statement::Print(_)));
        assert!(matches!(stmts[1], Statement::Dpoke { .. }));
        assert!(matches!(
            stmts[2],
            Statement::Color {
                border: Some(_),
                background: Some(_),
                pen: Some(_)
            }
        ));
        assert!(matches!(
            stmts[3],
            Statement::MobEnable { enabled: true, .. }
        ));
        assert!(matches!(stmts[4], Statement::Cmob { .. }));
        assert!(matches!(stmts[5], Statement::Pause { .. }));
    }

    /// `COLOR` accepts any subset of (border, background, pen) by
    /// position — leading or middle commas leave the corresponding
    /// slot at `None`. Used at runtime to keep that channel at its
    /// previous value. Regression: the older parser treated
    /// `COLOR ,X` as "set pen=X" (consuming the only expression
    /// into the wrong slot) and choked on `COLOR ,X,Y` entirely.
    #[test]
    fn tsb_color_with_omitted_args_parses() {
        // Helper: build a one-line program "10 COLOR <bytes>" and
        // return the resulting Color statement.
        let parse_color = |suffix: &[u8]| -> Statement {
            let mut bytes = vec![TOK_TSB_PREFIX, TSB_COLOR];
            bytes.extend_from_slice(suffix);
            let stmts = line_body(10, &bytes).unwrap();
            assert_eq!(stmts.len(), 1, "single statement expected");
            stmts.into_iter().next().unwrap()
        };

        // `COLOR ,5` is shorthand for "set pen only".
        match parse_color(b",5") {
            Statement::Color {
                border: None,
                background: None,
                pen: Some(_),
            } => {}
            other => panic!("expected ,5 to parse as pen only, got {other:?}"),
        }
        // `COLOR ,,5` — same shorthand with one extra comma before the pen.
        match parse_color(b",,5") {
            Statement::Color {
                border: None,
                background: None,
                pen: Some(_),
            } => {}
            other => panic!("expected ,,5 to parse as pen only, got {other:?}"),
        }
        // `COLOR 1,2` — border + background, no pen.
        match parse_color(b"1,2") {
            Statement::Color {
                border: Some(_),
                background: Some(_),
                pen: None,
            } => {}
            other => panic!("expected 1,2 to parse as border+bg, got {other:?}"),
        }
        // `COLOR 1,,3` — border + pen, no background.
        match parse_color(b"1,,3") {
            Statement::Color {
                border: Some(_),
                background: None,
                pen: Some(_),
            } => {}
            other => panic!("expected 1,,3 to parse as border+pen, got {other:?}"),
        }
    }

    #[test]
    fn tsb_graphics_extra_args_parse() {
        let stmts = line_body(
            10,
            &[
                TOK_TSB_PREFIX,
                TSB_LINE,
                b'0',
                b',',
                b'0',
                b',',
                b'1',
                b'0',
                b',',
                b'1',
                b'9',
                b'9',
                b',',
                b'2',
                b':',
                TOK_TSB_PREFIX,
                TSB_DRAW,
                TOK_TO,
                b'1',
                b',',
                b'2',
                b',',
                b'1',
                b':',
                TOK_TSB_PREFIX,
                TSB_ROT,
                b'3',
                b',',
                b'4',
                b':',
                TOK_TSB_PREFIX,
                TSB_ANGL,
                b'1',
                b'0',
                b',',
                b'2',
                b'0',
                b',',
                b'9',
                b'0',
                b',',
                b'3',
                b'0',
                b',',
                b'4',
                b'0',
                b',',
                b'1',
            ],
        )
        .unwrap();

        assert!(matches!(stmts[0], Statement::Line { .. }));
        assert!(matches!(stmts[1], Statement::DrawTo { .. }));
        assert!(matches!(stmts[2], Statement::Rot { .. }));
        assert!(matches!(stmts[3], Statement::Angl { .. }));

        let stmts_ds = line_body(
            20,
            &[
                TOK_TSB_PREFIX,
                TSB_DRAW,
                b'"',
                b'R',
                b'"',
                b',',
                b'1',
                b',',
                b'2',
            ],
        )
        .unwrap();
        assert!(matches!(stmts_ds[0], Statement::DrawString { .. }));
    }

    #[test]
    fn tsb_low_hanging_ascii_aliases_parse() {
        let stmts = line_body(
            10,
            &[
                b'C',
                b'L',
                b'S',
                b':',
                b'C',
                b'O',
                b'L',
                TOK_OR,
                b'6',
                b',',
                b'0',
                b',',
                b'1',
                b':',
                b'M',
                b'O',
                b'B',
                b'C',
                b'O',
                b'L',
                b' ',
                b'1',
                b',',
                b'2',
                b':',
                b'D',
                b'!',
                TOK_POKE,
                b'1',
                b'0',
                b'2',
                b'4',
                b',',
                b'5',
                b'1',
                b'3',
                b':',
                b'M',
                b'O',
                b'B',
                b' ',
                b'1',
                b' ',
                TOK_TSB_PREFIX,
                TSB_OFF,
                b':',
                b'V',
                b'O',
                b'L',
                b' ',
                b'5',
            ],
        )
        .unwrap();
        assert!(matches!(stmts[0], Statement::Print(_)));
        assert!(matches!(
            stmts[1],
            Statement::Color {
                border: Some(_),
                background: Some(_),
                pen: Some(_)
            }
        ));
        assert!(matches!(stmts[2], Statement::Poke { .. }));
        assert!(matches!(stmts[3], Statement::Dpoke { .. }));
        assert!(matches!(
            stmts[4],
            Statement::MobEnable { enabled: false, .. }
        ));
        assert!(matches!(stmts[5], Statement::Poke { .. }));

        let stmts = line_body(
            20,
            &[
                TOK_PRINT, b'D', b'!', TOK_PEEK, b'(', b'1', b'0', b'2', b'4', b')',
            ],
        )
        .unwrap();
        let Statement::Print(print) = &stmts[0] else {
            panic!("expected PRINT");
        };
        assert!(matches!(print.items[0], PrintItem::Expr(Expr::Bin(..))));
    }

    #[test]
    fn tsb_runtime_batch_ascii_aliases_parse() {
        let stmts = line_body(
            10,
            b"MUSIC 4,\"CDE\":PLAY ON:BFLASH 2,0,6:FLASH OFF:FETCH \"\",8,A$:KEY 1,\"RUN\":DISPLAY:COPY 1024,2048,40:SCRSV 49152,0:SCRLD 49152,1:MEM CLR 4096,16,0:MEMCLR 4096,8:MEMSAVE 1024,2048,40:MEMLOAD 2048,1024,40:MEMREAD 1024,3072,40:MEMDEF 1,2,3:DESIGN 832,1,2,3",
        )
        .unwrap();

        assert!(matches!(stmts[0], Statement::Music { .. }));
        assert!(matches!(stmts[1], Statement::Play { .. }));
        assert!(matches!(stmts[2], Statement::Bflash { .. }));
        assert!(matches!(
            stmts[3],
            Statement::Flash {
                enabled: Some(false),
                ..
            }
        ));
        assert!(matches!(stmts[4], Statement::Fetch { .. }));
        assert!(matches!(stmts[5], Statement::KeySet { .. }));
        assert!(matches!(stmts[6], Statement::DisplayKeys));
        assert!(matches!(stmts[7], Statement::Copy { .. }));
        assert!(matches!(stmts[8], Statement::ScrSave { .. }));
        assert!(matches!(stmts[9], Statement::ScrLoad { .. }));
        assert!(matches!(stmts[10], Statement::MemClr { .. }));
        assert!(matches!(stmts[11], Statement::MemClr { .. }));
        assert!(matches!(stmts[12], Statement::Copy { .. }));
        assert!(matches!(stmts[13], Statement::Copy { .. }));
        assert!(matches!(stmts[14], Statement::Copy { .. }));
        assert!(matches!(stmts[15], Statement::MemDef { .. }));
        assert!(matches!(stmts[16], Statement::Design { .. }));

        let stmts = line_body(
            20,
            &[
                TOK_TSB_PREFIX,
                TSB_MEM,
                TOK_SAVE,
                b'1',
                b'0',
                b'2',
                b'4',
                b',',
                b'2',
                b'0',
                b'4',
                b'8',
                b',',
                b'4',
                b'0',
                b':',
                TOK_TSB_PREFIX,
                TSB_MEM,
                TOK_LOAD,
                b':',
                TOK_TSB_PREFIX,
                TSB_MEM,
                TOK_CLR,
                b'4',
                b'0',
                b'9',
                b'6',
                b',',
                b'8',
            ],
        )
        .unwrap();
        assert!(matches!(stmts[0], Statement::Copy { .. }));
        assert!(matches!(stmts[1], Statement::MemTransfer { .. }));
        assert!(matches!(stmts[2], Statement::MemClr { .. }));
    }

    #[test]
    fn tsb_gap_statement_forms_parse() {
        let stmts = line_body(
            10,
            b"VOL ON:VOL OFF:SCRSV DEF 49152,1:SCRSV:SCRSV RESTORE:SCRLD DEF 49152,0:SCRLD:SCRLD RESTORE:AT(A$,B$):INSERT \"abcdefghi\",1,2,3,4,5:LIN:INST(\"HELLO\",\"EL\"):PLACE(\"EL\",\"HELLO\")",
        )
        .unwrap();

        assert!(matches!(
            &stmts[0],
            Statement::Poke {
                addr: Expr::Number(n),
                value: Expr::Bin(BinOp::And, lhs, _),
            } if *n == 0xD418 as f64 && matches!(lhs.as_ref(), Expr::Number(10.0))
        ));
        assert!(matches!(
            &stmts[1],
            Statement::Poke {
                addr: Expr::Number(n),
                value: Expr::Bin(BinOp::And, lhs, _),
            } if *n == 0xD418 as f64 && matches!(lhs.as_ref(), Expr::Number(0.0))
        ));
        assert!(matches!(
            &stmts[2],
            Statement::ScrDef {
                save: true,
                addr: Expr::Number(49152.0),
                mode: Some(Expr::Number(1.0)),
            }
        ));
        assert!(matches!(
            &stmts[3],
            Statement::ScrSave {
                addr: None,
                mode: None,
            }
        ));
        assert!(matches!(&stmts[4], Statement::ScrRestore { save: true }));
        assert!(matches!(
            &stmts[5],
            Statement::ScrDef {
                save: false,
                addr: Expr::Number(49152.0),
                mode: Some(Expr::Number(0.0)),
            }
        ));
        assert!(matches!(
            &stmts[6],
            Statement::ScrLoad {
                addr: None,
                mode: None,
            }
        ));
        assert!(matches!(&stmts[7], Statement::ScrRestore { save: false }));
        assert!(matches!(
            &stmts[8],
            Statement::SwapStr { lhs, rhs }
                if lhs.base == "A" && rhs.base == "B"
        ));
        assert!(matches!(&stmts[9], Statement::InsertBox { .. }));
        assert!(matches!(
            &stmts[10],
            Statement::Print(PrintStmt { items, .. })
                if matches!(items.as_slice(), [PrintItem::Expr(Expr::Lin)])
        ));
        assert!(matches!(
            &stmts[11],
            Statement::Print(PrintStmt { items, .. })
                if matches!(items.as_slice(), [PrintItem::Expr(Expr::Inst { .. })])
        ));
        assert!(matches!(
            &stmts[12],
            Statement::Print(PrintStmt { items, .. })
                if matches!(items.as_slice(), [PrintItem::Expr(Expr::Inst { .. })])
        ));
    }

    #[test]
    fn tsb_literals_and_numeric_functions_parse() {
        let stmts = line_body(
            10,
            &[
                TOK_PRINT,
                b'$',
                b'D',
                b'0',
                b'2',
                b'0',
                b',',
                b'%',
                b'1',
                b'0',
                b'1',
                b'0',
                b',',
                b'$',
                b'$',
                b'0',
                b'0',
                b'F',
                b'F',
                b',',
                TOK_TSB_PREFIX,
                TSB_EXOR,
                b'(',
                b'1',
                b',',
                b'2',
                b')',
                b',',
                TOK_TSB_PREFIX,
                TSB_FRAC,
                b'(',
                b'3',
                b'.',
                b'5',
                b')',
                b',',
                TOK_TSB_PREFIX,
                TSB_PENX,
                b',',
                TOK_TSB_PREFIX,
                TSB_JOY,
                b'(',
                b'2',
                b')',
            ],
        )
        .unwrap();
        let Statement::Print(print) = &stmts[0] else {
            panic!("expected PRINT");
        };
        let exprs: Vec<&Expr> = print
            .items
            .iter()
            .filter_map(|item| match item {
                PrintItem::Expr(e) => Some(e),
                _ => None,
            })
            .collect();
        assert_eq!(exprs.len(), 7);
        assert!(matches!(exprs[3], Expr::Bin(BinOp::Xor, _, _)));
        assert!(matches!(exprs[6], Expr::Joy(_)));
    }

    #[test]
    fn tsbneo_easy_commands_and_functions_parse() {
        let stmts = line_body(
            10,
            &[
                TOK_TSB_PREFIX,
                TSB_CENTER,
                b'"',
                b'H',
                b'I',
                b'"',
                b',',
                b'1',
                b'0',
                b':',
                TOK_TSB_PREFIX,
                TSB_BCKGNDS,
                b'0',
                b',',
                b'1',
                b',',
                b'2',
                b',',
                b'3',
                b':',
                TOK_TSB_PREFIX,
                TSB_NRM,
                b':',
                TOK_TSB_PREFIX,
                TSB_CSET,
                b'1',
                b':',
                TOK_TSB_PREFIX,
                TSB_DIV,
            ],
        )
        .unwrap();
        // CENTER prints the string with no trailing CR.
        assert!(matches!(
            &stmts[0],
            Statement::Print(PrintStmt {
                items,
                trailing_newline: false,
            }) if matches!(items.as_slice(), [PrintItem::Spc(_), PrintItem::StrExpr(_)])
        ));
        assert!(matches!(stmts[1], Statement::Bckgnds { .. }));
        assert!(matches!(stmts[2], Statement::Nrm));
        assert!(matches!(stmts[3], Statement::Cset { .. }));
        assert!(matches!(stmts[4], Statement::Print(_)));

        let stmts = line_body(
            20,
            &[
                TOK_PRINT,
                TOK_TSB_PREFIX,
                TSB_MOD,
                b'(',
                b'7',
                b',',
                b'3',
                b')',
                b',',
                TOK_TSB_PREFIX,
                TSB_DIV,
                b'(',
                b'7',
                b',',
                b'3',
                b')',
                b',',
                TOK_TSB_PREFIX,
                TSB_POT,
                b'(',
                b'0',
                b')',
                b',',
                TOK_TSB_PREFIX,
                TSB_INKEY,
            ],
        )
        .unwrap();
        let Statement::Print(print) = &stmts[0] else {
            panic!("expected PRINT");
        };
        let exprs: Vec<&Expr> = print
            .items
            .iter()
            .filter_map(|item| match item {
                PrintItem::Expr(e) => Some(e),
                _ => None,
            })
            .collect();
        assert_eq!(exprs.len(), 4);
        assert!(matches!(exprs[0], Expr::Bin(BinOp::Sub, _, _)));
        assert!(matches!(exprs[1], Expr::Func1(Func1::Int, _)));
        assert!(matches!(exprs[2], Expr::Pot(_)));
        assert!(matches!(exprs[3], Expr::Inkey));
    }

    #[test]
    fn tsbneo_compatibility_batch_parse() {
        let stmts = line_body(
            10,
            b"MOVE 0,0,10,2,5,5:UPW 0,0,10,2:KEYGET A$:DISK \"I0\":PAGE:TRACE",
        )
        .unwrap();
        assert!(matches!(stmts[0], Statement::ScreenMove { .. }));
        assert!(matches!(stmts[1], Statement::ScreenScroll { .. }));
        assert!(matches!(stmts[2], Statement::KeyGet { .. }));
        assert!(matches!(stmts[3], Statement::Disk { .. }));
        assert!(matches!(stmts[4], Statement::Rem(_)));
        assert!(matches!(stmts[5], Statement::Rem(_)));

        let stmts = line_body(
            20,
            &[
                TOK_LOAD, b'"', b'D', b'A', b'T', b'A', b'"', b',', b'8', b' ', b'U', b'S', b'E',
                b',', b'0', b',', b'4', b'0', b'9', b'6',
            ],
        )
        .unwrap();
        assert!(matches!(
            &stmts[0],
            Statement::Load {
                load_addr: Some(Expr::Number(4096.0)),
                ..
            }
        ));

        let mut print_line = vec![TOK_PRINT, b' '];
        print_line.extend_from_slice(b"LIN(),SOUND(),GRAPHICS(),DISPLAY(),PLACE(\"EL\",\"HELLO\")");
        let stmts = line_body(30, &print_line).unwrap();
        let Statement::Print(print) = &stmts[0] else {
            panic!("expected PRINT");
        };
        let exprs: Vec<&Expr> = print
            .items
            .iter()
            .filter_map(|item| match item {
                PrintItem::Expr(e) => Some(e),
                _ => None,
            })
            .collect();
        assert_eq!(exprs.len(), 5);
        assert!(matches!(exprs[0], Expr::Lin));
        assert!(matches!(exprs[1], Expr::Number(n) if *n == 0xD400 as f64));
        assert!(matches!(exprs[2], Expr::Number(n) if *n == 0xD000 as f64));
        assert!(matches!(exprs[3], Expr::Bin(BinOp::Mul, _, _)));
        assert!(matches!(exprs[4], Expr::Inst { .. }));
    }

    #[test]
    fn load_accepts_string_expression_filename() {
        // `LOAD A$, 8, 1` — filename held in a string variable.
        let stmts = line_body(10, &[TOK_LOAD, b'A', b'$', b',', b'8', b',', b'1']).unwrap();
        assert!(matches!(
            &stmts[0],
            Statement::Load {
                filename: StrExpr::Var(_),
                device: Some(Expr::Number(8.0)),
                secondary: Some(Expr::Number(1.0)),
                ..
            }
        ));

        // `LOAD (A$), 8` — a parenthesised string expression.
        let stmts = line_body(20, &[TOK_LOAD, b'(', b'A', b'$', b')', b',', b'8']).unwrap();
        assert!(matches!(
            &stmts[0],
            Statement::Load {
                filename: StrExpr::Var(_),
                device: Some(Expr::Number(8.0)),
                ..
            }
        ));
    }

    #[test]
    fn unsupported_tsbneo_tokens_report_name() {
        // 0x40 = SECURE — still unsupported. With the bitmap-graphics
        // batch landed (PLOT/REC/BLOCK/CIRCLE/CHAR/TEXT/PAINT/ROT/
        // ANGL/HIRES/LINE/DRAW), the tokens that remain "not yet
        // supported" are non-graphics commands like SECURE/MUSIC/
        // FETCH/COPY/MERGE.
        let err = line_body(10, &[TOK_TSB_PREFIX, 0x40]).unwrap_err();
        assert!(matches!(
            err,
            ParseError::Unsupported {
                line: 10,
                name: "SECURE"
            }
        ));
    }

    #[test]
    fn tsb_proc_exec_can_be_omitted() {
        let stmts = line_body(10, b"WORK").unwrap();
        match &stmts[0] {
            Statement::ProcCall(name) => assert_eq!(name.0.as_slice(), b"WORK"),
            other => panic!("unexpected parse result: {other:?}"),
        }
    }

    /// Regression: `FOR Z=10 TO 0 STEP +.2` (TankAttack line 510).
    /// BASIC v2 lets the expression evaluator absorb a leading `+` as a
    /// no-op unary, but our parser only handled unary `-` and bailed
    /// with "expected an expression" on the `STEP +.2` form.
    #[test]
    fn unary_plus_in_for_step_parses() {
        let bytes = [
            TOK_FOR, b'Z', TOK_EQ, b'1', b'0', TOK_TO, b'0', TOK_STEP, TOK_PLUS, b'.', b'2',
        ];
        let stmts = line_body(10, &bytes).unwrap();
        match &stmts[0] {
            Statement::For { step, .. } => {
                // `+.2` should reduce to a plain Number(0.2), since
                // unary plus is a no-op.
                match step {
                    Expr::Number(n) => {
                        assert!((n - 0.2).abs() < 1e-9, "expected 0.2, got {n}")
                    }
                    other => panic!("expected Number(0.2), got {other:?}"),
                }
            }
            other => panic!("expected FOR statement, got {other:?}"),
        }
    }
}
