mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn asm_at(src: &str, profile: Profile) -> String {
    let prg = tokenize_program(src).expect("tokenizes");
    let opts = CompileOptions {
        profile,
        ..CompileOptions::default()
    };
    compile_with_options(&prg, opts).expect("compiles").asm
}

fn asm(src: &str) -> String {
    asm_at(src, Profile::Default)
}

/// Instruction mnemonics in order, comments and blanks stripped.
fn ops(asm: &str) -> Vec<String> {
    asm.lines()
        .filter(|l| l.starts_with("    "))
        .map(|l| l.split(';').next().unwrap_or("").trim().to_string())
        .filter(|l| !l.is_empty() && !l.starts_with('.'))
        .collect()
}

fn count_exact(asm: &str, mnem: &str) -> usize {
    ops(asm).iter().filter(|o| o.as_str() == mnem).count()
}

fn count_starting(asm: &str, prefix: &str) -> usize {
    ops(asm).iter().filter(|o| o.starts_with(prefix)).count()
}

/// Run to a clean exit and return the printed text.
fn run(src: &str) -> String {
    let prg = common::compile_source(src).expect("compiles");
    let r = emu64::run_prg_to_end(&prg, 80_000_000).expect("runs");
    assert!(r.clean_exit, "must reach a clean exit");
    common::petscii_to_string(&r.output)
}
const STR_SRC: &str = "\
10 A$=\"HELLO\"
20 B$=A$
30 C$=B$+\"!\"
40 PRINT C$
50 D$=C$
60 PRINT D$
70 END
";

#[test]
fn ph211_fires_on_real_string_assignment() {
    let before = count_exact(&asm(STR_SRC), "TYA");
    assert!(
        before < 4,
        "most string-pointer transfers should be folded away, {before} left"
    );
    assert!(
        count_starting(&asm(STR_SRC), "STY ") > 0,
        "and the folds must show up as STY"
    );
}

#[test]
fn ph211_keeps_string_assignment_correct() {
    assert_eq!(
        run(STR_SRC).split_whitespace().collect::<Vec<_>>(),
        ["HELLO!", "HELLO!"]
    );
}
#[test]
fn ph211_leaves_no_foldable_pair_that_liveness_allows() {
    // A broad program touching strings, arrays, floats and loops.
    let src = "\
10 DIM A(10),B$(5)
20 FOR I=0 TO 5:B$(I)=CHR$(65+I):NEXT I
30 S$=\"\"
40 FOR I=0 TO 5:S$=S$+B$(I):NEXT I
50 PRINT S$
60 FOR I=0 TO 10:A(I)=I*2:NEXT I
70 T=0:FOR I=0 TO 10:T=T+A(I):NEXT I
80 PRINT T
90 END
";
    let out = run(src);
    let words: Vec<&str> = out.split_whitespace().collect();
    assert_eq!(words, ["ABCDEF", "110"], "semantics unchanged: {out:?}");
    // The rule fired somewhere in this program.
    assert!(count_starting(&asm(src), "STY ") > 0);
}

/// A statically-DIMmed integer array indexed by literals is what builds `element * 2` byte
/// offsets at compile time.
const INT_ARR_SRC: &str = "\
10 DIM A%(20)
20 A%(1)=111:A%(2)=222:A%(3)=333:A%(4)=444
30 A%(5)=555:A%(6)=666:A%(7)=777:A%(8)=888
40 PRINT A%(1)+A%(2)+A%(3)+A%(4)
50 PRINT A%(5)+A%(6)+A%(7)+A%(8)
60 END
";
fn doubling_triples(asm: &str) -> usize {
    ops(asm)
        .windows(3)
        .filter(|w| {
            w[0].starts_with("LDA #")
                && (w[1] == "ASL" || w[1] == "ASL A")
                && (w[2] == "TAX" || w[2] == "TAY")
        })
        .count()
}

#[test]
fn ph212_folds_the_literal_index_doubling() {
    let a = asm(INT_ARR_SRC);
    // Eight literal element writes, each of which starts life as `LDA #k / ASL / TAX`.
    for element in 1..=7u8 {
        let doubled = element * 2;
        assert!(
            a.contains(&format!("LDX #${doubled:02X}")),
            "A%({element}) must index through `LDX #${doubled:02X}`:\n{a}"
        );
    }
    assert_eq!(
        doubling_triples(&a),
        1,
        "exactly the carry-blocked site may remain:\n{a}"
    );
}

#[test]
fn ph212_keeps_integer_array_arithmetic_correct() {
    let out = run(INT_ARR_SRC);
    let words: Vec<&str> = out.split_whitespace().collect();
    assert_eq!(words, ["1110", "2886"], "got {out:?}");
}
const STEP_SRC: &str = "\
10 PRINT \"AB\";
20 PRINT \"CDE\";
30 PRINT \"FG\";
40 PRINT \"HIJK\"
50 A$=MID$(\"ABCDEFGH\",2,3)
60 PRINT A$
70 PRINT LEFT$(\"ABCDEFGH\",4)
80 PRINT RIGHT$(\"ABCDEFGH\",3)
90 END
";

#[test]
fn ph213_replaces_neighbouring_immediate_loads_with_bumps() {
    let a = asm(STEP_SRC);
    let bumps = count_exact(&a, "INX")
        + count_exact(&a, "DEX")
        + count_exact(&a, "INY")
        + count_exact(&a, "DEY");
    assert!(bumps > 0, "the rule must fire somewhere:\n{a}");
}

#[test]
fn ph213_keeps_string_functions_correct() {
    let out = run(STEP_SRC);
    let words: Vec<&str> = out.split_whitespace().collect();
    assert_eq!(words, ["ABCDEFGHIJK", "BCD", "ABCD", "FGH"], "got {out:?}");
}

/// `WAIT` compiles to a self-modifying poll: codegen writes the user's mask into the `AND`
/// instruction's immediate byte at run time.
#[test]
fn ph214_never_touches_the_self_modified_wait_mask() {
    let src = "\
10 POKE 828,255
20 WAIT 828,16
30 PRINT \"OK\"
40 END
";
    let a = asm(src);
    assert!(
        a.contains("AND #$01"),
        "the patched placeholder mask must be intact:\n{a}"
    );
    assert!(
        a.lines().any(|l| l.trim().starts_with("STA __WAIT_AND_")),
        "the runtime mask patch must be intact:\n{a}"
    );
    let o = ops(&a);
    assert!(
        !o.windows(2)
            .any(|w| w[0] == "LSR" && (w[1].starts_with("BCC") || w[1].starts_with("BCS"))),
        "no LSR/BCC pair may appear:\n{a}"
    );
}
#[test]
fn the_factoring_family_still_produces_helpers() {
    let src = "\
10 PRINT \"ALPHA\":PRINT \"BETA\":PRINT \"GAMMA\"
20 PRINT \"DELTA\":PRINT \"EPSILON\":PRINT \"ZETA\"
30 A=1:B=2:C=3:D=4:E=5:F=6
40 PRINT A+B:PRINT C+D:PRINT E+F
50 PRINT A*B:PRINT C*D:PRINT E*F
60 END
";
    let a = asm(src);
    let helpers = a
        .lines()
        .filter(|l| {
            ["__SEQ_", "__JSR2_", "__TAIL_", "__PH326_"]
                .iter()
                .any(|p| l.starts_with(p))
        })
        .count();
    assert!(helpers > 0, "the factoring family must still fire:\n{a}");
}
#[test]
fn ph019_and_ph020_fast_paths_still_fire() {
    let src = "\
10 DIM A(5)
20 FOR I=0 TO 5:A(I)=I:NEXT I
30 PRINT A(3)
40 END
";
    let a = asm(src);
    let o = ops(&a);
    let pairs = o
        .windows(2)
        .filter(|w| {
            w[0].starts_with("LDA ")
                && !w[0].starts_with("LDA #")
                && !w[0].contains('(')
                && !w[0].ends_with(",X")
                && (w[1] == "TAX")
        })
        .count();
    assert_eq!(pairs, 0, "PH019 must still fold LDA/TAX:\n{a}");
    assert_eq!(run(src).trim(), "3");
}

// profile and determinism

/// Every profile must stay self-consistent and reproducible; the rules run on all three.
#[test]
fn all_profiles_are_deterministic_and_correct() {
    let src = "\
10 A$=\"XY\":B$=A$+\"Z\"
20 DIM A%(8):A%(1)=7:A%(2)=9
30 PRINT B$;A%(1)+A%(2)
40 END
";
    for profile in [Profile::Default, Profile::Size, Profile::Speed] {
        let first = asm_at(src, profile);
        let second = asm_at(src, profile);
        assert_eq!(first, second, "{profile:?} must be reproducible");
    }
    assert_eq!(run(src).trim(), "XYZ 16");
}
