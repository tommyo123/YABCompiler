

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

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
fn asm(src: &str) -> String {
    asm_at(src, Profile::Speed)
}

fn text_output(src: &str) -> String {
    let prg = common::compile_source(src).expect("compiles");
    common::run_clean(&prg).unwrap_or_else(|| panic!("reaches a clean exit:\n{src}"))
}
fn insns(a: &str) -> Vec<String> {
    a.lines()
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

fn count_seq(a: &str, seq: &[&str]) -> usize {
    let got = insns(a);
    got.windows(seq.len())
        .filter(|w| w.iter().zip(seq).all(|(x, y)| insn_matches(x, y)))
        .count()
}

#[track_caller]
fn assert_seq(a: &str, seq: &[&str]) {
    assert!(
        count_seq(a, seq) > 0,
        "expected consecutive\n  {}\nin\n{a}",
        seq.join("\n  ")
    );
}

#[track_caller]
fn assert_no(a: &str, needle: &str) {
    assert!(!a.contains(needle), "expected no {needle:?} in\n{a}");
}

/// A float comparison used as a *value* is the only thing that reaches `emit_compare`.
#[test]
fn fac_compare_materialises_the_boolean_without_a_jmp() {
    for op in ["<", "<=", "=", ">=", ">", "<>"] {
        let src = format!("10 a=peek(1024)/3:b=peek(1025)/7\n20 c=(a{op}b):print c\n");
        let a = asm(&src);
        assert_seq(&a, &["LDA #$FF", "BIT $61"]);
        assert_no(&a, "JMP __CMP_D");
        assert_no(&a, "LDA $61");
        assert_no(&a, "LDA $66");
    }
}
#[test]
fn fac_compare_ordering_arms_probe_exponent_then_sign() {
    let lt = asm("10 a=peek(1024)/3:b=peek(1025)/7\n20 c=(a<b):print c\n");
    assert_seq(&lt, &["LDA #$FF", "BIT $61", "BEQ", "BIT $66", "BMI"]);
    assert!(lt.contains("__CMP_F_"), "`<` needs a false arm:\n{lt}");

    let le = asm("10 a=peek(1024)/3:b=peek(1025)/7\n20 c=(a<=b):print c\n");
    assert_seq(&le, &["LDA #$FF", "BIT $61", "BEQ", "BIT $66", "BMI"]);
    assert_no(&le, "__CMP_F_");
}

/// 1 / 0 for every operator, against the ROM's own float compare.
const FAC_BOOL: &str = "\
10 for i=0 to 3
20 a=i-1.5:b=0.5*i-1
30 print (a<b);(a<=b);(a=b);(a>=b);(a>b);(a<>b)
40 next i
50 end
";

#[test]
fn fac_compare_booleans_match_the_rom() {
    assert_eq!(
        text_output(FAC_BOOL),
        "-1 -1  0  0  0 -1 \n 0 -1 -1 -1  0  0 \n 0  0  0 -1 -1 -1 \n 0  0  0 -1 -1 -1 \n"
    );
}
#[test]
fn int16_compare_fast_path_still_fires() {
    for op in ["<", "<=", "=", ">=", ">", "<>"] {
        let src = format!(
            "10 for k=1 to 2:read x%,y%\n\
             20 if x%{op}y% then print \"t\"\n\
             30 next k\n\
             40 data 1000,-31768,-5,-6\n"
        );
        let a = asm(&src);
        assert!(
            count_seq(&a, &["SEC", "LDA VI_X", "SBC VI_Y"])
                + count_seq(&a, &["SEC", "LDA VI_Y", "SBC VI_X"])
                > 0,
            "`{op}` left the inline SBC chain:\n{a}"
        );
        assert_no(&a, "__BOOL_TO_FAC");
    }
}

/// `>` and `<=` are emitted as `b - a`, and the whole `TAX / ORA / BEQ / TXA` tail is gone: one
/// branch decides.
#[test]
fn int16_gt_and_le_transpose_the_sbc_operands() {
    let gt = asm("10 for k=1 to 2:read x%,y%\n\
                  20 if x%>y% then print \"t\"\n\
                  30 next k\n\
                  40 data 1000,-31768,-5,-6\n");
    assert_seq(
        &gt,
        &[
            "SEC",
            "LDA VI_Y",
            "SBC VI_X",
            "LDA VI_Y+1",
            "SBC VI_X+1",
            "BVC",
            "EOR #$80",
            "BPL",
        ],
    );
    assert_no(&gt, "ORA __FOR_DIFF_LO");
    assert_no(&gt, "STA __FOR_DIFF_LO");
    assert_no(&gt, "__IF_KEEP");

    let le = asm("10 for k=1 to 2:read x%,y%\n\
                  20 if x%<=y% then print \"t\"\n\
                  30 next k\n\
                  40 data 1000,-31768,-5,-6\n");
    assert_seq(
        &le,
        &[
            "SEC",
            "LDA VI_Y",
            "SBC VI_X",
            "LDA VI_Y+1",
            "SBC VI_X+1",
            "BVC",
            "EOR #$80",
            "BMI",
        ],
    );
    assert_no(&le, "ORA __FOR_DIFF_LO");
    assert_no(&le, "__IF_KEEP");
}
#[test]
fn int16_lt_and_ge_are_a_single_branch() {
    for (op, want) in [("<", "BPL"), (">=", "BMI")] {
        let a = asm(&format!(
            "10 for k=1 to 2:read x%,y%\n\
             20 if x%{op}y% then print \"t\"\n\
             30 next k\n\
             40 data 1000,-31768,-5,-6\n"
        ));
        assert_seq(
            &a,
            &[
                "SEC",
                "LDA VI_X",
                "SBC VI_Y",
                "LDA VI_X+1",
                "SBC VI_Y+1",
                "BVC",
                "EOR #$80",
                want,
            ],
        );
        assert_no(&a, "STA __FOR_DIFF_LO");
        assert_no(&a, "ORA __FOR_DIFF_LO");
        assert_no(&a, "__FOR_DIFF_LO:");
    }
}
#[test]
fn int16_equality_skips_the_sign_fixup() {
    for (op, want) in [("=", "BNE"), ("<>", "BEQ")] {
        let a = asm(&format!(
            "10 for k=1 to 2:read x%,y%\n\
             20 if x%{op}y% then print \"t\"\n\
             30 next k\n\
             40 data 1000,-31768,-5,-6\n"
        ));
        assert_seq(
            &a,
            &[
                "SEC",
                "LDA VI_X",
                "SBC VI_Y",
                "STA __FOR_DIFF_LO",
                "LDA VI_X+1",
                "SBC VI_Y+1",
                "ORA __FOR_DIFF_LO",
                want,
            ],
        );
        assert_no(&a, "__IF_NV_");
        assert_no(&a, "EOR #$80");
    }
}
#[test]
fn signed_byte_storage_operand_is_not_transposed() {
    let a = asm("10 for i=0 to 1\n\
                 20 if i=0 then a%=-2\n\
                 30 if i=1 then a%=5\n\
                 40 if i=0 then b%=-300\n\
                 50 if i=1 then b%=3\n\
                 60 if a%>b% then print \"g\"\n\
                 70 next i\n");
    // `A%` really is the signed-byte case: its hi byte is the ladder.
    assert_seq(&a, &["LDA VI_A", "ASL", "LDA #$00", "BCC", "LDA #$FF"]);
    assert_seq(&a, &["SEC", "LDA VI_A", "SBC VI_B", "STA __FOR_DIFF_LO"]);
    assert_seq(&a, &["TAX", "ORA __FOR_DIFF_LO", "BEQ"]);
}
const EQ_OVERFLOW: &str = "\
10 for k=1 to 4
20 read x%,y%
30 s$=\"\"
40 if x%<y% then s$=s$+\"l\"
50 if x%<=y% then s$=s$+\"m\"
60 if x%=y% then s$=s$+\"e\"
70 if x%>=y% then s$=s$+\"n\"
80 if x%>y% then s$=s$+\"g\"
90 if x%<>y% then s$=s$+\"d\"
100 print s$
110 next k
120 data 0,-32768,-32768,0,1000,-31768,32767,-32768
130 end
";

#[test]
fn int16_equality_survives_a_32768_difference() {
    assert_eq!(text_output(EQ_OVERFLOW), "NGD\nLMD\nNGD\nNGD\n");
}

/// Ordinary orderings, both signs, either side of the 8-bit boundary.
const INT16_ORDERINGS: &str = "\
10 for k=1 to 5
20 read x%,y%
30 s$=\"\"
40 if x%<y% then s$=s$+\"l\"
50 if x%<=y% then s$=s$+\"m\"
60 if x%=y% then s$=s$+\"e\"
70 if x%>=y% then s$=s$+\"n\"
80 if x%>y% then s$=s$+\"g\"
90 if x%<>y% then s$=s$+\"d\"
100 print s$
110 next k
120 data -1,1,300,300,-5,-6,256,255,255,256
130 end
";

#[test]
fn int16_orderings_match_the_rom() {
    assert_eq!(text_output(INT16_ORDERINGS), "LMD\nMEN\nNGD\nNGD\nLMD\n");
}
#[test]
fn peek_vs_literal_gt_and_le_are_one_branch() {
    let gt = asm("10 for x=0 to 20\n20 if peek(1024+x)>32 then print \"a\"\n30 next x\n");
    assert_seq(&gt, &["LDA #$20", "LDX VI_X", "CMP $0400,X", "BCS"]);
    assert_no(&gt, "__IF_PEEK_KEEP");

    let le = asm("10 for x=0 to 20\n20 if peek(1024+x)<=32 then print \"a\"\n30 next x\n");
    assert_seq(&le, &["LDA #$20", "LDX VI_X", "CMP $0400,X", "BCC"]);
    assert_no(&le, "__IF_PEEK_KEEP");
}

/// PEEK against PEEK must *not* transpose: BASIC evaluates the left operand first and the read
/// itself can be observable (`$DC0D` clears the CIA latch).
#[test]
fn peek_vs_peek_keeps_its_read_order() {
    let a = asm("10 for x=0 to 20\n\
                 20 if peek(1024+x)>peek(1025+x) then print \"a\"\n\
                 30 next x\n");
    assert_seq(&a, &["LDA $0400,X", "CMP $0401,X", "BEQ", "BCC"]);
}
#[test]
fn u8_var_vs_literal_gt_and_le_are_one_branch() {
    let gt = asm("10 for x=0 to 20:c=peek(1024+x)\n20 if c>32 then print \"a\"\n30 next x\n");
    assert_seq(&gt, &["LDA #$20", "CMP", "BCS"]);
    assert_no(&gt, "__IF_U8V_KEEP");

    let le = asm("10 for x=0 to 20:c=peek(1024+x)\n20 if c<=32 then print \"a\"\n30 next x\n");
    assert_seq(&le, &["LDA #$20", "CMP", "BCC"]);
    assert_no(&le, "__IF_U8V_KEEP");
}

/// The u8-var-vs-u8-var path.
#[test]
fn u8_var_vs_var_gt_and_le_transpose() {
    let gt = asm("10 for x=0 to 20:c=peek(1024+x):d=peek(1025+x)\n\
                  20 if c>d then print \"a\"\n30 next x\n");
    assert_seq(&gt, &["LDA VI_D", "CMP VI_C", "BCS"]);
    assert_no(&gt, "__IF_U8VV_KEEP");

    let le = asm("10 for x=0 to 20:c=peek(1024+x):d=peek(1025+x)\n\
                  20 if c<=d then print \"a\"\n30 next x\n");
    assert_seq(&le, &["LDA VI_D", "CMP VI_C", "BCC"]);
    assert_no(&le, "__IF_U8VV_KEEP");
}

/// Behaviour for all three byte paths at once, across the whole u8 range and both sides of the
/// literal.
const BYTE_PATHS: &str = "\
10 for k=0 to 8:poke 49152+k,k*30:next k
20 for k=0 to 7
30 s$=\"\"
40 if peek(49152+k)>100 then s$=s$+\"g\"
50 if peek(49152+k)<=100 then s$=s$+\"m\"
60 if 100>peek(49152+k) then s$=s$+\"h\"
70 if 100<=peek(49152+k) then s$=s$+\"i\"
80 if peek(49152+k)>peek(49153+k) then s$=s$+\"p\"
90 print s$;\"/\";
100 next k
110 print
120 for k=0 to 7
130 c=peek(49152+k):d=peek(49153+k)
140 s$=\"\"
150 if c>d then s$=s$+\"g\"
160 if c<=d then s$=s$+\"m\"
170 if c<d then s$=s$+\"l\"
180 if c>=d then s$=s$+\"n\"
190 if c=d then s$=s$+\"e\"
200 if c<>d then s$=s$+\"d\"
210 if c>100 then s$=s$+\"x\"
220 if c<=100 then s$=s$+\"y\"
230 print s$;\"/\";
240 next k
250 print
260 end
";

#[test]
fn byte_compare_paths_match_the_rom() {
    assert_eq!(
        text_output(BYTE_PATHS),
        "MH/MH/MH/MH/GI/GI/GI/GI/\nMLDY/MLDY/MLDY/MLDY/MLDX/MLDX/MLDX/MLDX/\n"
    );
}

// Profile independence.

/// None of this is profile-gated: every rewrite is smaller *and* fewer instructions, so all
/// three profiles get it.
#[test]
fn every_profile_gets_the_new_encodings() {
    for profile in [Profile::Default, Profile::Size, Profile::Speed] {
        let a = asm_at(
            "10 for k=1 to 2:read x%,y%\n\
             20 if x%>y% then print \"t\"\n\
             30 next k\n\
             40 data 1000,-31768,-5,-6\n",
            profile,
        );
        assert_seq(&a, &["SEC", "LDA VI_Y", "SBC VI_X"]);

        let f = asm_at(
            "10 a=peek(1024)/3:b=peek(1025)/7\n20 c=(a<b):print c\n",
            profile,
        );
        assert_seq(&f, &["LDA #$FF", "BIT $61"]);
        assert_no(&f, "JMP __CMP_D");
    }
}
