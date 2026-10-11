mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const TWO_COMPUTED_POKES: &str = "\
10 for q=0 to 1:p=8192+q:gosub 100:next q
20 print peek(8192);peek(8193):end
100 if q=0 then poke p,11:return
110 poke p,22:return
";

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

fn asm_at(src: &str, profile: Profile) -> String {
    let prg = tokenize_program(src).expect("tokenizes");
    let opts = CompileOptions {
        profile,
        ..CompileOptions::default()
    };
    compile_with_options(&prg, opts)
        .unwrap_or_else(|e| panic!("compiles: {e}"))
        .asm
}

fn prg_at(src: &str, profile: Profile) -> Vec<u8> {
    let prg = tokenize_program(src).expect("tokenizes");
    let opts = CompileOptions {
        profile,
        ..CompileOptions::default()
    };
    compile_with_options(&prg, opts)
        .unwrap_or_else(|e| panic!("compiles: {e}"))
        .prg_bytes
}

/// A source line's label, if it declares one (`FOO:` at column 0-ish).
fn label_of(line: &str) -> Option<&str> {
    let t = line.trim_end();
    let t = t.split(';').next().unwrap_or(t).trim_end();
    let name = t.strip_suffix(':')?;
    if name.is_empty() || name.starts_with(char::is_whitespace) {
        return None;
    }
    name.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_')
        .then_some(name)
}

/// The instruction a source line holds, as `(mnem, operand)`.
fn insn_of(line: &str) -> Option<(String, String)> {
    let t = line.split(';').next().unwrap_or(line).trim();
    if t.is_empty() || t.starts_with('.') || t.starts_with('*') || t.ends_with(':') {
        return None;
    }
    let mut it = t.splitn(2, char::is_whitespace);
    let mnem = it.next()?;
    if !mnem.chars().all(|c| c.is_ascii_alphabetic()) || mnem.len() != 3 {
        return None;
    }
    Some((
        mnem.to_ascii_uppercase(),
        it.next().unwrap_or("").trim().to_string(),
    ))
}

fn smc_patched_labels(asm: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in asm.lines() {
        let Some((mnem, op)) = insn_of(line) else {
            continue;
        };
        if !matches!(mnem.as_str(), "STA" | "STX" | "STY") {
            continue;
        }
        let Some((label, offset)) = op.split_once('+') else {
            continue;
        };
        let (label, offset) = (label.trim(), offset.trim());
        if offset.parse::<u16>().is_ok()
            && !label.is_empty()
            && !label.starts_with(|c: char| c.is_ascii_digit())
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !out.iter().any(|l| l == label)
        {
            out.push(label.to_string());
        }
    }
    out
}

fn insn_under_each_smc_label(asm: &str) -> Vec<(String, String, String)> {
    let patched = smc_patched_labels(asm);
    let lines: Vec<&str> = asm.lines().collect();
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let Some(name) = label_of(line) else { continue };
        if !patched.iter().any(|l| l == name) {
            continue;
        }
        for follow in &lines[i + 1..] {
            if follow.trim().is_empty() || follow.trim().starts_with(';') {
                continue;
            }
            if label_of(follow).is_some() {
                continue;
            }
            if let Some((mnem, op)) = insn_of(follow) {
                out.push((name.to_string(), mnem, op));
            }
            break;
        }
    }
    out
}

#[test]
fn smc_patch_target_is_never_a_jump() {
    for profile in PROFILES {
        let asm = asm_at(TWO_COMPUTED_POKES, profile);
        let under = insn_under_each_smc_label(&asm);
        assert!(
            !under.is_empty(),
            "{profile:?}: expected computed POKEs to emit SMC patch sites; \
             the reproducer no longer exercises the pass\n{asm}"
        );
        for (label, mnem, op) in &under {
            assert_ne!(
                mnem, "JMP",
                "{profile:?}: SMC-patched label {label} names `JMP {op}`; \
                 the runtime patch will overwrite the jump target"
            );
        }
    }
}

/// The behaviour the structural check protects.
#[test]
fn computed_pokes_through_a_shared_tail_store_their_bytes() {
    for profile in PROFILES {
        let bytes = prg_at(TWO_COMPUTED_POKES, profile);
        let out = common::run_clean(&bytes)
            .unwrap_or_else(|| panic!("{profile:?}: reaches a clean exit"));
        assert_eq!(out, " 11  22 \n", "{profile:?}");
    }
}

/// A three-way computed POKE, so the sunk group has more than one non-keeper member and a partial refusal is not enough.
const THREE_COMPUTED_POKES: &str = "\
10 for q=0 to 2:p=8192+q:gosub 100:next q
20 print peek(8192);peek(8193);peek(8194):end
100 if q=0 then poke p,11:return
110 if q=1 then poke p,22:return
120 poke p,33:return
";

#[test]
fn three_computed_pokes_all_store_their_bytes() {
    for profile in PROFILES {
        let asm = asm_at(THREE_COMPUTED_POKES, profile);
        for (label, mnem, op) in insn_under_each_smc_label(&asm) {
            assert_ne!(
                mnem, "JMP",
                "{profile:?}: SMC-patched label {label} names `JMP {op}`"
            );
        }
        let bytes = prg_at(THREE_COMPUTED_POKES, profile);
        let out = common::run_clean(&bytes)
            .unwrap_or_else(|| panic!("{profile:?}: reaches a clean exit"));
        assert_eq!(out, " 11  22  33 \n", "{profile:?}");
    }
}

const SINKABLE_TAILS: &str = "\
10 for i=1 to 3
20 gosub 100
30 next i
40 gosub 200
45 if i<0 then 120
50 end
100 print \"A\";i
110 print \"B\";i*2
120 return
200 print \"C\"
210 return
";

#[test]
fn common_tail_sinking_still_fires_without_smc() {
    let asm = asm_at(SINKABLE_TAILS, Profile::Size);
    assert!(
        asm.contains("__TAIL_"),
        "PH305 no longer sinks any common tail at Profile::Size; the \
         SMC guard must be a refusal for SMC sites only, not a \
         disabling of the pass\n{asm}"
    );
}

#[test]
fn sinkable_tails_program_is_correct() {
    for profile in PROFILES {
        let bytes = prg_at(SINKABLE_TAILS, profile);
        let out = common::run_clean(&bytes)
            .unwrap_or_else(|| panic!("{profile:?}: reaches a clean exit"));
        assert_eq!(
            out, "A 1 \nB 2 \nA 2 \nB 4 \nA 3 \nB 6 \nC\n",
            "{profile:?}"
        );
    }
}
