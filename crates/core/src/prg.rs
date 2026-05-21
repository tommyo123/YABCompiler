//! Parse a tokenized C64 BASIC v2 `.prg` file.
//!
//! File layout:
//!   bytes 0..2     load address, little-endian (almost always $0801)
//!   then for each line:
//!     [2 bytes] absolute pointer to the next line (little-endian)
//!     [2 bytes] line number (little-endian, valid range 0..=63999)
//!     [n bytes] tokenized line body
//!     [1 byte ] $00 line terminator
//!   end of program is signalled by a next-line pointer whose *high byte*
//!   is zero (per C64 ROM behaviour); files in the wild typically use
//!   exactly $0000.
//!
//! The "next line pointer" is an *absolute* address that only makes sense
//! once the program has been loaded at its load address. We don't rely on
//! it for parsing — the NUL terminator is enough — but we expose it for
//! the dump command so the structure is easy to inspect.
//!
//! Line numbers are stored as `u16` here. Genuine BASIC v2 caps them at
//! 63999; we don't enforce that for listing purposes (any value LISTs
//! fine), but the eventual compiler will need to.

use std::fmt::Write as _;

use crate::petscii;
use crate::tokens::{self, TOKEN_MIN, TSB_PREFIX};

#[derive(Debug)]
pub enum ParseError {
    TooShort,
    UnterminatedLine { line_number: u16 },
    UnexpectedEof,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::TooShort => write!(f, "file is shorter than the 2-byte load address"),
            ParseError::UnterminatedLine { line_number } => {
                write!(f, "line {line_number} is missing its $00 terminator")
            }
            ParseError::UnexpectedEof => write!(f, "unexpected end of file inside a line header"),
        }
    }
}

impl std::error::Error for ParseError {}

#[derive(Debug)]
pub struct Line {
    pub next_ptr: u16,
    pub number: u16,
    pub body: Vec<u8>,
}

#[derive(Debug)]
pub struct Program {
    pub load_address: u16,
    pub lines: Vec<Line>,
}

impl Program {
    pub fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        if bytes.len() < 2 {
            return Err(ParseError::TooShort);
        }
        let load_address = u16::from_le_bytes([bytes[0], bytes[1]]);
        let mut cursor = 2usize;
        let mut lines = Vec::new();

        loop {
            // If the file ran out at a clean line boundary, treat it
            // as end-of-program. Some tokenized files omit the trailing
            // $0000 link.
            if cursor >= bytes.len() {
                break;
            }
            if cursor + 2 > bytes.len() {
                return Err(ParseError::UnexpectedEof);
            }
            let lo = bytes[cursor];
            let hi = bytes[cursor + 1];
            let next_ptr = u16::from_le_bytes([lo, hi]);
            cursor += 2;
            // End-of-program test matches the C64 ROM: the link's *high* byte
            // is what's checked. Real line addresses live at $0801+, so any
            // pointer with hi==0 is in zeropage and can't be a line — that's
            // what the ROM treats as the terminator. Most programs put $0000
            // here, but matching ROM semantics keeps us robust against the
            // odd hand-crafted file with hi=$00, lo!=$00.
            if hi == 0 {
                break;
            }
            if cursor + 2 > bytes.len() {
                return Err(ParseError::UnexpectedEof);
            }
            let number = u16::from_le_bytes([bytes[cursor], bytes[cursor + 1]]);
            cursor += 2;

            let start = cursor;
            while cursor < bytes.len() && bytes[cursor] != 0 {
                cursor += 1;
            }
            // The body ends at the first $00 or at end-of-file.
            // Unterminated last lines are accepted and end the program.
            let body = bytes[start..cursor].to_vec();
            if cursor < bytes.len() {
                cursor += 1; // consume the $00 terminator
            }

            lines.push(Line {
                next_ptr,
                number,
                body,
            });
        }

        Ok(Program {
            load_address,
            lines,
        })
    }

    pub fn detokenize(&self) -> String {
        let mut out = String::new();
        for line in &self.lines {
            writeln!(out, "{} {}", line.number, detokenize_body(&line.body))
                .expect("write to String");
        }
        out
    }

    pub fn dump(&self) -> String {
        let mut out = String::new();
        writeln!(out, "load address: ${:04X}", self.load_address).unwrap();
        writeln!(out, "lines:        {}", self.lines.len()).unwrap();
        writeln!(out).unwrap();
        for line in &self.lines {
            writeln!(
                out,
                "${:04X}  line {:>5}  {}",
                line.next_ptr,
                line.number,
                hex(&line.body)
            )
            .unwrap();
            writeln!(
                out,
                "                       {}",
                detokenize_body(&line.body)
            )
            .unwrap();
        }
        out
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        write!(s, "{b:02X}").unwrap();
    }
    s
}

/// Render a single tokenized line body as readable BASIC text.
///
/// Toggle quote mode on `"`; outside quotes, expand any byte ≥ $80 via the
/// keyword table and `$64 <id>` via the extended-token table. Everything
/// else is raw PETSCII passed through
/// `petscii::byte_to_string`.
///
/// REM and DATA need no special-case here. Bytes after either keyword stay
/// below $80 in the program stream and pass through naturally.
fn detokenize_body(body: &[u8]) -> String {
    let mut out = String::new();
    let mut quoted = false;
    let mut i = 0;
    while i < body.len() {
        let b = body[i];
        if b == b'"' {
            out.push('"');
            quoted = !quoted;
            i += 1;
            continue;
        }
        if !quoted && b == TSB_PREFIX {
            if let Some(&token) = body.get(i + 1) {
                if let Some(keyword) = tokens::tsb_keyword(token) {
                    out.push_str(keyword);
                } else {
                    write!(out, "{{{b:02X}}}{{{token:02X}}}").unwrap();
                }
                i += 2;
            } else {
                write!(out, "{{{b:02X}}}").unwrap();
                i += 1;
            }
            continue;
        }
        if !quoted && b >= TOKEN_MIN {
            out.push_str(tokens::keyword(b));
        } else {
            out.push_str(&petscii::byte_to_string(b));
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detokenize_expands_tsb_control_tokens() {
        let program = Program {
            load_address: 0x0801,
            lines: vec![Line {
                next_ptr: 0x080B,
                number: 10,
                body: vec![
                    TSB_PREFIX, 0x20, b':', TSB_PREFIX, 0x3A, b':', TSB_PREFIX, 0x36,
                ],
            }],
        };

        assert_eq!(program.detokenize(), "10 REPEAT:LOOP:END LOOP\n");
    }

    #[test]
    fn detokenize_uses_tsbneo_keyword_names() {
        let program = Program {
            load_address: 0x0801,
            lines: vec![Line {
                next_ptr: 0x0810,
                number: 10,
                body: vec![
                    TSB_PREFIX, 0x1A, b':', TSB_PREFIX, 0x23, b':', TSB_PREFIX, 0x41,
                ],
            }],
        };

        assert_eq!(program.detokenize(), "10 COLOR:CENTER:MOBCOL\n");
    }
}
