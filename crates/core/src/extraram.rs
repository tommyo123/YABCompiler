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

/// Highest address (exclusive) at which any compiler-emitted data may land: the first byte of the BASIC-ROM window.
pub const DATA_CEILING: u16 = 0xA000;

/// One past the last address that reads as BASIC ROM when the JT has banked it in ; the top of the `$A000-$BFFF` window.
pub const ROM_WINDOW_TOP: u16 = 0xC000;

pub const HEAP_CEILING: u16 = 0xD000;

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
        addr: 0xAB3B,
        label: "PRINT_GAP",
    },
    RomCall {
        addr: 0xB391,
        label: "GIVAYF",
    },
    RomCall {
        addr: 0xBC2B,
        label: "SIGN",
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
    // MUL10 and ADD_SIGNED_BYTE are the two entries `__SET_TI` reaches to accumulate `TI$` the way the ROM's own writer at $A9E7-$AA0E does.
    RomCall {
        addr: 0xBAE2,
        label: "MUL10",
    },
    RomCall {
        addr: 0xBD7E,
        label: "ADD_SIGNED_BYTE",
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
        addr: 0xBDCD,
        label: "LINPRT",
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
        addr: 0xB1BF,
        label: "AYINT",
    },
    RomCall {
        addr: 0xB3A2,
        label: "BYTEFAC",
    },
    RomCall {
        addr: 0xB7B5,
        label: "VAL_PARSE",
    },
    // FIN, called directly by `__INPUT_VAL` because VAL_PARSE throws away where the parse stopped and that position IS the `?REDO FROM START` test.
    RomCall {
        addr: 0xBCF3,
        label: "FIN",
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
    // $A46C prints " IN <line>" and READY. Used after a custom error message.
    RomCall {
        addr: 0xA46C,
        label: "BASIC_ERROR_TAIL",
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

/// The store a user `POKE 1,<v>` lowers to.
const PORT01_STORE: &str = "STA $0001";

const PORT01_LORAM_MASK: u8 = 0xFE;

/// Delimiters `codegen::bss_fill_block` wraps the entry-time zero-fill in, plus the two labels
/// it measures.
const BSS_FILL_OPEN: &str = "; --- BSS zero-fill ---";
const BSS_FILL_CLOSE: &str = "; --- end BSS zero-fill ---";

/// True iff [`strip_bss_tail_fill`] has anything to remove.
pub(crate) fn has_bss_tail_fill(asm: &str) -> bool {
    asm.contains(BSS_FILL_OPEN)
}

/// Drop the BSS zero-fill loop and its `__BSS_START:` / `__BSS_END:` brackets, returning the
/// asm to its "zeros are stored in the image" form.
pub(crate) fn strip_bss_tail_fill(asm: &str) -> String {
    if !asm.contains(BSS_FILL_OPEN) {
        return asm.to_string();
    }
    let mut out = String::with_capacity(asm.len());
    let mut inside = false;
    for line in asm.split_inclusive('\n') {
        let trimmed = line.trim();
        if trimmed == BSS_FILL_OPEN {
            inside = true;
            continue;
        }
        if inside {
            inside = trimmed != BSS_FILL_CLOSE;
            continue;
        }
        if trimmed == "__BSS_START:" || trimmed == "__BSS_END:" {
            continue;
        }
        out.push_str(line);
    }
    out
}

pub(crate) fn has_val_variants(asm: &str) -> bool {
    asm.contains(crate::codegen::VAL_COPY_OPEN)
}

pub(crate) fn select_val_variant(asm: &str, copy: bool) -> String {
    use crate::codegen::{
        VAL_BUF_CLOSE, VAL_BUF_OPEN, VAL_COPY_CLOSE, VAL_COPY_OPEN, VAL_INPLACE_CLOSE,
        VAL_INPLACE_OPEN,
    };
    if !asm.contains(VAL_COPY_OPEN) {
        return asm.to_string();
    }

    let blocks: [(&str, &str, bool); 3] = [
        (VAL_COPY_OPEN, VAL_COPY_CLOSE, copy),
        (VAL_INPLACE_OPEN, VAL_INPLACE_CLOSE, !copy),
        (VAL_BUF_OPEN, VAL_BUF_CLOSE, copy),
    ];
    let mut out = String::with_capacity(asm.len());
    // `dropping` is Some(close-sentinel) while inside a block whose body is being removed.
    let mut dropping: Option<&str> = None;
    for line in asm.split_inclusive('\n') {
        let trimmed = line.trim();
        if let Some(close) = dropping {
            if trimmed == close {
                dropping = None;
            }
            continue;
        }
        if let Some(&(_, close, keep)) = blocks.iter().find(|(open, _, _)| trimmed == *open) {
            if !keep {
                dropping = Some(close);
            }

            continue;
        }
        if blocks.iter().any(|(_, close, _)| trimmed == *close) {
            continue;
        }
        out.push_str(line);
    }
    out
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

/// Top-level entry point used by `compile.rs`.
pub fn inject(asm: &str) -> String {
    inject_with_reserved(asm, &[], false)
}

/// The heap ceiling this layout gets, given the ranges the assembler has been told to keep clear.
pub(crate) fn heap_ceiling(reserved: &[(u16, u16)], heap_bottom: u32) -> u16 {
    reserved
        .iter()
        .filter(|&&(_, hi)| u32::from(hi) >= heap_bottom)
        .map(|&(lo, _)| lo)
        .filter(|&lo| lo >= DATA_CEILING)
        .min()
        .map_or(HEAP_CEILING, |lo| lo.min(HEAP_CEILING))
}

/// The final heap floor is known only after linking.
pub(crate) fn refine_heap_ceiling(
    asm: &str,
    reserved: &[(u16, u16)],
    heap_bottom: u32,
) -> Option<String> {
    let initial = heap_ceiling(reserved, u32::from(DATA_CEILING));
    let settled = heap_ceiling(reserved, heap_bottom);
    if initial == settled || !asm.contains("__EXRAM_MEMSIZ_RAISE:\n") {
        return None;
    }
    let old = format!("__EXRAM_MEMSIZ_RAISE:\n{}", memsiz_raise(initial));
    let new = format!("__EXRAM_MEMSIZ_RAISE:\n{}", memsiz_raise(settled));
    assert!(asm.contains(&old), "generated MEMSIZ raise has its initial ceiling");
    Some(asm.replacen(&old, &new, 1))
}

pub fn inject_with_reserved(
    asm: &str,
    reserved: &[(u16, u16)],
    program_touches_memsiz: bool,
) -> String {

    let asm = &strip_bss_tail_fill(asm);
    // Phase 1: scan for unique ROM addresses referenced by JSR/JMP.
    let mut used: BTreeSet<u16> = BTreeSet::new();
    for line in asm.lines() {
        if let Some(addr) = parse_rom_call(line) {
            if needs_jt_wrapping(addr) {
                used.insert(addr);
            }
        }
    }

    let heap_ceil = (asm.contains("__HEAP_BOTTOM:") && !program_touches_memsiz)
        .then(|| heap_ceiling(reserved, u32::from(DATA_CEILING)));
    // The `SYS` wrappers are a couple of dozen bytes that most programs have no site for.
    let has_sys_wrap = asm.contains("; YAB_SYS_WRAP_IN");
    let has_ierror = asm.contains("__TSB_IERROR_ENTRY:");
    let has_sys_strings = asm.contains("__SYS_EVAL_ENTER:");
    let inplace_pinned = asm
        .lines()
        .any(|l| l.trim() == crate::compile::INPLACE_PIN_END);
    let mut header_buffer = String::new();
    let mut body_buffer = String::new();
    let mut header_done = false;
    let mut pinning = false;
    let mut tramps = SysTrampolines::default();
    let mut in_usr_vector = false;
    let mut has_usr_wrap = false;

    let mut wrapped: Option<Vec<String>> = None;
    for line in asm.lines() {
        if !header_done {
            header_buffer.push_str(line);
            header_buffer.push('\n');
            if line.trim_start().starts_with("*=") {
                header_done = true;
                pinning = inplace_pinned;
            }
            continue;
        }
        if pinning {
            header_buffer.push_str(line);
            header_buffer.push('\n');
            if line.trim() == crate::compile::INPLACE_PIN_END {
                pinning = false;
            }
            continue;
        }
        let trimmed = line.trim();
        if trimmed.ends_with(':') {
            in_usr_vector = trimmed == "__USR_VECTORED:";
        }
        // Only the compiler's USR dispatch tail is an external call.
        if in_usr_vector && trimmed == "JMP ($0311)" {
            body_buffer.push_str("    JMP __EXRAM_USRJT\n");
            has_usr_wrap = true;
            continue;
        }
        if trimmed == "; YAB_SYS_WRAP_IN" {
            if heap_ceil.is_some() {
                let _ = writeln!(body_buffer, "    JSR {SYS_IN}");
            }
            wrapped = Some(Vec::new());
            continue;
        }
        if trimmed == "; YAB_SYS_WRAP_OUT" {

            let inner = wrapped.take().unwrap_or_default();
            let _ = writeln!(body_buffer, "    JSR {}", tramps.intern(inner));
            if heap_ceil.is_some() {
                let _ = writeln!(body_buffer, "    JSR {SYS_OUT}");
            }
            continue;
        }

        if trimmed == PORT01_STORE {
            match &mut wrapped {
                Some(buf) => buf.push(format!("    AND #${PORT01_LORAM_MASK:02X}")),
                None => {
                    let _ = writeln!(body_buffer, "    AND #${PORT01_LORAM_MASK:02X}");
                }
            }
        }
        // Rewrite ROM-targeting JSR/JMP to JT labels.
        let rewritten = rewrite_line(line, heap_ceil).unwrap_or_else(|| line.to_string());

        let rewritten = if has_ierror {
            rewritten
                .replace("#<__TSB_IERROR_ENTRY", "#<__EXRAM_IERROR_ENTRY")
                .replace("#>__TSB_IERROR_ENTRY", "#>__EXRAM_IERROR_ENTRY")
        } else {
            rewritten
        };
        let rewritten = if has_sys_strings {
            let s = rewritten
                .replace("#<__SYS_EVAL_ENTER", "#<__EXRAM_SYS_EVAL_ENTER")
                .replace("#>__SYS_EVAL_ENTER", "#>__EXRAM_SYS_EVAL_ENTER")
                .replace("#<__SYS_EVAL_IERROR", "#<__EXRAM_SYS_EVAL_IERROR")
                .replace("#>__SYS_EVAL_IERROR", "#>__EXRAM_SYS_EVAL_IERROR");
            match s.trim() {
                "__SYS_EVAL_CHAIN:" => format!("{s}\n    JMP __EXRAM_SYS_EVAL_CHAIN"),
                "__SYS_EVAL_RETURN:" => format!("{s}\n    JMP __EXRAM_SYS_EVAL_RETURN"),
                "; YAB_SYS_EVAL_ERROR_CLEAN" if heap_ceil.is_some() =>
                    format!("{s}\n    JSR {MEMSIZ_APPLY}"),
                _ => s,
            }
        } else {
            rewritten
        };
        match &mut wrapped {
            Some(buf) => buf.push(rewritten),
            None => {
                body_buffer.push_str(&rewritten);
                body_buffer.push('\n');
            }
        }
    }

    tramps.redirect_patch_stores(&mut body_buffer);

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
        } else if block.is_data() || is_data_anchor_label(block.label.as_deref()) {

            data_blocks.push(block);
        } else {
            code_blocks.push(block);
        }
    }

    // Phase 4: stitch it all together.
    let mut out = header_buffer;
    out.push_str(&emit_preamble(
        &used,
        heap_ceil,
        has_sys_wrap,
        has_usr_wrap,
        has_ierror,
        has_sys_strings,
        &tramps,
    ));
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

    assert_no_unwrapped_rom_calls(&out);
    #[cfg(debug_assertions)]
    debug_assert_data_anchors_hoisted(&out);

    out
}

/// Check that data anchors and variable bounds move together above the extra-RAM code.
#[cfg(debug_assertions)]
fn debug_assert_data_anchors_hoisted(asm: &str) {
    let Some(code_start) = asm.find("__EXRAM_CODE_START:") else {
        return;
    };
    for anchor in DATA_ANCHOR_LABELS {
        // Match at line start so `__DATA:` doesn't match inside `__DATA_END:` or a comment.
        let Some(pos) = find_label_line(asm, anchor) else {
            continue;
        };
        assert!(
            pos < code_start,
            "extraram: data anchor `{anchor}` was left behind in the code \
             section — it must hoist with the pool it brackets. Add it to \
             `is_data_anchor_label`."
        );
    }
    if let (Some(start), Some(end)) = (
        find_label_line(asm, "__VARS_START:"),
        find_label_line(asm, "__VARS_END:"),
    ) {
        assert!(
            start < end,
            "extraram: `__VARS_START` must precede `__VARS_END`"
        );
        assert!(
            asm[start..end].contains(".byte"),
            "extraram: `__VARS_START`..`__VARS_END` encloses no data — \
             __STATE_RESET would memzero 0 bytes and CLR/RUN would clear \
             nothing:\n{}",
            &asm[start..end]
        );
    }
}

/// Byte offset of the line that consists solely of `label` (modulo leading whitespace), or `None`.
#[cfg(any(debug_assertions, test))]
fn find_label_line(asm: &str, label: &str) -> Option<usize> {
    let mut offset = 0usize;
    for line in asm.lines() {
        if line.trim() == label {
            return Some(offset);
        }
        offset += line.len() + 1;
    }
    None
}

fn assert_no_unwrapped_rom_calls(asm: &str) {
    let leaks = unwrapped_rom_calls(asm);
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

fn unwrapped_rom_calls(asm: &str) -> Vec<(usize, u16, &str)> {
    // The user code lives after `__EXRAM_CODE_START:`. The preamble
    // (where the JT entries live) is everything before that label.
    let Some(code_start) = asm.find("__EXRAM_CODE_START:") else {
        return Vec::new();
    };
    let user_code = &asm[code_start..];
    let mut leaks = Vec::new();
    for (lineno, line) in user_code.lines().enumerate() {
        let Some(addr) = parse_rom_call(line) else {
            continue;
        };
        if needs_jt_wrapping(addr) {
            leaks.push((lineno, addr, line));
        }
    }
    leaks
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

/// The `SYS` trampolines, one per distinct call, collected while the body is scanned and emitted into the preamble.
#[derive(Default)]
struct SysTrampolines {
    /// `(inner lines, label)`, in first-seen order.
    entries: Vec<(Vec<String>, String)>,

    patched_sites: Vec<String>,
}

const SYS_IND_TRAMP: &str = "__EXRAM_SYSJT_IND";
const SYS_IND_PATCH: &str = "__EXRAM_SYS_PATCHED";

impl SysTrampolines {
    /// Label for this call, creating the trampoline on first sight.
    fn intern(&mut self, inner: Vec<String>) -> String {
        if let Some(label) = patched_jsr_label(&inner) {
            self.patched_sites.push(label);
            return SYS_IND_TRAMP.to_string();
        }
        if let Some((_, label)) = self.entries.iter().find(|(k, _)| *k == inner) {
            return label.clone();
        }
        let label = format!("__EXRAM_SYSJT_{}", self.entries.len());
        self.entries.push((inner, label.clone()));
        label
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.patched_sites.is_empty()
    }

    fn redirect_patch_stores(&self, body: &mut String) {
        for label in &self.patched_sites {
            *body = body
                .replace(&format!("{label}+1"), &format!("{SYS_IND_PATCH}+1"))
                .replace(&format!("{label}+2"), &format!("{SYS_IND_PATCH}+2"));
        }
    }

    fn render(&self) -> String {
        let mut s = String::new();
        if !self.patched_sites.is_empty() {
            let _ = writeln!(s, "{SYS_IND_TRAMP}:");
            s.push_str("    INC $01\n");
            let _ = writeln!(s, "{SYS_IND_PATCH}:");
            s.push_str("    JSR $FFFF\n");
            s.push_str("    DEC $01\n");
            s.push_str("    RTS\n");
        }
        for (inner, label) in &self.entries {
            let _ = writeln!(s, "{label}:");
            s.push_str("    INC $01\n");
            for l in inner {
                if l.trim().is_empty() {
                    continue;
                }
                s.push_str(l);
                s.push('\n');
            }
            s.push_str("    DEC $01\n");
            s.push_str("    RTS\n");
        }
        s
    }
}

fn patched_jsr_label(inner: &[String]) -> Option<String> {
    if !inner.iter().any(|l| l.trim() == "JSR $FFFF") {
        return None;
    }
    inner
        .iter()
        .map(|l| l.trim())
        .find(|l| l.ends_with(':'))
        .map(|l| l.trim_end_matches(':').to_string())
}

/// Rewrite a single line to use the JT label when it targets an address that needs banking.
fn rewrite_line(line: &str, _ceiling: Option<u16>) -> Option<String> {
    debug_assert!(
        !matches!(
            line.trim(),
            "; YAB_SYS_WRAP_IN" | "; YAB_SYS_WRAP_OUT"
        ),
        "the SYS bank markers are the body loop's business, not this function's"
    );
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

/// Build the JT, the entry-point wrapper that banks ROM out, and the `JSR __EXRAM_CODE_START / INC $01 / RTS` exit sequence.
fn emit_preamble(
    used: &BTreeSet<u16>,
    ceiling: Option<u16>,
    has_sys: bool,
    has_usr: bool,
    has_ierror: bool,
    has_sys_strings: bool,
    tramps: &SysTrampolines,
) -> String {
    let mut s = String::new();
    s.push_str("; --- extraram: bank BASIC ROM out, route ROM calls through low JT ---\n");
    s.push_str("    JMP __EXRAM_ENTRY\n");
    if has_sys_strings {
        s.push_str("__EXRAM_SYS_EVAL_ENTER:\n    PHP\n    PHA\n    TXA\n    PHA\n    TYA\n    PHA\n    LDA $01\n    STA __SYS_EVAL_PORT\n    AND #$FE\n    STA $01\n    JMP __SYS_EVAL_DISPATCH\n");
        s.push_str("__EXRAM_SYS_EVAL_CHAIN:\n    LDA __SYS_EVAL_PORT\n    STA $01\n    PLA\n    TAY\n    PLA\n    TAX\n    PLA\n    PLP\n    JMP (__SYS_EVAL_PREVIOUS)\n");
        s.push_str("__EXRAM_SYS_EVAL_RETURN:\n    LDA __SYS_EVAL_PORT\n    STA $01\n    PLA\n    TAY\n    PLA\n    TAX\n    PLA\n    PLP\n    RTS\n");
        s.push_str("__EXRAM_SYS_EVAL_IERROR:\n    LDA $01\n    PHA\n    AND #$FE\n    STA $01\n    JSR __SYS_EVAL_ABORT\n    PLA\n    STA $01\n    JMP (__SYS_EVAL_OLD_ERROR)\n");
    }
    if has_ierror {
        s.push_str("__EXRAM_IERROR_ENTRY:\n    LDA $01\n    AND #$FE\n    STA $01\n");
        // A nonreturning error may have abandoned SYS/USR's MEMSIZ swap.
        if ceiling.is_some() {
            let _ = writeln!(s, "    JSR {MEMSIZ_APPLY}");
        }
        s.push_str("    JMP __TSB_ERROR\n");
    }

    if !tramps.is_empty() {
        s.push_str(&tramps.render());
    }
    if has_usr {
        s.push_str("__EXRAM_USRJT:\n");

        if ceiling.is_some() {
            let _ = writeln!(s, "    JSR {SYS_IN}");
        }
        s.push_str("    INC $01\n    JSR __EXRAM_USR_VECTOR\n    DEC $01\n");
        if ceiling.is_some() {
            let _ = writeln!(s, "    JSR {SYS_OUT}");
        }
        s.push_str("    RTS\n__EXRAM_USR_VECTOR:\n    JMP ($0311)\n");
    }
    for &addr in used {
        let Some(call) = rom_lookup(addr) else {
            continue;
        };
        let _ = writeln!(s, "__JT_{}:", call.label);

        if ceiling.is_some() && matches!(call.addr, 0xA437 | 0xA46C | 0xE37B) {
            let _ = writeln!(s, "    JSR {MEMSIZ_RESTORE}");
        }
        let _ = writeln!(s, "    INC $01");
        let _ = writeln!(s, "    JSR ${:04X}", call.addr);
        let _ = writeln!(s, "    DEC $01");
        let _ = writeln!(s, "    RTS");
    }
    s.push_str("__EXRAM_ENTRY:\n");
    s.push_str("    LDA #$36\n");
    s.push_str("    STA $01\n");
    if ceiling.is_some() {
        s.push_str("    LDA $37\n");
        let _ = writeln!(s, "    STA {MEMSIZ_SAVE}");
        s.push_str("    LDA $38\n");
        let _ = writeln!(s, "    STA {MEMSIZ_SAVE}+1");
        let _ = writeln!(s, "    JSR {MEMSIZ_APPLY}");
    }
    s.push_str("    JSR __EXRAM_CODE_START\n");
    if ceiling.is_some() {
        let _ = writeln!(s, "    JSR {MEMSIZ_RESTORE}");
    }
    // INC bumps LORAM 0 → 1, leaving $01 = $37 — what BASIC expects
    // when it resumes from the SYS call. One byte cheaper than
    // `LDA #$37 / STA $01`, and no A clobber.
    s.push_str("    INC $01\n");
    s.push_str("    RTS\n");
    if let Some(ceiling) = ceiling {
        let _ = writeln!(s, "{MEMSIZ_RESTORE}:");
        let _ = writeln!(s, "    LDA {MEMSIZ_SAVE}");
        s.push_str("    STA $37\n");
        let _ = writeln!(s, "    LDA {MEMSIZ_SAVE}+1");
        s.push_str("    STA $38\n");
        s.push_str("    RTS\n");

        let _ = writeln!(s, "{MEMSIZ_APPLY}:");
        s.push_str("    SEC\n");
        let _ = writeln!(s, "    LDA {MEMSIZ_SAVE}");
        s.push_str("    SBC #<__HEAP_BOTTOM\n");
        let _ = writeln!(s, "    LDA {MEMSIZ_SAVE}+1");

        s.push_str("    SBC #>__HEAP_BOTTOM\n");
        s.push_str("    BCC __EXRAM_MEMSIZ_RAISE\n");
        let _ = writeln!(
            s,
            "    CMP #${:02X}",
            (crate::compile::MIN_STRING_HEAP >> 8) as u8
        );
        s.push_str("    BCC __EXRAM_MEMSIZ_RAISE\n");
        let _ = writeln!(s, "    JMP {MEMSIZ_RESTORE}");
        s.push_str("__EXRAM_MEMSIZ_RAISE:\n");
        s.push_str(&memsiz_raise(ceiling));
        s.push_str("    RTS\n");
        if has_sys || has_usr {
            s.push_str(&emit_sys_wrappers());
        }

        let _ = writeln!(s, "{MEMSIZ_SAVE}:");
        s.push_str("    .byte $00,$00\n");
    }
    s.push_str("; --- end extraram preamble ---\n\n");
    s
}

/// Where the machine's own MEMSIZ is parked for the duration of the run.
const MEMSIZ_SAVE: &str = "__EXRAM_MEMSIZ_SAVE";

pub(crate) const MEMSIZ_APPLY: &str = "__EXRAM_MEMSIZ_APPLY";
/// `SYS` entry: MEMSIZ back to the machine's.
const SYS_IN: &str = "__EXRAM_SYS_IN";

const SYS_OUT: &str = "__EXRAM_SYS_OUT";

/// The two `SYS` wrappers, emitted only when MEMSIZ is relocated.
fn emit_sys_wrappers() -> String {
    let mut s = String::new();
    let _ = writeln!(s, "{SYS_IN}:");
    s.push_str("    PHP\n    PHA\n");
    let _ = writeln!(s, "    JSR {MEMSIZ_RESTORE}");
    s.push_str("    PLA\n    PLP\n    RTS\n");
    let _ = writeln!(s, "{SYS_OUT}:");
    s.push_str("    PHP\n    PHA\n");

    let _ = writeln!(s, "    JSR {MEMSIZ_APPLY}");
    s.push_str("    PLA\n    PLP\n    RTS\n");
    s
}
/// Puts [`MEMSIZ_SAVE`] back into `$37/$38`.
const MEMSIZ_RESTORE: &str = "__EXRAM_MEMSIZ_RESTORE";

/// `$37/$38 = ceiling`, clobbering `.A`.
fn memsiz_raise(ceiling: u16) -> String {
    format!(
        "    LDA #${:02X}\n    STA $37\n    LDA #${:02X}\n    STA $38\n",
        ceiling & 0xFF,
        ceiling >> 8,
    )
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

fn is_data_anchor_label(label: Option<&str>) -> bool {
    matches!(
        label,
        Some("__DATA:") | Some("__DATA_END:") | Some("__VARS_START:") | Some("__VARS_END:")
            | Some("__CP_BEGIN:") | Some("__CP_END:")
    )
        || label
            .and_then(|label| label.strip_suffix(':'))
            .is_some_and(|name| {
                ["__DATA_LINE_", "__DBAD_S", "__DBAD_E"].iter().any(|prefix| {
                    name.strip_prefix(prefix).is_some_and(|index| {
                        !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit())
                    })
                })
            })
}

#[cfg(debug_assertions)]
const DATA_ANCHOR_LABELS: &[&str] = &[
    "__DATA:", "__DATA_END:", "__VARS_START:", "__VARS_END:",
    "__CP_BEGIN:", "__CP_END:",
];

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

/// Shared markers for composing the two recognised layout refusals.
pub(crate) const DATA_CEILING_MARKER: &str = "hoisted data sections grew to";
pub(crate) const IO_WALL_MARKER: &str = "the image runs to";

pub fn verify_data_fits<F>(
    lookup: F,
    user_asked_for_extraram: bool,
    image_end: u32,
    heap_ceiling: u16,
) -> Result<(), String>
where
    F: Fn(&str) -> Option<u16>,
{
    let data_end = lookup(DATA_END_LABEL)
        .ok_or_else(|| format!("--extraram: assembler did not resolve `{DATA_END_LABEL}` label"))?;
    if data_end > DATA_CEILING {
        let over = u32::from(data_end) - u32::from(DATA_CEILING);
        let escape = if user_asked_for_extraram {
            " or run without --extraram"
        } else {
            ""
        };
        let past_io = image_end.saturating_sub(u32::from(heap_ceiling));
        if past_io > 0 {
            let binding = over.max(past_io);

            return Err(format!(
                "{IO_WALL_MARKER} ${image_end:04X}, {past_io} bytes above ${heap_ceiling:04X} \
                 — the top of the space this layout has, which is where the I/O registers \
                 start (${HEAP_CEILING:04X}) or the lowest address reserved below them. Its \
                 hoisted data sections also end at ${data_end:04X}, {over} bytes beyond the \
                 ${DATA_CEILING:04X} ceiling. The {binding} is the binding figure: both \
                 limits must be satisfied. These are emitted-byte overshoots, not amounts \
                 of BASIC source to delete. Reduce data (smaller arrays, fewer string literals, less \
                 DATA){escape}."
            ));
        }
        return Err(format!(
            "{DATA_CEILING_MARKER} ${data_end:04X}, {over} bytes beyond the \
             ${DATA_CEILING:04X} ceiling. Reduce data (smaller arrays, fewer \
             string literals, less DATA){escape}."
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
    fn usr_trampoline_returns_to_shadow_ram_and_preserves_fac() {
        for touches_memsiz in [false, true] {
            let input = "*=$080D\n    JSR __USR_VECTORED\n\
                STA $0404\n    STX $0405\n    STY $0406\n\
                PHP\n    PLA\n    STA $0407\n\
                LDA $01\n    STA $0402\n    LDA $38\n    STA $0403\n    RTS\n\
                __USR_VECTORED:\n    JMP ($0311)\n__HEAP_BOTTOM:\n";
            let out = inject_with_reserved(input, &[], touches_memsiz);
            let mut assembler = asm6502::Assembler6502::new();
            assembler.assemble_bytes(&out).unwrap();
            let padding = 0xA100 - assembler.lookup("__EXRAM_CODE_START").unwrap();
            let out = out.replace(
                "__EXRAM_CODE_START:\n",
                &format!("    .byte {}\n__EXRAM_CODE_START:\n", vec!["$EA"; usize::from(padding)].join(",")),
            );
            let mut assembler = asm6502::Assembler6502::new();
            let bytes = assembler.assemble_bytes(&out).unwrap();
            assert!(assembler.lookup("__EXRAM_USR_VECTOR").unwrap() < 0xA000);
            assert!(assembler.lookup("__USR_VECTORED").unwrap() >= 0xA100);
            let mut system = emu64::System::new();
            system.mem.ram[0x080D..0x080D + bytes.len()].copy_from_slice(&bytes);
            // JSR $080D; JMP $FFFE: an explicit return sentinel.
            system.mem.ram[0x0200..0x0206].copy_from_slice(&[0x20, 0x0D, 0x08, 0x4C, 0xFE, 0xFF]);
            system.mem.ram[0x0311..0x0313].copy_from_slice(&[0x40, 0x03]);
            let callee = assemble("*=$0340\n LDA $01\n STA $0400\n LDA $38\n STA $0401\n INC $61\n LDA #$9A\n LDX #$56\n LDY #$78\n SEC\n RTS\n");
            system.mem.ram[0x0340..0x0340 + callee.len()].copy_from_slice(&callee);
            system.mem.ram[1] = 0x37;
            system.mem.ram[0x37..0x39].copy_from_slice(&[0, 0xA0]);
            system.mem.ram[0x61..0x67].copy_from_slice(&[0x81, 0x80, 3, 4, 5, 0]);
            system.start_at(0x0200);
            system.cpu.regs.sp = 0xF0;
            for _ in 0..500 {
                if system.cpu.regs.pc == 0xFFFE {
                    break;
                }
                assert!(system.step(), "USR did not return at ${:04X}: {out}", system.cpu.regs.pc);
            }
            assert_eq!(system.cpu.regs.pc, 0xFFFE);
            assert_eq!(system.cpu.regs.sp, 0xF0, "balanced indirect call");
            assert_eq!(&system.mem.ram[0x400..0x407], &[0x37, 0xA0, 0x36, if touches_memsiz { 0xA0 } else { 0xD0 }, 0x9A, 0x56, 0x78]);
            assert_eq!(system.mem.ram[0x407] & 1, 1, "carry survives the swap");
            assert_eq!(&system.mem.ram[0x61..0x67], &[0x82, 0x80, 3, 4, 5, 0]);
            assert_eq!(system.mem.ram[1], 0x37, "normal program exit");
            assert_eq!(&system.mem.ram[0x37..0x39], &[0, 0xA0]);
        }
    }

    #[test]
    fn only_the_compilers_usr_vector_tail_gets_wrapped() {
        let unrelated = "*=$080D\nOTHER:\n    JMP ($0311)\n";
        let out = inject(unrelated);
        assert!(!out.contains("__EXRAM_USR"));
        assert!(out.contains("OTHER:\n    JMP ($0311)"));
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

    /// The two ROM entries `__SET_TI` reaches.
    #[test]
    fn ti_string_rom_helpers_get_wrapped() {
        for (addr, label) in [(0xBAE2u16, "MUL10"), (0xBD7E, "ADD_SIGNED_BYTE")] {
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
            // Exactly one direct `JSR $XXXX` may survive: the one inside the wrapper.
            let direct = format!("    JSR ${addr:04X}");
            let count = out.matches(&direct).count();
            assert_eq!(
                count, 1,
                "expected exactly one inner `{direct}` (the JT wrapper); found {count}\n{out}"
            );

            let body = out.split(&want_def).nth(1).unwrap();
            let head: Vec<&str> = body.lines().skip(1).take(4).map(str::trim).collect();
            assert_eq!(
                head,
                vec![
                    "INC $01",
                    &format!("JSR ${addr:04X}")[..],
                    "DEC $01",
                    "RTS"
                ],
                "{label} trampoline body\n{out}"
            );
        }
    }

    #[test]
    fn unwrapped_rom_call_detector_reports_the_addresses() {
        let asm = "__EXRAM_CODE_START:\n\
                   ; --- user code ---\n\
                       JSR __JT_FMULT\n\
                       JSR $BD7E\n\
                       LDA #$00\n\
                       JMP $BAE2\n\
                       JSR $FFD2\n\
                       JMP ($0314)\n\
                       RTS\n";
        let found: Vec<u16> = unwrapped_rom_calls(asm).iter().map(|l| l.1).collect();
        assert_eq!(
            found,
            vec![0xBD7E, 0xBAE2],
            "the two BASIC-window calls, in order; \
             $FFD2 is KERNAL (HIRAM, always mapped) and the indirect is not a ROM call"
        );

        let preamble = "__JT_ADD_SIGNED_BYTE:\n    INC $01\n    JSR $BD7E\n    DEC $01\n    RTS\n";
        let whole = format!("{preamble}__EXRAM_CODE_START:\n    JSR __JT_ADD_SIGNED_BYTE\n    RTS\n");
        assert!(
            unwrapped_rom_calls(&whole).is_empty(),
            "the wrapper's own inner JSR is not a leak"
        );

        // No marker label at all -> not an extraram image -> nothing to check.
        assert!(unwrapped_rom_calls("    JSR $BD7E\n    RTS\n").is_empty());
    }

    #[test]
    #[should_panic(expected = "unwrapped ROM call")]
    fn assert_no_unwrapped_rom_calls_panics_and_names_the_fix() {
        assert_no_unwrapped_rom_calls("__EXRAM_CODE_START:\n    JSR $BD7E\n    RTS\n");
    }

    #[test]
    #[should_panic(expected = "unwrapped ROM call")]
    fn inject_itself_refuses_to_emit_an_unwrapped_rom_call() {
        let _ = inject("*=$080D\n    JSR $A123\n    RTS\n");
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
        let code_start = out.find("__EXRAM_CODE_START:").expect("CODE_START boundary");
        let lda_pos = out.find("    LDA #<S0").expect("LDA #<S0 present in body");
        // S0 must appear BEFORE the boundary, and user code (the
        // LDA instruction) must appear AFTER the boundary.
        assert!(s0_pos < code_start, "S0 above CODE_START:\n{out}");
        assert!(lda_pos > code_start, "LDA below CODE_START:\n{out}");
        assert!(out.contains("__EXRAM_DATA_END:"));
    }

    #[test]
    fn vars_anchors_hoist_with_their_slots() {
        let input = "\
*=$080D
    JSR __STATE_RESET
    RTS

; --- variable slots ---
__VARS_START:
V_A:
    .byte $00,$00,$00,$00,$00
VI_C:
    .byte $00
__VARS_END:

__HEAP_BOTTOM:
";
        let out = inject(input);
        let code_start = out.find("__EXRAM_CODE_START:").expect("CODE_START boundary");
        let start = out.find("__VARS_START:").expect("__VARS_START present");
        let end = out.find("__VARS_END:").expect("__VARS_END present");
        assert!(start < code_start, "__VARS_START hoisted:\n{out}");
        assert!(end < code_start, "__VARS_END hoisted:\n{out}");

        let a = out.find("V_A:").expect("V_A present");
        let c = out.find("VI_C:").expect("VI_C present");
        assert!(
            start < a && a < c && c < end,
            "anchors must still bracket every slot:\n{out}"
        );
    }

    #[test]
    fn data_pool_anchors_hoist_with_their_pool() {
        let input = "\
*=$080D
    JSR __DATA_INIT
    RTS

__DATA:
__DATA_LINE_10:
    .byte $01,$02
__DATA_END:

__HEAP_BOTTOM:
";
        let out = inject(input);
        let code_start = out
            .find("__EXRAM_CODE_START:")
            .expect("CODE_START boundary");
        for anchor in ["__DATA:", "__DATA_LINE_10:", "__DATA_END:"] {
            let pos = out
                .find(anchor)
                .unwrap_or_else(|| panic!("{anchor} present:\n{out}"));
            assert!(pos < code_start, "{anchor} hoisted:\n{out}");
        }
    }

    #[test]
    fn adjacent_read_error_and_line_anchors_stay_inside_the_data_pool() {
        let input = "\
*=$080D
    RTS
__DATA:
__DATA_LINE_100:
__DBAD_S0:
    .byte $03,$4F,$4E,$45
__DATA_LINE_200:
__DBAD_S1:
__DBAD_E0:
    .byte $03,$54,$57,$4F
__DATA_LINE_300:
__DBAD_E1:
    .byte $01,$37
__DATA_END:
__DATA_BAD:
    .word __DBAD_S0,__DBAD_E0,100
    .word __DBAD_S1,__DBAD_E1,200
    .word $0000
__HEAP_BOTTOM:
";
        let out = inject(input);
        let labels = ["__DATA:", "__DATA_LINE_100:", "__DBAD_S0:",
            "__DATA_LINE_200:", "__DBAD_S1:", "__DBAD_E0:",
            "__DATA_LINE_300:", "__DBAD_E1:", "__DATA_END:",
            "__DATA_BAD:", "__EXRAM_CODE_START:"];
        let positions: Vec<_> = labels.iter().map(|label| find_label_line(&out, label).unwrap()).collect();
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "{out}");
        for label in [None, Some("__DBAD_S:"), Some("__DBAD_Edone:"),
            Some("__DBAD_S1_suffix:"), Some("__DATA_LINE_READ:"),
            Some("__DATA_LINE_12"), Some("__DATA_INIT:")] {
            assert!(!is_data_anchor_label(label), "not a numbered data anchor: {label:?}");
        }
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

    const IMAGE_WELL_CLEAR_OF_IO: u32 = 0xC000;

    #[test]
    fn heap_ceiling_ignores_only_reservations_entirely_below_the_floor() {
        let ranges = [(0xC000, 0xC0FF), (0xC800, 0xC8FF)];
        assert_eq!(heap_ceiling(&ranges, 0xBFFF), 0xC000);
        assert_eq!(heap_ceiling(&ranges, 0xC000), 0xC000);
        assert_eq!(heap_ceiling(&ranges, 0xC0FF), 0xC000);
        assert_eq!(heap_ceiling(&ranges, 0xC100), 0xC800);
        assert_eq!(heap_ceiling(&ranges, 0xC900), HEAP_CEILING);
        assert_eq!(heap_ceiling(&[(0xE000, 0xEFFF)], 0xC900), HEAP_CEILING);
    }

    fn fits_ending_at(data_end: u16) -> Result<(), String> {

        verify_data_fits(
            |name| (name == DATA_END_LABEL).then_some(data_end),
            false,
            IMAGE_WELL_CLEAR_OF_IO,
            HEAP_CEILING,
        )
    }

    #[test]
    fn data_ceiling_check_passes_for_small_program() {

        fits_ending_at(0x0820).expect("0820 is below ceiling");
    }

    #[test]
    fn data_ending_flush_against_the_rom_window_is_accepted() {
        fits_ending_at(0xA000).expect("last data byte at $9FFF is always-RAM");
    }

    #[test]
    fn data_in_the_last_page_below_the_rom_window_is_accepted() {
        for data_end in [0x9F00, 0x9F01, 0x9FA5, 0x9FFF] {
            fits_ending_at(data_end).unwrap_or_else(|e| panic!("${data_end:04X} is RAM: {e}"));
        }
    }

    #[test]
    fn data_ceiling_check_fails_for_overflow() {
        // One byte into the ROM window.
        let err = fits_ending_at(0xA001).expect_err("ceiling violation should be caught");
        assert!(err.contains("A001"), "{err}");
        assert!(err.contains("A000"), "{err}");
        // The overshoot, spelled out.
        assert!(err.contains("1 bytes beyond"), "{err}");
    }

    #[test]
    fn the_run_without_extraram_escape_is_offered_only_to_whoever_passed_it() {
        // The auto-predictor escalates to this layout on its own.
        let auto = fits_ending_at(0xA001).expect_err("refuses");
        assert!(
            !auto.contains("without --extraram"),
            "escalated builds must not offer a flag the user never set: {auto}"
        );
        let asked = verify_data_fits(
            |name| (name == DATA_END_LABEL).then_some(0xA001),
            true,
            IMAGE_WELL_CLEAR_OF_IO,
            HEAP_CEILING,
        )
        .expect_err("refuses");
        assert!(
            asked.contains("or run without --extraram"),
            "a user who DID pass it keeps the escape hatch: {asked}"
        );

        let asked_io = verify_data_fits(
            |name| (name == DATA_END_LABEL).then_some(0xA001),
            true,
            0xE000,
            HEAP_CEILING,
        )
        .expect_err("refuses");
        assert!(
            asked_io.contains("or run without --extraram"),
            "the I/O-wall arm keeps the escape hatch as well: {asked_io}"
        );
    }

    #[test]
    fn the_binding_ceiling_is_the_io_wall_when_the_image_is_past_it_too() {

        let err = verify_data_fits(
            |name| (name == DATA_END_LABEL).then_some(0xA026),
            false,
            0xE000,
            HEAP_CEILING,
        )
        .expect_err("both ceilings are violated");
        assert!(
            err.contains("4096 bytes above $D000"),
            "the I/O overshoot, to the byte: {err}"
        );
        assert!(
            err.contains("38 bytes beyond the $A000 ceiling"),
            "the data overshoot is carried, not discarded: {err}"
        );
        assert!(
            err.contains("The 4096 is the binding figure"),
            "and the larger one is named as the binding one: {err}"
        );
        assert!(
            err.starts_with(IO_WALL_MARKER),
            "must be recognisable to binding_layout_failure: {err}"
        );
        assert!(
            !err.starts_with(DATA_CEILING_MARKER),
            "and must not masquerade as the plain data-ceiling refusal: {err}"
        );
    }

    #[test]
    fn an_image_that_fits_under_the_io_wall_still_gets_the_plain_data_refusal() {

        let err = verify_data_fits(
            |name| (name == DATA_END_LABEL).then_some(0xACCC),
            false,
            u32::from(HEAP_CEILING),
            HEAP_CEILING,
        )
        .expect_err("the data ceiling is still violated");
        assert!(
            err.starts_with(DATA_CEILING_MARKER),
            "unchanged wording when only one ceiling binds: {err}"
        );
        assert!(
            err.contains("3276 bytes beyond"),
            "with its own figure: {err}"
        );
        assert!(
            !err.contains("the top of the space this layout has"),
            "no I/O clause for an image that fits: {err}"
        );
    }

    #[test]
    fn data_can_remain_the_larger_overshoot_when_both_ceilings_are_exceeded() {

        let err = verify_data_fits(
            |name| (name == DATA_END_LABEL).then_some(0xB000),
            false,
            0xD001,
            HEAP_CEILING,
        )
        .expect_err("both ceilings are violated");
        assert!(err.contains("1 bytes above $D000"), "{err}");
        assert!(err.contains("4096 bytes beyond the $A000 ceiling"), "{err}");
        assert!(err.contains("The 4096 is the binding figure"), "{err}");
    }

    #[test]
    fn a_reserved_range_lowers_the_wall_the_image_is_measured_against() {
        // The ceiling is a parameter, not $D000 spelled twice.
        let reserved_ceiling = heap_ceiling(&[(0xC000, 0xC7FF)], 0xB000);
        assert_eq!(reserved_ceiling, 0xC000, "premise: the reservation binds");
        let err = verify_data_fits(
            |name| (name == DATA_END_LABEL).then_some(0xA001),
            false,
            0xC800,
            reserved_ceiling,
        )
        .expect_err("refuses");
        assert!(
            err.contains("2048 bytes above $C000"),
            "measured against the reserved ceiling: {err}"
        );

        assert!(
            err.contains("($D000)"),
            "the real I/O address stays named even when the wall is lower: {err}"
        );
        // The default ceiling leaves the entire image within bounds.
        let plain = verify_data_fits(
            |name| (name == DATA_END_LABEL).then_some(0xA001),
            false,
            0xC800,
            HEAP_CEILING,
        )
        .expect_err("refuses");
        assert!(
            plain.starts_with(DATA_CEILING_MARKER),
            "and only the data ceiling without the reservation: {plain}"
        );
    }

    #[test]
    fn data_ceiling_check_fails_when_label_missing() {
        let err = verify_data_fits(|_| None, false, IMAGE_WELL_CLEAR_OF_IO, HEAP_CEILING)
            .expect_err("missing label is an error");
        assert!(err.contains("did not resolve"));
    }

    #[test]
    fn a_materialised_image_is_pinned_below_the_jump_table() {
        let input = concat!(
            "*=$080D\n",
            "    JMP __BT_INPLACE_AFTER\n",
            "__BT_INPLACE_BODY:\n",
            "    .byte $01,$02,$03\n",
            "__BT_INPLACE_AFTER:\n",
            "    LDX #$0E\n",
            "__BT_INPLACE_RESTORE:\n",
            "    LDA __BT_INPLACE_HEAD,X\n",
            "    STA $0801,X\n",
            "    DEX\n",
            "    BPL __BT_INPLACE_RESTORE\n",
            "    JMP __BT_INPLACE_START\n",
            "__BT_INPLACE_HEAD:\n",
            "    .byte $AA,$BB\n",
            "__BT_INPLACE_START:\n",
            "S0:\n",
            "    .byte $48,$49\n",
            "__PROG:\n",
            "    JSR $B867\n",
            "    RTS\n",
        );
        let out = inject(input);
        let at = |needle: &str| out.find(needle).unwrap_or_else(|| panic!("{needle}\n{out}"));

        let origin = at("*=$080D");
        let blob = at("__BT_INPLACE_BODY:");
        let head = at("__BT_INPLACE_HEAD:");
        let pin_end = at("__BT_INPLACE_START:");
        let preamble = at("extraram: bank BASIC ROM out");
        let jt = at("__JT_FADD:");
        let hoisted = at("S0:");
        let code = at("__EXRAM_CODE_START:");
        assert!(origin < blob, "the blob stays inside the originated section");
        assert!(blob < head);
        assert!(head < pin_end);
        assert!(
            pin_end < preamble,
            "the pinned prologue must precede the extraram preamble"
        );
        assert!(preamble < jt);
        assert!(jt < hoisted, "the JT still precedes the hoisted data");
        assert!(hoisted < code, "data is still hoisted above the code");

        assert!(out.contains("JSR __JT_FADD"));
    }

    /// Apply the heap limit only when preserving the original image.
    #[test]
    fn an_ordinary_program_keeps_the_jump_table_first() {
        let out = inject("*=$080D\nS0:\n    .byte $48\n__PROG:\n    JSR $B867\n    RTS\n");
        let jt = out.find("__JT_FADD:").expect("a JT entry");
        let data = out.find("S0:").expect("the data block");
        assert!(jt < data, "JT must still come first:\n{out}");
    }
}
