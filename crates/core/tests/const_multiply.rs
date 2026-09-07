//! Multiplication by a compile-time integer constant.

mod common;

use yabcompiler_core::{CompileOptions, compile_with_options, tokenize_program};

fn asm_of(src: &str) -> String {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(&prg, CompileOptions::default())
        .expect("compiles")
        .asm
}

fn run(src: &str) -> emu64::RunToEnd {
    let prg = common::compile_source(src).expect("compiles");
    emu64::run_prg_to_end(&prg, 40_000_000).expect("runs")
}

/// Decoded CHROUT transcript, trailing whitespace trimmed.
fn text(r: &emu64::RunToEnd) -> String {
    common::petscii_to_string(&r.output).trim_end().to_string()
}

/// The generic 16-round helper.
const MUL16_LABEL: &str = "__MUL16:";

/// A program whose only multiply is `X% * k`, so the presence of the `__MUL16` helper is exactly "this constant went through the loop".
fn one_mul_src(k: i32) -> String {
    format!("10 FOR R%=0 TO 24\n20 A%=R%*{k}\n30 POKE 1024+A%,81\n40 NEXT R%\n50 END\n")
}

#[test]
fn admitted_constants_do_not_reach_the_helper() {
    for k in [3, 5, 6, 10, 15, 17, 18, 20, 24, 33, 36, 40, 48, 63, 80, 96] {
        let asm = asm_of(&one_mul_src(k));
        assert!(
            !asm.contains(MUL16_LABEL),
            "K={k} fits the inline budget but still emits {MUL16_LABEL}"
        );
        assert!(
            asm.contains("ASL"),
            "K={k} took neither the helper nor a shift chain"
        );
    }
}

#[test]
fn refused_constants_still_reach_the_helper() {
    // 21/50/100/200: a plan exists, but 28 < cost. 30/60/1000: popcount 4 or more, no plan exists at all.
    for k in [21, 30, 50, 60, 100, 200, 1000] {
        let asm = asm_of(&one_mul_src(k));
        assert!(
            asm.contains(MUL16_LABEL),
            "K={k} is past the inline budget and must keep using {MUL16_LABEL}"
        );
    }
}

#[test]
fn either_operand_may_be_the_constant() {
    let lhs = asm_of("10 FOR R%=0 TO 9\n20 A%=40*R%\n30 POKE 1024+A%,81\n40 NEXT R%\n50 END\n");
    assert!(!lhs.contains(MUL16_LABEL), "40*R% must lower inline too");
    let pot2 = asm_of(&one_mul_src(64));
    assert!(!pot2.contains(MUL16_LABEL), "64 is a pure shift chain");
}

/// Both operands constant is the constant folder's job, and it must still be the constant folder's job ; no multiply of any kind.
#[test]
fn two_constants_fold_and_reach_neither_route() {
    let asm = asm_of("10 A%=17*40\n20 PRINT A%\n30 END\n");
    assert!(!asm.contains(MUL16_LABEL), "17*40 must be folded, not lowered");
    assert_eq!(text(&run("10 A%=17*40\n20 PRINT A%\n30 END\n")), " 680");
}

fn differential(k: i32) {
    // |X%| <= 128 and K% <= 255 keeps both products inside the window for every admitted K in this file (all <= 96).
    let konst = format!(
        "10 POKE 2,{k}\n20 K%=PEEK(2)\n30 FOR I=-128 TO 127\n40 X%=I\n\
         50 A%=X%*{k}\n60 PRINT A%;\n70 NEXT I\n80 PRINT\n90 END\n"
    );
    let varia = format!(
        "10 POKE 2,{k}\n20 K%=PEEK(2)\n30 FOR I=-128 TO 127\n40 X%=I\n\
         50 A%=X%*K%\n60 PRINT A%;\n70 NEXT I\n80 PRINT\n90 END\n"
    );
    let n = 256usize;

    let kasm = asm_of(&konst);
    let vasm = asm_of(&varia);
    assert!(
        !kasm.contains(MUL16_LABEL),
        "K={k}: the literal-multiplier program must take the shift chain"
    );
    assert!(
        kasm.contains("__SMUL_TMP"),
        "K={k}: the literal-multiplier program must take the shift chain \
         POSITIVELY — 'no __MUL16' is also true of the FAC path"
    );
    assert!(
        vasm.contains(MUL16_LABEL),
        "K={k}: the variable-multiplier program must take the helper — \
         without that the differential compares a route against itself"
    );
    for (name, asm) in [("literal", &kasm), ("variable", &vasm)] {
        assert!(
            !asm.contains("__AYINT"),
            "K={k}: the {name} program left the int island for FAC, so this \
             differential no longer compares the two lowerings"
        );
    }

    let kr = run(&konst);
    let vr = run(&varia);
    assert_eq!(kr.error_banner(), None, "K={k}: shift form raised an error");
    assert_eq!(vr.error_banner(), None, "K={k}: helper form raised an error");
    assert!(kr.clean_exit && vr.clean_exit, "K={k}: both must reach END");
    assert_eq!(
        text(&kr),
        text(&vr),
        "K={k}: the inline shift chain and __MUL16 disagree"
    );

    assert!(
        text(&kr).len() > 3 * n,
        "K={k}: transcript too short to be the {n} products: {:?}",
        text(&kr)
    );
}

#[test]
fn shift_chain_matches_mul16_for_40() {
    differential(40);
}

#[test]
fn shift_chain_matches_mul16_across_the_plan_shapes() {
    // Sum2Pow, ShiftAddSelf, ShiftSubSelf, Sum3Pow, and a wide two-term constant.
    for k in [20, 17, 63, 19, 96] {
        differential(k);
    }
}

#[test]
fn negative_operands_are_exact() {
    let src = "10 X%=-13\n20 A%=X%*40 : PRINT A%;\n30 A%=X%*20 : PRINT A%;\n\
               40 A%=X%*17 : PRINT A%;\n50 A%=-40*X% : PRINT A%\n60 END\n";
    let asm = asm_of(src);
    assert!(!asm.contains(MUL16_LABEL), "must be the shift chain, not the helper");
    let r = run(src);
    assert_eq!(r.error_banner(), None);
    assert!(r.clean_exit);
    assert_eq!(text(&r), "-520 -260 -221  520");
}

/// Overflow specifically: `1000*40 = 40000` does not fit i16.
#[test]
fn overflow_raises_where_the_rom_raises() {
    let src = "10 X%=1000\n20 A%=X%*40 : PRINT A%;\n30 X%=-1000\n40 A%=X%*40 : PRINT A%\n50 END\n";
    let r = run(src);
    assert_eq!(
        r.error_banner().as_deref(),
        Some("?ILLEGAL QUANTITY  ERROR IN 20"),
        "a `%` product outside AYINT's window must raise, not wrap: {:?}",
        text(&r)
    );

    let float_route = "10 X%=1000\n20 PRINT X%*40;\n30 X%=-1000\n40 PRINT X%*40\n50 END\n";
    let f = run(float_route);
    assert_eq!(f.error_banner(), None);
    assert_eq!(text(&f), " 40000 -40000");
}

/// End to end on the idiom the whole transform is for: a screen address computed as `base + row*40 + col`.
#[test]
fn screen_address_idiom_is_still_correct() {
    let src = "10 FOR R%=0 TO 4\n20 FOR C%=0 TO 3\n30 A%=1024+R%*40+C%\n\
               40 POKE A%,65+R%\n50 NEXT C%\n60 NEXT R%\n70 END\n";
    let asm = asm_of(src);
    assert!(!asm.contains(MUL16_LABEL), "the idiom must not use the helper");
    let r = run(src);
    assert!(r.clean_exit);
    for row in 0..5usize {
        for col in 0..4usize {
            // POKE writes the byte verbatim, so the cell holds `65 + row` and not the screen code for that letter.
            assert_eq!(
                r.screen[row * 40 + col],
                65 + row as u8,
                "screen cell ({row},{col})"
            );
        }

        assert_ne!(r.screen[row * 40 + 4], 65 + row as u8, "row {row} overran");
    }
}
