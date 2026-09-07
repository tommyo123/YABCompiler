//! Compile tokenized BASIC into a runnable PRG.

use asm6502::{Assembler6502, Item};

use crate::{ISSUES_URL, ast, codegen, extraram, ir, pack, parse, prg, rem_hints};

pub use crate::rem_hints::BasicHintDialect;
pub use crate::tokens::Dialect;

pub use crate::codegen::Profile;

/// Per-build options. Profile controls the size/speed trade-off; `extraram`
/// enables banking BASIC ROM out so $A000-$BFFF becomes RAM that the
/// program can use for code/data, with ROM calls going through a low
/// jump table that flips `$01` for the duration of each call.
#[derive(Debug, Clone)]
pub struct CompileOptions {
    pub profile: Profile,
    /// Force `--extraram` on. When false, the auto-predictor decides:
    /// it does a probe assembly without extraram and switches it on
    /// when the program would land at or beyond `AUTO_EXTRARAM_TRIGGER`.
    /// `force_extraram_off` overrides the predictor and stays off.
    pub extraram: bool,
    /// Force `--extraram` off. Defeats the auto-predictor — useful when
    /// the user knows the program runs against BASIC ROM (e.g. mixed
    /// compiled + interpreted code), or to test a build without the
    /// JT overhead even when the predictor would enable it.
    pub force_extraram_off: bool,
    /// Manually-supplied reserved memory ranges. The assembler skips
    /// these regions — useful for sprite blocks ($7800-$79FF), custom
    /// character sets, screen relocations, etc. that would otherwise
    /// be clobbered if code or data were placed there.
    ///
    /// Inclusive `(start, end)` pairs; a single-address reservation
    /// is `(addr, addr)`. Pairs go through `add_reserved_range` after
    /// any auto-discovered ranges, so manual entries take precedence
    /// when ranges would overlap.
    pub reserved_ranges: Vec<(u16, u16)>,
    /// Enable two-pass auto-discovery of reserved regions: scan the
    /// optimised IR for literal POKE/PEEK addresses (and the endpoints
    /// of literal-bounded POKE-fill loops) in `$0800..=$CFFF`, then
    /// merge runs of close-together hits into ranges and add them as
    /// reserved before emitting code. Catches sprite/screen/char
    /// init tables and bulk-fill destinations the program touches at
    /// runtime, so generated code/data won't land on top of them.
    pub auto_reserve: bool,
    /// Accept BASIC v2 typos that would otherwise abort the parse —
    /// `GOT1200` (=> GOTO 1200) or `CLOSE n, sa, dev`. v2 catches
    /// these at runtime, so they're harmless when the offending line
    /// is dead code; off by default to keep real syntax errors loud.
    pub lenient_syntax: bool,
    /// Save and restore $FB-$FE plus every ZP-pool cell the codegen
    /// allocated, around each `SYS` call. ML routines that touch zero
    /// page would otherwise corrupt the program's variable storage.
    /// Off by default; opt in when calling third-party ML.
    pub safe_sys_calls: bool,

    pub no_sys_param_substitution: bool,
    /// Load SYS register arguments into A/X/Y/SR before the call.
    pub sys_register_args: bool,
    /// Which third-party compiler's REM-hint syntax to honour. Only
    /// one dialect may be active at a time; `BasicHintDialect::None`
    /// (the default) treats REM as plain comment text.
    pub rem_hint_dialect: BasicHintDialect,
    /// Source dialect used to interpret extended keyword spellings.
    pub dialect: Dialect,
    /// Place machine code at a custom origin and ship a raw .prg with
    /// no SYS launcher. The user is expected to start the program
    /// manually (`SYS <address>` from BASIC, or load+jmp from ML).
    /// `None` keeps the default: load at $0801, 12-byte SYS stub,
    /// code at $080D. `Some(addr)` loads + originates at `addr` and
    /// skips the stub entirely.
    ///
    /// The auto-extraram predictor still runs for low custom origins
    /// (e.g. $8000) so a build that grows past $9F00 picks up the
    /// extra 8 KB of code space just like a default-origin build.
    /// Origins at or above $C000 sit past the BASIC-ROM shadow, so
    /// the predictor is skipped — extraram can't add space below the
    /// program's start.
    pub custom_start_address: Option<u16>,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self {
            profile: Profile::default(),
            extraram: false,
            force_extraram_off: false,
            reserved_ranges: Vec::new(),
            // Auto-reserve catches the sprite / screen / charset POKE
            // patterns most C64 BASIC games rely on; defaulting it on
            // means most programs compile correctly without the user
            // having to know about the flag.
            auto_reserve: true,
            lenient_syntax: false,
            safe_sys_calls: false,
            no_sys_param_substitution: false,
            sys_register_args: false,
            rem_hint_dialect: BasicHintDialect::None,
            dialect: Dialect::default(),
            custom_start_address: None,
        }
    }
}

/// Parse a comma-separated list of reserved addresses or ranges.
///
/// Each entry is `$AAAA`, `0xAAAA`, or an inclusive `$AAAA-$BBBB`
/// range. Whitespace around commas and dashes is ignored.
pub fn parse_reserved_ranges(s: &str) -> Result<Vec<(u16, u16)>, String> {
    let mut out = Vec::new();

    for raw in s.split(',') {
        let entry = raw.trim();
        if entry.is_empty() {
            continue;
        }

        let range = if let Some((lo, hi)) = entry.split_once('-') {
            let lo = parse_hex_addr(lo.trim())
                .ok_or_else(|| format!("invalid range start: '{}'", lo.trim()))?;
            let hi = parse_hex_addr(hi.trim())
                .ok_or_else(|| format!("invalid range end: '{}'", hi.trim()))?;
            if lo > hi {
                return Err(format!(
                    "range start ${lo:04X} is above end ${hi:04X} in '{entry}'"
                ));
            }
            (lo, hi)
        } else {
            let addr =
                parse_hex_addr(entry).ok_or_else(|| format!("invalid address: '{entry}'"))?;
            (addr, addr)
        };

        out.push(range);
    }

    Ok(out)
}

/// Parse a `--start-address` argument: `$XXXX`, `0xXXXX`, plain hex,
/// or decimal. Rejects values below $0200 — that range covers the
/// stack and KERNAL workspace and a program loaded there clobbers
/// runtime state before its first instruction.
pub fn parse_start_address(s: &str) -> Result<u16, String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err("expected an address (e.g. $C000)".to_string());
    }
    let addr = parse_hex_addr(trimmed)
        .or_else(|| {
            trimmed
                .parse::<u32>()
                .ok()
                .and_then(|v| u16::try_from(v).ok())
        })
        .ok_or_else(|| format!("invalid address: '{trimmed}'"))?;
    if addr < 0x0200 {
        return Err(format!(
            "address ${addr:04X} is below $0200 (stack + KERNAL workspace); pick \
             a higher start"
        ));
    }
    Ok(addr)
}

fn parse_hex_addr(s: &str) -> Option<u16> {
    let rest = if let Some(rest) = s.strip_prefix('$') {
        rest
    } else if let Some(rest) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        rest
    } else {
        s
    };

    u32::from_str_radix(rest, 16)
        .ok()
        .and_then(|value| u16::try_from(value).ok())
}

/// Load-image boundary that triggers automatic extra RAM mode.
pub const AUTO_EXTRARAM_TRIGGER: u32 = 0x9F00;

/// Default KERNAL memory limit and string-heap ceiling without extra RAM.
pub const DEFAULT_MEMSIZ: u32 = 0xA000;

/// Smallest string heap the extraram-OFF layout may ship with.
pub const MIN_STRING_HEAP: u32 = 0x0800;

pub const MAX_HEAP_CHUNK: u32 = 255 + 1 + 2;

/// Smallest heap window the extraram-ON layout may ship with, for a program that also carves runtime-`DIM` arrays out of it.
pub const MIN_EXTRARAM_DIM_HEAP: u32 = 0x0200;

/// First runtime address we hand to user code: BASIC's load address
/// (\$0801) plus the 12-byte SYS stub. Used by the predictor to
/// translate `machine_code.len()` into an absolute end address.
pub const CODE_ORIGIN: u32 = 0x080D;

#[derive(Debug)]
pub enum CompileError {
    Prg(prg::ParseError),
    Parse(parse::ParseError),
    Lower(ir::LowerError),
    Pass(ir::PassError),
    Codegen(codegen::CodegenError),
    /// The input .prg isn't tokenized BASIC: its line-link structure
    /// doesn't match what the BASIC interpreter writes, so it's almost
    /// always a machine-language program (a sprite editor at $C000, a
    /// music player at $7000, a DOS wedge at $CC00, …) or raw data
    /// that's already 6502 code with nothing to compile. Carries the
    /// load address so the message can name it.
    NotBasic {
        load_address: u16,
    },
    /// `message` is the human-readable diagnostic (kept short so it
    /// fits a status bar and a one-line CLI eprintln). `asm` carries
    /// the generated assembly only when an asm6502 pass produced one
    /// — opt-in via [`CompileError::generated_asm`] so callers can
    /// write it to a debug file without bloating every log line.
    Assemble {
        message: String,
        asm: Option<String>,
    },
}

impl CompileError {
    /// The full generated assembly for an `Assemble` error, when the
    /// asm6502 step actually ran. Other variants and pre-assembly
    /// failures (mutual-exclusion checks, reserved-range setup)
    /// return `None`. Used by CLI/GUI to dump the asm to a separate
    /// file or panel rather than smearing it into the user-visible
    /// status message.
    pub fn generated_asm(&self) -> Option<&str> {
        match self {
            CompileError::Assemble { asm, .. } => asm.as_deref(),
            _ => None,
        }
    }
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompileError::Prg(e) => write!(f, "prg parse: {e}"),
            CompileError::Parse(e) => write!(f, "basic parse: {e}"),
            CompileError::Lower(e) => write!(f, "lower: {e}"),
            CompileError::Pass(e) => write!(f, "pass: {e}"),
            CompileError::Codegen(e) => write!(f, "codegen: {e}"),
            CompileError::NotBasic { load_address } => write!(
                f,
                "not a BASIC program (loads at ${load_address:04X}): its line-link \
                 structure doesn't match tokenized BASIC. This looks like a \
                 machine-language program or raw data — it is already 6502 code, \
                 so there is nothing to compile."
            ),
            CompileError::Assemble { message, .. } => {

                match message.strip_prefix("internal error: ") {
                    Some(rest) => write!(
                        f,
                        "assemble: internal error: {rest}\n   This is a bug in the compiler, \
                         not a mistake in your program, and the detail above is for the \
                         compiler's authors — please report it at \
                         {ISSUES_URL} with the .prg. \
                         In the meantime --no-auto-reserve, --profile=size and \
                         --start-address= each change the layout and often get a build out."
                    ),
                    None => write!(f, "assemble: {message}"),
                }
            }
        }
    }
}

impl std::error::Error for CompileError {}

pub struct Compiled {
    pub asm: String,
    pub machine_code: Vec<u8>,
    pub prg_bytes: Vec<u8>,
    pub diagnostics: Diagnostics,
}

/// How the auto-extraram predictor (or the user) decided to bank
/// BASIC ROM out. Shown in CLI/GUI diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExtraRamDecision {
    #[default]
    /// Auto-predictor ran and decided extraram wasn't needed.
    AutoOff,
    /// Auto-predictor ran and switched extraram on (program crosses
    /// the `$9F00` ceiling without it).
    AutoOn,
    /// `--extraram` was set on the command line.
    ForcedOn,
    /// `--force-extraram-off` was set on the command line.
    ForcedOff,

    OffOriginalImage,
    /// Custom start address sits at or above the BASIC-ROM shadow
    /// (`>=$C000`) — the predictor was skipped because extraram
    /// can't add code space below the program's origin.
    SkippedHighOrigin,
}

impl ExtraRamDecision {
    pub fn is_on(self) -> bool {
        matches!(self, Self::AutoOn | Self::ForcedOn)
    }
}

impl std::fmt::Display for ExtraRamDecision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AutoOff => write!(f, "auto (off)"),
            Self::AutoOn => write!(f, "auto (on)"),
            Self::ForcedOn => write!(f, "on (forced)"),
            Self::ForcedOff => write!(f, "off (forced)"),
            Self::OffOriginalImage => {
                write!(f, "off (original image materialised in place)")
            }
            Self::SkippedHighOrigin => write!(f, "off (skipped — origin ≥ $C000)"),
        }
    }
}

/// Build-time facts that aren't easily recovered from the compiled
/// .prg alone. The CLI prints these after every successful compile;
/// the GUI surfaces them in the status panel and uses
/// `auto_reserved` to pre-fill the manual ranges field.
#[derive(Debug, Clone, Default)]
pub struct Diagnostics {
    /// Where the .prg loads in memory. `$0801` for default-stub
    /// builds; the user's value otherwise.
    pub start_address: u16,
    /// Last byte of compiled machine code (inclusive). For custom-
    /// start builds this is `start_address + machine_code.len() - 1`;
    /// for default builds it's the same arithmetic shifted past the
    /// SYS stub bytes.
    pub end_address: u16,
    /// Outcome of the extraram decision plus its trigger.
    pub extraram: ExtraRamDecision,
    /// Ranges that auto-reserve discovered from POKE/PEEK literals
    /// in the IR. Empty when `auto_reserve` was off or the program
    /// didn't touch any literal addresses.
    pub auto_reserved: Vec<(u16, u16)>,

    pub manual_reserved: Vec<(u16, u16)>,

    pub manual_reserved_clipped: Vec<(u16, u16)>,
    /// Auto + manual merged into the actual set of inclusive ranges
    /// the assembler reserved. Use this for display — `auto_reserved`
    /// and `manual_reserved` are kept around so the GUI can show the
    /// source breakdown when the two sets contributed distinct
    /// ranges, but most callers just want the effective layout.
    pub effective_reserved: Vec<(u16, u16)>,
    /// Statements compiled away because the parser doesn't implement
    /// the keyword. A live one silently does nothing, so callers
    /// should surface these.
    pub skipped_statements: Vec<crate::ast::SkippedStatement>,
    /// Variable references inside `SYS addr,<text>` parameter strings.
    pub sys_param_vars: Vec<crate::sys_params::Site>,

    pub syntax_traps: Vec<crate::ast::SyntaxTrap>,
    /// `DEF FN`s whose body the parser could not read.
    pub deferred_fn_bodies: Vec<crate::ast::DeferredFnBody>,
    /// Zero-page usage and denied promotions.
    pub zp_pressure: crate::codegen::ZpPressure,

    pub indirect_vectors: Vec<(String, u16)>,

    pub vector_pads: usize,

    pub bridge_shims: usize,

    pub inline_str_args_undone: bool,
    /// Trailing zero bytes initialized at startup and omitted from the load image.
    pub bss_truncated: usize,
    /// Reserved holes with a small amount of code above them.
    pub reserved_overshoots: Vec<ReservedOvershoot>,

    pub original_image: Option<(u16, u16)>,
    /// The build selected [`Profile::Size`] after a layout failure or to improve a tight managed heap window at the default profile.
    pub size_profile_fallback: bool,

    pub size_profile_heap_improvement: Option<(u32, u32)>,

    pub compact_heap_layout: bool,

    pub sys_param_substitution_dropped: bool,
}

/// A reserved hole and the short image tail that requires its padding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReservedOvershoot {
    /// The reserved range, inclusive.
    pub range: (u16, u16),
    /// Bytes of image stored above `range.1`.
    pub overshoot: u32,
    /// Size of the hole, i.e. the padding bytes the file is paying.
    pub hole: u32,
}

impl ReservedOvershoot {
    /// Bytes saved by removing the image tail and the hole's padding.
    pub fn recoverable(&self) -> u32 {
        self.hole + self.overshoot
    }
}

/// Maximum image tail above a reserved hole to report.
const RESERVED_OVERSHOOT_MAX: u32 = 256;

/// Find reserved holes followed by a short tail in the stored image range.
pub fn reserved_tail_overshoots(
    file_start: u16,
    file_end: u16,
    reserved: &[(u16, u16)],
) -> Vec<ReservedOvershoot> {
    let mut out: Vec<ReservedOvershoot> = reserved
        .iter()
        .filter_map(|&(lo, hi)| {
            if hi >= file_end || lo < file_start {
                return None;
            }
            let overshoot = u32::from(file_end) - u32::from(hi);
            let hole = u32::from(hi) - u32::from(lo) + 1;
            (overshoot <= RESERVED_OVERSHOOT_MAX && overshoot < hole).then_some(ReservedOvershoot {
                range: (lo, hi),
                overshoot,
                hole,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        b.recoverable()
            .cmp(&a.recoverable())
            .then(a.range.0.cmp(&b.range.0))
    });
    out
}

/// Heuristic: does this parsed .prg look like genuine tokenized
/// BASIC, as opposed to a machine-language program or raw data that
/// happened to parse without erroring?
///
/// The reliable signal is the next-line link chain. The BASIC
/// interpreter stores, ahead of every line, a pointer to where the
/// next line begins in memory — i.e. `load_address + cumulative line
/// lengths`. For real BASIC every link matches; for ML those bytes
/// are arbitrary and essentially never match. We require at least one
/// consistent link, which cleanly separates the two in practice while
/// still tolerating a truncated final line whose link is missing.
fn looks_like_basic(prg: &prg::Program) -> bool {
    let mut addr = prg.load_address;
    let mut consistent = 0usize;
    for line in &prg.lines {
        // Each line occupies: 2-byte link + 2-byte line number + body
        // + 1-byte $00 terminator.
        let line_len = (2 + 2 + line.body.len() + 1) as u16;
        let next_addr = addr.wrapping_add(line_len);
        if line.next_ptr == next_addr {
            consistent += 1;
        }
        addr = next_addr;
    }
    consistent > 0
}

/// Whether the input contains only a load address and optional end link.
fn is_empty_basic_program(prg_bytes: &[u8], prg: &prg::Program) -> bool {
    prg.lines.is_empty() && prg_bytes.len() <= 4
}

/// Drop every `SYS`-parameter variable reference, so those sites stage their raw bytes exactly as they did before substitution existed.
fn strip_sys_param_vars(module: &mut ir::Module) -> Result<(), CompileError> {
    struct Required(bool);
    impl crate::visit::Visitor for Required {
        fn visit_stmt(&mut self, line: u16, stmt: &ir::Stmt) {
            if let ir::Stmt::Sys { param_vars, .. } = stmt {
                self.0 |= param_vars.iter().any(|v| v.requires_string_bridge);
            }
            crate::visit::walk_stmt(self, line, stmt);
        }
    }
    let mut required = Required(false);
    crate::visit::Visitor::visit_module(&mut required, module);
    if required.0 {
        return Err(CompileError::Codegen(codegen::CodegenError::Unsupported(
            "discarding required compiled-string SYS parameters for layout",
        )));
    }
    struct Strip;
    impl crate::visit::MutVisitor for Strip {
        fn visit_stmt_mut(&mut self, line_no: u16, stmt: &mut ir::Stmt) {
            if let ir::Stmt::Sys { param_vars, .. } = stmt {
                param_vars.clear();
            }
            crate::visit::walk_stmt_mut(self, line_no, stmt);
        }
    }
    crate::visit::MutVisitor::visit_module_mut(&mut Strip, module);
    Ok(())
}

pub fn compile_with_options(
    prg_bytes: &[u8],
    options: CompileOptions,
) -> Result<Compiled, CompileError> {
    let mut saw_sys_param_vars = false;
    match compile_at_profile_reporting(prg_bytes, options.clone(), &mut saw_sys_param_vars) {
        Ok(compiled) => {
            let compiled = prefer_dim_heap_headroom(compiled, &options, || {
                compile_at_profile(
                    prg_bytes,
                    CompileOptions {
                        profile: Profile::Size,
                        ..options.clone()
                    },
                )
            });
            Ok(prefer_compact_heap_headroom(compiled, &options, || {
                compile_at_profile_rescue(
                    prg_bytes,
                    CompileOptions {
                        profile: Profile::Size,
                        ..options.clone()
                    },
                    LayoutRescue::Compact,
                )
            }))
        }
        Err(first) => {

            if !matches!(first, CompileError::Assemble { .. }) {
                return Err(first);
            }
            if options.profile != Profile::Size {
                let retry = CompileOptions {
                    profile: Profile::Size,
                    ..options.clone()
                };
                match compile_at_profile(prg_bytes, retry) {
                    Ok(mut compiled) => {
                        compiled.diagnostics.size_profile_fallback = true;
                        return Ok(prefer_compact_heap_headroom(compiled, &options, || {
                            compile_at_profile_rescue(
                                prg_bytes,
                                CompileOptions {
                                    profile: Profile::Size,
                                    ..options.clone()
                                },
                                LayoutRescue::Compact,
                            )
                        }));
                    }
                    Err(_) => {}
                }
                // Continue when the image exceeds the layout limit.
                if saw_sys_param_vars {
                    for p in [options.profile, Profile::Size] {
                        let no_subst = CompileOptions {
                            profile: p,
                            no_sys_param_substitution: true,
                            ..options.clone()
                        };
                        if let Ok(mut compiled) = compile_at_profile(prg_bytes, no_subst) {
                            compiled.diagnostics.size_profile_fallback = p == Profile::Size;
                            compiled.diagnostics.sys_param_substitution_dropped = true;
                            return Ok(compiled);
                        }
                    }
                }
            }
            // Keep the established failure-recovery order unchanged.
            for rescue in [
                LayoutRescue::Dedup,
                LayoutRescue::PackedPrint,
                LayoutRescue::Compact,
                LayoutRescue::DataFirst,
            ] {
                // A changed pool placement can expose computed writes whose full extent auto-reserve does not know.
                if matches!(rescue, LayoutRescue::DataFirst) && options.reserved_ranges.is_empty() {
                    continue;
                }
                let retry = CompileOptions {
                    profile: Profile::Size,
                    ..options.clone()
                };
                if let Ok(mut compiled) = compile_at_profile_rescue(prg_bytes, retry, rescue) {
                    compiled.diagnostics.size_profile_fallback = options.profile != Profile::Size;
                    return Ok(prefer_rescued_heap_headroom(compiled, &options, rescue, || {
                        compile_at_profile_rescue(
                            prg_bytes,
                            CompileOptions {
                                profile: Profile::Size,
                                ..options.clone()
                            },
                            LayoutRescue::Compact,
                        )
                    }));
                }
            }
            Err(first)
        }
    }
}

/// Minimum DIM space does not guarantee enough capacity for all arrays.
fn prefer_dim_heap_headroom(
    first: Compiled,
    options: &CompileOptions,
    retry: impl FnOnce() -> Result<Compiled, CompileError>,
) -> Compiled {
    if options.profile != Profile::Default {
        return first;
    }
    let origin = options
        .custom_start_address
        .map(u32::from)
        .unwrap_or(CODE_ORIGIN);
    let Ok(Some(before)) = extraram_heap_window(&first, origin) else {
        return first;
    };
    if before >= MIN_STRING_HEAP {
        return first;
    }
    if let Ok(mut smaller) = retry() {
        if let Ok(Some(after)) = extraram_heap_window(&smaller, origin) {
            if after > before {
                smaller.diagnostics.size_profile_fallback = true;
                smaller.diagnostics.size_profile_heap_improvement = Some((before, after));
                return smaller;
            }
        }
    }
    first
}

/// Check live-string capacity after a successful size fallback.
fn prefer_compact_heap_headroom(
    first: Compiled,
    options: &CompileOptions,
    retry: impl FnOnce() -> Result<Compiled, CompileError>,
) -> Compiled {
    if options.profile != Profile::Default {
        return first;
    }
    let origin = options
        .custom_start_address
        .map(u32::from)
        .unwrap_or(CODE_ORIGIN);
    let Some(before) = managed_extraram_heap_window(&first, origin) else {
        return first;
    };
    if before >= MIN_STRING_HEAP {
        return first;
    }
    if let Ok(mut compact) = retry() {
        if let Some(after) = managed_extraram_heap_window(&compact, origin) {
            if after > before {
                compact.diagnostics.size_profile_fallback = true;
                compact.diagnostics.size_profile_heap_improvement = Some((before, after));
                compact.diagnostics.compact_heap_layout = true;
                return compact;
            }
        }
    }
    first
}

/// Check live-string capacity after a successful layout retry.
fn prefer_rescued_heap_headroom(
    first: Compiled,
    options: &CompileOptions,
    rescue: LayoutRescue,
    retry: impl FnOnce() -> Result<Compiled, CompileError>,
) -> Compiled {
    if matches!(rescue, LayoutRescue::Dedup | LayoutRescue::PackedPrint) {
        prefer_compact_heap_headroom(first, options, retry)
    } else {
        first
    }
}

/// Layout capacity for any compiler-managed extraram heap, including string-only heaps without runtime DIM.
fn managed_extraram_heap_window(build: &Compiled, origin: u32) -> Option<u32> {
    if !build.diagnostics.extraram.is_on()
        || !build.asm.contains(extraram::MEMSIZ_APPLY)
        || !build.asm.contains("__HEAP_PTR:")
    {
        return None;
    }
    let end = origin + build.machine_code.len() as u32;
    let ceiling = u32::from(extraram::heap_ceiling(
        &build.diagnostics.effective_reserved,
        end,
    ));
    Some(ceiling.saturating_sub(end))
}

#[derive(Clone, Copy, Default)]
enum LayoutRescue {
    #[default]
    Ordinary,
    Dedup,
    PackedPrint,
    Compact,
    DataFirst,
}

fn compile_at_profile_rescue(
    prg_bytes: &[u8],
    options: CompileOptions,
    rescue: LayoutRescue,
) -> Result<Compiled, CompileError> {
    compile_at_profile_reporting_rescue(prg_bytes, options, &mut false, rescue)
}

/// One build at exactly the profile in `options`, gates included.
fn compile_at_profile(
    prg_bytes: &[u8],
    options: CompileOptions,
) -> Result<Compiled, CompileError> {
    compile_at_profile_reporting(prg_bytes, options, &mut false)
}

fn compile_at_profile_reporting(
    prg_bytes: &[u8],
    options: CompileOptions,
    saw_sys_param_vars: &mut bool,
) -> Result<Compiled, CompileError> {
    compile_at_profile_reporting_rescue(
        prg_bytes,
        options,
        saw_sys_param_vars,
        LayoutRescue::Ordinary,
    )
}

fn compile_at_profile_reporting_rescue(
    prg_bytes: &[u8],
    options: CompileOptions,
    saw_sys_param_vars: &mut bool,
    rescue: LayoutRescue,
) -> Result<Compiled, CompileError> {

    let mut syntax_traps = Vec::new();
    let mut deferred_fn_bodies = Vec::new();
    let mut compiled = compile_pipeline(
        prg_bytes,
        options,
        &mut syntax_traps,
        &mut deferred_fn_bodies,
        saw_sys_param_vars,
        rescue,
    )?;
    compiled.diagnostics.syntax_traps = syntax_traps;
    compiled.diagnostics.deferred_fn_bodies = deferred_fn_bodies;

    reject_io_area_overlap(&compiled)?;
    Ok(compiled)
}

fn compile_pipeline(
    prg_bytes: &[u8],
    options: CompileOptions,
    syntax_traps: &mut Vec<crate::ast::SyntaxTrap>,
    deferred_fn_bodies: &mut Vec<crate::ast::DeferredFnBody>,
    // Set to whether the program has any `SYS`-parameter variable reference; see `compile_at_profile_reporting`.
    saw_sys_param_vars: &mut bool,
    rescue: LayoutRescue,
) -> Result<Compiled, CompileError> {
    if options.extraram && options.force_extraram_off {
        return Err(CompileError::Assemble {
            message: "--extraram and --force-extraram-off are mutually exclusive".to_string(),
            asm: None,
        });
    }
    let prg = prg::Program::parse(prg_bytes).map_err(CompileError::Prg)?;
    // Reject machine-language .prgs up front.
    if !looks_like_basic(&prg) && !is_empty_basic_program(prg_bytes, &prg) {
        return Err(CompileError::NotBasic {
            load_address: prg.load_address,
        });
    }
    // REM-hint pre-pass: extract dialect-specific pragmas. We honour
    // the declarations strictly — exactly how Basic 64 / Basic-Boss
    // treat them: a hinted var becomes integer-typed, and assignments
    // from float expressions auto-convert via FAC_TO_INT16 at the
    // store. This matches the user's mental model ("I said it's int,
    // make it int") and lets follow-on shadow-int and int-island
    // analysis pick up the rest of the chain.
    let hints = rem_hints::extract_hints_from(
        prg.lines.iter().map(|l| l.body.as_slice()),
        options.rem_hint_dialect,
    );
    let parse_opts = parse::ParseOptions {
        lenient_syntax: options.lenient_syntax,
        int_hint_vars: hints.int_vars.clone(),
        dialect: options.dialect,
        sys_register_args: options.sys_register_args,
    };
    let ast = parse::program_with_options(&prg, parse_opts).map_err(CompileError::Parse)?;
    let skipped_statements = ast.skipped.clone();
    syntax_traps.clone_from(&ast.traps);
    deferred_fn_bodies.clone_from(&ast.deferred_fn_bodies);
    // Names whose body did not parse.
    let poisoned_fns: std::collections::HashMap<crate::ast::FnName, u8> = ast
        .deferred_fn_bodies
        .iter()
        .map(|d| (d.name.clone(), d.code))
        .collect();
    let (mut module, source_integer_vars) =
        lower_and_optimize_with_source_types(&ast, options.profile)?;
    *saw_sys_param_vars = !crate::sys_params::scan_module(&module).is_empty();
    if options.no_sys_param_substitution {
        // Retry SYS sites with raw parameter text to reduce code size.
        strip_sys_param_vars(&mut module)?;
    }
    let origin = options
        .custom_start_address
        .map(u32::from)
        .unwrap_or(CODE_ORIGIN);
    let (asm, zp_pressure) = codegen::emit_with_profile_at_opts_diag_and_data_rescue(
        &module,
        options.profile,
        origin as u16,
        options.safe_sys_calls,
        &hints,
        &poisoned_fns,
        source_integer_vars,
        matches!(rescue, LayoutRescue::Compact | LayoutRescue::DataFirst),
    )
    .map_err(CompileError::Codegen)?;
    // Run asm-level peephole passes before extraram rewrites so liveness analysis sees raw ROM calls.
    let peep = |a: &str| {
        let optimized = crate::peephole::run_with_options(
            a,
            options.profile,
            crate::peephole::Options {
                inline_str_print_args: true,
                aggressive_size_dedup: !matches!(rescue, LayoutRescue::Ordinary),
                compact_size_dedup: matches!(rescue, LayoutRescue::Compact | LayoutRescue::DataFirst),
                ..crate::peephole::Options::default()
            },
        );
        let packed = match rescue {
            LayoutRescue::PackedPrint => crate::packed_print::compress(&optimized).unwrap_or(optimized),
            LayoutRescue::Compact | LayoutRescue::DataFirst => crate::packed_print::compress_compact(&optimized)
                .or_else(|| crate::packed_print::compress(&optimized))
                .unwrap_or(optimized),
            _ => optimized,
        };
        if matches!(rescue, LayoutRescue::DataFirst) {
            data_before_constants(&packed).unwrap_or(packed)
        } else {
            packed
        }
    };
    let (asm, asm_copy_base) = resolve_val_variants(&asm, peep);

    let inplace = original_image_to_materialise(prg_bytes, &ast, &module, &options);
    let base_asm = asm;

    let attempt = |image: Option<InplaceImage<'_>>| -> Result<Compiled, CompileError> {

    let splice = |base: &str| -> Result<Option<String>, CompileError> {
        if let Some(image) = image {
            Ok(Some(materialise_original_image(base, image)))
        } else if module_reads_basic_txttab(&module) {

            if let Some(message) = basic_text_copy_ram_overrun(prg_bytes.len().saturating_sub(2)) {
                return Err(CompileError::Assemble { message, asm: None });
            }
            Ok(Some(embed_original_basic_text(base, prg_bytes)))
        } else {
            Ok(None)
        }
    };
    let spliced = splice(&base_asm)?;
    let asm: std::borrow::Cow<'_, str> = match spliced {
        Some(s) => std::borrow::Cow::Owned(s),
        None => std::borrow::Cow::Borrowed(base_asm.as_str()),
    };
    let asm_copy: Option<String> = match asm_copy_base.as_deref() {
        Some(base) => match splice(base)? {
            Some(s) => Some(s),
            None => Some(base.to_string()),
        },
        None => None,
    };
    let asm_copy = asm_copy.as_deref();

    if image.is_some() && !options.force_extraram_off {
        let mut off_failure: Option<CompileError> = None;
        if !options.extraram {
            match assemble_pass_dropping_unpaid_bss_fill(
                &asm,
                asm_copy,
                &module,
                false,
                &options,
                ExtraRamDecision::OffOriginalImage,
                &skipped_statements,
                zp_pressure,
                image,
            ) {
                Ok(built) => {
                    match verify_below_rom_window(&built, origin)
                        .and_then(|()| verify_string_heap(&built, origin))
                    {
                        Ok(()) => return Ok(built),
                        Err(err) => off_failure = Some(err),
                    }
                }
                Err(err) => off_failure = Some(err),
            }
        }
        let decision = if options.extraram {
            ExtraRamDecision::ForcedOn
        } else {
            ExtraRamDecision::AutoOn
        };
        return match assemble_pass_dropping_unpaid_bss_fill(
            &asm,
            asm_copy,
            &module,
            true,
            &options,
            decision,
            &skipped_statements,
            zp_pressure,
            image,
        ) {
            Ok(built) => verify_extraram_heap(&built, origin).map(|()| built),
            Err(err) => Err(binding_layout_failure(off_failure, err)),
        };
    }

    // Decide extraram. Three precedence levels:
    //   1. `--extraram` forces it on (user opt-in for any reason).
    //   2. `--force-extraram-off` keeps it off no matter what (escape
    //      hatch when the user knows ROM access is required, or wants
    //      a deterministic non-banked binary).
    //   3. Otherwise the auto-predictor runs a probe assembly without
    //      extraram, measures where the program ends, and switches it
    //      on when end address ≥ AUTO_EXTRARAM_TRIGGER (\$9F00).
    //
    // The probe is a real assemble — same reserved-range list, same
    // peephole'd asm. When the predictor decides "no extraram", we
    // ship the probe result directly; no second pass needed. When it
    // decides "yes extraram", we throw away the probe binary and run
    // the assemble step a second time with the extraram rewrite.
    if options.extraram {
        let built = assemble_pass_dropping_unpaid_bss_fill(
            &asm,
            asm_copy,
            &module,
            true,
            &options,
            ExtraRamDecision::ForcedOn,
            &skipped_statements,
            zp_pressure,
            image,
        )?;
        verify_extraram_heap(&built, origin)?;
        return Ok(built);
    }
    if options.force_extraram_off {
        let built = assemble_pass_dropping_unpaid_bss_fill(
            &asm,
            asm_copy,
            &module,
            false,
            &options,
            ExtraRamDecision::ForcedOff,
            &skipped_statements,
            zp_pressure,
            image,
        )?;
        verify_below_rom_window(&built, origin)?;
        verify_string_heap(&built, origin)?;
        return Ok(built);
    }
    // When the custom origin already sits at or above the BASIC-ROM
    // shadow ($A000-$BFFF), extraram can't add code space — that area
    // is below the program. Skip the predictor so a `--start-address
    // =$C000` build doesn't get a pointless JT prologue. Lower custom
    // origins ($0900, $8000, …) still go through the normal predictor
    // below so a program at $8000 that grows past $9F00 picks up the
    // extra 8 KB just like a default-origin build would.
    if matches!(options.custom_start_address, Some(addr) if u32::from(addr) >= 0xC000) {
        return assemble_pass_dropping_unpaid_bss_fill(
            &asm,
            asm_copy,
            &module,
            false,
            &options,
            ExtraRamDecision::SkippedHighOrigin,
            &skipped_statements,
            zp_pressure,
            image,
        );
    }
    let probe = assemble_pass_dropping_unpaid_bss_fill(
        &asm,
        asm_copy,
        &module,
        false,
        &options,
        ExtraRamDecision::AutoOff,
        &skipped_statements,
        zp_pressure,
        image,
    )?;
    let probe_end = origin + probe.machine_code.len() as u32;
    if probe_end < AUTO_EXTRARAM_TRIGGER && string_heap_window(&probe, origin).is_ok() {
        return Ok(probe);
    }
    // Predictor: either the *image* would land at or past \$9F00, or it fits but leaves the
    // runtime *string heap* starved (see `MIN_STRING_HEAP`).
    let extraram_build = assemble_pass_dropping_unpaid_bss_fill(
        &asm,
        asm_copy,
        &module,
        true,
        &options,
        ExtraRamDecision::AutoOn,
        &skipped_statements,
        zp_pressure,
        image,
    );
    match extraram_build {
        // The escalation's own post-condition.
        Ok(built) => verify_extraram_heap(&built, origin).map(|()| built),

        Err(err) if probe_end >= AUTO_EXTRARAM_TRIGGER => Err(err),

        Err(_) => {
            verify_below_rom_window(&probe, origin)?;
            verify_string_heap(&probe, origin)?;
            Ok(probe)
        }
    }
    };
    let Some(image) = inplace else {
        return attempt(None);
    };
    if !image.is_low_trimmed() {
        return attempt(Some(image));
    }
    // The two-way build.
    let full = InplaceImage {
        low_cut: INPLACE_HEAD,
        ..image
    };
    let low = attempt(Some(image));
    // Diagnostic: `YAB_DEBUG_LOWTRIM=1` reports what the low-trim attempt did.
    if std::env::var("YAB_DEBUG_LOWTRIM").is_ok() {
        let (lo, hi) = image.freed_span().unwrap_or((0, 0));
        match &low {
            Ok(c) => eprintln!(
                "low-trim: freed ${lo:04X}-${hi:04X} ({} bytes), image {} bytes, ends ${:04X}",
                usize::from(hi - lo) + 1,
                c.prg_bytes.len(),
                c.diagnostics.end_address,
            ),
            Err(e) => eprintln!("low-trim: freed ${lo:04X}-${hi:04X} REFUSED: {e}"),
        }
    }
    match (low, attempt(Some(full))) {
        (Ok(low), Ok(full)) => Ok(if low.prg_bytes.len() < full.prg_bytes.len() {
            low
        } else {
            full
        }),
        // The rescue case: the untrimmed image does not fit the machine and the trimmed one does.
        (Ok(low), Err(_)) => Ok(low),

        (Err(_), Ok(full)) => Ok(full),
        (Err(_), Err(err)) => Err(err),
    }
}

/// Require enough space for the string heap when extra RAM is disabled.
fn verify_string_heap(build: &Compiled, origin: u32) -> Result<(), CompileError> {
    if build.diagnostics.extraram.is_on() {
        // $A000-$BFFF is RAM in this layout, so MEMSIZ is not the heap's real ceiling and this
        // window means nothing.
        return Ok(());
    }
    let Err(window) = string_heap_window(build, origin) else {
        return Ok(());
    };
    let end = origin + build.machine_code.len() as u32;
    Err(CompileError::Assemble {
        message: format!(
            "?OUT OF MEMORY: the program ends at ${end:04X}, leaving only {window} bytes \
             of string heap below MEMSIZ (${DEFAULT_MEMSIZ:04X}); at least {} are needed. \
             Strings allocated past ${DEFAULT_MEMSIZ:04X} are written to RAM but read back \
             from BASIC ROM, so they come back as garbage with no error. Drop \
             --force-extraram-off so the RAM-under-ROM layout can be used, or shrink the \
             program.",
            MIN_STRING_HEAP,
        ),
        asm: Some(build.asm.clone()),
    })
}

/// Preserve the plain-layout failure and append the expanded layout's size constraints when it also fails.
fn binding_layout_failure(
    off_failure: Option<CompileError>,
    on_failure: CompileError,
) -> CompileError {
    let Some(off) = off_failure else {
        return on_failure;
    };
    // Only compose when both are `Assemble`.
    let (
        CompileError::Assemble {
            message: off_msg,
            asm: off_asm,
        },
        CompileError::Assemble {
            message: on_msg, ..
        },
    ) = (&off, &on_failure)
    else {
        return off;
    };
    // Compose only the two recognised layout constraints, keeping other errors intact.
    if !on_msg.contains(extraram::DATA_CEILING_MARKER) && !on_msg.contains(extraram::IO_WALL_MARKER)
    {
        return off;
    }
    CompileError::Assemble {
        message: format!(
            "{off_msg} The expanded RAM-under-ROM layout also fails; the ceiling to work \
             against is: {on_msg} Interpret these emitted-byte figures as layout overshoots, \
             not as how much BASIC to delete: the layout is re-planned on every build."
        ),
        asm: off_asm.clone(),
    }
}

/// Reject a load image that overlaps BASIC ROM without extra RAM enabled.
fn verify_below_rom_window(build: &Compiled, origin: u32) -> Result<(), CompileError> {
    if build.diagnostics.extraram.is_on() || origin >= DEFAULT_MEMSIZ {
        return Ok(());
    }
    let end = origin + build.machine_code.len() as u32;
    if end <= DEFAULT_MEMSIZ {
        return Ok(());
    }
    // Why extraram is not available decides what the user can do about it, so the remedy has to say which of the two situations this is.
    let remedy = if build.diagnostics.extraram == ExtraRamDecision::OffOriginalImage {

        "This program uses bytes from its original loaded image, so the compiler had to \
         hand that image back at $0801 and place the \
         compiled code above it; the RAM-under-ROM layout was tried as well and does not \
         fit either. Shrink the program"
    } else {
        "Drop --force-extraram-off so the RAM-under-ROM layout can be used, or remove"
    };
    Err(CompileError::Assemble {
        message: format!(
            "?OUT OF MEMORY: without extraram the program may not pass \
             ${DEFAULT_MEMSIZ:04X}, but this image ends at ${end:04X} — {} bytes of it lie \
             under the BASIC ROM window ($A000-$BFFF). Those bytes are written to RAM and \
             read back as ROM, so the program runs on garbage with no error. {remedy} about \
             {} bytes (shorter DATA, smaller arrays, fewer string literals).",
            end - DEFAULT_MEMSIZ,
            end - DEFAULT_MEMSIZ,
        ),
        asm: Some(build.asm.clone()),
    })
}

/// Bytes of string heap the finished extraram-OFF `build` leaves between the top of its load
/// image and MEMSIZ, or `Err(window)` when that is below [`MIN_STRING_HEAP`].
fn string_heap_window(build: &Compiled, origin: u32) -> Result<Option<u32>, u32> {
    if !build.asm.contains("__HEAP_BOTTOM:") {
        return Ok(None);
    }
    if origin >= DEFAULT_MEMSIZ {
        return Ok(None);
    }
    let end = origin + build.machine_code.len() as u32;
    let window = DEFAULT_MEMSIZ.saturating_sub(end);
    if window < MIN_STRING_HEAP {
        Err(window)
    } else {
        Ok(Some(window))
    }
}

fn extraram_heap_window(build: &Compiled, origin: u32) -> Result<Option<u32>, u32> {
    if !build.diagnostics.extraram.is_on() {
        return Ok(None);
    }
    if !build.asm.contains(extraram::MEMSIZ_APPLY) {
        return Ok(None);
    }
    if !build.asm.contains("__ARR_HEAP_TOP:") {
        return Ok(None);
    }
    let end = origin + build.machine_code.len() as u32;
    let ceiling = u32::from(extraram::heap_ceiling(
        &build.diagnostics.effective_reserved,
        end,
    ));
    let window = ceiling.saturating_sub(end);
    if window < MIN_EXTRARAM_DIM_HEAP {
        Err(window)
    } else {
        Ok(Some(window))
    }
}

fn verify_extraram_heap(build: &Compiled, origin: u32) -> Result<(), CompileError> {
    let Err(window) = extraram_heap_window(build, origin) else {
        return Ok(());
    };
    let end = origin + build.machine_code.len() as u32;
    let ceiling = u32::from(extraram::heap_ceiling(
        &build.diagnostics.effective_reserved,
        end,
    ));
    // A saturated window of zero hides any image overshoot.
    let deficit = (end + MIN_EXTRARAM_DIM_HEAP).saturating_sub(ceiling);
    Err(CompileError::Assemble {
        message: format!(
            "?OUT OF MEMORY: the program ends at ${end:04X}, leaving only {window} bytes \
             below ${ceiling:04X} for both the string heap and the arrays its runtime DIM \
             statements allocate downwards from there; at least {} are needed. The first \
             DIM would raise ?OUT OF MEMORY. Shrink the program by about {deficit} bytes \
             (shorter DATA, smaller arrays, fewer string literals).",
            MIN_EXTRARAM_DIM_HEAP,
        ),
        asm: Some(build.asm.clone()),
    })
}

/// [`assemble_pass`], plus the guarantee that the BSS zero-fill loop never costs a program bytes.
fn resolve_val_variants(asm: &str, peep: impl Fn(&str) -> String) -> (String, Option<String>) {
    let default = peep(&extraram::select_val_variant(asm, false));
    let copy = extraram::has_val_variants(asm)
        .then(|| peep(&extraram::select_val_variant(asm, true)));
    (default, copy)
}

/// Every path to the assembler goes through here, which is why the `VAL` variant choice is made here and nowhere else.
fn assemble_pass_dropping_unpaid_bss_fill(
    asm: &str,
    asm_val_copy: Option<&str>,
    module: &ir::Module,
    extraram_on: bool,
    options: &CompileOptions,
    extraram_decision: ExtraRamDecision,
    skipped_statements: &[crate::ast::SkippedStatement],
    zp_pressure: codegen::ZpPressure,
    inplace: Option<InplaceImage<'_>>,
) -> Result<Compiled, CompileError> {
    let built = assemble_pass_dropping_unpaid_bss_fill_inner(
        asm,
        module,
        extraram_on,
        options,
        extraram_decision,
        skipped_statements,
        zp_pressure,
        inplace,
    );
    let Some(copied) = asm_val_copy else {
        return built;
    };
    let origin = options
        .custom_start_address
        .map(u32::from)
        .unwrap_or(CODE_ORIGIN);
    let exposed = match &built {
        // `machine_code`, not `prg_bytes`: a truncated BSS tail is absent from the file and present in memory, and the heap starts past what is in memory.
        Ok(c) => {
            extraram_on
                && origin + (c.machine_code.len() as u32) < u32::from(extraram::ROM_WINDOW_TOP)
        }
        Err(_) => false,
    };
    if !exposed {
        return built;
    }
    let protected = assemble_pass_dropping_unpaid_bss_fill_inner(
        copied,
        module,
        extraram_on,
        options,
        extraram_decision,
        skipped_statements,
        zp_pressure,
        inplace,
    );
    if protected.is_ok() {
        return protected;
    }

    let lifted = format!("{asm}\n{VAL_HIGH_HEAP_MARKER}\n");
    assemble_pass_dropping_unpaid_bss_fill_inner(
        &lifted, module, extraram_on, options, extraram_decision,
        skipped_statements, zp_pressure, inplace,
    )
}

fn assemble_pass_dropping_unpaid_bss_fill_inner(
    asm: &str,
    module: &ir::Module,
    extraram_on: bool,
    options: &CompileOptions,
    extraram_decision: ExtraRamDecision,
    skipped_statements: &[crate::ast::SkippedStatement],
    zp_pressure: codegen::ZpPressure,
    inplace: Option<InplaceImage<'_>>,
) -> Result<Compiled, CompileError> {
    let built = assemble_pass_settling_bss_fill(
        asm,
        module,
        extraram_on,
        options,
        extraram_decision,
        skipped_statements,
        zp_pressure,
        inplace,
    );
    if built.is_ok() || !asm.contains(crate::peephole::STR_PRINT_INLINE_ENTRY) {
        return built;
    }
    let plain = crate::peephole::undo_str_print_inline_args(asm);
    match assemble_pass_settling_bss_fill(
        &plain,
        module,
        extraram_on,
        options,
        extraram_decision,
        skipped_statements,
        zp_pressure,
        inplace,
    ) {
        Ok(mut c) => {
            c.diagnostics.inline_str_args_undone = true;
            Ok(c)
        }
        Err(_) => built,
    }
}

fn assemble_pass_settling_bss_fill(
    asm: &str,
    module: &ir::Module,
    extraram_on: bool,
    options: &CompileOptions,
    extraram_decision: ExtraRamDecision,
    skipped_statements: &[crate::ast::SkippedStatement],
    zp_pressure: codegen::ZpPressure,
    inplace: Option<InplaceImage<'_>>,
) -> Result<Compiled, CompileError> {
    let with_fill = assemble_pass(
        asm,
        module,
        extraram_on,
        options,
        extraram_decision,
        skipped_statements,
        zp_pressure,
        inplace,
    );
    if !extraram::has_bss_tail_fill(asm) {
        return with_fill;
    }
    let settled = match &with_fill {
        // extraram strips the fill itself, so the retry would assemble byte-identical input.
        Ok(c) => {
            c.diagnostics.bss_truncated > 0
                && c.diagnostics.effective_reserved.is_empty()
                && !c.diagnostics.extraram.is_on()
        }
        Err(_) => false,
    };
    if settled {
        return with_fill;
    }
    let without_fill = assemble_pass(
        &extraram::strip_bss_tail_fill(asm),
        module,
        extraram_on,
        options,
        extraram_decision,
        skipped_statements,
        zp_pressure,
        inplace,
    );
    match (with_fill, without_fill) {
        (Ok(a), Ok(b)) => Ok(if b.prg_bytes.len() < a.prg_bytes.len() {
            b
        } else {
            a
        }),

        (Err(_), Ok(b)) => Ok(b),
        (Ok(a), Err(_)) => Ok(a),
        (Err(a), Err(_)) => Err(a),
    }
}

/// Run the assemble + extraram-rewrite + reserved-range setup once
/// against `asm`. Returns the produced binary plus the (possibly
/// rewritten) asm text. Pulled out of `compile_with_options` so the
/// auto-predictor can reuse it for both the probe pass and the final
/// extraram-on pass without duplicating reserved-range plumbing.
fn assemble_pass(
    asm: &str,
    module: &ir::Module,
    extraram_on: bool,
    options: &CompileOptions,
    extraram_decision: ExtraRamDecision,
    skipped_statements: &[crate::ast::SkippedStatement],
    zp_pressure: codegen::ZpPressure,
    inplace: Option<InplaceImage<'_>>,
) -> Result<Compiled, CompileError> {
    let inplace_body = inplace.map(|i| i.body);
    let inplace_window = inplace_body.map(original_image_bounds);
    let mut auto_ranges: Vec<(u16, u16)> = if options.auto_reserve {
        discover_reserved_ranges(module)
    } else {
        Vec::new()
    };
    if options.auto_reserve
        && let Some(InplaceImage {
            trigger: InplaceTrigger::CopiedPayload { destination }, ..
        }) = inplace
    {
        // The copy reads the preserved source, but also writes a complete destination block.
        auto_ranges.push(destination);
    }
    // Machine code the program runs out of its OWN image can aim the ROM's block move somewhere the BASIC text never names.
    if options.auto_reserve && let Some(body) = inplace_body {
        ml_rom_block_move_destinations(body, &mut auto_ranges);

        ml_page_copy_destinations(body, &mut auto_ranges);
        ml_indexed_clear_destinations(body, &mut auto_ranges);
        ml_indexed_page_copy_destinations(body, &mut auto_ranges);

        ml_selfmod_page_copy_destinations(body, original_image_bounds(body).0, &mut auto_ranges);
        note_sys_ml_write_extent(body, module, &mut auto_ranges);
    }

    let mut manual_ranges: Vec<(u16, u16)> = options.reserved_ranges.clone();
    let mut manual_reserved_clipped: Vec<(u16, u16)> = Vec::new();

    if let Some((free_lo, free_hi)) = inplace.and_then(|i| i.freed_span()) {
        let hits = |r: &(u16, u16)| r.1 >= free_lo && r.0 <= free_hi;
        if let Some((lo, hi)) = auto_ranges
            .iter()
            .chain(manual_ranges.iter())
            .find(|r| hits(r))
        {
            return Err(CompileError::Assemble {
                message: format!(
                    "low trim refused: reserved range ${lo:04X}-${hi:04X} lies in the \
                     ${free_lo:04X}-${free_hi:04X} span the trim would hand to compiled code"
                ),
                asm: None,
            });
        }
    }
    if let Some((lo, hi)) = inplace_window {
        let (kept, _) = clip_ranges_against_inplace_window(auto_ranges.drain(..), lo, hi);
        auto_ranges = kept;
        let (kept, dropped) = clip_ranges_against_inplace_window(manual_ranges.drain(..), lo, hi);
        manual_ranges = kept;
        manual_reserved_clipped = dropped;
    }
    let mut all_reserved: Vec<(u16, u16)> = Vec::new();
    all_reserved.extend(auto_ranges.iter().copied());
    all_reserved.extend(manual_ranges.iter().copied());

    if let Some(image) = inplace.filter(InplaceImage::is_low_trimmed) {
        all_reserved.push((image.tail_start(), image.image_last()));
    }
    let merged = merge_reserved_ranges(all_reserved);
    // Diagnostic: set `YAB_DEBUG_RESERVED=1` in the environment to
    // dump the resolved reserved ranges to stderr. Useful when
    // tuning auto-reserve heuristics for a new program.
    if std::env::var("YAB_DEBUG_RESERVED").is_ok() {
        eprintln!("auto-reserved ranges:");
        for (s, e) in &merged {
            eprintln!("  ${s:04X}-${e:04X}");
        }
    }

    // The DATA pool is one contiguous run of `[len, ascii…]` records
    // that the BASIC FIN parser walks byte-by-byte. asm6502 treats it
    // like any other run of bytes, so when a reservation falls in the
    // middle of the run, the assembler bridges over the hole with a
    // JMP-style insertion + zero pad — which the FIN parser reads as
    // length-0 records and garbage entries. The first iteration past
    // the bridge returns 0, and the remainder of the pool is
    // unreachable. Emit `*=<addr>` before `__DATA:` so the whole pool
    // lands contiguously past every reservation.
    let (final_asm, absorbed) = if extraram_on {
        (
            extraram::inject_with_reserved(
                asm,
                &merged,
                zp_pressure.program_touches_memsiz,
            ),
            Vec::new(),
        )
    } else {
        relocate_data_past_reservations(asm, &merged)
    };

    let assembler_reserved: Vec<(u16, u16)> = merged
        .iter()
        .copied()
        .filter(|r| !absorbed.contains(r))
        .collect();

    let mut assembled = assemble_with_aligned_vectors(final_asm, &assembler_reserved).map_err(|e| {
        explain_address_space_overflow(
            e,
            options.custom_start_address.unwrap_or(CODE_ORIGIN as u16),
            &assembler_reserved,
        )
    })?;
    if extraram_on && asm.contains(VAL_HIGH_HEAP_MARKER) {
        let floor = assembled.assembler.lookup("__HEAP_BOTTOM").ok_or_else(|| CompileError::Assemble {
            message: "VAL layout has no linked heap marker".into(),
            asm: Some(assembled.asm.clone()),
        })?;
        if floor < extraram::ROM_WINDOW_TOP {
            let origin = options.custom_start_address.unwrap_or(CODE_ORIGIN as u16);
            if u32::from(floor) != u32::from(origin) + assembled.machine_code.len() as u32
                || assembled.asm.contains("__BSS_START:\n")
            {
                return Err(CompileError::Assemble {
                    message: "VAL layout cannot pad a nonterminal heap or BSS tail".into(),
                    asm: Some(assembled.asm),
                });
            }
            let padded = pad_terminal_heap(&assembled.asm, usize::from(extraram::ROM_WINDOW_TOP - floor))
                .ok_or_else(|| CompileError::Assemble {
                    message: "VAL layout cannot locate its terminal heap marker".into(),
                    asm: Some(assembled.asm.clone()),
                })?;
            let relinked = assemble_with_aligned_vectors(padded, &assembler_reserved)?;
            assembled = Assembled {
                pads: assembled.pads + relinked.pads,
                shims: assembled.shims + relinked.shims,
                inline_args_undone: assembled.inline_args_undone || relinked.inline_args_undone,
                ..relinked
            };
            if assembled.assembler.lookup("__HEAP_BOTTOM").is_none_or(|floor| floor < extraram::ROM_WINDOW_TOP) {
                return Err(CompileError::Assemble {
                    message: "VAL heap still overlaps BASIC ROM after linked padding".into(),
                    asm: Some(assembled.asm),
                });
            }
        }
    }
    if let Some(image) = inplace.filter(InplaceImage::is_low_trimmed)
        && matches!(image.trigger, InplaceTrigger::CopiedPayload { .. })
        && let Some(floor) = assembled.assembler.lookup("__HEAP_BOTTOM")
        && floor <= image.image_last()
    {
        // patch_low_trim_tail may extend a short image up to its payload, but that cannot move the already linked heap pointer.
        return Err(CompileError::Assemble {
            message: format!(
                "low trim refused: heap starts at ${floor:04X}, below the copied payload's \
                 exclusive end ${:04X}",
                u32::from(image.image_last()) + 1,
            ),
            asm: Some(assembled.asm),
        });
    }
    if extraram_on {
        if let Some(floor) = assembled.assembler.lookup("__HEAP_BOTTOM") {
            if let Some(refined) =
                extraram::refine_heap_ceiling(&assembled.asm, &merged, u32::from(floor))
            {
                let relinked = assemble_with_aligned_vectors(refined, &assembler_reserved)?;
                // Only immediate values changed.
                let raise = assembled.assembler.lookup("__EXRAM_MEMSIZ_RAISE");
                let origin = options.custom_start_address.unwrap_or(CODE_ORIGIN as u16);
                let immediate_offsets = raise.map(|addr| {
                    let offset = usize::from(addr) - usize::from(origin);
                    [offset + 1, offset + 5]
                });
                if relinked.machine_code.len() != assembled.machine_code.len()
                    || relinked.assembler.lookup("__HEAP_BOTTOM") != Some(floor)
                    || raise != relinked.assembler.lookup("__EXRAM_MEMSIZ_RAISE")
                    || assembled.machine_code.iter().zip(&relinked.machine_code)
                        .enumerate().any(|(index, (before, after))| {
                            before != after
                                && !immediate_offsets.is_some_and(|offsets| offsets.contains(&index))
                        })
                {
                    return Err(CompileError::Assemble {
                        message: "heap-ceiling refinement changed the image outside its two immediates".to_string(),
                        asm: Some(relinked.asm),
                    });
                }
                // The second assembly starts from the already repaired source.
                assembled = Assembled {
                    pads: assembled.pads + relinked.pads,
                    shims: assembled.shims + relinked.shims,
                    inline_args_undone: assembled.inline_args_undone || relinked.inline_args_undone,
                    ..relinked
                };
            }
        }
    }
    let Assembled {
        assembler,
        mut machine_code,
        asm: final_asm,
        vectors: indirect_vectors,
        pads: vector_pads,
        shims: bridge_shims,
        inline_args_undone,
    } = assembled;
    verify_absorbed_ranges_filled(&absorbed, |name| assembler.lookup(name)).map_err(|message| {
        CompileError::Assemble {
            message,
            asm: Some(final_asm.clone()),
        }
    })?;

    // The image has to fit the 6502's address space.
    {
        let origin = options.custom_start_address.unwrap_or(CODE_ORIGIN as u16) as usize;
        let end = origin + machine_code.len();
        if end > 0x1_0000 {
            let over = end - 0x1_0000;
            return Err(CompileError::Assemble {
                message: format!(
                    "?OUT OF MEMORY: the program needs {} bytes from ${:04X}, which is \
                     {over} bytes past the top of memory. Reduce data (smaller arrays, \
                     fewer string literals, less DATA).",
                    machine_code.len(),
                    origin,
                ),
                asm: Some(final_asm.clone()),
            });
        }
    }

    // The start-up copy of the original BASIC text, if there is one.
    verify_embedded_text_pad(
        &final_asm,
        options.custom_start_address.unwrap_or(CODE_ORIGIN as u16),
        machine_code.len(),
    )
    .map_err(|message| CompileError::Assemble {
        message,
        asm: Some(final_asm.clone()),
    })?;

    if extraram_on {

        let image_end = u32::from(options.custom_start_address.unwrap_or(CODE_ORIGIN as u16))
            + machine_code.len() as u32;
        extraram::verify_data_fits(
            |name| assembler.lookup(name),
            options.extraram,
            image_end,
            extraram::heap_ceiling(&merged, image_end),
        )
        .map_err(|message| {
            CompileError::Assemble {
                message,
                // verify_data_fits walks label addresses after a
                // successful assemble, so the asm we just shipped to
                // the assembler is what produced this overflow —
                // attach it for debugging.
                asm: Some(final_asm.clone()),
            }
        })?;
    }
    verify_data_pool_unsplit(&merged, |name| assembler.lookup(name)).map_err(|message| {
        CompileError::Assemble {
            message,
            asm: Some(final_asm.clone()),
        }
    })?;
    verify_data_pool_contiguous(&final_asm, |name| assembler.lookup(name)).map_err(|message| {
        CompileError::Assemble {
            message,
            asm: Some(final_asm.clone()),
        }
    })?;

    verify_label_bytes(
        &final_asm,
        &machine_code,
        options.custom_start_address.unwrap_or(CODE_ORIGIN as u16),
        |name| assembler.lookup(name),
    )
    .map_err(|message| CompileError::Assemble {
        message,
        asm: Some(final_asm.clone()),
    })?;

    verify_addressing_modes(&final_asm, |name| assembler.lookup(name)).map_err(|message| {
        CompileError::Assemble {
            message,
            asm: Some(final_asm.clone()),
        }
    })?;

    // Put the surviving tail of a low-trimmed image back at its own address, in the hole the reservation held open for it.
    let low_trim_tail = match inplace.filter(InplaceImage::is_low_trimmed) {
        None => None,
        Some(image) => Some(
            patch_low_trim_tail(
                &mut machine_code,
                options.custom_start_address.unwrap_or(CODE_ORIGIN as u16),
                image,
            )
            .map_err(|message| CompileError::Assemble {
                message,
                asm: Some(final_asm.clone()),
            })?,
        ),
    };

    let keep = bss_tail_keep_len(
        &machine_code,
        options.custom_start_address.unwrap_or(CODE_ORIGIN as u16),
        |name| assembler.lookup(name),
    );

    if let Some(tail_end) = low_trim_tail
        && keep < tail_end
    {
        return Err(CompileError::Assemble {
            message: format!(
                "internal error: the BSS tail trim would cut the file to {keep} bytes, but the \
                 original image's tail runs to {tail_end}; the payload would be missing"
            ),
            asm: Some(final_asm.clone()),
        });
    }
    let bss_truncated = machine_code.len() - keep;
    let stored = &machine_code[..keep];
    let (prg_bytes, start_address) = if let Some(addr) = options.custom_start_address {
        (pack::pack_raw(addr, stored), addr)
    } else {
        (pack::pack(stored), 0x0801)
    };

    if let Some(image) = inplace {
        verify_original_image_verbatim(&prg_bytes, image).map_err(|message| {
            CompileError::Assemble {
                message,
                asm: Some(final_asm.clone()),
            }
        })?;
    }
    // end_address is the inclusive last byte of compiled code in
    // memory after the program has loaded. For default builds the
    // code starts at $080D (past the 12-byte SYS stub); for custom
    // builds the code starts at start_address itself.
    let code_origin = options.custom_start_address.unwrap_or(CODE_ORIGIN as u16);
    let end_address = code_origin
        .saturating_add(machine_code.len().saturating_sub(1).min(u16::MAX as usize) as u16);

    let file_end = start_address
        .saturating_add((prg_bytes.len().saturating_sub(3)).min(u16::MAX as usize) as u16);
    // The low trim's own reservation is not a hole and must never be reported as one.
    let overshoot_input: Vec<(u16, u16)> = match inplace.filter(InplaceImage::is_low_trimmed) {
        None => merged.clone(),
        Some(image) => {
            let tail = (image.tail_start(), image.image_last());
            merged.iter().copied().filter(|r| *r != tail).collect()
        }
    };
    let reserved_overshoots = reserved_tail_overshoots(start_address, file_end, &overshoot_input);
    let diagnostics = Diagnostics {
        start_address,
        end_address,
        extraram: extraram_decision,
        auto_reserved: auto_ranges,
        manual_reserved: manual_ranges,
        manual_reserved_clipped,
        effective_reserved: merged.clone(),
        skipped_statements: skipped_statements.to_vec(),
        sys_param_vars: crate::sys_params::scan_module(module),

        syntax_traps: Vec::new(),
        deferred_fn_bodies: Vec::new(),
        zp_pressure,
        indirect_vectors,
        vector_pads,
        bridge_shims,
        inline_str_args_undone: inline_args_undone,
        bss_truncated,
        reserved_overshoots,
        original_image: inplace_window,
        // Set by `compile_with_options` only, and only on the retry.
        size_profile_fallback: false,
        size_profile_heap_improvement: None,
        compact_heap_layout: false,
        sys_param_substitution_dropped: false,
    };
    Ok(Compiled {
        asm: final_asm,
        machine_code,
        prg_bytes,
        diagnostics,
    })
}

/// Final assembly, bytes and symbol table after layout checks.
struct Assembled {
    assembler: Assembler6502,
    machine_code: Vec<u8>,
    asm: String,
    /// Every `JMP (vector)` in the image, sorted by address.
    vectors: Vec<(String, u16)>,
    /// Pad bytes inserted to settle the layout; normally 0.
    pads: usize,
    /// Filler bytes inserted to keep a label attached to the item it names across a
    /// reserved-range bridge; normally 0.
    shims: usize,

    inline_args_undone: bool,
}

/// Describe assembler failures caused by address-space overflow.
fn explain_address_space_overflow(
    err: CompileError,
    origin: u16,
    reserved: &[(u16, u16)],
) -> CompileError {
    let CompileError::Assemble {
        message: original,
        asm: Some(asm),
    } = &err
    else {
        return err;
    };
    let mut probe = Assembler6502::new();
    let probed = probe.assemble_full(asm).ok().map(|(code, _)| code.len());

    let (len, exact) = match probed {
        Some(n) => (Some(n), true),
        None if wrapped_pc_branch_error(&err) => {
            let mut bare = Assembler6502::new();
            let stripped = branchless_probe_text(asm);
            (
                bare.assemble_full(&stripped).ok().map(|(c, _)| c.len()),
                false,
            )
        }
        None => {

            return err;
        }
    };
    if let Some(code_len) = len {
        let end = origin as usize + code_len;
        if end > 0x1_0000 {
            let over = end - 0x1_0000;
            let message = if exact {
                format!(
                    "?OUT OF MEMORY: the program needs {code_len} bytes from ${origin:04X}, \
                     which is {over} bytes past the top of memory. Reduce data (smaller \
                     arrays, fewer string literals, less DATA)."
                )
            } else {
                format!(
                    "?OUT OF MEMORY: the program needs at least {code_len} bytes from \
                     ${origin:04X}, which is at least {over} bytes past the top of memory. \
                     Reduce data (smaller arrays, fewer string literals, less DATA)."
                )
            };
            return CompileError::Assemble {
                message,
                asm: Some(asm.clone()),
            };
        }
        // Only the exact probe may reach the reservation verdict.
        if exact && !reserved.is_empty() {
            return reservation_overflow_message(asm, original, reserved, code_len, end);
        }
    }

    if wrapped_pc_branch_error(&err) {
        let tail = if reserved.is_empty() {
            String::new()
        } else {
            let reserved_bytes: usize = reserved
                .iter()
                .map(|(s, e)| *e as usize - *s as usize + 1)
                .sum();
            format!(
                " {} reserved range(s) totalling {reserved_bytes} bytes are part of that \
                 total; --no-auto-reserve drops the automatic ones.",
                reserved.len(),
            )
        };
        return CompileError::Assemble {
            message: format!(
                "?OUT OF MEMORY: the program does not fit the 6502's 64 KB address space — \
                 laid out from ${origin:04X} it runs past $FFFF and wraps. Reduce data \
                 (smaller arrays, fewer string literals, less DATA).{tail}"
            ),
            asm: Some(asm.clone()),
        };
    }
    err
}

fn reservation_overflow_message(
    asm: &str,
    original: &str,
    reserved: &[(u16, u16)],
    code_len: usize,
    end: usize,
) -> CompileError {
    let reserved_bytes: usize = reserved
        .iter()
        .map(|(s, e)| *e as usize - *s as usize + 1)
        .sum();

    let original = original
        .strip_prefix("internal error: ")
        .unwrap_or(original);
    // A jump is 3 bytes; 16 is generous room for whatever padding asm6502 puts around a bridge, so the denial has margin.
    const BRIDGE_SLACK: usize = 16;
    let end_last = end.saturating_sub(1);
    let most_it_could_end_at = end + reserved_bytes + BRIDGE_SLACK * reserved.len();
    let message = if most_it_could_end_at <= 0x1_0000 {
        format!(
            "the program is {code_len} bytes and assembles cleanly with no reserved ranges \
             (ending at ${end_last:04X}), but fails with the {} reserved range(s) totalling \
             {reserved_bytes} bytes in effect. This is not an out-of-memory failure: \
             {reserved_bytes} reserved bytes cannot push an image ending at ${end_last:04X} \
             past $FFFF. The reservations are what the assembler could not satisfy. Reserved \
             ranges keep compiled code off memory the program POKEs into; drop or narrow them \
             with --reserved / --no-auto-reserve. The assembler reports: {original}",
            reserved.len(),
        )
    } else {
        format!(
            "?OUT OF MEMORY: the program is {code_len} bytes and fits on its own (ending at \
             ${end_last:04X}), but bridging over {} reserved range(s) totalling \
             {reserved_bytes} bytes can add enough to push it past the top of memory. \
             Reserved ranges keep compiled code off memory the program POKEs into; drop or \
             narrow them with --reserved / --no-auto-reserve, or reduce the program's data. \
             The assembler reports: {original}",
            reserved.len(),
        )
    };
    CompileError::Assemble {
        message,
        asm: Some(asm.to_string()),
    }
}

/// Raw asm6502 failures that can only be the compiler's fault, marked as such so [`CompileError`]'s `Display` says so and links the tracker.
fn label_asm6502_internal(message: &str) -> String {
    const COMPILER_ONLY: [&str; 8] = [
        "Undefined label:",
        "Long-branch fix didn't converge",
        "Unknown mnemonic:",
        "Immediate value too large:",
        "Unsupported mode for",
        "Invalid expression:",
        "Division by zero",
        "Invalid indirect addressing mode",
    ];
    if message.starts_with("internal error: ") {
        return message.to_string();
    }
    if COMPILER_ONLY.iter().any(|shape| message.contains(shape)) {
        return format!("internal error: {message}");
    }
    message.to_string()
}

/// Does this assembler failure prove the image wrapped the 6502's address space?
fn wrapped_pc_branch_error(err: &CompileError) -> bool {
    let CompileError::Assemble { message, .. } = err else {
        return false;
    };
    let Some(rest) = message.split("Branch offset out of range: ").nth(1) else {
        return false;
    };
    let digits: String = rest
        .chars()
        .take_while(|c| *c == '-' || c.is_ascii_digit())
        .collect();
    digits
        .parse::<i32>()
        .is_ok_and(|n| !(-32768..=32767).contains(&n))
}

fn branchless_probe_text(asm: &str) -> String {
    const BRANCHES: [&str; 8] = ["BCC", "BCS", "BEQ", "BMI", "BNE", "BPL", "BVC", "BVS"];
    let mut out = String::with_capacity(asm.len());
    for line in asm.lines() {
        let body = line.split(';').next().unwrap_or(line);
        let trimmed = body.trim();
        let is_branch = trimmed
            .split_once(char::is_whitespace)
            .is_some_and(|(m, rest)| {
                BRANCHES.contains(&m.to_ascii_uppercase().as_str()) && !rest.trim().is_empty()
            });
        if is_branch {
            let indent = &line[..line.len() - line.trim_start().len()];
            out.push_str(indent);
            out.push_str("LDA #$00");
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

fn assemble_with_aligned_vectors(
    mut asm: String,
    reserved: &[(u16, u16)],
) -> Result<Assembled, CompileError> {
    // One assembler item keeps a reservation bridge out of the indexed table.
    asm = coalesce_for_handler_table(&asm).map_err(|message| CompileError::Assemble {
        message, asm: Some(asm.clone()),
    })?;
    let mut pads = 0usize;
    let mut shims = 0usize;
    let mut fills = 0usize;
    let mut inline_args_undone = false;
    loop {
        let mut assembler = Assembler6502::new();
        // Reserved-region pass: feed the assembler regions of memory that it must NOT place
        // generated code or data into.
        for (start, end) in reserved {
            assembler
                .add_reserved_range(*start, *end)
                .map_err(|e| CompileError::Assemble {
                    message: format!("reserved range ${start:04X}-${end:04X}: {e}"),
                    asm: None,
                })?;
        }

        let (machine_code, items) =
            assembler
                .assemble_full(&asm)
                .map_err(|e| CompileError::Assemble {
                    message: label_asm6502_internal(&format!("{e}")),
                    asm: Some(asm.clone()),
                })?;

        if !inline_args_undone && inline_arg_split_by_bridge(&items) {
            asm = crate::peephole::undo_str_print_inline_args(&asm);
            inline_args_undone = true;
            continue;
        }

        match data_pool_clearance_fill(&asm, reserved, |n| assembler.lookup(n)) {
            Err(message) => {
                return Err(CompileError::Assemble {
                    message,
                    asm: Some(asm),
                });
            }
            Ok(Some(need)) => {

                if fills > reserved.len() + 2 {
                    return Err(CompileError::Assemble {
                        message: format!(
                            "internal error: after {fills} round(s) of clearance fill the \
                             layout fills the DATA pool at ${:04X} and it\n  \
                             still overlaps a reserved range. The pool has to be one \
                             unbroken run: READ walks it byte by byte with no relocation, \
                             so anything the assembler's bridge leaves inside it is handed \
                             back to the program as a DATA value.",
                            assembler.lookup("__DATA").unwrap_or(0),
                        ),
                        asm: Some(asm),
                    });
                }
                asm = insert_data_pool_clearance(&asm, need).ok_or_else(|| {
                    CompileError::Assemble {
                        message: "internal error: a reserved range falls inside the DATA \
                                  pool, but there is no `__DATA:` definition in the \
                                  generated asm to splice clearance fill in front of"
                            .to_string(),
                        asm: Some(asm.clone()),
                    }
                })?;
                fills += 1;
                continue;
            }
            Ok(None) => {}
        }

        if let Some(orphan) = bridge_orphans(&items, reserved)
            .into_iter()
            .filter_map(|o| o.resolve(|n| assembler.lookup(n)))
            .min_by_key(|o| o.addr)
        {

            if shims > reserved.len() + 2 {
                return Err(CompileError::Assemble {
                    message: orphan.internal_error_message(shims),
                    asm: Some(asm),
                });
            }
            asm = insert_bridge_shim(&asm, &orphan).ok_or_else(|| CompileError::Assemble {
                message: orphan.unfixable_message(),
                asm: Some(asm.clone()),
            })?;
            shims += 1;
            continue;
        }

        let mut vectors =
            resolve_indirect_vectors(&asm, |name| assembler.lookup(name)).map_err(|message| {
                CompileError::Assemble {
                    message,
                    asm: Some(asm.clone()),
                }
            })?;
        let Some((label, addr)) = vector_page_offenders(&vectors).into_iter().next() else {
            verify_for_handler_table(&asm, &machine_code, |n| assembler.lookup(n))
                .map_err(|message| CompileError::Assemble { message, asm: Some(asm.clone()) })?;
            vectors.sort_by_key(|(_, addr)| *addr);
            return Ok(Assembled {
                assembler,
                machine_code,
                asm,
                vectors,
                pads,
                shims,
                inline_args_undone,
            });
        };

        if pads > vectors.len() + 2 {
            return Err(CompileError::Assemble {
                message: format!(
                    "internal error: could not keep the indirect-JMP vector '{label}' off a \
                     page boundary (still at ${addr:04X}) after {pads} pad bytes"
                ),
                asm: Some(asm),
            });
        }
        asm = insert_vector_pad(&asm, &label).ok_or_else(|| CompileError::Assemble {
            message: format!(
                "internal error: the indirect-JMP vector '{label}' is linked at ${addr:04X}, \
                 the last byte of a page, where an NMOS 6502 `JMP ({label})` reads its high \
                 byte from ${:04X} instead of ${:04X} — and there is no `{label}:` definition \
                 to pad in front of",
                addr & 0xFF00,
                addr.wrapping_add(1),
            ),
            asm: Some(asm.clone()),
        })?;
        pads += 1;
    }
}

// I/O-area overlap

/// First byte of the C64 I/O block.
pub const IO_AREA_LO: u16 = 0xD000;
/// Last byte of the C64 I/O block.
pub const IO_AREA_HI: u16 = 0xDFFF;

/// The `[start, end]` slice of `$D000-$DFFF` that a loaded image covers, or `None` when it
/// clears the I/O block entirely.
fn bss_tail_keep_len(
    machine_code: &[u8],
    origin: u16,
    lookup: impl Fn(&str) -> Option<u16>,
) -> usize {
    let full = machine_code.len();
    let (Some(start), Some(end)) = (
        lookup(codegen::BSS_START_LABEL),
        lookup(codegen::BSS_END_LABEL),
    ) else {
        return full;
    };
    let (Some(from), Some(to)) = (
        (start as usize).checked_sub(origin as usize),
        (end as usize).checked_sub(origin as usize),
    ) else {
        return full;
    };
    if to != full || from > to {
        return full;
    }
    if machine_code[from..].iter().any(|b| *b != 0) {
        return full;
    }
    from
}

fn io_area_overlap(image_start: u16, image_len: usize) -> Option<(u16, u16)> {
    let last = (image_start as usize).checked_add(image_len.checked_sub(1)?)?;
    let lo = image_start.max(IO_AREA_LO);
    let hi = u16::try_from(last).unwrap_or(u16::MAX).min(IO_AREA_HI);
    (lo <= hi && last >= IO_AREA_LO as usize).then_some((lo, hi))
}

/// Reject a load image that overlaps the I/O area at $D000-$DFFF.
fn reject_io_area_overlap(compiled: &Compiled) -> Result<(), CompileError> {
    let start = compiled.diagnostics.start_address;

    let len = compiled.prg_bytes.len().saturating_sub(2) + compiled.diagnostics.bss_truncated;
    let Some((lo, hi)) = io_area_overlap(start, len) else {
        return Ok(());
    };
    let count = hi as u32 - lo as u32 + 1;
    let end = start as u32 + len as u32 - 1;
    let ceiling = u32::from(IO_AREA_LO - 1);
    let excess = end.saturating_sub(ceiling);
    io_area_overlap_debug(compiled, start, end, excess);
    Err(CompileError::Assemble {
        message: format!(
            "the compiled image spans ${start:04X}-${end:04X} and covers {count} bytes of \
             the I/O area ${lo:04X}-${hi:04X}. A KERNAL LOAD stores that range into the \
             VIC/SID/CIA registers instead of RAM, so those {count} bytes never reach \
             memory and the loaded program is corrupt — it would not run. Code cannot be \
             placed around the hole either, so the image has to end at or before \
             ${ceiling:04X}: remove about {excess} bytes (shorter DATA, smaller arrays, \
             fewer or shorter string literals).",
        ),
        asm: Some(compiled.asm.clone()),
    })
}

fn io_area_overlap_debug(compiled: &Compiled, start: u16, end: u32, excess: u32) {
    if std::env::var("YAB_DEBUG_IOHOLE").as_deref() != Ok("1") {
        return;
    }
    eprintln!("{}", io_area_overlap_debug_line(compiled, start, end, excess));
}

/// The line [`io_area_overlap_debug`] prints, as a value.
fn io_area_overlap_debug_line(compiled: &Compiled, start: u16, end: u32, excess: u32) -> String {
    let stored = compiled.prg_bytes.len().saturating_sub(2);
    let bss = compiled.diagnostics.bss_truncated;

    let stored_end = (start as u32 + stored as u32).saturating_sub(1);
    format!(
        "iohole: start=${start:04X} stored={stored} stored_end=${stored_end:04X} \
         bss_truncated={bss} runtime_end=${end:04X} excess={excess} \
         stored_excess={} extraram={}",
        stored_end.saturating_sub(u32::from(IO_AREA_LO - 1)),
        compiled.diagnostics.extraram,
    )
}

/// Collect indirect-JMP operands in source order, without duplicates.
fn indirect_jmp_operands(asm: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in asm.lines() {
        let line = raw.split(';').next().unwrap_or("").trim();
        if !line.get(..3).is_some_and(|m| m.eq_ignore_ascii_case("JMP")) {
            continue;
        }
        let Some(inner) = line[3..].trim_start().strip_prefix('(') else {
            continue;
        };
        let Some(end) = inner.find(')') else {
            continue;
        };
        let operand = inner[..end].trim().to_string();
        if !operand.is_empty() && !out.contains(&operand) {
            out.push(operand);
        }
    }
    out
}

/// Split a vector operand into its label and byte offset.
fn split_vector_operand(operand: &str) -> Option<(&str, i32)> {
    let cut = operand.find(['+', '-'])?;
    let (base, off) = operand.split_at(cut);
    let base = base.trim();
    let (sign, digits) = off.split_at(1);
    let digits = digits.trim();
    let magnitude = if let Some(hex) = digits.strip_prefix('$') {
        i32::from_str_radix(hex, 16).ok()?
    } else {
        digits.parse::<i32>().ok()?
    };
    let signed = if sign == "-" { -magnitude } else { magnitude };
    Some((base, signed))
}

/// Parse a literal address operand: `$XXXX` hex, or bare decimal.
fn parse_literal_operand(operand: &str) -> Option<u16> {
    if let Some(hex) = operand.strip_prefix('$') {
        u16::from_str_radix(hex, 16).ok()
    } else if operand.chars().all(|c| c.is_ascii_digit()) && !operand.is_empty() {
        operand.parse::<u16>().ok()
    } else {
        None
    }
}

/// Resolve indirect-JMP operands to their final addresses.
fn resolve_indirect_vectors(
    asm: &str,
    lookup: impl Fn(&str) -> Option<u16>,
) -> Result<Vec<(String, u16)>, String> {
    let mut out = Vec::new();
    for operand in indirect_jmp_operands(asm) {
        if let Some(addr) = parse_literal_operand(&operand) {
            out.push((operand, addr));
            continue;
        }
        let (base, offset) = split_vector_operand(&operand).unwrap_or((operand.as_str(), 0));
        let Some(base_addr) = lookup(base) else {
            return Err(format!(
                "internal error: cannot resolve the operand of `JMP ({operand})`, so the \
                 NMOS `JMP ($xxFF)` page-boundary check cannot be applied to it"
            ));
        };
        let addr = (i32::from(base_addr) + offset) as u16;
        out.push((operand, addr));
    }
    Ok(out)
}

/// The vectors from [`resolve_indirect_vectors`] that start on the last byte of a page, lowest
/// address first.
fn vector_page_offenders(vectors: &[(String, u16)]) -> Vec<(String, u16)> {
    let mut bad: Vec<(String, u16)> = vectors
        .iter()
        .filter(|(_, addr)| addr & 0x00FF == 0x00FF)
        .cloned()
        .collect();
    bad.sort_by_key(|(_, addr)| *addr);
    bad
}

/// Insert a byte ahead of the vector label to move it off the page boundary.
fn insert_vector_pad(asm: &str, label: &str) -> Option<String> {
    let (base, _) = split_vector_operand(label).unwrap_or((label, 0));
    let def = format!("{base}:\n");
    // Anchor on a line start so `__FOO:` doesn't match inside `__BAR__FOO:`.
    let at = if asm.starts_with(&def) {
        0
    } else {
        asm.find(&format!("\n{def}"))? + 1
    };
    let mut out = String::with_capacity(asm.len() + 96);
    out.push_str(&asm[..at]);
    out.push_str(
        "; pad: an NMOS 6502 `JMP ($xxFF)` reads its high byte from $xx00, so this\n\
         ; vector must not start on the last byte of a page.\n    .byte $00\n",
    );
    out.push_str(&asm[at..]);
    Some(out)
}

const ZP_X_MNEMONICS: &[&str] = &[
    "ADC", "AND", "ASL", "CMP", "DEC", "EOR", "INC", "LDA", "LDY", "LSR", "ORA", "ROL", "ROR",
    "SBC", "STA", "STY",
];

/// Mnemonics with a `zeropage,Y` encoding.
const ZP_Y_MNEMONICS: &[&str] = &["LDX", "STX"];

/// Mnemonics with a plain `zeropage` encoding.
const ZP_MNEMONICS: &[&str] = &[
    "ADC", "AND", "ASL", "BIT", "CMP", "CPX", "CPY", "DEC", "EOR", "INC", "LDA", "LDX", "LDY",
    "LSR", "ORA", "ROL", "ROR", "SBC", "STA", "STX", "STY",
];

/// Mnemonics that take `(zp),Y` / `(zp,X)`.
const INDIRECT_MNEMONICS: &[&str] = &["ADC", "AND", "CMP", "EOR", "LDA", "ORA", "SBC", "STA"];

/// One instruction line of the finished assembly.
struct AsmInsn<'a> {
    line: usize,
    mnemonic: &'a str,
    /// Operand text with the comment and surrounding space removed; empty for
    /// implied/accumulator instructions.
    operand: &'a str,
}

/// The instruction lines of `asm`, in order.
fn asm_instructions(asm: &str) -> Vec<AsmInsn<'_>> {
    let mut out = Vec::new();
    for (i, raw) in asm.lines().enumerate() {
        let code = raw.split(';').next().unwrap_or("");
        if !code.starts_with(char::is_whitespace) {
            continue;
        }
        let trimmed = code.trim();
        let Some(mnemonic) = trimmed.get(..3) else {
            continue;
        };
        if !mnemonic.bytes().all(|c| c.is_ascii_uppercase()) {
            continue;
        }
        let rest = trimmed[3..].trim();
        if !trimmed[3..].is_empty() && !trimmed[3..].starts_with(char::is_whitespace) {
            continue;
        }
        out.push(AsmInsn {
            line: i + 1,
            mnemonic,
            operand: rest,
        });
    }
    out
}

/// Resolve a literal address, label or label with an offset.
fn resolve_addr_operand(operand: &str, lookup: &impl Fn(&str) -> Option<u16>) -> Option<u16> {
    if let Some(v) = parse_literal_operand(operand) {
        return Some(v);
    }
    if let Some((base, off)) = split_vector_operand(operand) {
        let base = base.trim();
        let base_addr = parse_literal_operand(base).or_else(|| lookup(base))?;
        return Some((i32::from(base_addr) + off) as u16);
    }
    lookup(operand)
}

/// Split `"$1234,X"` into `("$1234", 'X')`.
fn split_indexed_operand(operand: &str) -> Option<(&str, char)> {
    if operand.starts_with('(') {
        return None;
    }
    let (base, idx) = operand.rsplit_once(',')?;
    let idx = idx.trim();
    let reg = match idx {
        "X" | "x" => 'X',
        "Y" | "y" => 'Y',
        _ => return None,
    };
    Some((base.trim(), reg))
}

/// Split `"($FB),Y"` into `("$FB", true)` and `"($FB,X)"` into `("$FB", false)`; the flag says
/// whether it is the `,Y` form.
fn split_indirect_operand(operand: &str) -> Option<(&str, bool)> {
    let inner = operand.strip_prefix('(')?;
    if let Some(pos) = inner.find("),") {
        let idx = inner[pos + 2..].trim();
        if idx.eq_ignore_ascii_case("Y") {
            return Some((inner[..pos].trim(), true));
        }
        return None;
    }
    let inner = inner.strip_suffix(')')?;
    let (base, idx) = inner.rsplit_once(',')?;
    idx.trim()
        .eq_ignore_ascii_case("X")
        .then(|| (base.trim(), false))
}

fn verify_no_narrowed_index(
    asm: &str,
    lookup: &impl Fn(&str) -> Option<u16>,
) -> Result<(), String> {
    for insn in asm_instructions(asm) {
        if insn.operand.starts_with('>') {
            continue;
        }
        let Some((base, reg)) = split_indexed_operand(insn.operand) else {
            continue;
        };
        let table = if reg == 'X' {
            ZP_X_MNEMONICS
        } else {
            ZP_Y_MNEMONICS
        };
        if !table.contains(&insn.mnemonic) {
            continue;
        }
        let Some(addr) = resolve_addr_operand(base, lookup) else {
            continue;
        };
        if addr == 0 || addr >= 0x100 {
            continue;
        }
        let wraps_at = 0x100 - addr;
        return Err(format!(
            "internal error: line {} emits `{} {}`, whose base resolves to ${addr:04X}. \
             asm6502 picks the addressing mode from the operand's value, not from the width \
             it was written at, so this assembles as `zeropage,{reg}` and computes \
             `(${addr:02X} + {reg}) & $FF`. For {reg} >= {wraps_at} that reads $0000-${:04X} \
             where the absolute form reads $0100-${:04X}. Only a base of $0000 or one at \
             $0100 and above is wrap-free for an unrestricted byte index; this emit site \
             has to decline the indexed form and use a self-modifying operand instead.",
            insn.line,
            insn.mnemonic,
            insn.operand,
            addr - 1,
            addr.wrapping_add(0xFF),
        ));
    }
    Ok(())
}

/// Require indirect pointers to occupy a complete zero-page pair.
fn verify_indirect_zp_pointers(
    asm: &str,
    lookup: &impl Fn(&str) -> Option<u16>,
) -> Result<(), String> {
    for insn in asm_instructions(asm) {
        if !INDIRECT_MNEMONICS.contains(&insn.mnemonic) {
            continue;
        }
        let Some((base, is_y)) = split_indirect_operand(insn.operand) else {
            continue;
        };
        let form = if is_y { "(zp),Y" } else { "(zp,X)" };
        let Some(addr) = resolve_addr_operand(base, lookup) else {
            return Err(format!(
                "internal error: line {} emits `{} {}` and the operand `{base}` cannot be \
                 resolved, so the {form} zero-page requirement cannot be checked against it",
                insn.line, insn.mnemonic, insn.operand,
            ));
        };
        if addr >= 0x100 {
            return Err(format!(
                "internal error: line {} emits `{} {}`, but `{base}` is linked at \
                 ${addr:04X} — outside zero page. A {form} pointer is a single byte: \
                 asm6502 truncates the operand to ${:02X} without complaint, so the \
                 instruction would read its pointer from ${:04X} instead of ${addr:04X}.",
                insn.line,
                insn.mnemonic,
                insn.operand,
                addr & 0xFF,
                addr & 0xFF,
            ));
        }
        if addr == 0xFF {
            return Err(format!(
                "internal error: line {} emits `{} {}` with `{base}` at $00FF, the last \
                 byte of zero page. The 6502 wraps the second pointer fetch inside page \
                 zero, so it reads the high byte from $0000 rather than $0100 — the pair \
                 codegen laid out is not the pair the CPU reads.",
                insn.line, insn.mnemonic, insn.operand,
            ));
        }
    }
    Ok(())
}

fn encoded_len(insn: &AsmInsn<'_>, lookup: &impl Fn(&str) -> Option<u16>) -> Option<usize> {
    if insn.operand.is_empty() {
        return Some(1);
    }
    if insn.operand.starts_with('#') {
        return Some(2);
    }
    if matches!(
        insn.mnemonic,
        "BCC" | "BCS" | "BEQ" | "BMI" | "BNE" | "BPL" | "BVC" | "BVS"
    ) {
        return Some(2);
    }
    if insn.mnemonic == "JMP" || insn.mnemonic == "JSR" {
        return Some(3);
    }
    if insn.operand.starts_with('(') {
        return Some(2);
    }

    if insn.operand.starts_with('>') {
        return Some(3);
    }
    if insn.operand.starts_with('<') {
        return Some(2);
    }
    let (base, table) = match split_indexed_operand(insn.operand) {
        Some((base, 'X')) => (base, ZP_X_MNEMONICS),
        Some((base, _)) => (base, ZP_Y_MNEMONICS),
        None => (insn.operand, ZP_MNEMONICS),
    };
    let addr = resolve_addr_operand(base, lookup)?;
    Some(if addr < 0x100 && table.contains(&insn.mnemonic) {
        2
    } else {
        3
    })
}

/// Require self-modifying offsets to stay inside the target instruction.
fn verify_smc_patch_offsets(
    asm: &str,
    lookup: &impl Fn(&str) -> Option<u16>,
) -> Result<(), String> {
    // label -> the instruction it names, for labels immediately followed by one.
    let mut named: std::collections::HashMap<&str, AsmInsn<'_>> = std::collections::HashMap::new();
    let mut pending: Vec<&str> = Vec::new();
    for (i, raw) in asm.lines().enumerate() {
        let code = raw.split(';').next().unwrap_or("");
        let trimmed = code.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !code.starts_with(char::is_whitespace) {
            if let Some(name) = trimmed.strip_suffix(':') {
                pending.push(name);
            } else {
                // An equate or a directive at column 0 ends the run without naming an
                // instruction.
                pending.clear();
            }
            continue;
        }
        if pending.is_empty() {
            continue;
        }
        let Some(mnemonic) = trimmed.get(..3).filter(|m| {
            m.bytes().all(|c| c.is_ascii_uppercase())
                && (trimmed[3..].is_empty() || trimmed[3..].starts_with(char::is_whitespace))
        }) else {

            pending.clear();
            continue;
        };
        for name in pending.drain(..) {
            named.insert(
                name,
                AsmInsn {
                    line: i + 1,
                    mnemonic,
                    operand: trimmed[3..].trim(),
                },
            );
        }
    }

    for insn in asm_instructions(asm) {

        let core = insn
            .operand
            .trim_start_matches(['#', '<', '>'])
            .split(',')
            .next()
            .unwrap_or("")
            .trim();
        let Some((base, off)) = split_vector_operand(core) else {
            continue;
        };
        if off <= 0 {
            continue;
        }
        let Some(target) = named.get(base.trim()) else {
            continue;
        };
        let Some(len) = encoded_len(target, lookup) else {
            continue;
        };
        if (off as usize) < len {
            continue;
        }
        return Err(format!(
            "internal error: line {} references `{core}`, but `{}` names the {len}-byte \
             instruction `{} {}` on line {}. Offset +{off} is past its last byte, so a \
             store through it lands on the instruction that follows instead of on this \
             one's operand. A self-modifying template patched at +1 and +2 has to be a \
             three-byte absolute instruction; this one narrowed to zero page.",
            insn.line,
            base.trim(),
            target.mnemonic,
            target.operand,
            target.line,
        ));
    }
    Ok(())
}

/// Run all three addressing-mode guards over the finished assembly.
fn verify_addressing_modes(asm: &str, lookup: impl Fn(&str) -> Option<u16>) -> Result<(), String> {
    verify_no_narrowed_index(asm, &lookup)?;
    verify_indirect_zp_pointers(asm, &lookup)?;
    verify_smc_patch_offsets(asm, &lookup)?;
    Ok(())
}

pub fn verify_addressing_modes_for_test(
    asm: &str,
    lookup: impl Fn(&str) -> Option<u16>,
) -> Result<(), String> {
    verify_addressing_modes(asm, lookup)
}

/// Decode leading literal data operands, up to the requested byte limit.
fn leading_data_literals(operands: &str, word: bool, max: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for tok in operands.split(',') {
        if out.len() >= max {
            break;
        }
        let tok = tok.trim();
        let Some(hex) = tok.strip_prefix('$') else {
            break;
        };
        let width = if word { 4 } else { 2 };
        match u16::from_str_radix(hex, 16) {
            Ok(v) if hex.len() <= width => {
                out.push((v & 0xFF) as u8);
                if word {
                    out.push((v >> 8) as u8);
                }
            }
            _ => break,
        }
    }
    out.truncate(max);
    out
}

/// Bytes as a 6502 programmer writes them: `$8D $14 $03`.
fn hex_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("${b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Post-condition: every `LABEL:` in the asm that is immediately followed by a `.byte`/`.word`
/// run of literals must be linked at exactly those bytes in the finished image.
fn verify_label_bytes(
    asm: &str,
    machine_code: &[u8],
    origin: u16,
    lookup: impl Fn(&str) -> Option<u16>,
) -> Result<(), String> {
    // How many bytes to compare.
    const WINDOW: usize = 4;
    let mut pending: Vec<&str> = Vec::new();
    for raw in asm.lines() {
        let line = raw.split(';').next().unwrap_or("").trim_end();
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some(name) = trimmed.strip_suffix(':') {
            if !name.is_empty() && !name.contains(char::is_whitespace) {
                pending.push(name);
                continue;
            }
        }
        let head = trimmed.get(..5).unwrap_or_default();
        let word = head.eq_ignore_ascii_case(".word");
        if !word && !head.eq_ignore_ascii_case(".byte") {
            pending.clear();
            continue;
        }
        let want = leading_data_literals(trimmed[5..].trim(), word, WINDOW);
        if want.is_empty() {
            pending.clear();
            continue;
        }
        for name in pending.drain(..) {
            let Some(addr) = lookup(name) else { continue };
            let Some(off) = usize::from(addr).checked_sub(usize::from(origin)) else {
                continue;
            };
            let Some(got) = machine_code.get(off..off + want.len()) else {
                continue;
            };
            if got != want.as_slice() {
                return Err(format!(
                    "internal error: `{name}:` is linked at ${addr:04X}, but the image \
                     holds {} there instead of the {} the label names. \
                     The label was separated from its own bytes during layout — code \
                     that takes its address (`LDA #<{name}`) reads the wrong memory.",
                    hex_bytes(got),
                    hex_bytes(&want),
                ));
            }
        }
    }
    Ok(())
}

/// A run of labels the assembler stranded on a reserved-range bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BridgeOrphan {
    /// Labels stranded on the bridge, in emission order.
    labels: Vec<String>,
    /// First byte of the reserved range the bridge skips over.
    r_start: u16,
    /// Last byte of that range; the bridge `JMP` targets `r_end + 1`.
    r_end: u16,
}

/// A [`BridgeOrphan`] with the address the assembler linked it at, and with names that cannot
/// be repaired filtered out.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedOrphan {
    labels: Vec<String>,
    r_start: u16,
    r_end: u16,
    /// Linked address of the label displaced by a reserved-range bridge.
    addr: u16,
}

fn is_assembler_generated_label(name: &str) -> bool {
    name.starts_with("__skip_")
}

/// Labels that mark the end of the run in front of them instead of naming the bytes behind them.
fn is_end_marker_label(name: &str) -> bool {
    matches!(
        name,
        "__DATA_END" | "__DATA_HOISTED_END" | "__BSS_END" | "__CP_END"
            | crate::extraram::DATA_END_LABEL
    )
}

impl BridgeOrphan {
    /// Resolve the stranded run against the assembler's symbol table, dropping names that need
    /// no repair.
    fn resolve(self, lookup: impl Fn(&str) -> Option<u16>) -> Option<ResolvedOrphan> {
        let mut labels: Vec<String> = self
            .labels
            .into_iter()
            .filter(|n| !is_assembler_generated_label(n))
            .collect();
        // End markers only step aside when something else in the run can take the filler.
        if labels.iter().any(|n| !is_end_marker_label(n)) {
            labels.retain(|n| !is_end_marker_label(n));
        }
        let addr = labels.iter().find_map(|n| lookup(n))?;
        Some(ResolvedOrphan {
            labels,
            r_start: self.r_start,
            r_end: self.r_end,
            addr,
        })
    }
}

impl ResolvedOrphan {
    fn names(&self) -> String {
        self.labels.join(", ")
    }

    fn unfixable_message(&self) -> String {
        format!(
            "internal error: the label(s) {} resolve to ${:04X}, which is the \
             `JMP ${:04X}` the assembler inserted to skip reserved range \
             ${:04X}-${:04X} — their actual bytes start at ${:04X}. Any code that \
             takes the address of one of these reads the bridge instead of its \
             data. No `<label>:` definition was found in the generated asm to \
             splice filler in front of, so the layout cannot be repaired.",
            self.names(),
            self.addr,
            self.r_end.wrapping_add(1),
            self.r_start,
            self.r_end,
            self.r_end.wrapping_add(1),
        )
    }

    fn internal_error_message(&self, shims: usize) -> String {
        format!(
            "internal error: after {shims} layout shims the label(s) {} are still \
             stranded at ${:04X} on the bridge over reserved range ${:04X}-${:04X}",
            self.names(),
            self.addr,
            self.r_start,
            self.r_end,
        )
    }
}

/// Every label run that a reserved-range bridge separated from the item it names.
fn bridge_orphans(items: &[Item], reserved: &[(u16, u16)]) -> Vec<BridgeOrphan> {
    let mut out = Vec::new();
    for (i, item) in items.iter().enumerate() {
        if !matches!(item, Item::Pad(_)) {
            continue;
        }
        let Some(Item::Instruction {
            mnemonic,
            operand: Some(op),
        }) = items.get(i + 1)
        else {
            continue;
        };
        if !mnemonic.eq_ignore_ascii_case("JMP") {
            continue;
        }
        if !matches!(items.get(i + 2), Some(Item::Pad(_))) {
            continue;
        }
        let Some(target) = op
            .strip_prefix('$')
            .and_then(|h| u16::from_str_radix(h, 16).ok())
        else {
            continue;
        };
        let Some(&(r_start, r_end)) = reserved
            .iter()
            .find(|(_, end)| u32::from(*end) + 1 == u32::from(target))
        else {
            continue;
        };
        let mut labels: Vec<String> = Vec::new();
        let mut j = i;
        while j > 0 {
            match &items[j - 1] {
                Item::Label(name) => labels.push(name.clone()),

                Item::Constant(..) => {}
                _ => break,
            }
            j -= 1;
        }
        if labels.is_empty() {
            continue;
        }
        labels.reverse();
        out.push(BridgeOrphan {
            labels,
            r_start,
            r_end,
        });
    }
    out
}

/// True when the assembler put anything between a `JSR __STR_PRINT_I` and the `.word` that is supposed to be its argument.
fn inline_arg_split_by_bridge(items: &[Item]) -> bool {
    items.iter().enumerate().any(|(i, item)| {
        let Item::Instruction {
            mnemonic,
            operand: Some(op),
        } = item
        else {
            return false;
        };
        mnemonic.eq_ignore_ascii_case("JSR")
            && op.trim() == crate::peephole::STR_PRINT_INLINE_ENTRY
            && !matches!(items.get(i + 1), Some(Item::Words(_)))
    })
}

// FOR dispatch reads base+2*id, so all entries must be one indivisible assembler item.
fn for_handler_table(asm: &str) -> Result<Option<(std::ops::Range<usize>, Vec<&str>)>, String> {
    let mut offset = 0;
    let mut start = None;
    let mut end = 0;
    let mut entries = Vec::new();
    let mut inside = false;
    for line in asm.split_inclusive('\n') {
        if inside && line.trim() == "; layout: end of FOR handler table" {
            end = offset + line.len();
            inside = false;
        }
        let text = line.split(';').next().unwrap_or("").trim();
        if text == "__FOR_HANDLER_TABLE:" {
            if start.is_some() {
                return Err("internal error: duplicate FOR handler table".into());
            }
            start = Some(offset + line.len());
            end = offset + line.len();
            inside = true;
        } else if inside && !text.is_empty() {
            if let Some(words) = text.strip_prefix(".word ") {
                for word in words.split(',').map(str::trim) {
                    if word.is_empty() || !word.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'$') {
                        return Err("internal error: unsupported FOR handler table entry".into());
                    }
                    entries.push(word);
                }
                end = offset + line.len();
            } else if text.ends_with(':') {
                inside = false;
            } else {
                return Err("internal error: non-word content in FOR handler table".into());
            }
        }
        offset += line.len();
    }
    let Some(start) = start else { return Ok(None) };
    if entries.is_empty() {
        return Err("internal error: empty FOR handler table".into());
    }
    Ok(Some((start..end, entries)))
}

fn coalesce_for_handler_table(asm: &str) -> Result<String, String> {
    let Some((span, entries)) = for_handler_table(asm)? else { return Ok(asm.into()) };
    let mut out = String::with_capacity(asm.len());
    out.push_str(&asm[..span.start]);
    out.push_str("    .word ");
    out.push_str(&entries.join(","));
    out.push_str("\n; layout: end of FOR handler table\n");
    out.push_str(&asm[span.end..]);
    Ok(out)
}

fn verify_for_handler_table<F>(asm: &str, image: &[u8], lookup: F) -> Result<(), String>
where F: Fn(&str) -> Option<u16> {
    let Some((_, entries)) = for_handler_table(asm)? else { return Ok(()) };
    let origin = asm.lines().find_map(|line| line.trim().strip_prefix("*=$"))
        .and_then(|hex| u16::from_str_radix(hex.trim(), 16).ok())
        .ok_or("internal error: FOR handler table image has no literal origin")?;
    let start = lookup("__FOR_HANDLER_TABLE").ok_or("internal error: unlinked FOR handler table")?;
    let mut expected = Vec::with_capacity(entries.len() * 2);
    for entry in entries {
        let value = if let Some(hex) = entry.strip_prefix('$') {
            u16::from_str_radix(hex, 16).ok()
        } else {
            entry.parse::<u16>().ok().or_else(|| lookup(entry))
        }.ok_or_else(|| format!("internal error: unresolved FOR handler table entry {entry}"))?;
        expected.extend_from_slice(&value.to_le_bytes());
    }
    let offset = start.checked_sub(origin).map(usize::from);
    let actual = offset.and_then(|offset| image.get(offset..offset + expected.len()));
    if actual != Some(expected.as_slice()) {
        return Err(format!("internal error: FOR handler table at ${start:04X} is not contiguous with its resolved entries"));
    }
    Ok(())
}

/// Splice filler in front of a stranded label so the assembler bridges the filler instead of the label's own item.
fn insert_bridge_shim(asm: &str, orphan: &ResolvedOrphan) -> Option<String> {

    const BRIDGE_FILL_BYTE: &str = "$EA";

    let (label, at) = orphan.labels.iter().find_map(|name| {
        let def = format!("{name}:\n");
        let at = if asm.starts_with(&def) {
            0
        } else {
            // Anchor on a line start so `__FOO:` doesn't match inside `__BAR__FOO:`.
            asm.find(&format!("\n{def}"))? + 1
        };
        Some((name.clone(), at))
    })?;

    let addr = u32::from(orphan.addr);
    let r_start = u32::from(orphan.r_start);
    // The assembler only places a bridge where its 3-byte `JMP` fits, so a stranded label is
    // always at `r_start - 3` or below.
    if addr + 3 > r_start {
        return None;
    }
    // Split long fills across `.byte` lines so the asm stays readable.
    fn emit_fill(out: &mut String, mut count: u32) {
        const PER_LINE: u32 = 32;
        while count > 0 {
            let take = count.min(PER_LINE);
            out.push_str("    .byte ");
            out.push_str(&vec![BRIDGE_FILL_BYTE; take as usize].join(","));
            out.push('\n');
            count -= take;
        }
    }

    let mut filler = String::new();
    if addr + 6 <= r_start {
        filler.push_str(&format!("    JMP {label}\n"));
        emit_fill(&mut filler, (r_start - 3) - (addr + 3));
        filler.push_str(&format!("    .byte {BRIDGE_FILL_BYTE}\n"));
    } else {
        emit_fill(&mut filler, (r_start - 2).saturating_sub(addr).max(1));
    }

    let mut out = String::with_capacity(asm.len() + filler.len() + 256);
    out.push_str(&asm[..at]);
    out.push_str(&format!(
        "; layout: the assembler skips reserved ${:04X}-${:04X} with a `JMP ${:04X}`\n\
         ; placed at the first item that would cross it. Without this filler that\n\
         ; bridge lands between `{label}:` and the bytes it names, and the label\n\
         ; resolves to the JMP. The filler is what gets bridged instead.\n",
        orphan.r_start,
        orphan.r_end,
        orphan.r_end.wrapping_add(1),
    ));
    out.push_str(&filler);
    out.push_str(&asm[at..]);
    Some(out)
}

/// A final failure-only layout alternative: move the entire READ pool before the string constants.
const DATA_FIRST_MARKER: &str = "; layout: whole DATA pool before string constants (last rescue).";
const VAL_HIGH_HEAP_MARKER: &str = "; layout: VAL heap above BASIC ROM.";

fn pad_terminal_heap(asm: &str, bytes: usize) -> Option<String> {
    let marker = "\n__HEAP_BOTTOM:\n";
    let at = asm.find(marker)? + 1;
    if asm.lines().filter(|line| *line == "__HEAP_BOTTOM:").count() != 1
        || asm[at + "__HEAP_BOTTOM:\n".len()..].lines().any(|line| !line.trim().is_empty() && !line.trim().starts_with(';'))
    {
        return None;
    }
    let mut out = String::with_capacity(asm.len() + bytes * 4 + 128);
    out.push_str(&asm[..at]);
    out.push_str("; Linked fill keeps VAL's upward heap in ROM-visible RAM.\n");
    for chunk in (0..bytes).step_by(16) {
        out.push_str("    .byte ");
        out.push_str(&vec!["$00"; (bytes - chunk).min(16)].join(","));
        out.push('\n');
    }
    out.push_str(&asm[at..]);
    Some(out)
}

fn data_before_constants(asm: &str) -> Option<String> {
    let constants = asm.find("; --- string pool (length-prefixed) ---\n")?;
    let start = asm.find("\n__DATA:\n")? + 1;
    if constants >= start || asm.lines().filter(|line| *line == "__DATA:").count() != 1 {
        return None;
    }
    let close = format!("\n{}\n", crate::codegen::DATA_POOL_CLOSE);
    let end = start + asm[start..].find(&close)? + close.len();
    if asm.lines().filter(|line| *line == crate::codegen::DATA_POOL_CLOSE).count() != 1 {
        return None;
    }
    let mut has_bytes = false;
    for line in asm[start..end].lines() {
        let line = line.split(';').next().unwrap_or("").trim();
        if line.is_empty() || line == "__DATA:" || line == crate::codegen::DATA_POOL_CLOSE {
            continue;
        }
        if let Some(label) = line.strip_suffix(':') {
            if label.strip_prefix("__DATA_LINE_").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                || label.strip_prefix("__DBAD_").is_some_and(|n| {
                    let n = n.strip_prefix('S').or_else(|| n.strip_prefix('E'));
                    n.is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                })
            {
                continue;
            }
            return None;
        }
        let bytes = line.strip_prefix(".byte ")?;
        if !bytes.split(',').all(|b| b.trim().strip_prefix('$').is_some_and(|n| {
            n.len() == 2 && n.bytes().all(|b| b.is_ascii_hexdigit())
        })) {
            return None;
        }
        has_bytes = true;
    }
    if !has_bytes {
        return None;
    }
    let mut out = String::with_capacity(asm.len() + 80);
    out.push_str(&asm[..constants]);
    out.push_str(DATA_FIRST_MARKER);
    out.push('\n');
    out.push_str(&asm[start..end]);
    out.push_str(&asm[constants..start]);
    out.push_str(&asm[end..]);
    Some(out)
}

fn relocate_data_past_reservations(asm: &str, merged: &[(u16, u16)]) -> (String, Vec<(u16, u16)>) {
    if merged.is_empty() {
        return (asm.to_string(), Vec::new());
    }

    // Find the DATA pool: from the section comment (or the bare
    // label) up to the next blank-line + new section comment, or up
    // to the next top-level label that isn't a `.byte` continuation.
    // The section comment names the pool layout, so anchor on the
    // label and pick the comment up only when it sits directly above.
    let header = "__DATA:\n";
    let Some(label_at) = asm.find(&format!("\n{header}")).map(|p| p + 1) else {
        return (asm.to_string(), Vec::new());
    };
    let start = asm[..label_at]
        .rfind("\n; --- DATA pool")
        .map(|p| p + 1)
        .filter(|&p| asm[p..label_at].lines().count() == 1)
        .unwrap_or(label_at);

    let body_start = label_at + header.len();
    let mut end = body_start;
    for line in asm[body_start..].split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.trim_end() == crate::codegen::DATA_POOL_CLOSE {
            end += line.len();
            break;
        }
        if trimmed.is_empty() || trimmed.starts_with("; ---") {
            break;
        }

        let is_pool_label =
            trimmed.starts_with("__DATA_LINE_") || trimmed.starts_with("__DBAD_");
        if !trimmed.starts_with(".byte") && !trimmed.starts_with(';') && !is_pool_label {
            break;
        }
        end += line.len();
    }
    let data_block = &asm[start..end];

    // Splice the hoist site: right after the `*=$080D` line and any
    // ZP-equ lines. asm6502 doesn't emit bytes for the ZP equates,
    // so the JMP we insert is the very first byte at $080D.
    let origin = "*=$080D\n";
    let Some(origin_end) = asm.find(origin).map(|p| p + origin.len()) else {
        return (asm.to_string(), Vec::new());
    };
    // Skip blank lines + ZP equate definitions (they don't emit bytes).
    let mut hoist_at = match asm.find("__BT_INPLACE_START:\n") {
        Some(p) => p + "__BT_INPLACE_START:\n".len(),
        None => origin_end,
    };
    for line in asm[origin_end..].split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with(';') || trimmed.contains(" = $") {
            hoist_at += line.len();
            continue;
        }
        break;
    }

    let is_d4 = asm.contains("__BT_INPLACE_START:\n");
    let (fill_len, absorbed) = if is_d4 {
        (0usize, Vec::new())
    } else {
        plan_data_pool_fill(data_block, merged)
    };

    let head = &asm[..hoist_at];
    let middle = &asm[hoist_at..start];
    let tail = &asm[end..];
    let mut out = String::with_capacity(asm.len() + 64 + fill_len * 6);
    out.push_str(head);
    out.push_str("    JMP __DATA_HOISTED_END\n");
    if fill_len > 0 {
        out.push_str("; --- reserved-range fill, see relocate_data_past_reservations\n");
        out.push_str("__DATA_POOL_FILL:\n");
        // Sixteen bytes a line keeps the assembly readable and the parse cheap; the assembler concatenates them into one run.
        let mut left = fill_len;
        while left > 0 {
            let take = left.min(16);
            out.push_str("    .byte ");
            for i in 0..take {
                if i > 0 {
                    out.push(',');
                }
                out.push_str("$00");
            }
            out.push('\n');
            left -= take;
        }
    }
    out.push_str(data_block);
    out.push_str("__DATA_HOISTED_END:\n");
    out.push_str(middle);
    out.push_str(tail);
    (out, absorbed)
}

/// Bytes of `$00` to put in front of the hoisted pool, and the reservations that fill then covers end to end.
fn plan_data_pool_fill(data_block: &str, merged: &[(u16, u16)]) -> (usize, Vec<(u16, u16)>) {
    /// `$080D` origin + `JMP __DATA_HOISTED_END`.
    const NATURAL_POOL_START: u32 = CODE_ORIGIN + 3;

    const POOL_CEILING: u32 = 0x9F00;

    let pool_len = data_pool_byte_len(data_block) as u32;
    if pool_len == 0 {
        return (0, Vec::new());
    }
    let mut start = NATURAL_POOL_START;

    for _ in 0..=merged.len() {
        let last = start + pool_len - 1;
        let Some(&(_, hi)) = merged
            .iter()
            .find(|&&(lo, hi)| u32::from(lo) <= last && u32::from(hi) >= start)
        else {
            break;
        };
        start = u32::from(hi) + 1;
    }
    if start == NATURAL_POOL_START || start + pool_len > POOL_CEILING {
        return (0, Vec::new());
    }

    let absorbed: Vec<(u16, u16)> = merged
        .iter()
        .copied()
        .filter(|&(lo, hi)| u32::from(lo) >= NATURAL_POOL_START && u32::from(hi) < start)
        .collect();
    ((start - NATURAL_POOL_START) as usize, absorbed)
}

/// Count the bytes a hoisted `__DATA:` block assembles to: every operand of every `.byte` line in it.
fn data_pool_byte_len(data_block: &str) -> usize {
    data_block
        .lines()
        .filter_map(|l| l.trim_start().strip_prefix(".byte"))
        .map(|args| {
            args.split(';')
                .next()
                .unwrap_or("")
                .split(',')
                .filter(|f| !f.trim().is_empty())
                .count()
        })
        .sum()
}

fn verify_absorbed_ranges_filled<F>(absorbed: &[(u16, u16)], lookup: F) -> Result<(), String>
where
    F: Fn(&str) -> Option<u16>,
{
    if absorbed.is_empty() {
        return Ok(());
    }
    let (Some(fill), Some(pool_end)) = (lookup("__DATA_POOL_FILL"), lookup("__DATA_HOISTED_END"))
    else {
        return Err(format!(
            "internal error: {} reserved range(s) were withheld from the assembler on the \
             promise that the DATA-pool fill covers them, but the fill labels are not in \
             the image",
            absorbed.len(),
        ));
    };
    for &(lo, hi) in absorbed {
        if lo < fill || hi >= pool_end {
            return Err(format!(
                "internal error: reserved range ${lo:04X}-${hi:04X} was withheld from the \
                 assembler because the DATA-pool fill was supposed to cover it, but the fill \
                 plus pool only spans ${fill:04X}-${:04X}. Those bytes would hold live code \
                 inside a range the program writes at run time.",
                pool_end.saturating_sub(1),
            ));
        }
    }
    Ok(())
}

/// Lower bound of the auto-reserve scan window. Below `$0900` lives
/// zero page, the stack, KERNAL screen / colour RAM ($0400-$07FF),
/// and the very start of our own compiled-code segment ($0801 PRG
/// header, $080D code-origin) — the assembler can't place code
/// elsewhere, so a "reservation" that overlaps any of these bytes
/// can never be honoured. Honouring it would force the bridge JMP
/// into the SYS stub or fail outright; ignoring it lets the rest of
/// the program compile, with the understanding that any runtime
/// POKE into that area will trample our code. We pick `$0900` so
/// there's a full page of headroom past the code origin for the
/// JMP-bridge logic and a handful of helper bytes.
const AUTO_RESERVE_LO: u16 = 0x0900;
/// Upper bound: above `$D000` we're in BASIC ROM / I/O / KERNAL ROM,
/// outside the addressable code window. `$CFFF` is the last byte
/// before the I/O area at `$D000-$DFFF`.
const AUTO_RESERVE_HI: u16 = 0xCFFF;

fn verify_data_pool_unsplit<F>(reserved: &[(u16, u16)], lookup: F) -> Result<(), String>
where
    F: Fn(&str) -> Option<u16>,
{
    let (Some(start), Some(end)) = (lookup("__DATA"), lookup("__DATA_HOISTED_END")) else {
        return Ok(());
    };
    for (lo, hi) in reserved {
        if *lo < end && *hi >= start {
            return Err(format!(
                "reserved range ${lo:04X}-${hi:04X} falls inside the DATA pool \
                 (${start:04X}-${end:04X}); READ needs the pool in one piece. \
                 Move the reservation, or compile with --no-auto-reserve if it \
                 was discovered automatically."
            ));
        }
    }
    Ok(())
}

/// How many bytes of dead fill have to go in front of `__DATA:` for the pool to clear every reserved range, or `None` when it already does.
fn data_pool_clearance_fill<F>(
    asm: &str,
    reserved: &[(u16, u16)],
    lookup: F,
) -> Result<Option<usize>, String>
where
    F: Fn(&str) -> Option<u16>,
{
    if reserved.is_empty() {
        return Ok(None);
    }
    let Some(start) = lookup("__DATA") else {
        return Ok(None);
    };

    let want: usize = asm
        .lines()
        .filter(|l| is_pool_entry_line(l))
        .map(data_pool_byte_len)
        .sum();
    if want == 0 {
        return Ok(None);
    }
    let mut s = u32::from(start);
    // Each step clears one reservation and can expose the next, so iterate; `reserved` is disjoint, so one pass per range suffices.
    for _ in 0..=reserved.len() {
        let last = s + want as u32 - 1;
        let Some(&(_, hi)) = reserved
            .iter()
            .find(|&&(lo, hi)| u32::from(lo) <= last && u32::from(hi) >= s)
        else {
            break;
        };
        s = u32::from(hi) + 1;
    }
    if s == u32::from(start) {
        return Ok(None);
    }
    if s + want as u32 > 0x1_0000 {
        return Err(format!(
            "?OUT OF MEMORY: a reserved range falls inside the {want}-byte DATA pool at \
             ${start:04X}, and the pool has to be one unbroken run — READ walks it byte by \
             byte with no relocation. Clearing the reservation would put it at ${s:04X}, \
             past the top of memory. Move the reservation, or compile with \
             --no-auto-reserve if it was discovered automatically."
        ));
    }
    Ok(Some((s - u32::from(start)) as usize))
}

/// Splice `len` bytes of dead fill in front of the `__DATA:` label, so the pool that follows starts higher up.
fn insert_data_pool_clearance(asm: &str, len: usize) -> Option<String> {
    if len == 0 {
        return Some(asm.to_string());
    }
    let mut at = 0usize;
    let mut found = None;
    for line in asm.split_inclusive('\n') {
        if line.trim_end() == "__DATA:" {
            found = Some(at);
            break;
        }
        at += line.len();
    }
    let at = found?;
    let mut block = String::with_capacity(len * 4 + 256);
    block.push_str(
        "; layout: dead fill so the DATA pool starts above every reserved range.\n\
         ; The assembler bridges a reservation with a `JMP` plus zero pad, and\n\
         ; READ walks the pool byte by byte with no relocation — a bridge inside\n\
         ; it is handed back as DATA values. See `data_pool_clearance_fill`.\n",
    );
    let mut left = len;
    while left > 0 {
        let take = left.min(16);
        block.push_str("    .byte ");
        for i in 0..take {
            if i > 0 {
                block.push(',');
            }
            block.push_str("$00");
        }
        block.push('\n');
        left -= take;
    }
    let mut out = String::with_capacity(asm.len() + block.len());
    out.push_str(&asm[..at]);
    out.push_str(&block);
    out.push_str(&asm[at..]);
    Some(out)
}

/// Post-condition on the linked image: the DATA pool occupies exactly as many bytes as it has entries, in one run starting at `__DATA`.
fn verify_data_pool_contiguous<F>(asm: &str, lookup: F) -> Result<(), String>
where
    F: Fn(&str) -> Option<u16>,
{
    let Some(start) = lookup("__DATA") else {
        return Ok(());
    };
    let Some(close) = lookup("__DATA_END") else {
        return Err(
            "internal error: the DATA pool has no `__DATA_END` marker in the linked image, \
             so its integrity cannot be checked. Some pass dropped the label."
                .to_string(),
        );
    };

    let want: usize = asm
        .lines()
        .filter(|l| is_pool_entry_line(l))
        .map(data_pool_byte_len)
        .sum();
    let got = close.wrapping_sub(start) as usize;
    if got != want {
        return Err(format!(
            "internal error: the DATA pool is not contiguous — {want} byte(s) of entries \
             were emitted but `__DATA_END` links at ${close:04X}, {got} byte(s) past \
             `__DATA` (${start:04X}). READ walks the pool forward with no relocation, so \
             whatever was spliced into the gap would be handed back as DATA values."
        ));
    }
    Ok(())
}

/// A DATA-pool entry line, identified by the index comment codegen writes after the bytes: `    .byte $0A,$00  ; #0 = Float 10`.
fn is_pool_entry_line(line: &str) -> bool {
    let Some(rest) = line.trim_start().strip_prefix(".byte ") else {
        return false;
    };
    let Some(tail) = rest.split_once("; #").map(|(_, t)| t) else {
        return false;
    };
    let digits = tail.len() - tail.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    digits > 0 && tail[digits..].starts_with(" = ")
}

/// Whole-module facts the per-statement auto-reserve walk needs but cannot derive from the statement in front of it.
struct ReserveCtx {
    /// Bitmap mode: `$D011` bit 5 repurposes CB13 as the base of an 8 KB bitmap, so the 2 KB character window `$D018` names does not exist.
    bitmap_mode: bool,

    vic_bank: CharsetBank,
    /// Counts and refusal reasons from [`resolve_vic_bank`].
    vic_stats: VicStats,
    /// Variables that are ever assigned an expression naming themselves (`T = T + 1`).
    walking: std::collections::HashSet<crate::ast::VarName>,
    /// Scalars that are ever a `READ` target.
    read_targets: std::collections::HashSet<crate::ast::VarName>,
    /// Total DATA items in the program ; the upper bound on how many times any DATA-fed loader can go round.
    data_items: u32,
    /// The program's DATA pool reinterpreted as machine code: maximal runs of consecutive items that are integers in `0..=255`.
    data_runs: Vec<Vec<u8>>,
}

impl ReserveCtx {
    fn new(module: &ir::Module) -> Self {
        let mut walking = std::collections::HashSet::new();
        let mut read_targets = std::collections::HashSet::new();
        let mut data_items: u32 = 0;
        // Byte runs of the DATA pool, in program order.
        let mut data_runs: Vec<Vec<u8>> = Vec::new();
        let mut cur: Vec<u8> = Vec::new();
        for_each_stmt(module, &mut |stmt| match stmt {
            ir::Stmt::Let { var, value } => {
                if expr_names_var(value, var) {
                    walking.insert(var.clone());
                }
            }
            ir::Stmt::Read(targets) => {
                for t in targets {
                    if let ir::ReadTarget::Scalar(v) = t {
                        read_targets.insert(v.clone());
                    }
                }
            }
            ir::Stmt::Data(values) => {
                data_items = data_items.saturating_add(values.len() as u32);
                for v in values {
                    match v {
                        crate::ast::DataValue::Float(f)
                            if f.fract() == 0.0 && (0.0..=255.0).contains(f) =>
                        {
                            // `f` is integral and in range, so the cast is exact.
                            cur.push(*f as u8);
                        }

                        crate::ast::DataValue::String(b) if b.is_empty() => cur.push(0),
                        _ => {
                            if !cur.is_empty() {
                                data_runs.push(std::mem::take(&mut cur));
                            }
                        }
                    }
                }
            }
            _ => {}
        });
        if !cur.is_empty() {
            data_runs.push(cur);
        }
        let (vic_bank, vic_stats) = resolve_vic_bank(module);
        Self {
            bitmap_mode: module_sets_bitmap_mode(module),
            vic_bank,
            vic_stats,
            walking,
            read_targets,
            data_items,
            data_runs,
        }
    }
}

/// Visit every statement in `module`, descending into the bodies of the statements that carry one.
fn for_each_stmt(module: &ir::Module, f: &mut impl FnMut(&ir::Stmt)) {
    fn go(stmt: &ir::Stmt, f: &mut impl FnMut(&ir::Stmt)) {
        f(stmt);
        match stmt {
            ir::Stmt::If {
                then: ir::ThenIr::Stmts(inner),
                ..
            } => {
                for s in inner {
                    go(s, f);
                }
            }
            ir::Stmt::IfElse {
                then, else_then, ..
            } => {
                if let ir::ThenIr::Stmts(inner) = then {
                    for s in inner {
                        go(s, f);
                    }
                }
                if let ir::ThenIr::Stmts(inner) = else_then {
                    for s in inner {
                        go(s, f);
                    }
                }
            }
            _ => {}
        }
    }
    for line in &module.lines {
        for stmt in &line.stmts {
            go(stmt, f);
        }
    }
}

/// Does `e` read `var`?
fn expr_names_var(e: &ir::Expr, var: &crate::ast::VarName) -> bool {
    use ir::Expr;
    match e {
        Expr::Var(v) => v == var,
        Expr::Neg(inner) | Expr::Not(inner) => expr_names_var(inner, var),
        Expr::Bin(_, l, r) => expr_names_var(l, var) || expr_names_var(r, var),
        Expr::Func1(_, arg) => expr_names_var(arg, var),
        Expr::Peek(addr) => expr_names_var(addr, var),
        Expr::ArrayRef(_, idx) => idx.iter().any(|e| expr_names_var(e, var)),
        _ => false,
    }
}

fn discover_reserved_ranges(module: &ir::Module) -> Vec<(u16, u16)> {
    use std::collections::{BTreeSet, HashMap};
    let mut points: BTreeSet<u16> = BTreeSet::new();
    let mut ranges: Vec<(u16, u16)> = Vec::new();
    let mut for_stack: Vec<(crate::ast::VarName, i32, i32)> = Vec::new();
    // Parallel to `for_stack`: the value each live counter will hold once its `NEXT` falls through, when that is exactly knowable.
    let mut for_exit: Vec<Option<i32>> = Vec::new();
    let mut let_ranges: HashMap<crate::ast::VarName, (i32, i32)> = HashMap::new();
    let ctx = ReserveCtx::new(module);

    if std::env::var("YAB_DEBUG_VICBANK").as_deref() == Ok("1") {
        let s = ctx.vic_stats;
        eprintln!(
            "vicbank: bank={:?} bitmap={} bank_writes={} d018_writes={} multi={} opaque={}",
            ctx.vic_bank, ctx.bitmap_mode, s.bank_writes, s.d018_writes, s.multi_bank, s.opaque_bank
        );
    }

    const ARRAY_SETTLE_PASSES: usize = 8;
    let mut arrays = ArrayValueRanges::default();
    collect_opaque_array_writers(module, &mut arrays);
    for _ in 0..ARRAY_SETTLE_PASSES {
        let mut scratch_points: BTreeSet<u16> = BTreeSet::new();
        let mut scratch_ranges: Vec<(u16, u16)> = Vec::new();
        let mut scratch_let: HashMap<crate::ast::VarName, (i32, i32)> = HashMap::new();
        for line in &module.lines {
            for stmt in &line.stmts {
                collect_reserved_in_stmt(
                    stmt,
                    &mut scratch_points,
                    &mut scratch_ranges,
                    &mut for_stack,
                    &mut for_exit,
                    &mut scratch_let,
                    &mut arrays,
                    &ctx,
                );
            }
        }
        for_stack.clear();
        for_exit.clear();
        if !arrays.settle() {
            break;
        }
    }
    for line in &module.lines {
        for stmt in &line.stmts {
            collect_reserved_in_stmt(
                stmt,
                &mut points,
                &mut ranges,
                &mut for_stack,
                &mut for_exit,
                &mut let_ranges,
                &mut arrays,
                &ctx,
            );
        }
    }
    // Whole-module, not per-statement: the destination of a POKE-loaded block move is declared in the DATA pool, not at any one POKE site.
    note_ml_copy_destination(&ctx, &mut ranges);

    note_walking_scalar_ranges(module, &arrays, &mut ranges);
    let mut all: Vec<(u16, u16)> = ranges;
    for p in &points {
        all.push((*p, *p));
    }
    // MEMSIZ-hint fill-in: when the program does the canonical
    // `POKE 55, lo: POKE 56, hi` (or the reverse order) idiom — the
    // C64 way of saying "BASIC, don't touch anything from $hi*256+lo
    // upward, that's mine" — fill in gaps between the ranges we
    // discovered above. POKE-loaded ML often uses scratch bytes that
    // BASIC code never POKEs directly. Without backfilling, the
    // compiler would happily place code in those
    // gaps and the ML would corrupt them on first call.
    //
    // Conservative bound: only consolidate ranges within a 4 KB
    // window above MEMSIZ. Distant clusters (e.g. a custom charset
    // at $3000 when MEMSIZ is $1C00) stay separate so we don't
    // smear the whole address space into one giant reservation.
    if let Some(memsize) = detect_memsize_hint(module) {
        if let Some(end) = max_range_end_in_window(&all, memsize, 0x1000) {
            all.push((memsize, end));
        }
    }
    // `YAB_DEBUG_RESERVED=2` additionally dumps the pre-merge hits, widest first.
    if std::env::var("YAB_DEBUG_RESERVED").as_deref() == Ok("2") {
        let mut sorted = all.clone();
        sorted.sort_by_key(|(s, e)| (std::cmp::Reverse(*e as i32 - *s as i32), *s));
        eprintln!("auto-reserve raw hits ({} before merge):", sorted.len());
        for (s, e) in sorted.iter().take(20) {
            eprintln!("  ${s:04X}-${e:04X}  ({} bytes)", *e as u32 - *s as u32 + 1);
        }
    }
    merge_reserved_ranges(all)
}

/// Scan `module` for the canonical `POKE 55, lo: POKE 56, hi`
/// MEMSIZ-protect idiom (or the reversed order) and return the
/// resulting MEMSIZ value if found. Both POKEs must use literal
/// values; both halves must be present (a half-write is a different
/// trick we don't recognise).
fn detect_memsize_hint(module: &ir::Module) -> Option<u16> {
    use ir::Stmt;
    let mut lo: Option<u8> = None;
    let mut hi: Option<u8> = None;
    let mut visit = |stmt: &Stmt| {
        if let Stmt::Poke { addr, value } = stmt {
            let Some(a) = literal_addr(addr) else { return };
            let Some(v) = literal_byte(value) else { return };
            match a {
                0x37 => lo = Some(v),
                0x38 => hi = Some(v),
                _ => {}
            }
        }
    };
    for line in &module.lines {
        for stmt in &line.stmts {
            visit(stmt);
            // Walk one level into THEN-bodies — POKE 55/56 inside an
            // IF is unusual but not unheard of.
            if let Stmt::If {
                then: ir::ThenIr::Stmts(inner),
                ..
            } = stmt
            {
                for s in inner {
                    visit(s);
                }
            }
        }
    }
    let (lo, hi) = (lo?, hi?);
    let memsize = (hi as u16) * 256 + (lo as u16);
    // Below $0800 makes no sense — BASIC's TXTTAB starts at $0801,
    // and MEMSIZ has to be above that for the program to load. Bail
    // on suspicious values rather than emit garbage reservations.
    if memsize < 0x0800 {
        return None;
    }
    Some(memsize)
}

/// Pull a literal byte (0..=255) out of `e`, used by the MEMSIZ
/// detector to read the value side of `POKE 55/56, X`.
fn literal_byte(e: &ir::Expr) -> Option<u8> {
    let ir::Expr::Number(n) = e else { return None };
    if !n.is_finite() || n.fract() != 0.0 {
        return None;
    }
    let v = *n as i64;
    if (0..=255).contains(&v) {
        Some(v as u8)
    } else {
        None
    }
}

/// Pull a literal i64 address out of `e`, no auto-reserve-window
/// gating. Used by the MEMSIZ detector since the address side of
/// `POKE 55, X` lives below the auto-reserve window.
fn literal_addr(e: &ir::Expr) -> Option<u16> {
    let ir::Expr::Number(n) = e else { return None };
    if !n.is_finite() || n.fract() != 0.0 {
        return None;
    }
    let v = *n as i64;
    if (0..=0xFFFF).contains(&v) {
        Some(v as u16)
    } else {
        None
    }
}

/// Among the ranges in `all`, return the maximum `end` of any range
/// whose `start` falls in `[memsize, memsize + window)`. Used by
/// the MEMSIZ-fill heuristic to find how far up the auto-reserve
/// hits cluster — ranges further out (e.g. a custom charset window)
/// are intentionally left as their own clusters.
fn max_range_end_in_window(all: &[(u16, u16)], memsize: u16, window: u16) -> Option<u16> {
    let upper = memsize.saturating_add(window);
    all.iter()
        .filter(|(s, _)| *s >= memsize && *s < upper)
        .map(|(_, e)| *e)
        .max()
}

#[allow(clippy::too_many_arguments)]
fn collect_reserved_in_stmt(
    stmt: &ir::Stmt,
    points: &mut std::collections::BTreeSet<u16>,
    ranges: &mut Vec<(u16, u16)>,
    for_stack: &mut Vec<(crate::ast::VarName, i32, i32)>,
    for_exit: &mut Vec<Option<i32>>,
    let_ranges: &mut std::collections::HashMap<crate::ast::VarName, (i32, i32)>,
    arrays: &mut ArrayValueRanges,
    ctx: &ReserveCtx,
) {
    use ir::Stmt;
    match stmt {
        Stmt::Poke { addr, value } => {
            note_addr_literal(addr, points);

            note_data_loader_extent(addr, value, ctx, ranges);
            // Computed-address POKE inside a FOR with literal
            // bounds: range-evaluate the address expression with
            // each tracked counter substituted by `[start, end]`,
            // and reserve `[min, max]` if the result fits the
            // auto-reserve window.
            note_addr_for_range(addr, for_stack, let_ranges, arrays, ranges);
            note_d018_charset_window(addr, value, ranges, ctx.bitmap_mode, ctx.vic_bank);
            note_screen_page_window(addr, value, ranges);
            note_sprite_pointer_block(
                addr,
                value,
                ranges,
                ctx.vic_bank,
                for_stack,
                let_ranges,
                arrays,
            );
            note_peek_in_expr(value, points);
        }
        Stmt::Dpoke { addr, value } => {
            // DPOKE writes 2 consecutive bytes at addr and addr+1.
            if let Some(a) = addr_literal_in_window(addr) {
                points.insert(a);
                if a < AUTO_RESERVE_HI {
                    points.insert(a + 1);
                }
            }
            // Same range-eval treatment as POKE, plus the
            // adjacent byte. We add the high bound +1 too.
            if let Some((min, max)) = addr_for_range_in_window(addr, for_stack, let_ranges, arrays)
                && max < AUTO_RESERVE_HI
            {
                ranges.push((min, max + 1));
            }
            note_peek_in_expr(value, points);
        }
        Stmt::PokeFill {
            dst_start,
            dst_end,
            value,
        } => {

            if let (Some(s), Some(e)) = (
                addr_literal_in_window(dst_start),
                addr_literal_in_window(dst_end),
            ) {
                ranges.push((s, s.max(e)));
            }
            note_peek_in_expr(value, points);
        }
        Stmt::Let { var, value } => {
            note_peek_in_expr(value, points);
            // An assignment to a variable that is currently a live FOR counter voids the `[start, end]` interval this analysis has for it.
            invalidate_for_counter(for_stack, var);

            match eval_addr_range(value, for_stack, let_ranges, arrays) {
                Some(r) => {
                    let_ranges.insert(var.clone(), r);
                }
                None => {
                    let_ranges.remove(var);
                }
            }
        }
        Stmt::ArrayLet {
            name,
            indices,
            value,
        } => {
            for e in indices {
                note_peek_in_expr(e, points);
            }
            note_peek_in_expr(value, points);
            // Record this element's value.
            note_array_let(name, indices, value, for_stack, let_ranges, arrays);
        }
        Stmt::If { cond, then } => {
            note_peek_in_expr(cond, points);
            if let ir::ThenIr::Stmts(inner) = then {
                for s in inner {
                    collect_reserved_in_stmt(
                        s,
                        points,
                        ranges,
                        for_stack,
                        for_exit,
                        let_ranges,
                        arrays,
                        ctx,
                    );
                }
            }
        }
        Stmt::Print { items, .. } => {
            for p in items {
                if let ir::PrintPiece::Expr(e)
                | ir::PrintPiece::CharOut(e)
                | ir::PrintPiece::TabTo(e)
                | ir::PrintPiece::Spc(e) = p
                {
                    note_peek_in_expr(e, points);
                }
            }
        }
        // SYS to a literal address is ALSO interesting (program
        // calls user-supplied machine code at that address — we
        // mustn't overwrite it). Reserve the byte at the SYS target
        // even though we don't know the routine's actual length.
        Stmt::Sys { addr, .. } => {
            if let Some(a) = addr_literal_in_window(addr) {
                points.insert(a);
            }
        }
        Stmt::For {
            var,
            start,
            end,
            step,
            ..
        } => {
            // Track the counter on a stack when both bounds reduce
            // to a known interval — either a literal or an
            // expression in terms of outer FOR counters / earlier
            // LET bounds. E.T's `FOR D=64*E TO 64*E+62` (sprite-
            // block loader) has bounds that depend on the enclosing
            // `FOR E=200 TO 207`; evaluating them against the outer
            // stack gives `D ∈ [12800, 13311]`, which lets
            // discover_reserved_ranges keep the assembler off the
            // sprite blocks at $3200-$33FF.
            let s_range = eval_addr_range(start, for_stack, let_ranges, arrays);

            let e_range = if let Some((lo, hi)) = s_range {
                for_stack.push((var.clone(), lo, hi));
                let r = eval_addr_range(end, for_stack, let_ranges, arrays);
                for_stack.pop();
                r
            } else {
                eval_addr_range(end, for_stack, let_ranges, arrays)
            };

            let exit = match (
                s_range,
                e_range,
                eval_addr_range(step, for_stack, let_ranges, arrays),
            ) {
                (Some((s, s2)), Some((e, e2)), Some((st, st2)))
                    if s == s2 && e == e2 && st == st2 =>
                {
                    for_trips_and_exit(s, e, st).map(|(_, exit)| exit)
                }
                _ => None,
            };
            for_exit.push(exit);
            // A `FOR` rebinds its counter, so whatever `let_ranges` recorded for that name from an earlier assignment is dead.
            let_ranges.remove(var);
            if let (Some((s_lo, s_hi)), Some((e_lo, e_hi))) = (s_range, e_range) {
                let lo = s_lo.min(s_hi).min(e_lo).min(e_hi);
                let hi = s_lo.max(s_hi).max(e_lo).max(e_hi);
                for_stack.push((var.clone(), lo, hi));
            } else {
                // Push a sentinel with a name we'll never match so
                // matching NEXTs pop SOMETHING and the stack stays
                // balanced — but range eval ignores it (no var ever
                // matches).
                for_stack.push((
                    crate::ast::VarName {
                        base: "__UNKNOWN_FOR_BOUND__".to_string(),
                        kind: crate::ast::VarKind::Float,
                    },
                    0,
                    0,
                ));
            }
        }
        Stmt::Next { vars } => {
            // Bare NEXT closes one. NEXT i,j closes two — pop in
            // the order popped by the compiler.
            let n = vars.len().max(1);
            for _ in 0..n {
                let popped = for_stack.pop();

                match (popped, for_exit.pop().flatten()) {
                    (Some((name, _, _)), Some(v)) if !is_unknown_for_bound(&name) => {
                        let_ranges.insert(name, (v, v));
                    }
                    (Some((name, _, _)), _) => {
                        let_ranges.remove(&name);
                    }
                    (None, _) => {}
                }
            }
        }
        Stmt::Input { targets, .. } | Stmt::Read(targets) => {

            for t in targets {
                if let ir::ReadTarget::Scalar(name) = t {
                    invalidate_for_counter(for_stack, name);
                    let_ranges.remove(name);
                }
            }
        }
        _ => {}
    }
}

/// How many times `FOR var = s TO e STEP st` runs its body, and what `var` holds once the matching `NEXT` falls through.
fn for_trips_and_exit(s: i32, e: i32, st: i32) -> Option<(i32, i32)> {
    if st == 0 {
        return None;
    }
    let n = if (st > 0 && s > e) || (st < 0 && s < e) {
        1
    } else {
        e.checked_sub(s)?.checked_div(st)?.checked_add(1)?
    };
    let exit = n.checked_mul(st).and_then(|d| s.checked_add(d))?;
    Some((n, exit))
}

/// Name the sentinel entry `for_stack` carries for a counter whose interval this analysis cannot use.
const UNUSABLE_FOR_COUNTER: &str = "__UNKNOWN_FOR_BOUND__";

fn is_unknown_for_bound(name: &crate::ast::VarName) -> bool {
    name.base == UNUSABLE_FOR_COUNTER
}

/// Drop the tracked interval for `var` if it is a live FOR counter.
fn invalidate_for_counter(
    for_stack: &mut [(crate::ast::VarName, i32, i32)],
    var: &crate::ast::VarName,
) {
    for entry in for_stack.iter_mut() {
        if entry.0 == *var {
            entry.0 = crate::ast::VarName {
                base: UNUSABLE_FOR_COUNTER.to_string(),
                kind: entry.0.kind,
            };
        }
    }
}

/// What [`eval_addr_range`] knows about the values an array's elements can hold.
#[derive(Debug, Default, Clone, PartialEq)]
struct ArrayValueRanges {
    /// Last completed pass's answers, and the only thing [`ArrayValueRanges::get`] consults.
    snapshot: ArrayPass,
    /// The pass being built.
    cur: ArrayPass,
    /// Arrays with at least one write this analysis structurally cannot read ; `READ`, `INPUT`, a `FETCH` into a subscript.
    opaque: std::collections::HashSet<crate::ast::VarName>,
}

/// One settling pass's worth of answers.
#[derive(Debug, Default, Clone, PartialEq)]
struct ArrayPass {

    elems: std::collections::HashMap<(crate::ast::VarName, i32), (i32, i32)>,

    spread: std::collections::HashMap<crate::ast::VarName, (i32, i32)>,
    /// Elements that ARE assigned but whose value this pass could not evaluate ; distinct both from "known" and from "poisoned".
    unknown_elems: std::collections::HashSet<(crate::ast::VarName, i32)>,

    unknown_spread: std::collections::HashSet<crate::ast::VarName>,
}

impl ArrayPass {
    fn touches(&self, name: &crate::ast::VarName) -> bool {
        self.spread.contains_key(name)
            || self.unknown_spread.contains(name)
            || self.elems.keys().any(|(n, _)| n == name)
            || self.unknown_elems.iter().any(|(n, _)| n == name)
    }
}

/// The most element assignments one counter-driven statement may be unrolled into.
const ARRAY_UNROLL_LIMIT: i32 = 512;

fn note_array_let(
    name: &crate::ast::VarName,
    indices: &[ir::Expr],
    value: &ir::Expr,
    for_stack: &[(crate::ast::VarName, i32, i32)],
    let_ranges: &std::collections::HashMap<crate::ast::VarName, (i32, i32)>,
    arrays: &mut ArrayValueRanges,
) {
    // Only one-dimensional arrays are modelled.
    let [idx] = indices else {
        arrays.poison(name);
        return;
    };

    if let Some((lo, hi)) = eval_addr_range(idx, for_stack, let_ranges, arrays) {
        if lo == hi {
            match eval_addr_range(value, for_stack, let_ranges, arrays) {
                Some(r) => arrays.note_elem(name, lo, r),
                // Assigned, value not readable this pass.
                None => arrays.note_unknown_elem(name, lo),
            }
            return;
        }
    }

    // `S(X)` where `X` is exactly one tracked counter: unroll it.
    if let ir::Expr::Var(v) = idx {
        if let Some(&(_, lo, hi)) = for_stack.iter().find(|(n, _, _)| n == v) {
            if lo <= hi && hi - lo < ARRAY_UNROLL_LIMIT {
                let mut pinned: Vec<_> = for_stack.to_vec();
                let at = pinned
                    .iter()
                    .position(|(n, _, _)| n == v)
                    .expect("just found it in for_stack");
                for k in lo..=hi {
                    pinned[at].1 = k;
                    pinned[at].2 = k;
                    match eval_addr_range(value, &pinned, let_ranges, arrays) {
                        Some(r) => arrays.note_elem(name, k, r),
                        None => arrays.note_unknown_elem(name, k),
                    }
                }
                return;
            }
        }
    }

    match eval_addr_range(value, for_stack, let_ranges, arrays) {
        Some(r) => arrays.note_spread(name, r),
        None => arrays.note_unknown_spread(name),
    }
}

impl ArrayValueRanges {
    /// Record `S(idx) = range` for a literal index.
    fn note_elem(&mut self, name: &crate::ast::VarName, idx: i32, range: (i32, i32)) {
        let slot = self
            .cur
            .elems
            .entry((name.clone(), idx))
            .or_insert(range);
        slot.0 = slot.0.min(range.0);
        slot.1 = slot.1.max(range.1);
    }

    fn note_spread(&mut self, name: &crate::ast::VarName, range: (i32, i32)) {
        let slot = self.cur.spread.entry(name.clone()).or_insert(range);
        slot.0 = slot.0.min(range.0);
        slot.1 = slot.1.max(range.1);
    }

    /// `S(idx)` is assigned, but this pass could not evaluate what to.
    fn note_unknown_elem(&mut self, name: &crate::ast::VarName, idx: i32) {
        self.cur.unknown_elems.insert((name.clone(), idx));
    }

    /// Some element of `name` is assigned a value this pass cannot read.
    fn note_unknown_spread(&mut self, name: &crate::ast::VarName) {
        self.cur.unknown_spread.insert(name.clone());
    }

    /// Mark every element of `name` unreadable, for good.
    fn poison(&mut self, name: &crate::ast::VarName) {
        self.opaque.insert(name.clone());
    }

    /// The interval `S(idx)` may hold.
    fn get(&self, name: &crate::ast::VarName, idx: Option<i32>) -> Option<(i32, i32)> {
        if self.opaque.contains(name) {
            return None;
        }
        let s = &self.snapshot;
        let spread = s.spread.get(name).copied();

        if s.unknown_spread.contains(name) {
            return None;
        }
        match idx {
            Some(k) => {
                if s.unknown_elems.contains(&(name.clone(), k)) {
                    return None;
                }
                match (s.elems.get(&(name.clone(), k)).copied(), spread) {

                    (Some(e), None) => Some(e),

                    (e, Some(sp)) => {
                        let (mut lo, mut hi) = sp;
                        if let Some((el, eh)) = e {
                            lo = lo.min(el);
                            hi = hi.max(eh);
                        }
                        Some((lo.min(0), hi.max(0)))
                    }
                    // Never written, and every write to this array is accounted for: the ROM reads zero here.
                    (None, None) => {
                        if s.touches(name) {
                            Some((0, 0))
                        } else {
                            None
                        }
                    }
                }
            }

            None => {
                if s.unknown_elems.iter().any(|(n, _)| n == name) {
                    return None;
                }
                let mut acc: Option<(i32, i32)> = None;
                let mut fold = |(lo, hi): (i32, i32)| {
                    acc = Some(match acc {
                        None => (lo, hi),
                        Some((a, b)) => (a.min(lo), b.max(hi)),
                    });
                };
                for ((n, _), r) in &s.elems {
                    if n == name {
                        fold(*r);
                    }
                }
                if let Some(sp) = spread {
                    fold(sp);
                }
                acc.map(|(lo, hi)| (lo.min(0), hi.max(0)))
            }
        }
    }

    /// Does the settled map know anything at all about `name`?
    fn touches(&self, name: &crate::ast::VarName) -> bool {
        self.snapshot.touches(name)
    }

    fn settle(&mut self) -> bool {
        let next = std::mem::take(&mut self.cur);
        let changed = self.snapshot != next;
        self.snapshot = next;
        changed
    }

    fn is_empty(&self) -> bool {
        self.snapshot == ArrayPass::default()
    }
}

/// Poison every array written by a statement this walk does not read as an element assignment.
fn collect_opaque_array_writers(module: &ir::Module, out: &mut ArrayValueRanges) {
    fn visit(stmt: &ir::Stmt, out: &mut ArrayValueRanges) {
        match stmt {
            ir::Stmt::ArrayLet { .. } => {}
            ir::Stmt::If {
                then: ir::ThenIr::Stmts(inner),
                ..
            } => {
                for s in inner {
                    visit(s, out);
                }
            }
            other => {

                if let ir::Stmt::Fetch {
                    target,
                    target_indices,
                    ..
                } = other
                    && !target_indices.is_empty()
                {
                    out.poison(target);
                }
                for region in &crate::analysis::effect_summary_for_stmt(other).writes {
                    if let crate::analysis::EffectRegion::ArrayData(name) = region {
                        out.poison(name);
                    }
                }
            }
        }
    }
    for line in &module.lines {
        for stmt in &line.stmts {
            visit(stmt, out);
        }
    }
}

const MAX_WALK_SPAN: i32 = 8 * 1024;

/// A `FOR` frame, as [`WalkFlatten`] sees it.
struct WalkFrame {
    /// Exact trip count.
    trip: Option<i64>,
    /// Item index of the `FOR`.
    open_at: usize,
    /// Item index of the matching `NEXT`, if the walk reached one.
    close_at: Option<usize>,
    /// What each candidate scalar held on entry to this frame.
    entry: Vec<(crate::ast::VarName, (i32, i32))>,
}

enum ScalarWrite {

    None,
    /// Writes exactly these.
    Named(Vec<crate::ast::VarName>),

    Any,
}

/// One flattened statement, with the loop frames open at it.
struct WalkItem<'a> {
    stmt: &'a ir::Stmt,
    /// Frame ids open here, outermost first.
    open: Vec<usize>,

    conditional: bool,
    writes: ScalarWrite,

    snap: Option<Box<WalkSnapshot>>,
}

type WalkSnapshot = (
    std::collections::HashMap<crate::ast::VarName, (i32, i32)>,
    Vec<(crate::ast::VarName, i32, i32)>,
);

struct WalkFlatten<'a> {
    items: Vec<WalkItem<'a>>,
    frames: Vec<WalkFrame>,
    for_stack: Vec<(crate::ast::VarName, i32, i32)>,
    for_exit: Vec<Option<i32>>,
    /// Parallel to `for_stack`: the frame id of each open loop.
    stack_ids: Vec<usize>,
    let_ranges: std::collections::HashMap<crate::ast::VarName, (i32, i32)>,
}

impl<'a> WalkFlatten<'a> {

    fn invalidate(&mut self, var: &crate::ast::VarName) {
        for i in 0..self.for_stack.len() {
            if self.for_stack[i].0 == *var {
                self.for_stack[i].0 = crate::ast::VarName {
                    base: UNUSABLE_FOR_COUNTER.to_string(),
                    kind: self.for_stack[i].0.kind,
                };
                if let Some(fid) = self.stack_ids.get(i) {
                    self.frames[*fid].trip = None;
                }
            }
        }
    }

    fn stmt(
        &mut self,
        s: &'a ir::Stmt,
        conditional: bool,
        cands: &[crate::ast::VarName],
        arrays: &ArrayValueRanges,
    ) {
        use ir::Stmt;
        let snap = match s {
            Stmt::Poke { addr, .. } | Stmt::Dpoke { addr, .. }
                if cands.iter().any(|v| expr_names_var(addr, v)) =>
            {
                Some(Box::new((
                    self.let_ranges.clone(),
                    self.for_stack.clone(),
                )))
            }
            _ => None,
        };

        let writes = match s {
            Stmt::If {
                then: ir::ThenIr::Stmts(_),
                ..
            } => ScalarWrite::None,
            other => scalar_writes_of(other),
        };
        let idx = self.items.len();
        self.items.push(WalkItem {
            stmt: s,
            open: self.stack_ids.clone(),
            conditional,
            writes,
            snap,
        });
        match s {
            Stmt::Let { var, value } => {
                self.invalidate(var);
                match eval_addr_range(value, &self.for_stack, &self.let_ranges, arrays) {
                    Some(r) => {
                        self.let_ranges.insert(var.clone(), r);
                    }
                    None => {
                        self.let_ranges.remove(var);
                    }
                }
            }
            Stmt::If {
                then: ir::ThenIr::Stmts(inner),
                ..
            } => {
                for st in inner {
                    self.stmt(st, true, cands, arrays);
                }
            }
            Stmt::For {
                var,
                start,
                end,
                step,
                ..
            } => {
                let s_range = eval_addr_range(start, &self.for_stack, &self.let_ranges, arrays);
                let e_range = if let Some((lo, hi)) = s_range {
                    self.for_stack.push((var.clone(), lo, hi));
                    let r = eval_addr_range(end, &self.for_stack, &self.let_ranges, arrays);
                    self.for_stack.pop();
                    r
                } else {
                    eval_addr_range(end, &self.for_stack, &self.let_ranges, arrays)
                };
                let st_range = eval_addr_range(step, &self.for_stack, &self.let_ranges, arrays);
                let counted = match (s_range, e_range, st_range) {
                    (Some((s, s2)), Some((e, e2)), Some((st, st2)))
                        if s == s2 && e == e2 && st == st2 =>
                    {
                        for_trips_and_exit(s, e, st)
                    }
                    _ => None,
                };
                self.for_exit.push(counted.map(|(_, exit)| exit));
                self.let_ranges.remove(var);
                let fid = self.frames.len();
                self.frames.push(WalkFrame {
                    trip: counted.map(|(n, _)| n as i64),
                    open_at: idx,
                    close_at: None,
                    entry: cands
                        .iter()
                        .filter_map(|v| self.let_ranges.get(v).map(|r| (v.clone(), *r)))
                        .collect(),
                });
                match (s_range, e_range) {
                    (Some((s_lo, s_hi)), Some((e_lo, e_hi))) => {
                        let lo = s_lo.min(s_hi).min(e_lo).min(e_hi);
                        let hi = s_lo.max(s_hi).max(e_lo).max(e_hi);
                        self.for_stack.push((var.clone(), lo, hi));
                    }
                    _ => self.for_stack.push((
                        crate::ast::VarName {
                            base: UNUSABLE_FOR_COUNTER.to_string(),
                            kind: crate::ast::VarKind::Float,
                        },
                        0,
                        0,
                    )),
                }
                self.stack_ids.push(fid);
            }
            Stmt::Next { vars } => {
                for _ in 0..vars.len().max(1) {
                    let popped = self.for_stack.pop();
                    if let Some(fid) = self.stack_ids.pop() {
                        self.frames[fid].close_at = Some(idx);
                    }
                    match (popped, self.for_exit.pop().flatten()) {
                        (Some((name, _, _)), Some(v)) if !is_unknown_for_bound(&name) => {
                            self.let_ranges.insert(name, (v, v));
                        }
                        (Some((name, _, _)), _) => {
                            self.let_ranges.remove(&name);
                        }
                        (None, _) => {}
                    }
                }
            }
            Stmt::Input { targets, .. } | Stmt::Read(targets) => {
                for t in targets {
                    if let ir::ReadTarget::Scalar(name) = t {
                        self.invalidate(name);
                        self.let_ranges.remove(name);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Which scalars `stmt` may write, from its effect summary.
fn scalar_writes_of(stmt: &ir::Stmt) -> ScalarWrite {
    use crate::analysis::EffectRegion;
    let sum = crate::analysis::effect_summary_for_stmt(stmt);
    if sum.opaque_write {
        return ScalarWrite::Any;
    }
    let named: Vec<_> = sum
        .writes
        .iter()
        .filter_map(|r| match r {
            EffectRegion::ScalarFloat(v)
            | EffectRegion::ScalarInteger(v)
            | EffectRegion::ScalarString(v) => Some(v.clone()),
            _ => None,
        })
        .collect();
    if named.is_empty() {
        ScalarWrite::None
    } else {
        ScalarWrite::Named(named)
    }
}

/// `v = v + K` / `v = K + v` / `v = v - K` with a literal integer `K`.
fn const_self_stride(value: &ir::Expr, v: &crate::ast::VarName) -> Option<i64> {
    use crate::ast::BinOp;
    use ir::Expr;
    let lit = |e: &ir::Expr| match e {
        Expr::Number(n) if n.is_finite() && n.fract() == 0.0 => Some(*n as i64),
        _ => None,
    };
    let bare = |e: &ir::Expr| matches!(e, Expr::Var(x) if x == v);
    match value {
        Expr::Bin(BinOp::Add, l, r) if bare(l) => lit(r),
        Expr::Bin(BinOp::Add, l, r) if bare(r) => lit(l),
        Expr::Bin(BinOp::Sub, l, r) if bare(l) => lit(r).and_then(|k| k.checked_neg()),
        _ => None,
    }
}

/// What one flattened item does to the scalar being tracked.
enum WalkWrite {
    /// Leaves it alone.
    Untouched,
    /// Advances it by a constant.
    Stride(i64),
    /// Assigns it something this analysis cannot read as a stride.
    Odd,
    /// Sets it from somewhere else entirely.
    Rebase,
}

fn walk_write_of(item: &WalkItem<'_>, v: &crate::ast::VarName) -> WalkWrite {
    if let ir::Stmt::Let { var, value } = item.stmt {
        if var != v {
            return WalkWrite::Untouched;
        }
        return match const_self_stride(value, v) {
            Some(k) => WalkWrite::Stride(k),
            None if expr_names_var(value, v) => WalkWrite::Odd,
            None => WalkWrite::Rebase,
        };
    }
    match &item.writes {
        ScalarWrite::None => WalkWrite::Untouched,
        ScalarWrite::Any => WalkWrite::Rebase,
        ScalarWrite::Named(names) => {
            if names.contains(v) {
                WalkWrite::Rebase
            } else {
                WalkWrite::Untouched
            }
        }
    }
}

/// What one whole execution of a frame does to `v`.
struct FrameScan {
    /// Nothing inside the frame assigns `v` except constant strides of one sign ; the only case in which the frame bounds a walk.
    clean: bool,

    total: i64,
    sites: usize,

    trailing: i64,
}

fn scan_walk_frame(
    items: &[WalkItem<'_>],
    frames: &[WalkFrame],
    f: &WalkFrame,
    v: &crate::ast::VarName,
    poke_at: usize,
) -> FrameScan {
    let mut scan = FrameScan {
        clean: true,
        total: 0,
        sites: 0,
        trailing: 0,
    };
    let close = frame_close_index(items, f);
    let mut sign: i64 = 0;
    for (j, it) in items.iter().enumerate().take(close + 1).skip(f.open_at + 1) {
        let k = match walk_write_of(it, v) {
            WalkWrite::Untouched => continue,
            WalkWrite::Stride(k) => k,
            WalkWrite::Odd | WalkWrite::Rebase => {
                scan.clean = false;
                return scan;
            }
        };
        if k != 0 {
            let s = k.signum();
            if sign != 0 && sign != s {
                // Mixed directions.
                scan.clean = false;
                return scan;
            }
            sign = s;
        }

        let mut mult: i64 = 1;
        for fid in &it.open {
            let g = &frames[*fid];
            if g.open_at < f.open_at {
                continue; // outside `f`
            }
            match g.trip {
                Some(t) if t > 0 => match mult.checked_mul(t) {
                    Some(m) => mult = m,
                    None => {
                        scan.clean = false;
                        return scan;
                    }
                },
                _ => {
                    scan.clean = false;
                    return scan;
                }
            }
        }
        scan.sites += 1;
        if j > poke_at && !it.conditional {
            scan.trailing = k;
        }
        match k.checked_mul(mult).and_then(|d| scan.total.checked_add(d)) {
            Some(t) => scan.total = t,
            None => {
                scan.clean = false;
                return scan;
            }
        }
    }
    // The trailing trim is only valid while the walk is monotone.
    if scan.trailing.signum() != sign {
        scan.trailing = 0;
    }
    scan
}

/// The index of the last item inside `f`.
fn frame_close_index(items: &[WalkItem<'_>], f: &WalkFrame) -> usize {
    let last = items.len().saturating_sub(1);
    f.close_at.unwrap_or(last).min(last)
}

/// Does anything between `f`'s header and its `NEXT` write `u`?
fn frame_writes_scalar(
    items: &[WalkItem<'_>],
    f: &WalkFrame,
    u: &crate::ast::VarName,
) -> bool {
    items
        .iter()
        .take(frame_close_index(items, f) + 1)
        .skip(f.open_at + 1)
        .any(|it| !matches!(walk_write_of(it, u), WalkWrite::Untouched))
}

/// The run `items[idx]`'s `POKE` address covers, when `v` walks it under the enclosing loops.
fn walking_scalar_range(
    items: &[WalkItem<'_>],
    frames: &[WalkFrame],
    idx: usize,
    v: &crate::ast::VarName,
    cands: &[crate::ast::VarName],
    arrays: &ArrayValueRanges,
) -> Option<(u16, u16)> {
    let it = &items[idx];
    let (addr, wide) = match it.stmt {
        ir::Stmt::Poke { addr, .. } => (addr, 0),
        // `DPOKE` writes `addr` and `addr+1`.
        ir::Stmt::Dpoke { addr, .. } => (addr, 1),
        _ => return None,
    };
    let snap = it.snap.as_ref()?;

    if let Some(outer) = it.open.first() {
        let f = &frames[*outer];
        if cands
            .iter()
            .any(|u| u != v && expr_names_var(addr, u) && frame_writes_scalar(items, f, u))
        {
            return None;
        }
    }

    let (base, total, trailing) = it.open.iter().find_map(|fid| {
        let f = &frames[*fid];
        let scan = scan_walk_frame(items, frames, f, v, idx);
        if !scan.clean || scan.sites == 0 {
            return None;
        }
        let base = f.entry.iter().find(|(n, _)| n == v).map(|(_, r)| *r)?;
        Some((base, scan.total, scan.trailing))
    })?;

    let (blo, bhi) = base;
    let span = total.checked_sub(trailing)?;
    let (vlo, vhi) = if span >= 0 {
        (blo as i64, (bhi as i64).checked_add(span)?)
    } else {
        ((blo as i64).checked_add(span)?, bhi as i64)
    };
    let (vlo, vhi) = (i32::try_from(vlo).ok()?, i32::try_from(vhi).ok()?);

    let mut lets = snap.0.clone();
    lets.insert(v.clone(), (vlo, vhi));
    let mut stack = snap.1.clone();
    for entry in stack.iter_mut() {
        if entry.0 == *v {
            entry.0 = crate::ast::VarName {
                base: UNUSABLE_FOR_COUNTER.to_string(),
                kind: entry.0.kind,
            };
        }
    }
    let (lo, hi) = eval_addr_range(addr, &stack, &lets, arrays)?;
    let hi = hi.checked_add(wide)?;
    if lo < 0 || hi > 0xFFFF {
        return None;
    }

    if lo < AUTO_RESERVE_LO as i32 {
        return None;
    }
    let hi = hi.min(AUTO_RESERVE_HI as i32);
    if lo > hi || hi - lo >= MAX_WALK_SPAN {
        return None;
    }
    Some((lo as u16, hi as u16))
}

/// Add to `ranges` every run a `POKE` walks through a scalar the enclosing loops advance.
fn note_walking_scalar_ranges(
    module: &ir::Module,
    arrays: &ArrayValueRanges,
    ranges: &mut Vec<(u16, u16)>,
) {
    fn self_assigned(stmt: &ir::Stmt, out: &mut Vec<crate::ast::VarName>) {
        match stmt {
            ir::Stmt::Let { var, value } if expr_names_var(value, var) => {
                if !out.contains(var) {
                    out.push(var.clone());
                }
            }
            ir::Stmt::If {
                then: ir::ThenIr::Stmts(inner),
                ..
            } => {
                for s in inner {
                    self_assigned(s, out);
                }
            }
            _ => {}
        }
    }
    let mut cands: Vec<crate::ast::VarName> = Vec::new();
    for line in &module.lines {
        for stmt in &line.stmts {
            self_assigned(stmt, &mut cands);
        }
    }
    if cands.is_empty() {
        return;
    }

    let mut w = WalkFlatten {
        items: Vec::new(),
        frames: Vec::new(),
        for_stack: Vec::new(),
        for_exit: Vec::new(),
        stack_ids: Vec::new(),
        let_ranges: std::collections::HashMap::new(),
    };
    for line in &module.lines {
        for stmt in &line.stmts {
            w.stmt(stmt, false, &cands, arrays);
        }
    }

    for idx in 0..w.items.len() {
        let it = &w.items[idx];
        if it.snap.is_none() || it.open.is_empty() {

            continue;
        }
        let addr = match it.stmt {
            ir::Stmt::Poke { addr, .. } | ir::Stmt::Dpoke { addr, .. } => addr,
            _ => continue,
        };
        for v in cands.iter().filter(|v| expr_names_var(addr, v)) {
            if let Some(r) = walking_scalar_range(&w.items, &w.frames, idx, v, &cands, arrays) {
                ranges.push(r);
            }
        }
    }
}

/// Add to `ranges` the interval `[min, max]` reachable by `addr`
/// when the live FOR counters take any value in their tracked
/// `[start, end]` ranges and any tracked LICM-style LET-binding
/// resolves to its previously-recorded range. Skips when neither
/// dependency contributes (the address is fully literal and gets
/// handled elsewhere), the expression contains a free symbol /
/// non-arithmetic op, or the result falls outside the auto-reserve
/// window.
fn note_addr_for_range(
    addr: &ir::Expr,
    for_stack: &[(crate::ast::VarName, i32, i32)],
    let_ranges: &std::collections::HashMap<crate::ast::VarName, (i32, i32)>,
    arrays: &ArrayValueRanges,
    ranges: &mut Vec<(u16, u16)>,
) {
    if for_stack.is_empty() && let_ranges.is_empty() && arrays.is_empty() {
        return;
    }
    if let Some((min, max)) = addr_for_range_in_window(addr, for_stack, let_ranges, arrays) {
        ranges.push((min, max));
    }
}

/// Does `e` read an array element anywhere?
fn expr_reads_an_array(e: &ir::Expr) -> bool {
    use ir::Expr;
    match e {
        Expr::ArrayRef(..) => true,
        Expr::Bin(_, l, r) => expr_reads_an_array(l) || expr_reads_an_array(r),
        Expr::Neg(a) | Expr::Not(a) | Expr::Func1(_, a) => expr_reads_an_array(a),
        _ => false,
    }
}

fn addr_for_range_in_window(
    addr: &ir::Expr,
    for_stack: &[(crate::ast::VarName, i32, i32)],
    let_ranges: &std::collections::HashMap<crate::ast::VarName, (i32, i32)>,
    arrays: &ArrayValueRanges,
) -> Option<(u16, u16)> {
    // Require the expression to actually reference at least one
    // tracked symbol — a fully-literal address is already handled
    // by the `note_addr_literal` path, and re-adding it as a single
    // point is harmless but wasteful.
    if !expr_reads_any_tracked(addr, for_stack, let_ranges, arrays) {
        return None;
    }
    let (lo, hi) = eval_addr_range(addr, for_stack, let_ranges, arrays)?;
    // Discard, do not clamp, an interval that leaves the 6502's address space.
    if lo < 0 || hi > 0xFFFF {
        return None;
    }
    let lo32 = lo.max(AUTO_RESERVE_LO as i32);
    let hi32 = hi.min(AUTO_RESERVE_HI as i32);
    if lo32 > hi32 {
        return None;
    }

    const MAX_ARRAY_HULL: i32 = 12 * 1024;
    if expr_reads_an_array(addr) && hi32 - lo32 >= MAX_ARRAY_HULL {
        return None;
    }
    Some((lo32 as u16, hi32 as u16))
}

/// Range-evaluate `e` against `for_stack` plus `let_ranges` plus `arrays`.
fn eval_addr_range(
    e: &ir::Expr,
    for_stack: &[(crate::ast::VarName, i32, i32)],
    let_ranges: &std::collections::HashMap<crate::ast::VarName, (i32, i32)>,
    arrays: &ArrayValueRanges,
) -> Option<(i32, i32)> {
    use crate::ast::BinOp;
    use ir::Expr;
    match e {
        Expr::Number(n) if n.is_finite() && n.fract() == 0.0 => {
            let i = *n as i32;
            Some((i, i))
        }
        Expr::Var(v) => {
            if let Some(r) = for_stack
                .iter()
                .rev()
                .find(|(name, _, _)| name == v)
                .map(|(_, lo, hi)| (*lo, *hi))
            {
                return Some(r);
            }
            let_ranges.get(v).copied()
        }
        Expr::ArrayRef(name, idx) => {

            let [one] = idx.as_slice() else {
                return None;
            };
            let (lo, hi) = eval_addr_range(one, for_stack, let_ranges, arrays)?;
            if hi < lo || hi - lo >= ARRAY_UNROLL_LIMIT {
                return None;
            }
            let mut acc: Option<(i32, i32)> = None;
            for k in lo..=hi {
                let (klo, khi) = arrays.get(name, Some(k))?;
                acc = Some(match acc {
                    None => (klo, khi),
                    Some((a, b)) => (a.min(klo), b.max(khi)),
                });
            }
            acc
        }
        Expr::Bin(op, l, r) => {
            let (lmin, lmax) = eval_addr_range(l, for_stack, let_ranges, arrays)?;
            let (rmin, rmax) = eval_addr_range(r, for_stack, let_ranges, arrays)?;
            match op {
                BinOp::Add => Some((lmin.checked_add(rmin)?, lmax.checked_add(rmax)?)),
                BinOp::Sub => Some((lmin.checked_sub(rmax)?, lmax.checked_sub(rmin)?)),
                BinOp::Mul => {
                    // Interval mul: enumerate corner products.
                    let candidates = [
                        lmin.checked_mul(rmin)?,
                        lmin.checked_mul(rmax)?,
                        lmax.checked_mul(rmin)?,
                        lmax.checked_mul(rmax)?,
                    ];
                    Some((
                        *candidates.iter().min().unwrap(),
                        *candidates.iter().max().unwrap(),
                    ))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

fn expr_reads_any_tracked(
    e: &ir::Expr,
    for_stack: &[(crate::ast::VarName, i32, i32)],
    let_ranges: &std::collections::HashMap<crate::ast::VarName, (i32, i32)>,
    arrays: &ArrayValueRanges,
) -> bool {
    use ir::Expr;
    match e {
        Expr::Var(v) => {
            for_stack.iter().any(|(name, _, _)| name == v) || let_ranges.contains_key(v)
        }
        Expr::Neg(inner) | Expr::Not(inner) | Expr::Peek(inner) => {
            expr_reads_any_tracked(inner, for_stack, let_ranges, arrays)
        }
        Expr::Bin(_, l, r) => {
            expr_reads_any_tracked(l, for_stack, let_ranges, arrays)
                || expr_reads_any_tracked(r, for_stack, let_ranges, arrays)
        }
        Expr::Func1(_, arg) => expr_reads_any_tracked(arg, for_stack, let_ranges, arrays),
        Expr::ArrayRef(name, idx) => {
            arrays.touches(name)
                || idx
                    .iter()
                    .any(|e| expr_reads_any_tracked(e, for_stack, let_ranges, arrays))
        }
        _ => false,
    }
}

/// Try to read `e` as a literal i32 in the auto-reserve window.
/// Used by FOR-bound tracking — both bounds and step in the IR are
/// `Expr`, but for the autoreserve range we only care about literal
/// integer endpoints.
fn as_i32_literal(e: &ir::Expr) -> Option<i32> {
    match e {
        ir::Expr::Number(n) if n.is_finite() && n.fract() == 0.0 => Some(*n as i32),
        ir::Expr::Neg(inner) => match inner.as_ref() {
            ir::Expr::Number(n) if n.is_finite() && n.fract() == 0.0 => Some(-(*n as i32)),
            _ => None,
        },
        _ => None,
    }
}

/// `POKE 53272, lit` ; `$D018` writes select the VIC memory pointer's screen + character base.
fn note_d018_charset_window(
    addr: &ir::Expr,
    value: &ir::Expr,
    ranges: &mut Vec<(u16, u16)>,
    bitmap: bool,
    bank: CharsetBank,
) {
    if bitmap {
        return;
    }

    let CharsetBank::Fixed(bank_base) = bank else {
        return;
    };
    // The address check ignores the auto-reserve window — $D018 is
    // outside it (I/O lives at $D000+, which `addr_literal_in_window`
    // intentionally rejects), but the WINDOW we compute from the
    // value lands in user RAM and must be reservable.
    let ir::Expr::Number(n) = addr else { return };
    if !n.is_finite() || n.fract() != 0.0 {
        return;
    }
    if (*n as i64) != 0xD018 {
        return;
    }

    let (v, known) = known_bits(value);
    if known & 0b0000_1110 != 0b0000_1110 {
        return;
    }
    let bits = (v >> 1) & 0b111;
    if bank_sees_character_rom(bank_base) && (bits == 0b010 || bits == 0b011) {
        return;
    }
    // `$C000 + 7 * $0800` = `$F800`, so this cannot overflow a u16.
    let base = bank_base + (bits as u16) * 0x0800;
    let end = base.saturating_add(0x07FF);
    if base >= AUTO_RESERVE_LO && end <= AUTO_RESERVE_HI {
        ranges.push((base, end));
    }
}

/// Does the VIC read character ROM instead of RAM at `$1000-$1FFF` within the bank based at `bank_base`?
fn bank_sees_character_rom(bank_base: u16) -> bool {
    bank_base == 0x0000 || bank_base == 0x8000
}

/// The stock screen's eight sprite pointers, `$07F8-$07FF`.
const SPRITE_PTR_LO: i64 = 0x07F8;
const SPRITE_PTR_HI: i64 = 0x07FF;
/// One sprite is 63 bytes of data in a 64-byte slot.
const SPRITE_BLOCK_BYTES: u16 = 64;

/// `POKE 2040, block` ; the fifth thing a BASIC program can declare it owns.
fn note_sprite_pointer_block(
    addr: &ir::Expr,
    value: &ir::Expr,
    ranges: &mut Vec<(u16, u16)>,
    bank: CharsetBank,
    for_stack: &[(crate::ast::VarName, i32, i32)],
    let_ranges: &std::collections::HashMap<crate::ast::VarName, (i32, i32)>,
    arrays: &ArrayValueRanges,
) {
    let CharsetBank::Fixed(bank_base) = bank else {
        return;
    };
    if bank_base != 0x0000 {
        return;
    }
    // Which pointer cells does this POKE write?
    let cells = match addr {
        ir::Expr::Number(n) if n.is_finite() && n.fract() == 0.0 => {
            let a = *n as i64;
            (a, a)
        }
        _ => match eval_addr_range(addr, for_stack, let_ranges, arrays) {
            Some((lo, hi)) => (i64::from(lo), i64::from(hi)),
            None => return,
        },
    };
    if cells.0 < SPRITE_PTR_LO || cells.1 > SPRITE_PTR_HI || cells.0 > cells.1 {
        return;
    }
    // Which blocks does it name?
    let (blk_lo, blk_hi) = {
        let (block, known) = known_bits(value);
        if known == 0xFF {
            (i64::from(block), i64::from(block))
        } else {
            match eval_addr_range(value, for_stack, let_ranges, arrays) {
                Some((lo, hi)) => (i64::from(lo), i64::from(hi)),
                None => return,
            }
        }
    };
    if blk_lo < 0 || blk_hi > 255 || blk_lo > blk_hi {
        return;
    }
    let base = bank_base + (blk_lo as u16) * SPRITE_BLOCK_BYTES;
    let end = bank_base + (blk_hi as u16) * SPRITE_BLOCK_BYTES + (SPRITE_BLOCK_BYTES - 1);
    if base >= AUTO_RESERVE_LO && end <= AUTO_RESERVE_HI {
        ranges.push((base, end));
    }
}

/// `$0288`, the page the KERNAL screen editor uses as screen RAM.
const SCREEN_PAGE_PTR: i64 = 0x0288;
/// A 40x25 text screen: 1000 bytes.
const TEXT_SCREEN_BYTES: u16 = 1000;

/// `POKE 648, page` ; `$0288` tells the KERNAL screen editor where screen RAM is.
fn note_screen_page_window(addr: &ir::Expr, value: &ir::Expr, ranges: &mut Vec<(u16, u16)>) {

    let ir::Expr::Number(n) = addr else { return };
    if !n.is_finite() || n.fract() != 0.0 || (*n as i64) != SCREEN_PAGE_PTR {
        return;
    }
    // The whole byte is the page number, so every bit has to be known.
    let (page, known) = known_bits(value);
    if known != 0xFF {
        return;
    }
    let base = (page as u16) << 8;
    let end = base.saturating_add(TEXT_SCREEN_BYTES - 1);
    if base >= AUTO_RESERVE_LO && end <= AUTO_RESERVE_HI {
        ranges.push((base, end));
    }
}

/// Bit-level constant propagation over BASIC's `AND`/`OR`, enough to read a register field out of a read-modify-write.
fn known_bits(e: &ir::Expr) -> (u8, u8) {
    use crate::ast::BinOp;
    if let Some(v) = as_i32_literal(e) {
        return (v as u8, 0xFF);
    }
    let ir::Expr::Bin(op, l, r) = e else {
        return (0, 0);
    };
    let (lv, lk) = known_bits(l);
    let (rv, rk) = known_bits(r);
    match op {
        // Known where both sides are known, or where either side is a known 0 ; a cleared bit does not care what it is ANDed with.
        BinOp::And => (lv & rv, (lk & rk) | (lk & !lv) | (rk & !rv)),
        // The dual: a known 1 forces the result whatever it meets.
        BinOp::Or => (lv | rv, (lk & rk) | (lk & lv) | (rk & rv)),

        BinOp::Add if ((lv | !lk) & (rv | !rk)) == 0 => (lv | rv, lk & rk),
        _ => (0, 0),
    }
}

/// Bitmap mode (`$D011` bit 5) repurposes CB13 as the base of an 8 KB bitmap, so the 2 KB character window `$D018` normally names does not exist.
fn module_sets_bitmap_mode(module: &ir::Module) -> bool {
    fn sets_it(stmt: &ir::Stmt) -> bool {
        use ir::Stmt;
        match stmt {
            Stmt::Poke { addr, value } => {
                literal_addr(addr) == Some(0xD011)
                    && as_i32_literal(value).is_some_and(|v| v & 0x20 != 0)
            }
            Stmt::If { then, .. } => then_sets_it(then),
            Stmt::IfElse {
                then, else_then, ..
            } => then_sets_it(then) || then_sets_it(else_then),
            Stmt::Rcomp { then, else_then } => {
                then_sets_it(then) || else_then.as_ref().is_some_and(then_sets_it)
            }
            _ => false,
        }
    }
    fn then_sets_it(then: &ir::ThenIr) -> bool {
        matches!(then, ir::ThenIr::Stmts(inner) if inner.iter().any(sets_it))
    }
    module.lines.iter().any(|l| l.stmts.iter().any(sets_it))
}

/// The base address of the 16 KB window the VIC addresses, when the whole module agrees on one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CharsetBank {
    /// Every bank write in the module resolves and they all name this base.
    Fixed(u16),

    Unresolved,
}

/// CIA 2 port A.
const CIA2_PORT_A: u16 = 0xDD00;
/// CIA 2 port A data direction.
const CIA2_DDR_A: u16 = 0xDD02;

/// Which 16 KB window `v` (a `$DD00` value) selects.
fn vic_bank_base(v: u8) -> u16 {
    (3 - (v & 0b11) as u16) * 0x4000
}

/// Counts and refusal reasons from [`resolve_vic_bank`], for `YAB_DEBUG_VICBANK`.
#[derive(Clone, Copy, Default, Debug)]
struct VicStats {
    /// `POKE $DD00` / `POKE $DD02` sites.
    bank_writes: u32,
    /// `POKE $D018` sites.
    d018_writes: u32,
    /// Two bank writes named two different banks.
    multi_bank: bool,
    /// A bank write's low two bits could not be read.
    opaque_bank: bool,
}

/// Resolve the VIC bank for the whole module, and count the sites that bear on it.
fn resolve_vic_bank(module: &ir::Module) -> (CharsetBank, VicStats) {
    struct St {
        bank: Option<u16>,
        stats: VicStats,
    }
    fn record(st: &mut St, base: u16) {
        match st.bank {
            None => st.bank = Some(base),
            Some(b) if b == base => {}
            Some(_) => st.stats.multi_bank = true,
        }
    }
    fn visit(stmt: &ir::Stmt, st: &mut St) {
        use ir::Stmt;
        match stmt {
            Stmt::Poke { addr, value } => match literal_addr(addr) {
                Some(CIA2_PORT_A) => {
                    st.stats.bank_writes += 1;
                    let (v, known) = known_bits(value);
                    if known & 0b11 == 0b11 {
                        record(st, vic_bank_base(v));
                    } else {
                        st.stats.opaque_bank = true;
                    }
                }
                Some(CIA2_DDR_A) => {
                    st.stats.bank_writes += 1;
                    // `POKE 56578, PEEK(56578) OR 3` ; the near-universal spelling ; proves both bits stay outputs, which leaves the bank exactly where `$DD00` put it.
                    let (v, known) = known_bits(value);
                    if known & 0b11 != 0b11 || v & 0b11 != 0b11 {
                        st.stats.opaque_bank = true;
                    }
                }
                Some(0xD018) => st.stats.d018_writes += 1,
                _ => {}
            },
            Stmt::If { then, .. } => visit_then(then, st),
            Stmt::IfElse {
                then, else_then, ..
            } => {
                visit_then(then, st);
                visit_then(else_then, st);
            }
            Stmt::Rcomp { then, else_then } => {
                visit_then(then, st);
                if let Some(e) = else_then {
                    visit_then(e, st);
                }
            }
            _ => {}
        }
    }
    fn visit_then(then: &ir::ThenIr, st: &mut St) {
        if let ir::ThenIr::Stmts(inner) = then {
            for s in inner {
                visit(s, st);
            }
        }
    }
    let mut st = St {
        bank: None,
        stats: VicStats::default(),
    };
    for line in &module.lines {
        for stmt in &line.stmts {
            visit(stmt, &mut st);
        }
    }
    let bank = if st.stats.multi_bank || st.stats.opaque_bank {
        CharsetBank::Unresolved
    } else {
        CharsetBank::Fixed(st.bank.unwrap_or(0x0000))
    };
    (bank, st.stats)
}

fn note_data_loader_extent(
    addr: &ir::Expr,
    value: &ir::Expr,
    ctx: &ReserveCtx,
    ranges: &mut Vec<(u16, u16)>,
) {
    if ctx.data_items == 0 {
        return;
    }
    // The value has to be a scalar the program READs.
    let ir::Expr::Var(v) = value else { return };
    if !ctx.read_targets.contains(v) {
        return;
    }
    let Some(base) = loader_base(addr, ctx) else {
        return;
    };
    if !(AUTO_RESERVE_LO..=AUTO_RESERVE_HI).contains(&base) {
        return;
    }
    let end = (base as u32)
        .saturating_add(ctx.data_items)
        .min(AUTO_RESERVE_HI as u32) as u16;
    ranges.push((base, end));
}

/// Reserve the destination of a POKE-loaded machine-code page copy.
fn note_ml_copy_destination(ctx: &ReserveCtx, ranges: &mut Vec<(u16, u16)>) {
    for run in &ctx.data_runs {
        ml_page_copy_destinations(run, ranges);
        ml_indexed_clear_destinations(run, ranges);
        ml_indexed_page_copy_destinations(run, ranges);
    }
}

/// Add the known destinations of a small indexed-clear kernel.
fn ml_indexed_clear_destinations(code: &[u8], ranges: &mut Vec<(u16, u16)>) {
    for i in 0..code.len().saturating_sub(10) {
        if code[i..i + 3] != [0xA9, 0x00, 0xAA] {
            continue;
        }
        let first = i + 3;
        let mut tail = first;
        while code.get(tail) == Some(&0x9D) && tail + 2 < code.len() {
            tail += 3;
        }
        let Some(end) = code.get(tail..tail + 5) else { continue };
        if tail == first || end[0] != 0xE8 || end[1] != 0xE0
            || end[2] == 0 || end[3] != 0xD0
            || (tail + 5) as isize + (end[4] as i8 as isize) != first as isize
        {
            continue;
        }
        for at in (first..tail).step_by(3) {
            let base = u16::from_le_bytes([code[at + 1], code[at + 2]]) as u32;
            let last = base + end[2] as u32 - 1;
            if base >= AUTO_RESERVE_LO as u32 && last <= AUTO_RESERVE_HI as u32 {
                ranges.push((base as u16, last as u16));
            }
        }
    }
}

fn ml_indexed_page_copy_destinations(code: &[u8], ranges: &mut Vec<(u16, u16)>) {
    for i in 0..code.len().saturating_sub(10) {
        let (load, store, step) = match code.get(i..i + 2) {
            Some([0xA0, 0x00]) => (0xB9, 0x99, 0xC8),
            Some([0xA2, 0x00]) => (0xBD, 0x9D, 0xE8),
            _ => continue,
        };
        let first = i + 2;
        let mut tail = first;
        while let Some(pair) = code.get(tail..tail + 6) {
            if pair[0] != load || pair[3] != store {
                break;
            }
            tail += 6;
        }
        let Some(end) = code.get(tail..tail + 3) else { continue };
        if tail == first || end[0] != step || end[1] != 0xD0
            || (tail + 3) as isize + (end[2] as i8 as isize) != first as isize
        {
            continue;
        }
        for at in (first..tail).step_by(6) {
            let base = u32::from(u16::from_le_bytes([code[at + 4], code[at + 5]]));
            let last = base + 255;
            if base >= u32::from(AUTO_RESERVE_LO) && last <= u32::from(AUTO_RESERVE_HI) {
                ranges.push((base as u16, last as u16));
            }
        }
    }
}

use crate::image_reach::track_immediates;

/// Reserve where BASIC's own block move ; `BLTU` ; is aimed.
fn ml_rom_block_move_destinations(code: &[u8], ranges: &mut Vec<(u16, u16)>) {
    /// `20 B8 A3` / `20 BF A3` ; `JSR BLTU` at either entry.
    const CALL: usize = 3;
    if code.len() < CALL {
        return;
    }
    for i in 0..=code.len() - CALL {
        if code[i] != 0x20 || code[i + 2] != 0xA3 || !matches!(code[i + 1], 0xB8 | 0xBF) {
            continue;
        }
        let st = track_immediates(&code[..i]);
        let word = |lo: u8| -> Option<u16> {
            Some(u16::from_le_bytes([
                st.zp.get(&lo).copied()?,
                st.zp.get(&(lo + 1)).copied()?,
            ]))
        };
        // Both pointer pairs have to be statically known.
        let (Some(dest_end), Some(src_end)) = (word(0x58), word(0x5A)) else {
            continue;
        };
        if dest_end <= src_end {
            continue;
        }
        let lo = dest_end - src_end;

        if lo >= AUTO_RESERVE_LO && dest_end <= AUTO_RESERVE_HI {
            ranges.push((lo, dest_end));
        }
    }
}

/// Reserve everywhere the machine code a `SYS` enters in the program's own image can store to.
fn note_sys_ml_write_extent(body: &[u8], module: &ir::Module, ranges: &mut Vec<(u16, u16)>) {
    let (load, last) = original_image_bounds(body);
    let Some(hi) = last.checked_add(1) else {
        return;
    };
    let entries = sys_entries_into_original_text(module, load, hi);
    if entries.is_empty() {
        return;
    }
    let Some(spans) = crate::image_reach::write_extent(body, load, &entries) else {
        return;
    };
    for (s, e) in spans {
        let lo = s.max(AUTO_RESERVE_LO);
        let hi = e.min(AUTO_RESERVE_HI);
        if lo <= hi {
            ranges.push((lo, hi));
        }
    }
}

/// Which way a matched kernel walks its two pointers, and how the loop is brought to a stop.
struct PageWalk {
    down: bool,

    pages: u8,
}

/// Decode the step-and-terminate tail that follows the copy kernel's `B1 zs 91 zd C8 D0 F9` head.
fn page_walk_tail(tail: &[u8], sh: u8, dh: u8, hi_of: impl Fn(u8) -> Option<u8>) -> Option<PageWalk> {
    // `<step> p <step> q`, one opcode for both pointers.
    if tail.len() < 4 {
        return None;
    }
    let (step, p, q) = (tail[0], tail[1], tail[3]);
    if tail[2] != step {
        return None;
    }
    let down = match step {
        0xE6 => false,
        0xC6 => true,
        _ => return None,
    };
    // The two steps must be exactly the two high bytes, in either order, since authors write them both ways.
    if !((p == sh && q == dh) || (p == dh && q == sh)) {
        return None;
    }
    // `CA D0 xx` ; a page count in X, supplied by the caller.
    if tail.len() >= 7 && tail[4] == 0xCA && tail[5] == 0xD0 {
        return Some(PageWalk { down, pages: 0 });
    }
    // `A5 zh C9 lim D0 xx` ; run until a walked high byte reaches `lim`.
    if tail.len() >= 10 && tail[4] == 0xA5 && tail[6] == 0xC9 && tail[8] == 0xD0 {
        let (zh, lim) = (tail[5], tail[7]);
        if zh != sh && zh != dh {
            return None;
        }
        let start = hi_of(zh)?;
        let pages = if down {
            start.checked_sub(lim)?
        } else {
            lim.checked_sub(start)?
        };
        if pages == 0 {
            return None;
        }
        return Some(PageWalk { down, pages });
    }
    None
}

/// Find every page-copy kernel in `code` and push the inclusive destination range each one fills.
fn ml_page_copy_destinations(code: &[u8], ranges: &mut Vec<(u16, u16)>) {
    ml_byte_pointer_copy_destinations(code, ranges);
    /// `B1 zs 91 zd C8 D0 F9` ; the part every variant shares.
    const HEAD: usize = 7;
    /// The shortest tail, `E6 p E6 q CA D0 xx`.
    const MIN_TAIL: usize = 7;
    if code.len() < HEAD + MIN_TAIL {
        return;
    }
    for i in 0..=code.len() - (HEAD + MIN_TAIL) {
        let w = &code[i..];
        if w[0] != 0xB1 || w[2] != 0x91 || w[4] != 0xC8 || w[5] != 0xD0 || w[6] != 0xF9 {
            continue;
        }
        let (zs, zd) = (w[1], w[3]);
        // A copy needs two distinct pointers.
        if zs == zd {
            continue;
        }
        let (sh, dh) = (zs.wrapping_add(1), zd.wrapping_add(1));
        let st = track_immediates(&code[..i]);
        let Some(walk) = page_walk_tail(&w[HEAD..], sh, dh, |z| st.zp.get(&z).copied()) else {
            continue;
        };
        // A count of 0 from the tail means "in X" ; whatever X holds when the loop is entered.
        let pages = if walk.pages == 0 {
            match st.x {
                Some(n) if n != 0 => n,
                _ => continue,
            }
        } else {
            walk.pages
        };
        let (Some(lo), Some(hi)) = (st.zp.get(&zd).copied(), st.zp.get(&dh).copied()) else {
            continue;
        };

        let reach = pages - 1;
        let (lo_hi, hi_hi) = if walk.down {
            (hi.checked_sub(reach), Some(hi))
        } else {
            (Some(hi), hi.checked_add(reach))
        };
        let (Some(lo_hi), Some(hi_hi)) = (lo_hi, hi_hi) else {
            // The walk runs off one end of memory.
            continue;
        };
        let base = u16::from_le_bytes([lo, lo_hi]) as u32;
        let end = u16::from_le_bytes([lo, hi_hi]) as u32 + 0xFF;
        if let Some(hook) = ml_page_copy_debug() {
            hook(walk.down, pages, base, end);
        }

        if base >= AUTO_RESERVE_LO as u32 && end <= AUTO_RESERVE_HI as u32 {
            ranges.push((base as u16, end as u16));
        }
    }
}

fn ml_byte_pointer_copy_destinations(code: &[u8], ranges: &mut Vec<(u16, u16)>) {
    const LEN: usize = 28;
    if code.len() < LEN {
        return;
    }
    for i in 0..=code.len() - LEN {
        let w = &code[i..i + LEN];
        if w[0] != 0xB1 || w[2] != 0x91 {
            continue;
        }
        let (src, dst) = (w[1], w[3]);
        let (Some(src_hi), Some(dst_hi)) = (src.checked_add(1), dst.checked_add(1)) else {
            continue;
        };
        let mut cells = [src, src_hi, dst, dst_hi];
        cells.sort_unstable();

        if cells[0] < 2 || cells.windows(2).any(|p| p[0] == p[1]) {
            continue;
        }
        if w[4..18] != [
            0xE6, src, 0xD0, 0x02, 0xE6, src_hi,
            0xE6, dst, 0xD0, 0x02, 0xE6, dst_hi, 0xA5, src,
        ] || w[18] != 0xC9
            || w[20..25] != [0xD0, 0xEA, 0xA5, src_hi, 0xC9]
            || w[26..28] != [0xD0, 0xE4]
        {
            continue;
        }
        let st = track_immediates(&code[..i]);
        if st.y != Some(0) {
            continue;
        }
        let (Some(&sl), Some(&sh), Some(&dl), Some(&dh)) = (
            st.zp.get(&src), st.zp.get(&src_hi), st.zp.get(&dst), st.zp.get(&dst_hi),
        ) else {
            continue;
        };
        let source = u16::from_le_bytes([sl, sh]);
        let limit = u16::from_le_bytes([w[19], w[25]]);
        let Some(count) = limit.checked_sub(source).filter(|n| *n != 0) else {
            // Refuse wrapping and zero-distance post-tested loops.
            continue;
        };
        let destination = u16::from_le_bytes([dl, dh]);
        let Some(last) = destination.checked_add(count - 1) else {
            continue;
        };
        if destination >= AUTO_RESERVE_LO && last <= AUTO_RESERVE_HI {
            ranges.push((destination, last));
        }
    }
}

/// Reserve the destination of a self-modifying absolute-indexed page copy.
fn ml_selfmod_page_copy_destinations(code: &[u8], load: u16, ranges: &mut Vec<(u16, u16)>) {

    const KERNEL: usize = 18;
    if code.len() < KERNEL {
        return;
    }
    for i in 0..=code.len() - KERNEL {
        let w = &code[i..];

        if w[0] != 0xB9 || w[3] != 0x99 || w[6] != 0xC8 || w[7] != 0xD0 || w[8] != 0xF7 {
            continue;
        }

        let step = w[9];
        if w[12] != step {
            continue;
        }
        let down = match step {
            0xEE => false,
            0xCE => true,
            _ => continue,
        };
        let Some(base_addr) = load.checked_add(i as u16) else {
            continue;
        };

        let (Some(src_hi_at), Some(dst_hi_at)) =
            (base_addr.checked_add(2), base_addr.checked_add(5))
        else {
            continue;
        };
        let p = u16::from_le_bytes([w[10], w[11]]);
        let q = u16::from_le_bytes([w[13], w[14]]);
        if !((p == src_hi_at && q == dst_hi_at) || (p == dst_hi_at && q == src_hi_at)) {
            continue;
        }
        // `CA D0 xx` ; a page count in X, and the branch has to return to the copy head.
        if w[15] != 0xCA || w[16] != 0xD0 {
            continue;
        }
        let back = (w[17] as i8) as i32;
        let target = i as i32 + KERNEL as i32 + back;
        if target > i as i32 || (i as i32 - target) > 3 {
            continue;
        }
        let st = track_immediates(&code[..i]);
        let pages = match st.x {
            Some(n) if n != 0 => n,
            _ => continue,
        };

        let (lo, hi) = (w[4], w[5]);
        let reach = pages - 1;
        let (lo_hi, hi_hi) = if down {
            (hi.checked_sub(reach), Some(hi))
        } else {
            (Some(hi), hi.checked_add(reach))
        };
        let (Some(lo_hi), Some(hi_hi)) = (lo_hi, hi_hi) else {
            continue;
        };
        let dest = u16::from_le_bytes([lo, lo_hi]) as u32;
        let end = u16::from_le_bytes([lo, hi_hi]) as u32 + 0xFF;
        if let Some(hook) = ml_page_copy_debug() {
            hook(down, pages, dest, end);
        }
        if dest >= AUTO_RESERVE_LO as u32 && end <= AUTO_RESERVE_HI as u32 {
            ranges.push((dest as u16, end as u16));
        }
    }
}

/// `YAB_DEBUG_PAGECOPY=1` reports every kernel this matcher resolves, before the window fence drops any of them.
fn ml_page_copy_debug() -> Option<impl Fn(bool, u8, u32, u32)> {
    std::env::var("YAB_DEBUG_PAGECOPY").ok().map(|_| {
        |down: bool, pages: u8, base: u32, end: u32| {
            eprintln!(
                "pagecopy: dir={} pages={pages} dest=${base:04X}-${end:04X} in_window={}",
                if down { "down" } else { "up" },
                base >= AUTO_RESERVE_LO as u32 && end <= AUTO_RESERVE_HI as u32
            );
        }
    })
}

/// Match `<literal> + <walking counter>` in either order and return the literal.
fn loader_base(addr: &ir::Expr, ctx: &ReserveCtx) -> Option<u16> {
    use crate::ast::BinOp;
    use ir::Expr;
    let Expr::Bin(BinOp::Add, l, r) = addr else {
        return None;
    };
    let walks = |e: &Expr| matches!(e, Expr::Var(v) if ctx.walking.contains(v));
    match (&**l, &**r) {
        (Expr::Number(_), other) if walks(other) => literal_addr(l),
        (other, Expr::Number(_)) if walks(other) => literal_addr(r),
        _ => None,
    }
}

fn note_addr_literal(addr: &ir::Expr, points: &mut std::collections::BTreeSet<u16>) {
    if let Some(a) = addr_literal_in_window(addr) {
        points.insert(a);
    }
}

/// Walk `e` looking for `PEEK(literal)` reads in the auto-reserve
/// window. PEEK targets a single byte, so we add the byte address
/// only — clusters of consecutive PEEKs (typical when reading a
/// table) coalesce in `merge_reserved_ranges`.
fn note_peek_in_expr(e: &ir::Expr, points: &mut std::collections::BTreeSet<u16>) {
    use ir::Expr;
    match e {
        Expr::Peek(addr) => {
            if let Some(a) = addr_literal_in_window(addr) {
                points.insert(a);
            }
            note_peek_in_expr(addr, points);
        }
        Expr::Neg(inner) | Expr::Not(inner) => note_peek_in_expr(inner, points),
        Expr::Bin(_, l, r) => {
            note_peek_in_expr(l, points);
            note_peek_in_expr(r, points);
        }
        Expr::Func1(_, arg) => note_peek_in_expr(arg, points),
        Expr::ArrayRef(_, idx) => {
            for e in idx {
                note_peek_in_expr(e, points);
            }
        }
        _ => {}
    }
}

fn addr_literal_in_window(e: &ir::Expr) -> Option<u16> {
    let ir::Expr::Number(n) = e else {
        return None;
    };
    if !n.is_finite() || n.fract() != 0.0 {
        return None;
    }
    let v = *n as i64;
    let lo = AUTO_RESERVE_LO as i64;
    let hi = AUTO_RESERVE_HI as i64;
    if (lo..=hi).contains(&v) {
        Some(v as u16)
    } else {
        None
    }
}

/// Sort, dedupe, and coalesce raw reserved ranges so the final list is friendly to `add_reserved_range`.
fn is_txttab_peek(e: &ir::Expr) -> bool {
    matches!(e, ir::Expr::Peek(addr)
        if matches!(addr.as_ref(), ir::Expr::Number(n)
            if n.is_finite() && (*n as i32 == 43 || *n as i32 == 44)))
}

fn module_reads_basic_txttab(module: &ir::Module) -> bool {
    use crate::visit::Visitor;
    struct Det {
        seen: bool,
    }
    impl Visitor for Det {
        fn visit_expr(&mut self, e: &ir::Expr) {
            if self.seen {
                return;
            }
            // `PEEK(43) <rel> <anything not derived from the pointer>`: the pointer byte is consumed by the comparison and goes no further.
            if let ir::Expr::Bin(op, a, b) = e
                && matches!(
                    op,
                    crate::ast::BinOp::Eq
                        | crate::ast::BinOp::Ne
                        | crate::ast::BinOp::Lt
                        | crate::ast::BinOp::Le
                        | crate::ast::BinOp::Gt
                        | crate::ast::BinOp::Ge
                )
                && (is_txttab_peek(a) || is_txttab_peek(b))
            {
                if !is_txttab_peek(a) {
                    self.visit_expr(a);
                }
                if !is_txttab_peek(b) {
                    self.visit_expr(b);
                }
                return;
            }
            if is_txttab_peek(e) {
                self.seen = true;
                return;
            }
            crate::visit::walk_expr(self, e);
        }
    }
    let mut d = Det { seen: false };
    crate::visit::walk_module(&mut d, module);
    d.seen
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TxttabShape {
    /// `PEEK(<expr containing PEEK(43|44)>)` ; the program reads a byte of its own text through the pointer.
    pub deref_read: usize,
    /// `POKE <expr containing PEEK(43|44)>, v` ; it writes one.
    pub deref_write: usize,
    /// The pointer is (part of) a `SYS` target: one address, and the reachability scan already bounds what the code there touches.
    pub sys_target: usize,
    /// Any other use ; assigned to a variable, printed, compared against something the exemption did not cover.
    pub other: usize,
}

impl TxttabShape {

    pub fn only_sys_targets(self) -> bool {
        self.sys_target > 0 && self.deref_read == 0 && self.deref_write == 0 && self.other == 0
    }
}

fn txttab_shape(module: &ir::Module) -> TxttabShape {
    use crate::visit::Visitor;

    /// Does any sub-expression read `$2B`/`$2C`?
    struct Has(bool);
    impl Visitor for Has {
        fn visit_expr(&mut self, e: &ir::Expr) {
            if is_txttab_peek(e) {
                self.0 = true;
                return;
            }
            crate::visit::walk_expr(self, e);
        }
    }
    fn has(e: &ir::Expr) -> bool {
        let mut h = Has(false);
        h.visit_expr(e);
        h.0
    }

    struct Scan {
        sh: TxttabShape,
        uses: usize,
        derefs: usize,
    }
    impl Visitor for Scan {
        fn visit_stmt(&mut self, line_no: u16, stmt: &ir::Stmt) {
            match stmt {
                ir::Stmt::Poke { addr, .. } | ir::Stmt::Dpoke { addr, .. } if has(addr) => {
                    self.sh.deref_write += 1
                }
                ir::Stmt::PokeFill {
                    dst_start, dst_end, ..
                } if has(dst_start) || has(dst_end) => self.sh.deref_write += 1,
                ir::Stmt::Sys { addr, .. } if has(addr) => self.sh.sys_target += 1,
                _ => {}
            }
            crate::visit::walk_stmt(self, line_no, stmt);
        }
        fn visit_expr(&mut self, e: &ir::Expr) {
            if is_txttab_peek(e) {
                self.uses += 1;
            } else if let ir::Expr::Peek(a) = e
                && has(a)
            {

                self.derefs += 1;
            }
            crate::visit::walk_expr(self, e);
        }
    }

    let mut s = Scan {
        sh: TxttabShape::default(),
        uses: 0,
        derefs: 0,
    };
    crate::visit::walk_module(&mut s, module);
    s.sh.deref_read = s.derefs;
    // Whatever is left over is unaccounted for.
    s.sh.other = s
        .uses
        .saturating_sub(s.sh.deref_read + s.sh.deref_write + s.sh.sys_target);
    s.sh
}

pub(crate) fn basic_text_end(prg_bytes: &[u8]) -> Option<u16> {
    if prg_bytes.len() < 4 {
        return None;
    }
    let load = u16::from_le_bytes([prg_bytes[0], prg_bytes[1]]);
    let mut p: usize = 2;

    for _ in 0..0x1_0000 {
        if p + 3 >= prg_bytes.len() {
            break;
        }
        let next = u16::from_le_bytes([prg_bytes[p], prg_bytes[p + 1]]);
        if next == 0 {
            break;
        }
        let np = (next as i64) - (load as i64) + 2;
        if np <= p as i64 || np > prg_bytes.len() as i64 {
            break;
        }
        p = np as usize;
    }
    u16::try_from(load as usize + p - 2).ok()
}

/// Every value a numeric variable is ever assigned from a literal, anywhere in the module.
fn literal_var_values(
    module: &ir::Module,
) -> std::collections::HashMap<crate::ast::VarName, Vec<f64>> {
    use crate::visit::Visitor;
    /// Cap per variable.
    const MAX_PER_VAR: usize = 8;
    struct Collect {
        map: std::collections::HashMap<crate::ast::VarName, Vec<f64>>,
    }
    fn literal_of(e: &ir::Expr) -> Option<f64> {
        match e {
            ir::Expr::Number(n) if n.is_finite() => Some(*n),
            ir::Expr::Neg(inner) => literal_of(inner).map(|v| -v),
            _ => None,
        }
    }
    impl Visitor for Collect {
        fn visit_stmt(&mut self, line_no: u16, stmt: &ir::Stmt) {
            if let ir::Stmt::Let { var, value } = stmt
                && let Some(v) = literal_of(value)
            {
                let slot = self.map.entry(var.clone()).or_default();
                if slot.len() < MAX_PER_VAR && !slot.contains(&v) {
                    slot.push(v);
                }
            }
            crate::visit::walk_stmt(self, line_no, stmt);
        }
    }
    let mut c = Collect {
        map: std::collections::HashMap::new(),
    };
    crate::visit::walk_module(&mut c, module);
    c.map
}

/// Every address a static expression could evaluate to, given the literal assignments `consts` records.
fn static_addr_candidates(
    e: &ir::Expr,
    consts: &std::collections::HashMap<crate::ast::VarName, Vec<f64>>,
    depth: u32,
) -> Vec<f64> {
    use crate::ast::BinOp;

    const MAX_CANDIDATES: usize = 32;
    if depth > 6 {
        return Vec::new();
    }
    match e {
        ir::Expr::Number(n) if n.is_finite() => vec![*n],
        ir::Expr::Var(v) => consts.get(v).cloned().unwrap_or_default(),
        ir::Expr::Neg(inner) => static_addr_candidates(inner, consts, depth + 1)
            .into_iter()
            .map(|v| -v)
            .collect(),
        ir::Expr::Bin(op @ (BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div), a, b) => {
            let (la, lb) = (
                static_addr_candidates(a, consts, depth + 1),
                static_addr_candidates(b, consts, depth + 1),
            );
            let mut out = Vec::new();
            for x in &la {
                for y in &lb {
                    if out.len() >= MAX_CANDIDATES {
                        return out;
                    }
                    let v = match op {
                        BinOp::Add => x + y,
                        BinOp::Sub => x - y,
                        BinOp::Mul => x * y,
                        BinOp::Div if *y != 0.0 => x / y,
                        _ => continue,
                    };
                    if v.is_finite() {
                        out.push(v);
                    }
                }
            }
            out
        }
        _ => Vec::new(),
    }
}

fn sys_entries_into_original_text(module: &ir::Module, lo: u16, hi: u16) -> Vec<u16> {
    use crate::visit::Visitor;
    let consts = literal_var_values(module);
    let in_range = |v: f64| v.is_finite() && v >= f64::from(lo) && v < f64::from(hi);
    struct Scan<'a> {
        consts: &'a std::collections::HashMap<crate::ast::VarName, Vec<f64>>,
        hit: bool,
        hits: Vec<u16>,
        in_range: &'a dyn Fn(f64) -> bool,
        /// `POKE 785,lo` / `POKE 786,hi` ; the USR vector at $0311/$0312.
        usr_lo: Vec<u8>,
        usr_hi: Vec<u8>,
        saw_usr: bool,
    }
    impl Scan<'_> {
        fn check(&mut self, e: &ir::Expr) {
            for v in static_addr_candidates(e, self.consts, 0) {
                if (self.in_range)(v) {
                    self.hit = true;
                    // `in_range` has already proved this is a u16.
                    self.hits.push(v as u16);
                }
            }
        }
    }
    impl Visitor for Scan<'_> {
        fn visit_stmt(&mut self, line_no: u16, stmt: &ir::Stmt) {
            match stmt {
                ir::Stmt::Sys { addr, .. } => self.check(addr),
                ir::Stmt::Poke { addr, value } => {
                    if let (Some(a), Some(v)) = (literal_addr(addr), literal_byte(value)) {
                        match a {
                            0x0311 => self.usr_lo.push(v),
                            0x0312 => self.usr_hi.push(v),
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            crate::visit::walk_stmt(self, line_no, stmt);
        }
        fn visit_expr(&mut self, e: &ir::Expr) {
            if let ir::Expr::Usr(_) = e {
                self.saw_usr = true;
            }
            crate::visit::walk_expr(self, e);
        }
    }
    let mut s = Scan {
        consts: &consts,
        hit: false,
        hits: Vec::new(),
        in_range: &in_range,
        usr_lo: Vec::new(),
        usr_hi: Vec::new(),
        saw_usr: false,
    };
    crate::visit::walk_module(&mut s, module);
    if s.saw_usr {
        for l in &s.usr_lo {
            for h in &s.usr_hi {
                let a = u16::from_le_bytes([*l, *h]);
                if in_range(f64::from(a)) {
                    s.hits.push(a);
                }
            }
        }
    }
    s.hits.sort_unstable();
    s.hits.dedup();
    s.hits
}

/// Original image address range that machine-code calls require unchanged.
fn verify_original_image_verbatim(prg_bytes: &[u8], image: InplaceImage<'_>) -> Result<(), String> {
    let body = image.body;
    let stored = prg_bytes.get(2..).unwrap_or(&[]);
    if stored.len() < body.len() {
        return Err(format!(
            "internal error: the original .prg image was to be materialised in place, but \
             the produced file is {} bytes where the source body alone is {}",
            stored.len(),
            body.len(),
        ));
    }

    for i in image.low_cut..body.len() {
        if stored[i] != body[i] {
            return Err(format!(
                "internal error: the original .prg image was to be materialised in place, but \
                 the byte for ${:04X} is ${:02X} where the source has ${:02X}. Some later pass \
                 moved the spliced block; every address the program SYSes into its own image \
                 is now wrong.",
                0x0801 + i,
                stored[i],
                body[i],
            ));
        }
    }
    if !stored
        .windows(INPLACE_HEAD)
        .any(|w| w == &body[..INPLACE_HEAD])
    {
        return Err(String::from(
            "internal error: the original .prg image was materialised in place, but the saved \
             copy of its first 15 bytes is not in the image, so the prologue cannot restore \
             $0801-$080F",
        ));
    }
    Ok(())
}

fn patch_low_trim_tail(
    machine_code: &mut Vec<u8>,
    origin: u16,
    image: InplaceImage<'_>,
) -> Result<usize, String> {

    if u32::from(origin) != CODE_ORIGIN {
        return Err(format!(
            "internal error: a low-trimmed image was built at origin ${origin:04X}, but the \
             layout is only defined for ${CODE_ORIGIN:04X}"
        ));
    }
    let tail = image.body.get(image.low_cut..).ok_or_else(|| {
        format!(
            "internal error: low trim cut at offset {} of a {}-byte image",
            image.low_cut,
            image.body.len(),
        )
    })?;
    let start = usize::from(image.tail_start()) - usize::from(origin);
    let end = start + tail.len();
    if end > 0x1_0000 - usize::from(origin) {
        return Err(String::from(
            "internal error: a low-trimmed image runs past the top of memory",
        ));
    }
    if machine_code.len() < end {

        machine_code.resize(end, 0);
    }
    if let Some(off) = machine_code[start..end].iter().position(|b| *b != 0) {
        return Err(format!(
            "low trim refused: ${:04X} holds ${:02X} where the reserved hole for the \
             original image's tail must be empty",
            start + off + usize::from(origin),
            machine_code[start + off],
        ));
    }
    machine_code[start..end].copy_from_slice(tail);
    Ok(end)
}

const BRIDGE_ROOM: u16 = 4;

/// Split `ranges` against the in-place window `[lo, hi]`.
fn clip_ranges_against_inplace_window(
    ranges: impl IntoIterator<Item = (u16, u16)>,
    lo: u16,
    hi: u16,
) -> (Vec<(u16, u16)>, Vec<(u16, u16)>) {
    let mut kept: Vec<(u16, u16)> = Vec::new();
    let mut dropped: Vec<(u16, u16)> = Vec::new();
    let resume = hi.checked_add(BRIDGE_ROOM);
    for (s, e) in ranges {
        // Wholly clear of the window on either side: untouched.
        if e < lo || resume.is_some_and(|r| s >= r) {
            kept.push((s, e));
            continue;
        }
        if s < lo {
            kept.push((s, lo - 1));
        }
        if let Some(resume) = resume
            && e >= resume
        {
            kept.push((resume, e));
        }

        let lost_lo = s.max(lo);
        let lost_hi = match resume {
            Some(resume) => e.min(resume - 1),

            None => e,
        };
        if lost_lo <= lost_hi {
            dropped.push((lost_lo, lost_hi));
        }
    }
    (kept, dropped)
}

/// Inclusive `[first, last]` addresses `body` occupies once loaded.
fn original_image_bounds(body: &[u8]) -> (u16, u16) {
    (
        0x0801,
        (0x0801 + body.len().saturating_sub(1).min(0xF7FE)) as u16,
    )
}

/// Why a build is materialising its original image in place.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InplaceTrigger {
    /// A `SYS` statically resolves into the original image.
    SysIntoImage,
    /// A DATA-loaded machine routine copies an appended source payload.
    CopiedPayload { destination: (u16, u16) },

    Txttab,
}

#[derive(Clone, Copy)]
pub(crate) struct InplaceImage<'a> {
    pub(crate) body: &'a [u8],
    pub(crate) trigger: InplaceTrigger,
    pub(crate) low_cut: usize,
}

impl<'a> InplaceImage<'a> {
    fn whole(body: &'a [u8], trigger: InplaceTrigger) -> Self {
        Self {
            body,
            trigger,
            low_cut: INPLACE_HEAD,
        }
    }

    /// The build is deleting dead BASIC text from the bottom of the image and putting compiled code there.
    fn is_low_trimmed(&self) -> bool {
        self.low_cut > INPLACE_HEAD
    }

    /// First address the surviving tail occupies.
    fn tail_start(&self) -> u16 {
        0x0801u16.saturating_add(self.low_cut as u16)
    }

    /// Inclusive last address of the image.
    fn image_last(&self) -> u16 {
        original_image_bounds(self.body).1
    }

    /// The address range the low trim frees for compiled code: everything between the head the launch needs and the tail.
    fn freed_span(&self) -> Option<(u16, u16)> {
        self.is_low_trimmed()
            .then(|| (0x0801 + INPLACE_HEAD as u16, self.tail_start() - 1))
    }
}

fn original_image_to_materialise<'a>(
    prg_bytes: &'a [u8],
    ast: &ast::Program,
    module: &ir::Module,
    options: &CompileOptions,
) -> Option<InplaceImage<'a>> {

    if options.custom_start_address.is_some() {
        return None;
    }
    if prg_bytes.len() < 2 {
        return None;
    }
    let load = u16::from_le_bytes([prg_bytes[0], prg_bytes[1]]);
    if load != 0x0801 {
        return None;
    }
    let body_len = prg_bytes.len() - 2;
    // The splice needs the 12-byte SYS stub plus the 3-byte jump over the blob to sit inside the original image.
    if body_len < INPLACE_HEAD {
        return None;
    }

    let text_end = basic_text_end(prg_bytes)?;
    if text_end <= load {
        return None;
    }
    let last = u16::try_from(load as usize + body_len - 1).ok()?;
    debug_assert_eq!(original_image_bounds(&prg_bytes[2..]), (load, last));

    let hi = if (last as u32) >= u32::from(text_end) + 2 {
        last.checked_add(1)?
    } else {
        text_end
    };
    let entries = sys_entries_into_original_text(module, load, hi);
    if entries.is_empty() {

        if module_reads_basic_txttab(module) {
            return Some(InplaceImage::whole(
                &prg_bytes[2..],
                InplaceTrigger::Txttab,
            ));
        }
        if let Some((start, destination)) = copied_payload_start(ast, text_end, last) {
            return Some(InplaceImage {
                body: &prg_bytes[2..],
                trigger: InplaceTrigger::CopiedPayload { destination },
                low_cut: usize::from(start - load),
            });
        }
        return None;
    }
    let body = &prg_bytes[2..];

    let has_appended_payload = (last as u32) >= u32::from(text_end) + 2;
    if has_appended_payload {
        // The payload has to stay at its own address, so nothing can be truncated off the top.
        return Some(InplaceImage {
            body,
            trigger: InplaceTrigger::SysIntoImage,
            low_cut: materialise_low_cut(body, load, text_end, &entries, module)
                .taken()
                .unwrap_or(INPLACE_HEAD),
        });
    }
    Some(InplaceImage::whole(
        match materialise_prefix_len(body, load, &entries, module) {
            Some(n) => &body[..n],
            None => body,
        },
        InplaceTrigger::SysIntoImage,
    ))
}

/// A cassette-buffer loader whose entire DATA stream is a known page copy.
fn copied_payload_start(
    program: &ast::Program,
    text_end: u16,
    image_last: u16,
) -> Option<(u16, (u16, u16))> {
    use ast::{Expr, ReadTarget, Statement};

    fn literal(expr: &Expr) -> Option<u16> {
        match expr {
            Expr::Number(n) if n.is_finite() && n.fract() == 0.0 && (0.0..=65535.0).contains(n) => {
                Some(*n as u16)
            }
            _ => None,
        }
    }

    // Keep the common path cheap: most programs do not contain this exact four-statement loader.
    let mut loaders = Vec::new();
    for (line_index, line) in program.lines.iter().enumerate() {
        let [
            Statement::For {
                var,
                start,
                end,
                step,
            },
            Statement::Read(targets),
            Statement::Poke {
                addr: Expr::Var(poke_var),
                value: Expr::Var(value_var),
            },
            Statement::Next { vars },
        ] = line.statements.as_slice()
        else {
            continue;
        };
        let [ReadTarget::Scalar(read_var)] = targets.as_slice() else {
            continue;
        };
        let [next_var] = vars.as_slice() else {
            continue;
        };
        let (Some(start), Some(end)) = (literal(start), literal(end)) else {
            continue;
        };
        if start < 0x033C
            || end > 0x03FF
            || end.checked_sub(start) != Some(18)
            || literal(step) != Some(1)
            || var != poke_var
            || read_var != value_var
            || read_var == var
            || next_var.as_ref().is_some_and(|next| next != var)
            || read_var.kind == ast::VarKind::String
        {
            continue;
        }
        loaders.push((line_index, start));
    }
    if loaders.is_empty() {
        return None;
    }

    // Count through nested statements too.
    #[derive(Default)]
    struct DataFacts {
        reads: usize,
        reset: bool,
        bytes: Vec<u8>,
        invalid: bool,
    }
    impl crate::visit::Visitor for DataFacts {
        fn visit_stmt(&mut self, line_no: u16, stmt: &ir::Stmt) {
            match stmt {
                ir::Stmt::Read(_) => self.reads += 1,
                ir::Stmt::Restore | ir::Stmt::Reset { .. } => self.reset = true,
                ir::Stmt::Data(values) => {
                    for value in values {
                        match value {
                            ast::DataValue::Float(n)
                                if n.is_finite()
                                    && n.fract() == 0.0
                                    && (0.0..=255.0).contains(n) =>
                            {
                                self.bytes.push(*n as u8);
                            }
                            _ => self.invalid = true,
                        }
                    }
                }
                _ => {}
            }
            crate::visit::walk_stmt(self, line_no, stmt);
        }
    }
    let mut facts = DataFacts::default();
    crate::visit::Visitor::visit_module(&mut facts, &ir::lower(program).ok()?);
    if facts.reads != 1 || facts.reset || facts.invalid || facts.bytes.len() != 19 {
        return None;
    }
    let code = facts.bytes.as_slice();
    let (pages, src_zp, dst_zp) = (code[1], code[5], code[7]);
    let (src_hi, dst_hi) = (src_zp.checked_add(1)?, dst_zp.checked_add(1)?);
    if pages == 0
        || src_zp.abs_diff(dst_zp) < 2
        || code
            != [
                0xA2, pages, 0xA0, 0x00, 0xB1, src_zp, 0x91, dst_zp, 0xC8, 0xD0, 0xF9, 0xE6,
                src_hi, 0xE6, dst_hi, 0xCA, 0xD0, 0xF0, 0x60,
            ]
    {
        return None;
    }

    for (loader_line, entry) in loaders {

        let call_line = program.lines[loader_line + 1..].iter().find(|line| {
            line.statements
                .iter()
                .any(|stmt| !matches!(stmt, Statement::Data(_)))
        });
        let Some(line) = call_line else { continue };
        let stmts = line.statements.as_slice();
        if stmts.len() != 5 && !(stmts.len() == 6 && matches!(stmts[5], Statement::Return)) {
            continue;
        }
        let mut values = [0u8; 4];
        let mut valid = true;
        for (index, addr) in [src_zp, src_hi, dst_zp, dst_hi].into_iter().enumerate() {
            match &stmts[index] {
                Statement::Poke {
                    addr: target,
                    value,
                } if literal(target) == Some(u16::from(addr)) => {
                    match literal(value).and_then(|value| u8::try_from(value).ok()) {
                        Some(value) => values[index] = value,
                        None => valid = false,
                    }
                }
                _ => valid = false,
            }
        }
        let Statement::Sys {
            addr, regs, params, ..
        } = &stmts[4]
        else {
            continue;
        };
        if !valid || literal(addr) != Some(entry) || !regs.is_empty() || !params.is_empty() {
            continue;
        }
        let source = u16::from_le_bytes([values[0], values[1]]);
        let dest = u16::from_le_bytes([values[2], values[3]]);
        let bytes = u16::from(pages) * 256;
        let (Some(source_end), Some(dest_end)) =
            (source.checked_add(bytes - 1), dest.checked_add(bytes - 1))
        else {
            continue;
        };

        if u32::from(source) >= u32::from(text_end) + 2
            && source_end <= image_last
            && dest >= 0xC000
            && dest_end <= 0xCFFF
            && dest > image_last
        {
            return Some((source, (dest, dest_end)));
        }
    }
    None
}

const MATERIALISE_SLACK: u16 = 0x0100;

const MATERIALISE_MIN_SAVING: usize = 2048;

fn materialise_prefix_len(
    body: &[u8],
    load: u16,
    entries: &[u16],
    module: &ir::Module,
) -> Option<usize> {
    let high = crate::image_reach::reachable_end(body, load, entries)?;
    materialise_prefix_len_from(body, load, high, module)
}

/// [`materialise_prefix_len`] with the reachability answer supplied.
fn materialise_prefix_len_from(
    body: &[u8],
    load: u16,
    high: u16,
    module: &ir::Module,
) -> Option<usize> {
    let mut high = high;

    let last = u16::try_from(load as usize + body.len() - 1).ok()?;
    for (lo, hi) in discover_reserved_ranges(module) {
        if hi >= load && lo <= last {
            high = high.max(hi.min(last));
        }
    }
    let want = u32::from(high) + u32::from(MATERIALISE_SLACK);
    let want = u16::try_from(want).unwrap_or(u16::MAX).min(last);

    let cut = basic_line_end_at_or_after(body, load, want).unwrap_or(want);
    let keep = (cut as usize).checked_sub(load as usize)? + 1;
    let keep = keep.min(body.len()).max(INPLACE_HEAD);
    (body.len().saturating_sub(keep) >= MATERIALISE_MIN_SAVING).then_some(keep)
}

/// Offset in `body` at which the surviving tail of a low-trimmed image begins, or `None` for "hand this program its whole image back".
fn materialise_low_cut(
    body: &[u8],
    load: u16,
    text_end: u16,
    entries: &[u16],
    module: &ir::Module,
) -> LowCut {
    materialise_low_cut_from(body, load, text_end, entries, module, |b, l, e| {
        crate::image_reach::reachable_span(b, l, e)
    })
}

fn materialise_low_cut_from(
    body: &[u8],
    load: u16,
    text_end: u16,
    entries: &[u16],
    module: &ir::Module,
    span: impl Fn(&[u8], u16, &[u16]) -> Option<(u16, u16)>,
) -> LowCut {
    // A program that walks its own text can read any byte of it.
    if module_reads_basic_txttab(module) {
        return LowCut::Refused("txttab");
    }
    let Some(&lowest) = entries.first() else {
        return LowCut::Refused("no-entry");
    };
    let Some(head_floor) = load.checked_add(INPLACE_HEAD as u16) else {
        return LowCut::Refused("head");
    };

    let Some(ceiling) = lowest
        .min(text_end)
        .checked_sub(MATERIALISE_SLACK)
        .filter(|c| *c >= load)
    else {
        return LowCut::Refused("slack");
    };
    let Some(mut cut) = basic_line_start_at_or_before(body, load, ceiling) else {
        return LowCut::Refused("line-chain");
    };
    // A literal PEEK or POKE into the span being deleted is reading or writing the blob, so pull the cut down under the lowest one.
    for (lo, hi) in discover_reserved_ranges(module) {
        if hi >= head_floor && lo < cut {
            cut = cut.min(lo);
        }
    }
    if cut <= head_floor {
        return LowCut::Refused("peek-poke");
    }
    // Pulling the cut down can land it in the middle of a line; round back to a start, and refuse if the chain will not answer.
    let Some(cut) = basic_line_start_at_or_before(body, load, cut) else {
        return LowCut::Refused("line-chain");
    };
    if cut <= head_floor {
        return LowCut::Refused("peek-poke");
    }

    let Some((low, _high)) = span(body, load, entries) else {
        return LowCut::Refused("scan-refused");
    };
    if low < cut {
        return LowCut::Refused("scan-reaches-down");
    }
    let Some(cut_off) = cut.checked_sub(load).map(usize::from) else {
        return LowCut::Refused("head");
    };
    if cut_off > body.len() {
        return LowCut::Refused("head");
    }
    match cut_off.checked_sub(INPLACE_HEAD) {
        Some(n) if n >= MATERIALISE_MIN_SAVING => LowCut::Take(cut_off),
        _ => LowCut::Refused("min-saving"),
    }
}

/// The low-trim decision, with the refusal named.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LowCut {
    /// Offset in the body at which the surviving tail begins.
    Take(usize),
    /// Which guard said no.
    Refused(&'static str),
}

impl LowCut {
    fn taken(self) -> Option<usize> {
        match self {
            LowCut::Take(n) => Some(n),
            LowCut::Refused(_) => None,
        }
    }

    pub fn reason(self) -> Option<&'static str> {
        match self {
            LowCut::Take(_) => None,
            LowCut::Refused(why) => Some(why),
        }
    }
}

fn basic_line_start_at_or_before(body: &[u8], load: u16, addr: u16) -> Option<u16> {
    if addr < load {
        return None;
    }
    let mut at = load;
    let mut best = None;
    for _ in 0..0x1_0000 {
        if at > addr {
            return best;
        }
        let off = usize::from(at.checked_sub(load)?);
        let link = u16::from_le_bytes([*body.get(off)?, *body.get(off + 1)?]);
        if link == 0 || link <= at {
            return None;
        }

        best = Some(at);
        at = link;
    }
    None
}

#[derive(Debug, Clone)]
pub struct LowTrimFacts {

    pub load: u16,

    pub body_len: usize,
    /// Last address the loaded image occupies, inclusive.
    pub image_last: u16,
    /// One past the last byte of the link chain.
    pub text_end: u16,

    pub has_appended_payload: bool,
    /// Which trigger, if any, materialises this image today.
    pub trigger: Option<InplaceTrigger>,
    /// Every entry point the compiler can resolve into the image.
    pub entries: Vec<u16>,
    /// Bytes the build embeds today.
    pub materialised_len: usize,

    pub reads_txttab: bool,
    /// Literal `PEEK`/`POKE` ranges that land inside the image.
    pub peek_poke_ranges: Vec<(u16, u16)>,
    /// `None` when the reachability scan refused, which means keep everything.
    pub reachable_end: Option<u16>,

    pub raw_dead_bytes: usize,

    pub structural_dead_bytes: usize,
    /// Where a low trim could cut to with every guard applied.
    pub guarded_cut: Option<u16>,
    /// Bytes between the head and `guarded_cut`, or 0 when a guard vetoed the trim.
    pub guarded_dead_bytes: usize,
    /// The reachability scan could not account for the code at the entry points.
    pub scan_refused: bool,
    /// The lowest image address the reachability walk names, or `None` when it refused.
    pub reachable_low: Option<u16>,

    pub decision: LowCut,

    pub refusals: Vec<crate::image_reach::ReachRefusal>,

    pub optimistic_span: Option<(u16, u16)>,

    pub hypo_prefix_saving: usize,
    /// The low-trim decision recomputed with `optimistic_span` in place of the scan's answer ; the build's own arithmetic, not a reconstruction of it.
    pub hypo_decision: LowCut,

    pub txttab: TxttabShape,
}

pub fn low_trim_facts(prg_bytes: &[u8], options: &CompileOptions) -> Option<LowTrimFacts> {
    let prg = prg::Program::parse(prg_bytes).ok()?;
    if !looks_like_basic(&prg) && !is_empty_basic_program(prg_bytes, &prg) {
        return None;
    }
    let hints = rem_hints::extract_hints_from(
        prg.lines.iter().map(|l| l.body.as_slice()),
        options.rem_hint_dialect,
    );
    let ast = parse::program_with_options(
        &prg,
        parse::ParseOptions {
            lenient_syntax: options.lenient_syntax,
            int_hint_vars: hints.int_vars.clone(),
            dialect: options.dialect,
            sys_register_args: options.sys_register_args,
        },
    )
    .ok()?;
    let module = lower_and_optimize(&ast, options.profile).ok()?;

    let load = u16::from_le_bytes([*prg_bytes.first()?, *prg_bytes.get(1)?]);
    if load != 0x0801 || prg_bytes.len() < 2 {
        return None;
    }
    let body = &prg_bytes[2..];
    let body_len = body.len();
    if body_len < INPLACE_HEAD {
        return None;
    }
    let text_end = basic_text_end(prg_bytes)?;
    if text_end <= load {
        return None;
    }
    let image_last = u16::try_from(load as usize + body_len - 1).ok()?;
    let has_appended_payload = (image_last as u32) >= u32::from(text_end) + 2;
    let hi = if has_appended_payload {
        image_last.checked_add(1)?
    } else {
        text_end
    };
    let entries = sys_entries_into_original_text(&module, load, hi);
    let entries_empty = entries.is_empty();
    let reads_txttab = module_reads_basic_txttab(&module);
    let trigger = if !entries.is_empty() {
        Some(InplaceTrigger::SysIntoImage)
    } else if reads_txttab {
        Some(InplaceTrigger::Txttab)
    } else {
        None
    };
    trigger?;
    let materialised_len = match trigger {
        Some(InplaceTrigger::Txttab) => body_len,
        _ if has_appended_payload => body_len,
        _ => materialise_prefix_len(body, load, &entries, &module).unwrap_or(body_len),
    };
    let reachable_end = if entries.is_empty() {
        None
    } else {
        crate::image_reach::reachable_end(body, load, &entries)
    };
    let peek_poke_ranges: Vec<(u16, u16)> = discover_reserved_ranges(&module)
        .into_iter()
        .filter(|(lo, hi)| *hi >= load && *lo <= image_last)
        .collect();

    let lowest = entries.first().copied();
    let raw_dead_bytes = match lowest {
        Some(l) => usize::from(l.min(text_end).saturating_sub(load)),
        None => 0,
    };

    let head_floor = load.checked_add(INPLACE_HEAD as u16)?;
    let structural_cut = lowest
        .and_then(|l| l.min(text_end).checked_sub(MATERIALISE_SLACK))
        .and_then(|ceiling| basic_line_start_at_or_before(body, load, ceiling))
        .filter(|cut| *cut > head_floor);
    let structural_dead_bytes = structural_cut
        .map(|c| usize::from(c - head_floor))
        .filter(|n| *n >= MATERIALISE_MIN_SAVING)
        .unwrap_or(0);

    let guarded_cut = (|| {
        if reads_txttab {
            return None;
        }
        let mut cut = structural_cut?;
        for (lo, hi) in &peek_poke_ranges {
            if *hi >= head_floor && *lo < cut {
                cut = cut.min(*lo);
            }
        }
        if cut <= head_floor {
            return None;
        }
        let cut = basic_line_start_at_or_before(body, load, cut)?;
        (cut > head_floor).then_some(cut)
    })();
    let guarded_dead_bytes = guarded_cut
        .map(|c| usize::from(c - head_floor))
        .filter(|n| *n >= MATERIALISE_MIN_SAVING)
        .unwrap_or(0);
    let reachable_low = if entries_empty {
        None
    } else {
        crate::image_reach::reachable_span(body, load, &entries).map(|(lo, _)| lo)
    };
    let decision = if has_appended_payload {
        materialise_low_cut(body, load, text_end, &entries, &module)
    } else {

        LowCut::Refused("prefix-family")
    };

    let diag = crate::image_reach::diagnose(body, load, &entries);
    let optimistic_span = diag.as_ref().map(|d| (d.low, d.high));
    let hypo_prefix_saving = match (has_appended_payload, optimistic_span) {
        (false, Some((_, high))) => materialise_prefix_len_from(body, load, high, &module)
            .map(|keep| body_len.saturating_sub(keep))
            .unwrap_or(0),
        _ => 0,
    };
    let hypo_decision = if has_appended_payload {
        materialise_low_cut_from(body, load, text_end, &entries, &module, |_, _, _| {
            optimistic_span
        })
    } else {
        LowCut::Refused("prefix-family")
    };
    let txttab = if reads_txttab {
        txttab_shape(&module)
    } else {
        TxttabShape::default()
    };

    Some(LowTrimFacts {
        load,
        body_len,
        image_last,
        text_end,
        has_appended_payload,
        trigger,
        entries,
        materialised_len,
        reads_txttab,
        peek_poke_ranges,
        scan_refused: reachable_end.is_none() && !entries_empty,
        reachable_end,
        raw_dead_bytes,
        structural_dead_bytes,
        guarded_cut,
        guarded_dead_bytes,
        reachable_low,
        decision,
        refusals: diag.map(|d| d.refusals).unwrap_or_default(),
        optimistic_span,
        hypo_prefix_saving,
        hypo_decision,
        txttab,
    })
}

fn basic_line_end_at_or_after(body: &[u8], load: u16, addr: u16) -> Option<u16> {
    let mut at = load;
    for _ in 0..0x1_0000 {
        let off = (at.checked_sub(load)?) as usize;
        let link = u16::from_le_bytes([*body.get(off)?, *body.get(off + 1)?]);
        if link == 0 || link <= at {
            return None;
        }

        let end = link - 1;
        if end >= addr {
            return Some(end);
        }
        at = link;
    }
    None
}

const INPLACE_HEAD: usize = 15;

pub(crate) const INPLACE_PIN_END: &str = "__BT_INPLACE_START:";

fn materialise_original_image(asm: &str, image: InplaceImage<'_>) -> String {
    let InplaceImage { body, trigger, .. } = image;
    if body.len() < INPLACE_HEAD {
        return asm.to_string();
    }
    let origin = "*=$080D\n";
    let Some(pos) = asm.find(origin) else {
        return asm.to_string();
    };
    let split = pos + origin.len();

    let mut out = String::with_capacity(asm.len() + body.len() * 5 + 1024);
    out.push_str(&asm[..split]);
    // Jump over the blob, then hand $0801..$080F back.
    out.push_str(
        "    JMP __BT_INPLACE_AFTER\n\
         __BT_INPLACE_BODY:\n",
    );
    if !image.is_low_trimmed() {
        push_byte_block(&mut out, &body[INPLACE_HEAD..]);
    }
    // `LDX #<head-1>` down to zero: the loop is written from `INPLACE_HEAD` so the two cannot drift apart.
    let last = INPLACE_HEAD - 1;

    let txtptr = match trigger {
        InplaceTrigger::Txttab => {
            "\x20   LDA #<__BT_INPLACE_EOT\n\
             \x20   STA $7A\n\
             \x20   LDA #>__BT_INPLACE_EOT\n\
             \x20   STA $7B\n"
        }
        InplaceTrigger::SysIntoImage | InplaceTrigger::CopiedPayload { .. } => "",
    };
    let eot = match trigger {
        InplaceTrigger::Txttab => "__BT_INPLACE_EOT:\n\x20   .byte $00,$00,$00\n",
        InplaceTrigger::SysIntoImage | InplaceTrigger::CopiedPayload { .. } => "",
    };
    out.push_str(&format!(
        "__BT_INPLACE_AFTER:\n\
         \x20   LDX #${last:02X}\n\
         __BT_INPLACE_RESTORE:\n\
         \x20   LDA __BT_INPLACE_HEAD,X\n\
         \x20   STA $0801,X\n\
         \x20   DEX\n\
         \x20   BPL __BT_INPLACE_RESTORE\n\
         \x20   LDA #$01\n\
         \x20   STA $2B\n\
         \x20   LDA #$08\n\
         \x20   STA $2C\n\
         {txtptr}\
         \x20   JMP __BT_INPLACE_START\n\
         {eot}\
         __BT_INPLACE_HEAD:\n"
    ));
    push_byte_block(&mut out, &body[..INPLACE_HEAD]);
    out.push_str(INPLACE_PIN_END);
    out.push('\n');
    out.push_str(&asm[split..]);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Emit `bytes` as `.byte` directives, 16 per line.
fn push_byte_block(out: &mut String, bytes: &[u8]) {
    use std::fmt::Write as _;
    for chunk in bytes.chunks(16) {
        out.push_str("    .byte ");
        for (i, b) in chunk.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let _ = write!(out, "${b:02X}");
        }
        out.push('\n');
    }
}

/// Runtime landing pad for the relocated copy of the original BASIC text.
const BASIC_TEXT_PAD: u16 = 0x8001;

/// Last byte of always-visible RAM.
const ALWAYS_RAM_TOP: u16 = 0x9FFF;

const BASIC_TEXT_COPY_LEN_MARKER: &str = "; __BASIC_TEXT_COPY_LEN=";

/// Splice a copy of the original BASIC text into the assembly and stamp its address into $2B/$2C at program start.
fn embed_original_basic_text(asm: &str, prg_bytes: &[u8]) -> String {
    if prg_bytes.len() < 2 {
        return asm.to_string();
    }
    let body = &prg_bytes[2..];
    let len = body.len();
    let pages = (len / 256) as u8;
    let remainder = (len % 256) as u8;

    let pad_lo = (BASIC_TEXT_PAD & 0xFF) as u8;
    let pad_hi = (BASIC_TEXT_PAD >> 8) as u8;
    let prelude = format!(
        "    LDA #${pad_lo:02X}\n    STA $2B\n    LDA #${pad_hi:02X}\n    STA $2C\n\
         \n    LDA #<__BASIC_TEXT_SRC\n    STA $FB\n    \
         LDA #>__BASIC_TEXT_SRC\n    STA $FC\n    \
         LDA #${pad_lo:02X}\n    STA $FD\n    LDA #${pad_hi:02X}\n    STA $FE\n"
    );
    let mut copy_loop = String::new();
    if pages > 0 {
        copy_loop.push_str(&format!(
            "    LDX #${pages:02X}\n\
             __BTORIG_PAGE_LOOP:\n    \
             LDY #$00\n\
             __BTORIG_INNER:\n    \
             LDA ($FB),Y\n    STA ($FD),Y\n    INY\n    \
             BNE __BTORIG_INNER\n    INC $FC\n    INC $FE\n    \
             DEX\n    BNE __BTORIG_PAGE_LOOP\n"
        ));
    }
    if remainder > 0 {
        copy_loop.push_str(&format!(
            "    LDY #$00\n\
             __BTORIG_REM_LOOP:\n    \
             CPY #${remainder:02X}\n    BEQ __BTORIG_DONE\n    \
             LDA ($FB),Y\n    STA ($FD),Y\n    INY\n    \
             JMP __BTORIG_REM_LOOP\n\
             __BTORIG_DONE:\n"
        ));
    }
    let origin = "*=$080D\n";
    let Some(pos) = asm.find(origin) else {
        return asm.to_string();
    };
    let split = pos + origin.len();
    let mut out = String::with_capacity(asm.len() + body.len() * 6 + 1024);
    out.push_str(&asm[..split]);
    // The extent of the start-up copy, in the assembly, for `verify_embedded_text_pad` to read back.
    out.push_str(&format!("{BASIC_TEXT_COPY_LEN_MARKER}{len}\n"));
    out.push_str(&prelude);
    out.push_str(&copy_loop);
    out.push_str(&asm[split..]);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("\n__BASIC_TEXT_SRC:\n");
    for chunk in body.chunks(16) {
        out.push_str("    .byte ");
        for (i, b) in chunk.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&format!("${b:02X}"));
        }
        out.push('\n');
    }
    out
}

/// The half of the text-copy check that needs nothing but the length of the source file: does the copy leave always-RAM?
fn basic_text_copy_ram_overrun(len: usize) -> Option<String> {
    let pad = u32::from(BASIC_TEXT_PAD);
    let last = pad + len as u32 - 1;
    if len == 0 || last <= u32::from(ALWAYS_RAM_TOP) {
        return None;
    }
    Some(format!(
        "?OUT OF MEMORY: this program reads BASIC's text pointer at $2B/$2C, so the \
         compiled image carries a copy of the original {len}-byte BASIC text and relocates \
         it to ${pad:04X} at start-up — but ${pad:04X} + {len} runs to ${last:04X}, past \
         the ${ALWAYS_RAM_TOP:04X} top of always-RAM. The copy would overwrite the \
         VIC/SID/CIA registers at $D000-$DFFF. Shrink the program, or compile the part \
         that uses `PEEK(43)`/`PEEK(44)` separately."
    ))
}

fn verify_embedded_text_pad(asm: &str, origin: u16, image_len: usize) -> Result<(), String> {
    let marked = asm.lines().find_map(|l| {
        l.trim_end()
            .strip_prefix(BASIC_TEXT_COPY_LEN_MARKER)
            .and_then(|n| n.trim().parse::<usize>().ok())
    });
    let Some(len) = marked else {
        // No copy in this image is the common case and fine.
        if asm.contains("__BASIC_TEXT_SRC:") {
            return Err("internal error: the compiled image carries a copy of the original \
                        BASIC text, but the prologue's `__BASIC_TEXT_COPY_LEN` marker is not \
                        in the generated asm, so the start-up copy's extent cannot be checked"
                .to_string());
        }
        return Ok(());
    };
    if len == 0 {
        return Ok(());
    }
    if let Some(message) = basic_text_copy_ram_overrun(len) {
        return Err(message);
    }
    let pad = u32::from(BASIC_TEXT_PAD);
    let image_end = u32::from(origin) + image_len as u32 - 1;
    if image_len > 0 && image_end >= pad {
        let over = image_end - pad + 1;
        return Err(format!(
            "?OUT OF MEMORY: this program reads BASIC's text pointer at $2B/$2C, so the \
             compiled image relocates a copy of the original BASIC text to ${pad:04X} \
             before the first statement runs, but the image itself spans ${origin:04X}-\
             ${image_end:04X} and reaches into that window. The copy would overwrite {over} \
             byte(s) of the program's own code and data. Shrink the program by about {over} \
             bytes, or compile the part that uses `PEEK(43)`/`PEEK(44)` separately.",
        ));
    }
    Ok(())
}

fn merge_reserved_ranges(mut input: Vec<(u16, u16)>) -> Vec<(u16, u16)> {
    if input.is_empty() {
        return input;
    }
    // Normalise: ensure start <= end, drop nonsensical entries.
    input.retain_mut(|(s, e)| {
        if *s > *e {
            std::mem::swap(s, e);
        }
        true
    });
    input.sort_by_key(|(s, _)| *s);
    let mut out: Vec<(u16, u16)> = Vec::with_capacity(input.len());
    for (s, e) in input {
        if let Some(last) = out.last_mut() {
            // Merge if the new range overlaps or is within 3 bytes
            // of the previous one (asm6502 needs >=3 bytes of gap
            // for the inter-range JMP).
            let gap_start = last.1.saturating_add(1);
            let gap_needed: u32 = 4;
            if (s as u32) <= (gap_start as u32).saturating_add(gap_needed - 1) {
                last.1 = last.1.max(e);
                continue;
            }
        }
        out.push((s, e));
    }
    out
}

#[cfg(test)]
#[path = "compile/dim_heap_size_choice_tests.rs"]
mod dim_heap_size_choice_tests;

#[cfg(test)]
#[path = "compile/string_heap_layout_choice_tests.rs"]
mod string_heap_layout_choice_tests;

#[cfg(test)]
#[path = "compile/packed_print_pointer_tests.rs"]
mod packed_print_pointer_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_retry_fits_after_packed_print_refusal_and_preserves_output() {
        let mut source = String::new();
        for i in 0..24u8 {
            let letter = char::from(b'A' + i);
            source.push_str(&format!("{} PRINT \"{}AB{letter}\";\n", 10 + u32::from(i) * 10, letter.to_string().repeat(177)));
        }
        source.push_str("500 END\n");
        let input = crate::tokenize_program(&source).unwrap();
        for extraram in [false, true] {
            let options = CompileOptions { profile: Profile::Size, extraram, force_extraram_off: !extraram, auto_reserve: false, ..CompileOptions::default() };
            let ordinary = compile_at_profile(&input, options.clone()).unwrap();
            let packed = compile_at_profile_rescue(&input, options.clone(), LayoutRescue::PackedPrint).unwrap();
            let compact = compile_at_profile_rescue(&input, options.clone(), LayoutRescue::Compact).unwrap();
            assert!(compact.asm.contains("__CP_COUNTS:"));
            assert!(compact.diagnostics.end_address < packed.diagnostics.end_address);
            let tight = CompileOptions { reserved_ranges: vec![(compact.diagnostics.end_address + 8, 0xcfff)], ..options };
            assert!(compile_at_profile_rescue(&input, tight.clone(), LayoutRescue::PackedPrint).is_err());
            let rescued = compile_with_options(&input, tight).unwrap();
            assert!(rescued.asm.contains("__CP_COUNTS:"), "must reach the final retry");
            let raw = emu64::run_prg_to_end(&ordinary.prg_bytes, 10_000_000).unwrap();
            let result = emu64::run_prg_to_end(&rescued.prg_bytes, 10_000_000).unwrap();
            assert!(raw.clean_exit && result.clean_exit);
            assert_eq!(raw.output, result.output);
            // Existing successful admission returns before Compact.
            let unchanged = compile_with_options(&input, CompileOptions { profile: Profile::Size, extraram, force_extraram_off: !extraram, auto_reserve: false, ..CompileOptions::default() }).unwrap();
            assert_eq!(unchanged.prg_bytes, ordinary.prg_bytes);
        }
    }



    #[test]
    fn data_first_reorder_keeps_whole_pool_and_all_other_bytes() {
        let source = "*=$080D\n    RTS\n; --- string pool (length-prefixed) ---\n\
            S0:\n    .byte $01,$41\n__VARS_START:\nV_A:\n    .byte $00,$00\n__VARS_END:\n\
            __DATA:\n__DATA_LINE_10:\n    .byte $01,$31 ; numeric READ\n\
            __DBAD_S1:\n    .byte $01,$58\n__DBAD_E1:\n__DATA_LINE_20:\n    .byte $01,$32\n\
            __DATA_END:\n__DATA_BAD:\n    .word __DBAD_S1,__DBAD_E1,10\n__HEAP_BOTTOM:\n";
        let reordered = data_before_constants(source).unwrap();
        assert!(reordered.find("__DATA:\n").unwrap() < reordered.find("S0:\n").unwrap());
        let mut before = Assembler6502::new();
        let mut after = Assembler6502::new();
        let old = before.assemble_bytes(source).unwrap();
        let new = after.assemble_bytes(&reordered).unwrap();
        for (start, end) in [("__DATA", "__DATA_END"), ("__VARS_START", "__VARS_END")] {
            let span = |a: &Assembler6502| usize::from(a.lookup(start).unwrap() - 0x080d)..usize::from(a.lookup(end).unwrap() - 0x080d);
            assert_eq!(&old[span(&before)], &new[span(&after)], "{start} remains contiguous");
        }
        assert_eq!(old.len(), new.len());
        assert!(data_before_constants(&reordered).is_none(), "already reordered");
    }

    #[test]
    fn data_first_reorder_refuses_non_pool_content() {
        let source = "*=$080D\n    RTS\n; --- string pool (length-prefixed) ---\n\
            S0:\n    .byte $01,$41\n__DATA:\n__DATA_LINE_10:\n    .byte $01,$31\n__DATA_END:\n__HEAP_BOTTOM:\n";
        for foreign in ["    JSR helper\n", "__BSS_START:\n", "foreign:\n", "    .word S0\n", "    .byte <S0\n", "    .byte $100\n"] {
            let malformed = source.replace("__DATA_END:\n", &format!("{foreign}__DATA_END:\n"));
            assert!(data_before_constants(&malformed).is_none(), "{foreign}");
        }
        assert!(data_before_constants(&source.replace("__DATA_END:\n", "")).is_none());
        assert!(data_before_constants(&source.replace("__DATA_LINE_10:", "__DATA:" )).is_none());
    }

    #[test]
    fn data_first_reorder_heap_padding_is_real_and_terminal() {
        let source = "*=$BFFE\n    RTS\n__HEAP_BOTTOM:\n; tail marker\n";
        let padded = pad_terminal_heap(source, 1).unwrap();
        let mut a = Assembler6502::new();
        let bytes = a.assemble_bytes(&padded).unwrap();
        assert_eq!(bytes, [0x60, 0]);
        assert_eq!(a.lookup("__HEAP_BOTTOM"), Some(0xc000));
        assert!(pad_terminal_heap(&source.replace("; tail marker", "    RTS"), 1).is_none());
        assert!(pad_terminal_heap("*=$BFFE\n    RTS\n", 1).is_none());
    }

    #[test]
    fn data_first_reorder_is_checked_by_the_normal_reserved_linker() {
        let source = "10 A$=\"THE STRING POOL MUST STAY VALID\":PRINT A$\n\
            20 READ A,B$,C:PRINT A;B$;C:RESTORE 100:READ D:PRINT D:END\n\
            100 DATA 123,HELLO,456,123456789,123456789,123456789,123456789,123456789\n";
        let prg = crate::tokenize_program(source).unwrap();
        let options = CompileOptions { profile: Profile::Size, extraram: true, ..CompileOptions::default() };
        let ordinary = compile_at_profile_rescue(&prg, options.clone(), LayoutRescue::Compact).unwrap();
        let mut assembler = Assembler6502::new();
        assembler.assemble_bytes(&ordinary.asm).unwrap();
        let middle = (u32::from(assembler.lookup("__DATA").unwrap()) + u32::from(assembler.lookup("__DATA_END").unwrap())) / 2;
        let options = CompileOptions { reserved_ranges: vec![(middle as u16, middle as u16)], ..options };
        let compact = compile_at_profile_rescue(&prg, options.clone(), LayoutRescue::Compact).unwrap();
        let first = compile_at_profile_rescue(&prg, options.clone(), LayoutRescue::DataFirst).unwrap();
        assert!(first.prg_bytes.len() < compact.prg_bytes.len(), "first={} compact={}", first.prg_bytes.len(), compact.prg_bytes.len());
        // Return the first successful ordinary layout.
        let public = compile_with_options(&prg, options.clone()).unwrap();
        let prior = compile_at_profile_rescue(&prg, options, LayoutRescue::Ordinary).unwrap();
        assert_eq!(public.prg_bytes, prior.prg_bytes);
    }

    fn dual_val_asm() -> String {
        format!(
            "__VAL_HELPER:\n    LDY #$00\n{}\n    STA __VAL_BUF-1,Y\n{}\n{}\n    INC $22\n{}\n\
             {}\n__VAL_BUF:\n    .byte $00\n{}\n",
            codegen::VAL_COPY_OPEN,
            codegen::VAL_COPY_CLOSE,
            codegen::VAL_INPLACE_OPEN,
            codegen::VAL_INPLACE_CLOSE,
            codegen::VAL_BUF_OPEN,
            codegen::VAL_BUF_CLOSE,
        )
    }

    /// The peephole must never be shown asm that still contains both `__VAL_HELPER` bodies.
    #[test]
    fn the_peephole_never_sees_both_val_bodies() {
        let seen = std::cell::RefCell::new(Vec::new());
        let (default, copy) = resolve_val_variants(&dual_val_asm(), |a| {
            seen.borrow_mut().push(a.to_string());
            a.to_string()
        });
        let seen = seen.into_inner();
        assert_eq!(seen.len(), 2, "both variants should be peepholed, separately");
        for (i, s) in seen.iter().enumerate() {
            assert!(
                !s.contains("@@VAL_"),
                "the peephole was handed asm still carrying a variant sentinel (call {i}):\n{s}"
            );
            let both = s.contains("STA __VAL_BUF-1,Y") && s.contains("    INC $22\n");
            assert!(!both, "the peephole was handed BOTH __VAL_HELPER bodies (call {i}):\n{s}");
        }

        assert!(
            default.contains("    INC $22\n") && !default.contains("__VAL_BUF"),
            "default is not the in-place body:\n{default}"
        );
        let copy = copy.expect("a dual asm yields a copy variant");
        assert!(
            copy.contains("STA __VAL_BUF-1,Y") && copy.contains("__VAL_BUF:"),
            "copy variant lost its buffer:\n{copy}"
        );
        assert!(
            !copy.contains("    INC $22\n"),
            "copy variant still carries the in-place body:\n{copy}"
        );
    }

    /// A program with no heap-capable `VAL` has no variants, gets no second peephole run, and is handed straight through.
    #[test]
    fn asm_without_val_variants_is_untouched_and_peepholed_once() {
        let plain = "__VAL_HELPER:\n    LDY #$00\n    INC $22\n";
        let calls = std::cell::Cell::new(0);
        let (default, copy) = resolve_val_variants(plain, |a| {
            calls.set(calls.get() + 1);
            a.to_string()
        });
        assert_eq!(calls.get(), 1, "a program with no variants must be peepholed once");
        assert_eq!(default, plain, "asm without sentinels must pass through unchanged");
        assert!(copy.is_none(), "no variants means no copy build to carry");
    }

    #[test]
    fn a_subtracted_literal_too_large_to_negate_is_refused_not_wrapped() {
        assert_eq!(
            (-1.0e19_f64) as i64,
            i64::MIN,
            "the fixture only bites if the saturating cast reaches i64::MIN"
        );
        let v = ast::VarName {
            base: "R".to_string(),
            kind: ast::VarKind::Float,
        };
        let value = ir::Expr::Bin(
            ast::BinOp::Sub,
            Box::new(ir::Expr::Var(v.clone())),
            Box::new(ir::Expr::Number(-1.0e19)),
        );
        assert_eq!(
            const_self_stride(&value, &v),
            None,
            "negating i64::MIN has no result, so the stride must be refused"
        );
    }

    #[test]
    fn an_ordinary_subtracted_literal_is_still_a_negative_stride() {
        let v = ast::VarName {
            base: "R".to_string(),
            kind: ast::VarKind::Float,
        };
        let value = ir::Expr::Bin(
            ast::BinOp::Sub,
            Box::new(ir::Expr::Var(v.clone())),
            Box::new(ir::Expr::Number(8.0)),
        );
        assert_eq!(const_self_stride(&value, &v), Some(-8));
    }
    /// Build a minimal tokenised PRG with `10 PRINT 1`. Just enough
    /// for the parser to accept and codegen to produce real output —
    /// the compiled binary is tiny (well under the auto-trigger).
    fn tiny_prg() -> Vec<u8> {
        // layout: load=$0801, next=$080B, line=10, body=PRINT 1, terminator,
        // EOP=$0000. PRINT token = $99.
        vec![
            0x01, 0x08, // load address
            0x09, 0x08, // next-line link -> $0809 (the EOP marker)
            0x0A, 0x00, // line 10
            0x99, 0x20, 0x31, // PRINT space "1"
            0x00, // line terminator
            0x00, 0x00, // end-of-program
        ]
    }

    #[test]
    fn a_range_wholly_below_the_window_is_untouched() {

        let (kept, dropped) = clip_ranges_against_inplace_window([(0x0400, 0x07FF)], 0x0801, 0x1000);
        assert_eq!(kept, vec![(0x0400, 0x07FF)]);
        assert!(dropped.is_empty(), "nothing overlaps, nothing may be lost");
    }

    #[test]
    fn a_range_wholly_above_the_window_is_untouched() {

        let (kept, dropped) = clip_ranges_against_inplace_window([(0x2000, 0x2010)], 0x0801, 0x1000);
        assert_eq!(kept, vec![(0x2000, 0x2010)]);
        assert!(dropped.is_empty());
    }

    #[test]
    fn a_range_just_above_the_window_survives_the_cushion() {
        // BRIDGE_ROOM is 4, so $1004 is the first address that can be reserved.
        let (kept, dropped) = clip_ranges_against_inplace_window([(0x1004, 0x1010)], 0x0801, 0x1000);
        assert_eq!(kept, vec![(0x1004, 0x1010)]);
        assert!(dropped.is_empty());

        let (kept, dropped) = clip_ranges_against_inplace_window([(0x1003, 0x1003)], 0x0801, 0x1000);
        assert!(
            kept.is_empty(),
            "$1003 is inside the bridge cushion and cannot be reserved"
        );
        assert_eq!(dropped, vec![(0x1003, 0x1003)]);
    }

    #[test]
    fn a_range_entirely_in_the_cushion_is_dropped_though_it_clears_the_blob() {

        let (kept, dropped) = clip_ranges_against_inplace_window([(0x1001, 0x1002)], 0x0801, 0x1000);
        assert!(kept.is_empty());
        assert_eq!(dropped, vec![(0x1001, 0x1002)]);
    }

    #[test]
    fn a_range_wholly_inside_the_window_is_dropped() {
        let (kept, dropped) = clip_ranges_against_inplace_window([(0x0900, 0x0910)], 0x0801, 0x1000);
        assert!(kept.is_empty(), "the assembler must be told nothing");
        assert_eq!(
            dropped,
            vec![(0x0900, 0x0910)],
            "and the caller must be told exactly what was given up"
        );
    }

    #[test]
    fn a_range_straddling_the_top_keeps_its_upper_half() {
        // `Vier op Een Rij` in miniature.
        let (kept, dropped) = clip_ranges_against_inplace_window([(0x0FF0, 0x1100)], 0x0801, 0x1000);
        assert_eq!(kept, vec![(0x1004, 0x1100)]);
        assert_eq!(dropped, vec![(0x0FF0, 0x1003)]);
    }

    #[test]
    fn a_range_straddling_the_bottom_keeps_its_lower_half() {

        let (kept, dropped) = clip_ranges_against_inplace_window([(0x0700, 0x0810)], 0x0801, 0x1000);
        assert_eq!(kept, vec![(0x0700, 0x0800)]);
        assert_eq!(dropped, vec![(0x0801, 0x0810)]);
    }

    #[test]
    fn a_range_spanning_the_whole_window_keeps_both_ends() {
        let (kept, dropped) = clip_ranges_against_inplace_window([(0x0700, 0x1100)], 0x0801, 0x1000);
        assert_eq!(kept, vec![(0x0700, 0x0800), (0x1004, 0x1100)]);
        assert_eq!(dropped, vec![(0x0801, 0x1003)]);
    }

    #[test]
    fn a_window_that_reaches_the_top_of_memory_loses_the_tail() {
        // `hi + BRIDGE_ROOM` overflows, so there is no address the reservation could resume at and the whole overlap is lost.
        let (kept, dropped) = clip_ranges_against_inplace_window([(0x8000, 0xFFFF)], 0x0801, 0xFFFE);
        assert!(kept.is_empty());
        assert_eq!(dropped, vec![(0x8000, 0xFFFF)]);
    }

    #[test]
    fn the_reported_loss_never_names_bytes_the_caller_did_not_ask_for() {
        // The cushion runs to $1003, but this range stops at $0FF5, so the report has to stop there too.
        let (_, dropped) = clip_ranges_against_inplace_window([(0x0FF0, 0x0FF5)], 0x0801, 0x1000);
        assert_eq!(dropped, vec![(0x0FF0, 0x0FF5)]);
    }

    #[test]
    fn several_ranges_are_clipped_independently_and_keep_their_order() {
        let (kept, dropped) = clip_ranges_against_inplace_window(
            [(0x0400, 0x0500), (0x0900, 0x0910), (0x0FF0, 0x1100)],
            0x0801,
            0x1000,
        );
        assert_eq!(kept, vec![(0x0400, 0x0500), (0x1004, 0x1100)]);
        assert_eq!(dropped, vec![(0x0900, 0x0910), (0x0FF0, 0x1003)]);
    }

    #[test]
    fn machine_language_prg_is_rejected_with_clear_message() {
        // A .prg that loads at $C000 is machine code, not BASIC text.
        // The compiler should bail early naming the load address rather
        // than letting the parser report a confusing unsupported token.
        let mut bytes = tiny_prg();
        bytes[0] = 0x00;
        bytes[1] = 0xC0; // load address $C000, where machine code lives
        match compile_with_options(&bytes, CompileOptions::default()) {
            Err(CompileError::NotBasic { load_address }) => {
                assert_eq!(load_address, 0xC000);
                let msg = format!("{}", CompileError::NotBasic { load_address });
                assert!(msg.contains("$C000"), "names the load address: {msg}");
                assert!(
                    msg.contains("not a BASIC program"),
                    "explains it isn't BASIC: {msg}"
                );
            }
            Err(e) => panic!("expected NotBasic error, got a different error: {e}"),
            Ok(_) => panic!("expected NotBasic error, got a successful compile"),
        }
    }

    #[test]
    fn relocated_basic_with_consistent_links_still_compiles() {
        // Real BASIC saved at a non-$0801 load address (e.g. $1001)
        // has consistent links and must still compile — codegen
        // re-targets $0801 regardless of the source's load address.
        let bytes = vec![
            0x01, 0x10, // load address $1001
            0x09, 0x10, // next-line link -> $1009 (EOP), consistent
            0x0A, 0x00, // line 10
            0x99, 0x20, 0x31, // PRINT space "1"
            0x00, // line terminator
            0x00, 0x00, // end-of-program
        ];
        let out = compile_with_options(&bytes, CompileOptions::default())
            .expect("relocated BASIC compiles");
        // Output is a normal $0801 SYS-stub program.
        assert_eq!(&out.prg_bytes[0..2], &[0x01, 0x08]);
    }

    #[test]
    fn mutual_exclusion_extraram_and_force_off_errors() {
        let bytes = tiny_prg();
        let opts = CompileOptions {
            extraram: true,
            force_extraram_off: true,
            ..CompileOptions::default()
        };
        match compile_with_options(&bytes, opts) {
            Err(e) => {
                let msg = format!("{e}");
                assert!(
                    msg.contains("mutually exclusive"),
                    "error mentions exclusivity: {msg}"
                );
            }
            Ok(_) => panic!("must reject contradictory flags"),
        }
    }

    #[test]
    fn auto_predictor_keeps_small_program_off_extraram() {
        // Tiny program ends well under $9F00; predictor should leave
        // extraram off, so the asm carries no JT preamble.
        let bytes = tiny_prg();
        let out =
            compile_with_options(&bytes, CompileOptions::default()).expect("tiny program compiles");
        assert!(
            !out.asm.contains("__JT_"),
            "no JT preamble in default build:\n{}",
            head(&out.asm, 20)
        );
        assert!(
            !out.asm.contains("__EXRAM_ENTRY"),
            "no extraram entry label:\n{}",
            head(&out.asm, 20)
        );
    }

    #[test]
    fn force_off_suppresses_extraram_even_when_predictor_would_enable() {
        // Same shape as the auto test, but with force_off set
        // explicitly — outcome is the same here (predictor would say
        // "off" anyway), so we just verify the flag is honoured
        // without surprises.
        let bytes = tiny_prg();
        let opts = CompileOptions {
            force_extraram_off: true,
            ..CompileOptions::default()
        };
        let out = compile_with_options(&bytes, opts).expect("compiles");
        assert!(!out.asm.contains("__JT_"));
        assert!(!out.asm.contains("__EXRAM_ENTRY"));
    }

    #[test]
    fn explicit_extraram_emits_jt_preamble() {
        let bytes = tiny_prg();
        let opts = CompileOptions {
            extraram: true,
            ..CompileOptions::default()
        };
        let out = compile_with_options(&bytes, opts).expect("compiles with extraram");
        assert!(
            out.asm.contains("__EXRAM_ENTRY"),
            "extraram preamble injected"
        );
        // PRINT 1 routes through ROM helpers (FOUT, MOVMF, …) that
        // get JT-wrapped — at least one __JT_ label must appear.
        assert!(
            out.asm.contains("__JT_"),
            "at least one JT entry present:\n{}",
            head(&out.asm, 40)
        );
    }

    /// A program that builds a string on the heap at runtime, so codegen emits `__HEAP_BOTTOM`
    /// and the window matters.
    fn heap_using_prg() -> Vec<u8> {
        crate::source::tokenize_program("10 A$=\"X\"\n20 FOR I=1 TO 5:A$=A$+\"Y\":NEXT\n30 PRINT A$\n")
            .expect("tokenizes")
    }

    #[test]
    fn string_program_with_room_below_memsiz_keeps_extraram_off() {

        let bytes = heap_using_prg();
        let opts = CompileOptions {
            custom_start_address: Some(0x9000),
            ..CompileOptions::default()
        };
        let out = compile_with_options(&bytes, opts).expect("compiles");
        assert!(
            out.asm.contains("__HEAP_BOTTOM:"),
            "the fixture really does use the string heap"
        );
        assert!(
            matches!(out.diagnostics.extraram, ExtraRamDecision::AutoOff),
            "predictor left extraram off, got {:?}",
            out.diagnostics.extraram
        );
        assert!(!out.asm.contains("__EXRAM_ENTRY"), "no JT preamble");
        let window = string_heap_window(&out, 0x9000).expect("window is adequate");
        assert!(
            window.expect("program has a heap") >= MIN_STRING_HEAP,
            "window {window:?} clears the floor"
        );
    }

    #[test]
    fn string_program_starved_of_heap_switches_to_extraram() {

        let bytes = heap_using_prg();
        let opts = CompileOptions {
            custom_start_address: Some(0x9C00),
            ..CompileOptions::default()
        };
        let out = compile_with_options(&bytes, opts).expect("compiles");
        assert!(
            matches!(out.diagnostics.extraram, ExtraRamDecision::AutoOn),
            "starved heap forces the extraram layout, got {:?}",
            out.diagnostics.extraram
        );
        assert!(
            out.asm.contains("__EXRAM_ENTRY"),
            "extraram preamble injected"
        );
    }

    #[test]
    fn heapless_program_near_memsiz_keeps_extraram_off() {

        let bytes = tiny_prg();
        let opts = CompileOptions {
            custom_start_address: Some(0x9C00),
            ..CompileOptions::default()
        };
        let out = compile_with_options(&bytes, opts).expect("compiles");
        assert!(
            !out.asm.contains("__HEAP_BOTTOM:"),
            "fixture has no runtime string heap"
        );
        assert!(
            matches!(out.diagnostics.extraram, ExtraRamDecision::AutoOff),
            "no heap means no heap window to protect, got {:?}",
            out.diagnostics.extraram
        );
        assert_eq!(
            string_heap_window(&out, 0x9C00),
            Ok(None),
            "window question does not apply"
        );
    }

    #[test]
    fn force_extraram_off_refuses_a_starved_string_heap() {
        // The build-time assertion.
        let bytes = heap_using_prg();
        let opts = CompileOptions {
            custom_start_address: Some(0x9C00),
            force_extraram_off: true,
            ..CompileOptions::default()
        };
        match compile_with_options(&bytes, opts) {
            Err(e) => {
                let msg = format!("{e}");
                assert!(
                    msg.contains("string heap"),
                    "diagnoses the heap window: {msg}"
                );
                assert!(msg.contains("$A000"), "names MEMSIZ: {msg}");
                assert!(
                    msg.contains("BASIC ROM"),
                    "explains why the bytes come back wrong: {msg}"
                );
            }
            Ok(out) => panic!(
                "must refuse; shipped {} bytes ending at ${:04X}",
                out.machine_code.len(),
                0x9C00 + out.machine_code.len(),
            ),
        }
    }

    #[test]
    fn string_heap_window_is_skipped_above_the_rom_window() {

        let bytes = heap_using_prg();
        let opts = CompileOptions {
            custom_start_address: Some(0xC000),
            ..CompileOptions::default()
        };
        let out = compile_with_options(&bytes, opts).expect("compiles at $C000");
        assert_eq!(string_heap_window(&out, 0xC000), Ok(None));
        assert!(
            matches!(out.diagnostics.extraram, ExtraRamDecision::SkippedHighOrigin),
            "got {:?}",
            out.diagnostics.extraram
        );
    }

    const EXR_ORIGIN: u16 = 0x9D00;

    fn runtime_dim_prg() -> Vec<u8> {
        crate::source::tokenize_program("10 N=PEEK(53280)\n20 DIM A(N)\n30 A(0)=1\n40 PRINT A(0)\n")
            .expect("tokenizes")
    }

    fn exr_opts(reserved: Vec<(u16, u16)>) -> CompileOptions {
        CompileOptions {
            custom_start_address: Some(EXR_ORIGIN),
            reserved_ranges: reserved,
            // The fixture's ceiling must be the one this test places, not one discovered from its own PEEK literal.
            auto_reserve: false,
            ..CompileOptions::default()
        }
    }

    /// Builds `bytes` with no ceiling reservation and returns the exclusive end of its image ; the address the heap starts at.
    fn exr_image_end(bytes: &[u8]) -> u32 {
        let base = compile_with_options(bytes, exr_opts(Vec::new())).expect("base build");
        assert!(
            matches!(base.diagnostics.extraram, ExtraRamDecision::AutoOn),
            "fixture must reach the extraram escalation, got {:?}",
            base.diagnostics.extraram
        );
        assert!(
            base.asm.contains(extraram::MEMSIZ_APPLY),
            "fixture's runtime really does relocate MEMSIZ"
        );
        u32::from(EXR_ORIGIN) + base.machine_code.len() as u32
    }

    fn ceiling_at(end: u32, window: u32) -> Vec<(u16, u16)> {
        let lo = u16::try_from(end + window).expect("ceiling fits");
        assert!(
            u32::from(lo) >= u32::from(extraram::DATA_CEILING),
            "a range below ${:04X} is inside the hoisted data and cannot be a ceiling",
            extraram::DATA_CEILING
        );
        vec![(lo, lo + 0xFF)]
    }

    #[test]
    fn extraram_dim_floor_is_one_maximal_heap_chunk_rounded_up_to_a_page() {

        const PAGE: u32 = 0x100;
        assert_eq!(
            MIN_EXTRARAM_DIM_HEAP,
            MAX_HEAP_CHUNK.div_ceil(PAGE) * PAGE,
            "floor must be MAX_HEAP_CHUNK ({MAX_HEAP_CHUNK}) rounded up to a whole page"
        );
    }

    #[test]
    fn extraram_runtime_dim_is_accepted_exactly_at_the_floor() {
        // The hard admission boundary is independent of the optional profile preference for more runtime-DIM heap headroom.
        let bytes = runtime_dim_prg();
        let end = exr_image_end(&bytes);
        let out = compile_at_profile(&bytes, exr_opts(ceiling_at(end, MIN_EXTRARAM_DIM_HEAP)))
            .expect("a window of exactly the floor builds");
        assert_eq!(
            extraram_heap_window(&out, u32::from(EXR_ORIGIN)),
            Ok(Some(MIN_EXTRARAM_DIM_HEAP)),
            "the window really is the floor, to the byte"
        );
        assert!(
            !out.diagnostics.size_profile_fallback,
            "nothing to rescue: the requested profile is what ships"
        );
        assert_eq!(
            u32::from(EXR_ORIGIN) + out.machine_code.len() as u32,
            end,
            "image is byte-for-byte the length it had with no ceiling at all"
        );
    }

    #[test]
    fn an_extraram_heap_starts_above_a_reservation_bridged_inside_the_image() {
        let bytes = runtime_dim_prg();
        let mut options = exr_opts(vec![(0xA000, 0xC1FF)]);
        options.custom_start_address = Some(0x9E00);
        let out = compile_at_profile(&bytes, options)
            .expect("the heap above the bridged reservation has room for DIM");
        let end = 0x9E00 + out.machine_code.len() as u32;
        assert!(end > 0xC1FF && end < 0xCE00, "fixture ends at ${end:04X}");
        assert!(out.asm.contains(
            "__EXRAM_MEMSIZ_RAISE:\n    LDA #$00\n    STA $37\n    LDA #$D0\n    STA $38\n"
        ), "the runtime must use the space above the entire image");
        assert_eq!(
            extraram_heap_window(&out, 0x9E00),
            Ok(Some(0xD000 - end)),
            "the build guard must agree with the runtime ceiling"
        );
        assert!(!out.diagnostics.size_profile_fallback);
    }

    #[test]
    fn extraram_runtime_dim_one_byte_under_the_floor_is_refused() {

        let bytes = runtime_dim_prg();
        let end = exr_image_end(&bytes);
        let starved = MIN_EXTRARAM_DIM_HEAP - 1;
        let err = match compile_at_profile(&bytes, exr_opts(ceiling_at(end, starved))) {
            Err(e) => e,
            Ok(out) => panic!(
                "must refuse; shipped {} bytes ending at ${:04X} with a {}-byte window",
                out.machine_code.len(),
                u32::from(EXR_ORIGIN) + out.machine_code.len() as u32,
                end + starved - (u32::from(EXR_ORIGIN) + out.machine_code.len() as u32),
            ),
        };
        let msg = format!("{err}");
        assert!(
            matches!(err, CompileError::Assemble { .. }),
            "must be the class the Size ladder retries: {msg}"
        );
        assert!(
            msg.contains(&format!("only {starved} bytes")),
            "reports the window it measured, to the byte: {msg}"
        );
        assert!(
            msg.contains(&format!("at least {MIN_EXTRARAM_DIM_HEAP} are needed")),
            "reports the floor it wanted: {msg}"
        );
        assert!(
            msg.contains(&format!("${:04X}", end + starved)),
            "names the ceiling it measured against, not $A000 or $D000: {msg}"
        );
    }

    #[test]
    fn starved_extraram_runtime_dim_is_rescued_by_the_size_profile() {
        // The remedy.
        let bytes = runtime_dim_prg();
        let end = exr_image_end(&bytes);
        let starved = MIN_EXTRARAM_DIM_HEAP - 1;
        let out = compile_with_options(&bytes, exr_opts(ceiling_at(end, starved)))
            .expect("the Size retry finds the room");
        assert!(
            out.diagnostics.size_profile_fallback,
            "rescued by Size, not by the check quietly passing"
        );
        let rescued_end = u32::from(EXR_ORIGIN) + out.machine_code.len() as u32;
        assert!(
            rescued_end < end,
            "the rescue is a smaller image: ${rescued_end:04X} vs ${end:04X}"
        );
        assert_eq!(
            extraram_heap_window(&out, u32::from(EXR_ORIGIN)),
            Ok(Some(end + starved - rescued_end)),
            "and the image that ships clears the floor"
        );
    }

    #[test]
    fn extraram_heap_starvation_survives_the_whole_ladder_as_out_of_memory() {

        let bytes = runtime_dim_prg();
        let end = exr_image_end(&bytes);
        let ceiling = u32::from(extraram::DATA_CEILING);
        assert!(
            ceiling > end && ceiling - end < MIN_EXTRARAM_DIM_HEAP,
            "fixture premise: ${ceiling:04X} starves the ${end:04X} image"
        );
        let lo = u16::try_from(ceiling).expect("fits");
        let err = match compile_with_options(&bytes, exr_opts(vec![(lo, lo + 0xFF)])) {
            Err(e) => e,
            Ok(out) => panic!(
                "must refuse; shipped {} bytes, size_fallback={}",
                out.machine_code.len(),
                out.diagnostics.size_profile_fallback,
            ),
        };
        let msg = format!("{err}");
        assert!(msg.contains("?OUT OF MEMORY"), "{msg}");
        assert!(
            msg.contains("runtime DIM"),
            "names what the window is being shared with: {msg}"
        );
    }

    /// A finished extraram-ON build with a chosen image length and reserved list, and nothing else that matters.
    fn extraram_build_ending_at(end: u32, reserved: Vec<(u16, u16)>) -> Compiled {
        let origin = u32::from(EXR_ORIGIN);
        assert!(end > origin, "image must end above its origin");
        Compiled {
            asm: format!("{}:\n__ARR_HEAP_TOP:\n", extraram::MEMSIZ_APPLY),
            machine_code: vec![0u8; (end - origin) as usize],
            prg_bytes: Vec::new(),
            diagnostics: Diagnostics {
                extraram: ExtraRamDecision::AutoOn,
                effective_reserved: reserved,
                ..Diagnostics::default()
            },
        }
    }

    #[test]
    fn extraram_heap_shortfall_is_measured_past_the_ceiling_not_clamped_to_the_floor() {

        let origin = u32::from(EXR_ORIGIN);
        let end = 0xE394;
        let build = extraram_build_ending_at(end, Vec::new());
        let ceiling = u32::from(extraram::heap_ceiling(
            &build.diagnostics.effective_reserved,
            end,
        ));
        assert_eq!(ceiling, 0xD000, "fixture premise: the default heap ceiling");
        assert_eq!(
            extraram_heap_window(&build, origin),
            Err(0),
            "fixture premise: the window really is clamped to zero"
        );
        let msg = format!(
            "{}",
            verify_extraram_heap(&build, origin).expect_err("must refuse")
        );

        assert!(
            msg.contains("Shrink the program by about 5524 bytes"),
            "must charge the overshoot plus the floor: {msg}"
        );
        assert!(
            !msg.contains("by about 512 bytes"),
            "must not quote the bare floor as the shortfall: {msg}"
        );
    }

    #[test]
    fn extraram_heap_shortfall_below_the_ceiling_is_the_floor_minus_the_window() {

        let origin = u32::from(EXR_ORIGIN);
        let window = MIN_EXTRARAM_DIM_HEAP - 300;
        let end = 0xD000 - window;
        let build = extraram_build_ending_at(end, Vec::new());
        assert_eq!(
            extraram_heap_window(&build, origin),
            Err(window),
            "fixture premise: under the floor but still below the ceiling"
        );
        let msg = format!(
            "{}",
            verify_extraram_heap(&build, origin).expect_err("must refuse")
        );
        assert!(
            msg.contains("Shrink the program by about 300 bytes"),
            "floor minus the window it has, to the byte: {msg}"
        );
    }

    #[test]
    fn the_rom_window_refusal_keeps_the_force_extraram_off_remedy() {

        let bytes = heap_using_prg();
        let opts = CompileOptions {
            force_extraram_off: true,
            custom_start_address: Some(0x9F00),
            ..CompileOptions::default()
        };
        let err = match compile_with_options(&bytes, opts) {
            Err(e) => e,
            Ok(out) => panic!(
                "must refuse: shipped {} bytes ending at ${:04X}",
                out.machine_code.len(),
                0x9F00 + out.machine_code.len(),
            ),
        };
        let msg = format!("{err}");
        assert!(
            msg.contains("--force-extraram-off"),
            "the remedy must name the flag that is in the way: {msg}"
        );
    }

    #[test]
    fn a_layout_failure_reports_the_binding_ceiling_and_keeps_the_first() {

        let off = CompileError::Assemble {
            message: "?OUT OF MEMORY: ... remove about 16496 bytes.".to_string(),
            asm: Some("OFF-ASM".to_string()),
        };
        let on = CompileError::Assemble {
            message: format!(
                "{} $ACCC, 3276 bytes beyond the $A000 ceiling.",
                extraram::DATA_CEILING_MARKER
            ),
            asm: Some("ON-ASM".to_string()),
        };
        let out = binding_layout_failure(Some(off), on);
        let msg = format!("{out}");
        assert!(
            msg.contains("remove about 16496 bytes"),
            "keeps the extraram-off explanation: {msg}"
        );
        assert!(
            msg.contains("grew to $ACCC"),
            "carries the binding ceiling's own figure: {msg}"
        );
        assert!(
            msg.find("remove about 16496").unwrap() < msg.find("grew to $ACCC").unwrap(),
            "the binding figure comes last, as the one to act on: {msg}"
        );

        assert!(
            msg.contains("not as how much BASIC to delete"),
            "the binding figure must not read as a subtraction target: {msg}"
        );
        assert_eq!(
            out.generated_asm(),
            Some("OFF-ASM"),
            "the attached asm stays the one the user's build produced"
        );
    }

    #[test]
    fn a_layout_failure_with_no_off_attempt_reports_the_extraram_error_alone() {

        let body = format!(
            "{} $ACCC, 3276 bytes beyond the $A000 ceiling.",
            extraram::DATA_CEILING_MARKER
        );
        let on = CompileError::Assemble {
            message: body.clone(),
            asm: Some("ON-ASM".to_string()),
        };
        let msg = format!("{}", binding_layout_failure(None, on));
        assert_eq!(
            msg,
            format!("assemble: {body}"),
            "verbatim, with nothing appended: {msg}"
        );
    }

    #[test]
    fn a_wall_extraram_does_not_move_is_not_offered_as_the_binding_ceiling() {

        let off = CompileError::Assemble {
            message: "?OUT OF MEMORY: the program needs 67460 bytes from $080D, which is \
                      3985 bytes past the top of memory."
                .to_string(),
            asm: Some("OFF-ASM".to_string()),
        };
        let on = CompileError::Assemble {
            message: "?OUT OF MEMORY: the program needs 67715 bytes from $080D, which is \
                      4240 bytes past the top of memory."
                .to_string(),
            asm: Some("ON-ASM".to_string()),
        };
        let msg = format!("{}", binding_layout_failure(Some(off), on));
        assert!(
            msg.contains("3985 bytes past"),
            "the first error stands: {msg}"
        );
        assert!(
            !msg.contains("4240"),
            "and the bigger extraram figure must not be offered as the remedy: {msg}"
        );
        assert!(
            !msg.contains("the ceiling to work against"),
            "no composition at all for a wall extraram does not move: {msg}"
        );
    }

    #[test]
    fn the_io_wall_is_composed_with_because_extraram_moves_it() {

        let off = CompileError::Assemble {
            message: "?OUT OF MEMORY: ... Shrink the program about 21338 bytes.".to_string(),
            asm: Some("OFF-ASM".to_string()),
        };
        let on = CompileError::Assemble {
            message: format!(
                "{} $F619, 12744 bytes above $C451 — the top of the space this layout has.",
                extraram::IO_WALL_MARKER
            ),
            asm: Some("ON-ASM".to_string()),
        };
        let msg = format!("{}", binding_layout_failure(Some(off), on));
        assert!(
            msg.contains("the ceiling to work against"),
            "the I/O wall must compose, unlike the 64 KB wall: {msg}"
        );
        assert!(
            msg.contains("12744 bytes above $C451"),
            "and carry the binding figure: {msg}"
        );
        assert!(
            msg.contains("Shrink the program about 21338 bytes"),
            "without discarding the plain layout's explanation: {msg}"
        );
        assert!(
            msg.find("21338").unwrap() < msg.find("12744").unwrap(),
            "binding figure last, as the one to act on: {msg}"
        );

        assert!(12744 < 21338, "premise of the whole composition");
    }

    #[test]
    fn a_non_layout_first_error_is_not_composed_with() {

        let first = CompileError::NotBasic {
            load_address: 0x0801,
        };
        let on = CompileError::Assemble {
            message: "--extraram: data sections grew to $ACCC, beyond the $A000 ceiling."
                .to_string(),
            asm: None,
        };
        let msg = format!("{}", binding_layout_failure(Some(first), on));
        assert!(msg.contains("not a BASIC program"), "{msg}");
        assert!(
            !msg.contains("grew to $ACCC"),
            "nothing appended to a layout-independent failure: {msg}"
        );
    }

    #[test]
    fn extraram_starved_program_without_runtime_dim_still_builds() {

        let bytes = heap_using_prg();
        let end = exr_image_end(&bytes);
        let options = exr_opts(ceiling_at(end, MIN_EXTRARAM_DIM_HEAP - 1));
        let ordinary = compile_at_profile(&bytes, options.clone())
            .expect("the requested string-only layout is not charged the DIM floor");
        assert!(!ordinary.diagnostics.size_profile_fallback);
        assert_eq!(u32::from(EXR_ORIGIN) + ordinary.machine_code.len() as u32, end);
        let before = managed_extraram_heap_window(&ordinary, u32::from(EXR_ORIGIN)).unwrap();
        assert_eq!(before, MIN_EXTRARAM_DIM_HEAP - 1);
        let out = compile_with_options(&bytes, options.clone())
            .expect("the public compiler still admits a string-only program below the DIM floor");
        assert!(
            !out.asm.contains("__ARR_HEAP_TOP:"),
            "fixture premise: no runtime DIM in this program"
        );
        assert_eq!(
            extraram_heap_window(&out, u32::from(EXR_ORIGIN)),
            Ok(None),
            "the question must be DECLINED, not answered and passed"
        );
        let after = managed_extraram_heap_window(&out, u32::from(EXR_ORIGIN)).unwrap();
        if out.prg_bytes == ordinary.prg_bytes {
            assert_eq!(after, before);
            assert!(!out.diagnostics.size_profile_fallback);
            assert_eq!(out.diagnostics.size_profile_heap_improvement, None);
        } else {
            assert!(after > before, "a soft choice must improve verified capacity");
            assert_eq!(out.diagnostics.size_profile_heap_improvement, Some((before, after)));
            assert!(out.diagnostics.compact_heap_layout);
            let checked = compile_at_profile_rescue(
                &bytes,
                CompileOptions { profile: Profile::Size, ..options },
                LayoutRescue::Compact,
            ).expect("the selected Compact alternative passes every normal layout gate");
            assert_eq!(out.prg_bytes, checked.prg_bytes);
            assert_eq!(out.machine_code, checked.machine_code);
        }
    }

    #[test]
    fn extraram_heap_window_is_measured_against_the_reserved_ceiling() {

        let bytes = runtime_dim_prg();
        let end = exr_image_end(&bytes);
        let unreserved = compile_with_options(&bytes, exr_opts(Vec::new())).expect("builds");
        assert_eq!(
            extraram_heap_window(&unreserved, u32::from(EXR_ORIGIN)),
            Ok(Some(u32::from(extraram::HEAP_CEILING) - end)),
            "with no reservation the ceiling is the machine's"
        );
        let window = MIN_EXTRARAM_DIM_HEAP * 3;
        let reserved =
            compile_at_profile(&bytes, exr_opts(ceiling_at(end, window))).expect("builds");
        assert_eq!(
            extraram_heap_window(&reserved, u32::from(EXR_ORIGIN)),
            Ok(Some(window)),
            "with one, the ceiling is the reservation's first byte"
        );
    }

    #[test]
    fn custom_start_address_skips_sys_launcher_and_relocates_origin() {
        let bytes = tiny_prg();
        let opts = CompileOptions {
            custom_start_address: Some(0xC000),
            ..CompileOptions::default()
        };
        let out = compile_with_options(&bytes, opts).expect("compiles at $C000");
        // First two bytes of the .prg are the load address; with no
        // SYS stub the code starts immediately after them.
        assert_eq!(&out.prg_bytes[0..2], &[0x00, 0xC0], "load address is $C000");
        assert_eq!(
            out.prg_bytes.len() - 2,
            out.machine_code.len(),
            "no SYS launcher bytes between load addr and code"
        );
        assert!(
            out.asm.contains("*=$C000"),
            "codegen originates at $C000:\n{}",
            head(&out.asm, 5)
        );
    }

    #[test]
    fn custom_start_above_rom_shadow_skips_auto_extraram() {
        // Origin at $C000 sits past the BASIC-ROM shadow — extraram
        // can't add code space below the program. Predictor must
        // skip even though probe_end clearly exceeds $9F00.
        let bytes = tiny_prg();
        let opts = CompileOptions {
            custom_start_address: Some(0xC000),
            ..CompileOptions::default()
        };
        let out = compile_with_options(&bytes, opts).expect("compiles");
        assert!(
            !out.asm.contains("__EXRAM_ENTRY"),
            "no extraram preamble for $C000 origin"
        );
    }

    #[test]
    fn custom_start_below_rom_shadow_still_routes_through_predictor() {
        // Origin at $8000 — the predictor must still run with the
        // shifted origin, so a build whose probe end crosses $9F00
        // gets the extraram path. We don't require the program to
        // fit (the test fixture deliberately overflows even with
        // extraram on); the proxy is that the error path is the
        // extraram-data-ceiling one, not "no extraram engaged".
        let mut basic = String::from("10 DIM A(2000)\n");
        for line in 0..50 {
            basic.push_str(&format!("{} PRINT \"X\"\n", 20 + line));
        }
        let prg = crate::source::tokenize_program(&basic).expect("tokenises");
        let opts = CompileOptions {
            custom_start_address: Some(0x8000),
            ..CompileOptions::default()
        };
        let err = match compile_with_options(&prg, opts) {
            Err(e) => e,
            Ok(_) => panic!(
                "fixture is sized to overflow even with extraram on; expected a ceiling error"
            ),
        };
        let msg = format!("{err}");

        assert!(
            msg.contains("hoisted data sections grew to"),
            "error message must show the predictor switched extraram on for the $8000 \
             origin (else it would have been a default-layout error). got: {msg}"
        );
        assert!(
            msg.contains(&format!("${:04X} ceiling", extraram::DATA_CEILING)),
            "and must be the DATA-ceiling refusal specifically. got: {msg}"
        );
    }

    #[test]
    fn custom_start_below_rom_shadow_keeps_off_when_program_fits() {
        // A small program at $8000 lands well under $9F00; predictor
        // should leave extraram off — same outcome as the default
        // origin for a small program, just relocated.
        let bytes = tiny_prg();
        let opts = CompileOptions {
            custom_start_address: Some(0x8000),
            ..CompileOptions::default()
        };
        let out = compile_with_options(&bytes, opts).expect("compiles");
        assert!(
            !out.asm.contains("__EXRAM_ENTRY"),
            "small program at $8000 stays off extraram"
        );
        assert!(
            out.asm.contains("*=$8000"),
            "code originates at $8000:\n{}",
            head(&out.asm, 5)
        );
    }

    fn head(s: &str, n: usize) -> String {
        s.lines().take(n).collect::<Vec<_>>().join("\n")
    }

    /// Tokenized PRG for `10 FOR I=12288 TO 12351:POKE I,255:NEXT`.
    /// Only enough bytes to prove discover_reserved_ranges picks
    /// up the FOR-loop POKE pattern.
    fn for_poke_prg() -> Vec<u8> {
        // Hand-tokenized: `10 FOR I=12288 TO 12351:POKE I,255:NEXT`
        // FOR=$81 I = 12288 TO=$A4 12351 :POKE=$97 I,255 :NEXT=$82
        // BASIC stores numeric constants as literal ASCII.
        let body: Vec<u8> = b"\x81 I\xb212288\xa412351:\x97 I,255:\x82".to_vec();
        let mut prg: Vec<u8> = vec![0x01, 0x08]; // load addr
// Link -> next line: $0801 + 2(link) + 2(line#) + body + 1(term).
        let next_link: u16 = 0x0806 + body.len() as u16;
        prg.extend_from_slice(&next_link.to_le_bytes());
        prg.extend_from_slice(&[0x0A, 0x00]); // line 10
        prg.extend_from_slice(&body);
        prg.push(0x00); // line term
        prg.extend_from_slice(&[0x00, 0x00]); // EOP
        prg
    }

    /// Regression: `FOR I=lit TO lit: POKE I, X: NEXT` — the
    /// canonical custom-charset / ML-loader idiom — must surface
    /// in auto-reserve as the inclusive `[start, end]` range.
    /// Without this, charset loaders using `FOR AD=12288 TO 12351:
    /// READ A: POKE AD, A: NEXT` leave `$3000-$303F` unreserved.
    #[test]
    fn auto_reserve_picks_up_for_loop_poke_with_var_address() {
        let bytes = for_poke_prg();
        let opts = CompileOptions {
            auto_reserve: true,
            ..CompileOptions::default()
        };
        let out = compile_with_options(&bytes, opts).expect("compiles");
        // Hard-to-verify directly without exposing the discover
        // function — instead, assert the compile succeeded and
        // re-run discover on the same module to inspect the result.
        let _ = out;
        let prg = crate::prg::Program::parse(&bytes).expect("prg parses");
        let ast = crate::parse::program(&prg).expect("source parses");
        let module = lower_and_optimize(&ast, Profile::default()).expect("ir lowers");
        let ranges = discover_reserved_ranges(&module);
        assert!(
            ranges.iter().any(|(s, e)| *s <= 12288 && *e >= 12351),
            "auto-reserve must include the FOR-loop POKE range \
             $3000-$303F (= 12288..12351); got {ranges:?}"
        );
    }

    /// `*=$080D` followed by `n` filler bytes.
    fn filler_asm(n: usize) -> String {
        let mut s = String::from("*=$080D\n");
        s.push_str(&filler_bytes(n));
        s
    }

    /// `filler_asm` without the `*=`, so a fixture can place a label or an instruction at a chosen offset and keep filling after it.
    fn filler_bytes(n: usize) -> String {
        let mut s = String::new();
        for chunk in (0..n).collect::<Vec<_>>().chunks(1000) {
            s.push_str("    .byte ");
            s.push_str(&vec!["$00"; chunk.len()].join(","));
            s.push('\n');
        }
        s
    }

    fn branch_error(asm: String) -> CompileError {

        CompileError::Assemble {
            message: "Assembly error: $FFFA: BEQ __FOR_RT_WALK_DONE_253 - Branch offset \
                      out of range: -65530. Target: $0002, Current: $FFFA"
                .to_string(),
            asm: Some(asm),
        }
    }

    #[test]
    fn internal_errors_tell_the_user_it_is_our_bug() {
        let e = CompileError::Assemble {
            message: "internal error: the label(s) __DATA_END resolve to $9F00".to_string(),
            asm: None,
        };
        let msg = format!("{e}");
        assert!(
            msg.contains("bug in the compiler"),
            "the user must be told whose fault it is; got: {msg}"
        );
        assert!(
            msg.contains(ISSUES_URL),
            "and where to send it; got: {msg}"
        );
        assert!(
            msg.contains("--no-auto-reserve"),
            "and something to try meanwhile; got: {msg}"
        );
        assert!(
            msg.contains("__DATA_END"),
            "without losing the detail the report needs; got: {msg}"
        );
    }

    #[test]
    fn ordinary_assemble_errors_get_no_bug_boilerplate() {
        let e = CompileError::Assemble {
            message: "?OUT OF MEMORY: the program needs 67652 bytes from $080D".to_string(),
            asm: None,
        };
        let msg = format!("{e}");
        assert_eq!(
            msg, "assemble: ?OUT OF MEMORY: the program needs 67652 bytes from $080D",
            "a user-actionable message must pass through byte for byte"
        );
    }

    /// Bytes in an internal error must be spelled the way the machine's own tools spell them.
    #[test]
    fn image_bytes_are_spelled_for_a_6502_programmer() {
        let got = hex_bytes(&[0x8D, 0x14, 0x03]);
        assert_eq!(got, "$8D $14 $03", "6502 notation, not Rust Debug");
        assert!(
            !got.contains('[') && !got.contains(','),
            "no Rust slice syntax may reach the user; got: {got}"
        );
        assert_eq!(hex_bytes(&[]), "", "an empty run renders as nothing");
    }

    #[test]
    fn narrowed_index_error_names_no_rust_symbol() {
        let asm = "*=$080D\n    LDA $00C8,X\n";
        let err = verify_no_narrowed_index(&asm.to_string(), &|name: &str| {
            (name == "$00C8").then_some(0x00C8)
        })
        .expect_err("a base of $00C8 with an unrestricted X index must be refused");
        assert!(
            err.contains("internal error:") && err.contains("zeropage,X"),
            "the diagnosis itself must survive; got: {err}"
        );
        assert!(
            !err.contains("indexed_base_cannot_wrap") && !err.contains(".rs"),
            "no Rust symbol and no Rust filename may reach the user; got: {err}"
        );
    }

    #[test]
    fn compiler_only_assembler_errors_are_marked_as_our_bug() {
        for raw in [
            "Assembly error: Undefined label: __FOR_EXIT_RT4",
            "Assembly error: Long-branch fix didn't converge after 20 iterations. \
             Problematic branches:\n  BNE __L7",
            "Assembly error: Long-branch fix didn't converge after 20 iterations \
             (no obvious problematic branches found)",
            "Assembly error: Unknown mnemonic: LDZ",
            "Assembly error: Immediate value too large: $0140",
            "Assembly error: Unsupported mode for STX",
        ] {
            let out = label_asm6502_internal(raw);
            assert!(
                out.starts_with("internal error: "),
                "must be marked as ours; got: {out}"
            );
            assert!(
                out.ends_with(raw),
                "the assembler's own words must survive byte for byte; got: {out}"
            );
            let shown = format!(
                "{}",
                CompileError::Assemble {
                    message: out.clone(),
                    asm: None,
                }
            );
            assert!(
                shown.contains("bug in the compiler") && shown.contains(ISSUES_URL),
                "and the prefix must reach the Display wrapper; got: {shown}"
            );
        }
    }

    #[test]
    fn compiler_only_leaves_are_marked_through_every_wrapper() {

        for (src, claimed) in [
            // `Division by zero` ; a `/` by zero in an emitted expression, in each of its four wrappers.
            (
                "*=$2A10\n.byte 4/0\n",
                "Assembly error: .byte directive at $2A10: Division by zero",
            ),
            (
                "*=$2A10\n.word 4/0\n",
                "Assembly error: .word directive at $2A10: Division by zero",
            ),
            ("*=4/0\n", "Assembly error: ORG directive: Division by zero"),

            (
                "ZSI_V_Q = 4/0\n",
                "Assembly error: Constant 'ZSI_V_Q': Division by zero",
            ),
            // `Invalid indirect addressing mode` ; a malformed `(zp),Y`.
            (
                "*=$0C40\nLDA (__PTR\n",
                "Assembly error: $0C40: LDA (__PTR - Invalid indirect addressing mode",
            ),
        ] {
            let raw = format!(
                "{}",
                Assembler6502::new()
                    .assemble_bytes(src)
                    .expect_err("this source must not assemble")
            );
            assert_eq!(
                raw, claimed,
                "the census's text for {src:?} is not what asm6502 emits"
            );
            assert_eq!(
                label_asm6502_internal(&raw),
                format!("internal error: {raw}"),
                "must be marked as ours, by pure prefix and nothing else"
            );
        }

        for (src, wrapper) in [
            ("*=$0801\n.byte $GG\n", "Line 2: .byte $GG - "),
            ("*=$1234\nLDA __A+\n", "$1234: LDA __A+ - "),
        ] {
            let raw = format!(
                "{}",
                Assembler6502::new()
                    .assemble_bytes(src)
                    .expect_err("an unparseable operand must not assemble")
            );
            assert!(
                raw.starts_with(&format!("Assembly error: {wrapper}Invalid expression:")),
                "the wrapper and the leaf must both be what the census claims; got: {raw}"
            );
            assert_eq!(
                label_asm6502_internal(&raw),
                format!("internal error: {raw}"),
                "must be marked as ours, by pure prefix and nothing else"
            );
        }
    }

    /// The wrappers carry no verdict of their own, which is why they are NOT listed in `COMPILER_ONLY`.
    #[test]
    fn wrappers_inherit_their_leaf_and_do_not_decide_on_their_own() {
        // Same wrapper, leaf already on the marked list.
        assert_eq!(
            label_asm6502_internal("Assembly error: .byte directive at $2A10: Undefined label: __VARS"),
            "internal error: Assembly error: .byte directive at $2A10: Undefined label: __VARS",
        );
        // Same wrapper text, a leaf that is the user's ; stays theirs.
        let users = "Assembly error: .byte directive at $2A10: \
                     PC $080D lands inside reserved range $080D-$080F";
        assert_eq!(label_asm6502_internal(users), users);
    }

    #[test]
    fn swallowed_number_errors_surface_as_invalid_expression() {
        let err = Assembler6502::new()
            .assemble_bytes("*=$0801\n.byte $GG\n")
            .expect_err("a malformed hex literal must not assemble");
        let msg = format!("{err}");
        for swallowed in ["Invalid hexadecimal", "Invalid binary", "Invalid decimal"] {
            assert!(
                !msg.contains(swallowed),
                "{swallowed} is documented as unreachable, but the assembler emitted it: {msg}"
            );
        }
        assert!(
            msg.contains("Invalid expression: $GG"),
            "it must arrive as the expression shape instead; got: {msg}"
        );

        assert!(
            label_asm6502_internal(&msg).starts_with("internal error: "),
            "got: {msg}"
        );
    }

    #[test]
    fn division_by_zero_and_bad_indirect_are_producible_and_marked() {
        let div = Assembler6502::new()
            .assemble_bytes("*=$0801\n.byte 4/0\n")
            .expect_err("a divide by zero must not assemble");
        let div = format!("{div}");
        assert!(div.contains("Division by zero"), "got: {div}");
        assert_eq!(
            label_asm6502_internal(&div),
            format!("internal error: {div}")
        );

        let ind = Assembler6502::new()
            .assemble_bytes("*=$0801\nLDA ($10),Z\n")
            .expect_err("a malformed indirect operand must not assemble");
        let ind = format!("{ind}");
        assert!(
            ind.contains("Invalid indirect addressing mode"),
            "got: {ind}"
        );
        assert_eq!(
            label_asm6502_internal(&ind),
            format!("internal error: {ind}")
        );
    }

    #[test]
    fn reserved_range_errors_are_the_users_and_pass_through() {
        for raw in [
            "Assembly error: PC $080D lands inside reserved range $080D-$080F",
            "Assembly error: Reserved range $F000-$FFFF extends to end of address space; \
             nothing to JMP to",
            "Assembly error: Cannot fit 3-byte JMP before reserved $2000-$2FFF: \
             PC=$1FFE, only 2 bytes available",

            "Assembly error: $FFFC: BPL __X - Branch offset out of range: -65533",
        ] {
            assert_eq!(
                label_asm6502_internal(raw),
                raw,
                "must pass through byte for byte"
            );
        }
    }

    #[test]
    fn internal_marking_is_idempotent() {
        let once = label_asm6502_internal("Assembly error: Undefined label: __X");
        assert_eq!(label_asm6502_internal(&once), once);
    }

    #[test]
    fn the_internal_marker_does_not_leak_into_the_reservation_verdict() {
        let asm = filler_asm(0x100);
        let marked = label_asm6502_internal("Assembly error: Undefined label: __HEAP_PTR");
        assert!(marked.starts_with("internal error: "), "premise");
        let out = explain_address_space_overflow(
            CompileError::Assemble {
                message: marked,
                asm: Some(asm),
            },
            0x080D,
            &[(0x2000, 0x2FFF)],
        );
        let msg = format!("{out}");
        assert!(
            msg.contains("Undefined label: __HEAP_PTR"),
            "the assembler's own words must still be carried; got: {msg}"
        );
        assert!(
            !msg.contains("internal error"),
            "but our classification of them must not be; got: {msg}"
        );
    }

    #[test]
    fn address_space_overflow_reports_out_of_memory_not_a_branch_error() {
        let asm = filler_asm(0x1_0000);
        let out = explain_address_space_overflow(branch_error(asm), 0x080D, &[]);
        let msg = format!("{out}");
        assert!(
            msg.contains("?OUT OF MEMORY"),
            "an image past the top of memory must say so; got: {msg}"
        );
        assert!(
            !msg.contains("Branch offset"),
            "the wrapped-PC branch message must not survive; got: {msg}"
        );
        assert!(
            msg.contains("past the top of memory"),
            "the report should quote the overshoot; got: {msg}"
        );
    }

    /// Distinguish reservation overflow from an image that exceeds address space.
    #[test]
    fn reservation_driven_overflow_names_the_reservations() {
        let asm = filler_asm(0x8000);
        let out = explain_address_space_overflow(branch_error(asm), 0x080D, &[(0x2000, 0x9FFF)]);
        let msg = format!("{out}");
        assert!(
            msg.contains("past the top of memory"),
            "0x080D + 0x8000 + 0x8000 clears $FFFF, so the ceiling is live here and \
             must be reported; got: {msg}"
        );
        assert!(
            msg.contains("can add enough"),
            "the ceiling is not PROVABLE from these numbers -- a range above the image \
             is never crossed -- so the claim must be hedged; got: {msg}"
        );
        assert!(
            msg.contains("Branch offset"),
            "the assembler's own message must survive on this path too; got: {msg}"
        );
        assert!(
            msg.contains("reserved range"),
            "the reservations are the cause and must be named; got: {msg}"
        );
        assert!(
            msg.contains("--no-auto-reserve"),
            "the message should point at the escape hatch; got: {msg}"
        );
    }

    #[test]
    fn reservation_blame_requires_the_arithmetic_to_support_it() {

        let asm = filler_asm(0x29DD);
        let out = explain_address_space_overflow(
            branch_error(asm),
            0x080D,
            &[
                (0x080D, 0x080F),
                (0x1000, 0x15FF),
                (0x2000, 0x2000),
                (0x3000, 0x3001),
                (0x4000, 0x4001),
            ],
        );
        let msg = format!("{out}");
        assert!(
            !msg.contains("past the top of memory"),
            "1,544 bytes cannot push an image ending at $31E9 past $FFFF; got: {msg}"
        );
        assert!(
            msg.contains("reserved range"),
            "the reservations ARE the difference and should still be named; got: {msg}"
        );
        assert!(
            msg.contains("Branch offset"),
            "the assembler's own message must survive so the failure stays \
             diagnosable; got: {msg}"
        );
    }

    #[test]
    fn genuine_assembler_error_passes_through_unchanged() {
        let asm = filler_asm(0x100);
        let original = "Assembly error: Undefined label: __NOT_A_LABEL".to_string();
        let out = explain_address_space_overflow(
            CompileError::Assemble {
                message: original.clone(),
                asm: Some(asm),
            },
            0x080D,
            &[],
        );
        let msg = format!("{out}");
        assert!(
            msg.contains(&original),
            "a real error on an image that fits must survive verbatim; got: {msg}"
        );
        assert!(
            !msg.contains("?OUT OF MEMORY"),
            "must not invent an out-of-memory report; got: {msg}"
        );
    }

    #[test]
    fn wrapped_pc_branch_still_reports_out_of_memory() {

        let mut asm = filler_asm(0xF7EF);
        asm.push_str("    BPL __WRAPPED\n");
        asm.push_str(&filler_bytes(3));
        asm.push_str("__WRAPPED:\n");
        asm.push_str(&filler_bytes(100));

        let mut probe = Assembler6502::new();
        let broke = probe.assemble_full(&asm);
        assert!(
            broke.is_err(),
            "fixture must break the plain probe, else it tests the old path"
        );
        let out = explain_address_space_overflow(branch_error(asm), 0x080D, &[]);
        let msg = format!("{out}");
        assert!(
            msg.contains("?OUT OF MEMORY"),
            "a wrapped image must still be reported as full memory; got: {msg}"
        );
        assert!(
            !msg.contains("Branch offset") && !msg.contains("__"),
            "no assembler text and no internal label may survive; got: {msg}"
        );
    }

    #[test]
    fn wrapped_pc_with_no_measurable_length_still_reports_out_of_memory() {
        let asm = format!("{}    JMP __NO_SUCH_LABEL\n", filler_asm(0x100));

        assert!(
            Assembler6502::new().assemble_full(&asm).is_err(),
            "fixture must break the plain probe"
        );
        assert!(
            Assembler6502::new()
                .assemble_full(&branchless_probe_text(&asm))
                .is_err(),
            "fixture must break the branchless probe too, else the ceiling \
             test consumes it and this tests a different path"
        );
        let out = explain_address_space_overflow(branch_error(asm), 0x080D, &[(0x2000, 0x2FFF)]);
        let msg = format!("{out}");
        assert!(
            msg.contains("?OUT OF MEMORY") && msg.contains("wraps"),
            "the error alone proves the wrap and the report must say so; got: {msg}"
        );
        assert!(
            !msg.contains("Branch offset") && !msg.contains("__"),
            "no assembler text and no internal label may survive; got: {msg}"
        );
        assert!(
            msg.contains("reserved range") && msg.contains("--no-auto-reserve"),
            "reservations in force are part of the total and must be named; got: {msg}"
        );
    }

    /// The branch strip has to be length-preserving, because the number the user is handed is computed from it.
    #[test]
    fn branchless_probe_preserves_length() {
        let mut asm = filler_asm(0x100);
        for _ in 0..50 {
            asm.push_str("    BNE __HERE\n__HERE:\n");
        }
        let plain = Assembler6502::new()
            .assemble_full(&asm)
            .expect("fits")
            .0
            .len();
        let stripped = Assembler6502::new()
            .assemble_full(&branchless_probe_text(&asm))
            .expect("strip assembles")
            .0
            .len();
        assert_eq!(
            plain, stripped,
            "the strip must not change the measured image size"
        );
    }

    #[test]
    fn small_branch_offset_is_not_a_wrapped_pc() {
        let err = CompileError::Assemble {
            message: "Assembly error: $0900: BEQ __L1 - Branch offset out of range: 300. \
                      Target: $0A30, Current: $0900"
                .to_string(),
            asm: Some(filler_asm(0x1_0000)),
        };
        assert!(
            !wrapped_pc_branch_error(&err),
            "300 fits an i16; that is not a wrapped program counter"
        );
        let big = CompileError::Assemble {
            message: "Assembly error: $FFFC: BPL __X - Branch offset out of range: -65533. \
                      Target: $0001, Current: $FFFC"
                .to_string(),
            asm: Some(String::new()),
        };
        assert!(
            wrapped_pc_branch_error(&big),
            "-65533 cannot arise without the program counter wrapping"
        );
    }

    /// Range-discover the auto-reserve hits for a BASIC source text.
    fn ranges_for(src: &str) -> Vec<(u16, u16)> {
        let bytes = crate::source::tokenize_program(src).expect("tokenizes");
        let prg = crate::prg::Program::parse(&bytes).expect("prg parses");
        let ast = crate::parse::program(&prg).expect("source parses");
        let module = lower_and_optimize(&ast, Profile::default()).expect("ir lowers");
        discover_reserved_ranges(&module)
    }

    #[test]
    fn auto_reserve_keeps_tight_let_plus_for_interval() {
        let ranges = ranges_for("10 B=12288\n20 FOR I=0 TO 63:POKE B+I,255:NEXT\n");
        assert!(
            ranges.iter().any(|(s, e)| *s <= 12288 && *e >= 12351),
            "a LET base plus a literal-bounded FOR counter is exactly the \
             charset-loader idiom auto-reserve exists for; expected $3000-$303F, \
             got {ranges:?}"
        );
    }

    /// An interval that leaves the 6502's address space is the evaluator losing the variable, not a program that pokes all of RAM.
    #[test]
    fn auto_reserve_discards_interval_past_the_address_space() {
        let ranges = ranges_for("10 Y=56173\n20 FOR I=1024 TO 1063:POKE I+40*Y,160:NEXT\n");
        assert!(
            !ranges
                .iter()
                .any(|(s, e)| *e as u32 - *s as u32 > 0x4000),
            "an interval reaching past $FFFF must be dropped, never clamped into \
             a window-wide reservation; got {ranges:?}"
        );
    }

    #[test]
    fn auto_reserve_never_swallows_the_whole_window() {
        let ranges = ranges_for(
            "10 M=40\n20 Y=56173\n30 FOR I=1024 TO 1063:FOR T=0 TO Y-1:POKE I+T*M,160:NEXT:NEXT\n",
        );
        let total: u32 = ranges.iter().map(|(s, e)| *e as u32 - *s as u32 + 1).sum();
        assert!(
            total < 0x4000,
            "auto-reserve claimed {total} bytes ({ranges:?}); a heuristic that reserves \
             most of the address space cannot be honoured — the code has to go somewhere"
        );
    }

    /// A `FOR` rebinds its counter, so an earlier `LET` range for that name is dead.
    #[test]
    fn for_counter_retracts_stale_let_range() {
        // `T=9000` then `FOR T=0 TO Q` (Q unknown, so the header is unusable).
        let ranges = ranges_for("10 T=9000\n20 FOR T=0 TO Q:POKE 1024+T*40,160:NEXT\n");
        assert!(
            !ranges.iter().any(|(s, _)| *s >= 0x0900),
            "the FOR rebinds T, so the stale T=9000 must not reach the POKE; \
             got {ranges:?}"
        );
    }

    /// An assignment the evaluator cannot read retracts the binding.
    #[test]
    fn unreadable_let_retracts_stale_range() {
        let ranges = ranges_for(
            "10 B=12288\n20 B=PEEK(53280)*256\n30 FOR I=0 TO 63:POKE B+I,255:NEXT\n",
        );
        assert!(
            !ranges.iter().any(|(s, e)| *s <= 12288 && *e >= 12351),
            "B was reassigned from a PEEK, so the old B=12288 must not still be \
             driving the POKE range; got {ranges:?}"
        );
    }

    /// Regression: `POKE 53272, lit` (= POKE V+24, X) — writes the
    /// VIC memory pointer. The bits-3:1 nibble names the active 2KB
    /// charset window; VIC reads the entire 2KB even when the
    /// program writes only part of it. Reserve the window so
    /// emitted code/data can't accidentally show up as garbage
    /// glyphs for unwritten chars.
    #[test]
    fn auto_reserve_picks_up_d018_charset_window() {
        // `10 POKE 53272,28` selects charset at $3000 (bits 3:1 =
        // 110 → 6 * $0800 = $3000). Auto-reserve should add
        // $3000-$37FF.
        let body: Vec<u8> = b"\x97 53272,28".to_vec();
        let mut bytes: Vec<u8> = vec![0x01, 0x08];
        let next_link: u16 = 0x080B + body.len() as u16 + 5;
        bytes.extend_from_slice(&next_link.to_le_bytes());
        bytes.extend_from_slice(&[0x0A, 0x00]);
        bytes.extend_from_slice(&body);
        bytes.push(0x00);
        bytes.extend_from_slice(&[0x00, 0x00]);

        let prg = crate::prg::Program::parse(&bytes).expect("prg parses");
        let ast = crate::parse::program(&prg).expect("source parses");
        let module = lower_and_optimize(&ast, Profile::default()).expect("ir lowers");
        let ranges = discover_reserved_ranges(&module);
        assert!(
            ranges.iter().any(|(s, e)| *s <= 0x3000 && *e >= 0x37FF),
            "auto-reserve must include the $D018=28 charset window \
             $3000-$37FF; got {ranges:?}"
        );
    }

    /// Regression: the canonical C64 MEMSIZ-protect idiom
    /// `POKE 55, lo: POKE 56, hi` — used by every program that
    /// loads custom ML / charset / sprite data into protected
    /// memory — must be detected and used to fill in gaps between
    /// the explicit per-POKE reservations. ML code POKE-loaded
    /// into the protected region typically uses scratch bytes that
    /// BASIC code never POKEs by name. The MEMSIZ hint reserves the
    /// whole protected region.
    #[test]
    fn auto_reserve_uses_memsize_hint_to_fill_gaps() {
        // `10 POKE 55,0:POKE 56,28:FOR I=7168 TO 7393:POKE I,0:NEXT`
        // MEMSIZ = $1C00 (= 28*256). The FOR-loop POKE auto-reserves
        // $1C00-$1CE1. The MEMSIZ hint fills the gap up to wherever
        // the highest cluster member ends — but here $1CE1 *is* the
        // highest, so the consolidated range is $1C00-$1CE1 again.
        // Add a separate POKE further up to test the fill behaviour.
        // Use `$1DFF` as the second target.
        let body: Vec<u8> =
            b"\x97 55,0:\x97 56,28:\x81 I\xb27168\xa47393:\x97 I,0:\x82:\x97 7679,1".to_vec();
        let mut bytes: Vec<u8> = vec![0x01, 0x08];
        let next_link: u16 = 0x080B + body.len() as u16 + 5;
        bytes.extend_from_slice(&next_link.to_le_bytes());
        bytes.extend_from_slice(&[0x0A, 0x00]);
        bytes.extend_from_slice(&body);
        bytes.push(0x00);
        bytes.extend_from_slice(&[0x00, 0x00]);

        let prg = crate::prg::Program::parse(&bytes).expect("prg parses");
        let ast = crate::parse::program(&prg).expect("source parses");
        let module = lower_and_optimize(&ast, Profile::default()).expect("ir lowers");
        let ranges = discover_reserved_ranges(&module);
        // The hint should expand the cluster to start at $1C00 and
        // run to the highest endpoint in the cluster window
        // ($1DFF). The per-POKE-only path would have given two
        // disjoint ranges; with the hint they're consolidated.
        assert!(
            ranges.iter().any(|(s, e)| *s == 0x1C00 && *e >= 0x1DFF),
            "MEMSIZ hint must consolidate into a single $1C00-… \
             range; got {ranges:?}"
        );
    }

    /// Regression: `POKE 53272, 20` — selects ROM upper/graphics
    /// charset at $1000. That's a ROM mirror; the underlying RAM
    /// bytes are free for code emission, so we must NOT reserve
    /// $1000-$17FF (doing so would push code higher and waste 2KB
    /// for a pure ROM-charset program).
    #[test]
    fn auto_reserve_skips_d018_rom_charset_windows() {
        // `10 POKE 53272,20` — bits 3:1 = 010, ROM upper/graphics.
        let body: Vec<u8> = b"\x97 53272,20".to_vec();
        let mut bytes: Vec<u8> = vec![0x01, 0x08];
        let next_link: u16 = 0x080B + body.len() as u16 + 5;
        bytes.extend_from_slice(&next_link.to_le_bytes());
        bytes.extend_from_slice(&[0x0A, 0x00]);
        bytes.extend_from_slice(&body);
        bytes.push(0x00);
        bytes.extend_from_slice(&[0x00, 0x00]);

        let prg = crate::prg::Program::parse(&bytes).expect("prg parses");
        let ast = crate::parse::program(&prg).expect("source parses");
        let module = lower_and_optimize(&ast, Profile::default()).expect("ir lowers");
        let ranges = discover_reserved_ranges(&module);
        assert!(
            !ranges.iter().any(|(s, e)| *s <= 0x1000 && *e >= 0x17FF),
            "auto-reserve must NOT reserve the ROM charset window \
             $1000-$17FF (VIC reads it from ROM, not RAM); \
             got {ranges:?}"
        );
    }

    /// Tokenise `body` as line 10 of a one-line program.
    fn one_line_prg(body: &[u8]) -> Vec<u8> {
        let mut bytes: Vec<u8> = vec![0x01, 0x08];
        let next_link: u16 = 0x080B + body.len() as u16 + 5;
        bytes.extend_from_slice(&next_link.to_le_bytes());
        bytes.extend_from_slice(&[0x0A, 0x00]);
        bytes.extend_from_slice(body);
        bytes.push(0x00);
        bytes.extend_from_slice(&[0x00, 0x00]);
        bytes
    }

    fn reserved_for(body: &[u8]) -> Vec<(u16, u16)> {
        let bytes = one_line_prg(body);
        let prg = crate::prg::Program::parse(&bytes).expect("prg parses");
        let ast = crate::parse::program(&prg).expect("source parses");
        let module = lower_and_optimize(&ast, Profile::default()).expect("ir lowers");
        discover_reserved_ranges(&module)
    }

    /// Does any discovered range cover `a`?
    fn covers(rs: &[(u16, u16)], a: u16) -> bool {
        rs.iter().any(|(s, e)| *s <= a && a <= *e)
    }

    #[test]
    fn auto_reserve_folds_for_limit_that_names_its_own_counter() {

        let r = reserved_for(b"\x81 I\xb212800\xa4I\xaa46:\x97 I,255:\x82");
        assert!(
            r.iter().any(|(s, e)| *s == 0x3200 && *e == 0x322E),
            "limit folds to 12846; got {r:?}"
        );
    }

    #[test]
    fn auto_reserve_follows_a_chained_for_loader() {

        let r = reserved_for(
            b"\x81 I\xb212800\xa4I\xaa46:\x97 I,255:\x82:\x81 I\xb2I\xa4I\xaa38:\x97 I,0:\x82",
        );

        assert!(covers(&r, 0x3200) && covers(&r, 0x322E), "first leg: {r:?}");
        assert!(covers(&r, 0x322F) && covers(&r, 0x3255), "second leg: {r:?}");
        assert!(!covers(&r, 0x3256), "must not run past the chain: {r:?}");
    }

    #[test]
    fn auto_reserve_publishes_no_exit_value_for_an_unreadable_limit() {

        let r = reserved_for(b"\x81 I\xb21\xa4N:\x82:\x97 I\xaa12288,0");
        assert!(
            !r.iter().any(|(s, e)| *e >= 0x3000 && *s <= 0x30FF),
            "an unknown limit leaves the counter unknown; got {r:?}"
        );
    }

    #[test]
    fn auto_reserve_exit_value_is_the_step_past_the_limit() {

        let r = reserved_for(b"\x81 I\xb2100\xa4110\xa94:\x82:\x97 I\xaa12288,0");
        assert!(covers(&r, 0x3070), "I=112 -> $3070; got {r:?}");
        assert!(!covers(&r, 0x306E), "110 would be $306E; got {r:?}");
    }

    #[test]
    fn auto_reserve_exit_value_follows_a_negative_step() {

        let r = reserved_for(b"\x81 I\xb2110\xa4100\xa9\xab4:\x82:\x97 I\xaa12288,0");
        assert!(covers(&r, 0x3062), "I=98 -> $3062; got {r:?}");
    }

    #[test]
    fn auto_reserve_exit_value_when_the_header_starts_past_its_limit() {

        let r = reserved_for(b"\x81 I\xb210\xa45:\x82:\x97 I\xaa12288,0");
        assert!(covers(&r, 0x300B), "I=11 -> $300B; got {r:?}");
        assert!(!covers(&r, 0x300A), "I=10 would be $300A; got {r:?}");
    }

    #[test]
    fn auto_reserve_literal_for_bounds_are_unchanged() {

        let r = reserved_for(b"\x81 I\xb212800\xa412846:\x97 I,255:\x82");
        assert!(
            r.iter().any(|(s, e)| *s == 0x3200 && *e == 0x322E),
            "unchanged; got {r:?}"
        );
    }

    /// Regression (MAD): in bitmap mode `$D018`'s CB13 bit picks an
    /// 8 KB bitmap base relative to the VIC bank, not a 2 KB charset.
    /// Reserving the charset window anyway put a hole at $2000 that
    /// the DATA pool had to be bridged around.
    #[test]
    fn auto_reserve_skips_d018_window_in_bitmap_mode() {
        // 10 POKE 53265,59 : POKE 53272,24
        let plain = reserved_for(b"\x97 53272,24");
        assert!(
            plain.iter().any(|(s, e)| *s == 0x2000 && *e == 0x27FF),
            "text mode still reserves the charset window; got {plain:?}"
        );
        let bitmap = reserved_for(b"\x97 53265,59:\x97 53272,24");
        assert!(
            !bitmap.iter().any(|(s, _)| *s == 0x2000),
            "bitmap mode must not reserve a charset window; got {bitmap:?}"
        );
    }

    /// Same, for a program that moves the VIC bank — `$D018` is
    /// relative to it, so the absolute address is unknowable here.
    #[test]
    fn auto_reserve_skips_d018_window_when_vic_bank_moves() {
        let moved = reserved_for(b"\x97 56576,2:\x97 53272,24");
        assert!(
            !moved.iter().any(|(s, _)| *s == 0x2000),
            "a moved VIC bank must not reserve a charset window; got {moved:?}"
        );
    }

    /// Infer the character window from a VIC register read-modify-write.
    #[test]
    fn auto_reserve_reads_the_d018_charset_window_out_of_a_read_modify_write() {
        for body in [
            // POKE 53272,(PEEK(53272)AND240)OR12
            b"\x97 53272,(\xc2(53272)\xaf240)\xb012".to_vec(),
            // POKE 53272,12 OR (PEEK(53272)AND240)
            b"\x97 53272,12\xb0(\xc2(53272)\xaf240)".to_vec(),
            // Select the character window while preserving the screen offset.
            b"\x97 53272,(\xc2(53272)\xaf240)\xaa12".to_vec(),
        ] {
            let ranges = reserved_for(&body);
            assert!(
                ranges.iter().any(|(s, e)| *s <= 0x3000 && *e >= 0x37FF),
                "the RMW form names the $3000 charset window as firmly as a \
                 literal does; got {ranges:?}"
            );
        }
    }

    #[test]
    fn auto_reserve_ignores_a_d018_write_that_does_not_pin_the_charset_bits() {
        // POKE 53272,PEEK(53272)OR8 ; bit 3 forced, bits 2:1 unknown.
        let loose = reserved_for(b"\x97 53272,\xc2(53272)\xb08");
        assert!(
            !loose.iter().any(|(s, e)| e.saturating_sub(*s) == 0x07FF),
            "an unmasked OR pins only bit 3, so no 2 KB window is named; \
             got {loose:?}"
        );

        let partial = reserved_for(b"\x97 53272,(\xc2(53272)\xaf246)\xb012");
        assert!(
            !partial.iter().any(|(s, e)| e.saturating_sub(*s) == 0x07FF),
            "a mask that does not clear the whole field leaves the window \
             ambiguous; got {partial:?}"
        );
    }

    #[test]
    fn d018_read_modify_write_still_honours_the_rom_and_bank_vetoes() {

        let rom = reserved_for(b"\x97 53272,(\xc2(53272)\xaf240)\xb020");
        assert!(
            !rom.iter().any(|(s, e)| *s <= 0x1000 && *e >= 0x17FF),
            "the ROM charset window must stay free for code; got {rom:?}"
        );

        let moved = reserved_for(b"\x97 56576,2:\x97 53272,(\xc2(53272)\xaf240)\xb012");
        assert!(
            !moved.iter().any(|(s, _)| *s == 0x3000),
            "a moved VIC bank must not reserve a charset window; got {moved:?}"
        );
    }

    #[test]
    fn known_bits_tracks_and_or_masks() {
        use crate::ast::BinOp;
        let num = |n: f64| ir::Expr::Number(n);
        let peek = || ir::Expr::Peek(Box::new(num(53272.0)));
        let bin = |op, l: ir::Expr, r: ir::Expr| ir::Expr::Bin(op, Box::new(l), Box::new(r));

        assert_eq!(known_bits(&num(28.0)), (28, 0xFF));
        assert_eq!(known_bits(&peek()), (0, 0x00));
        // AND with a literal: the cleared bits become known zeros.
        assert_eq!(known_bits(&bin(BinOp::And, peek(), num(240.0))), (0, 0x0F));
        // OR with a literal: the set bits become known ones.
        assert_eq!(known_bits(&bin(BinOp::Or, peek(), num(12.0))), (12, 0x0C));
        // Composed, the low nibble is fully determined.
        let rmw = bin(
            BinOp::Or,
            bin(BinOp::And, peek(), num(240.0)),
            num(12.0),
        );
        assert_eq!(known_bits(&rmw), (12, 0x0F));
        // `+` is `OR` only where no carry is possible.
        let add_rmw = bin(
            BinOp::Add,
            bin(BinOp::And, peek(), num(240.0)),
            num(12.0),
        );
        assert_eq!(known_bits(&add_rmw), (12, 0x0F));

        let carry = bin(
            BinOp::Add,
            bin(BinOp::And, peek(), num(246.0)),
            num(12.0),
        );
        assert_eq!(known_bits(&carry), (0, 0x00));
        // Two unknowns cannot be added bit-wise at all.
        assert_eq!(known_bits(&bin(BinOp::Add, peek(), peek())), (0, 0x00));

        assert_eq!(known_bits(&bin(BinOp::Mul, num(1.0), num(2.0))), (0, 0x00));
    }

    /// Regression: the hoist matched a section header that no longer
    /// existed once the layout note was added to it, so it silently
    /// did nothing and pools were left to be bridged in place.
    #[test]
    fn data_pool_is_hoisted_ahead_of_reservations() {
        let asm = "*=$080D\n    RTS\n\n; --- DATA pool (ASCII length-prefixed) ---\n\
                   __DATA:\n    .byte $01,$31  ; #0\n\n; --- next ---\n";
        let (out, absorbed) = relocate_data_past_reservations(asm, &[(0x0900, 0x09FF)]);
        let jmp = out.find("JMP __DATA_HOISTED_END").expect("bridge emitted");
        let pool = out.find("__DATA:").expect("pool kept");
        let rts = out.find("    RTS").expect("code kept");
        assert!(
            jmp < pool && pool < rts,
            "pool moved ahead of the code:\n{out}"
        );
        // Two bytes of pool at $0810 clear $0900 on their own ; no fill, and nothing withheld from the assembler.
        assert!(absorbed.is_empty(), "nothing to absorb: {absorbed:?}");
        assert!(!out.contains("__DATA_POOL_FILL"), "no fill needed:\n{out}");
    }

    #[test]
    fn a_long_pool_is_pushed_above_a_low_reservation() {

        let mut asm = String::from("*=$080D\n    RTS\n\n; --- DATA pool ---\n__DATA:\n");
        for _ in 0..32 {
            asm.push_str("    .byte ");
            asm.push_str(&vec!["$41"; 16].join(","));
            asm.push('\n');
        }
        asm.push_str("\n; --- next ---\n");
        let (out, absorbed) = relocate_data_past_reservations(&asm, &[(0x0971, 0x0971)]);
        assert_eq!(absorbed, vec![(0x0971, 0x0971)], "the range was absorbed");
        let fill_at = out.find("__DATA_POOL_FILL:").expect("fill emitted");
        let pool_at = out.find("__DATA:").expect("pool kept");
        assert!(fill_at < pool_at, "fill comes first:\n{out}");
        // $0810 up to and including $0971 is 0x162 = 354 bytes.
        let fill_bytes: usize = out[fill_at..pool_at]
            .lines()
            .filter_map(|l| l.trim_start().strip_prefix(".byte"))
            .map(|a| a.split(',').count())
            .sum();
        assert_eq!(
            fill_bytes,
            0x0972 - 0x0810,
            "pool starts just past the hole"
        );
    }

    #[test]
    fn a_withheld_range_outside_the_fill_is_refused() {
        let lookup = |name: &str| match name {
            "__DATA_POOL_FILL" => Some(0x0810u16),
            "__DATA_HOISTED_END" => Some(0x0900u16),
            _ => None,
        };
        assert!(verify_absorbed_ranges_filled(&[(0x0850, 0x0860)], lookup).is_ok());
        let err = verify_absorbed_ranges_filled(&[(0x0900, 0x0910)], lookup)
            .expect_err("a range past the fill must be refused");
        assert!(err.contains("$0900-$0910"), "names the range: {err}");

        let err = verify_absorbed_ranges_filled(&[(0x0850, 0x0860)], |_| None)
            .expect_err("missing labels must be refused");
        assert!(err.contains("fill labels are not in the image"), "{err}");
    }

    #[test]
    fn a_for_counter_the_body_reassigns_reserves_nothing() {

        let src = "25 FORA=0TO53287:READN:IFN>255THENA=N:READN\n\
                   26 POKEA,N:NEXT\n\
                   27 DATA 49152,1,2,3\n";
        let bytes = crate::source::tokenize_program(src).expect("tokenize");
        let prg = crate::prg::Program::parse(&bytes).expect("prg parses");
        let ast = crate::parse::program(&prg).expect("parse");
        let module = lower_and_optimize(&ast, Profile::Default).expect("lower");
        let ranges = discover_reserved_ranges(&module);
        assert!(
            !ranges.iter().any(|&(lo, hi)| hi - lo > 0x2000),
            "the sentinel bound must not become a huge reservation; got {ranges:?}"
        );
    }

    #[test]
    fn a_plain_poke_loop_still_reserves_its_range() {
        let src = "25 FORA=12288TO12295:READN:POKEA,N:NEXT\n\
                   27 DATA 1,2,3,4,5,6,7,8\n";
        let bytes = crate::source::tokenize_program(src).expect("tokenize");
        let prg = crate::prg::Program::parse(&bytes).expect("prg parses");
        let ast = crate::parse::program(&prg).expect("parse");
        let module = lower_and_optimize(&ast, Profile::Default).expect("lower");
        let ranges = discover_reserved_ranges(&module);
        assert!(
            ranges
                .iter()
                .any(|&(lo, hi)| lo <= 0x3000 && hi >= 0x3007),
            "the literal loop must still be reserved; got {ranges:?}"
        );
    }

    /// A reservation the pool can't be moved clear of has to be an
    /// error: the assembler's JMP bridge would be read as a value.
    #[test]
    fn data_pool_split_by_a_reservation_is_rejected() {
        let lookup = |name: &str| match name {
            "__DATA" => Some(0x0810u16),
            "__DATA_HOISTED_END" => Some(0x2D58u16),
            _ => None,
        };
        assert!(verify_data_pool_unsplit(&[(0x4000, 0x4FFF)], lookup).is_ok());
        let err = verify_data_pool_unsplit(&[(0x2000, 0x27FF)], lookup)
            .expect_err("overlap must be rejected");
        assert!(err.contains("$2000-$27FF"), "names the range: {err}");
    }

    /// Tokenize ASCII BASIC source and compile with defaults.
    fn compile_src(src: &str) -> Compiled {
        let prg = crate::source::tokenize_program(src).expect("tokenize");
        compile_with_options(&prg, CompileOptions::default()).expect("compile")
    }

    /// Whether the prologue initializes the shared CURLIN high byte.
    fn hoists_curlin_hi(asm: &str) -> bool {
        match (asm.find("STA $3A"), asm.find("STA $39")) {
            (Some(hi), Some(lo)) => hi < lo,
            (Some(_), None) => true,
            (None, _) => false,
        }
    }

    #[test]
    fn question_mark_print_is_compiled_not_dropped() {
        // `?` is the PRINT abbreviation.
        let c = compile_src("10 ?\"HI\"\n20 END\n");
        assert!(
            c.diagnostics.skipped_statements.is_empty(),
            "nothing may be dropped: {:?}",
            c.diagnostics.skipped_statements
        );
        assert!(
            c.asm.contains("STR_PRINT") || c.asm.contains("$AB1E") || c.asm.contains("$FFD2"),
            "the PRINT must actually be emitted:\n{}",
            c.asm
        );
    }

    #[test]
    fn question_mark_print_matches_spelled_out_print() {
        // The abbreviation must compile to exactly the same program as the long form.
        let short = compile_src("10 ?\"HI\":?42\n20 END\n");
        let long = compile_src("10 PRINT\"HI\":PRINT42\n20 END\n");
        assert_eq!(
            short.machine_code, long.machine_code,
            "`?` and PRINT must produce identical code"
        );
    }

    #[test]
    fn question_mark_print_survives_inside_a_conditional() {

        let c = compile_src("10 A=1\n20 IF A=1 THEN ?\"YES\"\n30 END\n");
        assert!(
            c.diagnostics.skipped_statements.is_empty(),
            "conditional body must survive: {:?}",
            c.diagnostics.skipped_statements
        );
    }

    #[test]
    fn curlin_hi_hoisted_when_all_lines_share_a_high_byte() {
        // Float division makes every line fallible, so each one wants a CURLIN stamp.
        let c = compile_src("10 A=1/2\n20 PRINT A\n30 B=A/3\n40 PRINT B\n50 END\n");
        assert!(
            hoists_curlin_hi(&c.asm),
            "high byte belongs in the prologue:\n{}",
            c.asm
        );
        assert_eq!(
            c.asm.matches("STA $3A").count(),
            1,
            "exactly one high-byte stamp for the whole module:\n{}",
            c.asm
        );
        assert!(
            c.asm.matches("STA $39").count() >= 2,
            "every fallible line still stamps its own low byte:\n{}",
            c.asm
        );
    }

    #[test]
    fn curlin_hi_not_hoisted_across_mixed_high_bytes() {

        let c = compile_src("10 A=1/2\n20 PRINT A\n300 B=A/3\n310 PRINT B\n320 END\n");
        assert!(
            !hoists_curlin_hi(&c.asm),
            "mixed high bytes must not hoist:\n{}",
            c.asm
        );
    }

    #[test]
    fn curlin_hi_not_hoisted_when_program_uses_sys() {

        let c = compile_src("10 A=1/2\n20 PRINT A\n30 SYS 49152\n40 B=A/3\n50 PRINT B\n60 END\n");
        assert!(
            !hoists_curlin_hi(&c.asm),
            "SYS must disable the invariant:\n{}",
            c.asm
        );
    }

    #[test]
    fn curlin_hi_not_hoisted_when_program_uses_usr() {
        let c = compile_src("10 A=1/2\n20 PRINT A\n30 B=USR(3)\n40 PRINT B\n50 END\n");
        assert!(
            !hoists_curlin_hi(&c.asm),
            "USR must disable the invariant:\n{}",
            c.asm
        );
    }

    #[test]
    fn absx_static_int_array_uses_indexed_addressing() {

        let c = compile_src(
            "10 DIM A%(10)\n20 J%=PEEK(830)\n30 A%(J%)=5\n40 PRINT A%(J%)\n50 END\n",
        );
        assert!(
            c.asm.contains("__ARR_A_I,X"),
            "read/write go through abs,X:\n{}",
            c.asm
        );

        assert!(
            c.asm.contains("CMP #$0B") && c.asm.contains("__BAD_SUBSCRIPT"),
            "?BAD SUBSCRIPT semantics preserved via byte compare:\n{}",
            c.asm
        );
    }

    #[test]
    fn absx_bounds_check_elided_for_proven_counter() {
        let c = compile_src("10 DIM A%(10)\n20 FOR J=0 TO 10\n30 A%(J)=J\n40 NEXT\n50 END\n");
        assert!(
            c.asm.contains("__ARR_A_I,X"),
            "store goes through abs,X:\n{}",
            c.asm
        );
        assert!(
            !c.asm.contains("__ABX_OK") && !c.asm.contains("__BNDS"),
            "FOR range proof elides every bounds check:\n{}",
            c.asm
        );
    }

    #[test]
    fn absx_declines_oversized_and_runtime_arrays() {

        let big =
            compile_src("10 DIM A%(200)\n20 J%=PEEK(830)\n30 PRINT A%(J%)\n40 END\n");
        assert!(
            !big.asm.contains("__ARR_A_I,X"),
            "oversized array stays on the pointer path:\n{}",
            big.asm
        );

        let rt = compile_src(
            "10 N=PEEK(830)+5\n20 DIM A%(N)\n30 J%=PEEK(831)\n40 PRINT A%(J%)\n50 END\n",
        );
        assert!(
            !rt.asm.contains("__ARR_A_I,X"),
            "runtime array stays on the pointer path:\n{}",
            rt.asm
        );
    }

    // indirect-JMP vector page alignment: the plumbing

    #[test]
    fn indirect_jmp_operands_ignores_direct_jumps_and_prose() {
        let asm = "\
    JMP __PLAIN
    JMP $E37B
; the DEF FN dispatcher deliberately avoids JMP (__FNV_A)
    JMP (__FOR_DISP_VEC)
    JSR (__NOT_A_JMP)
JMPTBL:
    JMP ($0311)
    JMP (__FOR_DISP_VEC)   ; a second site for the same vector
    jmp\t(__LOWER)
";
        assert_eq!(
            indirect_jmp_operands(&asm.replace("\\t", "\t")),
            vec![
                "__FOR_DISP_VEC".to_string(),
                "$0311".to_string(),
                "__LOWER".to_string()
            ],
        );
    }

    #[test]
    fn vector_operands_resolve_through_labels_literals_and_offsets() {
        let asm = "    JMP (__V)\n    JMP (__V+2)\n    JMP (__W-1)\n    JMP ($0311)\n";
        let got = resolve_indirect_vectors(asm, |n| match n {
            "__V" => Some(0x9000),
            "__W" => Some(0x9100),
            _ => None,
        })
        .expect("all four resolve");
        assert_eq!(
            got,
            vec![
                ("__V".to_string(), 0x9000),
                ("__V+2".to_string(), 0x9002),
                ("__W-1".to_string(), 0x90FF),
                ("$0311".to_string(), 0x0311),
            ]
        );
        // Only the one that lands on the last byte of a page is bad.
        assert_eq!(
            vector_page_offenders(&got),
            vec![("__W-1".to_string(), 0x90FF)]
        );
    }

    #[test]
    fn unresolvable_vector_operand_is_an_error_not_a_silent_pass() {

        let err = resolve_indirect_vectors("    JMP (__MYSTERY)\n", |_| None)
            .expect_err("unknown label is rejected");
        assert!(err.contains("__MYSTERY"), "{err}");
    }

    #[test]
    fn pad_goes_in_front_of_the_vector_definition() {
        let asm = "__OTHER__V:\n    .byte $00\n__V:\n    .byte $00,$00\n";
        let out = insert_vector_pad(asm, "__V+1").expect("label found");

        let pad_at = out.find("    .byte $00\n__V:").expect("pad before __V");
        assert!(out[..pad_at].contains("__OTHER__V:"));

        assert_eq!(out.matches("    .byte $00\n").count(), 2);

        assert!(insert_vector_pad(asm, "$0311").is_none());
    }

    /// A symbol table for the guard tests.
    fn syms(pairs: &[(&str, u16)]) -> impl Fn(&str) -> Option<u16> + use<> {
        let map: std::collections::HashMap<String, u16> =
            pairs.iter().map(|(n, a)| ((*n).to_string(), *a)).collect();
        move |name: &str| map.get(name).copied()
    }

    #[test]
    fn indexed_base_inside_zero_page_is_refused() {
        let none = syms(&[]);
        // `PEEK(200 + I%)`.
        let bad = "    LDX VI_X\n    LDY $00C8,X\n";
        let err = verify_no_narrowed_index(bad, &none).expect_err("must refuse");
        assert!(err.contains("$00C8"), "{err}");
        assert!(err.contains("zeropage,X"), "{err}");

        verify_no_narrowed_index("    LDY $0100,X\n", &none).expect("above zero page is fine");
        // Base $0000 is the other wrap-free case: `0 + X` never leaves the page, so the two
        // encodings agree for every index.
        verify_no_narrowed_index("    LDA $0000,Y\n", &none).expect("base zero is fine");
        verify_no_narrowed_index("    LDA $0000,X\n", &none).expect("base zero is fine");
        // `LDA abs,Y` has no `zeropage,Y` encoding at all, so a low base cannot narrow it.
        verify_no_narrowed_index("    LDA $0080,Y\n", &none).expect("no zeropage,Y for LDA");

        assert!(verify_no_narrowed_index("    LDX $0080,Y\n", &none).is_err());
    }

    #[test]
    fn indexed_base_is_checked_through_labels_too() {
        // A zero-page-promoted variable used as an index base: nothing in the operand text says
        // `$00xx`, only the symbol table does.
        let zp = syms(&[("VI_K", 0x00C8), ("__TABLE", 0x1234)]);
        assert!(verify_no_narrowed_index("    STA VI_K,X\n", &zp).is_err());
        verify_no_narrowed_index("    STA __TABLE,X\n", &zp).expect("data label is fine");
        verify_no_narrowed_index("    STA __TABLE+1,X\n", &zp).expect("offset label is fine");
    }

    #[test]
    fn indirect_pointer_outside_zero_page_is_refused() {

        let high = syms(&[("V_P", 0x0834)]);
        let err = verify_indirect_zp_pointers("    LDA (V_P),Y\n", &high).expect_err("refuse");
        assert!(err.contains("$0834"), "{err}");
        assert!(err.contains("$34"), "{err}");
        // The `(zp,X)` form truncates the same way.
        assert!(verify_indirect_zp_pointers("    STA (V_P,X)\n", &high).is_err());
        // A real promotion resolves inside zero page and is fine.
        let zp = syms(&[("V_P", 0x00FB)]);
        verify_indirect_zp_pointers("    LDA (V_P),Y\n", &zp).expect("zero page is fine");
        // Literal operands, which is what nearly every site emits.
        let none = syms(&[]);
        verify_indirect_zp_pointers("    LDA ($FD),Y\n", &none).expect("literal zp is fine");
        // `JMP (vector)` is a 16-bit indirect and belongs to the vector-alignment pass, not to
        // this one.
        verify_indirect_zp_pointers("    JMP (__FOR_DISP_VEC)\n", &none).expect("JMP is not ours");
    }

    #[test]
    fn indirect_pointer_on_the_last_zero_page_byte_is_refused() {
        let none = syms(&[]);
        // `LDA ($FF),Y` reads the high byte from $0000, not $0100.
        let err = verify_indirect_zp_pointers("    LDA ($FF),Y\n", &none).expect_err("refuse");
        assert!(err.contains("$00FF"), "{err}");
        // One byte down the pair is contiguous and legal.
        verify_indirect_zp_pointers("    LDA ($FE),Y\n", &none).expect("$FE/$FF is a real pair");
    }

    #[test]
    fn smc_patch_past_the_end_of_its_template_is_refused() {
        let none = syms(&[]);

        let good = "    LDA #$00\n    STA __SITE+1\n    LDA #$08\n    STA __SITE+2\n\
                    __SITE:\n    LDA $FFFF\n    RTS\n";
        verify_smc_patch_offsets(good, &none).expect("three-byte template is fine");

        let bad = "    LDA #$00\n    STA __SITE+1\n    LDA #$08\n    STA __SITE+2\n\
                   __SITE:\n    LDA $00FF,X\n    RTS\n";
        let err = verify_smc_patch_offsets(bad, &none).expect_err("must refuse");
        assert!(err.contains("+2"), "{err}");
        assert!(err.contains("2-byte"), "{err}");

        let one_only = "    STA __SITE+1\n__SITE:\n    LDA $00FF,X\n    RTS\n";
        verify_smc_patch_offsets(one_only, &none).expect("+1 is inside a two-byte instruction");
        // `+1` on an implied one-byte instruction is not.
        let implied = "    STA __SITE+1\n__SITE:\n    RTS\n";
        assert!(verify_smc_patch_offsets(implied, &none).is_err());
    }

    #[test]
    fn smc_check_leaves_data_labels_alone() {
        let none = syms(&[]);
        // `V_A+2` is the third byte of a five-byte float slot, and `__ARR_A_I+1,X` the high
        // half of an integer array element.
        let data = "    LDA V_A+2\n    LDA __ARR_A_I+1,X\n\
                    V_A:\n    .byte $00,$00,$00,$00,$00\n\
                    __ARR_A_I:\n    .byte $00,$00,$00,$00\n";
        verify_smc_patch_offsets(data, &none).expect("data offsets are not SMC patches");

        let run = "    STA __HCS_SRC+2\n__HCS_PAGE:\n__HCS_SRC:\n    LDA $0000,Y\n";
        verify_smc_patch_offsets(run, &none).expect("abs,Y template is three bytes");
        let run_bad = "    STA __HCS_SRC+2\n__HCS_PAGE:\n__HCS_SRC:\n    LDA $0000,X\n";
        assert!(
            verify_smc_patch_offsets(run_bad, &none).is_err(),
            "LDA $0000,X is zeropage,X — two bytes — so +2 is off the end"
        );
    }

    // reserved-range bridge orphans

    fn bridge(target: u16) -> Vec<Item> {
        vec![
            Item::Pad(0),
            Item::Instruction {
                mnemonic: "JMP".to_string(),
                operand: Some(format!("${target:04X}")),
            },
            Item::Pad(4),
        ]
    }

    #[test]
    fn bridge_orphans_reports_the_whole_label_run_before_a_bridge() {
        let mut items = vec![
            Item::Label("__EARLIER".into()),
            Item::Data(Vec::new()),
            Item::Label("__A".into()),
            Item::Label("__B".into()),
        ];
        items.extend(bridge(0x3880));
        items.push(Item::Data(Vec::new()));
        let got = bridge_orphans(&items, &[(0x3800, 0x387F)]);
        assert_eq!(
            got,
            vec![BridgeOrphan {
                labels: vec!["__A".into(), "__B".into()],
                r_start: 0x3800,
                r_end: 0x387F,
            }],
            "both stacked labels sit on the bridge; the one before the \
             intervening data item does not"
        );
    }

    #[test]
    fn a_bridge_with_no_label_in_front_of_it_is_not_an_orphan() {
        // The repaired shape: filler takes the bridge, the label follows it.
        let mut items = vec![Item::Data(Vec::new())];
        items.extend(bridge(0x3880));
        items.push(Item::Data(Vec::new()));
        items.push(Item::Label("__A".into()));
        items.push(Item::Data(Vec::new()));
        assert!(bridge_orphans(&items, &[(0x3800, 0x387F)]).is_empty());
    }

    #[test]
    fn a_plain_jmp_that_happens_to_target_past_a_reservation_is_not_a_bridge() {
        // Without the leading `Pad` marker this is user code, not an assembler bridge.
        let items = vec![
            Item::Label("__A".into()),
            Item::Instruction {
                mnemonic: "JMP".to_string(),
                operand: Some("$3880".to_string()),
            },
            Item::Pad(4),
        ];
        assert!(bridge_orphans(&items, &[(0x3800, 0x387F)]).is_empty());
    }

    #[test]
    fn a_zero_page_equate_beside_a_bridge_is_not_reported_as_stranded() {

        let mut asm = String::from("*=$0800\n");
        for _ in 0..0xF0 {
            asm.push_str("    NOP\n");
        }
        asm.push_str("MYDATA:\nZPSLOT = $FB\n    .byte ");
        let bytes: Vec<String> = (1..=32u32).map(|n| format!("${n:02X}")).collect();
        asm.push_str(&bytes.join(","));
        asm.push('\n');

        let mut a = Assembler6502::new();
        a.add_reserved_range(0x0900, 0x09FF).expect("range accepted");
        let (_, items) = a.assemble_full(&asm).expect("assembles");
        assert_eq!(a.lookup("ZPSLOT"), Some(0x00FB), "equate keeps its value");
        assert_eq!(
            bridge_orphans(&items, &[(0x0900, 0x09FF)]),
            vec![BridgeOrphan {
                labels: vec!["MYDATA".into()],
                r_start: 0x0900,
                r_end: 0x09FF,
            }],
            "only the label is stranded; the equate must not be listed"
        );
    }

    #[test]
    fn assembler_generated_skip_labels_need_no_repair() {

        let orphan = BridgeOrphan {
            labels: vec!["__skip_7".into()],
            r_start: 0x3800,
            r_end: 0x387F,
        };
        assert!(orphan.resolve(|_| Some(0x37FD)).is_none());
    }

    #[test]
    fn the_repair_skips_the_pool_end_marker_and_moves_the_label_behind_it() {

        let orphan = BridgeOrphan {
            labels: vec!["__DATA_END".into(), "__ARR_BM_I".into()],
            r_start: 0x132A,
            r_end: 0x132B,
        };
        let resolved = orphan.resolve(|_| Some(0x0D24)).expect("array is repairable");
        assert_eq!(resolved.labels, vec!["__ARR_BM_I".to_string()]);
    }

    #[test]
    fn an_end_marker_alone_in_the_run_still_takes_the_filler() {
        // The marker steps aside only when something else can take the filler.
        let orphan = BridgeOrphan {
            labels: vec!["__DATA_END".into()],
            r_start: 0x3800,
            r_end: 0x387F,
        };
        let resolved = orphan.resolve(|_| Some(0x37FD)).expect("still repairable");
        assert_eq!(resolved.labels, vec!["__DATA_END".to_string()]);
    }

    #[test]
    fn every_end_marker_steps_aside_for_a_label_that_names_bytes() {
        // `__DATA_END`, `__DATA_HOISTED_END`, `__BSS_END` and `__EXRAM_DATA_END` all bound the run in front of them.
        for marker in [
            "__DATA_END",
            "__DATA_HOISTED_END",
            "__BSS_END",
            "__CP_END",
            crate::extraram::DATA_END_LABEL,
        ] {
            let orphan = BridgeOrphan {
                labels: vec![marker.into(), "__ARR_A_I".into()],
                r_start: 0x3800,
                r_end: 0x387F,
            };
            let resolved = orphan.resolve(|_| Some(0x37FD)).expect("array is repairable");
            assert_eq!(resolved.labels, vec!["__ARR_A_I".to_string()], "{marker}");
        }
    }

    const POOL_ASM: &str = "__DATA:\n\
                            __DATA_LINE_100:\n\
                            \x20   .byte $01,$00  ; #0 = Float 1\n\
                            \x20   .byte $02,$00  ; #1 = Float 2\n\
                            \x20   .byte $03,$00  ; #2 = Float 3\n\
                            \x20   .byte $04,$00  ; #3 = Float 4\n\
                            __DATA_END:\n";

    #[test]
    fn a_pool_clear_of_every_reservation_takes_no_clearance_fill() {

        let far = &[(0x3000u16, 0x303Fu16)];
        assert_eq!(
            data_pool_clearance_fill(POOL_ASM, far, |n| (n == "__DATA").then_some(0x0C74)),
            Ok(None)
        );

        let touching = &[(0x0C70u16, 0x0C73u16), (0x0C7Cu16, 0x0C8Fu16)];
        assert_eq!(
            data_pool_clearance_fill(POOL_ASM, touching, |n| (n == "__DATA").then_some(0x0C74)),
            Ok(None)
        );
    }

    #[test]
    fn a_reservation_inside_the_pool_is_measured_from_the_entries_not_the_labels() {
        // The pool is 8 bytes at $0C74-$0C7B and $0C78-$0C7F is reserved, so the pool has to start at $0C80: four bytes of fill.
        let inside = &[(0x0C78u16, 0x0C7Fu16)];
        assert_eq!(
            data_pool_clearance_fill(POOL_ASM, inside, |n| match n {
                "__DATA" => Some(0x0C74),
                "__DATA_END" => Some(0x1000), // already split; must be ignored
                _ => None,
            }),
            Ok(Some(0x0C80 - 0x0C74))
        );
    }

    #[test]
    fn clearing_one_reservation_walks_the_pool_past_the_next() {

        let two = &[(0x0C78u16, 0x0C7Fu16), (0x0C80u16, 0x0C8Fu16)];
        assert_eq!(
            data_pool_clearance_fill(POOL_ASM, two, |n| (n == "__DATA").then_some(0x0C74)),
            Ok(Some(0x0C90 - 0x0C74))
        );
    }

    #[test]
    fn a_pool_that_cannot_be_cleared_below_the_top_of_memory_is_refused() {
        let high = &[(0x0C74u16, 0xFFFFu16)];
        let err = data_pool_clearance_fill(POOL_ASM, high, |n| (n == "__DATA").then_some(0x0C74))
            .expect_err("no room above the reservation");
        assert!(err.contains("?OUT OF MEMORY"), "{err}");
        assert!(err.contains("--no-auto-reserve"), "{err}");
    }

    #[test]
    fn clearance_fill_goes_in_front_of_the_pool_and_nowhere_else() {
        let out = insert_data_pool_clearance(POOL_ASM, 4).expect("__DATA: is in the asm");
        let at = out.find("__DATA:").expect("label survives");
        let head = &out[..at];
        assert_eq!(head.matches("$00").count(), 4, "{out}");
        assert!(head.contains("dead fill"), "{out}");
        // The entries themselves are untouched and still in order.
        let tail = &out[at..];
        assert_eq!(tail, POOL_ASM, "{out}");

        assert_eq!(
            insert_data_pool_clearance(POOL_ASM, 0).as_deref(),
            Some(POOL_ASM)
        );
    }

    #[test]
    fn clearance_fill_without_a_pool_label_is_reported_not_guessed() {
        assert!(insert_data_pool_clearance("    NOP\n", 4).is_none());
    }

    #[test]
    fn an_image_with_no_text_copy_is_not_checked() {
        verify_embedded_text_pad("    NOP\n", 0x080D, 0x400).expect("no marker, no copy, no check");
    }

    #[test]
    fn a_text_copy_that_fits_in_always_ram_is_accepted() {
        // 4 KB of text at $8001 ends at $9000, and a 6 KB image ends at $1FEC ; both clear.
        let asm = format!("{BASIC_TEXT_COPY_LEN_MARKER}4096\n__BASIC_TEXT_SRC:\n");
        verify_embedded_text_pad(&asm, 0x080D, 6_112).expect("fits");
    }

    #[test]
    fn a_text_copy_past_the_top_of_always_ram_is_refused() {
        // The copy reaches $F955 and writes VIC, SID, and CIA registers.
        let asm = format!("{BASIC_TEXT_COPY_LEN_MARKER}31061\n__BASIC_TEXT_SRC:\n");
        let err = verify_embedded_text_pad(&asm, 0x080D, 0x400).expect_err("refused");
        assert!(err.contains("$D000-$DFFF"), "{err}");
        assert!(err.contains("$2B/$2C"), "{err}");
        assert!(err.contains("$F955"), "{err}");
    }

    #[test]
    fn a_text_copy_that_lands_on_the_image_is_refused() {

        let asm = format!("{BASIC_TEXT_COPY_LEN_MARKER}1024\n__BASIC_TEXT_SRC:\n");
        let err = verify_embedded_text_pad(&asm, 0x080D, 0x8100 - 0x080D + 1)
            .expect_err("the copy overlaps the image");
        assert!(err.contains("$8001"), "{err}");
        assert!(err.contains("$080D-$8100"), "{err}");
        assert!(err.contains("256 byte(s)"), "{err}");
    }

    #[test]
    fn a_text_copy_whose_marker_went_missing_is_an_internal_error() {

        let err = verify_embedded_text_pad("__BASIC_TEXT_SRC:\n    .byte $01\n", 0x080D, 0x400)
            .expect_err("the extent cannot be checked");
        assert!(err.contains("__BASIC_TEXT_COPY_LEN"), "{err}");
    }

    #[test]
    fn shim_lets_the_bridge_land_in_front_of_filler_not_the_label() {
        let asm = "S78:\n    .byte $01\nS79:\n    .byte $02,$03\n";
        let orphan = ResolvedOrphan {
            labels: vec!["S79".into()],
            r_start: 0x3800,
            r_end: 0x387E,
            addr: 0x37D6,
        };
        let out = insert_bridge_shim(asm, &orphan).expect("label found");
        let at = out.find("\nS79:\n").expect("label survives") + 1;
        let head = &out[..at];

        assert!(head.contains("    JMP S79\n"), "{out}");

        assert_eq!(head.matches("$EA").count(), 36 + 1, "{out}");
        assert!(!head.contains("$00"), "the fill must not be BRK: {out}");
        assert!(head.ends_with("    .byte $EA\n"), "{out}");

        assert!(out.starts_with("S78:\n    .byte $01\n"), "{out}");
    }

    #[test]
    fn a_shim_too_close_to_the_reservation_degrades_to_bare_filler() {

        let asm = "S79:\n    .byte $02\n";
        let orphan = ResolvedOrphan {
            labels: vec!["S79".into()],
            r_start: 0x3800,
            r_end: 0x387E,
            addr: 0x37FD,
        };
        let out = insert_bridge_shim(asm, &orphan).expect("label found");
        assert!(!out.contains("    JMP "), "{out}");
        let at = out.find("\nS79:\n").expect("label survives") + 1;
        assert_eq!(out[..at].matches("$EA").count(), 1, "{out}");
        assert!(!out[..at].contains("$00"), "the fill must not be BRK: {out}");
    }

    #[test]
    fn shim_finds_nothing_to_splice_in_front_of() {
        let orphan = ResolvedOrphan {
            labels: vec!["__NOT_IN_THE_TEXT".into()],
            r_start: 0x3800,
            r_end: 0x387E,
            addr: 0x37D6,
        };
        assert!(insert_bridge_shim("S79:\n    .byte $02\n", &orphan).is_none());
    }

    #[test]
    fn label_bytes_check_accepts_a_label_on_its_own_data() {
        let asm = "*=$0801\n__A:\n    .byte $27,$93,$11\n";
        assert!(verify_label_bytes(asm, &[0x27, 0x93, 0x11], 0x0801, |_| Some(0x0801)).is_ok());
    }

    #[test]
    fn label_bytes_check_catches_a_label_stranded_on_a_bridge() {
        // What SNOOPY shipped: `S79` linked at the bridge, so the image holds `4C 7F 38 00`
        // where the string's length prefix and first three bytes belong.
        let asm = "*=$0801\nS79:\n    .byte $27,$93,$11,$11\n";
        let image = [0x4C, 0x7F, 0x38, 0x00, 0x27, 0x93, 0x11, 0x11];
        let err = verify_label_bytes(asm, &image, 0x0801, |_| Some(0x0801))
            .expect_err("a label off its own bytes must be rejected");
        assert!(err.contains("S79"), "{err}");
    }

    #[test]
    fn label_bytes_check_skips_what_it_cannot_compare() {

        assert_eq!(leading_data_literals("<S1,>S1", false, 4), Vec::<u8>::new());
        assert_eq!(
            leading_data_literals("$01,$02,<S1,$04", false, 4),
            vec![1, 2]
        );
        assert_eq!(
            leading_data_literals("$01,$02,$03,$04,$05", false, 3),
            vec![1, 2, 3]
        );
        let asm = "*=$0801\n__ZP:\n    .byte $99\n";
        assert!(verify_label_bytes(asm, &[0x00], 0x0801, |_| Some(0x00FB)).is_ok());
    }

    #[test]
    fn label_bytes_check_covers_the_word_table_too() {
        // `__FOR_HANDLER_TABLE:` names `.word` entries and is indexed at runtime, so it is
        // address-taken exactly like a string.
        assert_eq!(
            leading_data_literals("$0000,$1234", true, 4),
            vec![0x00, 0x00, 0x34, 0x12]
        );
        assert_eq!(
            leading_data_literals("$0000,__FOR_NEXT_HANDLER_1", true, 4),
            vec![0x00, 0x00]
        );
        let asm = "*=$0801\n__T:\n    .word $0000,$1234\n";
        assert!(verify_label_bytes(asm, &[0, 0, 0x34, 0x12], 0x0801, |_| Some(0x0801)).is_ok());
        let err = verify_label_bytes(asm, &[0x4C, 0x00, 0x40, 0x00], 0x0801, |_| Some(0x0801))
            .expect_err("a stranded word table must be rejected");
        assert!(err.contains("__T"), "{err}");
    }

    #[test]
    fn for_handler_table_reservation_keeps_every_symbolic_word() {
        for start in 0x08f6..=0x08fd {
            let asm = format!("*=${start:04X}\nHANDLER=$1234\n__FOR_HANDLER_TABLE:\n    .word $0000\n    .word HANDLER\n");
            let mut raw = Assembler6502::new();
            raw.add_reserved_range(0x0900, 0x9000).unwrap();
            let image = raw.assemble_bytes(&asm).unwrap();
            let previously_valid = verify_for_handler_table(&asm, &image, |n| raw.lookup(n)).is_ok();
            if start == 0x08fa {
                assert!(!previously_valid, "the intact zero word masked the split handler");
                assert_eq!(&image[..5], &[0, 0, 0x4c, 1, 0x90]);
            }
            let fixed = assemble_with_aligned_vectors(asm.clone(), &[(0x0900, 0x9000)]).unwrap();
            verify_for_handler_table(&fixed.asm, &fixed.machine_code, |n| fixed.assembler.lookup(n)).unwrap();
            if previously_valid {
                assert!(fixed.machine_code == image, "an intact table at {start:04X} is unchanged");
            }
            let plain = assemble_with_aligned_vectors(asm.clone(), &[]).unwrap();
            assert_eq!(plain.machine_code, Assembler6502::new().assemble_bytes(&asm).unwrap());
        }
    }

    #[test]
    fn for_handler_table_clears_multiple_reservations_or_refuses() {
        let mut asm = "*=$08E0\n__FOR_HANDLER_TABLE:\n    .word $0000\n".to_string();
        for _ in 0..20 { asm.push_str("    .word HANDLER\n"); }
        asm.push_str("HANDLER:\n    RTS\n");
        let fixed = assemble_with_aligned_vectors(asm.clone(), &[(0x0900, 0x0910), (0x0920, 0x0940)]).unwrap();
        assert!(fixed.assembler.lookup("__FOR_HANDLER_TABLE").unwrap() > 0x0940);
        verify_for_handler_table(&fixed.asm, &fixed.machine_code, |n| fixed.assembler.lookup(n)).unwrap();
        assert!(assemble_with_aligned_vectors(asm, &[(0x0900, 0xfffe)]).is_err());
        assert!(coalesce_for_handler_table("__FOR_HANDLER_TABLE:\n    .word $0000\n    .byte $00\n").is_err());
        assert!(coalesce_for_handler_table("__FOR_HANDLER_TABLE:\n    .word $0000\n__FOR_HANDLER_TABLE:\n    .word $0000\n").is_err());
    }

    // Assembly-only causal replay of an existing untrimmed, default-origin compiler PRG.


    // I/O area overlap: the arithmetic

    #[test]
    fn io_overlap_is_the_intersection_with_d000_dfff() {
        // Ends one byte short of the I/O block: clean.
        assert_eq!(io_area_overlap(0x0801, 0xD000 - 0x0801), None);
        // Ends on the first I/O byte: one byte of overlap.
        assert_eq!(
            io_area_overlap(0x0801, 0xD000 - 0x0801 + 1),
            Some((0xD000, 0xD000))
        );
        // Straight through: the whole 4 KB is lost.
        assert_eq!(
            io_area_overlap(0x0801, 0xF978 - 0x0801),
            Some((0xD000, 0xDFFF))
        );
        // Loads above the block ($E000 origin): writes there fall through to the RAM under
        // KERNAL ROM, so nothing is lost.
        assert_eq!(io_area_overlap(0xE000, 0x1000), None);
        // Starts inside the block.
        assert_eq!(io_area_overlap(0xDF00, 0x200), Some((0xDF00, 0xDFFF)));
        // Empty image can't overlap anything.
        assert_eq!(io_area_overlap(0x0801, 0), None);
    }

    fn compiled_with_lengths(stored_body: usize, bss_truncated: usize) -> Compiled {
        Compiled {
            asm: String::new(),
            machine_code: Vec::new(),
            // +2 for the load address the .prg carries and the split arithmetic subtracts back off.
            prg_bytes: vec![0u8; stored_body + 2],
            diagnostics: Diagnostics {
                bss_truncated,
                ..Diagnostics::default()
            },
        }
    }

    #[test]
    fn iohole_debug_line_charges_the_stored_half_when_the_file_itself_overruns() {

        let c = compiled_with_lengths(51756, 0);
        let line = io_area_overlap_debug_line(&c, 0x0801, 0xD22C, 557);
        assert!(line.contains("stored=51756"), "{line}");
        assert!(line.contains("stored_end=$D22C"), "{line}");
        assert!(line.contains("bss_truncated=0"), "{line}");
        assert!(
            line.contains("stored_excess=557"),
            "the whole overshoot is charged to the stored half: {line}"
        );
    }

    #[test]
    fn iohole_debug_line_charges_nothing_to_the_stored_half_when_only_the_bss_enters_the_hole() {

        let c = compiled_with_lengths(0xCFFF - 0x0801 + 1, 557);
        let line = io_area_overlap_debug_line(&c, 0x0801, 0xD22C, 557);
        assert!(line.contains("stored_end=$CFFF"), "{line}");
        assert!(line.contains("bss_truncated=557"), "{line}");
        assert!(
            line.contains("stored_excess=0"),
            "a loadable file is charged nothing: {line}"
        );
        assert!(
            line.contains("excess=557"),
            "the runtime overshoot is still reported: {line}"
        );
    }

    #[test]
    fn iohole_debug_line_puts_the_boundary_between_cfff_and_d000() {

        let last_good = compiled_with_lengths(0xCFFF - 0x0801 + 1, 0);
        assert!(
            io_area_overlap_debug_line(&last_good, 0x0801, 0xCFFF, 0).contains("stored_excess=0"),
            "an image ending on $CFFF is entirely loadable"
        );
        let first_bad = compiled_with_lengths(0xD000 - 0x0801 + 1, 0);
        assert!(
            io_area_overlap_debug_line(&first_bad, 0x0801, 0xD000, 1).contains("stored_excess=1"),
            "one byte into the register block is one byte charged"
        );
    }

    #[test]
    fn iohole_debug_line_survives_an_empty_body() {

        let c = compiled_with_lengths(0, 0);
        let line = io_area_overlap_debug_line(&c, 0x0801, 0xD000, 1);
        assert!(line.contains("stored=0"), "{line}");
        assert!(line.contains("stored_excess=0"), "{line}");
    }

    #[test]
    fn a_program_that_stops_below_the_hole_is_not_refused_at_all() {

        let out = compile_with_options(&tiny_prg(), CompileOptions::default())
            .expect("an ordinary program is not refused by the I/O-area gate");
        let body = out.prg_bytes.len() - 2 + out.diagnostics.bss_truncated;
        assert!(
            io_area_overlap(0x0801, body).is_none(),
            "a tiny program does not reach $D000"
        );
    }
}

/// Lower the AST and run the whole optimising pipeline, returning the IR module in exactly the state `codegen` is handed.
pub fn lower_and_optimize(ast: &ast::Program, _profile: Profile) -> Result<ir::Module, CompileError> {
    lower_and_optimize_with_source_types(ast, _profile).map(|(module, _)| module)
}

fn lower_and_optimize_with_source_types(
    ast: &ast::Program,
    _profile: Profile,
) -> Result<(ir::Module, codegen::SourceIntegerVars), CompileError> {
    // AST-level passes run BEFORE lowering so the IR sees the
    // already-rewritten statement sequence.
    //
    // 1. `localize_proc_vars` rewrites references to LOCAL-declared
    //    names inside PROC bodies to mangled per-PROC identifiers.
    //    Must run first so the cloned bodies in step 2 carry the
    //    mangled names with them.
    // 2. `inline_procs_ast` flattens single-use PROC bodies into
    //    their EXEC sites — sparing the JSR + RTS overhead and
    //    giving downstream passes (LocalConstProp, ConstantFold,
    //    dead-code-elim) facts that span the original PROC boundary.
    let mut ast = ast.clone();
    crate::passes::localize_proc_vars(&mut ast);
    // Fold `DESIGN type, addr` + following `@`-rows into a single
    // Design statement with the resolved byte sequence. Must run
    // *before* PROC inlining: a `DESIGN` block inside a single-use
    // PROC that's called from `IF cond THEN <proc>` would otherwise
    // end up nested in the IF's THEN body, where `group_design_blocks`
    // (which only walks top-level statements per line) can't see it —
    // and the codegen would then treat the target address as a
    // POKE-value byte (`?ILLEGAL QUANTITY` for any addr > 255).
    // Folding here keeps
    // the @-rows adjacent (they're on consecutive PROC-body lines)
    // and what `inline_procs_ast` later lifts is the resolved
    // `Design { addr, bytes }`.
    crate::passes::group_design_blocks(&mut ast).map_err(CompileError::Parse)?;
    crate::passes::inline_procs_ast(&mut ast);
    let mut module = ir::lower(&ast).map_err(CompileError::Lower)?;
    use ir::Pass;
    crate::passes::ConstantFold
        .run(&mut module)
        .map_err(CompileError::Pass)?;
    if module.has_on_key() || module.has_error_continuation() {

        let source_integer_vars = codegen::SourceIntegerVars::from_module(&module);
        return Ok((module, source_integer_vars));
    }
    // Promote single-assignment-with-literal scalars to inlined
    // Number nodes everywhere, then re-fold. ConstVarProp only marks
    // a var as a folding candidate when its RHS is already a literal,
    // so chained definitions (`a=5: b=a*2: c=b+1: dim x(c)`) need a
    // round trip per layer — the first pass propagates `a`, the fold
    // collapses `b=a*2` to `b=10`, the next pass picks up `b`, and
    // so on. The loop runs to fixpoint with a hard cap so we can't
    // hang on a degenerate chain.
    for _ in 0..16 {
        let changed_num = crate::passes::run_const_var_prop(&mut module);
        let changed_str = crate::passes::run_str_const_var_prop(&mut module);
        crate::passes::ConstantFold
            .run(&mut module)
            .map_err(CompileError::Pass)?;
        if !changed_num && !changed_str {
            break;
        }
    }
    let mut pipeline = ir::Pipeline::new();
    // Drop unreachable statements that follow GOTO/RETURN/END/STOP
    // within a line (or THEN body). After ConstVarProp this can
    // expose more dead code since some IF conds collapse to literal
    // True/False.
    pipeline.add(crate::passes::DeadCodeAfterTransfer);
    // Fold GOTO/GOSUB chains so trampoline lines (`100 GOTO 200`)
    // get bypassed at every call site.
    pipeline.add(crate::passes::GotoChainFold);
    // `GOSUB target: RETURN` is a tail handoff; rewrite it to
    // `GOTO target`, then fold any trampoline lines exposed by that.
    pipeline.add(crate::passes::TailGosubRewrite);
    pipeline.add(crate::passes::GotoChainFold);
    // Inline narrow single-use subroutines before local propagation so
    // the caller and body can share same-line facts.
    pipeline.add(crate::passes::GosubSingleUseInline);
    // Inline very short (≤2 stmt) multi-call-site subroutines too —
    // the JSR/RTS overhead (12 cycles + 3 bytes call site) costs more
    // than the few-byte body copy.
    pipeline.add(crate::passes::GosubShortBodyInline);
    pipeline.add(crate::passes::LocalConstProp);
    pipeline.add(crate::passes::ConstantFold);
    // Run IF folding after constants have propagated so `IF 0 THEN`
    // stops keeping unreachable GOTO targets alive. A second same-line
    // dead-code sweep then removes statements exposed after
    // `IF -1 THEN GOTO`.
    pipeline.add(crate::passes::IfConditionFold);
    pipeline.add(crate::passes::DeadCodeAfterTransfer);
    pipeline.add(crate::passes::DeadLineElim);
    // Dead-store elimination via live-variable analysis. Drops LETs
    // whose target is unread on every forward path (and whose RHS is
    // pure — we can't drop a LET whose RHS could raise or have a
    // side effect, e.g. `LET X = SQR(-1)` or anything calling FN/USR).
    // ResolveBareNext first so bare `NEXT` carries its loop var
    // before liveness analysis runs.
    pipeline.add(crate::passes::ResolveBareNext);
    pipeline.add(crate::passes::DeadStoreElim);
    // Split multi-type scalars (SSA-style): when a Float var has
    // disjoint def-use lifetimes (one float-assigned, one int-
    // assigned with no shared readers), rename each lifetime to its
    // own VarName so later type analyses can promote them
    // independently. Run before `IntPromote` so the int lifetime
    // can demote to int16; UBL's overloaded `U` is the canonical
    // case.
    pipeline.add(crate::passes::SplitMultiTypeVars);
    // Run dead-code elimination before recording source integer names.
    pipeline.run(&mut module).map_err(CompileError::Pass)?;
    let mut source_integer_vars = codegen::SourceIntegerVars::from_module(&module);
    let mut pipeline = ir::Pipeline::new();
    // Integer promotion: demote float scalars to int16 storage
    // wherever every assigned value is i16-range integer AND the
    // per-var cost/benefit gate inside `compute_int_promotable`
    // says the int-island wins outweigh the FAC-conversion cost
    // at PRINT/transcendental sites. The gate runs on every
    // profile so default builds also get the speedup; size mode
    // only affects unrelated rules.
    pipeline.add(crate::passes::IntPromote);
    // POKE-loop fusion runs before any other FOR analysis. When it
    // finds `FOR I=A TO B: POKE I,V: NEXT` it rewrites the triplet
    // into a single `Stmt::PokeFill` that codegen lowers to a tight
    // memory fill — at typical 1000-byte clear-screen patterns this
    // is roughly 6× faster than the general FOR loop, and it removes
    // a FOR/NEXT pair that the induction passes would otherwise
    // analyse for nothing.
    pipeline.add(crate::passes::PokeLoopFusion);
    // LICM hoists pure invariant subexpressions (`X*K` where X and
    // K are loop-invariant scalars) into a fresh LET before the
    // FOR. For BASIC v2 this is a big win because float
    // multiplications cost ~200 cycles each and naive codegen
    // re-evaluates the whole RHS every iteration. Conservative
    // scope: pure arithmetic only (no PEEK/array/string/USR/FN/
    // GOSUB), no division/pow/sqr/log/sin/cos/tan/atn (could
    // trap when the loop runs zero times), no Rnd (stateful).
    // Re-fold afterwards to clean up any constant subexpressions
    // exposed by the rewrite.
    pipeline.add(crate::passes::LoopInvariantCodeMotion);
    pipeline.add(crate::passes::ConstantFold);
    // Promote provably-integer float arrays to 2-byte integer storage
    // before the induction passes so they see the final element size
    // (and the array-pointer stride). Runs after constant folding so
    // DATA values and FOR bounds are in literal form for the
    // integer-safety check.
    pipeline.run(&mut module).map_err(CompileError::Pass)?;

    source_integer_vars.retain_live_arrays(&module);
    let mut pipeline = ir::Pipeline::new();
    pipeline.add(crate::passes::IntArrayPromote);
    // After dead-line elim so we don't waste analysis on lines that'll
    // be removed anyway. Loop-induction detection runs first so
    // `IntForBodyAnalysis` can skip body reads of `i*K` when the
    // FOR is going to materialise a strength-reduced induction
    // slot (codegen substitutes those reads with a MOVFM, so they
    // never touch V_var).
    pipeline.add(crate::passes::LoopInductionDetect);
    // Array-pointer induction also needs to land before
    // IntForBodyAnalysis so the body's V_var-sync gating sees the
    // final shape of FOR annotations.
    pipeline.add(crate::passes::ArrayPtrInductionDetect);
    pipeline.add(crate::passes::IntForBodyAnalysis);
    pipeline.run(&mut module).map_err(CompileError::Pass)?;
    Ok((module, source_integer_vars))
}
