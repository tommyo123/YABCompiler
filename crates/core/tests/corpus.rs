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

/// Programs that compile but never return to `READY`, so there is no end-of-run screen to compare.
const NON_TERMINATING: &[&str] = &["reject_cont"];

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
fn reject_cont_compiles_and_hangs_like_the_rom() {
    let path = corpus_root().join("reject_cont.prg");
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let prg = common::compile_prg(&bytes).expect("a reached bare CONT is a hang, not a refusal");
    let r = emu64::run_prg_to_end(&prg, common::MAX_INSNS).expect("runs");
    let out = common::petscii_to_string(&r.output);
    assert!(out.contains("BEFORE"), "{out:?}");
    assert!(!out.contains("AFTER"), "{out:?}");
    assert_eq!(r.error_banner(), None, "the ROM prints nothing here");
    assert!(!r.clean_exit, "must never reach READY");
}

#[test]
fn corpus_behavior_matches_golden() {
    let root = corpus_root();
    // Prefer the committed `.prg` twin as compiler input (the path this
    // corpus exists to cover); `check_behavior` falls back to
    // tokenizing the `.bas` if it is missing or fails to compile.
    let report = common::check_behavior(&root, |name, _src| {

        if NON_TERMINATING.contains(&name) {
            return None;
        }
        let prg = root.join(format!("{name}.prg"));
        let bytes = std::fs::read(prg).ok()?;
        common::compile_prg(&bytes).ok()
    });
    report.assert_ok("test_corpus");
}
