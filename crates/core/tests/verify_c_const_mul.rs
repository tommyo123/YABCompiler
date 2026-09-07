mod common;

use yabcompiler_core::{CompileOptions, compile_with_options, tokenize_program};

const MUL16_LABEL: &str = "__MUL16:";

fn asm_of(src: &str) -> String {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(&prg, CompileOptions::default())
        .expect("compiles")
        .asm
}

/// A program whose only multiply is `X% * k`.
fn probe_src(k: i64) -> String {
    format!("10 FOR R%=0 TO 24\n20 A%=R%*{k}\n30 POKE 1024+A%,81\n40 NEXT R%\n50 END\n")
}

/// The set of K the compiler currently lowers inline, read off the emitter.
fn admitted_set(hi: i64) -> Vec<i64> {
    (3..=hi)
        .filter(|k| !asm_of(&probe_src(*k)).contains(MUL16_LABEL))
        .collect()
}

const SCAN_HI: i64 = 1024;

#[test]
fn admitted_set_is_what_the_analysis_says() {
    let got = admitted_set(SCAN_HI);
    // Powers of two are the pot2 path, not this one, but they also do not reach __MUL16 ; so they appear in the scan.
    let (pot2, non_pot2): (Vec<i64>, Vec<i64>) =
        got.iter().partition(|k| (**k as u64).is_power_of_two());
    println!("VERIFY-C admitted non-pot2 K (scan 3..={SCAN_HI}): {non_pot2:?}");
    println!("VERIFY-C pot2 K also inline: {pot2:?}");
    println!("VERIFY-C count non-pot2 = {}", non_pot2.len());

    assert!(
        non_pot2.iter().all(|k| *k <= 512),
        "an admitted constant above 512 means SCAN_HI is not a ceiling: {non_pot2:?}"
    );
    // Non-vacuity: the headline constant must be in there.
    assert!(non_pot2.contains(&40), "40 must be admitted");
    assert!(!non_pot2.contains(&100), "100 must not be admitted");
}

fn exhaustive_src(k: i64) -> String {
    format!(
        "10 POKE 2,{k}\n\
         20 K%=PEEK(2)\n\
         30 FOR I=-128 TO 127\n\
         40 X%=I\n\
         50 A%=X%*{k}\n\
         60 B%=X%*K%\n\
         70 IF A%<>B% THEN E%=1 : F%=X%\n\
         80 NEXT I\n\
         90 PRINT \"R\";E%;F%\n\
         99 END\n"
    )
}

fn exhaustive_check(k: i64) {
    let src = exhaustive_src(k);
    let asm = asm_of(&src);

    assert!(
        asm.contains(MUL16_LABEL),
        "K={k}: the variable-multiplier line must pull in {MUL16_LABEL}"
    );

    assert!(
        !asm_of(&probe_src(k)).contains(MUL16_LABEL),
        "K={k}: the literal lowering is not inline, nothing to verify"
    );

    assert!(
        asm.contains("__SMUL_TMP"),
        "K={k}: the literal half must be a shift chain, not FAC"
    );
    assert!(
        !asm.contains("__AYINT"),
        "K={k}: a half left the int island for FAC — nothing is being compared"
    );

    let prg = common::compile_source(&src).expect("compiles");
    let r = emu64::run_prg_to_end(&prg, 3_000_000_000).expect("runs");
    assert_eq!(r.error_banner(), None, "K={k}: runtime error");
    assert!(r.clean_exit, "K={k}: did not reach END");
    let out = common::petscii_to_string(&r.output);
    let out = out.trim_end();
    assert_eq!(
        out, "R 0  0",
        "K={k}: inline chain disagrees with __MUL16 (E%,F% = {out:?})"
    );
    println!("VERIFY-C K={k}: 65536/65536 inputs agree ({} insns)", r.instructions);
}

/// The whole admitted set, exhaustively.
#[test]
#[ignore = "exhaustive: run with --ignored --nocapture"]
fn every_admitted_constant_agrees_with_mul16_over_all_65536_inputs() {
    let set: Vec<i64> = admitted_set(SCAN_HI)
        .into_iter()
        .filter(|k| !(*k as u64).is_power_of_two())
        .collect();
    assert!(set.len() > 30, "only {} admitted, scan is wrong", set.len());
    for k in set {
        exhaustive_check(k);
    }
}

#[test]
fn headline_constants_agree_over_all_65536_inputs() {
    for k in [40i64, 17, 63, 13] {
        exhaustive_check(k);
    }
}

/// Two constant multiplies in one expression share the single global scratch slot `__SMUL_TMP`.
fn composite_src(expr_lit: &str, expr_var: &str, k: i64, m: i64) -> String {
    format!(
        "10 POKE 2,{k}\n\
         12 K%=PEEK(2)\n\
         14 POKE 2,{m}\n\
         16 M%=PEEK(2)\n\
         30 FOR I=-64 TO 63\n\
         35 X%=I\n\
         40 A%={expr_lit}\n\
         50 B%={expr_var}\n\
         60 IF A%<>B% THEN E%=1 : F%=X%\n\
         80 NEXT I\n\
         90 PRINT \"R\";E%;F%\n\
         99 END\n"
    )
}

fn composite_check(name: &str, lit: &str, var: &str, k: i64, m: i64) {
    let src = composite_src(lit, var, k, m);
    let asm = asm_of(&src);
    assert!(
        asm.contains("__MUL16:"),
        "{name}: the oracle half must use the helper"
    );
    assert!(
        asm.contains("__SMUL_TMP"),
        "{name}: the literal half must use a shift chain, or this tests nothing"
    );
    assert!(
        !asm.contains("__AYINT"),
        "{name}: a half left the int island for FAC — nothing is being compared"
    );
    let prg = common::compile_source(&src).expect("compiles");
    let r = emu64::run_prg_to_end(&prg, 3_000_000_000).expect("runs");
    assert_eq!(r.error_banner(), None, "{name}: runtime error");
    assert!(r.clean_exit, "{name}: did not reach END");
    let out = common::petscii_to_string(&r.output);
    assert_eq!(
        out.trim_end(),
        "R 0  0",
        "{name}: composite shift chains disagree with __MUL16"
    );
    println!("VERIFY-C {name}: 65536/65536 inputs agree");
}

#[test]
fn two_chains_in_one_expression_do_not_clobber_each_other() {
    // Two Sum2Pow chains, both live across one Add.
    composite_check("X*40+X*24", "X%*40+X%*24", "X%*K%+X%*M%", 40, 24);
    // A shift chain beside a power-of-two chain.
    composite_check("X*40+X*64", "X%*40+X%*64", "X%*K%+X%*M%", 40, 64);
    // Three-term plan beside a two-term one.
    composite_check("X*13+X*80", "X%*13+X%*80", "X%*K%+X%*M%", 13, 80);
    // Subtraction, so a borrow chain sits between the two saves.
    composite_check("X*96-X*17", "X%*96-X%*17", "X%*K%-X%*M%", 96, 17);
    // Operand is an add/sub tree, not a leaf.
    composite_check("(X+7)*40", "(X%+7)*40", "(X%+7)*K%", 40, 1);
    composite_check("(X-9)*255", "(X%-9)*255", "(X%-9)*K%", 255, 1);
}

#[test]
fn the_exhaustive_harness_can_fail() {
    // Literal 40 against a variable 41: must differ for most inputs.
    let src = exhaustive_src(40).replace("10 POKE 2,40", "10 POKE 2,41");
    let prg = common::compile_source(&src).expect("compiles");
    let r = emu64::run_prg_to_end(&prg, 3_000_000_000).expect("runs");
    assert!(r.clean_exit);
    let out = common::petscii_to_string(&r.output);
    assert!(
        !out.trim_end().starts_with("R 0 "),
        "harness cannot detect a wrong product — every exhaustive pass is vacuous: {out:?}"
    );
    println!("VERIFY-C mutation control: harness reports {:?}", out.trim_end());
}
