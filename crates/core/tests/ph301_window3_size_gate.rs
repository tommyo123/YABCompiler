//! PH301's 3-instruction window is `Profile::Size` only.

use yabcompiler_core::codegen::Profile;
use yabcompiler_core::peephole;
use yabcompiler_core::{CompileOptions, compile_with_options, tokenize_program};

/// Instruction budget for the behavioural arm.
const MAX_INSNS: u64 = 40_000_000;

fn repeated(body: &[&str], n: usize) -> String {
    let mut s = String::new();
    s.push_str("    LDA #$00\n");
    for i in 0..n {
        for line in body {
            s.push_str("    ");
            s.push_str(line);
            s.push('\n');
        }
        // $0400.. is screen RAM: observable, so no liveness rule may delete these, and each address differs so no two separators are interchangeable.
        s.push_str(&format!("    STA ${:04X}\n", 0x0400 + i));
    }
    s.push_str("    RTS\n");
    s
}

/// The 3-instruction window under test.
const W3: &[&str] = &["LDA $C1", "JSR $BBA2", "STA $FB"];

const W4: &[&str] = &["LDA $C1", "JSR $BBA2", "STA $FB", "LDY #$07"];

fn factored(asm: &str, profile: Profile) -> bool {
    peephole::run(asm, profile).contains("__SEQ_")
}

#[test]
fn window_3_factors_only_at_size() {
    let asm = repeated(W3, 6);
    assert!(
        factored(&asm, Profile::Size),
        "Profile::Size must take the 3-window:\n{}",
        peephole::run(&asm, Profile::Size)
    );
    assert!(
        !factored(&asm, Profile::Default),
        "Profile::Default must NOT take the 3-window:\n{}",
        peephole::run(&asm, Profile::Default)
    );
    assert!(
        !factored(&asm, Profile::Speed),
        "Profile::Speed must NOT take the 3-window:\n{}",
        peephole::run(&asm, Profile::Speed)
    );
}

#[test]
fn window_4_factors_at_every_profile() {
    let asm = repeated(W4, 6);
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        assert!(
            factored(&asm, profile),
            "the 4-window is not gated and must factor at {profile:?}:\n{}",
            peephole::run(&asm, profile)
        );
    }
}

#[test]
fn three_occurrences_never_factor_even_at_size() {
    let asm = repeated(W3, 3);
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        assert!(
            !factored(&asm, profile),
            "3 sites are below threshold and must not factor at {profile:?}:\n{}",
            peephole::run(&asm, profile)
        );
    }
}

#[test]
fn size_factoring_is_a_call_not_a_deletion() {
    let sites = 6;
    let out = peephole::run(&repeated(W3, sites), Profile::Size);
    let calls = out.matches("JSR __SEQ_").count();
    assert_eq!(
        calls, sites,
        "every occurrence must become a call, got {calls} of {sites}:\n{out}"
    );
    let label = out
        .lines()
        .find(|l| l.trim_start().starts_with("__SEQ_") && l.trim_end().ends_with(':'))
        .unwrap_or_else(|| panic!("no __SEQ_ helper definition:\n{out}"))
        .trim()
        .trim_end_matches(':')
        .to_string();
    let body: String = out
        .split(&format!("{label}:"))
        .nth(1)
        .unwrap_or_else(|| panic!("helper {label} has no body:\n{out}"))
        .lines()
        .take_while(|l| !l.trim_start().starts_with("RTS"))
        .collect::<Vec<_>>()
        .join("\n");
    for insn in W3 {
        assert!(
            body.contains(insn),
            "helper body lost `{insn}`:\n{body}\n--- full ---\n{out}"
        );
    }
    assert!(
        out.contains(&format!("{label}:")) && out[out.find(&format!("{label}:")).unwrap()..].contains("RTS"),
        "helper {label} must return:\n{out}"
    );
}

fn source() -> String {
    let mut s = String::new();
    s.push_str("10 A=3:B=5:C=0\n");
    let mut line = 20;
    for i in 0..60 {
        s.push_str(&format!("{line} C=C+A*B+{i}:D=C/2:PRINT C;D\n"));
        line += 10;
        s.push_str(&format!("{line} E$=\"X\"+STR$({i}):PRINT E$;\n"));
        line += 10;
    }
    s.push_str(&format!("{line} PRINT:PRINT \"END\";C\n"));
    s
}

fn build(profile: Profile) -> (Vec<u8>, String) {
    let prg = tokenize_program(&source()).expect("tokenize");
    let opts = CompileOptions {
        profile,
        ..CompileOptions::default()
    };
    let c = compile_with_options(&prg, opts)
        .unwrap_or_else(|e| panic!("compile at {profile:?}: {e}"));
    (c.prg_bytes, c.asm)
}

/// Distinct `__SEQ_n` helpers defined in a listing.
fn seq_helpers(asm: &str) -> std::collections::BTreeSet<String> {
    asm.lines()
        .filter_map(|l| {
            let t = l.trim();
            let name = t.strip_suffix(':')?;
            name.starts_with("__SEQ_").then(|| name.to_string())
        })
        .collect()
}

#[test]
fn size_image_is_smaller_and_behaves_identically() {
    let (def, def_asm) = build(Profile::Default);
    let (size, size_asm) = build(Profile::Size);
    assert!(
        size.len() < def.len(),
        "Size image ({}) is not smaller than Default ({})",
        size.len(),
        def.len()
    );

    let at_size = seq_helpers(&size_asm);
    let at_default = seq_helpers(&def_asm);
    assert!(
        at_size.len() > at_default.len(),
        "Size must factor MORE sequence helpers than Default; \
         Default has {} {:?}, Size has {} {:?}",
        at_default.len(),
        at_default,
        at_size.len(),
        at_size
    );

    let run = |img: &[u8], label: &str| {
        let r = emu64::run_prg_to_end(img, MAX_INSNS)
            .unwrap_or_else(|e| panic!("{label} failed to run: {e:?}"));
        assert!(r.clean_exit, "{label} did not reach a clean exit");
        r.output
    };
    let a = run(&def, "Default");
    let b = run(&size, "Size");

    assert!(
        a.len() > 400,
        "Default output is too short to be evidence: {} bytes",
        a.len()
    );
    assert_eq!(
        String::from_utf8_lossy(&a),
        String::from_utf8_lossy(&b),
        "Size image printed something different from Default"
    );
}
