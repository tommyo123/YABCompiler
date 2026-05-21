//! Compile corpus: every tokenized `.prg` in `test_corpus/` must
//! compile (covering the path where the input is already-tokenized
//! BASIC, as saved by a real C64), the known-unsupported programs must
//! stay rejected, and the deterministic ones must produce the expected
//! output when run through `emu64`.
//!
//! Regenerate behavioural goldens after an intentional codegen change:
//!     UPDATE_EMU_GOLDEN=1 cargo test -p yabcompiler-core --test corpus

mod common;

use std::path::PathBuf;

/// Programs that exercise constructs the compiler intentionally does
/// not support; each must fail to compile.
const EXPECTED_REJECTS: &[&str] = &["reject_cont"];

fn corpus_root() -> PathBuf {
    common::repo_path("test_corpus")
}

fn corpus_prgs() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(corpus_root())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("prg"))
        .collect();
    paths.sort();
    paths
}

#[test]
fn corpus_programs_compile() {
    let mut failures = Vec::new();

    for path in corpus_prgs() {
        let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if EXPECTED_REJECTS.contains(&name) {
            continue;
        }
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                failures.push(format!("{}: read failed: {e}", path.display()));
                continue;
            }
        };
        match common::compile_prg(&bytes) {
            Ok(prg) if prg.len() <= 2 => failures.push(format!("{}: empty output", path.display())),
            Ok(_) => {}
            Err(e) => failures.push(format!("{}: {e}", path.display())),
        }
    }

    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn unsupported_corpus_programs_stay_rejected() {
    for name in EXPECTED_REJECTS {
        let path = corpus_root().join(format!("{name}.prg"));
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert!(
            common::compile_prg(&bytes).is_err(),
            "{} compiled unexpectedly",
            path.display()
        );
    }
}

#[test]
fn corpus_behavior_matches_golden() {
    let root = corpus_root();
    // Prefer the committed `.prg` twin as compiler input (the path this
    // corpus exists to cover); `check_behavior` falls back to
    // tokenizing the `.bas` if it is missing or fails to compile.
    let report = common::check_behavior(&root, |name, _src| {
        if EXPECTED_REJECTS.contains(&name) {
            return None;
        }
        let prg = root.join(format!("{name}.prg"));
        let bytes = std::fs::read(prg).ok()?;
        common::compile_prg(&bytes).ok()
    });
    report.assert_ok("test_corpus");
}
