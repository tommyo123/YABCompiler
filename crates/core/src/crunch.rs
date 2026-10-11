//! Compress a program with a 6502 decoder that restores BASIC state before RUN.

use lzan_c64::{Decruncher, Direction, Format, Variant, first_safe_base};

/// Use the exact parse only for programs up to this size.
const OPTIMAL_PARSE_MAX: usize = 24 * 1024;

/// Load address of the packed and unpacked programs.
const BASIC_START: u32 = 0x0801;
/// Memory bank with all RAM visible during decoding.
const UNPACK_BANK: u8 = 0x30;
/// Memory bank with BASIC, KERNAL and I/O visible.
const RUN_BANK: u8 = 0x37;
/// Extra space for the decoder bit buffer.
const GAP_SLACK: u32 = 8;
/// Space reserved for the decoder and wrapper.
const STAGED_ROOM: u32 = 384;
/// Decoder address below the CPU vectors.
const UNDER_VECTORS: u32 = 0xFFFA - STAGED_ROOM;
/// Screen RAM fallback when no higher address is free.
const SCREEN: u32 = 0x0400;
/// LOAD cannot write RAM behind I/O.
const IO_START: u32 = 0xD000;

/// Compressed program and memory layout.
#[derive(Debug, Clone)]
pub struct SelfExtracting {
    /// Load address, startup code, decoder and packed bytes.
    pub prg: Vec<u8>,
    /// Unpacked bytes without the load address.
    pub unpacked_len: usize,
    /// Bytes of the packed stream.
    pub packed_len: usize,
    /// Where the decoder runs while unpacking.
    pub decoder_at: u16,
    /// First byte after the unpacked image, used for VARTAB.
    pub unpacked_end: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CrunchError {
    /// Shorter than a load address and one byte.
    TooShort,
    /// The program must load at $0801 for RUN.
    NotAtBasicStart(u16),
    /// The packed program does not fit in memory.
    DoesNotFit(String),
    /// Decoder assembly failed.
    Build(String),
}

impl std::fmt::Display for CrunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CrunchError::TooShort => write!(f, "Cannot compress an empty program."),
            CrunchError::NotAtBasicStart(load) => write!(
                f,
                "The program loads at ${load:04X}. Compression needs the default start address."
            ),
            CrunchError::DoesNotFit(why) => write!(f, "Cannot compress the program. {why}"),
            CrunchError::Build(why) => write!(f, "Cannot build the decoder. {why}"),
        }
    }
}

impl std::error::Error for CrunchError {}

/// Pack a PRG with its decoder.
pub fn self_extracting_prg(prg: &[u8]) -> Result<SelfExtracting, CrunchError> {
    if prg.len() < 3 {
        return Err(CrunchError::TooShort);
    }
    let load = u16::from_le_bytes([prg[0], prg[1]]);
    if u32::from(load) != BASIC_START {
        return Err(CrunchError::NotAtBasicStart(load));
    }
    let data = &prg[2..];
    let unpacked_end = BASIC_START + data.len() as u32;
    if unpacked_end > 0xFFFF {
        return Err(CrunchError::DoesNotFit(format!(
            "the program runs to ${:04X}, past the end of memory",
            unpacked_end - 1
        )));
    }

    // Choose compression effort by input size.
    let effort = if data.len() <= OPTIMAL_PARSE_MAX { 3 } else { 1 };
    let stream = lzan::zx::compress_min_eof_e(data, effort)[1..].to_vec();
    // Keep the write head clear of unread packed bytes.
    let need = lzan_c64::lzan_min_gap_forward(&stream) as u32 + GAP_SLACK;
    let fits_under = |top: u32| top >= unpacked_end + need && top - stream.len() as u32 > BASIC_START;
    if !fits_under(0x1_0000) {
        return Err(CrunchError::DoesNotFit(format!(
            "unpacking in place needs {need} free bytes above the program, which ends at ${:04X}",
            unpacked_end - 1
        )));
    }

    // Stage the decoder above the whole file.
    let file_end_estimate = BASIC_START + 64 + STAGED_ROOM + stream.len() as u32;
    let packed_start = 0x1_0000 - stream.len() as u32;
    let mut candidates = Vec::new();
    for (floor, ceiling) in [(BASIC_START, 0xA000), (0xC000, IO_START)] {
        let at = floor.max(unpacked_end).max(file_end_estimate);
        if at + STAGED_ROOM <= ceiling.min(packed_start) {
            candidates.push((at, 0x1_0000));
        }
    }
    // The required gap protects unread bytes during overlapping output.
    if fits_under(UNDER_VECTORS) {
        candidates.push((UNDER_VECTORS, UNDER_VECTORS));
    }
    candidates.push((SCREEN, 0x1_0000));

    let mut last_error = None;
    for (decoder_at, packed_end) in candidates {
        match build(&stream, data.len(), unpacked_end, decoder_at, packed_end) {
            Ok(file) => {
                let file_end = BASIC_START + file.len() as u32 - 2;
                if file_end > IO_START {
                    return Err(CrunchError::DoesNotFit(format!(
                        "the packed file runs to ${:04X}, and LOAD cannot write $D000-$DFFF",
                        file_end - 1
                    )));
                }
                // Keep the staged decoder clear of its source file.
                if decoder_at != SCREEN && decoder_at < file_end {
                    last_error = Some(CrunchError::DoesNotFit(format!(
                        "the decoder at ${decoder_at:04X} would overlap the packed file"
                    )));
                    continue;
                }
                return Ok(SelfExtracting {
                    prg: file,
                    unpacked_len: data.len(),
                    packed_len: stream.len(),
                    decoder_at: decoder_at as u16,
                    unpacked_end,
                });
            }
            Err(e) => last_error = Some(e),
        }
    }
    Err(last_error.unwrap_or_else(|| CrunchError::DoesNotFit("no room for the decoder".into())))
}

/// Assemble the self-extracting file with the decoder staged at `decoder_at`.
fn build(
    stream: &[u8],
    unpacked_len: usize,
    unpacked_end: u32,
    decoder_at: u32,
    packed_end: u32,
) -> Result<Vec<u8>, CrunchError> {
    let spec_zp = lzan_c64::find_routine(Format::LzanMin, Direction::Forward, Variant::OptSpeed)
        .ok_or_else(|| CrunchError::Build("The requested decoder is unavailable.".into()))?
        .zp_len;
    // Use BASIC temporary zero page bytes restored by CLR and RUN.
    let zp = first_safe_base(spec_zp, 0x07)
        .ok_or_else(|| CrunchError::Build("no safe zero page for the decoder".into()))?;
    let epilogue = format!(
        "        LDA #${RUN_BANK:02X}\n\
         \x20       STA $01\n\
         \x20       CLI\n\
         \x20       LDA #${:02X}\n\
         \x20       STA $2D\n\
         \x20       LDA #${:02X}\n\
         \x20       STA $2E\n\
         \x20       JSR $A659       ; Clear BASIC state\n\
         \x20       JMP $A7AE       ; Run the unpacked program\n",
        unpacked_end & 0xFF,
        unpacked_end >> 8,
    );
    let built = Decruncher::with_variant(Format::LzanMin, Direction::Forward, Variant::OptSpeed)
        .map_err(|e| CrunchError::Build(e.to_string()))?
        .basic_stub()
        .zero_page(zp)
        .packed_inline(stream.to_vec())
        .output(BASIC_START as u16)
        .output_len(unpacked_len as u16)
        .move_packed_to_top((packed_end - 1) as u16)
        .stage_decruncher_at(decoder_at as u16)
        .fold_mover_into_stage()
        // Restore memory banking and interrupts before CLR.
        .all_ram_with(UNPACK_BANK, None)
        .custom_post(&epilogue)
        .assemble()
        .map_err(|e| CrunchError::Build(e.to_string()))?;
    if u32::from(built.origin) != BASIC_START {
        return Err(CrunchError::Build(format!(
            "The decoder loads at ${:04X}. It must load at $0801.",
            built.origin
        )));
    }
    let mut file = Vec::with_capacity(built.bytes.len() + 2);
    file.extend_from_slice(&[BASIC_START as u8, (BASIC_START >> 8) as u8]);
    file.extend_from_slice(&built.bytes);
    Ok(file)
}
