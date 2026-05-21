use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use yabcompiler_core::{
    CompileOptions, Profile, compile, config, is_basic_source_path, parse_reserved_ranges,
    parse_start_address, prg, tokenize_program,
};

fn usage(program: &str) {
    eprintln!(
        "YABCompiler {} - Yet Another Basic Compiler for the Commodore 64",
        config::VERSION
    );
    eprintln!();
    eprintln!("Usage:");
    eprintln!("    {program} <command> [args]");
    eprintln!();
    eprintln!("Commands:");
    eprintln!("    list <file>           Detokenize a .prg, or echo a .bas after tokenizing.");
    eprintln!("    dump <file>           Show parsed lines and raw token bytes.");
    eprintln!("    tokenize <in> <out>   Tokenize BASIC source (.bas) to a .prg.");
    eprintln!("    compile <in> <out>    Compile a .bas or tokenized .prg to a runnable .prg.");
    eprintln!();
    eprintln!("Compile options:");
    eprintln!("    --asm <file>          Also write the generated 6502 assembly.");
    eprintln!("    --profile=<p>         default | speed | size  (default: default)");
    eprintln!("    --lenient-syntax      Accept BASIC v2 typos that v2 only catches at runtime.");
    eprintln!();
    eprintln!("Memory layout:");
    eprintln!("    --extraram            Force RAM under BASIC ROM ($A000-$BFFF) on.");
    eprintln!("    --force-extraram-off  Disable the auto-extraram predictor.");
    eprintln!("    --no-auto-reserve     Disable auto-discovery of POKE/PEEK target ranges.");
    eprintln!("    --reserved=<list>     Manual reserved ranges. Example: $7800-$79FF,$C000");
    eprintln!(
        "    --start-address=<a>   Load at <a> with no SYS stub. Start with `SYS <decimal>`."
    );
    eprintln!("                          Example: --start-address=$C000  (run with: SYS 49152)");
    eprintln!();
    eprintln!("Input is either a .bas source file or a tokenized .prg. The compiler");
    eprintln!("auto-detects extraram and reserved ranges by default. Use the flags above");
    eprintln!("to override.");
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let program = args.first().map(String::as_str).unwrap_or("yabcompiler");

    if args.len() < 2 {
        usage(program);
        return ExitCode::from(2);
    }

    match args[1].as_str() {
        "list" if args.len() == 3 => run_list(&args[2]),
        "dump" if args.len() == 3 => run_dump(&args[2]),
        "tokenize" if args.len() == 4 => run_tokenize(&args[2], &args[3]),
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
fn load_prg_bytes(path: &str) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("failed to read {path}: {e}"))?;
    if is_basic_source_path(Path::new(path)) {
        let source = String::from_utf8(bytes)
            .map_err(|e| format!("{path} is not valid UTF-8 source: {e}"))?;
        tokenize_program(&source).map_err(|e| format!("tokenize {path}: {e}"))
    } else {
        Ok(bytes)
    }
}

fn run_list(path: &str) -> ExitCode {
    let bytes = match load_prg_bytes(path) {
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
    let bytes = match load_prg_bytes(path) {
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

fn run_tokenize(in_path: &str, out_path: &str) -> ExitCode {
    let source = match std::fs::read_to_string(in_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: failed to read {in_path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let prg = match tokenize_program(&source) {
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

    let bytes = match load_prg_bytes(in_path) {
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
    eprintln!("  extraram: {}", d.extraram);
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
