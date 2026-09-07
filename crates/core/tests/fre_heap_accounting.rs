//! `FRE()` and the memory it is supposed to be able to see.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

fn build(src: &str, profile: Profile) -> (Vec<u8>, String) {
    let prg = tokenize_program(src).expect("tokenizes");
    let opts = CompileOptions {
        profile,
        ..CompileOptions::default()
    };
    let c = compile_with_options(&prg, opts)
        .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}\n{src}"));
    (c.prg_bytes, c.asm)
}

fn asm_of(src: &str) -> String {
    build(src, Profile::Default).1
}

/// Assert `src` prints `expected` and ends cleanly, on every profile.
fn check_all_profiles(src: &str, expected: &str) {
    for profile in PROFILES {
        let (bytes, _) = build(src, profile);
        let got = common::run_clean(&bytes)
            .unwrap_or_else(|| panic!("runs to a clean exit ({profile:?}):\n{src}"));
        assert_eq!(got, expected, "{profile:?} output mismatch for:\n{src}");
    }
}

/// Two runtime `DIM`s of 301 two-byte elements each.
const RUNTIME_DIM: &str = "\
10 read n
20 f0 = fre(0)
30 dim s$(n)
40 f1 = fre(0)
50 dim i%(n)
60 f2 = fre(0)
70 print \"d1:\"; f0 - f1
80 print \"d2:\"; f1 - f2
90 s$(n) = \"z\"
100 i%(n) = 7
110 print s$(n); i%(n)
120 data 300
";

/// 20 heap chunks of `[len][3 chars][owner lo][owner hi]` = 120 bytes.
const LIVE_STRINGS: &str = "\
10 dim a$(20)
20 read n
30 f0 = fre(0)
40 for i=1 to 20
50 a$(i) = chr$(n+i) + chr$(n+i) + chr$(n+i)
60 next i
70 print \"used:\"; f0 - fre(0)
80 print a$(1); a$(20)
90 data 65
";

/// Five live array strings, then fifty more allocations that overwrite them.
const GARBAGE_THEN_FRE: &str = "\
10 read n
20 dim a$(5)
30 for i=1 to 5
40 a$(i) = chr$(n+i)+chr$(n+i)+chr$(n+i)+chr$(n+i)+chr$(n+i)
50 next i
60 f1 = fre(0)
70 for j=1 to 10
80 for i=1 to 5
90 a$(i) = chr$(n+i)+chr$(n+i)+chr$(n+i)+chr$(n+i)+chr$(n+i)
100 next i
110 next j
120 print \"gc leak:\"; f1 - fre(0)
130 print a$(1); a$(5)
140 data 65
";

/// ~25.7 KB of string garbage, then `FRE(0)` as a collection trigger, then a 14002-byte runtime
/// `DIM` that only fits if the collection stuck.
const POOR_MANS_GC: &str = "\
10 read n, m
20 dim g$(3)
30 b$ = \"\"
40 for i=1 to 20
50 b$ = b$ + \"0123456789\"
60 next i
70 for j=1 to 42
80 for i=1 to 3
90 g$(i) = b$ + chr$(n)
100 next i
110 next j
120 x = fre(0)
130 dim a%(m)
140 a%(m) = 1234
150 print \"dimmed ok:\"; a%(m)
160 print len(g$(1)); len(b$)
170 data 65, 7000
";
#[test]
fn fre_falls_by_the_size_of_each_runtime_dim() {
    check_all_profiles(RUNTIME_DIM, "D1: 602 \nD2: 602 \nZ 7 \n");
}

#[test]
fn fre_measures_against_the_array_ceiling_when_a_runtime_dim_exists() {
    let asm = asm_of(RUNTIME_DIM);
    let fre = fre_block(&asm);
    assert!(
        fre.contains("LDA __ARR_HEAP_TOP") && fre.contains("SBC __HEAP_PTR"),
        "FRE must subtract from the runtime-array ceiling:\n{fre}"
    );
    assert!(
        !fre.contains("LDA $37"),
        "FRE must not read MEMSIZ once runtime DIM owns the ceiling:\n{fre}"
    );
}
#[test]
fn fre_still_uses_memsiz_when_there_is_no_runtime_dim() {
    let asm = asm_of(LIVE_STRINGS);
    let fre = fre_block(&asm);
    assert!(
        fre.contains("LDA $37") && fre.contains("LDA $38"),
        "FRE should still read MEMSIZ directly here:\n{fre}"
    );
    assert!(
        !asm.contains("__ARR_HEAP_TOP"),
        "no runtime DIM in this program — the array ceiling must not \
         be materialised at all"
    );
}
fn fre_block(asm: &str) -> String {
    let start = asm
        .find("    JSR __GC_RUN\n    SEC\n")
        .unwrap_or_else(|| panic!("no FRE sequence in:\n{asm}"));
    let tail = &asm[start..];
    let end = tail.find("JSR $B391").expect("FRE ends in GIVAYF") + "JSR $B391".len();
    tail[..end].to_string()
}
#[test]
fn fre_forces_a_collection() {
    check_all_profiles(GARBAGE_THEN_FRE, "GC LEAK: 0 \nBBBBBFFFFF\n");
}
#[test]
fn a_collection_forced_by_fre_survives_the_statement() {
    check_all_profiles(POOR_MANS_GC, "DIMMED OK: 1234 \n 201  200 \n");
}
#[test]
fn fre_tracks_live_string_heap_growth() {
    check_all_profiles(LIVE_STRINGS, "USED: 120 \nBBBUUU\n");
}

const UNGUARDED_RESTORE: &str = "__HEAP_END_NONLET:\n    \
     LDA __HEAP_SAVE\n    STA __HEAP_PTR\n    \
     LDA __HEAP_SAVE+1\n    STA __HEAP_PTR+1\n    RTS";
#[test]
fn heap_end_nonlet_keeps_the_unguarded_restore_when_nothing_calls_fre() {
    let src = "\
10 read n
20 dim a$(3)
30 for i=1 to 3
40 a$(i) = chr$(n+i) + chr$(n+i)
50 next i
60 print a$(1) + a$(3); len(a$(1) + a$(2))
70 data 65
";
    let asm = asm_of(src);
    assert!(
        asm.contains("__GC_RUN:"),
        "this program is only interesting if it does pull in the \
         collector:\n{asm}"
    );
    assert_eq!(
        end_nonlet_block(&asm),
        UNGUARDED_RESTORE,
        "a collector without FRE must not pay for the clamp"
    );
    check_all_profiles(src, "BBDD 4 \n");
}
#[test]
fn heap_end_nonlet_keeps_the_unguarded_restore_without_a_collector() {
    let src = "\
10 read n
20 a$ = chr$(n)
30 print a$ + a$; len(a$ + a$)
40 data 65
";
    let asm = asm_of(src);
    assert!(
        !asm.contains("__GC_RUN"),
        "this program should not pull in the collector:\n{asm}"
    );
    assert_eq!(
        end_nonlet_block(&asm),
        UNGUARDED_RESTORE,
        "collector-free programs must not pay for the clamp"
    );
}

#[test]
fn heap_end_nonlet_clamps_when_a_collector_exists() {
    let asm = asm_of(GARBAGE_THEN_FRE);
    let restore = end_nonlet_block(&asm);
    assert!(
        restore.contains("CMP __HEAP_PTR") && restore.contains("BCS __HEN_KEEP"),
        "with a collector in the program the restore must be clamped:\n{restore}"
    );
}

/// `__HEAP_END_NONLET:` through its `RTS`.
fn end_nonlet_block(asm: &str) -> String {
    let start = asm
        .find("__HEAP_END_NONLET:\n")
        .unwrap_or_else(|| panic!("no __HEAP_END_NONLET in:\n{asm}"));
    let tail = &asm[start..];
    let end = tail.find("    RTS").expect("ends in RTS") + "    RTS".len();
    tail[..end].to_string()
}

/// `hag_pre`'s shape: `A$` is built from operands the compiler knows statically, and `B$ = A$`
/// aliases it, which keeps both off the BSS fast path and on the heap.
const FOLDABLE_INTO_HEAP_VAR: &str = "\
10 c$ = \"world\"
20 f1 = fre(0)
30 for i=1 to 20
40 a$ = \"hello\" + c$
50 b$ = a$
60 next i
70 print \"live:\"; f1 - fre(0)
80 print len(b$); b$
";
const FOLDABLE_INTO_HEAP_VAR_NO_FRE: &str = "\
10 c$ = \"world\"
30 for i=1 to 20
40 a$ = \"hello\" + c$
50 b$ = a$
60 next i
80 print len(b$); b$
";

/// `live_cat`'s shape.
const FOLDABLE_INTO_ARRAY: &str = "\
10 dim z$(3)
20 f1 = fre(0)
30 z$(1) = \"x\" + \"y\"
40 print \"used:\"; f1 - fre(0)
50 print z$(1)
";

const FOLDABLE_INTO_ARRAY_NO_FRE: &str = "\
10 dim z$(3)
30 z$(1) = \"x\" + \"y\"
50 print z$(1)
";
const INTERLEAVED_GARBAGE: &str = "\
10 dim g$(2)
20 x$ = \"0123456789\"
30 f1 = fre(0)
40 for j=1 to 20
50 for i=1 to 2
60 g$(i) = x$ + \"abcde\"
70 next i
80 next j
90 print \"leak:\"; f1 - fre(0)
100 print len(g$(1)); len(g$(2))
";

/// `heap_temp` in miniature: the concatenation is discarded by `LEN`, so nothing holds the
/// chunk and the collection `FRE` runs reclaims it.
const FOLDED_TEMPORARY: &str = "\
10 f1 = fre(0)
20 for i=1 to 50
30 a = len(\"x\" + \"y\" + \"z\")
40 next i
50 print \"temp:\"; f1 - fre(0)
60 print a
";

#[test]
fn fre_sees_a_constant_concat_bound_to_a_heap_variable() {
    check_all_profiles(FOLDABLE_INTO_HEAP_VAR, "LIVE: 26 \n 10 HELLOWORLD\n");
}

#[test]
fn fre_sees_a_constant_concat_bound_to_an_array_element() {
    check_all_profiles(FOLDABLE_INTO_ARRAY, "USED: 5 \nXY\n");
}

/// The collection is what keeps the other 38 chunks from counting.
#[test]
fn fre_collects_interleaved_array_garbage() {
    check_all_profiles(INTERLEAVED_GARBAGE, "LEAK: 36 \n 15  15 \n");
}

/// The other half of the rule: a chunk nothing holds is invisible, so the fold stays.
#[test]
fn fre_does_not_see_folded_temporaries() {
    check_all_profiles(FOLDED_TEMPORARY, "TEMP: 0 \n 3 \n");
}
#[test]
fn constant_string_concat_still_folds_when_nothing_calls_fre() {
    for src in [FOLDABLE_INTO_HEAP_VAR_NO_FRE, FOLDABLE_INTO_ARRAY_NO_FRE] {
        let asm = asm_of(src);
        assert!(
            !asm.contains("__STR_CONCAT"),
            "no FRE in this program — the concat must still fold away:\n{asm}"
        );
        assert!(
            !asm.contains("__GC_RUN"),
            "a fully folded program must not link the collector:\n{asm}"
        );
    }
    check_all_profiles(FOLDABLE_INTO_HEAP_VAR_NO_FRE, " 10 HELLOWORLD\n");
    check_all_profiles(FOLDABLE_INTO_ARRAY_NO_FRE, "XY\n");
}
#[test]
fn a_program_that_calls_fre_keeps_its_constant_concat() {
    for src in [FOLDABLE_INTO_HEAP_VAR, FOLDABLE_INTO_ARRAY] {
        let asm = asm_of(src);
        assert!(
            asm.contains("__STR_CONCAT"),
            "FRE makes the allocation observable — the concat must \
             survive:\n{asm}"
        );
    }
}
#[test]
fn a_conditional_fre_still_arms_the_gate() {
    let src = "\
10 c$ = \"world\"
20 a$ = \"hello\" + c$
30 print len(a$)
40 if a$ = \"zzz\" then print fre(0)
";
    let asm = asm_of(src);
    assert!(
        asm.contains("__STR_CONCAT"),
        "a FRE that may run makes the heap observable:\n{asm}"
    );
    check_all_profiles(src, " 10 \n");
}
#[test]
fn an_unreachable_fre_does_not_arm_the_gate() {
    let src = "\
10 c$ = \"world\"
20 a$ = \"hello\" + c$
30 print len(a$)
40 end
50 print fre(0)
";
    let asm = asm_of(src);
    assert!(
        !asm.contains("__STR_CONCAT"),
        "the FRE is unreachable and gone; the concat must still \
         fold:\n{asm}"
    );
    check_all_profiles(src, " 10 \n");
}
#[test]
fn self_assignment_costs_nothing_after_the_binding_allocates() {
    check_all_profiles(
        "\
10 a$=\"ab\"+\"cd\"
20 f1=fre(0):a$=a$:f2=fre(0)
30 print a$;f1-f2
",
        "ABCD 0 \n",
    );
}
#[test]
fn every_fre_site_collects_before_it_reports() {
    let src = "\
10 dim g$(2)
20 x$ = \"abc\"
30 g$(1) = x$ + \"d\"
40 print fre(0); fre(0)
50 print g$(1)
";
    let asm = asm_of(src);
    let sites = asm.matches("    JSR __GC_RUN\n    SEC\n").count();
    assert_eq!(
        sites, 2,
        "two FRE calls must collect twice, not once:\n{asm}"
    );
}
#[test]
fn the_corrected_heap_codegen_is_deterministic() {
    for src in [
        RUNTIME_DIM,
        LIVE_STRINGS,
        GARBAGE_THEN_FRE,
        POOR_MANS_GC,
        FOLDABLE_INTO_HEAP_VAR,
        FOLDABLE_INTO_ARRAY,
        INTERLEAVED_GARBAGE,
        FOLDED_TEMPORARY,
    ] {
        for profile in PROFILES {
            let (a, asm_a) = build(src, profile);
            let (b, asm_b) = build(src, profile);
            assert_eq!(a, b, "{profile:?} image is not reproducible");
            assert_eq!(asm_a, asm_b, "{profile:?} asm is not reproducible");
        }
    }
}
