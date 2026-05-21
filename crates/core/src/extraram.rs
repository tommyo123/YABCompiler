//! `--extraram` post-codegen rewrite: bank BASIC ROM out and route
//! every ROM call through a low jump table.
//!
//! C64 memory layout primer:
//!
//!   $A000-$BFFF  BASIC ROM when LORAM bit of $01 is set ($01=$37
//!                = the default after a fresh BASIC start). When the
//!                LORAM bit is cleared ($01=$36) the same address
//!                window reads RAM.
//!
//!   Writes to $A000-$BFFF always land in RAM regardless of LORAM.
//!
//! Strategy:
//!
//!   * The compiler keeps `$01 = $36` for the lifetime of the program,
//!     so the program owns all 8 KB of $A000-$BFFF.
//!   * Each ROM call is replaced by `JSR __JT_<name>`. The JT entry
//!     does `INC $01` (LORAM 0→1, ROM in), `JSR <rom-addr>`, `DEC $01`
//!     (LORAM 1→0, ROM out), `RTS`.
//!   * INC/DEC chosen over LDA/ORA-AND/STA: 5 cycles vs 8, no A
//!     clobber so wrappers don't need to PHA/PLA arguments.
//!   * The whole JT lives at the very top of the program text — at
//!     $080D after the SYS-stub — so JT entries are guaranteed below
//!     $A000 even if the program itself grows past that.
//!   * Data sections (string pool, float pool, var slots, FOR-counter
//!     slots, BSS buffers, runtime-array descriptors, …) are
//!     reordered to land immediately after the preamble. This puts
//!     every memory-pointer ROM operand into the low (always-RAM)
//!     range below `$9F00`, so the JT entries can be plain
//!     `INC/JSR/DEC/RTS` with no per-call buffer-copy fast path. The
//!     compiler asserts post-assembly that data ended below `$9F00`;
//!     otherwise it errors out.
//!   * Program exit restores `$01 = $37` so BASIC returns to its
//!     default environment.
//!
//! The rewrite runs after `peephole::run` so all liveness and JSR-pair
//! analyses see the raw ROM addresses they were designed against.
//! Only the final asm string is mutated here.

use std::collections::BTreeSet;
use std::fmt::Write as _;

/// Highest address (exclusive) at which any compiler-emitted data may
/// land. Set 256 bytes below the actual BASIC-ROM start ($A000) as
/// safety margin in case some helper grows or some piece of data
/// turns out to be slightly larger than the budget at the moment of
/// the assertion.
pub const DATA_CEILING: u16 = 0x9F00;

/// Label the compiler-injected wrapper places at the very end of the
/// reordered data section. After assembly, `lookup(DATA_END_LABEL)`
/// returns the next byte after the last data byte; if it exceeds
/// `DATA_CEILING`, the build fails.
pub const DATA_END_LABEL: &str = "__EXRAM_DATA_END";

/// Every ROM address the codegen ever emits inside the LORAM-controlled
/// window ($A000-$BFFF). KERNAL routines ($E000-$FFFF) are HIRAM-
/// controlled, stay live, and need no wrapper.
struct RomCall {
    addr: u16,
    label: &'static str,
}

/// All BASIC-ROM entries the compiler can emit. Order is irrelevant;
/// only entries actually referenced by the input asm get wrapped.
const ROM_CALLS: &[RomCall] = &[
    RomCall {
        addr: 0xAB1E,
        label: "STROUT",
    },
    RomCall {
        addr: 0xB391,
        label: "GIVAYF",
    },
    RomCall {
        addr: 0xBBA2,
        label: "MOVFM",
    },
    RomCall {
        addr: 0xBBD4,
        label: "MOVMF",
    },
    RomCall {
        addr: 0xB867,
        label: "FADD",
    },
    RomCall {
        addr: 0xB850,
        label: "FSUB",
    },
    RomCall {
        addr: 0xBA28,
        label: "FMULT",
    },
    RomCall {
        addr: 0xBB0F,
        label: "FDIV",
    },
    RomCall {
        addr: 0xBF7B,
        label: "FPWRT",
    },
    RomCall {
        addr: 0xBC0C,
        label: "FACARG",
    },
    RomCall {
        addr: 0xBC5B,
        label: "FCOMP",
    },
    RomCall {
        addr: 0xBDDD,
        label: "FOUT",
    },
    RomCall {
        addr: 0xBC58,
        label: "FN_ABS",
    },
    RomCall {
        addr: 0xBCCC,
        label: "FN_INT",
    },
    RomCall {
        addr: 0xBC39,
        label: "FN_SGN",
    },
    RomCall {
        addr: 0xBF71,
        label: "FN_SQR",
    },
    RomCall {
        addr: 0xB9EA,
        label: "FN_LOG",
    },
    RomCall {
        addr: 0xBFED,
        label: "FN_EXP",
    },
    RomCall {
        addr: 0xB7F7,
        label: "FACWORD",
    },
    RomCall {
        addr: 0xB3A2,
        label: "BYTEFAC",
    },
    RomCall {
        addr: 0xB7B5,
        label: "VAL_PARSE",
    },
    RomCall {
        addr: 0xBC9B,
        label: "QINT",
    },
    RomCall {
        addr: 0xAF08,
        label: "ERRSYN",
    },
    RomCall {
        addr: 0xA437,
        label: "BASIC_ERROR",
    },
    // Math functions that live above $BFFF in the C64 ROM. Their
    // entry points are outside the BASIC-ROM-bank-out window
    // ($A000-$BFFF), but every one of them calls back INTO that
    // window for FAC arithmetic (polynomial eval, FADD/FMUL,
    // exponent normalisation, etc.). Without wrapping, the call
    // reaches the entry point fine but the first internal JSR to
    // $A000-$BFFF lands on banked-out RAM (user code/data) and
    // execution wanders off — typical symptom is the program
    // exits to BASIC READY immediately on the first transcendental
    // call, with the error scrolled off-screen.
    RomCall {
        addr: 0xE097,
        label: "FN_RND",
    },
    RomCall {
        addr: 0xE26B,
        label: "FN_SIN",
    },
    RomCall {
        addr: 0xE264,
        label: "FN_COS",
    },
    RomCall {
        addr: 0xE2B4,
        label: "FN_TAN",
    },
    RomCall {
        addr: 0xE30E,
        label: "FN_ATN",
    },
    // BASIC's warm-start entry. Used by the compiled STOP handler
    // (after printing "BREAK IN <line>") to hand control to the
    // READY prompt forever. Entry is at $E37B in the KERNAL-ROM
    // window, but warm-start re-initialises the BASIC interpreter
    // — which lives at $A000-$BFFF. Without wrapping, STOP under
    // extraram lands on banked-out RAM during BASIC's reinit.
    // Wrapping with the standard JT pattern is correct even though
    // the call doesn't return: `INC $01` runs before the JSR, so
    // BASIC takes over with ROM banked in; the post-JSR
    // `DEC $01 / RTS` simply never runs.
    RomCall {
        addr: 0xE37B,
        label: "BASIC_WARM_START",
    },
];

fn rom_lookup(addr: u16) -> Option<&'static RomCall> {
    ROM_CALLS.iter().find(|c| c.addr == addr)
}

/// Returns true when `addr` is a ROM call that the JT must wrap.
///
/// Two cases:
/// 1. **Anywhere in `$A000-$BFFF`** — that whole range is RAM under
///    extraram, so any direct JSR/JMP into it lands on user data.
/// 2. **Specific math entry points above `$BFFF`** — RND/SIN/COS/TAN/ATN
///    live at `$E097`, `$E26B`, `$E264`, `$E2B4`, `$E30E` (in the
///    KERNAL-ROM half of the address space, outside the bank-out
///    window). Their entry points are reachable, but they call BACK
///    into BASIC ROM at `$A000-$BFFF` for FAC arithmetic. Without
///    wrapping, that internal callback hits banked-out RAM and
///    execution wanders off — observed as ARKTISK 0DE silently
///    returning to BASIC READY when `RND(.5)` ran on line 3017.
fn needs_jt_wrapping(addr: u16) -> bool {
    if (0xA000..=0xBFFF).contains(&addr) {
        return true;
    }
    rom_lookup(addr).is_some()
}

/// Top-level entry point used by `compile.rs`. Takes the post-peephole
/// asm and returns the rewritten asm with the JT injected and data
/// pulled to the low half of the binary.
pub fn inject(asm: &str) -> String {
    // Phase 1: scan for unique ROM addresses referenced by JSR/JMP.
    let mut used: BTreeSet<u16> = BTreeSet::new();
    for line in asm.lines() {
        if let Some(addr) = parse_rom_call(line) {
            if needs_jt_wrapping(addr) {
                used.insert(addr);
            }
        }
    }

    // Phase 2: split off the header (everything up to and including
    // the `*=$080D` directive) so we can re-emit the JT INSIDE the
    // originated section.
    let mut header_buffer = String::new();
    let mut body_buffer = String::new();
    let mut header_done = false;
    for line in asm.lines() {
        if !header_done {
            header_buffer.push_str(line);
            header_buffer.push('\n');
            if line.trim_start().starts_with("*=") {
                header_done = true;
            }
            continue;
        }
        // Rewrite ROM-targeting JSR/JMP to JT labels.
        match rewrite_line(line) {
            Some(rewritten) => {
                body_buffer.push_str(&rewritten);
                body_buffer.push('\n');
            }
            None => {
                body_buffer.push_str(line);
                body_buffer.push('\n');
            }
        }
    }

    // Phase 3: classify each block in the body as DATA or CODE so we
    // can hoist data above code. `__HEAP_BOTTOM` is special-cased to
    // remain at the very end (it marks the start of the runtime heap,
    // which lives BEYOND the program image).
    let blocks = split_into_blocks(&body_buffer);
    let mut data_blocks: Vec<Block> = Vec::new();
    let mut code_blocks: Vec<Block> = Vec::new();
    let mut heap_marker: Option<Block> = None;
    for block in blocks {
        if block.label.as_deref() == Some("__HEAP_BOTTOM:") {
            heap_marker = Some(block);
        } else if block.is_data() || is_data_pool_marker_label(block.label.as_deref()) {
            // The bare `__DATA:` label sits right above the first
            // `__DATA_LINE_<n>:` label and has no body of its own,
            // so `is_data()` returns false. Hoisting the data
            // bytes without `__DATA:` makes `__DATA_INIT` (which
            // does `LDA #<__DATA`) point at the LEFT-BEHIND empty
            // label far past the actual bytes — and `READ` then
            // walks unrelated memory. Treat the marker label as
            // data so it stays adjacent to its content.
            data_blocks.push(block);
        } else {
            code_blocks.push(block);
        }
    }

    // Phase 4: stitch it all together.
    let mut out = header_buffer;
    out.push_str(&emit_preamble(&used));
    out.push_str("; --- extraram: hoisted data sections ---\n");
    for b in &data_blocks {
        out.push_str(&b.text);
    }
    let _ = writeln!(out, "{DATA_END_LABEL}:");
    out.push_str("__EXRAM_CODE_START:\n");
    out.push_str("; --- end extraram hoisted data ---\n\n");
    for b in &code_blocks {
        out.push_str(&b.text);
    }
    if let Some(b) = heap_marker {
        out.push_str(&b.text);
    }

    // Phase 5 (debug-only): scan the user-code section for any direct
    // ROM call that *should* have been routed through the JT. This is
    // a safety net for new ROM addresses being added to runtime.rs
    // without a matching ROM_CALLS entry — without this, the bug would
    // only surface as a hard-to-debug crash on the first call once the
    // program grew big enough to trigger extraram. Guarded by
    // debug_assertions so it costs nothing in release builds.
    #[cfg(debug_assertions)]
    debug_assert_no_unwrapped_rom_calls(&out);

    out
}

/// Scan the post-inject asm for any direct `JSR/JMP $XXXX` that
/// targets an address inside the BASIC ROM bank-out window
/// ($A000-$BFFF) or one of the explicitly-listed `$E000+` math /
/// warm-start entries — but only in the USER CODE section, not the
/// JT preamble (whose entire purpose is to host the wrapped calls).
///
/// Panics with a clear diagnostic if any slip through. Catches
/// regressions where a new ROM helper is added to `runtime.rs` and
/// emitted by `codegen.rs` without a matching `ROM_CALLS` entry.
#[cfg(debug_assertions)]
fn debug_assert_no_unwrapped_rom_calls(asm: &str) {
    // The user code lives after `__EXRAM_CODE_START:`. The preamble
    // (where the JT entries live) is everything before that label.
    let Some(code_start) = asm.find("__EXRAM_CODE_START:") else {
        return;
    };
    let user_code = &asm[code_start..];
    let mut leaks: Vec<(usize, u16, &str)> = Vec::new();
    for (lineno, line) in user_code.lines().enumerate() {
        let Some(addr) = parse_rom_call(line) else {
            continue;
        };
        if needs_jt_wrapping(addr) {
            leaks.push((lineno, addr, line));
        }
    }
    if !leaks.is_empty() {
        let mut msg = String::from(
            "extraram: unwrapped ROM call(s) in user code section — \
             add an entry to `ROM_CALLS` for each address listed below \
             so the JT generator picks them up:\n",
        );
        for (lineno, addr, line) in leaks {
            let _ = writeln!(msg, "  line +{lineno}: ${addr:04X}  ({})", line.trim());
        }
        panic!("{msg}");
    }
}

/// If the line is `    JSR $XXXX` or `    JMP $XXXX`, return the
/// parsed `XXXX`. Tolerant of leading whitespace; rejects indirect
/// jumps (which don't target ROM in our codegen).
fn parse_rom_call(line: &str) -> Option<u16> {
    let trimmed = line.trim_start();
    let rest = trimmed
        .strip_prefix("JSR ")
        .or_else(|| trimmed.strip_prefix("JMP "))?;
    let rest = rest.trim_start();
    if rest.starts_with('(') {
        return None;
    }
    let rest = rest.strip_prefix('$')?;
    let hex: String = rest.chars().take(4).collect();
    if hex.len() != 4 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u16::from_str_radix(&hex, 16).ok()
}

/// Rewrite a single line to use the JT label when it targets an address
/// that needs banking. Pure KERNAL calls (CHROUT, GETIN, etc.) pass
/// through — they don't depend on the BASIC ROM bank state.
fn rewrite_line(line: &str) -> Option<String> {
    let addr = parse_rom_call(line)?;
    if !needs_jt_wrapping(addr) {
        return None;
    }
    let call = rom_lookup(addr)?;
    let stripped = line.trim_start();
    let leading = &line[..line.len() - stripped.len()];
    let mnemonic = if stripped.starts_with("JSR ") {
        "JSR"
    } else {
        "JMP"
    };
    Some(format!("{leading}{mnemonic} __JT_{}", call.label))
}

/// Build the JT, the entry-point wrapper that banks ROM out, and the
/// `JSR __EXRAM_CODE_START / INC $01 / RTS` exit sequence. `JSR` is
/// chosen over fall-through so the program's natural top-level RTS
/// returns OUT of __EXRAM_CODE_START — saving the trouble of finding
/// and rewriting the exit-RTS in the user code.
fn emit_preamble(used: &BTreeSet<u16>) -> String {
    let mut s = String::new();
    s.push_str("; --- extraram: bank BASIC ROM out, route ROM calls through low JT ---\n");
    s.push_str("    JMP __EXRAM_ENTRY\n");
    for &addr in used {
        let Some(call) = rom_lookup(addr) else {
            continue;
        };
        let _ = writeln!(s, "__JT_{}:", call.label);
        let _ = writeln!(s, "    INC $01");
        let _ = writeln!(s, "    JSR ${:04X}", call.addr);
        let _ = writeln!(s, "    DEC $01");
        let _ = writeln!(s, "    RTS");
    }
    s.push_str("__EXRAM_ENTRY:\n");
    s.push_str("    LDA #$36\n");
    s.push_str("    STA $01\n");
    s.push_str("    JSR __EXRAM_CODE_START\n");
    // INC bumps LORAM 0 → 1, leaving $01 = $37 — what BASIC expects
    // when it resumes from the SYS call. One byte cheaper than
    // `LDA #$37 / STA $01`, and no A clobber.
    s.push_str("    INC $01\n");
    s.push_str("    RTS\n");
    s.push_str("; --- end extraram preamble ---\n\n");
    s
}

/// One contiguous slab of asm — usually a label and the lines that
/// follow it until the next label. Section-comment lines that appear
/// between blocks attach to the FOLLOWING block so the resulting
/// section comments stay with their data when we reorder.
struct Block {
    /// The label that opens the block (e.g. `S0:`), if any. A block
    /// without a label is just leading text that didn't belong to any
    /// later block (typically the very first lines of the body).
    label: Option<String>,
    /// Full text of the block, including the label line, any leading
    /// section/blank comment lines, and the body lines until the next
    /// block boundary. Trailing newline preserved.
    text: String,
    /// Whether the block's body lines are pure data (`.byte`/`.word`).
    body_kind: BodyKind,
}

#[derive(PartialEq, Eq)]
enum BodyKind {
    /// All non-label, non-blank, non-comment lines are `.byte`/`.word`.
    Data,
    /// Any line is an opcode/instruction.
    Code,
    /// Block is empty (no body) — typically a marker label like
    /// `__HEAP_BOTTOM:`. Treated as code by `is_data` so it stays in
    /// the code section unless special-cased.
    Empty,
}

impl Block {
    fn is_data(&self) -> bool {
        matches!(self.body_kind, BodyKind::Data)
    }
}

/// Empty label that sits as a header above one or more data blocks.
/// `__DATA:` immediately precedes the first `__DATA_LINE_<n>:` and has
/// no body of its own — without this hint it would land in the code
/// section even though every byte it points at went to data.
fn is_data_pool_marker_label(label: Option<&str>) -> bool {
    matches!(label, Some("__DATA:"))
}

fn split_into_blocks(body: &str) -> Vec<Block> {
    // Two-stage parse:
    //   1. Walk the input and build a list of "raw block units": each is
    //      a label line plus the directive/opcode lines that follow it
    //      until the next label. Blank lines and `;` comments are NOT
    //      part of any unit; they collect into a "separator" buffer
    //      between units.
    //   2. Stitch separators onto units. A separator between unit U_n
    //      and unit U_{n+1} attaches to U_{n+1} (the convention is
    //      that section comments precede the section they describe).
    //      A separator at EOF is dropped (purely decorative).
    enum Tok {
        Label(String),
        Body(String, BodyKind),
        Separator(String),
    }

    let mut tokens: Vec<Tok> = Vec::new();
    let mut sep = String::new();
    for raw_line in body.lines() {
        let trimmed = raw_line.trim_start();
        if is_label(trimmed) {
            if !sep.is_empty() {
                tokens.push(Tok::Separator(std::mem::take(&mut sep)));
            }
            let mut s = String::new();
            s.push_str(raw_line);
            s.push('\n');
            tokens.push(Tok::Label(s));
        } else if trimmed.is_empty() || trimmed.starts_with(';') {
            sep.push_str(raw_line);
            sep.push('\n');
        } else {
            // A directive/opcode "owns" its preceding inline whitespace
            // (e.g. comments inside a routine). The separator becomes
            // part of the body text for the current open unit.
            let mut s = std::mem::take(&mut sep);
            s.push_str(raw_line);
            s.push('\n');
            let kind = classify_line(trimmed);
            tokens.push(Tok::Body(s, kind));
        }
    }
    // Trailing separator at EOF: drop.

    // Stitch tokens into Blocks. A block opens at a Label token and
    // closes when the next Label appears. Separators that immediately
    // precede a Label join the upcoming block (header for the next
    // section). Body tokens append to the current block's text and
    // update its kind.
    let mut blocks: Vec<Block> = Vec::new();
    let mut prefix = String::new();
    let mut cur_label: Option<String> = None;
    let mut cur_text = String::new();
    let mut cur_kind = BodyKind::Empty;

    let close = |label: Option<String>,
                 prefix: String,
                 text: String,
                 kind: BodyKind,
                 blocks: &mut Vec<Block>| {
        let mut full = prefix;
        full.push_str(&text);
        if !full.is_empty() || label.is_some() {
            blocks.push(Block {
                label,
                text: full,
                body_kind: kind,
            });
        }
    };

    for tok in tokens {
        match tok {
            Tok::Separator(s) => {
                // Park as prefix for the next unit. If a body token
                // follows before a label, it's claimed there instead.
                prefix.push_str(&s);
            }
            Tok::Label(label_line) => {
                close(
                    cur_label.take(),
                    String::new(),
                    std::mem::take(&mut cur_text),
                    std::mem::replace(&mut cur_kind, BodyKind::Empty),
                    &mut blocks,
                );
                // The prefix collected so far belongs to this new block.
                let mut block_text = std::mem::take(&mut prefix);
                block_text.push_str(&label_line);
                // Bare label name = label_line without trailing ":\n".
                let bare = label_line.trim_end_matches('\n').trim().to_string();
                cur_label = Some(bare);
                cur_text = block_text;
                cur_kind = BodyKind::Empty;
            }
            Tok::Body(body_line, kind) => {
                // Stitch any pending prefix INTO the current block
                // (it's inline whitespace within the routine).
                cur_text.push_str(&std::mem::take(&mut prefix));
                cur_text.push_str(&body_line);
                cur_kind = match (std::mem::replace(&mut cur_kind, BodyKind::Empty), kind) {
                    (BodyKind::Empty, k) => k,
                    (BodyKind::Data, BodyKind::Data) => BodyKind::Data,
                    (BodyKind::Data, _) | (_, BodyKind::Code) => BodyKind::Code,
                    (k, BodyKind::Empty) => k,
                    _ => BodyKind::Code,
                };
            }
        }
    }
    close(
        cur_label.take(),
        String::new(),
        cur_text,
        cur_kind,
        &mut blocks,
    );

    blocks
}

fn is_label(trimmed: &str) -> bool {
    if !trimmed.ends_with(':') {
        return false;
    }
    let body = &trimmed[..trimmed.len() - 1];
    if body.is_empty() {
        return false;
    }
    // A label is one identifier; reject anything containing whitespace
    // or operator characters that would mark it as something else
    // (though we don't expect those in our codegen).
    body.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !body.starts_with(|c: char| c.is_ascii_digit())
}

fn classify_line(trimmed: &str) -> BodyKind {
    if trimmed.starts_with(".byte") || trimmed.starts_with(".word") {
        BodyKind::Data
    } else {
        BodyKind::Code
    }
}

/// Verify that the assembled binary's data section ended below the
/// configured ceiling. Called from compile.rs after the assembler
/// has resolved all labels.
pub fn verify_data_fits<F>(lookup: F) -> Result<(), String>
where
    F: Fn(&str) -> Option<u16>,
{
    let data_end = lookup(DATA_END_LABEL)
        .ok_or_else(|| format!("--extraram: assembler did not resolve `{DATA_END_LABEL}` label"))?;
    if data_end > DATA_CEILING {
        return Err(format!(
            "--extraram: data sections grew to ${data_end:04X}, beyond the \
             ${DATA_CEILING:04X} ceiling. Reduce data (smaller arrays, fewer \
             string literals, less DATA) or run without --extraram."
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assemble(asm: &str) -> Vec<u8> {
        use asm6502::Assembler6502;
        let mut a = Assembler6502::new();
        a.assemble_bytes(asm).unwrap()
    }

    #[test]
    fn rewrites_jsr_to_jt_label() {
        let input = "*=$080D\n    JSR $B867\n    RTS\n";
        let out = inject(input);
        assert!(out.contains("JSR __JT_FADD"), "FADD JSR rewritten:\n{out}");
        assert!(out.contains("__JT_FADD:"));
        assert!(out.contains("INC $01"));
        assert!(out.contains("DEC $01"));
    }

    #[test]
    fn rewrites_jmp_to_jt_label() {
        let input = "*=$080D\n    JMP $BBA2\n";
        let out = inject(input);
        assert!(out.contains("JMP __JT_MOVFM"));
    }

    /// Regression: math functions whose entry points sit ABOVE the
    /// banked-out window ($A000-$BFFF) must still be wrapped, because
    /// they call BACK into BASIC ROM for FAC arithmetic. Without
    /// wrapping, the entry point is reachable but the first internal
    /// JSR to $A0xx-$BFxx hits banked-out RAM and the program wanders
    /// off — observed as ARKTISK 0DE silently exiting to BASIC READY
    /// the first time `RND(.5)` was evaluated under extraram.
    #[test]
    fn math_functions_above_bfff_get_wrapped() {
        for (addr, label) in [
            (0xE097u16, "FN_RND"),
            (0xE26B, "FN_SIN"),
            (0xE264, "FN_COS"),
            (0xE2B4, "FN_TAN"),
            (0xE30E, "FN_ATN"),
        ] {
            let input = format!("*=$080D\n    JSR ${addr:04X}\n    RTS\n");
            let out = inject(&input);
            let want_jsr = format!("JSR __JT_{label}");
            let want_def = format!("__JT_{label}:");
            assert!(
                out.contains(&want_jsr),
                "${addr:04X} ({label}) JSR rewritten to JT label\n{out}"
            );
            assert!(
                out.contains(&want_def),
                "${addr:04X} ({label}) JT entry defined\n{out}"
            );
            // And the original direct JSR must not survive — that's
            // exactly the buggy form that crashes under extraram.
            let direct = format!("    JSR ${addr:04X}");
            // The JT body itself contains the inner `JSR $XXXX`, so
            // we accept exactly one occurrence (the wrapper). Any
            // more would mean the user code's call slipped through.
            let count = out.matches(&direct).count();
            assert_eq!(
                count, 1,
                "expected exactly one inner `{direct}` (the JT wrapper); \
                 found {count}\n{out}"
            );
        }
    }

    /// BASIC_WARM_START ($E37B) is special: a non-returning JMP
    /// from the STOP handler. Entry sits in the KERNAL window but
    /// warm-start re-runs BASIC interpreter init code at $A000+. The
    /// JT wrapper banks ROM in *before* the JSR, so the never-
    /// returning call enters BASIC with ROM mapped — the trailing
    /// `DEC $01 / RTS` never executes, which is fine.
    #[test]
    fn basic_warm_start_gets_wrapped() {
        let input = "*=$080D\n    JMP $E37B\n";
        let out = inject(input);
        assert!(
            out.contains("JMP __JT_BASIC_WARM_START"),
            "STOP's warm-start jump must route through JT\n{out}"
        );
        assert!(
            out.contains("__JT_BASIC_WARM_START:"),
            "JT entry for warm-start must be defined\n{out}"
        );
    }

    /// KERNAL routines (CHROUT $FFD2, GETIN $FFE4, etc.) live above
    /// $E000 too, but they don't call back into BASIC ROM and the
    /// KERNAL bank stays mapped during extraram — so they must NOT
    /// be wrapped (wrapping would needlessly INC/DEC $01 around
    /// every CHROUT).
    #[test]
    fn kernal_calls_pass_through_unwrapped() {
        let input = "*=$080D\n    JSR $FFD2\n    RTS\n";
        let out = inject(input);
        assert!(
            out.contains("    JSR $FFD2"),
            "KERNAL CHROUT must pass through unwrapped\n{out}"
        );
        assert!(
            !out.contains("__JT_") || !out.contains("$FFD2"),
            "no JT entry for CHROUT\n{out}"
        );
    }

    #[test]
    fn jt_entries_have_no_buffer() {
        // After dropping the auto-buffer fast path, every JT entry
        // is a plain INC/JSR/DEC/RTS. The CPY #$A0 buffer test must
        // not appear anywhere in the rewritten asm.
        let input = "*=$080D\n    JSR $B867\n    RTS\n";
        let out = inject(input);
        assert!(!out.contains("CPY #$A0"));
        assert!(!out.contains("__EXRAM_BUF"));
    }

    #[test]
    fn skip_jump_first_in_section() {
        let input = "*=$080D\n    JSR $B867\n    RTS\n";
        let out = inject(input);
        let after_origin = out.split("*=$080D\n").nth(1).unwrap();
        let first_instr = after_origin
            .lines()
            .find(|l| {
                let t = l.trim_start();
                !t.is_empty() && !t.starts_with(';') && !t.starts_with('.')
            })
            .unwrap();
        assert!(first_instr.contains("JMP __EXRAM_ENTRY"));
    }

    #[test]
    fn entry_jsrs_into_code_start() {
        // The entry label `__EXRAM_ENTRY` banks ROM out then JSRs
        // directly into `__EXRAM_CODE_START` (which is below all
        // hoisted data). Jumping over data this way avoids a
        // separate JMP-around.
        let input = "*=$080D\n    JSR $B867\n    RTS\n";
        let out = inject(input);
        let entry = out.split("__EXRAM_ENTRY:").nth(1).unwrap();
        assert!(entry.contains("LDA #$36"));
        assert!(entry.contains("JSR __EXRAM_CODE_START"));
        assert!(entry.contains("INC $01"));
    }

    #[test]
    fn section_comment_attaches_to_following_label_not_previous() {
        // Mirrors the actual codegen layout where a section comment
        // sits between two blocks: it must attach to the FOLLOWING
        // block, not stay as a trailing comment on the previous one.
        let input = "\
*=$080D
__VAL_ZERO:
    LDA #$00
    JMP $B391

; --- string pool (length-prefixed) ---
S0:
    .byte $05, $48, $49, $0D, $00

; --- peephole-factored helpers ---
__CHROUT_3F:
    LDA #$3F
    JMP $FFD2
";
        let out = inject(input);
        // The data section (S0:) must carry its `; --- string pool ---`
        // header. The code section's __CHROUT_3F: must carry its
        // `; --- peephole-factored helpers ---` header. Comments
        // must NOT be swapped or orphaned.
        let s0_idx = out.find("S0:").expect("S0 in output");
        let strpool_idx = out
            .find("; --- string pool")
            .expect("string-pool comment present");
        let cr_idx = out.find("__CHROUT_3F:").expect("CHROUT_3F in output");
        let pe_idx = out
            .find("; --- peephole")
            .expect("peephole comment present");
        assert!(
            strpool_idx < s0_idx,
            "string-pool comment precedes S0:\n{out}"
        );
        assert!(
            pe_idx < cr_idx,
            "peephole comment precedes CHROUT_3F:\n{out}"
        );
        // Both comments should appear EXACTLY ONCE.
        assert_eq!(
            out.matches("; --- string pool").count(),
            1,
            "string-pool comment unique:\n{out}"
        );
        assert_eq!(
            out.matches("; --- peephole").count(),
            1,
            "peephole comment unique:\n{out}"
        );
    }

    #[test]
    fn data_block_hoisted_above_code() {
        // A program that emits both code (LDA, JSR, RTS) and data
        // (S0 + .byte) must, after the rewrite, place the data
        // section S0 BEFORE the user-code lines that reference it.
        let input = "\
*=$080D
    LDA #<S0
    LDY #>S0
    JSR $AB1E
    RTS

S0:
    .byte $05, $48, $45, $4C, $4C, $4F
";
        let out = inject(input);
        let s0_pos = out.find("S0:").expect("S0 label present");
        // The user code starts at __EXRAM_CODE_START (the boundary
        // emitted by inject after the hoisted data).
        let code_start = out
            .find("__EXRAM_CODE_START:")
            .expect("CODE_START boundary");
        let lda_pos = out.find("    LDA #<S0").expect("LDA #<S0 present in body");
        // S0 must appear BEFORE the boundary, and user code (the
        // LDA instruction) must appear AFTER the boundary.
        assert!(s0_pos < code_start, "S0 above CODE_START:\n{out}");
        assert!(lda_pos > code_start, "LDA below CODE_START:\n{out}");
        assert!(out.contains("__EXRAM_DATA_END:"));
    }

    #[test]
    fn heap_bottom_stays_at_end() {
        let input = "\
*=$080D
    JSR $AB1E
    RTS

S0:
    .byte $00

__HEAP_BOTTOM:
";
        let out = inject(input);
        // `__HEAP_BOTTOM:` must be the last non-blank line of the
        // rewritten asm — it marks where the runtime heap starts.
        let last = out.lines().filter(|l| !l.trim().is_empty()).last().unwrap();
        assert!(
            last.contains("__HEAP_BOTTOM:"),
            "last line is heap bottom: {last}"
        );
    }

    #[test]
    fn passes_through_non_rom_calls() {
        let input = "*=$080D\n    JSR $FFD2\n    RTS\n";
        let out = inject(input);
        assert!(out.contains("JSR $FFD2"));
        assert!(!out.contains("__JT_"));
    }

    #[test]
    fn unused_rom_targets_emit_no_jt() {
        let input = "*=$080D\n    JSR $B867\n    RTS\n";
        let out = inject(input);
        assert!(out.contains("__JT_FADD"));
        assert!(!out.contains("__JT_FSUB"));
        assert!(!out.contains("__JT_FOUT"));
    }

    #[test]
    fn assembles_clean() {
        let input = "\
*=$080D
    JSR $B867
    JSR $BBA2
    JSR $BDDD
    JSR $FFD2
    RTS

S0:
    .byte $05,$48,$49,$0D,$00
";
        let out = inject(input);
        let bytes = assemble(&out);
        assert!(
            bytes.len() > 10,
            "produced non-trivial output: {} bytes",
            bytes.len()
        );
    }

    #[test]
    fn data_ceiling_check_passes_for_small_program() {
        // A trivial program's data section ends well below $9F00.
        // Verify the helper accepts it.
        verify_data_fits(|name| {
            if name == DATA_END_LABEL {
                Some(0x0820)
            } else {
                None
            }
        })
        .expect("0820 is below ceiling");
    }

    #[test]
    fn data_ceiling_check_fails_for_overflow() {
        // Data ending at $9F01 = above ceiling. Helper must reject.
        let err = verify_data_fits(|name| {
            if name == DATA_END_LABEL {
                Some(0x9F01)
            } else {
                None
            }
        })
        .expect_err("ceiling violation should be caught");
        assert!(err.contains("9F01"), "{err}");
        assert!(err.contains("9F00"));
    }

    #[test]
    fn data_ceiling_check_fails_when_label_missing() {
        let err = verify_data_fits(|_| None).expect_err("missing label is an error");
        assert!(err.contains("did not resolve"));
    }
}
