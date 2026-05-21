//! `emu64` CLI.
//!
//! Usage:
//!   emu64 run <file.prg> <expected-hex> [--max-insns N]
//!
//! Exits 0 on screen-RAM pattern match, 1 on timeout or error.
//! Diagnostics go to stderr.

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let prog = args.first().map(String::as_str).unwrap_or("emu64");

    let sub = args.get(1).map(String::as_str);
    match sub {
        Some("run") => match run(&args[2..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("emu64: {e}");
                ExitCode::FAILURE
            }
        },
        Some("version") | Some("-V") | Some("--version") => {
            println!("emu64 {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("help") | Some("-h") | Some("--help") | None => {
            print_help(prog);
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("emu64: unknown subcommand `{other}`");
            ExitCode::FAILURE
        }
    }
}

fn print_help(prog: &str) {
    println!("emu64 — minimal C64 emulator\n");
    println!("USAGE:");
    println!("    {prog} run <file.prg> <expected-hex> [options]\n");
    println!("OPTIONS:");
    println!("    --max-insns <N>      Instruction budget (default 50_000_000)");
    println!("    --dump-screen        Print the final 25-row screen on exit");
    println!("    --dump-output        Print captured CHROUT bytes (PETSCII hex)");
}

struct RunArgs {
    prg: PathBuf,
    pattern: Vec<u8>,
    max_insns: u64,
    dump_screen: bool,
    dump_output: bool,
}

fn parse_run_args(args: &[String]) -> Result<RunArgs, String> {
    let mut prg: Option<PathBuf> = None;
    let mut pattern: Option<Vec<u8>> = None;
    let mut max_insns: u64 = 50_000_000;
    let mut dump_screen = false;
    let mut dump_output = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--max-insns" => {
                i += 1;
                max_insns = args
                    .get(i)
                    .ok_or("`--max-insns` requires a value")?
                    .parse::<u64>()
                    .map_err(|e| format!("--max-insns: {e}"))?;
            }
            "--dump-screen" => dump_screen = true,
            "--dump-output" => dump_output = true,
            s if !s.starts_with("--") && prg.is_none() => prg = Some(PathBuf::from(s)),
            s if !s.starts_with("--") && pattern.is_none() => {
                pattern = Some(hex_decode(s)?);
            }
            s => return Err(format!("unknown arg `{s}`")),
        }
        i += 1;
    }
    Ok(RunArgs {
        prg: prg.ok_or("missing <file.prg>")?,
        pattern: pattern.ok_or("missing <expected-hex>")?,
        max_insns,
        dump_screen,
        dump_output,
    })
}

fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    let s = s.replace(' ', "");
    if s.len() % 2 != 0 {
        return Err("hex pattern length must be even".into());
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    for i in (0..s.len()).step_by(2) {
        let byte = u8::from_str_radix(&s[i..i + 2], 16)
            .map_err(|e| format!("bad hex `{}`: {e}", &s[i..i + 2]))?;
        out.push(byte);
    }
    Ok(out)
}

fn run(args: &[String]) -> Result<(), String> {
    let opts = parse_run_args(args)?;
    let bytes =
        std::fs::read(&opts.prg).map_err(|e| format!("read {}: {e}", opts.prg.display()))?;

    let started = std::time::Instant::now();
    let result = emu64::run_until_screen_pattern(&bytes, &opts.pattern, opts.max_insns);
    let elapsed = started.elapsed();

    match result {
        Ok(rr) => {
            eprintln!(
                "emu64: match at row {} col {} after {} insns ({:?})",
                rr.matched_row, rr.matched_col, rr.instructions, elapsed
            );
            if opts.dump_output && !rr.output.is_empty() {
                eprintln!("emu64: CHROUT bytes ({} captured):", rr.output.len());
                for chunk in rr.output.chunks(40) {
                    eprintln!(
                        "  {}",
                        chunk
                            .iter()
                            .map(|b| format!("{b:02x}"))
                            .collect::<Vec<_>>()
                            .join(" ")
                    );
                }
            }
            if opts.dump_screen {
                let mut sys = emu64::System::new();
                let entry = sys.load_prg(&bytes)?;
                sys.start_at(entry);
                for _ in 0..rr.instructions {
                    sys.step();
                }
                dump_screen(&sys);
            }
            Ok(())
        }
        Err(e) => {
            // Re-run to recover a final screen state for the dump.
            if opts.dump_screen {
                let mut sys = emu64::System::new();
                let entry = sys.load_prg(&bytes)?;
                sys.start_at(entry);
                for _ in 0..opts.max_insns {
                    if !sys.step() {
                        break;
                    }
                }
                dump_screen(&sys);
            }
            Err(e)
        }
    }
}

fn dump_screen(sys: &emu64::System) {
    let screen = sys.screen_bytes();
    eprintln!("emu64: final 25-row screen at $0400:");
    for r in 0..25 {
        let row = &screen[r * 40..(r + 1) * 40];
        if row.iter().all(|&b| b == 0x00 || b == 0x20) {
            continue;
        }
        let hex = row
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(" ");
        eprintln!("  r{:02}: {}", r, hex);
    }
}
