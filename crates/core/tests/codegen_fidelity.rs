mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const MAX_INSNS: u64 = 200_000_000;

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

fn compile_err(src: &str) -> String {
    let prg = tokenize_program(src).expect("tokenizes");
    match compile_with_options(&prg, CompileOptions::default()) {
        Ok(_) => panic!("expected a compile error, got a binary:\n{src}"),
        Err(e) => e.to_string(),
    }
}

fn raw_output(src: &str) -> Vec<u8> {
    let prg = common::compile_source(src).expect("compiles");
    emu64::run_prg_to_end(&prg, MAX_INSNS).expect("runs").output
}

fn text_output(src: &str) -> String {
    let prg = common::compile_source(src).expect("compiles");
    common::run_clean(&prg).unwrap_or_else(|| panic!("reaches a clean exit:\n{src}"))
}

fn screen_bytes(src: &str) -> [u8; 1000] {
    let prg = common::compile_source(src).expect("compiles");
    emu64::run_prg_to_end(&prg, MAX_INSNS).expect("runs").screen
}

/// BASIC's `PRINT` separates numbers by *moving* the cursor one column, not by writing a blank
/// over it.
#[test]
fn print_number_advances_with_cursor_right_not_a_space() {
    let out = raw_output("10 PRINT 1;2\n");
    assert_eq!(
        out,
        vec![0x20, 0x31, 0x1D, 0x20, 0x32, 0x1D, 0x0D],
        "PRINT 1;2 must emit <sp>1<crsr-right><sp>2<crsr-right><cr>"
    );
}

#[test]
fn print_number_does_not_erase_the_column_it_skips() {
    // All three profiles share `__PRINT_FAC`; assert on each so a profile-specific copy cannot
    // drift.
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let asm = asm_at("10 PRINT 1\n", profile);
        let body = asm
            .split("__PRINT_FAC:")
            .nth(1)
            .expect("__PRINT_FAC is emitted");
        let tail: String = body.lines().take(6).collect::<Vec<_>>().join("\n");
        assert!(
            tail.contains("LDA #$1D"),
            "{profile:?}: __PRINT_FAC must advance with cursor-right:\n{tail}"
        );
        assert!(
            !tail.contains("LDA #$20"),
            "{profile:?}: __PRINT_FAC must not write a space:\n{tail}"
        );
    }
}

#[test]
fn adjacent_printed_numbers_are_still_separated() {
    assert_eq!(text_output("10 PRINT 1;2;3\n"), " 1  2  3 \n");
}

/// BASIC's `FOR` tests at `NEXT`, so `FOR I=5 TO 4` runs its body once.
#[test]
fn fused_poke_loop_runs_its_body_once_on_an_empty_range() {
    let src = "10 POKE 1024,0:POKE 1025,0\n\
               20 FOR I=1025 TO 1024:POKE I,7:NEXT\n";
    let screen = screen_bytes(src);
    assert_eq!(
        screen[1], 7,
        "the single mandatory iteration must POKE 1025"
    );
    assert_eq!(screen[0], 0, "and must not POKE 1024");
}

#[test]
fn fused_poke_loop_runs_once_with_computed_endpoints() {
    let src = "10 POKE 1024,0:POKE 1025,0\n\
               20 READ A,B\n\
               30 FOR I=A TO B:POKE I,7:NEXT\n\
               40 DATA 1025,1024\n";
    let screen = screen_bytes(src);
    assert_eq!(screen[1], 7);
    assert_eq!(screen[0], 0);
}

/// The fusion deletes the `FOR`, so nothing else writes the loop variable.
#[test]
fn fused_poke_loop_leaves_the_loop_variable_at_its_basic_exit_value() {
    assert_eq!(
        text_output("10 FOR I=1025 TO 1024:POKE I,7:NEXT\n20 PRINT I\n"),
        " 1026 \n",
        "empty range: BASIC ends at start+1"
    );
    assert_eq!(
        text_output("10 FOR I=1024 TO 1026:POKE I,7:NEXT\n20 PRINT I\n"),
        " 1027 \n",
        "normal range: BASIC ends at end+1"
    );
}

#[test]
fn clear_screen_idiom_still_fuses_with_no_writeback() {
    let asm = asm_of("10 FOR I=0 TO 999:POKE 1024+I,32:NEXT\n20 PRINT \"X\"\n");
    assert!(
        asm.contains("JSR __POKE_FILL"),
        "fusion still fires:\n{asm}"
    );
    assert!(
        !asm.contains("__FOR_TOP"),
        "and the FOR/NEXT pair is gone:\n{asm}"
    );
}

#[test]
fn fused_poke_loop_still_fills_the_whole_range() {
    let screen = screen_bytes("10 FOR I=1024 TO 1033:POKE I,5:NEXT\n");
    assert!(
        screen[..10].iter().all(|&b| b == 5),
        "ten bytes filled: {:?}",
        &screen[..12]
    );
    assert_ne!(screen[10], 5, "and not one byte more");
}

/// `S$ = "0" + S$` is THE zero-padding idiom.
#[test]
fn prefixing_a_heap_string_with_a_literal_keeps_the_string() {
    let src = "10 C=7\n\
               20 S$=MID$(STR$(C),2)\n\
               30 S$=\"0\"+S$\n\
               40 PRINT S$\n";
    assert_eq!(text_output(src), "07\n");
}

#[test]
fn zero_padded_money_formatting_is_exact() {
    let src = "10 C=7\n\
               20 S$=MID$(STR$(C),2)\n\
               30 IF LEN(S$)<2 THEN S$=\"0\"+S$\n\
               40 PRINT \"0.\";S$\n";
    assert_eq!(text_output(src), "0.07\n");
}

#[test]
fn in_place_append_still_rewinds_the_heap() {
    let src = "10 S$=CHR$(65)\n\
               20 FOR I=1 TO 3:S$=S$+CHR$(66):NEXT\n\
               30 PRINT S$\n";
    let asm = asm_of(src);
    assert!(
        asm.contains("CMP __LAST_STR_VAR_LO"),
        "the LASTVAR rewind must still be emitted for the append idiom:\n{asm}"
    );
    assert_eq!(text_output(src), "ABBB\n");
}

#[test]
fn slicing_a_string_into_itself_is_still_correct() {
    let src = "10 S$=CHR$(65)+CHR$(66)+CHR$(67)\n\
               20 S$=LEFT$(S$,2)\n\
               30 PRINT S$\n";
    assert_eq!(text_output(src), "AB\n");
    // The guard must not have disqualified the shape wholesale: a heap-resident self-slice
    // keeps the rewind.
    let heap = "10 DIM Z$(1)\n\
                20 Z$(0)=CHR$(65)\n\
                30 S$=Z$(0)+CHR$(66)+CHR$(67)\n\
                40 S$=MID$(S$,2)\n\
                50 PRINT S$\n";
    assert_eq!(text_output(heap), "BC\n");
}

/// `10 GOSUB 100 / 20 END / 100 A=1/0:RETURN` stops with `?DIVISION BY ZERO  ERROR IN 100` on a
/// real C64.
#[test]
fn inlined_gosub_body_reports_its_own_line() {
    let src = "10 GOSUB 100\n\
               20 END\n\
               100 L=PEEK(57)+PEEK(58)*256:PRINT L:RETURN\n";
    assert_eq!(text_output(src), " 100 \n");
}

#[test]
fn inlined_gosub_body_reports_its_own_line_from_every_caller() {
    let src = "10 GOSUB 100\n\
               20 GOSUB 100\n\
               30 END\n\
               100 L=PEEK(57):PRINT L:RETURN\n";
    assert_eq!(text_output(src), " 100 \n 100 \n");
}

#[test]
fn the_caller_line_is_restored_after_an_inlined_body() {
    let src = "10 GOSUB 100:L=PEEK(57):PRINT L\n\
               20 END\n\
               100 K=PEEK(57):PRINT K:RETURN\n";
    assert_eq!(text_output(src), " 100 \n 10 \n");
}

#[test]
fn a_non_erroring_inlined_body_costs_no_stamp() {
    let asm = asm_of(
        "10 GOSUB 100\n\
         20 PRINT \"X\"\n\
         30 END\n\
         100 POKE 53280,1:RETURN\n",
    );
    assert!(
        !asm.contains("LDA #$64"),
        "no CURLIN stamp for line 100 should be emitted:\n{asm}"
    );
    assert!(
        asm.contains("STA $D020"),
        "…but the body is still inlined:\n{asm}"
    );
}

#[test]
fn single_use_gosub_is_still_inlined() {
    let asm = asm_of(
        "10 GOSUB 100\n\
         20 END\n\
         100 A=1/0:RETURN\n",
    );
    assert!(!asm.contains("JSR L100"), "body must be inlined:\n{asm}");
}

/// `POKE -1,5` stops with `?ILLEGAL QUANTITY  ERROR IN 10` on a real C64.
#[test]
fn negative_literal_poke_address_traps() {
    let asm = asm_of("10 POKE -1,5\n20 PRINT \"NO ERROR\"\n");
    assert!(asm.contains("__ILLEGAL_QTY"), "{asm}");
    assert!(
        !asm.contains("STA $FFFF"),
        "must not fall through to a $FFFF store:\n{asm}"
    );
}

#[test]
fn constant_propagated_negative_poke_address_traps() {
    let asm = asm_of("10 A=-1\n20 POKE A,1\n30 PRINT \"NO ERROR\"\n");
    assert!(asm.contains("__ILLEGAL_QTY"), "{asm}");
}

#[test]
fn negative_peek_address_traps() {
    let asm = asm_of("10 A=-1\n20 PRINT PEEK(A)\n");
    assert!(asm.contains("__ILLEGAL_QTY"), "{asm}");
}

/// A constant start address the fused POKE loop would write on its mandatory first iteration is
/// the same error.
#[test]
fn fused_poke_loop_with_a_negative_start_traps() {
    let asm = asm_of("10 FOR I=-5 TO -1:POKE I,1:NEXT\n");
    assert!(asm.contains("__ILLEGAL_QTY"), "{asm}");
}

/// The line must be stamped, or the ROM reports the error against whatever line CURLIN happened
/// to hold.
#[test]
fn a_trapping_poke_still_stamps_its_line() {
    let src = "10 PRINT \"A\"\n20 POKE -1,5\n";
    let asm = asm_of(src);
    let before = asm.split("__ILLEGAL_QTY").next().unwrap();
    assert!(
        before.contains("LDA #$14"),
        "CURLIN must hold 20 when the trap fires:\n{asm}"
    );
}

#[test]
fn high_addresses_that_wrap_i16_still_work() {
    let src = "10 READ I\n\
               20 POKE 32000+I,7\n\
               30 PRINT PEEK(32000+I)\n\
               40 DATA 1000\n";
    let asm = asm_of(src);
    assert!(
        !asm.contains("__ILLEGAL_QTY"),
        "no trap for a legal high address:\n{asm}"
    );
    assert_eq!(text_output(src), " 7 \n");
}

/// `PRINT ASC("")` is `?ILLEGAL QUANTITY  ERROR IN 10` on a real C64.
#[test]
fn asc_of_the_empty_string_is_illegal_quantity() {
    for src in [
        "10 PRINT ASC(\"\")\n",
        "10 A$=\"\"\n20 PRINT ASC(A$)\n",
        // byte-context lowering (`try_emit_byte_no_fac`), not the FAC one
        "10 A$=\"\"\n20 POKE 1024,ASC(A$)\n",
    ] {
        let asm = asm_of(src);
        assert!(
            asm.contains("__ILLEGAL_QTY"),
            "ASC(\"\") must raise ?ILLEGAL QUANTITY:\n{src}\n{asm}"
        );
        assert!(
            !asm.contains("JMP $AF08"),
            "…and not ?SYNTAX ERROR:\n{src}\n{asm}"
        );
    }
}

/// `MID$` indexes from 1.
#[test]
fn mid_with_a_zero_start_is_illegal_quantity() {
    for src in [
        "10 A$=\"ABC\"\n20 PRINT MID$(A$,0)\n",
        "10 A$=\"ABC\"\n20 N=0\n30 PRINT MID$(A$,N)\n",
    ] {
        let asm = asm_of(src);
        assert!(
            asm.contains("__ILLEGAL_QTY"),
            "MID$ start 0 must raise:\n{src}\n{asm}"
        );
    }
}

/// `LEFT$`/`RIGHT$`/`MID$` read their counts with GETBYT, which raises above 255.
#[test]
fn string_slice_counts_above_255_are_illegal_quantity() {
    for src in [
        "10 A$=\"ABC\"\n20 PRINT LEFT$(A$,256)\n",
        "10 A$=\"ABC\"\n20 PRINT RIGHT$(A$,256)\n",
        "10 A$=\"ABC\"\n20 PRINT MID$(A$,1,256)\n",
        "10 A$=\"ABC\"\n20 PRINT MID$(A$,256)\n",
    ] {
        let asm = asm_of(src);
        assert!(
            asm.contains("$0E") || asm.contains("__ILLEGAL_QTY"),
            "count > 255 must raise ?ILLEGAL QUANTITY (error $0E):\n{src}\n{asm}"
        );
        assert!(
            !asm.contains("\"NO ERROR\""),
            "…and must not print past the error"
        );
    }
}

#[test]
fn in_range_string_slices_still_fold() {
    let asm = asm_of("10 A$=\"ABCDE\"\n20 PRINT LEFT$(A$,2);MID$(A$,4,2)\n");
    assert!(
        !asm.contains("JSR __STR_LEFT") && !asm.contains("JSR __STR_MID"),
        "LEFT$/MID$ of a literal must still fold:\n{asm}"
    );
    assert_eq!(
        text_output("10 A$=\"ABCDE\"\n20 PRINT LEFT$(A$,2);MID$(A$,4,2)\n"),
        "ABDE\n"
    );
}

#[test]
fn the_argument_checks_that_already_fired_still_fire() {
    for src in [
        "10 A$=\"ABC\"\n20 PRINT LEFT$(A$,-1)\n",
        "10 A$=\"ABC\"\n20 PRINT RIGHT$(A$,-1)\n",
        "10 PRINT CHR$(256)\n",
        "10 A$=\"ABC\"\n20 PRINT MID$(A$,2,-1)\n",
    ] {
        let asm = asm_of(src);
        assert!(
            asm.contains("$0E"),
            "still raises ?ILLEGAL QUANTITY:\n{src}\n{asm}"
        );
    }
}

#[test]
fn an_accumulator_that_leaves_i16_is_not_narrowed() {
    let src = "10 POKE 828,200\n\
               20 A=PEEK(828)\n\
               30 S=0\n\
               40 FOR I=1 TO 200\n\
               50 S=S+A\n\
               60 POKE 1024,S-I*A+I\n\
               70 NEXT\n\
               80 PRINT S\n";
    let asm = asm_of(src);
    assert!(
        !asm.contains("VI_S") && !asm.contains("ZSI_V_S"),
        "S must keep float storage — it reaches 40000:\n{asm}"
    );
    assert_eq!(text_output(src), " 40000 \n");
}

#[test]
fn bounded_variables_are_still_narrowed() {
    let asm = asm_of(
        "10 POKE 828,200\n\
         20 A=PEEK(828)\n\
         30 S=0\n\
         40 FOR I=1 TO 200\n\
         50 S=S+A\n\
         60 POKE 1024,S-I*A+I\n\
         70 NEXT\n\
         80 PRINT S\n",
    );
    assert!(asm.contains("VI_A"), "PEEK-fed A stays promoted:\n{asm}");
    assert!(
        asm.contains("VI_I"),
        "the FOR counter stays promoted:\n{asm}"
    );
}

#[test]
fn a_product_that_leaves_i16_is_not_narrowed() {
    let src = "10 POKE 828,250\n\
               20 A=PEEK(828)\n\
               30 C=A*A\n\
               40 POKE 1024,A-A\n\
               50 PRINT C\n";
    let asm = asm_of(src);
    assert!(!asm.contains("VI_C"), "C reaches 62500:\n{asm}");
    assert_eq!(text_output(src), " 62500 \n");
}

#[test]
fn an_externally_bumped_for_counter_keeps_its_integer_slot() {
    let src = "10 FOR I=1 TO 6\n\
               20 GOTO 100\n\
               30 PRINT I;\n\
               40 NEXT I\n\
               50 PRINT \"F\";I\n\
               60 END\n\
               100 I=I+2\n\
               110 GOTO 30\n";
    assert_eq!(text_output(src), " 3  6 F 7 \n");
}

/// `DIM A(20000)` is 100005 bytes of zeroed BSS.
#[test]
fn a_static_dim_past_the_address_space_is_refused() {
    let msg = compile_err("10 DIM A(20000)\n20 PRINT \"OK\"\n");
    assert!(
        msg.contains("OUT OF MEMORY"),
        "expected an out-of-memory diagnostic, got: {msg}"
    );
}

/// The integer-array form wraps at a different size; catch it too.
#[test]
fn a_static_integer_dim_past_the_address_space_is_refused() {
    let msg = compile_err("10 DIM A%(40000)\n20 PRINT \"OK\"\n");
    assert!(msg.contains("OUT OF MEMORY"), "got: {msg}");
}

#[test]
fn a_large_but_fitting_dim_still_compiles() {
    let asm = asm_of("10 DIM A(2000)\n20 A(5)=7\n30 PRINT A(5)\n");
    assert!(asm.contains("__ARR_A") || asm.contains("A:"), "{asm}");
}

fn nested_fors(depth: usize) -> String {
    let names: Vec<String> = (0..26)
        .flat_map(|a| (0..10).map(move |b| format!("{}{}", (b'A' + a) as char, b)))
        .collect();
    let mut s = String::new();
    let mut n = 10u32;
    for name in names.iter().take(depth) {
        s.push_str(&format!("{n} FOR {name}=1 TO 2\n"));
        n += 10;
    }
    let body = n;
    s.push_str(&format!("{n} POKE 1024,1\n"));
    n += 10;
    for name in names.iter().take(depth).rev() {
        s.push_str(&format!("{n} NEXT {name}\n"));
        n += 10;
    }
    // A branch back into the body makes every NEXT an orphan, which is what routes the loops
    // through the runtime dispatcher and its one-byte handler ids.
    s.push_str(&format!("{n} IF A0=99 THEN GOTO {body}\n"));
    s
}

#[test]
fn too_many_runtime_fors_is_a_compile_error_not_a_panic() {
    let msg = compile_err(&nested_fors(255));
    assert!(
        msg.contains("single byte"),
        "expected the id-space diagnostic, got: {msg}"
    );
}

#[test]
fn a_reasonable_nesting_depth_still_compiles() {
    let asm = asm_of(&nested_fors(8));
    assert!(asm.contains("__FOR_STACK"), "{asm}");
}
