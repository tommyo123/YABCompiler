//! PETSCII to Unicode conversion for the unshifted (upper-case + graphics)
//! character set used by C64 BASIC programs.
//!
//! This is intentionally a partial mapping focused on what shows up in
//! source code: printable ASCII range, common control codes, and a handful
//! of graphics characters. Anything we don't have a sensible mapping for is
//! rendered as `{$XX}` so the listing stays unambiguous.

/// Convert one PETSCII byte to its display form.
///
/// Printable 7-bit ASCII passes through. Letters in $41..=$5A are upper-case
/// in the unshifted character set; we keep them upper-case. Bytes in
/// $C1..=$DA are the same letters with the high bit set in screen codes —
/// in PETSCII they're shifted graphics, but in BASIC source they almost
/// always represent upper-case letters too, so we map them to A..Z.
pub fn byte_to_string(b: u8) -> String {
    match b {
        0x00 => "{null}".into(),
        0x0D => "\n".into(),
        0x20..=0x40 => (b as char).to_string(),
        0x41..=0x5A => (b as char).to_string(),
        0x5B..=0x5F => (b as char).to_string(),
        0x60 => "\u{2500}".into(), // horizontal line
        0xC1..=0xDA => ((b - 0x80) as char).to_string(),
        _ => format!("{{${b:02X}}}"),
    }
}
