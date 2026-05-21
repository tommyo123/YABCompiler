//! Commodore BASIC v2 keyword tokens.
//!
//! Flat 128-entry table covering the entire $80..=$FF range so that every
//! high-bit byte has a textual form. Used keywords are the BASIC v2 set
//! ($80..=$CB); $CC..=$FE are unused on the C64 and round-trip as `{XX}`
//! placeholders; $FF is the unshifted-pi literal.
//!
//! Lookup is therefore total over the high-bit range — the detokenizer
//! never needs a fallback path for "unknown token".

/// Smallest byte value that introduces a keyword token.
pub const TOKEN_MIN: u8 = 0x80;

/// Prefix byte used by the extended keyword table.
pub const TSB_PREFIX: u8 = 0x64;

/// Textual form of every byte in $80..=$FF.
const TOKENS: [&str; 128] = [
    "END", "FOR", "NEXT", "DATA", // 80..83
    "INPUT#", "INPUT", "DIM", "READ", // 84..87
    "LET", "GOTO", "RUN", "IF", // 88..8B
    "RESTORE", "GOSUB", "RETURN", "REM", // 8C..8F
    "STOP", "ON", "WAIT", "LOAD", // 90..93
    "SAVE", "VERIFY", "DEF", "POKE", // 94..97
    "PRINT#", "PRINT", "CONT", "LIST", // 98..9B
    "CLR", "CMD", "SYS", "OPEN", // 9C..9F
    "CLOSE", "GET", "NEW", "TAB(", // A0..A3
    "TO", "FN", "SPC(", "THEN", // A4..A7
    "NOT", "STEP", "+", "-", // A8..AB
    "*", "/", "^", "AND", // AC..AF
    "OR", ">", "=", "<", // B0..B3
    "SGN", "INT", "ABS", "USR", // B4..B7
    "FRE", "POS", "SQR", "RND", // B8..BB
    "LOG", "EXP", "COS", "SIN", // BC..BF
    "TAN", "ATN", "PEEK", "LEN", // C0..C3
    "STR$", "VAL", "ASC", "CHR$", // C4..C7
    "LEFT$", "RIGHT$", "MID$", "GO", // C8..CB
    // $CC..$FE are unused on the C64; render as {XX} placeholders.
    "{CC}", "{CD}", "{CE}", "{CF}", "{D0}", "{D1}", "{D2}", "{D3}", "{D4}", "{D5}", "{D6}", "{D7}",
    "{D8}", "{D9}", "{DA}", "{DB}", "{DC}", "{DD}", "{DE}", "{DF}", "{E0}", "{E1}", "{E2}", "{E3}",
    "{E4}", "{E5}", "{E6}", "{E7}", "{E8}", "{E9}", "{EA}", "{EB}", "{EC}", "{ED}", "{EE}", "{EF}",
    "{F0}", "{F1}", "{F2}", "{F3}", "{F4}", "{F5}", "{F6}", "{F7}", "{F8}", "{F9}", "{FA}", "{FB}",
    "{FC}", "{FD}", "{FE}", "{PI}", // FF
];

/// Look up the textual form of any high-bit byte. Total over $80..=$FF.
pub fn keyword(byte: u8) -> &'static str {
    debug_assert!(byte >= TOKEN_MIN);
    TOKENS[(byte - TOKEN_MIN) as usize]
}

/// Map high-bit token aliases back to their logical id.
pub fn normalize_tsb_token(byte: u8) -> u8 {
    match byte {
        0xB3 | 0xB2 | 0xB1 => byte ^ 0x8F,
        other => other,
    }
}

/// Textual form for `$64 <id>` extended tokens.
///
/// Dialect aliases are normalized to `COLOR`, `CENTER`, `MOBCOL`.
pub fn tsb_keyword(byte: u8) -> Option<&'static str> {
    match normalize_tsb_token(byte) {
        0x01 => Some("HIRES"),
        0x02 => Some("PLOT"),
        0x03 => Some("LINE"),
        0x04 => Some("BLOCK"),
        0x05 => Some("FCHR"),
        0x06 => Some("FCOL"),
        0x07 => Some("FILL"),
        0x08 => Some("REC"),
        0x09 => Some("ROT"),
        0x0A => Some("DRAW"),
        0x0B => Some("CHAR"),
        0x0C => Some("HI COL"),
        0x0D => Some("INV"),
        0x0E => Some("FRAC"),
        0x0F => Some("MOVE"),
        0x10 => Some("PLACE"),
        0x11 => Some("UPB"),
        0x12 => Some("UPW"),
        0x13 => Some("LEFTW"),
        0x14 => Some("LEFTB"),
        0x15 => Some("DOWNB"),
        0x16 => Some("DOWNW"),
        0x17 => Some("RIGHTB"),
        0x18 => Some("RIGHTW"),
        0x19 => Some("MULTI"),
        0x1A => Some("COLOR"),
        0x1B => Some("MMOB"),
        0x1C => Some("BFLASH"),
        0x1D => Some("MOB SET"),
        0x1E => Some("MUSIC"),
        0x1F => Some("FLASH"),
        0x20 => Some("REPEAT"),
        0x21 => Some("PLAY"),
        0x22 => Some("DO"),
        0x23 => Some("CENTER"),
        0x24 => Some("ENVELOPE"),
        0x25 => Some("CGOTO"),
        0x26 => Some("WAVE"),
        0x27 => Some("FETCH"),
        0x28 => Some("AT("),
        0x29 => Some("UNTIL"),
        0x2C => Some("USE"),
        0x2E => Some("GLOBAL"),
        0x30 => Some("RESET"),
        0x31 => Some("PROC"),
        0x32 => Some("CALL"),
        0x33 => Some("EXEC"),
        0x34 => Some("END PROC"),
        0x35 => Some("EXIT"),
        0x36 => Some("END LOOP"),
        0x37 => Some("ON KEY"),
        0x38 => Some("DISABLE"),
        0x39 => Some("RESUME"),
        0x3A => Some("LOOP"),
        0x3B => Some("DELAY"),
        0x3C => Some("CLS"),
        0x3D => Some("X!"),
        0x3E => Some("MAP"),
        0x40 => Some("SECURE"),
        0x41 => Some("MOBCOL"),
        0x42 => Some("CIRCLE"),
        0x43 => Some("ON ERROR"),
        0x44 => Some("NO ERROR"),
        0x45 => Some("LOCAL"),
        0x46 => Some("RCOMP"),
        0x47 => Some("ELSE"),
        0x48 => Some("RETRACE"),
        0x49 => Some("TRACE"),
        0x4A => Some("DIR"),
        0x4B => Some("PAGE"),
        0x4C => Some("DUMP"),
        0x4D => Some("FIND"),
        0x4E => Some("OPTION"),
        0x4F => Some("AUTO"),
        0x50 => Some("OLD"),
        0x51 => Some("JOY"),
        0x52 => Some("MOD"),
        0x53 => Some("DIV"),
        0x54 => Some("D!"),
        0x55 => Some("DUP"),
        0x56 => Some("INKEY"),
        0x57 => Some("INST"),
        0x58 => Some("TEST"),
        0x59 => Some("LIN"),
        0x5A => Some("EXOR"),
        0x5B => Some("INSERT"),
        0x5C => Some("POT"),
        0x5D => Some("PENX"),
        0x5F => Some("PENY"),
        0x60 => Some("SOUND"),
        0x61 => Some("GRAPHICS"),
        0x62 => Some("DESIGN"),
        0x63 => Some("RLOCMOB"),
        0x64 => Some("CMOB"),
        0x65 => Some("BCKGNDS"),
        0x66 => Some("PAUSE"),
        0x67 => Some("NRM"),
        0x68 => Some("MOB"),
        0x69 => Some("OFF"),
        0x6A => Some("ANGL"),
        0x6B => Some("ARC"),
        0x6C => Some("COLD"),
        0x6D => Some("SCRSV"),
        0x6E => Some("SCRLD"),
        0x6F => Some("TEXT"),
        0x70 => Some("CSET"),
        0x71 => Some("VOL"),
        0x72 => Some("DISK"),
        0x73 => Some("HRDCPY"),
        0x74 => Some("KEY"),
        0x75 => Some("PAINT"),
        0x76 => Some("LOW COL"),
        0x77 => Some("COPY"),
        0x78 => Some("MERGE"),
        0x79 => Some("RENUMBER"),
        0x7A => Some("MEM"),
        0x7B => Some("DETECT"),
        0x7C => Some("CHECK"),
        0x7D => Some("DISPLAY"),
        0x7E => Some("ERR"),
        0x7F => Some("OUT"),
        _ => None,
    }
}
