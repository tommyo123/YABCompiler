use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use yabcompiler_core::{
    BasicHintDialect, CompileOptions, Dialect, Profile, compile, config, is_basic_source_path,
    parse_reserved_ranges, parse_start_address, prg, resolved_dialect, tokenize_program_with,
};

/// Output stream for command-line help.
#[derive(Clone, Copy, PartialEq)]
enum UsageTo {
    Stdout,
    Stderr,
}

macro_rules! usage_line {
    ($to:expr, $($arg:tt)*) => {
        match $to {
            UsageTo::Stdout => println!($($arg)*),
            UsageTo::Stderr => eprintln!($($arg)*),
        }
    };
}

fn usage(program: &str) {
    usage_to(program, UsageTo::Stderr);
}

fn usage_to(program: &str, to: UsageTo) {
    usage_line!(
        to,
        "YABCompiler {} - Yet Another Basic Compiler for the Commodore 64",
        config::VERSION
    );
    usage_line!(to, "");
    usage_line!(to, "Usage:");
    usage_line!(to, "    {program} <command> [args]");
    usage_line!(to, "");
    usage_line!(to, "Commands:");
    usage_line!(to, "    list <file>           Detokenize a .prg, or echo a .bas after tokenizing.");
    usage_line!(to, "    dump <file>           Show parsed lines and raw token bytes.");
    usage_line!(to, "    tokenize <in> <out>   Tokenize BASIC source (.bas) to a .prg.");
    usage_line!(to, "                          Accepts --dialect=<d> (see below).");
    usage_line!(to, "    compile <in> <out>    Compile a .bas or tokenized .prg to a runnable .prg.");
    usage_line!(to, "");
    usage_line!(to, "Compile options:");
    usage_line!(to, "    --asm <file>          Also write the generated 6502 assembly.");
    usage_line!(to, "    --profile=<p>         default | speed | size  (default: default)");
    usage_line!(to, "    --lenient-syntax      Accept BASIC v2 typos that v2 only catches at runtime.");
    usage_line!(to, "    --safe-sys-calls      Save/restore $FB-$FE and ZP-pool cells around every SYS."
    );
    usage_line!(to, "    --sys-register-args   Non-v2: read `SYS a,x,y,sr` as A/X/Y/SR preloads instead");
    usage_line!(to, "                          of as text the callee parses off TXTPTR.");
    usage_line!(to, "    --rem-hints=<d>       Honour third-party compiler REM hints: basic64 | basic-boss."
    );
    usage_line!(to, "    --dialect=<d>         auto | basic2 | tsb   (default: auto)");
    usage_line!(to, "                          Are bare ASCII MOD / LINE / KEY / DO / ... extended");
    usage_line!(to, "                          keywords, or plain BASIC v2 variables? `auto` decides");
    usage_line!(to, "                          per program; `basic2` forces stock v2.");
    usage_line!(to, "");
    usage_line!(to, "Memory layout:");
    usage_line!(to, "    --extraram            Force RAM under BASIC ROM ($A000-$BFFF) on.");
    usage_line!(to, "    --force-extraram-off  Disable the auto-extraram predictor.");
    usage_line!(to, "    --no-auto-reserve     Disable auto-discovery of POKE/PEEK target ranges.");
    usage_line!(to, "    --reserved=<list>     Manual reserved ranges. Example: $7800-$79FF,$C000");
    usage_line!(to, "    --start-address=<a>   Load at <a> with no SYS stub. Start with `SYS <decimal>`."
    );
    usage_line!(to, "                          Example: --start-address=$C000  (run with: SYS 49152)");
    usage_line!(to, "");
    usage_line!(to, "Input is either a .bas source file or a tokenized .prg. The compiler");
    usage_line!(to, "auto-detects extraram and reserved ranges by default. Use the flags above");
    usage_line!(to, "to override.");
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let program = args.first().map(String::as_str).unwrap_or("yabcompiler");

    if args.len() < 2 {
        usage(program);
        return ExitCode::from(2);
    }

    match args[1].as_str() {
        // Explicit help requests succeed on stdout.
        "help" | "--help" | "-h" => {
            usage_to(program, UsageTo::Stdout);
            ExitCode::SUCCESS
        }
        "--version" | "-V" => {
            println!("YABCompiler {}", config::VERSION);
            ExitCode::SUCCESS
        }
        "list" if args.len() == 3 => run_list(&args[2]),
        "dump" if args.len() == 3 => run_dump(&args[2]),
        "tokenize" if args.len() >= 4 => run_tokenize(&args[2], &args[3], &args[4..]),
        "compile" => run_compile(&args[2..]),
        _ => {
            usage(program);
            ExitCode::from(2)
        }
    }
}

/// Read `path` and return its tokenized .prg bytes. `.bas` source
/// files are passed through the tokenizer first; everything else is
/// treated as already-tokenized .prg bytes. Centralising this here
/// keeps `list` / `dump` / `compile` accepting both shapes.
fn load_prg_bytes(path: &str, dialect: Dialect) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("failed to read {path}: {e}"))?;
    if is_basic_source_path(Path::new(path)) {
        let source = String::from_utf8(bytes)
            .map_err(|e| format!("{path} is not valid UTF-8 source: {e}"))?;
        // Auto dialect is otherwise silent: list prints the same text either way.
        if matches!(dialect, Dialect::Auto) {
            let picked = match resolved_dialect(&source, dialect) {
                Dialect::Tsb => "tsb (extended keywords found in statement position)",
                _ => "basic2",
            };
            eprintln!("  dialect: auto -> {picked}");
        }
        tokenize_program_with(&source, dialect).map_err(|e| format!("tokenize {path}: {e}"))
    } else {
        Ok(bytes)
    }
}

fn run_list(path: &str) -> ExitCode {
    let bytes = match load_prg_bytes(path, Dialect::default()) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    match prg::Program::parse(&bytes) {
        Ok(program) => {
            print!("{}", program.detokenize());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_dump(path: &str) -> ExitCode {
    let bytes = match load_prg_bytes(path, Dialect::default()) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    match prg::Program::parse(&bytes) {
        Ok(program) => {
            print!("{}", program.dump());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_tokenize(in_path: &str, out_path: &str, rest: &[String]) -> ExitCode {
    let mut dialect = Dialect::default();
    for arg in rest {
        match arg.strip_prefix("--dialect=") {
            Some(value) => match Dialect::parse(value) {
                Ok(d) => dialect = d,
                Err(e) => {
                    eprintln!("tokenize: --dialect: {e}");
                    return ExitCode::from(2);
                }
            },
            None => {
                eprintln!("tokenize: unknown arg '{arg}'");
                return ExitCode::from(2);
            }
        }
    }
    let source = match std::fs::read_to_string(in_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: failed to read {in_path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let prg = match tokenize_program_with(&source, dialect) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: tokenize {in_path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = std::fs::write(out_path, &prg) {
        eprintln!("error: failed to write {out_path}: {e}");
        return ExitCode::FAILURE;
    }
    eprintln!("tokenized: {} bytes", prg.len());
    ExitCode::SUCCESS
}

fn run_compile(args: &[String]) -> ExitCode {
    if args.len() < 2 {
        eprintln!("compile: expected <in.prg> <out.prg> [--asm <out.s>] [--profile=...]");
        return ExitCode::from(2);
    }

    let in_path = &args[0];
    let out_path = PathBuf::from(&args[1]);
    let mut asm_out: Option<PathBuf> = None;
    let mut options = CompileOptions::default();
    let mut i = 2;

    while i < args.len() {
        match args[i].as_str() {
            "--asm" if i + 1 < args.len() => {
                asm_out = Some(PathBuf::from(&args[i + 1]));
                i += 2;
            }
            arg if arg.starts_with("--profile=") => {
                let value = &arg["--profile=".len()..];
                options.profile = match value {
                    "default" => Profile::Default,
                    "speed" => Profile::Speed,
                    "size" => Profile::Size,
                    other => {
                        eprintln!("compile: --profile expected default|speed|size, got '{other}'");
                        return ExitCode::from(2);
                    }
                };
                i += 1;
            }
            "--extraram" => {
                options.extraram = true;
                i += 1;
            }
            "--force-extraram-off" => {
                options.force_extraram_off = true;
                i += 1;
            }
            "--auto-reserve" => {
                // Kept as a no-op alias: auto-reserve is on by default.
                options.auto_reserve = true;
                i += 1;
            }
            "--no-auto-reserve" => {
                options.auto_reserve = false;
                i += 1;
            }
            "--lenient-syntax" => {
                options.lenient_syntax = true;
                i += 1;
            }
            "--safe-sys-calls" => {
                options.safe_sys_calls = true;
                i += 1;
            }
            "--sys-register-args" => {
                options.sys_register_args = true;
                i += 1;
            }
            arg if arg.starts_with("--dialect=") => {
                let value = &arg["--dialect=".len()..];
                match Dialect::parse(value) {
                    Ok(d) => options.dialect = d,
                    Err(e) => {
                        eprintln!("compile: --dialect: {e}");
                        return ExitCode::from(2);
                    }
                }
                i += 1;
            }
            arg if arg.starts_with("--rem-hints=") => {
                let value = &arg["--rem-hints=".len()..];
                match BasicHintDialect::parse(value) {
                    Ok(d) => options.rem_hint_dialect = d,
                    Err(e) => {
                        eprintln!("compile: --rem-hints: {e}");
                        return ExitCode::from(2);
                    }
                }
                i += 1;
            }
            arg if arg.starts_with("--reserved=") => {
                let value = &arg["--reserved=".len()..];
                match parse_reserved_ranges(value) {
                    Ok(ranges) => options.reserved_ranges.extend(ranges),
                    Err(e) => {
                        eprintln!("compile: --reserved: {e}");
                        return ExitCode::from(2);
                    }
                }
                i += 1;
            }
            arg if arg.starts_with("--start-address=") => {
                let value = &arg["--start-address=".len()..];
                match parse_start_address(value) {
                    Ok(addr) => options.custom_start_address = Some(addr),
                    Err(e) => {
                        eprintln!("compile: --start-address: {e}");
                        return ExitCode::from(2);
                    }
                }
                i += 1;
            }
            other => {
                eprintln!("compile: unknown arg '{other}'");
                return ExitCode::from(2);
            }
        }
    }

    let bytes = match load_prg_bytes(in_path, options.dialect) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    match compile::compile_with_options(&bytes, options) {
        Ok(compiled) => {
            if let Some(path) = asm_out {
                if let Err(e) = std::fs::write(&path, &compiled.asm) {
                    eprintln!("error: failed to write {}: {e}", path.display());
                    return ExitCode::FAILURE;
                }
            }
            if let Err(e) = std::fs::write(&out_path, &compiled.prg_bytes) {
                eprintln!("error: failed to write {}: {e}", out_path.display());
                return ExitCode::FAILURE;
            }
            print_diagnostics(&compiled);
            ExitCode::SUCCESS
        }
        Err(e) => {
            // Drop the generated asm into the user-supplied --asm
            // path when available — even on failure they almost
            // certainly want to grep through it. The message in
            // stderr stays short (asm6502 error text only); the
            // dump goes to disk.
            if let (Some(path), Some(asm)) = (asm_out.as_ref(), e.generated_asm()) {
                let _ = std::fs::write(path, asm);
                eprintln!(
                    "error: {e}\n   (generated assembly written to {})",
                    path.display()
                );
            } else if let Some(_asm) = e.generated_asm() {
                eprintln!("error: {e}\n   (pass --asm <path> to dump the generated assembly)");
            } else {
                eprintln!("error: {e}");
            }
            ExitCode::FAILURE
        }
    }
}

fn print_diagnostics(compiled: &yabcompiler_core::Compiled) {
    let d = &compiled.diagnostics;
    let code_bytes = compiled.machine_code.len();
    eprintln!(
        "compiled: {} bytes machine code, {} bytes total .prg",
        code_bytes,
        compiled.prg_bytes.len()
    );
    eprintln!(
        "  layout : load ${:04X}, code ends ${:04X} ({} bytes)",
        d.start_address, d.end_address, code_bytes
    );
    if d.bss_truncated > 0 {
        // Trailing zeros are initialized at startup and omitted from the file.
        eprintln!(
            "  bss    : {} trailing zero byte(s) left out of the file and zero-filled at entry",
            d.bss_truncated
        );
    }
    eprintln!("  extraram: {}", d.extraram);
    if let Some((before, after)) = d.size_profile_heap_improvement {
        let layout = if d.compact_heap_layout { "compact " } else { "" };
        eprintln!(
            "  profile : selected {layout}--profile=size layout to increase verified heap capacity \
             from {before} to {after} bytes"
        );
    } else if d.size_profile_fallback {
        // Report when layout limits require the size profile.
        eprintln!(
            "  profile : the requested profile did not fit any legal layout; \
             rebuilt at --profile=size (smaller, slower)"
        );
    }
    if let Some((lo, hi)) = d.original_image {
        // Report the preserved input image and relocated compiled code.
        eprintln!(
            "  original: ${lo:04X}-${hi:04X} of the source .prg materialised in place (source bytes are used at runtime)"
        );
    }
    if !d.manual_reserved_clipped.is_empty() {
        // Report reservations that overlap the preserved input image.
        let what = d
            .manual_reserved_clipped
            .iter()
            .map(|(lo, hi)| {
                if lo == hi {
                    format!("${lo:04X}")
                } else {
                    format!("${lo:04X}-${hi:04X}")
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        eprintln!(
            "  reserved: {what} dropped from --reserved: it falls inside the image handed \
             back verbatim above, where no compiled code is placed anyway"
        );
    }
    if d.vector_pads > 0 {
        // Indirect jump vectors must not start at a page's last byte.
        let where_at = d
            .indirect_vectors
            .iter()
            .map(|(name, addr)| format!("{name}=${addr:04X}"))
            .collect::<Vec<_>>()
            .join(", ");
        eprintln!(
            "  vectors : {} pad byte(s) inserted to keep indirect-JMP vectors off a \
             $xxFF page boundary ({where_at})",
            d.vector_pads
        );
    }
    if d.bridge_shims > 0 {
        // Padding keeps reserved-range jumps separate from labels and their data.
        eprintln!(
            "  reserved: {} filler block(s) inserted so no label is stranded on a \
             reserved-range bridge",
            d.bridge_shims
        );
    }
    if d.zp_pressure.pool_initial > 0 {
        let zp = &d.zp_pressure;
        let mut line = format!(
            "  zp pool : {}/{} bytes used",
            zp.pool_initial - zp.pool_left,
            zp.pool_initial
        );
        if zp.denied_total() > 0 {
            let mut denied = Vec::new();
            for (n, what) in [
                (zp.counter_denied, "FOR counter(s)"),
                (zp.array_base_denied, "array base(s)"),
                (zp.int_denied, "int var(s)"),
                (zp.shadow_denied, "shadow int(s)"),
            ] {
                if n > 0 {
                    denied.push(format!("{n} {what}"));
                }
            }
            line.push_str(&format!(", left out: {}", denied.join(", ")));
        }
        eprintln!("{line}");
    }
    {
        // Report the program's zero-page use and allocation limits.
        let zp = &d.zp_pressure;
        let touched = zp.program_zp_writes + zp.program_zp_reads;
        if touched > 0 || zp.pairs_relocated > 0 || zp.addrs_unprotectable > 0 {
            eprintln!(
                "  zp reach: program touches {} byte(s) ({} write, {} read); \
                 addresses: {} clear of ZP, {} narrowed, {} unbounded",
                touched,
                zp.program_zp_writes,
                zp.program_zp_reads,
                zp.addrs_proved_clear,
                zp.addrs_narrowed,
                zp.addrs_unprotectable
            );
            if zp.pairs_relocated > 0 || zp.pool_lost_to_program > 0 {
                eprintln!(
                    "            relocated {} pointer pair(s), pool lost {} byte(s)",
                    zp.pairs_relocated, zp.pool_lost_to_program
                );
            }
            if zp.pairs_unresolved > 0 {
                eprintln!(
                    "  WARNING : {} compiler pointer pair(s) had to stay on zero page \
                     the program also uses — expect corruption",
                    zp.pairs_unresolved
                );
            }
            if zp.machine_code_calls > 0 {
                eprintln!(
                    "            {} SYS/USR call(s): machine code we cannot analyse. \
                     DATA cursor moved to absolute memory; add --safe-sys-calls \
                     if the routine also clobbers promoted zero-page variables.",
                    zp.machine_code_calls
                );
            }
        }
    }
    if !d.sys_param_vars.is_empty() {
        // `SYS a,<text>` hands the text to the callee at TXTPTR.
        let total = d.sys_param_vars.len();
        let unhandled: Vec<_> = d.sys_param_vars.iter().filter(|s| !s.substituted).collect();
        eprintln!(
            "  sys params: {total} variable reference(s) in SYS parameter text; \
             {} substituted, {} not",
            total - unhandled.len(),
            unhandled.len()
        );
        // Include totals for sites omitted from the detailed diagnostics.
        const SHAPES: [&str; 5] = [
            "FN call",
            "array",
            "string variable",
            "string array",
            "numeric scalar",
        ];
        let shape_ix = |s: &&yabcompiler_core::sys_params::Site| -> usize {
            let string = matches!(s.var.kind, yabcompiler_core::ast::VarKind::String);
            match (s.var.is_fn, s.var.subscripted, string) {
                (true, _, _) => 0,
                (_, true, false) => 1,
                (_, false, true) => 2,
                (_, true, true) => 3,
                (_, false, false) => 4,
            }
        };
        if !unhandled.is_empty() {
            let mut by_shape = [0usize; 5];
            for s in &unhandled {
                by_shape[shape_ix(s)] += 1;
            }
            eprintln!(
                "              not substituted by shape: {} array, {} string variable, \
                 {} string array, {} FN call, {} numeric scalar",
                by_shape[1], by_shape[2], by_shape[3], by_shape[0], by_shape[4]
            );
        }
        for s in unhandled.iter().take(6) {
            let shape = SHAPES[shape_ix(s)];
            eprintln!(
                "  WARNING : line {}: SYS parameter text references {} `{}` — \
                 no compiled-value bridge; the callee parses the original tokens",
                s.line, shape, s.var.base
            );
        }
        if unhandled.len() > 6 {
            eprintln!("            ... and {} more", unhandled.len() - 6);
        }
        if d.sys_param_substitution_dropped {
            eprintln!(
                "  WARNING : the image did not fit with SYS-parameter substitution, so it \
                 was dropped: ALL {total} reference(s) are left to the native parser"
            );
        }
    }
    if !d.effective_reserved.is_empty() {
        let source = match (d.auto_reserved.is_empty(), d.manual_reserved.is_empty()) {
            (true, true) => "",
            (false, true) => " (auto)",
            (true, false) => " (manual)",
            (false, false) => " (auto + manual)",
        };
        eprintln!(
            "  reserved{source}: {}",
            format_ranges(&d.effective_reserved)
        );
    }
    for o in &d.reserved_overshoots {
        // A contiguous load image includes padding for every reserved hole it spans.
        eprintln!(
            "  warning: image ends ${:04X}, {} byte(s) past reserved ${:04X}-${:04X}; \
             the hole costs {} padding byte(s) in the .prg",
            u32::from(o.range.1) + o.overshoot,
            o.overshoot,
            o.range.0,
            o.range.1,
            o.hole,
        );
        // Removing the overshoot also removes the hole's padding and jump bridge.
        eprintln!(
            "           losing those {} byte(s) of code would drop the image below \
             ${:04X} and shrink the file by at least {} byte(s)",
            o.overshoot,
            o.range.0,
            o.recoverable(),
        );
    }
    // Report statements that raise BASIC errors when reached.
    for t in &d.syntax_traps {
        let msg = if t.code == 22 {
            "?TYPE MISMATCH"
        } else {
            "?SYNTAX ERROR"
        };
        eprintln!(
            "  warning: {} — compiled to a runtime {msg}, raised only if line {} runs",
            t.detail, t.line
        );
    }
    // Invalid DEF FN bodies raise errors at their call sites.
    for f in &d.deferred_fn_bodies {
        let msg = if f.code == 22 {
            "?TYPE MISMATCH"
        } else {
            "?SYNTAX ERROR"
        };
        eprintln!(
            "  warning: {} — DEF FN {} body dropped; the ROM parses a body only when the \
             function is called, so this raises {msg} at a call site, not at line {}",
            f.detail, f.name.0, f.line
        );
    }
    for s in &d.skipped_statements {
        let scope = if s.whole_conditional {
            "conditional dropped"
        } else {
            "statement dropped"
        };
        eprintln!(
            "  warning: line {}: token ${:02X} is not supported, {scope}",
            s.line, s.token
        );
    }
}

fn format_ranges(ranges: &[(u16, u16)]) -> String {
    ranges
        .iter()
        .map(|(s, e)| {
            if s == e {
                format!("${s:04X}")
            } else {
                format!("${s:04X}-${e:04X}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}
