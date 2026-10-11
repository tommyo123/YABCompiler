//! Shared helpers for the in-process corpus tests (`corpus.rs`,
//! `emu_corpus.rs`).
//!
//! Both tests compile BASIC programs and run the result through the
//! vendored `emu64` C64 emulator. emu64 executes the real BASIC and
//! KERNAL ROM, so captured PRINT output is a faithful reference.

#![allow(dead_code)] // each test binary uses a different subset.

use std::path::{Path, PathBuf};

use yabcompiler_core::{CompileOptions, compile_with_options, tokenize_program};

/// Instruction budget per program. Comfortably covers the heaviest
/// corpus programs (bubble sort, recursion) while still terminating a
/// runaway program so the test fails instead of hanging.
pub const MAX_INSNS: u64 = 80_000_000;

/// Word-ish tokens whose presence makes a program unsuitable for
/// deterministic headless replay: emu64 returns no key for
/// `INPUT`/`GET`, has no entropy for `RND`, never ticks the jiffy clock
/// for `TI`/`TI$`, and cannot run the machine code behind
/// `SYS`/`USR`/`WAIT`.
const SKIP_TOKENS: &[&str] = &["INPUT", "GET", "RND", "WAIT", "USR", "SYS", "TI$", "TI"];

/// Resolve a path relative to the repository root (two levels up from
/// this crate).
pub fn repo_path(sub: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(sub)
}

/// Decode a CHROUT byte stream to readable text.
pub fn petscii_to_string(bytes: &[u8]) -> String {
    let mut s = String::new();
    for &b in bytes {
        match b {
            0x0d => s.push('\n'),
            0x1d => s.push(' '),
            0x20..=0x5a => s.push(b as char),
            0xc1..=0xda => s.push((b - 0x80) as char),
            _ => {}
        }
    }
    s
}

/// Whether a program should be skipped for behavioural (run) testing.
/// Tokens are matched on non-alphanumeric boundaries so a variable like
/// `LISTING` does not trip on `TI`.
pub fn should_skip(src: &str) -> bool {
    let up = src.to_uppercase();
    let bytes = up.as_bytes();
    SKIP_TOKENS.iter().any(|t| {
        up.match_indices(t).any(|(i, _)| {
            let before = i.checked_sub(1).map(|j| bytes[j]);
            let after = bytes.get(i + t.len()).copied();
            let free = |c: Option<u8>| !matches!(c, Some(b) if b.is_ascii_alphanumeric());
            free(before) && (t.ends_with('$') || free(after))
        })
    })
}

/// Compile already-tokenized PRG bytes, returning the runnable image.
pub fn compile_prg(prg: &[u8]) -> Result<Vec<u8>, String> {
    compile_with_options(prg, CompileOptions::default())
        .map(|c| c.prg_bytes)
        .map_err(|e| e.to_string())
}

/// Tokenize `.bas` source, then compile it.
pub fn compile_source(src: &str) -> Result<Vec<u8>, String> {
    let prg = tokenize_program(src).map_err(|e| format!("tokenize: {e:?}"))?;
    compile_prg(&prg)
}

/// Run a compiled image to completion in emu64. Returns the decoded
/// PRINT output, or `None` if it fails to run or never reaches a clean
/// exit (so the caller can skip it rather than assert on noise).
pub fn run_clean(prg_bytes: &[u8]) -> Option<String> {
    let r = emu64::run_prg_to_end(prg_bytes, MAX_INSNS).ok()?;
    r.clean_exit.then(|| petscii_to_string(&r.output))
}

/// True when goldens should be (re)written rather than asserted.
pub fn updating_goldens() -> bool {
    std::env::var("UPDATE_EMU_GOLDEN").is_ok()
}

/// Outcome of a behavioural corpus run, suitable for asserting on.
#[derive(Default)]
pub struct CorpusReport {
    pub checked: usize,
    pub skipped: usize,
    pub no_golden: Vec<String>,
    pub mismatches: Vec<(String, String, String)>,
}

impl CorpusReport {
    /// Print a summary and panic if anything failed (unless we were
    /// only (re)generating goldens).
    pub fn assert_ok(&self, label: &str) {
        eprintln!(
            "{label}: {} checked, {} skipped, {} without golden, {} mismatches",
            self.checked,
            self.skipped,
            self.no_golden.len(),
            self.mismatches.len()
        );
        if updating_goldens() {
            eprintln!("(goldens written)");
            return;
        }
        if !self.no_golden.is_empty() {
            eprintln!(
                "no golden for: {} (run with UPDATE_EMU_GOLDEN=1)",
                self.no_golden.join(", ")
            );
        }
        for (name, expected, got) in &self.mismatches {
            eprintln!("--- mismatch: {name}\n  expected: {expected:?}\n  got:      {got:?}");
        }
        assert!(
            self.mismatches.is_empty(),
            "{}: {} output mismatch(es)",
            label,
            self.mismatches.len()
        );
        assert!(self.checked > 0, "{label}: no programs were checked");
    }
}

/// Compile and run every deterministic `.bas` in `bas_dir`, comparing
/// the captured output against goldens in `bas_dir/emu_golden/`. With
/// `UPDATE_EMU_GOLDEN=1` set, goldens are (re)written instead.
///
/// `compiled` lets the caller supply already-tokenized PRG bytes (e.g.
/// from a committed `.prg` twin) instead of tokenizing the `.bas` in
/// process; return `None` to fall back to tokenizing the source.
pub fn check_behavior(
    bas_dir: &Path,
    compiled: impl Fn(&str, &str) -> Option<Vec<u8>>,
) -> CorpusReport {
    let golden_dir = bas_dir.join("emu_golden");
    let update = updating_goldens();
    if update {
        std::fs::create_dir_all(&golden_dir).expect("create golden dir");
    }

    let mut programs: Vec<PathBuf> = std::fs::read_dir(bas_dir)
        .expect("read corpus dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|x| x == "bas").unwrap_or(false))
        .collect();
    programs.sort();

    let mut report = CorpusReport::default();
    for path in &programs {
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let src = std::fs::read_to_string(path).expect("read .bas");

        if should_skip(&src) {
            report.skipped += 1;
            continue;
        }
        let prg = match compiled(&name, &src).or_else(|| compile_source(&src).ok()) {
            Some(p) => p,
            None => {
                report.skipped += 1;
                continue;
            }
        };
        let out = match run_clean(&prg) {
            Some(o) => o,
            None => {
                report.skipped += 1;
                continue;
            }
        };

        let golden = golden_dir.join(format!("{name}.out"));
        if update {
            std::fs::write(&golden, &out).expect("write golden");
            report.checked += 1;
            continue;
        }
        match std::fs::read_to_string(&golden) {
            Ok(expected) => {
                report.checked += 1;
                // Goldens are written with `\n`, but a checkout under
                // core.autocrlf or an edit in a Windows editor can store
                // them with `\r\n`. Emulator output only ever uses `\n`
                // (the C64 line terminator $0D is decoded to `\n`), so
                // normalise the golden's line endings before comparing.
                let expected = expected.replace("\r\n", "\n").replace('\r', "\n");
                if expected != out {
                    report.mismatches.push((name, expected, out));
                }
            }
            Err(_) => report.no_golden.push(name),
        }
    }
    report
}

pub fn expand_call_entries(asm: &str) -> String {
    let lines: Vec<&str> = asm.lines().collect();
    let entry_name = |l: &str| {
        let t = l.trim();
        t.strip_suffix(':').filter(|n| n.starts_with("__PH326_")).map(str::to_string)
    };
    let is_label = |l: &str| {
        let t = l.trim();
        !l.starts_with(' ') && t.ends_with(':') && !t.contains(char::is_whitespace)
    };
    let mut entries: std::collections::HashMap<String, (Vec<String>, String)> =
        std::collections::HashMap::new();
    let mut dropped = std::collections::HashSet::new();
    let mut i = 0;
    while i < lines.len() {
        if entry_name(lines[i]).is_none() {
            i += 1;
            continue;
        }
        let start = i;
        let mut j = i;
        while j < lines.len() && (entry_name(lines[j]).is_some() || !is_label(lines[j])) {
            j += 1;
        }
        let routine = lines.get(j).map_or(String::new(), |l| l.trim().trim_end_matches(':').to_string());
        for k in start..j {
            dropped.insert(k);
            if let Some(name) = entry_name(lines[k]) {
                let body = lines[k + 1..j]
                    .iter()
                    .filter(|l| entry_name(l).is_none() && !l.trim().is_empty())
                    .map(|l| l.to_string())
                    .collect();
                entries.insert(name, (body, routine.clone()));
            }
        }
        i = j;
    }
    let mut out = String::new();
    for (k, line) in lines.iter().enumerate() {
        if dropped.contains(&k) {
            continue;
        }
        let t = line.trim();
        let call = ["JSR ", "JMP "].iter().find_map(|m| {
            let target = t.strip_prefix(m)?;
            entries.get(target.trim()).map(|e| (m.trim(), e))
        });
        match call {
            Some((mnem, (body, routine))) => {
                for b in body {
                    out.push_str(b);
                    out.push('\n');
                }
                out.push_str(&format!("    {mnem} {routine}\n"));
            }
            None => {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    out
}

const FIRST_RUN: [u8; 9] = [0x20, 0xBF, 0xE3, 0x20, 0x59, 0xA6, 0x4C, 0xAE, 0xA7];
const RUN_AGAIN: [u8; 6] = [0x20, 0x59, 0xA6, 0x4C, 0xAE, 0xA7];
const RUN_STUB: usize = 0x0334;

pub type TypedRun = (String, Option<u8>);

fn run_from(sys: &mut emu64::System, stub: &[u8]) -> TypedRun {
    sys.mem.ram[RUN_STUB..RUN_STUB + stub.len()].copy_from_slice(stub);
    sys.output.clear();
    sys.ready_reached = false;
    sys.stop_key_down = false;
    sys.start_at(RUN_STUB as u16);
    for _ in 0..MAX_INSNS {
        if !sys.step() {
            break;
        }
    }
    assert!(sys.ready_reached, "the program ends at READY");
    (petscii_to_string(&sys.output), sys.error_code)
}

pub fn load_and_run_twice(prg: &[u8]) -> (TypedRun, TypedRun) {
    let mut sys = emu64::System::new();
    let (start, end) = sys.mem.load_prg(prg).expect("loads");
    assert_eq!(start, 0x0801);
    let vartab = end + 1;
    for zp in [0x2Dusize, 0x2F, 0x31] {
        sys.mem.ram[zp] = vartab as u8;
        sys.mem.ram[zp + 1] = (vartab >> 8) as u8;
    }
    sys.mem.ram[0x0281..0x0285].copy_from_slice(&[0x00, 0x08, 0x00, 0xA0]);
    let first = run_from(&mut sys, &FIRST_RUN);
    let second = run_from(&mut sys, &RUN_AGAIN);
    (first, second)
}

pub fn zeroed_offsets(code: &str, label: &str) -> std::collections::BTreeSet<usize> {
    let mut offsets = std::collections::BTreeSet::new();
    let mut count = None;
    for line in code.lines().map(str::trim) {
        if let Some(n) = line.strip_prefix("LDX #$") {
            count = usize::from_str_radix(n, 16).ok();
            continue;
        }
        let Some(target) = line.strip_prefix("STA ").and_then(|t| t.strip_prefix(label)) else {
            continue;
        };
        let (base, indexed) = match target.strip_suffix(",X") {
            Some(t) => (t, true),
            None => (target, false),
        };
        let base: isize = match base {
            "" => 0,
            b => b.parse().expect("offset"),
        };
        match (indexed, count) {
            (true, Some(n)) => offsets.extend((1..=n).map(|x| (base + x as isize) as usize)),
            (false, _) => {
                offsets.insert(base as usize);
            }
            (true, None) => panic!("indexed store outside a loop: {line}"),
        }
    }
    offsets
}
