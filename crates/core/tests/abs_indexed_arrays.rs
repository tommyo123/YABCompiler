mod common;

use yabcompiler_core::{CompileOptions, compile_with_options, tokenize_program};

fn asm(src: &str) -> String {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(&prg, CompileOptions::default())
        .expect("compiles")
        .asm
}

/// Run to a clean exit and return the printed text.
fn run(src: &str) -> String {
    let prg = common::compile_source(src).expect("compiles");
    let r = emu64::run_prg_to_end(&prg, 80_000_000).expect("runs");
    assert!(r.clean_exit, "must reach a clean exit");
    common::petscii_to_string(&r.output)
}

fn run_err(src: &str) -> (Option<u8>, Option<u16>, String) {
    let prg = common::compile_source(src).expect("compiles");
    let r = emu64::run_prg_to_end(&prg, 80_000_000).expect("runs");
    (
        r.error_code,
        r.error_line,
        common::petscii_to_string(&r.output),
    )
}

const ERR_BAD_SUBSCRIPT: u8 = 18;

fn count(asm: &str, needle: &str) -> usize {
    asm.lines().filter(|l| l.trim() == needle).count()
}

fn builds_cheap_address(asm: &str, label: &str) -> bool {
    let ops: Vec<&str> = asm.lines().map(|l| l.trim()).collect();
    ops.windows(2)
        .any(|w| w[0] == format!("LDA #>{label}") && w[1] == "ADC #$00")
}

/// `A$(J)` in expression context.
const STR_READ_SRC: &str = "\
10 DIM A$(9)
20 FOR I=0 TO 9:READ A$(I):NEXT
30 FOR J=0 TO 9:PRINT A$(J);:NEXT
40 PRINT
50 END
60 DATA A,B,C,D,E,F,G,H,I,J
";

#[test]
fn str_array_read_takes_absx() {
    let a = asm(STR_READ_SRC);
    assert!(
        count(&a, "LDY __ARR_A_S+1,X") > 0 && count(&a, "LDA __ARR_A_S,X") > 0,
        "string-array reads must load the descriptor pointer via abs,X\n{a}"
    );
}

#[test]
fn str_array_read_still_correct() {
    assert_eq!(run(STR_READ_SRC), "ABCDEFGHIJ\n");
}

/// `A$(I) = "literal"` stores the pool pointer straight into the slot.
const STR_LIT_WRITE_SRC: &str = "\
10 DIM A$(4)
20 FOR I=0 TO 4:A$(I)=\"-\":NEXT
30 A$(2)=\"X\"
40 FOR I=0 TO 4:PRINT A$(I);:NEXT
50 PRINT
60 END
";

#[test]
fn str_array_literal_write_takes_absx() {
    let a = asm(STR_LIT_WRITE_SRC);
    assert!(
        count(&a, "STA __ARR_A_S,X") > 0 && count(&a, "STA __ARR_A_S+1,X") > 0,
        "literal string-array writes must store via abs,X\n{a}"
    );
    assert_eq!(
        count(&a, "STA __ARRAY_LET_STR_LO"),
        0,
        "the staging pair should be unused when every write is abs,X\n{a}"
    );
}

#[test]
fn str_array_literal_write_still_correct() {
    assert_eq!(run(STR_LIT_WRITE_SRC), "--X--\n");
}

/// `READ A$(I)` writes the DATA pool pointer into the slot.
#[test]
fn str_array_read_stmt_takes_absx() {
    let a = asm(STR_READ_SRC);
    assert!(
        count(&a, "STA __ARR_A_S,X") > 0,
        "READ into a string array must store via abs,X\n{a}"
    );
}

const INT_SRC: &str = "\
10 DIM A%(20)
20 FOR I=0 TO 20:A%(I)=I*2:NEXT
30 S=0
40 FOR J=0 TO 20:S=S+A%(J):NEXT
50 PRINT S
60 END
";

#[test]
fn int_array_still_takes_absx() {
    let a = asm(INT_SRC);
    assert!(
        count(&a, "LDY __ARR_A_I,X") > 0 && count(&a, "LDA __ARR_A_I+1,X") > 0,
        "integer-array reads must still take the abs,X path\n{a}"
    );
    assert!(
        count(&a, "STA __ARR_A_I,X") > 0,
        "integer-array writes must still take the abs,X path\n{a}"
    );
}

#[test]
fn int_array_still_correct() {
    assert_eq!(run(INT_SRC), " 420 \n");
}

const FLOAT_SRC: &str = "\
10 DIM A(50)
20 POKE 679,17:Q=PEEK(679):P=INT(Q)
30 A(P)=A(P)+7.5
40 PRINT A(17)
50 END
";

#[test]
fn float_array_takes_cheap_static_address() {
    let a = asm(FLOAT_SRC);
    assert!(
        a.lines().any(|l| l.trim_end() == "__ARR_A:"),
        "the array must still be 5-byte float storage\n{a}"
    );
    assert!(
        builds_cheap_address(&a, "__ARR_A"),
        "float-array access must build the address as label + 5*i\n{a}"
    );
    // The LINNUM round trip scales in place through ARRAY_ADDR.
    assert_eq!(
        count(&a, "ASL $FD"),
        0,
        "the LINNUM scale-in-place chain must be gone\n{a}"
    );
}

#[test]
fn float_array_still_correct() {
    assert_eq!(run(FLOAT_SRC), " 7.5 \n");
}

#[test]
fn float_array_scale_reserves_its_stash() {
    let a = asm(FLOAT_SRC);
    assert!(
        count(&a, "STA __ARRAY_INT_TMP") > 0 && a.contains("__ARRAY_INT_TMP:"),
        "the x*5 scale must both use and reserve its 1-byte stash\n{a}"
    );
}

/// `A$(I) = <runtime expression>` is an owner-backed heap write: the destination is a slot
/// *address*, which `abs,X` cannot produce.
const STR_OWNER_SRC: &str = "\
10 DIM A$(4)
20 B$=\"AB\"
30 FOR I=0 TO 4:A$(I)=B$+CHR$(49+I):NEXT
40 FOR I=0 TO 4:PRINT A$(I);:NEXT
50 PRINT
60 END
";

#[test]
fn str_owner_write_still_correct() {
    assert_eq!(run(STR_OWNER_SRC), "AB1AB2AB3AB4AB5\n");
}

/// The name-level gate stays Integer-only precisely so a string array under an induction `FOR`
/// keeps its `AP_n` slot for the owner write.
const S40_SHAPE: &str = "\
10 DIM A$(10)
20 FOR I=1 TO 5
30 READ A$(I)
40 NEXT
50 DATA APPLE,BANANA,CHERRY,DATE,ELDERBERRY
60 N=5
100 FOR I=1 TO N-1
110 FOR J=1 TO N-I
120 IF A$(J)<=A$(J+1) THEN 160
130 T$=A$(J)
140 A$(J)=A$(J+1)
150 A$(J+1)=T$
160 NEXT J
170 NEXT I
190 FOR I=1 TO N
200 PRINT A$(I);
210 NEXT
220 PRINT
230 END
";

#[test]
fn string_array_under_induction_keeps_its_running_pointer() {
    let a = asm(S40_SHAPE);
    assert!(
        a.lines().any(|l| l.trim_end() == "AP_0:"),
        "a string array with an induction shape must still get AP_n\n{a}"
    );
    assert!(
        count(&a, "LDY __ARR_A_S+1,X") > 0,
        "reads should still be abs,X even while AP_n exists\n{a}"
    );
}

#[test]
fn s40_shape_still_sorts() {
    assert_eq!(run(S40_SHAPE), "APPLEBANANACHERRYDATEELDERBERRY\n");
}

// Negative gates

/// 201 elements x 2 bytes = 402 bytes: past what an 8-bit index reaches.
#[test]
fn oversized_string_array_refuses_absx() {
    let a = asm("\
10 DIM A$(200)
20 FOR I=0 TO 200:A$(I)=\"-\":NEXT
30 PRINT A$(150)
40 END
");
    assert_eq!(
        count(&a, "STA __ARR_A_S,X"),
        0,
        "a 402-byte string array must not take abs,X\n{a}"
    );
}

/// 51 elements x 5 bytes = 255 bytes fits; 52 x 5 = 260 does not.
#[test]
fn float_array_size_boundary() {
    let fits = asm("\
10 DIM A(50)
20 POKE 679,3:Q=PEEK(679):P=INT(Q)
30 A(P)=1.5
40 PRINT A(3)
50 END
");
    let over = asm("\
10 DIM A(51)
20 POKE 679,3:Q=PEEK(679):P=INT(Q)
30 A(P)=1.5
40 PRINT A(3)
50 END
");
    assert!(
        builds_cheap_address(&fits, "__ARR_A"),
        "DIM A(50) is 255 bytes and must take the cheap address\n{fits}"
    );
    assert!(
        !builds_cheap_address(&over, "__ARR_A"),
        "DIM A(51) is 260 bytes and must not\n{over}"
    );
    assert!(
        count(&over, "ASL $FD") > 0,
        "the refused array must fall back to the LINNUM scale chain\n{over}"
    );
}

/// Two dimensions never qualify: the fast paths are 1-D by construction.
#[test]
fn two_dim_string_array_refuses_absx() {
    let a = asm("\
10 DIM A$(3,3)
20 A$(1,2)=\"X\"
30 PRINT A$(1,2)
40 END
");
    assert_eq!(
        count(&a, "STA __ARR_A_S,X"),
        0,
        "a 2-D string array must not take abs,X\n{a}"
    );
}

/// A runtime `DIM` has no compile-time base address, so neither path can name one.
#[test]
fn runtime_dim_string_array_refuses_absx() {
    let a = asm("\
10 POKE 679,5:N=PEEK(679)
20 DIM A$(N)
30 A$(2)=\"X\"
40 PRINT A$(2)
50 END
");
    assert_eq!(
        count(&a, "STA __ARR_A_S,X"),
        0,
        "a runtime-DIM'd string array must not take abs,X\n{a}"
    );
}

/// BASIC v2 raises ?BAD SUBSCRIPT for an out-of-range subscript.
#[test]
fn absx_string_array_still_raises_bad_subscript() {
    let (err, line, out) = run_err(
        "\
10 DIM A$(4)
20 POKE 679,9:I=PEEK(679)
30 PRINT \"BEFORE\"
40 A$(I)=\"X\"
50 PRINT \"AFTER\"
60 END
",
    );
    assert_eq!(err, Some(ERR_BAD_SUBSCRIPT), "out={out:?}");
    assert_eq!(line, Some(40), "the trap must be attributed to line 40");
    assert!(
        !out.contains("AFTER"),
        "execution must stop at the bad subscript, got {out:?}"
    );
}

#[test]
fn cheap_address_float_still_raises_bad_subscript() {
    let (err, line, out) = run_err(
        "\
10 DIM A(4)
20 POKE 679,9:I=PEEK(679):J=INT(I)
30 A(J)=1
40 PRINT \"AFTER\"
50 END
",
    );
    assert_eq!(err, Some(ERR_BAD_SUBSCRIPT), "out={out:?}");
    assert_eq!(line, Some(30), "the trap must be attributed to line 30");
}

/// After `CLR`, BASIC v2's auto-`DIM` caps the legal subscript at 10 even though the BSS slot
/// the compiler laid out may be wider.
#[test]
fn absx_honours_the_post_clr_auto_dim_bound() {
    let (err, line, out) = run_err(
        "\
10 DIM A$(30)
20 CLR
30 POKE 679,20:I=PEEK(679)
40 PRINT \"BEFORE\"
50 A$(I)=\"X\"
60 PRINT \"AFTER\"
70 END
",
    );
    assert_eq!(
        err,
        Some(ERR_BAD_SUBSCRIPT),
        "subscript 20 is past the post-CLR auto-DIM bound of 10, out={out:?}"
    );
    assert_eq!(line, Some(50));
    let (err_ok, _, out_ok) = run_err(
        "\
10 DIM A$(30)
20 CLR
30 POKE 679,10:I=PEEK(679)
40 A$(I)=\"X\"
50 PRINT A$(10)
60 END
",
    );
    assert_eq!(err_ok, None, "subscript 10 is in range, out={out_ok:?}");
    assert_eq!(out_ok, "X\n");
}

// End-to-end: a string sort, the shape s_40_string_array is

#[test]
fn string_bubble_sort_round_trips() {
    let out = run("\
10 DIM A$(5)
20 FOR I=0 TO 5:READ A$(I):NEXT
30 FOR P=1 TO 5
40 FOR J=0 TO 4
50 IF A$(J)<=A$(J+1) THEN 90
60 T$=A$(J)
70 A$(J)=A$(J+1)
80 A$(J+1)=T$
90 NEXT J
100 NEXT P
110 FOR I=0 TO 5:PRINT A$(I);:NEXT
120 PRINT
130 END
140 DATA FF,EE,DD,CC,BB,AA
");
    assert_eq!(out, "AABBCCDDEEFF\n");
}
