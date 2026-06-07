//! BASIC v2 plus extended-token source tokenizer.
//!
//! Converts `.bas` text source to the tokenized `.prg` byte layout
//! consumed by `prg::Program::parse` and `compile_with_options`.
//! Existing tokenized `.prg` files still go through `prg.rs` directly.
//!
//! The matching ladder is:
//!   1. Inside string literals (`"..."`), every byte is literal
//!      PETSCII.
//!   2. After `REM`, the rest of the line is literal PETSCII.
//!   3. Extended tokens match before BASIC v2 so multi-word forms
//!      (`END LOOP`, `END PROC`, `MOB SET`, `LOW COL`, `HI COL`,
//!      `ON ERROR`, `NO ERROR`, `ON KEY`) win over the bare BASIC v2
//!      prefixes (`END`, `MOB`, `LOW`, `HI`, `ON`, `NO`).
//!   4. BASIC v2 keywords (longest-first to handle `GOSUB` vs `GO`,
//!      `INPUT#` vs `INPUT`).
//!   5. Operator-like punctuation (`+ - * / ^ > = <`).
//!   6. Otherwise: emit one PETSCII byte. Lower-case ASCII is
//!      uppercased so vars/keywords land in the editor-canonical
//!      form.

/// BASIC v2 keyword tokens, $80–$CB. Order matters for matching:
/// the lookup uses longest-first so `GOSUB` matches before `GO`,
/// `INPUT#` before `INPUT`, etc.
const KEYWORDS_V2: &[(&str, u8)] = &[
    ("END", 0x80),
    ("FOR", 0x81),
    ("NEXT", 0x82),
    ("DATA", 0x83),
    ("INPUT#", 0x84),
    ("INPUT", 0x85),
    ("DIM", 0x86),
    ("READ", 0x87),
    ("LET", 0x88),
    ("GOTO", 0x89),
    ("RUN", 0x8A),
    ("IF", 0x8B),
    ("RESTORE", 0x8C),
    ("GOSUB", 0x8D),
    ("RETURN", 0x8E),
    ("REM", 0x8F),
    ("STOP", 0x90),
    ("ON", 0x91),
    ("WAIT", 0x92),
    ("LOAD", 0x93),
    ("SAVE", 0x94),
    ("VERIFY", 0x95),
    ("DEF", 0x96),
    ("POKE", 0x97),
    ("PRINT#", 0x98),
    ("PRINT", 0x99),
    ("CONT", 0x9A),
    ("LIST", 0x9B),
    ("CLR", 0x9C),
    ("CMD", 0x9D),
    ("SYS", 0x9E),
    ("OPEN", 0x9F),
    ("CLOSE", 0xA0),
    ("GET", 0xA1),
    ("NEW", 0xA2),
    ("TAB(", 0xA3),
    ("TO", 0xA4),
    ("FN", 0xA5),
    ("SPC(", 0xA6),
    ("THEN", 0xA7),
    ("NOT", 0xA8),
    ("STEP", 0xA9),
    ("AND", 0xAF),
    ("OR", 0xB0),
    ("SGN", 0xB4),
    ("INT", 0xB5),
    ("ABS", 0xB6),
    ("USR", 0xB7),
    ("FRE", 0xB8),
    ("POS", 0xB9),
    ("SQR", 0xBA),
    ("RND", 0xBB),
    ("LOG", 0xBC),
    ("EXP", 0xBD),
    ("COS", 0xBE),
    ("SIN", 0xBF),
    ("TAN", 0xC0),
    ("ATN", 0xC1),
    ("PEEK", 0xC2),
    ("LEN", 0xC3),
    ("STR$", 0xC4),
    ("VAL", 0xC5),
    ("ASC", 0xC6),
    ("CHR$", 0xC7),
    ("LEFT$", 0xC8),
    ("RIGHT$", 0xC9),
    ("MID$", 0xCA),
    ("GO", 0xCB),
];

const OPS_V2: &[(&str, u8)] = &[
    ("+", 0xAA),
    ("-", 0xAB),
    ("*", 0xAC),
    ("/", 0xAD),
    ("^", 0xAE),
    (">", 0xB1),
    ("=", 0xB2),
    ("<", 0xB3),
];

/// PETSCII control-code escape names. Inside a string literal,
/// `"{name}"` is replaced with the listed byte, and `"{name*N}"`
/// repeats it `N` times. Names are matched case-insensitively.
///
/// Multiple aliases per code are kept because source archives use
/// different spellings (`reverse on`, `rvs on`, `rvon`).
const PETSCII_ESCAPE_NAMES: &[(&str, u8)] = &[
    // Colours (canonical + common aliases).
    ("black", 0x90),
    ("blk", 0x90),
    ("white", 0x05),
    ("wht", 0x05),
    ("red", 0x1C),
    ("cyan", 0x9F),
    ("cyn", 0x9F),
    ("purple", 0x9C),
    ("pur", 0x9C),
    ("magenta", 0x9C),
    ("green", 0x1E),
    ("grn", 0x1E),
    ("blue", 0x1F),
    ("blu", 0x1F),
    ("yellow", 0x9E),
    ("yel", 0x9E),
    ("orange", 0x81),
    ("orng", 0x81),
    ("brown", 0x95),
    ("pink", 0x96),
    ("light red", 0x96),
    ("lred", 0x96),
    ("dark grey", 0x97),
    ("dark gray", 0x97),
    ("grey1", 0x97),
    ("gray1", 0x97),
    ("grey", 0x98),
    ("gray", 0x98),
    ("medium grey", 0x98),
    ("medium gray", 0x98),
    ("grey2", 0x98),
    ("gray2", 0x98),
    ("light green", 0x99),
    ("lgrn", 0x99),
    ("lgreen", 0x99),
    ("light blue", 0x9A),
    ("lblu", 0x9A),
    ("lblue", 0x9A),
    ("light grey", 0x9B),
    ("light gray", 0x9B),
    ("lgrey", 0x9B),
    ("lgray", 0x9B),
    ("grey3", 0x9B),
    ("gray3", 0x9B),
    // Cursor / editor controls.
    ("home", 0x13),
    ("clear", 0x93),
    ("clr", 0x93),
    ("up", 0x91),
    ("down", 0x11),
    ("left", 0x9D),
    ("right", 0x1D),
    ("inst", 0x94),
    ("insert", 0x94),
    ("del", 0x14),
    ("delete", 0x14),
    ("return", 0x0D),
    ("cr", 0x0D),
    ("lf", 0x0A),
    ("esc", 0x1B),
    ("space", 0x20),
    ("rvs on", 0x12),
    ("rvson", 0x12),
    ("rvon", 0x12),
    ("reverse on", 0x12),
    ("rvs off", 0x92),
    ("rvsoff", 0x92),
    ("rvof", 0x92),
    ("reverse off", 0x92),
    ("stop", 0x03),
    ("run", 0x83),
    // Function keys F1–F8.
    ("f1", 0x85),
    ("f2", 0x89),
    ("f3", 0x86),
    ("f4", 0x8A),
    ("f5", 0x87),
    ("f6", 0x8B),
    ("f7", 0x88),
    ("f8", 0x8C),
    // Symbol glyphs sometimes spelled out.
    ("pound", 0x5C),
    ("pi", 0xFF),
    ("arrow left", 0x5F),
    ("arrow up", 0x5E),
    // Shifted symbol glyphs used by C64 screen art.
    // `sh asterisk` is shift+`*` = PETSCII $C0 (a graphic character).
    ("sh asterisk", 0xC0),
    ("sh *", 0xC0),
    ("shift asterisk", 0xC0),
    ("shift *", 0xC0),
    ("sh +", 0xDB),
    ("sh -", 0xDD),
    ("sh /", 0xDF),
    ("sh @", 0xBA),
    ("sh pound", 0xA9),
    ("sh space", 0xA0),
    ("shift space", 0xA0),
    // Shifted cursor: shift+down = cursor-up etc.
    ("sh cursor down", 0x91),
    ("sh cursor up", 0x11),
    ("sh cursor left", 0x1D),
    ("sh cursor right", 0x9D),
    ("sh up", 0x91),
    ("sh down", 0x11),
    ("sh left", 0x1D),
    ("sh right", 0x9D),
    ("sh home", 0x53),
    ("sh return", 0x8D),
    // CBM-key + symbol shortcuts that crop up in source archives.
    ("cm +", 0xA6),
    ("cm -", 0xDC),
    ("cm @", 0xA4),
    ("cm asterisk", 0xDF),
    ("cm *", 0xDF),
    ("cm pound", 0xA8),
    ("cm space", 0xA0),
    ("cm up arrow", 0xDE),
    ("ct up arrow", 0x1E),
];

/// CBM+letter PETSCII bytes. CBM+A is $B0, CBM+B is $BF, etc. — the
/// mapping isn't contiguous (it follows the C64 keyboard's
/// keymatrix layout), so we materialise it as a literal table.
const CBM_LETTER_BYTES: [u8; 26] = [
    0xB0, // A
    0xBF, // B
    0xBC, // C
    0xAC, // D
    0xB1, // E
    0xBB, // F
    0xA5, // G
    0xB4, // H
    0xA2, // I
    0xB5, // J
    0xA1, // K
    0xB6, // L
    0xA7, // M
    0xAA, // N
    0xB9, // O
    0xAF, // P
    0xAB, // Q
    0xB2, // R
    0xAE, // S
    0xA3, // T
    0xB8, // U
    0xBE, // V
    0xB3, // W
    0xBD, // X
    0xB7, // Y
    0xAD, // Z
];

/// Map ctrl-digit to the corresponding colour control byte.
/// `{ctrl 1}` ... `{ctrl 8}` are the keyboard colour shortcuts on
/// a C64 — same effect as `{black}` ... `{yellow}`.
fn ctrl_digit_byte(d: u8) -> Option<u8> {
    match d {
        b'1' => Some(0x90), // black
        b'2' => Some(0x05), // white
        b'3' => Some(0x1C), // red
        b'4' => Some(0x9F), // cyan
        b'5' => Some(0x9C), // purple
        b'6' => Some(0x1E), // green
        b'7' => Some(0x1F), // blue
        b'8' => Some(0x9E), // yellow
        b'9' => Some(0x12), // reverse on
        b'0' => Some(0x92), // reverse off
        _ => None,
    }
}

/// Extended tokens. Each token is encoded as the prefix byte $64
/// followed by the second byte listed here — the inverse of
/// [`crate::tokens::tsb_keyword`], which this table must stay in sync
/// with so `.bas` → `.prg` → detokenize round-trips.
///
/// The matcher is first-hit, so where one entry is a byte-prefix of
/// another the longer one must come first: keep `LINE` before `LIN`,
/// `DOWNB`/`DOWNW` before `DO`, and `MOB SET`/`MOBCOL` before `MOB`.
/// Multi-word forms carry the literal space, so they beat the bare
/// BASIC v2 prefixes (`END`, `ON`, `NO`, `LOW`, `HI`) automatically
/// (this table is consulted before [`KEYWORDS_V2`]). `AT(` includes
/// the open paren as part of the token — like `TAB(` / `SPC(` — so it
/// never false-hits inside an identifier.
const KEYWORDS_TSB: &[(&str, u8)] = &[
    // Multi-word forms first.
    ("HI COL", 0x0C),
    ("LOW COL", 0x76),
    ("MOB SET", 0x1D),
    ("MOBCOL", 0x41), // before MOB
    ("END PROC", 0x34),
    ("END LOOP", 0x36),
    ("ON KEY", 0x37),
    ("ON ERROR", 0x43),
    ("NO ERROR", 0x44),
    // Graphics / drawing.
    ("HIRES", 0x01),
    ("PLOT", 0x02),
    ("LINE", 0x03), // before LIN
    ("BLOCK", 0x04),
    ("FCHR", 0x05),
    ("FCOL", 0x06),
    ("FILL", 0x07),
    ("REC", 0x08),
    ("ROT", 0x09),
    ("DRAW", 0x0A),
    ("CHAR", 0x0B),
    ("INV", 0x0D),
    ("FRAC", 0x0E),
    ("MOVE", 0x0F),
    ("PLACE", 0x10),
    ("UPB", 0x11),
    ("UPW", 0x12),
    ("LEFTW", 0x13),
    ("LEFTB", 0x14),
    ("DOWNB", 0x15), // before DO
    ("DOWNW", 0x16), // before DO
    ("RIGHTB", 0x17),
    ("RIGHTW", 0x18),
    ("CIRCLE", 0x42),
    ("DESIGN", 0x62),
    ("PAINT", 0x75),
    ("GRAPHICS", 0x61),
    // Sprites.
    ("MULTI", 0x19),
    ("MMOB", 0x1B),
    ("RLOCMOB", 0x63),
    ("CMOB", 0x64),
    ("MOB", 0x68), // after MOB SET / MOBCOL
    ("DETECT", 0x7B),
    ("CHECK", 0x7C),
    // Colour / screen.
    ("COLOR", 0x1A),
    ("COLOUR", 0x1A),
    ("BCKGNDS", 0x65),
    ("CLS", 0x3C),
    ("CSET", 0x70),
    ("MAP", 0x3E),
    ("MEM", 0x7A),
    ("DISPLAY", 0x7D),
    ("TEXT", 0x6F),
    // Sound.
    ("BFLASH", 0x1C),
    ("FLASH", 0x1F),
    ("MUSIC", 0x1E),
    ("PLAY", 0x21),
    ("ENVELOPE", 0x24),
    ("WAVE", 0x26),
    ("SOUND", 0x60),
    ("VOL", 0x71),
    // Control flow / structure.
    ("REPEAT", 0x20),
    ("UNTIL", 0x29),
    ("DO", 0x22), // after DOWNB / DOWNW
    ("LOOP", 0x3A),
    ("EXIT", 0x35),
    ("PROC", 0x31),
    ("EXEC", 0x33),
    ("CALL", 0x32),
    ("LOCAL", 0x45),
    ("GLOBAL", 0x2E),
    ("CENTER", 0x23),
    ("CENTRE", 0x23),
    ("CGOTO", 0x25),
    ("RCOMP", 0x46),
    ("ELSE", 0x47),
    ("DISABLE", 0x38),
    ("RESUME", 0x39),
    ("RETRACE", 0x48),
    ("TRACE", 0x49),
    ("PAUSE", 0x66),
    ("DELAY", 0x3B),
    ("USE", 0x2C),
    ("RESET", 0x30),
    ("SECURE", 0x40),
    ("NRM", 0x67),
    ("FETCH", 0x27),
    // Editor / disk / system.
    ("DIR", 0x4A),
    ("PAGE", 0x4B),
    ("DUMP", 0x4C),
    ("FIND", 0x4D),
    ("OPTION", 0x4E),
    ("AUTO", 0x4F),
    ("OLD", 0x50),
    ("RENUMBER", 0x79),
    ("COPY", 0x77),
    ("MERGE", 0x78),
    ("DISK", 0x72),
    ("HRDCPY", 0x73),
    ("KEY", 0x74),
    ("SCRSV", 0x6D),
    ("SCRLD", 0x6E),
    ("COLD", 0x6C),
    ("OFF", 0x69),
    ("ERR", 0x7E),
    ("OUT", 0x7F),
    // Functions / operators.
    ("AT(", 0x28),
    ("LIN", 0x59), // after LINE
    ("MOD", 0x52),
    ("DIV", 0x53),
    ("EXOR", 0x5A),
    ("JOY", 0x51),
    ("INKEY", 0x56),
    ("INSERT", 0x5B),
    ("INST", 0x57),
    ("TEST", 0x58),
    ("DUP", 0x55),
    ("POT", 0x5C),
    ("PENX", 0x5D),
    ("PENY", 0x5F),
    ("ANGL", 0x6A),
    ("ARC", 0x6B),
    ("X!", 0x3D),
    ("D!", 0x54),
];

/// Default BASIC load address on a stock C64.
pub const LOAD_ADDR_C64: u16 = 0x0801;

#[derive(Debug)]
pub enum TokenizeError {
    /// A non-blank, non-comment line was missing its leading line
    /// number. Carries the 1-based source line index.
    MissingLineNumber(usize),
    /// Line number didn't fit in a u16 (BASIC v2 limit is 0–63999;
    /// the .prg layout itself goes up to 65535).
    LineNumberOutOfRange(usize, u32),
    /// The encoded program exceeded the 16-bit address space.
    /// Practically only triggered by truly enormous inputs.
    AddressOverflow,
}

impl std::fmt::Display for TokenizeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TokenizeError::MissingLineNumber(n) => {
                write!(f, "source line {n}: missing leading line number")
            }
            TokenizeError::LineNumberOutOfRange(n, v) => {
                write!(f, "source line {n}: line number {v} out of range (0–65535)")
            }
            TokenizeError::AddressOverflow => write!(f, "tokenized program exceeds 64KB"),
        }
    }
}

impl std::error::Error for TokenizeError {}

/// Tokenize one source line body (everything after the line number).
///
/// `body` is the raw text after the leading line number and one
/// optional separator space. The output is the per-line body bytes
/// — the caller is responsible for the surrounding `.prg` framing
/// (next-link pointer, line number word, terminating `0x00`).
fn tokenize_body(body: &str) -> Vec<u8> {
    let bytes = body.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    let mut in_string = false;
    let mut in_rem = false;
    let mut in_data = false;

    while i < bytes.len() {
        let c = bytes[i];
        if in_string {
            // PETSCII escape: `"{name}"` becomes the corresponding
            // control byte, repeated by `{name*N}` when requested.
            // Unknown names fall through as literal text.
            if c == b'{' {
                if let Some(close) = bytes[i + 1..].iter().position(|&b| b == b'}') {
                    let inner_start = i + 1;
                    let inner_end = inner_start + close;
                    let inner = &body[inner_start..inner_end];
                    if let Some((byte, count)) = resolve_petscii_escape(inner) {
                        for _ in 0..count {
                            out.push(byte);
                        }
                        i = inner_end + 1;
                        continue;
                    }
                }
                // Fall through: treat `{` as literal.
            }
            // Closing quote ends the literal but is itself preserved.
            if c == b'"' {
                in_string = false;
                out.push(c);
                i += 1;
                continue;
            }
            // C64 BASIC stores lowercase source letters as PETSCII
            // uppercase and uppercase source letters as shifted
            // letters. This keeps on-screen text looking as typed.
            let mapped = if c.is_ascii_lowercase() {
                c - 0x20 // a-z → A-Z (PETSCII $41-$5A)
            } else if c.is_ascii_uppercase() {
                c | 0x80 // A-Z → shifted A-Z (PETSCII $C1-$DA)
            } else {
                c
            };
            out.push(mapped);
            i += 1;
            continue;
        }
        if in_rem {
            // After REM the rest of the line is literal PETSCII.
            // Lower-case is left alone — REM bodies are
            // free-form text and the user's casing matters.
            out.push(c);
            i += 1;
            continue;
        }
        if in_data {
            // After DATA, every byte up to a `:` (statement
            // terminator) or end-of-line is a raw value byte —
            // never a tokenized keyword. Without this, words like
            // "BORING" were partly tokenized: the embedded "OR"
            // ($B0) replaced two characters, so the runtime READ
            // pulled in PETSCII $B0 instead of the letters O and
            // R, and PRINT spat out a graphic glyph in the middle
            // of the word. (`:` ends the DATA scope.)
            //
            // Case handling matches the outside-string default:
            // lowercase ASCII folds to uppercase PETSCII (= screen
            // letters in upper/graphics charset), uppercase
            // letters pass through verbatim (= also screen
            // letters). The string-literal case-swap is intentionally
            // not applied here; DATA text should print as letters, not
            // upper/graphics glyphs.
            if c == b':' {
                in_data = false;
                out.push(c);
                i += 1;
                continue;
            }
            if c.is_ascii_lowercase() {
                out.push(c.to_ascii_uppercase());
            } else {
                out.push(c);
            }
            i += 1;
            continue;
        }
        if c == b'"' {
            in_string = true;
            out.push(b'"');
            i += 1;
            continue;
        }

        // No word-boundary gate: BASIC v2 tokenises greedily wherever
        // keyword letters appear outside DATA and string literals.
        // Extended tokens are matched first because some forms
        // ("END LOOP", "MOB SET") share a prefix with bare BASIC v2
        // keywords. Without this ordering "END LOOP" would tokenise
        // as the BASIC v2 END followed by a literal " LOOP" string,
        // which the parser would never accept.
        if let Some((len, tok)) = match_keyword(KEYWORDS_TSB, bytes, i) {
            out.push(0x64);
            out.push(tok);
            i += len;
            continue;
        }

        if let Some((len, tok)) = match_keyword(KEYWORDS_V2, bytes, i) {
            out.push(tok);
            if tok == 0x8F {
                in_rem = true;
            }
            if tok == 0x83 {
                in_data = true;
            }
            i += len;
            continue;
        }

        if let Some((len, tok)) = match_keyword(OPS_V2, bytes, i) {
            out.push(tok);
            i += len;
            continue;
        }

        // Fall through to a literal PETSCII byte. Upper-case
        // alphabetic bytes match the editor's canonical form for
        // identifiers and unrecognised words, so `print` outside a
        // keyword position becomes `PRINT`-styled glyphs on a real
        // C64.
        if c.is_ascii_lowercase() {
            out.push(c.to_ascii_uppercase());
        } else {
            out.push(c);
        }
        i += 1;
    }
    out
}

/// Resolve one PETSCII escape body (everything between the
/// curly braces) to `(byte, repeat_count)`. Accepts:
///   * `name` — look up in [`PETSCII_ESCAPE_NAMES`].
///   * `name*N` — same byte, repeated `N` times.
///   * `<digits>` — a literal decimal byte value (`{193}` → $C1).
///   * `ctrl <digit>` / `ctrl-<digit>` — colour shortcut.
///   * `ctrl <letter>` / `ctrl-<letter>` / `control-<letter>` —
///     PETSCII $01–$1A (A=$01, B=$02, ..., Z=$1A).
///   * `cm <letter>` / `cbm <letter>` / `cbm-<letter>` — CBM-key +
///     letter using [`CBM_LETTER_BYTES`].
///   * `sh <letter>` / `shift <letter>` — shift+letter, which on
///     PETSCII is `0xC0 + (letter - 'A' + 1)` = $C1–$DA.
/// Names are case-insensitive and tolerant of `<sep>` being either
/// space, `-`, or `_` between the prefix and the argument.
///
/// Returns `None` for unrecognised contents — the caller emits the
/// raw `{...}` text in that case so a normal program with literal
/// curly braces in a string isn't mangled.
fn resolve_petscii_escape(body: &str) -> Option<(u8, usize)> {
    let body = body.trim();
    if body.is_empty() {
        return None;
    }

    // Strip the optional `*N` suffix once, on the OUTER form, so the
    // caller can repeat whatever single byte the rest resolves to.
    let (name, repeat) = if let Some(idx) = body.rfind('*') {
        let count_str = body[idx + 1..].trim();
        match count_str.parse::<usize>() {
            Ok(n) if n > 0 => (body[..idx].trim(), n),
            _ => (body, 1),
        }
    } else {
        (body, 1)
    };

    // Pure decimal byte, e.g. `{193}` → $C1.
    if name.bytes().all(|b| b.is_ascii_digit()) {
        if let Ok(v) = name.parse::<u16>()
            && v <= 255
        {
            return Some((v as u8, repeat));
        }
    }

    // Hex byte, e.g. `{$A6}` → $A6. This is the form the detokenizer
    // emits for bytes that have no named escape, so a listing tokenizes
    // back to the same bytes.
    if let Some(hex) = name.strip_prefix('$')
        && !hex.is_empty()
        && hex.bytes().all(|b| b.is_ascii_hexdigit())
        && let Ok(v) = u16::from_str_radix(hex, 16)
        && v <= 255
    {
        return Some((v as u8, repeat));
    }

    let lower = name.to_ascii_lowercase();

    // Direct hit on the named table (handles "rvs on", "sh asterisk",
    // "cm pound", etc.).
    for (kw, byte) in PETSCII_ESCAPE_NAMES {
        if lower == *kw {
            return Some((*byte, repeat));
        }
    }

    // Prefix-style escapes: "ctrl-X", "control-X", "cm X", "sh X".
    let (prefix, arg) = match split_prefix_arg(&lower) {
        Some(p) => p,
        None => return None,
    };

    match prefix {
        "ctrl" | "cntrl" | "control" => {
            let arg = arg.trim();
            if arg.len() == 1 {
                let b = arg.as_bytes()[0];
                if let Some(c) = ctrl_digit_byte(b) {
                    return Some((c, repeat));
                }
                if b.is_ascii_alphabetic() {
                    let letter = b.to_ascii_uppercase();
                    return Some(((letter - b'A' + 1) as u8, repeat));
                }
                // Non-alpha, non-digit `{ctrl X}` follows the C64
                // keyboard CTRL rule: clear bits 5-7.
                return Some((b & 0x1F, repeat));
            }
            None
        }
        "cm" | "cbm" => {
            let arg = arg.trim();
            if arg.len() == 1 {
                let b = arg.as_bytes()[0];
                if b.is_ascii_alphabetic() {
                    let idx = (b.to_ascii_uppercase() - b'A') as usize;
                    return Some((CBM_LETTER_BYTES[idx], repeat));
                }
            }
            // Multi-char CBM args ("cm pound", "cm asterisk") are
            // handled by the named-table fall-through above. If we
            // get here the form is unrecognised.
            None
        }
        "sh" | "shift" => {
            let arg = arg.trim();
            if arg.len() == 1 {
                let b = arg.as_bytes()[0];
                if b.is_ascii_alphabetic() {
                    let letter = b.to_ascii_uppercase();
                    return Some((0xC0 + (letter - b'A' + 1), repeat));
                }
            }
            None
        }
        _ => None,
    }
}

/// Split `"prefix sep rest"` into `("prefix", "rest")` where `sep` is
/// space, `-`, or `_`. Used by [`resolve_petscii_escape`] to peel off
/// `ctrl-`, `cm `, etc. before parsing the argument.
fn split_prefix_arg(s: &str) -> Option<(&str, &str)> {
    for (i, c) in s.char_indices() {
        if c == ' ' || c == '-' || c == '_' {
            return Some((&s[..i], &s[i + c.len_utf8()..]));
        }
    }
    None
}

/// Walk the keyword table and return `(matched_length, token_byte)`
/// for the longest entry whose textual form matches `bytes[i..]`
/// case-insensitively. Tables are ordered longest-first so the first
/// hit is also the longest hit.
fn match_keyword(table: &[(&str, u8)], bytes: &[u8], i: usize) -> Option<(usize, u8)> {
    // The extended and BASIC v2 tables are short enough (~150 entries
    // combined) that a linear scan with case-insensitive byte compare
    // beats anything with hashing — we'd lose the first-hit / longest-
    // match invariant and gain nothing on this size of input.
    for (kw, tok) in table {
        let kw_bytes = kw.as_bytes();
        if i + kw_bytes.len() > bytes.len() {
            continue;
        }
        if bytes[i..i + kw_bytes.len()]
            .iter()
            .zip(kw_bytes.iter())
            .all(|(a, b)| a.to_ascii_uppercase() == b.to_ascii_uppercase())
        {
            return Some((kw_bytes.len(), *tok));
        }
    }
    None
}

/// Convert BASIC v2 plus extended-token source text to the .prg byte layout.
///
/// Tokenized PRG layout:
/// ```text
///   [load_addr_lo, load_addr_hi]
///   per line: [next_link_lo, next_link_hi,
///              line_no_lo, line_no_hi,
///              body_bytes..., 0x00]
///   final:    [0x00, 0x00]
/// ```
///
/// Blank lines are dropped silently (BASIC source allows them as
/// readability whitespace). Lines without a leading line number
/// raise [`TokenizeError::MissingLineNumber`] — there's no implicit
/// auto-numbering.
pub fn tokenize_program(source: &str) -> Result<Vec<u8>, TokenizeError> {
    tokenize_program_at(source, LOAD_ADDR_C64)
}

/// Same as [`tokenize_program`] but with an explicit load address.
/// Exposed for tests and for future targets (C128/PET) that don't
/// share the C64 default.
pub fn tokenize_program_at(source: &str, load_addr: u16) -> Result<Vec<u8>, TokenizeError> {
    let mut out = Vec::with_capacity(source.len() + 64);
    out.push((load_addr & 0xFF) as u8);
    out.push(((load_addr >> 8) & 0xFF) as u8);

    // C64-native dumps use bare CR; `str::lines` only splits on `\n`
    // and `\r\n`.
    let normalized = if source.contains('\r') {
        source.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        source.to_string()
    };

    let mut cur_addr: u32 = load_addr as u32;
    for (idx, raw_line) in normalized.lines().enumerate() {
        let trimmed = raw_line.trim_end();
        if trimmed.trim().is_empty() {
            continue;
        }
        let stripped = trimmed.trim_start();

        // Consume the leading run of ASCII digits as the line
        // number. BASIC v2 tolerates more than one space between
        // the number and the body, so we skip ALL whitespace after
        // the number rather than exactly one byte.
        let line_no_str: String = stripped
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if line_no_str.is_empty() {
            return Err(TokenizeError::MissingLineNumber(idx + 1));
        }
        let line_no: u32 = line_no_str.parse().unwrap_or(u32::MAX);
        if line_no > 0xFFFF {
            return Err(TokenizeError::LineNumberOutOfRange(idx + 1, line_no));
        }

        let body_start = stripped[line_no_str.len()..].trim_start_matches(' ');
        let body = tokenize_body(body_start);

        let line_len: u32 = 4 + body.len() as u32 + 1; // link + line# + body + NUL
        let next_addr = cur_addr
            .checked_add(line_len)
            .ok_or(TokenizeError::AddressOverflow)?;
        if next_addr > 0xFFFF {
            return Err(TokenizeError::AddressOverflow);
        }

        out.push((next_addr & 0xFF) as u8);
        out.push(((next_addr >> 8) & 0xFF) as u8);
        out.push((line_no & 0xFF) as u8);
        out.push(((line_no >> 8) & 0xFF) as u8);
        out.extend_from_slice(&body);
        out.push(0x00);
        cur_addr = next_addr;
    }

    out.push(0x00);
    out.push(0x00);
    Ok(out)
}

/// True when `path`'s extension (case-insensitive) is `.bas` — the
/// signal CLI/GUI use to decide between "tokenize then compile" and
/// "compile this `.prg` directly".
pub fn is_basic_source_path(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("bas"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(line: &str) -> Vec<u8> {
        tokenize_body(line)
    }

    #[test]
    fn basic_v2_keyword_tokenises() {
        // PRINT "HI" -> $99, literal space, then shifted PETSCII
        // letters inside the string.
        assert_eq!(
            body("PRINT \"HI\""),
            vec![0x99, b' ', 0x22, 0xC8, 0xC9, 0x22]
        );
    }

    #[test]
    fn longer_keyword_wins_over_prefix() {
        // GOSUB must beat GO+SUB. GOSUB is $8D.
        assert_eq!(body("GOSUB100"), vec![0x8D, b'1', b'0', b'0']);
    }

    #[test]
    fn input_hash_beats_input() {
        // INPUT# is $84, INPUT alone is $85. Make sure INPUT# wins
        // when the user typed it.
        assert_eq!(body("INPUT#1"), vec![0x84, b'1']);
    }

    #[test]
    fn rem_body_is_literal_petscii() {
        // After REM ($8F) the rest of the line is verbatim — the
        // word "PRINT" inside the comment must NOT tokenise.
        let out = body("REM PRINT");
        assert_eq!(out[0], 0x8F);
        assert_eq!(&out[1..], b" PRINT");
    }

    #[test]
    fn string_literal_protects_petscii() {
        // PRINT "GOTO" should not see GOTO ($89); string text is
        // tokenized only as PETSCII bytes.
        let out = body("PRINT \"GOTO\"");
        assert_eq!(out, vec![0x99, b' ', 0x22, 0xC7, 0xCF, 0xD4, 0xCF, 0x22]);
    }

    #[test]
    fn lowercase_keyword_uppercases() {
        // BASIC v2 editors store keywords as token bytes regardless
        // of source case. The tokenizer is case-insensitive on
        // matching, so `print` becomes $99 too.
        assert_eq!(body("print"), vec![0x99]);
    }

    #[test]
    fn lowercase_identifier_uppercases() {
        // Outside a keyword position, lowercase ASCII is rewritten
        // to upper so var names match the C64 editor's canonical
        // form. `abc` → A B C.
        assert_eq!(body("abc"), vec![b'A', b'B', b'C']);
    }

    #[test]
    fn tsbneo_multi_word_beats_basic_prefix() {
        // END LOOP is a single keyword ($64 $36); it must
        // beat the bare BASIC v2 END ($80) which would otherwise
        // grab "END" alone and leave " LOOP" as a literal string —
        // the parser would never accept that.
        let out = body("END LOOP");
        assert_eq!(out, vec![0x64, 0x36]);
    }

    #[test]
    fn tsbneo_at_and_lin_tokenise() {
        // `AT(` includes the open paren, like TAB(/SPC().
        // `LIN` must also tokenise before parsing.
        let out = body("PRINTAT(LIN-4,0)\"\";");
        assert_eq!(
            out,
            vec![
                0x99, // PRINT
                0x64, 0x28, // AT(
                0x64, 0x59, // LIN
                0xAB, // -
                b'4', 0x2C, // 4 ,
                b'0', 0x29, // 0 )
                0x22, 0x22, // ""
                0x3B, // ;
            ]
        );
    }

    #[test]
    fn tsbneo_line_beats_lin() {
        // `LINE` ($64 $03) is a byte-prefix conflict with `LIN`
        // ($64 $59); the table must keep LINE first so `LINE10`
        // doesn't tokenise as LIN + "E10".
        assert_eq!(body("LINE10"), vec![0x64, 0x03, b'1', b'0']);
        // `LIN` still tokenises when it stands alone.
        assert_eq!(body("X=LIN"), vec![b'X', 0xB2, 0x64, 0x59]);
    }

    #[test]
    fn tsbneo_downb_beats_do() {
        // `DO` ($64 $22) is a byte-prefix of `DOWNB` ($64 $15) /
        // `DOWNW` ($64 $16) — the longer forms must win.
        assert_eq!(body("DOWNB10"), vec![0x64, 0x15, b'1', b'0']);
        assert_eq!(body("DOWNW10"), vec![0x64, 0x16, b'1', b'0']);
        assert_eq!(body("DO"), vec![0x64, 0x22]);
    }

    #[test]
    fn tsbneo_mob_set_and_mobcol_beat_mob() {
        // `MOB` ($64 $68) is a byte-prefix of `MOB SET` ($64 $1D)
        // and `MOBCOL` ($64 $41).
        assert_eq!(body("MOB SET1"), vec![0x64, 0x1D, b'1']);
        assert_eq!(body("MOBCOL1"), vec![0x64, 0x41, b'1']);
        assert_eq!(body("MOB0"), vec![0x64, 0x68, b'0']);
    }

    #[test]
    fn operators_tokenise() {
        // `=` is $B2, not literal $3D.
        assert_eq!(body("A=1"), vec![b'A', 0xB2, b'1']);
    }

    #[test]
    fn data_does_not_tokenise_keywords_inside_values() {
        // DATA text is literal until `:`, so keyword-like substrings
        // inside values must not be tokenized.
        let out = body("DATA BORING");
        assert_eq!(out, vec![0x83, b' ', b'B', b'O', b'R', b'I', b'N', b'G']);
    }

    #[test]
    fn data_terminates_on_colon() {
        // `DATA FOO:PRINT` — the colon ends the DATA scope,
        // so the following PRINT must still tokenize.
        let out = body("DATA FOO:PRINT");
        assert_eq!(
            out,
            vec![
                0x83, b' ', b'F', b'O', b'O', b':',
                0x99, // PRINT — must still tokenize after the colon
            ]
        );
    }

    #[test]
    fn keyword_inside_identifier_outside_data_tokenises_greedily() {
        // C64 BASIC v2's tokeniser is greedy: `LET BORING=1`
        // matches `OR` inside `BORING` and emits the OR token.
        // Real BASIC then errors at runtime trying to parse the
        // resulting `B <OR> ING=1` as an expression — but a
        // word-boundary guard that suppressed the match would
        // also break common shapes like `IF X<>12ANDPE<>36`,
        // `IFZAND8THEN…`, etc., where the keyword sits next to
        // a letter on both sides on purpose. Match the ROM
        // tokeniser; programs that name variables `BORING` are
        // already broken in real BASIC for the same reason.
        // The DATA scope is special-cased separately; see
        // `data_does_not_tokenise_keywords_inside_values`.
        let out = body("LET BORING=1");
        assert_eq!(
            out,
            vec![
                0x88, b' ', b'B', 0xB0, // OR — greedy, matches inside the identifier
                b'I', b'N', b'G', 0xB2, b'1',
            ]
        );
    }

    #[test]
    fn keyword_at_word_boundary_still_tokenises() {
        // `IF X=1 OR Y=2` — `OR` after a space (delimiter) is a
        // real keyword and must tokenize.
        let out = body("IF X=1 OR Y=2");
        assert_eq!(
            out,
            vec![
                0x8B, b' ', b'X', 0xB2, b'1', b' ', 0xB0, // OR
                b' ', b'Y', 0xB2, b'2',
            ]
        );
    }

    #[test]
    fn keyword_after_single_letter_var_tokenises() {
        // `Z` is a 1-letter var and `AND` must tokenise immediately
        // after it.
        let out = body("IFZAND8THENA=1");
        assert_eq!(
            out,
            vec![
                0x8B, // IF
                b'Z', 0xAF, // AND
                b'8', 0xA7, // THEN
                b'A', 0xB2, b'1',
            ]
        );
    }

    #[test]
    fn keyword_after_keyword_still_tokenises() {
        // `IF X=1 THEN GOSUB 100` written without trailing
        // spaces (`THENGOSUB100`) — the source byte right
        // before the second keyword is the last LETTER of the
        // first keyword (`N` of THEN), but the emitted byte is
        // the keyword token. The gate must look at emitted bytes.
        let out = body("IFX=1THENGOSUB100");
        assert_eq!(
            out,
            vec![
                0x8B, // IF
                b'X', 0xB2, b'1', 0xA7, // THEN
                0x8D, // GOSUB
                b'1', b'0', b'0',
            ]
        );
    }

    #[test]
    fn keyword_after_digit_still_tokenises() {
        // `IF X<>12ANDP<>36` is valid BASIC v2 without spaces.
        // The left boundary is enough to separate this from an identifier.
        let out = body("IF X<>12ANDP<>36");
        assert_eq!(
            out,
            vec![
                0x8B, b' ', b'X', 0xB3, 0xB1, b'1', b'2', 0xAF, // AND
                b'P', 0xB3, 0xB1, b'3', b'6',
            ]
        );
    }

    #[test]
    fn full_program_layout() {
        // 10 PRINT
        // 20 END
        // Should produce valid .prg bytes that prg::Program::parse
        // accepts and that round-trips both line numbers.
        let prg = tokenize_program("10 PRINT\n20 END\n").unwrap();
        let parsed = crate::prg::Program::parse(&prg).expect("parse round-trip");
        let line_numbers: Vec<u16> = parsed.lines.iter().map(|l| l.number).collect();
        assert_eq!(line_numbers, vec![10, 20]);
    }

    #[test]
    fn blank_lines_dropped() {
        let prg = tokenize_program("10 PRINT\n\n  \n20 END\n").unwrap();
        let parsed = crate::prg::Program::parse(&prg).unwrap();
        assert_eq!(parsed.lines.len(), 2);
    }

    #[test]
    fn missing_line_number_errors() {
        let err = tokenize_program("PRINT\n").unwrap_err();
        assert!(matches!(err, TokenizeError::MissingLineNumber(1)));
    }

    #[test]
    fn cr_only_line_endings() {
        // C64-native dumps use bare CR as the separator. Must produce
        // the same .prg as the LF-separated form.
        let prg = tokenize_program("10 PRINT\r20 END\r").unwrap();
        let parsed = crate::prg::Program::parse(&prg).unwrap();
        let line_numbers: Vec<u16> = parsed.lines.iter().map(|l| l.number).collect();
        assert_eq!(line_numbers, vec![10, 20]);
    }

    #[test]
    fn crlf_line_endings() {
        let prg = tokenize_program("10 PRINT\r\n20 END\r\n").unwrap();
        let parsed = crate::prg::Program::parse(&prg).unwrap();
        let line_numbers: Vec<u16> = parsed.lines.iter().map(|l| l.number).collect();
        assert_eq!(line_numbers, vec![10, 20]);
    }

    #[test]
    fn line_number_out_of_range_errors() {
        let err = tokenize_program("70000 PRINT\n").unwrap_err();
        assert!(matches!(err, TokenizeError::LineNumberOutOfRange(1, 70000)));
    }

    #[test]
    fn petscii_color_escape_inside_string() {
        // `{black}` → $90 — the C64 black-text colour code.
        // `{clear}` → $93 — clear screen.
        let out = body("PRINT \"{black}{clear}HI\"");
        assert_eq!(out, vec![0x99, b' ', 0x22, 0x90, 0x93, 0xC8, 0xC9, 0x22]);
    }

    #[test]
    fn petscii_repetition() {
        // `{down*3}` should expand to three $11 bytes.
        let out = body("PRINT \"{down*3}\"");
        assert_eq!(out, vec![0x99, b' ', 0x22, 0x11, 0x11, 0x11, 0x22]);
    }

    #[test]
    fn petscii_numeric_escape() {
        // `{193}` → $C1 (shift-A graphic glyph).
        let out = body("PRINT \"{193}\"");
        assert_eq!(out, vec![0x99, b' ', 0x22, 0xC1, 0x22]);
    }

    #[test]
    fn petscii_hex_escape() {
        // `{$A6}` is the form the detokenizer emits for bytes without a
        // named escape; it must tokenize back to that byte so a listing
        // round-trips. Lower-case hex digits are accepted too.
        assert_eq!(body("PRINT \"{$A6}\""), vec![0x99, b' ', 0x22, 0xA6, 0x22]);
        assert_eq!(body("PRINT \"{$a6}\""), vec![0x99, b' ', 0x22, 0xA6, 0x22]);
    }

    #[test]
    fn petscii_hex_escape_repeat() {
        // The `*N` repeat suffix works with the hex form as well.
        assert_eq!(
            body("PRINT \"{$A6*2}\""),
            vec![0x99, b' ', 0x22, 0xA6, 0xA6, 0x22]
        );
    }

    #[test]
    fn petscii_listing_round_trips_unmapped_byte() {
        // Detokenizing byte $A6 yields `{$A6}`; re-tokenizing must
        // reproduce the original byte.
        let rendered = crate::petscii::byte_to_string(0xA6);
        assert_eq!(rendered, "{$A6}");
        assert_eq!(body(&format!("PRINT \"{rendered}\"")), vec![
            0x99, b' ', 0x22, 0xA6, 0x22
        ]);
    }

    #[test]
    fn petscii_shift_letter() {
        // `{sh asterisk}` -> $C0.
        let out = body("PRINT \"{sh asterisk}\"");
        assert_eq!(out, vec![0x99, b' ', 0x22, 0xC0, 0x22]);
    }

    #[test]
    fn petscii_cbm_letter() {
        // CBM+T → $A3.
        let out = body("PRINT \"{cm t}\"");
        assert_eq!(out, vec![0x99, b' ', 0x22, 0xA3, 0x22]);
    }

    #[test]
    fn petscii_ctrl_digit_color_alias() {
        // `{ctrl 1}` is the same as `{black}` — both produce $90.
        let out = body("PRINT \"{ctrl 1}\"");
        assert_eq!(out, vec![0x99, b' ', 0x22, 0x90, 0x22]);
    }

    #[test]
    fn petscii_control_letter() {
        // `{control-q}` → $11 (PETSCII CTRL+Q == cursor-down byte).
        let out = body("PRINT \"{control-q}\"");
        assert_eq!(out, vec![0x99, b' ', 0x22, 0x11, 0x22]);
    }

    #[test]
    fn petscii_ctrl_punctuation() {
        // Non-letter, non-digit `{ctrl X}` follows the C64 keyboard
        // rule: clear bits 5-7 of the key's PETSCII.
        let out = body("PRINT \"{ctrl ;}\"");
        assert_eq!(out, vec![0x99, b' ', 0x22, 0x1B, 0x22]);
    }

    #[test]
    fn petscii_unknown_falls_through_literal() {
        // Unrecognised escapes are passed through as raw bytes so
        // programs that legitimately have `{`/`}` in a string keep
        // working. The string case-swap still applies to the letters
        // inside the brace text — `{nope}` becomes `{NOPE}` because
        // lowercase ASCII a-z maps to PETSCII $41-$5A.
        let out = body("PRINT \"{nope}\"");
        let expected: Vec<u8> = std::iter::once(0x99u8)
            .chain(std::iter::once(b' '))
            .chain(std::iter::once(0x22))
            .chain([b'{', b'N', b'O', b'P', b'E', b'}'])
            .chain(std::iter::once(0x22))
            .collect();
        assert_eq!(out, expected);
    }

    #[test]
    fn petscii_outside_string_is_literal() {
        // `{black}` outside of a string is just literal source text.
        let out = body("{black}");
        assert_eq!(out, b"{BLACK}".to_vec());
    }

    #[test]
    fn extension_detector_is_case_insensitive() {
        assert!(is_basic_source_path(std::path::Path::new("foo.bas")));
        assert!(is_basic_source_path(std::path::Path::new("foo.BAS")));
        assert!(is_basic_source_path(std::path::Path::new("a/b/c.Bas")));
        assert!(!is_basic_source_path(std::path::Path::new("foo.prg")));
        assert!(!is_basic_source_path(std::path::Path::new("foo")));
    }
}
