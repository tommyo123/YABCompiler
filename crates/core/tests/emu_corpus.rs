//! Behavioural corpus: every deterministic program in
//! `test_synthetic/` is tokenized and compiled in process, run through
//! the `emu64` C64 emulator, and its PRINT output compared against a
//! golden file. This exercises the full `.bas → tokenize → compile →
//! run` path users hit through the CLI and GUI.
//!
//! Regenerate goldens after an intentional codegen change:
//!     UPDATE_EMU_GOLDEN=1 cargo test -p yabcompiler-core --test emu_corpus

mod common;

#[test]
fn synthetic_corpus_matches_golden() {
    let dir = common::repo_path("test_synthetic");
    // Always tokenize the source in process — there are no `.prg` twins.
    let report = common::check_behavior(&dir, |_name, _src| None);
    report.assert_ok("test_synthetic");
}
