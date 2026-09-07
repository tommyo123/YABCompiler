//! The compiler must be a function: same source, same flags, same bytes.

use std::path::PathBuf;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

/// Compiles per program.
const REPEATS: usize = 6;

fn repo_path(sub: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(sub)
}

fn opts(profile: Profile) -> CompileOptions {
    CompileOptions {
        profile,
        ..CompileOptions::default()
    }
}

/// Tokenized bytes for a `test_synthetic` ASCII source.
fn synthetic(name: &str) -> Vec<u8> {
    let path = repo_path("test_synthetic").join(name);
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {name}: {e}"));
    tokenize_program(&src).unwrap_or_else(|e| panic!("tokenize {name}: {e:?}"))
}

fn corpus(name: &str) -> Vec<u8> {
    let path = repo_path("test_corpus").join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {name}: {e}"))
}

/// Compile `prg` `REPEATS` times and return the asm of the first run.
fn stable_asm(label: &str, prg: &[u8], profile: Profile) -> String {
    let first = compile_with_options(prg, opts(profile))
        .unwrap_or_else(|e| panic!("{label}: compile failed: {e}"));
    assert!(
        first.prg_bytes.len() > 2,
        "{label}: produced no code — the determinism check would be vacuous"
    );

    for run in 1..REPEATS {
        let again = compile_with_options(prg, opts(profile))
            .unwrap_or_else(|e| panic!("{label}: compile {run} failed: {e}"));
        if again.prg_bytes == first.prg_bytes && again.asm == first.asm {
            continue;
        }
        let where_ = first
            .asm
            .lines()
            .zip(again.asm.lines())
            .enumerate()
            .find(|(_, (a, b))| a != b)
            .map(|(i, (a, b))| format!("asm line {}: {a:?} vs {b:?}", i + 1))
            .unwrap_or_else(|| "asm identical, prg bytes differ".to_string());
        panic!(
            "{label}: compile {run} differs from compile 0 \
             (len {} vs {}) — {where_}",
            again.prg_bytes.len(),
            first.prg_bytes.len()
        );
    }
    first.asm
}

// the programs

const REPRESENTATIVE_PROFILES: &[&str] = &["23_conversions.bas", "40_string_array.bas"];

const REPRESENTATIVE_SYNTHETIC: &[&str] = &[
    "23_conversions.bas",
    "35_def_fn_complex.bas",
    "40_string_array.bas",
    "48_complex_print.bas",
    "89_orphan_next.bas",
    "13_on_goto.bas",
    "17_bubble_sort.bas",
    "31_var_types.bas",
    "57_tsbneo_proc.bas",
];

const REPRESENTATIVE_CORPUS: &[&str] = &[
    "strings.prg",
    "str_funcs.prg",
    "on_branch.prg",
    "arrays.prg",
    "def_fn.prg",
    "structured_proc.prg",
    "structured_proc_with_for.prg",
    "tsb_low_hanging.prg",
];

#[test]
fn synthetic_programs_compile_to_identical_bytes() {
    for name in REPRESENTATIVE_SYNTHETIC {
        stable_asm(name, &synthetic(name), Profile::Default);
    }
}

#[test]
fn corpus_programs_compile_to_identical_bytes() {
    for name in REPRESENTATIVE_CORPUS {
        stable_asm(name, &corpus(name), Profile::Default);
    }
}

/// Everything in both in-repo suites, twice.
#[test]
fn whole_in_repo_corpus_is_stable() {
    let mut checked = 0usize;
    for (dir, ext) in [("test_synthetic", "bas"), ("test_corpus", "prg")] {
        let mut paths: Vec<PathBuf> = std::fs::read_dir(repo_path(dir))
            .unwrap_or_else(|e| panic!("read {dir}: {e}"))
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some(ext))
            .collect();
        paths.sort();
        for path in paths {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let prg = if ext == "bas" {
                let src = std::fs::read_to_string(&path).expect("read .bas");
                match tokenize_program(&src) {
                    Ok(p) => p,
                    Err(_) => continue, // Skip sources the tokenizer rejects.
                }
            } else {
                std::fs::read(&path).expect("read .prg")
            };
            // Negative tests (`reject_*`) legitimately fail to compile; a program that produces
            // nothing has nothing to compare.
            let Ok(first) = compile_with_options(&prg, opts(Profile::Default)) else {
                continue;
            };
            if first.prg_bytes.len() <= 2 {
                continue;
            }
            let again = compile_with_options(&prg, opts(Profile::Default))
                .unwrap_or_else(|e| panic!("{name}: second compile failed: {e}"));
            assert_eq!(
                again.prg_bytes.len(),
                first.prg_bytes.len(),
                "{name}: two compiles produced different-length output"
            );
            assert!(
                again.prg_bytes == first.prg_bytes && again.asm == first.asm,
                "{name}: two compiles of the same program produced different bytes"
            );
            checked += 1;
        }
    }
    assert!(
        checked > 200,
        "expected 200+ compilable programs, found {checked} — the sweep is not covering the corpora"
    );
}

/// `speed` and `size` disable/lower different peephole families, so they reach emit sites the
/// default profile does not.
#[test]
fn non_default_profiles_compile_to_identical_bytes() {
    for profile in [Profile::Speed, Profile::Size] {
        for name in REPRESENTATIVE_PROFILES {
            stable_asm(&format!("{name} @ {profile:?}"), &synthetic(name), profile);
        }
        stable_asm(
            &format!("strings.prg @ {profile:?}"),
            &corpus("strings.prg"),
            profile,
        );
    }
}

const THREE_INDUCTIONS: &str = "\
10 dim a$(50),b$(50),c$(50)
20 for j=1 to 40
30 a$(j)=a$(j)+\"x\"
40 b$(j)=b$(j)+\"y\"
50 c$(j)=c$(j)+\"z\"
60 a$(j)=a$(j)+b$(j)
70 b$(j)=b$(j)+c$(j)
80 c$(j)=c$(j)+a$(j)
90 next j
100 print a$(3);b$(3);c$(3)
";

#[test]
fn three_array_pointer_inductions() {
    let prg = tokenize_program(THREE_INDUCTIONS).expect("tokenize");
    let asm = stable_asm("three-inductions", &prg, Profile::Default);
    for slot in ["AP_0", "AP_1", "AP_2"] {
        assert!(
            asm.contains(slot),
            "array-pointer induction stopped firing: no {slot} in output"
        );
    }
}

const CONST_OP_STUBS: &str = "\
10 dim a(20)
20 for i=0 to 20:a(i)=i/3:next i
30 for i=0 to 20
40 p=a(i)*1.5
50 q=a(i)+2.5
60 r=2.5+(p*q)
70 s=1.5*(p+q)
80 print p;q;r;s
90 next i
";

#[test]
fn float_const_op_stubs_still_emitted() {
    let prg = tokenize_program(CONST_OP_STUBS).expect("tokenize");
    let asm = stable_asm("const-op-stubs", &prg, Profile::Default);
    let stubs: Vec<&str> = asm
        .lines()
        .filter(|l| {
            l.ends_with(':')
                && ["__FADD_F", "__FSUB_F", "__FMUL_F", "__FDIV_F"]
                    .iter()
                    .any(|p| l.starts_with(p))
        })
        .collect();
    assert!(
        stubs.len() >= 2,
        "expected 2+ float const-op stubs, found {}: {stubs:?}",
        stubs.len()
    );
    let mut uniq = stubs.clone();
    uniq.sort_unstable();
    uniq.dedup();
    assert_eq!(
        uniq.len(),
        stubs.len(),
        "duplicate float const-op stub labels: {stubs:?}"
    );
}

/// Three BSS-promoted string variables.
const THREE_BSS_STRINGS: &str = "\
10 poke 56,15:clr
20 a$=\"12345\":b$=\"67890\"
30 for i=1 to 60:c$=a$+b$:a$=right$(c$,5):b$=left$(c$,5):next
40 print a$;\"/\";b$
";

#[test]
fn bss_string_buffers_still_emitted_in_label_order() {
    let prg = tokenize_program(THREE_BSS_STRINGS).expect("tokenize");
    let asm = stable_asm("three-bss-strings", &prg, Profile::Default);

    let in_label_order = |what: &str, got: Vec<String>| {
        assert!(
            got.len() >= 3,
            "{what}: BSS string promotion stopped firing, got {got:?}"
        );
        let mut want = got.clone();
        want.sort();
        assert_eq!(got, want, "{what} must be emitted in label order");
    };

    in_label_order(
        "__SBUF_ buffers",
        asm.lines()
            .filter_map(|l| l.strip_prefix("__SBUF_VS_")?.strip_suffix(':'))
            .map(str::to_string)
            .collect(),
    );
    in_label_order(
        "the BSS zeroing run",
        asm.lines()
            .filter_map(|l| l.trim().strip_prefix("STA __SBUF_VS_"))
            .map(str::to_string)
            .collect(),
    );
    // The `V_` slots themselves: pre-interned from the same map, so their position in the
    // variable block followed hash order too.
    in_label_order(
        "VS_ variable slots",
        asm.lines()
            .filter_map(|l| l.strip_prefix("VS_")?.strip_suffix(':'))
            .map(str::to_string)
            .collect(),
    );
}

#[test]
fn runtime_for_handlers_still_emitted_in_id_order() {
    let asm = stable_asm(
        "89_orphan_next.bas",
        &synthetic("89_orphan_next.bas"),
        Profile::Default,
    );
    let ids: Vec<u32> = asm
        .lines()
        .filter_map(|l| {
            l.strip_prefix("__FOR_NEXT_HANDLER_")?
                .strip_suffix(':')?
                .parse()
                .ok()
        })
        .collect();
    assert!(
        !ids.is_empty(),
        "runtime FOR-stack lowering stopped firing — no handlers emitted"
    );
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    assert_eq!(ids, sorted, "handler table must be emitted in id order");
}

#[test]
fn computed_branch_dispatch_still_compiles() {
    let asm = stable_asm(
        "13_on_goto.bas",
        &synthetic("13_on_goto.bas"),
        Profile::Default,
    );
    assert!(
        asm.contains("JMP") || asm.contains("JSR"),
        "ON..GOTO lowering produced no transfer of control"
    );
}

/// `cross_reached_nexts` decides whether a bare `NEXT` reached from outside can keep its inline
/// tail, and to do it it asks which loop is *innermost* at the jump site.
#[test]
fn innermost_loop_selection_is_order_independent() {
    let src = "10 FOR I=0 TO 3\n\
               20 FOR J=1 TO 4\n\
               30 FOR K=0 TO 3\n\
               40 C=C+1:PRINT C;\n\
               50 ON (C AND 3)+1 GOTO 60,70,80,90\n\
               60 NEXT\n\
               70 NEXT\n\
               80 NEXT\n\
               90 PRINT\"e\";I;J;K\n";
    let prg = tokenize_program(src).expect("tokenizes");
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let asm = stable_asm("three_deep_bare_nexts", &prg, profile);
        assert!(
            asm.contains("__FOR_STACK"),
            "{profile:?}: the dispatching lowering must still be the one under test:\n{asm}"
        );
    }
}
