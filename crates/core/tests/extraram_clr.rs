

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

/// Compile `src` at `profile`, with extraram forced on or off.
fn build(src: &str, profile: Profile, extraram: bool) -> yabcompiler_core::Compiled {
    let prg = tokenize_program(src).expect("tokenize");
    let opts = CompileOptions {
        profile,
        extraram,
        force_extraram_off: !extraram,
        ..CompileOptions::default()
    };
    compile_with_options(&prg, opts).expect("compile")
}

/// Byte offset of the line consisting solely of `label`.
fn label_pos(asm: &str, label: &str) -> Option<usize> {
    let mut off = 0usize;
    for line in asm.lines() {
        if line.trim() == label {
            return Some(off);
        }
        off += line.len() + 1;
    }
    None
}
fn byte_count(line: &str) -> usize {
    let t = line.trim();
    match t.strip_prefix(".byte") {
        Some(rest) => rest.split(',').filter(|f| !f.trim().is_empty()).count(),
        None => match t.strip_prefix(".word") {
            Some(rest) => rest.split(',').filter(|f| !f.trim().is_empty()).count() * 2,
            None => 0,
        },
    }
}

/// Total data bytes between `__VARS_START:` and `__VARS_END:`, plus the list of labels defined
/// inside that span.
fn vars_span(asm: &str) -> (usize, Vec<String>) {
    let start = label_pos(asm, "__VARS_START:").expect("__VARS_START defined");
    let end = label_pos(asm, "__VARS_END:").expect("__VARS_END defined");
    assert!(start < end, "__VARS_START must precede __VARS_END");
    let mut bytes = 0usize;
    let mut labels = Vec::new();
    for line in asm[start..end].lines().skip(1) {
        let t = line.trim();
        if t.ends_with(':') && !t.starts_with(';') {
            labels.push(t.trim_end_matches(':').to_string());
        }
        bytes += byte_count(line);
    }
    (bytes, labels)
}
fn defined_var_labels(asm: &str) -> Vec<String> {
    asm.lines()
        .map(str::trim)
        .filter(|t| t.ends_with(':'))
        .map(|t| t.trim_end_matches(':').to_string())
        .filter(|l| l.starts_with("V_") || l.starts_with("VI_") || l.starts_with("VS_"))
        .collect()
}
const MANY: &str = "10 A=5:B$=\"X\":C%=7\n\
                    20 FOR I=1 TO 3:S=S+I:NEXT\n\
                    30 FOR K=1 TO 4:T=T+K:NEXT\n\
                    40 CLR\n\
                    50 PRINT A;C%;S;T;I;K\n\
                    60 PRINT LEN(B$)\n";

const SIMPLE: &str = "10 A=5\n20 CLR\n30 PRINT A\n";

/// `RUN` restarts the program; the cassette-buffer byte at 828 ($033C) survives the restart (it
/// isn't part of the variable pool), so it can gate the second pass.
const RUNCLR: &str = "10 IF PEEK(828)=1 THEN 60\n\
                      20 POKE 828,1\n\
                      30 A=5\n\
                      40 PRINT A\n\
                      50 RUN\n\
                      60 PRINT A\n\
                      70 POKE 828,0\n";

// Layout

#[test]
fn extraram_vars_anchors_are_not_empty() {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let asm = build(SIMPLE, profile, true).asm;
        let (bytes, labels) = vars_span(&asm);
        assert_eq!(
            labels,
            vec!["V_A".to_string()],
            "the one variable slot must sit inside the anchors ({profile:?}):\n{asm}"
        );
        assert_eq!(
            bytes, 5,
            "V_A is a 5-byte MFLPT slot; the memzero count must be 5, not 0 ({profile:?})"
        );
    }
}

/// The anchors must hoist WITH the pool: both above `__EXRAM_CODE_START`, in the always-RAM low
/// half.
#[test]
fn extraram_vars_anchors_hoist_with_the_pool() {
    let asm = build(MANY, Profile::Default, true).asm;
    let code_start = label_pos(&asm, "__EXRAM_CODE_START:").expect("code-start boundary");
    for anchor in ["__VARS_START:", "__VARS_END:"] {
        let pos = label_pos(&asm, anchor).unwrap_or_else(|| panic!("{anchor} defined:\n{asm}"));
        assert!(
            pos < code_start,
            "{anchor} must be hoisted above __EXRAM_CODE_START:\n{asm}"
        );
    }
}
#[test]
fn vars_span_covers_every_slot_in_both_layouts() {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let off = build(MANY, profile, false).asm;
        let on = build(MANY, profile, true).asm;

        let (off_bytes, off_labels) = vars_span(&off);
        let (on_bytes, on_labels) = vars_span(&on);

        assert_eq!(
            off_labels, on_labels,
            "extraram and non-extraram must enclose the same slots ({profile:?})"
        );
        assert_eq!(
            off_bytes, on_bytes,
            "memzero count must match across layouts ({profile:?})"
        );
        assert!(off_bytes > 0, "non-empty span ({profile:?})");

        // No variable slot may be left outside the anchors.
        for layout in [(&off, "extraram off"), (&on, "extraram on")] {
            let (_, inside) = vars_span(layout.0);
            for label in defined_var_labels(layout.0) {
                assert!(
                    inside.contains(&label),
                    "{label} is defined outside __VARS_START..__VARS_END ({}, {profile:?})",
                    layout.1
                );
            }
        }
    }
}
#[test]
fn state_reset_still_uses_one_memzero_over_the_label_difference() {
    for extraram in [false, true] {
        let asm = build(MANY, Profile::Default, extraram).asm;
        let reset = asm
            .split("__STATE_RESET:")
            .nth(1)
            .unwrap_or_else(|| panic!("__STATE_RESET emitted (extraram={extraram}):\n{asm}"));
        // Only look at the routine body, up to its RTS.
        let body = reset.split("\n    RTS").next().unwrap();
        assert!(
            body.contains("LDA #<__VARS_START") && body.contains("LDA #>__VARS_START"),
            "variable wipe still starts at __VARS_START (extraram={extraram}):\n{body}"
        );
        assert!(
            body.contains("LDA #<(__VARS_END-__VARS_START)")
                && body.contains("LDA #>(__VARS_END-__VARS_START)"),
            "count is still the live label difference (extraram={extraram}):\n{body}"
        );
        assert_eq!(
            body.matches("LDA #<__VARS_START").count(),
            1,
            "exactly one bulk wipe of the variable pool (extraram={extraram}):\n{body}"
        );
    }
}

// Behaviour

/// Cross-layout identity: the extraram and non-extraram builds of the same program must produce
/// the same output.
#[test]
fn clr_clears_variables_in_both_layouts() {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let off = build(SIMPLE, profile, false).prg_bytes;
        let on = build(SIMPLE, profile, true).prg_bytes;
        let off_out = common::run_clean(&off)
            .unwrap_or_else(|| panic!("extraram-off build ran clean ({profile:?})"));
        let on_out = common::run_clean(&on)
            .unwrap_or_else(|| panic!("extraram-on build ran clean ({profile:?})"));
        assert_eq!(
            off_out.trim_end(),
            " 0",
            "BASIC V2: CLR zeroes A ({profile:?})"
        );
        assert_eq!(
            on_out, off_out,
            "extraram must agree with non-extraram ({profile:?})"
        );
    }
}

/// Same, with a pool holding float / integer / string slots and two FOR counters: `CLR` must
/// zero all of them, so BASIC V2 prints six zeroes.
#[test]
fn clr_clears_every_variable_kind_in_both_layouts() {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let off = build(MANY, profile, false).prg_bytes;
        let on = build(MANY, profile, true).prg_bytes;
        let off_out = common::run_clean(&off)
            .unwrap_or_else(|| panic!("extraram-off build ran clean ({profile:?})"));
        let on_out = common::run_clean(&on)
            .unwrap_or_else(|| panic!("extraram-on build ran clean ({profile:?})"));
        assert_eq!(
            off_out.split_whitespace().collect::<Vec<_>>(),
            vec!["0", "0", "0", "0", "0", "0", "0"],
            "CLR zeroes every variable ({profile:?}); got {off_out:?}"
        );
        assert_eq!(
            on_out, off_out,
            "extraram must agree with non-extraram ({profile:?})"
        );
    }
}
#[test]
fn const_var_prop_still_fires_without_run_or_clr() {
    let folded = build("10 A=5\n20 PRINT A\n", Profile::Default, false).asm;
    assert!(
        !folded.contains("V_A"),
        "A must still fold to a literal when nothing resets variables:\n{folded}"
    );
    let kept = build(RUNCLR, Profile::Default, false).asm;
    assert!(
        kept.contains("V_A"),
        "A must keep a runtime slot when RUN can clear it:\n{kept}"
    );
    let kept_clr = build(SIMPLE, Profile::Default, false).asm;
    assert!(
        kept_clr.contains("V_A"),
        "A must keep a runtime slot when CLR can clear it:\n{kept_clr}"
    );
}
#[test]
fn corpus_clr_programs_agree_across_layouts() {
    for name in ["clr_test", "dim_dyn_clr"] {
        let prg = std::fs::read(common::repo_path(&format!("test_corpus/{name}.prg")))
            .unwrap_or_else(|e| panic!("read {name}.prg: {e}"));
        let golden = std::fs::read_to_string(common::repo_path(&format!(
            "test_corpus/emu_golden/{name}.out"
        )))
        .unwrap_or_else(|e| panic!("read {name}.out: {e}"));

        for profile in [Profile::Default, Profile::Speed, Profile::Size] {
            let mut outs = Vec::new();
            for extraram in [false, true] {
                let opts = CompileOptions {
                    profile,
                    extraram,
                    force_extraram_off: !extraram,
                    ..CompileOptions::default()
                };
                let built = compile_with_options(&prg, opts)
                    .unwrap_or_else(|e| panic!("{name} compile (extraram={extraram}): {e}"));
                outs.push(common::run_clean(&built.prg_bytes).unwrap_or_else(|| {
                    panic!("{name} ran clean (extraram={extraram}, {profile:?})")
                }));
            }
            assert_eq!(
                outs[1], outs[0],
                "{name}: extraram output must match non-extraram ({profile:?})"
            );
            assert_eq!(
                outs[0].trim_end(),
                golden.trim_end(),
                "{name}: both layouts must match the existing golden ({profile:?})"
            );
        }
    }
}
#[test]
fn run_clears_variables_in_both_layouts() {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let off = build(RUNCLR, profile, false).prg_bytes;
        let on = build(RUNCLR, profile, true).prg_bytes;
        let off_out = common::run_clean(&off)
            .unwrap_or_else(|| panic!("extraram-off build ran clean ({profile:?})"));
        let on_out = common::run_clean(&on)
            .unwrap_or_else(|| panic!("extraram-on build ran clean ({profile:?})"));
        assert_eq!(
            off_out.split_whitespace().collect::<Vec<_>>(),
            vec!["5", "0"],
            "RUN restarts with A cleared ({profile:?}); got {off_out:?}"
        );
        assert_eq!(
            on_out, off_out,
            "extraram must agree with non-extraram ({profile:?})"
        );
    }
}
