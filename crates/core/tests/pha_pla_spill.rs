mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

/// Compile to assembly text at `profile`.
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

/// Count `PHA` / `TYA` / `PHA` triples left in the final stream.
fn spill_triples(asm: &str) -> usize {
    let ops: Vec<&str> = asm
        .lines()
        .map(|l| l.split(';').next().unwrap_or("").trim())
        .filter(|l| !l.is_empty())
        .collect();
    ops.windows(3)
        .filter(|w| w[0] == "PHA" && w[1] == "TYA" && w[2] == "PHA")
        .count()
}

fn ph209_sites(asm: &str) -> usize {
    asm.lines().filter(|l| l.contains("ph209:")).count()
}

const CMP_SRC: &str = "\
10 A$=\"BANANA\"
20 IF A$=\"BANANA\" THEN PRINT \"EQ\"
30 IF A$<\"CHERRY\" THEN PRINT \"LT\"
40 IF A$>\"APPLE\" THEN PRINT \"GT\"
50 END
";

const CAT_SRC: &str = "\
10 READ A$,B$
20 DATA AB,CD
30 C$=A$+\"CD\"
40 PRINT C$
50 D$=C$+B$
60 PRINT D$
70 E$=\"X\"+A$
80 PRINT E$
90 END
";

#[test]
fn ph209_fires_on_real_string_compare_codegen() {
    let got = asm(CMP_SRC);
    assert!(
        ph209_sites(&got) >= 3,
        "the three compares should all be hoisted, saw {}:\n{got}",
        ph209_sites(&got)
    );
    assert_eq!(
        spill_triples(&got),
        0,
        "no stack spill left in front of __STRCMP:\n{got}"
    );
}

#[test]
fn ph209_fires_on_real_string_concat_codegen() {
    let got = asm(CAT_SRC);
    assert!(
        ph209_sites(&got) >= 2,
        "concat operands hoisted, saw {}:\n{got}",
        ph209_sites(&got)
    );
    assert_eq!(spill_triples(&got), 0, "no stack spill left:\n{got}");
}

#[test]
fn ph209_fires_on_every_profile() {
    // Nothing about the rule is profile-gated: it is fewer bytes AND fewer cycles, so `speed`
    // must take it too.
    for profile in [Profile::Default, Profile::Size, Profile::Speed] {
        let got = asm_at(CMP_SRC, profile);
        assert!(
            ph209_sites(&got) >= 3,
            "{profile:?} hoists the spill, saw {}",
            ph209_sites(&got)
        );
    }
}

#[test]
fn string_compare_still_answers_correctly() {
    let src = "\
10 A$=\"BANANA\"
20 IF A$=\"BANANA\" THEN PRINT \"EQ\"
30 IF A$<\"CHERRY\" THEN PRINT \"LT\"
40 IF A$>\"APPLE\" THEN PRINT \"GT\"
50 IF A$<\"APPLE\" THEN PRINT \"BAD1\"
60 IF A$>\"CHERRY\" THEN PRINT \"BAD2\"
70 IF A$<>\"BANANA\" THEN PRINT \"BAD3\"
80 IF \"AB\"<\"ABC\" THEN PRINT \"PFX\"
90 PRINT \"DONE\"
100 END
";
    let prg = common::compile_source(src).expect("compiles");
    let out = common::run_clean(&prg).expect("runs to a clean exit");
    assert_eq!(
        out, "EQ\nLT\nGT\nPFX\nDONE\n",
        "BASIC V2 orders these by PETSCII, shorter-prefix first"
    );
}

#[test]
fn string_concat_still_builds_the_right_bytes() {
    let src = "\
10 READ A$,B$
20 DATA AB,CD
30 C$=A$+\"CD\"
40 PRINT C$
50 D$=C$+B$+A$
60 PRINT D$
70 PRINT LEN(D$)
80 END
";
    let prg = common::compile_source(src).expect("compiles");
    let out = common::run_clean(&prg).expect("runs to a clean exit");
    assert_eq!(out, "ABCD\nABCDCDAB\n 8 \n");
}

#[test]
fn a_longer_concat_chain_still_builds_the_right_bytes() {
    let src = "\
10 A$=\"AB\":B$=\"CD\"
20 C$=A$+\"CD\"
30 PRINT C$
40 D$=C$+B$+A$
50 PRINT D$
60 PRINT LEN(D$)
70 E$=B$+A$
80 PRINT E$
90 PRINT LEN(E$)
100 END
";
    let prg = common::compile_source(src).expect("compiles");
    let out = common::run_clean(&prg).expect("runs to a clean exit");
    assert_eq!(out, "ABCD\nABCDCDAB\n 8 \nCDAB\n 4 \n");
}

#[test]
fn compare_and_concat_interleaved_keep_their_operands_apart() {
    let src = "\
10 A$=\"ALPHA\":B$=\"BETA\"
20 C$=A$+B$
30 IF C$=\"ALPHABETA\" THEN PRINT \"CAT OK\"
40 IF C$>A$ THEN PRINT \"GT OK\"
50 D$=B$+A$
60 IF D$=\"BETAALPHA\" THEN PRINT \"REV OK\"
70 IF D$<>C$ THEN PRINT \"NE OK\"
80 PRINT C$;\"/\";D$
90 END
";
    let prg = common::compile_source(src).expect("compiles");
    let out = common::run_clean(&prg).expect("runs to a clean exit");
    assert_eq!(out, "CAT OK\nGT OK\nREV OK\nNE OK\nALPHABETA/BETAALPHA\n");
}

#[test]
fn hoisted_spill_survives_a_loop_that_reuses_the_pointers() {
    let src = "\
10 DIM W$(4)
20 W$(0)=\"AA\":W$(1)=\"AB\":W$(2)=\"BA\":W$(3)=\"BB\"
30 N=0
40 FOR I=0 TO 3
50 IF W$(I)<\"BA\" THEN N=N+1
60 T$=T$+W$(I)
70 NEXT I
80 PRINT N
90 PRINT T$
100 END
";
    let prg = common::compile_source(src).expect("compiles");
    let out = common::run_clean(&prg).expect("runs to a clean exit");
    assert_eq!(out, " 2 \nAAABBABB\n");
}

#[test]
fn the_rule_is_deterministic_across_repeated_runs() {
    let first = asm(CMP_SRC);
    for _ in 0..4 {
        assert_eq!(asm(CMP_SRC), first, "same input, same assembly");
    }
}
