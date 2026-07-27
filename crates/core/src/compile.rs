//! End-to-end compilation: tokenized BASIC `.prg` → runnable `.prg`.
//!
//! Stages: parse PRG → AST → IR → optimization passes → asm text →
//! machine code → SYS-stub-wrapped PRG.

use asm6502::Assembler6502;

use crate::{ast, codegen, extraram, ir, pack, parse, prg, rem_hints};

pub use crate::rem_hints::BasicHintDialect;

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
    /// Which third-party compiler's REM-hint syntax to honour. Only
    /// one dialect may be active at a time; `BasicHintDialect::None`
    /// (the default) treats REM as plain comment text.
    pub rem_hint_dialect: BasicHintDialect,
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
            rem_hint_dialect: BasicHintDialect::None,
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

/// Memory address the load-image (code+data, excluding runtime heap)
/// must stay strictly below if we want to skip extraram. At or above
/// this point the program would start encroaching on the BASIC-ROM
/// shadow (\$A000-\$BFFF), so the auto-predictor switches extraram
/// on. Set 256 bytes below \$A000 so a marginal probe build still
/// has runtime breathing room (string-heap allocations, runtime DIM
/// arrays, FAC temporaries) before MEMSIZ/heap collisions bite.
pub const AUTO_EXTRARAM_TRIGGER: u32 = 0x9F00;

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
            CompileError::Assemble { message, .. } => write!(f, "assemble: {message}"),
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
    /// Manual ranges the caller supplied via `reserved_ranges`.
    /// Echoed back so the GUI can confirm what landed in the
    /// assembler's reservation list.
    pub manual_reserved: Vec<(u16, u16)>,
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

pub fn compile_with_options(
    prg_bytes: &[u8],
    options: CompileOptions,
) -> Result<Compiled, CompileError> {
    if options.extraram && options.force_extraram_off {
        return Err(CompileError::Assemble {
            message: "--extraram and --force-extraram-off are mutually exclusive".to_string(),
            asm: None,
        });
    }
    let prg = prg::Program::parse(prg_bytes).map_err(CompileError::Prg)?;
    // Reject machine-language .prgs up front. A tokenized BASIC
    // program has self-consistent next-line links (each line's link
    // points at the following line); an ML program — a $C000 sprite
    // editor, a $7000 music player, a $CC00 DOS wedge — has effectively
    // random bytes there and satisfies none of them. Without this the
    // BASIC parser walks the ML and reports a confusing "token $XX is
    // not yet supported" deep inside the code, or worse, parses it as a
    // garbage program and emits nonsense. The load address alone isn't
    // enough to tell them apart (relocated BASIC saved at $1001/$4001
    // is real, while some ML carries a leading $00 and looks like an
    // $0800 BASIC load), so we check the structure instead.
    if !looks_like_basic(&prg) {
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
    };
    let ast = parse::program_with_options(&prg, parse_opts).map_err(CompileError::Parse)?;
    let skipped_statements = ast.skipped.clone();
    let module = lower_and_optimize(&ast, options.profile)?;
    let origin = options
        .custom_start_address
        .map(u32::from)
        .unwrap_or(CODE_ORIGIN);
    let asm = codegen::emit_with_profile_at_opts(
        &module,
        options.profile,
        origin as u16,
        options.safe_sys_calls,
        &hints,
    )
    .map_err(CompileError::Codegen)?;
    // Run asm-level peephole passes before extraram rewrites so
    // liveness analysis sees raw ROM calls.
    let asm = crate::peephole::run(&asm, options.profile);
    // Programs that load ML embedded in REM lines via the
    // `SYS PEEK(44)*256+N` idiom (CavernRun, lots of crackme-style
    // 80s code) read the BASIC TXTTAB pointer at ZP $2B/$2C to
    // locate the embedded bytes. Our compiled output replaces the
    // BASIC text region with ML, so those reads would resolve to
    // garbage. Detect the pattern and bolt on a copy of the
    // original PRG body plus a prologue that stamps its address
    // into $2B/$2C, so PEEK(43)/PEEK(44) and the SYS that follows
    // see the same bytes the program was authored against.
    let asm = if module_reads_basic_txttab(&module) {
        embed_original_basic_text(&asm, prg_bytes)
    } else {
        asm
    };

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
        return assemble_pass(
            &asm,
            &module,
            true,
            &options,
            ExtraRamDecision::ForcedOn,
            &skipped_statements,
        );
    }
    if options.force_extraram_off {
        return assemble_pass(
            &asm,
            &module,
            false,
            &options,
            ExtraRamDecision::ForcedOff,
            &skipped_statements,
        );
    }
    // When the custom origin already sits at or above the BASIC-ROM
    // shadow ($A000-$BFFF), extraram can't add code space — that area
    // is below the program. Skip the predictor so a `--start-address
    // =$C000` build doesn't get a pointless JT prologue. Lower custom
    // origins ($0900, $8000, …) still go through the normal predictor
    // below so a program at $8000 that grows past $9F00 picks up the
    // extra 8 KB just like a default-origin build would.
    if matches!(options.custom_start_address, Some(addr) if u32::from(addr) >= 0xC000) {
        return assemble_pass(
            &asm,
            &module,
            false,
            &options,
            ExtraRamDecision::SkippedHighOrigin,
            &skipped_statements,
        );
    }
    let probe = assemble_pass(
        &asm,
        &module,
        false,
        &options,
        ExtraRamDecision::AutoOff,
        &skipped_statements,
    )?;
    let probe_end = origin + probe.machine_code.len() as u32;
    if probe_end < AUTO_EXTRARAM_TRIGGER {
        return Ok(probe);
    }
    // Predictor: program would land at or past \$9F00. Re-assemble
    // with extraram on so the data hoists and ROM-bank-out give the
    // program 8 KB more code space ($A000-$BFFF). If the data
    // section itself overflows the \$9F00 ceiling, `verify_data_fits`
    // surfaces the error; there is no third pass that could rescue
    // it (extraram is the largest available memory layout).
    assemble_pass(
        &asm,
        &module,
        true,
        &options,
        ExtraRamDecision::AutoOn,
        &skipped_statements,
    )
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
) -> Result<Compiled, CompileError> {
    let auto_ranges: Vec<(u16, u16)> = if options.auto_reserve {
        discover_reserved_ranges(module)
    } else {
        Vec::new()
    };
    let manual_ranges: Vec<(u16, u16)> = options.reserved_ranges.clone();
    let mut all_reserved: Vec<(u16, u16)> = Vec::new();
    all_reserved.extend(auto_ranges.iter().copied());
    all_reserved.extend(manual_ranges.iter().copied());
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
    let final_asm = if extraram_on {
        extraram::inject(asm)
    } else {
        relocate_data_past_reservations(asm, &merged)
    };

    let mut assembler = Assembler6502::new();
    // Reserved-region pass: feed the assembler regions of memory that
    // it must NOT place generated code or data into. Auto-discovered
    // ranges (POKE/PEEK literal addresses) come first, then user-
    // supplied ranges; the assembler refuses overlap so the sort+
    // merge in apply_reserved_ranges has to dedupe before either set
    // reaches the underlying `add_reserved_range` calls.
    for (start, end) in &merged {
        assembler
            .add_reserved_range(*start, *end)
            .map_err(|e| CompileError::Assemble {
                message: format!("reserved range ${start:04X}-${end:04X}: {e}"),
                asm: None,
            })?;
    }

    let machine_code =
        assembler
            .assemble_bytes(&final_asm)
            .map_err(|e| CompileError::Assemble {
                // The asm6502 error text is short and self-explanatory
                // ("undefined label 'foo'", "branch out of range"). The
                // full asm goes in `asm` so callers can write it to disk
                // when they need to grep — see `CompileError::generated_asm`.
                message: format!("{e}"),
                asm: Some(final_asm.clone()),
            })?;

    if extraram_on {
        extraram::verify_data_fits(|name| assembler.lookup(name)).map_err(|message| {
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

    let (prg_bytes, start_address) = if let Some(addr) = options.custom_start_address {
        (pack::pack_raw(addr, &machine_code), addr)
    } else {
        (pack::pack(&machine_code), 0x0801)
    };
    // end_address is the inclusive last byte of compiled code in
    // memory after the program has loaded. For default builds the
    // code starts at $080D (past the 12-byte SYS stub); for custom
    // builds the code starts at start_address itself.
    let code_origin = options.custom_start_address.unwrap_or(CODE_ORIGIN as u16);
    let end_address = code_origin
        .saturating_add(machine_code.len().saturating_sub(1).min(u16::MAX as usize) as u16);
    let diagnostics = Diagnostics {
        start_address,
        end_address,
        extraram: extraram_decision,
        auto_reserved: auto_ranges,
        manual_reserved: manual_ranges,
        effective_reserved: merged.clone(),
        skipped_statements: skipped_statements.to_vec(),
    };
    Ok(Compiled {
        asm: final_asm,
        machine_code,
        prg_bytes,
        diagnostics,
    })
}

/// Hoist the `__DATA:` block to the front of the code stream so it
/// lands in low memory and can never be split across a user-supplied
/// reserved region. asm6502 emits a JMP-bridge over reservations,
/// which works for code but corrupts a length-prefixed DATA pool —
/// the BASIC FIN parser reads the bridge bytes as length-0 records
/// and the rest of the pool becomes unreachable. The hoisted block
/// is wrapped in `JMP __DATA_END / __DATA: <bytes> / __DATA_END:`
/// so the original instruction at $080D still gets executed (the
/// SYS stub jumps to $080D, where we now emit the bridge JMP first
/// and the data after).
///
/// When there are no reservations, the asm passes through unchanged
/// — the natural placement is still safe since asm6502 won't insert
/// a bridge.
///
/// DATA pools must stay contiguous. If a reserved hole splits one, the
/// assembler bridge bytes become visible to READ and corrupt the pool.
fn relocate_data_past_reservations(asm: &str, merged: &[(u16, u16)]) -> String {
    if merged.is_empty() {
        return asm.to_string();
    }

    // Find the DATA pool: from the section comment (or the bare
    // label) up to the next blank-line + new section comment, or up
    // to the next top-level label that isn't a `.byte` continuation.
    // The section comment names the pool layout, so anchor on the
    // label and pick the comment up only when it sits directly above.
    let header = "__DATA:\n";
    let Some(label_at) = asm.find(&format!("\n{header}")).map(|p| p + 1) else {
        return asm.to_string();
    };
    let start = asm[..label_at]
        .rfind("\n; --- DATA pool")
        .map(|p| p + 1)
        .filter(|&p| asm[p..label_at].lines().count() == 1)
        .unwrap_or(label_at);
    // The pool ends at the next blank line or section comment after
    // the contiguous `.byte` lines under `__DATA:`. `__DATA_LINE_<n>:`
    // labels (added for `RESET <line>`) sit between byte runs and
    // need to count as pool continuation, not as the next section.
    let body_start = label_at + header.len();
    let mut end = body_start;
    for line in asm[body_start..].split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with("; ---") {
            break;
        }
        // Continuation of the pool: `.byte ...`, a comment line,
        // or a `__DATA_LINE_<n>:` per-line label. Stop on anything
        // else (next label/instruction).
        let is_data_line_label = trimmed.starts_with("__DATA_LINE_");
        if !trimmed.starts_with(".byte") && !trimmed.starts_with(';') && !is_data_line_label {
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
        return asm.to_string();
    };
    // Skip blank lines + ZP equate definitions (they don't emit
    // bytes). The first non-equate line is where actual code begins.
    let mut hoist_at = origin_end;
    for line in asm[origin_end..].split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with(';') || trimmed.contains(" = $") {
            hoist_at += line.len();
            continue;
        }
        break;
    }

    // Build the new asm: head + hoist (JMP-bridge + DATA + bridge
    // tail) + middle (asm between hoist site and original DATA pool)
    // + tail (asm after the original DATA pool).
    let head = &asm[..hoist_at];
    let middle = &asm[hoist_at..start];
    let tail = &asm[end..];
    let mut out = String::with_capacity(asm.len() + 64);
    out.push_str(head);
    out.push_str("    JMP __DATA_HOISTED_END\n");
    out.push_str(data_block);
    out.push_str("__DATA_HOISTED_END:\n");
    out.push_str(middle);
    out.push_str(tail);
    out
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

/// Walk the optimised IR and collect every literal address (or
/// literal range) the program reads or writes through POKE/DPOKE/
/// PokeFill/PEEK in the `$0800..=$CFFF` window. The asm6502
/// assembler will then skip over those ranges so emitted code
/// can't land on top of sprite blocks, custom screens, custom
/// charsets, etc. that the program is going to clobber at runtime.
///
/// Three sources contribute:
///   1. Literal-address POKEs / DPOKEs / PokeFills / PEEKs (the
///      stable original auto-reserve).
///   2. POKEs inside a literally-bounded FOR loop where the address
///      expression evaluates to a known interval given each FOR
///      counter's range — covers the canonical custom-charset and
///      ML-loader idiom `FOR I=lit TO lit: READ A: POKE I, A: NEXT`
///      and the invader-table form `FOR Y=0 TO 4: FOR X=0 TO 5:
///      POKE base+Y*K+X*L, …: NEXT, NEXT`.
///   3. `POKE 53272, lit` (= `POKE V+24, X`) writing the VIC memory
///      pointer — the `bits[3:1] * $0800` window names the active
///      character set, which the VIC reads in full whether the
///      program writes the whole 2KB or just a few chars.
///
/// Returns merged inclusive `(start, end)` ranges; close-together
/// hits get coalesced so we don't waste reserved bytes on dozens of
/// tiny one-byte islands (see `merge_reserved_ranges`).
/// Fail when a reserved range lands inside the DATA pool. The
/// assembler bridges reservations with a `JMP`, which is fine for code
/// but not for a pool `READ` walks byte by byte — the bridge would be
/// read as a data value and every later `READ` would return garbage.
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

fn discover_reserved_ranges(module: &ir::Module) -> Vec<(u16, u16)> {
    use std::collections::{BTreeSet, HashMap};
    let mut points: BTreeSet<u16> = BTreeSet::new();
    let mut ranges: Vec<(u16, u16)> = Vec::new();
    let mut for_stack: Vec<(crate::ast::VarName, i32, i32)> = Vec::new();
    let mut let_ranges: HashMap<crate::ast::VarName, (i32, i32)> = HashMap::new();
    let d018_is_charset = d018_names_a_charset(module);
    for line in &module.lines {
        for stmt in &line.stmts {
            collect_reserved_in_stmt(
                stmt,
                &mut points,
                &mut ranges,
                &mut for_stack,
                &mut let_ranges,
                d018_is_charset,
            );
        }
    }
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

fn collect_reserved_in_stmt(
    stmt: &ir::Stmt,
    points: &mut std::collections::BTreeSet<u16>,
    ranges: &mut Vec<(u16, u16)>,
    for_stack: &mut Vec<(crate::ast::VarName, i32, i32)>,
    let_ranges: &mut std::collections::HashMap<crate::ast::VarName, (i32, i32)>,
    d018_is_charset: bool,
) {
    use ir::Stmt;
    match stmt {
        Stmt::Poke { addr, value } => {
            note_addr_literal(addr, points);
            // Computed-address POKE inside a FOR with literal
            // bounds: range-evaluate the address expression with
            // each tracked counter substituted by `[start, end]`,
            // and reserve `[min, max]` if the result fits the
            // auto-reserve window.
            note_addr_for_range(addr, for_stack, let_ranges, ranges);
            note_d018_charset_window(addr, value, ranges, d018_is_charset);
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
            if let Some((min, max)) = addr_for_range_in_window(addr, for_stack, let_ranges)
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
            // PokeFill is the canonical bulk-init shape — treat the
            // whole inclusive range as reserved when both endpoints
            // fold to literals.
            if let (Some(s), Some(e)) = (
                addr_literal_in_window(dst_start),
                addr_literal_in_window(dst_end),
            ) {
                if s <= e {
                    ranges.push((s, e));
                }
            }
            note_peek_in_expr(value, points);
        }
        Stmt::Let { var, value } => {
            note_peek_in_expr(value, points);
            // Track LET assignments whose RHS evaluates to a known
            // range using the current FOR stack + previously
            // tracked LETs. This is what binds LICM-hoisted
            // intermediates (`__LICM_0 = 7620 + Y1*12`) into the
            // analysis: subsequent POKEs that read `__LICM_0` get
            // its range substituted in.
            if let Some(r) = eval_addr_range(value, for_stack, let_ranges) {
                let_ranges.insert(var.clone(), r);
            }
        }
        Stmt::ArrayLet { indices, value, .. } => {
            for e in indices {
                note_peek_in_expr(e, points);
            }
            note_peek_in_expr(value, points);
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
                        let_ranges,
                        d018_is_charset,
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
            var, start, end, ..
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
            let s_range = eval_addr_range(start, for_stack, let_ranges);
            let e_range = eval_addr_range(end, for_stack, let_ranges);
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
                for_stack.pop();
            }
        }
        _ => {}
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
    ranges: &mut Vec<(u16, u16)>,
) {
    if for_stack.is_empty() && let_ranges.is_empty() {
        return;
    }
    if let Some((min, max)) = addr_for_range_in_window(addr, for_stack, let_ranges) {
        ranges.push((min, max));
    }
}

fn addr_for_range_in_window(
    addr: &ir::Expr,
    for_stack: &[(crate::ast::VarName, i32, i32)],
    let_ranges: &std::collections::HashMap<crate::ast::VarName, (i32, i32)>,
) -> Option<(u16, u16)> {
    // Require the expression to actually reference at least one
    // tracked symbol — a fully-literal address is already handled
    // by the `note_addr_literal` path, and re-adding it as a single
    // point is harmless but wasteful.
    if !expr_reads_any_tracked(addr, for_stack, let_ranges) {
        return None;
    }
    let (lo, hi) = eval_addr_range(addr, for_stack, let_ranges)?;
    let lo32 = lo.max(AUTO_RESERVE_LO as i32);
    let hi32 = hi.min(AUTO_RESERVE_HI as i32);
    if lo32 > hi32 {
        return None;
    }
    Some((lo32 as u16, hi32 as u16))
}

/// Range-evaluate `e` against `for_stack` plus `let_ranges`. Each
/// tracked counter gets its `[start, end]` range; tracked LET
/// bindings (= LICM intermediates and similar) get the recorded
/// range; literals contribute their value; `+`, `-`, `*` propagate
/// via interval arithmetic. Anything else (PEEK, function call,
/// untracked Var, fractional literal) bails the evaluation.
fn eval_addr_range(
    e: &ir::Expr,
    for_stack: &[(crate::ast::VarName, i32, i32)],
    let_ranges: &std::collections::HashMap<crate::ast::VarName, (i32, i32)>,
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
        Expr::Bin(op, l, r) => {
            let (lmin, lmax) = eval_addr_range(l, for_stack, let_ranges)?;
            let (rmin, rmax) = eval_addr_range(r, for_stack, let_ranges)?;
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

/// True iff `e` references at least one variable currently tracked
/// on `for_stack` or as a LET-binding range. Used to gate the
/// range-eval path so we don't double-count fully-literal addresses
/// already handled by the point/range note functions.
fn expr_reads_any_tracked(
    e: &ir::Expr,
    for_stack: &[(crate::ast::VarName, i32, i32)],
    let_ranges: &std::collections::HashMap<crate::ast::VarName, (i32, i32)>,
) -> bool {
    use ir::Expr;
    match e {
        Expr::Var(v) => {
            for_stack.iter().any(|(name, _, _)| name == v) || let_ranges.contains_key(v)
        }
        Expr::Neg(inner) | Expr::Not(inner) | Expr::Peek(inner) => {
            expr_reads_any_tracked(inner, for_stack, let_ranges)
        }
        Expr::Bin(_, l, r) => {
            expr_reads_any_tracked(l, for_stack, let_ranges)
                || expr_reads_any_tracked(r, for_stack, let_ranges)
        }
        Expr::Func1(_, arg) => expr_reads_any_tracked(arg, for_stack, let_ranges),
        Expr::ArrayRef(_, idx) => idx
            .iter()
            .any(|e| expr_reads_any_tracked(e, for_stack, let_ranges)),
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

/// `POKE 53272, lit` — `$D018` writes select the VIC memory
/// pointer's screen + character base. The bits-3:1 nibble names the
/// 2KB character window (`bits * $0800`); VIC reads the entire 2KB
/// even when the program writes only part of it. Reserve that
/// window so emitted code/data can't accidentally provide character
/// bytes to chars 0..255 it never wrote.
///
/// Skips ROM character bases ($1000 / $1800) — those are masked
/// anyway when the VIC reads from the ROM area, and reserving them
/// would conflict with low-RAM code emission.
///
/// `d018_is_charset` is false when the program makes those bits mean
/// something else — see [`d018_names_a_charset`].
fn note_d018_charset_window(
    addr: &ir::Expr,
    value: &ir::Expr,
    ranges: &mut Vec<(u16, u16)>,
    d018_is_charset: bool,
) {
    if !d018_is_charset {
        return;
    }
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
    let Some(v) = as_i32_literal(value) else {
        return;
    };
    let bits = ((v as u32) >> 1) & 0b111;
    // bits 010 = $1000 (ROM upper/graphics) and 011 = $1800 (ROM
    // upper/lower) — VIC pulls these from the character-ROM mirror,
    // not from RAM, so the underlying RAM addresses are free for
    // code emission. Skip them.
    if bits == 0b010 || bits == 0b011 {
        return;
    }
    let base = (bits as u16) * 0x0800;
    let end = base.saturating_add(0x07FF);
    if base >= AUTO_RESERVE_LO && end <= AUTO_RESERVE_HI {
        ranges.push((base, end));
    }
}

/// Whether the `$D018` bits still name a character set at a known
/// address. Two things break that reading, and the program tells us
/// about both:
///
/// * Bitmap mode (`$D011` bit 5) repurposes CB13 as the base of an
///   8 KB bitmap — the 2 KB charset window doesn't exist.
/// * `$DD00` selects the VIC bank, and `$D018` is relative to it. A
///   program that moves the bank puts the charset somewhere this
///   heuristic has no way to compute.
///
/// MAD's `POKE V+17,59 : POKE V+24,24` with the bank moved to $4000
/// was the report: `$D018` read as a charset at `$2000`, the range was
/// reserved, and the DATA pool — which really lives there — was split
/// around it.
fn d018_names_a_charset(module: &ir::Module) -> bool {
    fn breaks_it(stmt: &ir::Stmt) -> bool {
        use ir::Stmt;
        match stmt {
            Stmt::Poke { addr, value } => match literal_addr(addr) {
                Some(0xDD00) | Some(0xDD02) => true,
                Some(0xD011) => as_i32_literal(value).is_some_and(|v| v & 0x20 != 0),
                _ => false,
            },
            Stmt::If { then, .. } => then_breaks_it(then),
            Stmt::IfElse {
                then, else_then, ..
            } => then_breaks_it(then) || then_breaks_it(else_then),
            Stmt::Rcomp { then, else_then } => {
                then_breaks_it(then) || else_then.as_ref().is_some_and(then_breaks_it)
            }
            _ => false,
        }
    }
    fn then_breaks_it(then: &ir::ThenIr) -> bool {
        matches!(then, ir::ThenIr::Stmts(inner) if inner.iter().any(breaks_it))
    }
    !module.lines.iter().any(|l| l.stmts.iter().any(breaks_it))
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

/// Sort, dedupe, and coalesce raw reserved ranges so the final list
/// is friendly to `add_reserved_range`. Two rules:
///
/// 1. Inclusive ranges that overlap or touch (gap < 4 bytes) are
///    merged. asm6502 needs at least 3 bytes of free space between
///    successive reserved ranges to fit the `JMP` it emits to skip
///    each one — leaving a 4-byte cushion keeps us comfortably above
///    that limit.
/// 2. The order of the input list (auto-discovered first, then
///    user-supplied) is preserved as the dedupe-by-min ordering, so
///    the assembler sees ranges sorted ascending — its overlap check
///    walks the existing list, and ascending order keeps that check
///    fast.
/// True iff any expression in the module reads ZP $2B/$2C — BASIC's
/// TXTTAB pointer. Recognised shapes: `PEEK(43)`, `PEEK(44)`,
/// `PEEK($2B)`, `PEEK($2C)`. Programs that do this are usually
/// looking up REM-embedded machine code via `SYS PEEK(44)*256+N`.
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
            if let ir::Expr::Peek(addr) = e
                && let ir::Expr::Number(n) = addr.as_ref()
                && n.is_finite()
                && (*n as i32 == 43 || *n as i32 == 44)
            {
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

/// Splice a copy of the original BASIC text into the assembly and
/// stamp its address into $2B/$2C at program start. Programs that
/// load REM-embedded ML via `SYS PEEK(44)*256+N` assume TXTTAB has
/// the form `$XX01` so `PEEK(44)*256` evaluates to `$XX00` (i.e.
/// `TXTTAB - 1`) and `+N` then lands at byte `N - 1` of the BASIC
/// text. The embedded copy is therefore parked at a stable `$8001`
/// landing pad, far above where compiled programs of this size
/// reach. The bytes
/// can't simply be assembled at `$8001` because asm6502 emits a
/// contiguous binary (`*=` only retargets labels, not file
/// position), so the prologue runs a small two-loop memcpy from
/// the inline `__BASIC_TEXT_SRC` block to `$8001` before any user
/// statement runs.
fn embed_original_basic_text(asm: &str, prg_bytes: &[u8]) -> String {
    if prg_bytes.len() < 2 {
        return asm.to_string();
    }
    let body = &prg_bytes[2..];
    let len = body.len();
    let pages = (len / 256) as u8;
    let remainder = (len % 256) as u8;
    // Stamp TXTTAB and copy the embedded body to its runtime
    // landing pad. `$FB/$FC` and `$FD/$FE` are scratch ZP that
    // BASIC ROM marks "free for any purpose"; the prologue runs
    // before any other use, so we don't fight ARRAY_ADDR.
    let prelude = format!(
        "    LDA #$01\n    STA $2B\n    LDA #$80\n    STA $2C\n\
         \n    LDA #<__BASIC_TEXT_SRC\n    STA $FB\n    \
         LDA #>__BASIC_TEXT_SRC\n    STA $FC\n    \
         LDA #$01\n    STA $FD\n    LDA #$80\n    STA $FE\n"
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
mod tests {
    use super::*;

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
            msg.contains("--extraram"),
            "error message must show the predictor switched extraram on for the $8000 \
             origin (else it would have been a default-layout error). got: {msg}"
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

    /// Regression: the hoist matched a section header that no longer
    /// existed once the layout note was added to it, so it silently
    /// did nothing and pools were left to be bridged in place.
    #[test]
    fn data_pool_is_hoisted_ahead_of_reservations() {
        let asm = "*=$080D\n    RTS\n\n; --- DATA pool (ASCII length-prefixed) ---\n\
                   __DATA:\n    .byte $01,$31  ; #0\n\n; --- next ---\n";
        let out = relocate_data_past_reservations(asm, &[(0x0900, 0x09FF)]);
        let jmp = out.find("JMP __DATA_HOISTED_END").expect("bridge emitted");
        let pool = out.find("__DATA:").expect("pool kept");
        let rts = out.find("    RTS").expect("code kept");
        assert!(
            jmp < pool && pool < rts,
            "pool moved ahead of the code:\n{out}"
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
}

fn lower_and_optimize(ast: &ast::Program, _profile: Profile) -> Result<ir::Module, CompileError> {
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
    Ok(module)
}
