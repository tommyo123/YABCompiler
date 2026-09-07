//! `IF <byte> <op> <byte>` where at least one side is a `PEEK`.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn asm_of(src: &str) -> String {
    asm_at(src, Profile::Default)
}

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

fn text_output(src: &str) -> String {
    let prg = common::compile_source(src).expect("compiles");
    common::run_clean(&prg).unwrap_or_else(|| panic!("reaches a clean exit:\n{src}"))
}
fn insns(asm: &str) -> Vec<String> {
    asm.lines()
        .map(str::trim)
        .filter(|l| {
            !l.is_empty()
                && !l.starts_with(';')
                && !l.starts_with('*')
                && !l.starts_with('.')
                && !l.ends_with(':')
                && !l.contains('=')
        })
        .map(str::to_string)
        .collect()
}
fn insn_matches(actual: &str, want: &str) -> bool {
    let mut a = actual.split_whitespace();
    let mut w = want.split_whitespace();
    loop {
        match (a.next(), w.next()) {
            (_, None) => return true,
            (None, Some(_)) => return false,
            (Some(x), Some(y)) if x != y => return false,
            _ => {}
        }
    }
}

/// True when `seq` appears as consecutive instructions in `asm`.
fn has_seq(asm: &str, seq: &[&str]) -> bool {
    let got = insns(asm);
    got.windows(seq.len())
        .any(|w| w.iter().zip(seq).all(|(a, b)| insn_matches(a, b)))
}

fn assert_seq(asm: &str, seq: &[&str]) {
    assert!(
        has_seq(asm, seq),
        "expected consecutive\n  {}\nin\n{asm}",
        seq.join("\n  ")
    );
}

fn assert_no(asm: &str, needle: &str) {
    assert!(!asm.contains(needle), "expected no {needle:?} in\n{asm}");
}
#[test]
fn literal_address_vs_literal_byte_still_reaches_the_byte_compare() {
    let asm = asm_of("10 if peek(197)=13 then print \"a\"\n");
    assert_seq(&asm, &["LDA $00C5", "CMP #$0D", "BNE"]);
    assert_no(&asm, "__PEEK_INT");

    // Literal on the left: the operand order flips and `>=` becomes `<=`.
    let asm = asm_of("10 if 13>=peek(197) then print \"a\"\n");
    assert_seq(&asm, &["LDA #$0D", "CMP $00C5", "BCC"]);
    assert_no(&asm, "__PEEK_INT");
    assert_no(&asm, "__IF_PEEK_KEEP");
}
#[test]
fn let_form_indexed_peek_still_reaches_abs_x() {
    let asm = asm_of("10 w=1024\n20 for x=0 to 20:c=peek(w+x):next\n");
    assert!(
        asm.contains("LDA $0400,X") || asm.contains("LDY $0400,X"),
        "LET form lost abs,X:\n{asm}"
    );
    assert_no(&asm, "__PEEK_INT");
    assert_no(&asm, "LDA $FFFF");
    let asm = asm_of(
        "5 w=1024\n\
         10 for x=0 to 238:poke w+x,238-x:next x\n\
         20 for i=1 to 10\n\
         30 for x=0 to 238\n\
         40 c=peek(w+x):d=peek(w+x+1)\n\
         50 if c<=d then 70\n\
         60 poke w+x,d:poke w+x+1,c\n\
         70 next x\n\
         80 next i\n\
         90 end\n",
    );
    assert!(
        asm.contains("$0400,X") && asm.contains("$0401,X"),
        "bubble lost abs,X:\n{asm}"
    );
    assert_no(&asm, "__PEEK_INT");
}

const ABSX_VS_LITERAL: &str = "\
10 w=49152
20 for x=0 to 20:poke w+x,x:next
30 n=0
40 for x=0 to 20
50 if peek(w+x)>=10 then n=n+1
60 next
70 print \"n=\";n
";

#[test]
fn indexed_peek_vs_literal_reaches_abs_x_compare() {
    let asm = asm_of(ABSX_VS_LITERAL);
    assert_seq(&asm, &["LDX VI_X", "LDA $C000,X", "CMP #$0A", "BCC"]);
    assert_no(&asm, "__PEEK_INT");
    assert_no(&asm, "LDA $FFFF");
}

#[test]
fn indexed_peek_vs_literal_matches_the_rom() {
    // Native: cells $C000..$C014 hold 0..20, eleven of them are >= 10.
    assert_eq!(text_output(ABSX_VS_LITERAL), "N= 11 \n");
}

/// The counter side of an indexed PEEK has to come from the slot that is *live* at that point.
const STALE_COUNTER: &str = "\
10 w=49152
20 for x=0 to 15:poke w+x,x*2:next
30 n=0
40 for x=0 to 15
50 if peek(w+x)=10 then n=n+1
60 next
70 print \"n=\";n
80 for x=0 to 15
90 if peek(w+x)>=20 then print x;
100 next
110 print:print \"done\"
";

#[test]
fn indexed_peek_reads_the_live_counter_slot_not_stale_v_var() {
    let asm = asm_of(STALE_COUNTER);
    // Both loops index off the counter slot, never off `V_X`.
    assert_seq(&asm, &["LDX VI_X", "LDA $C000,X", "CMP #$0A", "BNE"]);
    assert_seq(&asm, &["LDX VI_X", "LDA $C000,X", "CMP #$14", "BCC"]);
    assert_no(&asm, "__PEEK_INT");
}

#[test]
fn indexed_peek_live_counter_matches_the_rom() {
    assert_eq!(
        text_output(STALE_COUNTER),
        "N= 1 \n 10  11  12  13  14  15 \nDONE\n"
    );
}

/// Two literal addresses: plain `LDA abs / CMP abs`.
#[test]
fn two_literal_peeks_reach_the_byte_compare() {
    let asm = asm_of("10 if peek(53280)<peek(53281) then print \"a\"\n");
    assert_seq(&asm, &["LDA $D020", "CMP $D021", "BCS"]);
    assert_no(&asm, "__PEEK_INT");
}
const TWO_PEEKS_SAME_SLOT: &str = "\
10 w=49152
20 for x=0 to 20:poke w+x,20-x:next
30 s=0
40 for p=1 to 20
50 for x=0 to 19
60 if peek(w+x)<=peek(w+x+1) then 100
70 c=peek(w+x):d=peek(w+x+1)
80 poke w+x,d:poke w+x+1,c
90 s=s+1
100 next x
110 next p
120 for x=0 to 20:print peek(w+x);:next
130 print:print \"s=\";s
";

#[test]
fn two_indexed_peeks_on_one_slot_share_a_single_ldx() {
    let asm = asm_of(TWO_PEEKS_SAME_SLOT);
    assert_seq(&asm, &["LDX VI_X", "LDA $C000,X", "CMP $C001,X"]);
    assert!(
        !has_seq(
            &asm,
            &["LDX VI_X", "LDA $C000,X", "LDX VI_X", "CMP $C001,X"]
        ),
        "same-slot pair must not reload .X:\n{asm}"
    );
    assert_no(&asm, "__PEEK_INT");
}

#[test]
fn two_indexed_peeks_on_one_slot_sort_like_the_rom() {
    assert_eq!(
        text_output(TWO_PEEKS_SAME_SLOT),
        " 0  1  2  3  4  5  6  7  8  9  10  11  12  13  14  15  16  17  18  19  20 \nS= 210 \n"
    );
}

/// Two indexed PEEKs whose indices resolve to *different* live slots.
const TWO_PEEKS_TWO_SLOTS: &str = "\
10 w=49152
20 for x=0 to 20:poke w+x,(x*7) and 255:next
30 n=0
40 for i=0 to 20
50 j=20-i
60 if peek(w+i)<peek(w+j) then n=n+1
70 next
80 print \"n=\";n
";

#[test]
fn two_indexed_peeks_on_two_slots_reload_x() {
    let asm = asm_of(TWO_PEEKS_TWO_SLOTS);
    assert_seq(
        &asm,
        &["LDX VI_I", "LDA $C000,X", "LDX VI_J", "CMP $C000,X", "BCS"],
    );
    assert_no(&asm, "__PEEK_INT");
}

#[test]
fn two_indexed_peeks_on_two_slots_match_the_rom() {
    assert_eq!(text_output(TWO_PEEKS_TWO_SLOTS), "N= 10 \n");
}

/// Indexed PEEKs that cross a page boundary.
const PAGE_CROSS: &str = "\
10 w=49352
20 for x=0 to 255:poke w+x,x and 1:next
30 n=0
40 for x=0 to 254
50 if peek(w+x)<=peek(w+x+1) then n=n+1
60 next
70 print \"n=\";n
";

#[test]
fn indexed_peek_pair_crosses_a_page() {
    let asm = asm_of(PAGE_CROSS);
    assert_seq(&asm, &["LDX VI_X", "LDA $C0C8,X", "CMP $C0C9,X"]);
    assert_no(&asm, "__PEEK_INT");
}

#[test]
fn indexed_peek_pair_across_a_page_matches_the_rom() {
    // Alternating 0,1: `<=` holds for the 128 even x in 0..=254.
    assert_eq!(text_output(PAGE_CROSS), "N= 128 \n");
}

/// The keyboard-poll idiom with the wanted key in a variable.
#[test]
fn peek_vs_byte_var_reaches_the_byte_compare() {
    let asm = asm_of("10 k=peek(198)\n20 if peek(197)=k then print \"a\"\n");
    assert_seq(&asm, &["LDA $00C5", "CMP VI_K", "BNE"]);
    assert_no(&asm, "__PEEK_INT");
}

/// The variable may sit on either side, and the index var of an indexed PEEK may itself be the
/// compare's other operand.
#[test]
fn indexed_peek_vs_the_index_var_itself() {
    let asm = asm_of("10 w=49152\n20 for x=0 to 255\n30 if peek(w+x)=x then print x\n40 next\n");
    assert_seq(&asm, &["LDX VI_X", "LDA $C000,X", "CMP VI_X", "BNE"]);
    assert_no(&asm, "__PEEK_INT");
}

const PEEK_VS_VAR: &str = "\
10 poke 49152,77:poke 49153,77:poke 49154,78
20 v=peek(49153)
30 if peek(49152)=v then print \"a\";
40 if v=peek(49152) then print \"b\";
50 v=peek(49154)
60 if peek(49152)<v then print \"c\";
70 if v>peek(49152) then print \"d\";
80 print \"e\"
";

#[test]
fn peek_vs_byte_var_both_orientations_match_the_rom() {
    assert_eq!(text_output(PEEK_VS_VAR), "ABCDE\n");
}

const IDX_VAR: &str = "\
10 w=49152
20 for x=0 to 255:poke w+x,x:next
30 n=0
40 for x=0 to 255
50 if peek(w+x)=x then n=n+1
60 next
70 print \"n=\";n
";

#[test]
fn indexed_peek_vs_index_var_matches_the_rom() {
    assert_eq!(text_output(IDX_VAR), "N= 256 \n");
}

// Soundness: shapes the byte compare must REFUSE.

const WIDE_VAR: &str = "\
10 poke 49152,32
20 v=peek(49152)+256
30 if peek(49152)=v then print \"wrong1\"
40 w=peek(49152)-256
50 if peek(49152)=w then print \"wrong2\"
60 if peek(49152)<v then print \"lt-ok\"
70 if peek(49152)>w then print \"gt-ok\"
80 print \"done\"
";

#[test]
fn peek_vs_wide_var_does_not_take_the_byte_compare() {
    let asm = asm_of(WIDE_VAR);
    // Neither `V` (=288) nor `W` (=-224) may be read as a single byte next to the PEEK: the
    // high byte carries the answer.
    assert!(
        !has_seq(&asm, &["LDA $C000", "CMP VI_V"]) && !has_seq(&asm, &["LDA $C000", "CMP VI_W"]),
        "a wide var must not be compared as a byte:\n{asm}"
    );
}

#[test]
fn peek_vs_wide_var_matches_the_rom() {
    // 32 is neither 288 nor -224; it is less than one and more than the other.
    assert_eq!(text_output(WIDE_VAR), "LT-OK\nGT-OK\nDONE\n");
}
#[test]
fn zero_page_base_does_not_reach_the_indexed_compare() {
    let src = "10 poke 828,32\n20 x=peek(828)\n30 if peek(240+x)=77 then print \"hit\"\n";
    let asm = asm_of(src);
    assert_no(&asm, "LDA $00F0,X");
    assert_no(&asm, "CMP $00F0,X");
    let asm = asm_of("10 x=peek(828)\n20 if peek(x)=77 then print \"hit\"\n");
    assert_seq(&asm, &["LDX VI_X", "LDA $0000,X", "CMP #$4D", "BNE"]);
}

// Composition with the surrounding IF machinery.

const AND_OR: &str = "\
10 poke 49152,10:poke 49153,20
20 if peek(49152)=10 and peek(49153)=20 then print \"and-ok\"
30 if peek(49152)=99 or peek(49153)=20 then print \"or-ok\"
40 w=49152
50 for x=0 to 1
60 if peek(w+x)>5 and peek(w+x)<25 then print \"r\";x
70 next
80 print \"done\"
";

#[test]
fn and_or_chains_lower_each_leaf_to_a_byte_compare() {
    let asm = asm_of(AND_OR);
    assert_seq(&asm, &["LDA $C000", "CMP #$0A", "BNE"]);
    assert_seq(&asm, &["LDA $C001", "CMP #$14", "BNE"]);
    assert_no(&asm, "__PEEK_INT");
}
#[test]
fn and_of_indexed_peeks_still_does_not_short_circuit() {
    let asm = asm_of(
        "10 w=49152\n20 for x=0 to 1\n30 if peek(w+x)>5 and peek(w+x)<25 then print x\n40 next\n",
    );
    assert!(
        !has_seq(&asm, &["LDX VI_X", "LDA $C000,X", "CMP #$05"]),
        "indexed AND unexpectedly short-circuits — good news, but the \
         comment above and the scope notes need updating:\n{asm}"
    );
}

#[test]
fn and_or_chains_match_the_rom() {
    assert_eq!(text_output(AND_OR), "AND-OK\nOR-OK\nR 0 \nR 1 \nDONE\n");
}

const ALL_OPS: &str = "\
10 poke 49152,10:poke 49153,20:poke 49154,10
20 if peek(49152)=peek(49154) then print \"eq\";
30 if peek(49152)<>peek(49153) then print \"ne\";
40 if peek(49152)<peek(49153) then print \"lt\";
50 if peek(49152)<=peek(49154) then print \"le\";
60 if peek(49153)>peek(49152) then print \"gt\";
70 if peek(49152)>=peek(49154) then print \"ge\";
80 print
90 if peek(49153)=peek(49152) then print \"xeq\";
100 if peek(49152)<>peek(49154) then print \"xne\";
110 if peek(49153)<peek(49152) then print \"xlt\";
120 if peek(49153)<=peek(49152) then print \"xle\";
130 if peek(49152)>peek(49153) then print \"xgt\";
140 if peek(49152)>=peek(49153) then print \"xge\";
150 print \"end\"
";

#[test]
fn every_operator_keeps_its_polarity_against_the_rom() {
    // Each of the six operators once true, then once false.
    assert_eq!(text_output(ALL_OPS), "EQNELTLEGTGE\nEND\n");
    assert_no(&asm_of(ALL_OPS), "__PEEK_INT");
}

const FLIPPED: &str = "\
10 poke 49152,32
20 if 32<=peek(49152) then print \"a\";
30 if 33>peek(49152) then print \"b\";
40 if 32=peek(49152) then print \"c\";
50 if 31<peek(49152) then print \"d\";
60 print \"e\"
";

#[test]
fn literal_on_the_left_keeps_its_polarity_against_the_rom() {
    assert_eq!(text_output(FLIPPED), "ABCDE\n");
}

#[test]
fn the_byte_compare_fires_at_every_profile() {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let asm = asm_at(ABSX_VS_LITERAL, profile);
        assert!(
            has_seq(&asm, &["LDX VI_X", "LDA $C000,X", "CMP #$0A"]),
            "{profile:?} lost the indexed byte compare:\n{asm}"
        );
    }
}
