//! Behavioural corpus under `--profile=size`.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

#[test]
fn synthetic_corpus_matches_golden_at_size_profile() {
    let dir = common::repo_path("test_synthetic");
    let report = common::check_behavior(&dir, |_name, src| {
        let prg = tokenize_program(src).ok()?;
        let opts = CompileOptions {
            profile: Profile::Size,
            ..CompileOptions::default()
        };
        compile_with_options(&prg, opts).ok().map(|c| c.prg_bytes)
    });
    report.assert_ok("test_synthetic (size profile)");
}
