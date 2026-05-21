//! Wrap assembled machine code in a runnable C64 `.prg` file.
//!
//! Layout, big picture:
//!
//! ```text
//!   file offset  contents
//!   ───────────  ───────────────────────────────────────────────
//!   0..2         load address $0801 (little-endian)
//!   2..14        BASIC line "10 SYS 2061" + end-of-program marker
//!   14..         machine code, originated at $080D = 2061
//! ```
//!
//! In memory once LOADed:
//!
//! ```text
//!   $0801..$0802   next-line link  $080B
//!   $0803..$0804   line number     10
//!   $0805          SYS token       $9E
//!   $0806..$0809   ASCII "2061"
//!   $080A          line terminator $00
//!   $080B..$080C   end-of-program  $0000
//!   $080D..        machine code
//! ```
//!
//! `RUN` reads "10 SYS 2061", interprets the address, JSRs into the
//! machine code at $080D. We exit via RTS back to BASIC's READY.
//!
//! If the SYS target ever needs to change (e.g. for a longer stub), update
//! both `STUB` and `codegen::CODE_ORIGIN` together.

const LOAD_ADDRESS: [u8; 2] = [0x01, 0x08];

/// 12-byte BASIC line: link to $080B, line 10, SYS, " 2061", $00 terminator,
/// then the $0000 end-of-program marker.
const STUB: [u8; 12] = [
    0x0B, 0x08, // next-line link → $080B
    0x0A, 0x00, // line number 10
    0x9E, // SYS token
    0x32, 0x30, 0x36, 0x31, // "2061"
    0x00, // end of line
    0x00, 0x00, // end of program
];

pub fn pack(machine_code: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + STUB.len() + machine_code.len());
    out.extend_from_slice(&LOAD_ADDRESS);
    out.extend_from_slice(&STUB);
    out.extend_from_slice(machine_code);
    out
}

/// Custom-start variant: emit just `[lo, hi]` load address + raw
/// machine code, no SYS launcher. The image LOADs at `load_addr` and
/// the user starts it manually (`SYS <load_addr>` from BASIC, or `JMP`
/// from ML). Used by the `--start-address` / GUI checkbox path.
pub fn pack_raw(load_addr: u16, machine_code: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + machine_code.len());
    out.push((load_addr & 0xFF) as u8);
    out.push((load_addr >> 8) as u8);
    out.extend_from_slice(machine_code);
    out
}
