//! CURLIN's low byte ($39): the per-line delta encoding, and the set of lines it may not be
//! held across.

mod common;

use yabcompiler_core::{
    CompileOptions, Dialect, Profile, compile_with_options, resolved_dialect, tokenize_program,
    tokenize_program_with,
};

fn asm_dialect(src: &str, dialect: Dialect) -> String {
    let prg = tokenize_program_with(src, dialect).expect("tokenizes");
    let opts = CompileOptions {
        profile: Profile::Default,
        dialect,
        ..CompileOptions::default()
    };
    compile_with_options(&prg, opts)
        .unwrap_or_else(|e| panic!("compiles: {e}"))
        .asm
}

fn asm(src: &str) -> String {
    asm_dialect(src, Dialect::BasicV2)
}

/// The ordered CURLIN write events in a listing, as they appear in the final instruction
/// stream.
fn curlin_events(asm: &str) -> Vec<String> {
    let insns: Vec<&str> = asm
        .lines()
        .map(|l| l.trim())
        .filter(|l| {
            !l.is_empty() && !l.starts_with(';') && !l.starts_with('.') && !l.ends_with(':')
        })
        .collect();
    let mut out = Vec::new();
    for (i, ins) in insns.iter().enumerate() {
        if *ins == "INC $39" {
            out.push("INC".to_string());
            continue;
        }
        let reg = match *ins {
            "STA $39" => "LO",
            "STA $3A" => "HI",
            _ => continue,
        };
        let imm = i
            .checked_sub(1)
            .and_then(|k| insns[k].strip_prefix("LDA #"))
            .unwrap_or("?");
        out.push(format!("{reg} {imm}"));
    }
    out
}

/// Does `events` contain `want` as a contiguous run?
fn has_run(events: &[String], want: &[&str]) -> bool {
    events
        .windows(want.len())
        .any(|w| w.iter().zip(want).all(|(a, b)| a == b))
}

/// A `+1` run of line numbers encodes as `INC $39` after the first stamp.
#[test]
fn consecutive_lines_encode_the_low_byte_as_inc() {
    let a = asm("10 Z=0\n\
         11 PRINT 1/1\n\
         12 PRINT 2/1\n\
         13 PRINT 1/Z\n");

    assert_eq!(
        curlin_events(&a),
        vec!["HI $00", "LO $0B", "INC", "INC"],
        "the +1 run must collapse to two INCs:\n{a}"
    );
}

#[test]
fn inc_delta_does_not_disturb_the_high_byte_cache() {
    let a = asm("300 Z=0\n\
         301 PRINT 1/1\n\
         302 PRINT 1/Z\n");
    assert_eq!(
        curlin_events(&a),
        vec!["HI $01", "LO $2D", "INC"],
        "302 is +1 from 301 and shares its high byte:\n{a}"
    );
}

#[test]
fn equal_low_bytes_emit_no_store_at_all() {
    let a = asm("44 Z=0\n\
         45 PRINT 1/1\n\
         301 PRINT 1/Z\n");

    assert_eq!(
        curlin_events(&a),
        vec!["LO $2D", "HI $00", "HI $01"],
        "the shared low byte must not be re-stored:\n{a}"
    );
}

// The entry set the cache may not be held across.

/// A static branch target is entered from an unknown predecessor, so it re-states both bytes.
#[test]
fn branch_target_restates_the_low_byte() {
    let a = asm("10 Z=0:GOTO 13\n\
         11 PRINT 1/1\n\
         12 PRINT 2/1\n\
         13 PRINT 1/Z\n");
    // Lines 11 and 12 are unreachable and get eliminated; 13 is the GOTO target and states its
    // low byte outright.
    assert!(
        curlin_events(&a).contains(&"LO $0D".to_string()),
        "L13 must pay a full low-byte stamp:\n{a}"
    );
    assert!(
        !a.contains("INC $39"),
        "nothing in this program may reach the delta encoding:\n{a}"
    );
}

/// The line after a `FOR` header is re-entered by `NEXT`'s back-jump to `__FOR_TOP_n`, which
/// sits *after* that header line's own stamp.
#[test]
fn line_after_for_header_restates_both_bytes() {
    let a = asm("10 Q=1:R=0\n\
         20 FOR I=1 TO 9\n\
         30 PRINT 1/Q\n\
         40 R=R+1:Q=Q-1\n\
         300 PRINT R\n\
         310 NEXT I\n");

    assert!(
        has_run(&curlin_events(&a), &["LO $1E", "HI $00"]),
        "the line after FOR must re-state both bytes:\n{a}"
    );
}

#[test]
fn a_line_that_closes_one_loop_and_opens_another_still_guards_the_next() {
    let a = asm("10 FOR A=1 TO 2\n\
                 20 PRINT 1/1\n\
                 30 NEXT A: FOR B=1 TO 3\n\
                 31 PRINT 2/1\n\
                 32 NEXT B\n");
    let lines: Vec<&str> = a.lines().map(str::trim).collect();
    let top = lines
        .iter()
        .rposition(|l| l.starts_with("__FOR_TOP_") && l.ends_with(':'))
        .expect("B's loop head is in the listing");
    let next_insn = lines[top + 1..]
        .iter()
        .find(|l| !l.is_empty() && !l.starts_with(';'))
        .copied()
        .unwrap_or("");
    assert_ne!(
        next_insn, "INC $39",
        "line 31's stamp sits inside B's loop; a delta there is applied \
         once per iteration:\n{a}"
    );
    assert!(
        curlin_events(&a).contains(&"LO $1F".to_string()),
        "line 31 must re-state its low byte outright:\n{a}"
    );
}

#[test]
fn line_after_repeat_head_restates_both_bytes() {
    let a = asm_dialect(
        "10 Q=1:R=0\n\
         20 REPEAT\n\
         30 PRINT 1/Q\n\
         40 R=R+1:Q=Q-1\n\
         300 PRINT R\n\
         310 UNTIL R=9\n",
        Dialect::Tsb,
    );
    assert!(
        a.contains("__REPEAT_TOP_"),
        "the loop is really a REPEAT:\n{a}"
    );
    assert!(
        has_run(&curlin_events(&a), &["LO $1E", "HI $00"]),
        "UNTIL's back-jump lands past L20's stamp with $3A == 1:\n{a}"
    );
}

#[test]
fn line_after_loop_head_restates_both_bytes() {
    let a = asm_dialect(
        "10 Q=1:R=0\n\
         20 LOOP\n\
         30 PRINT 1/Q\n\
         40 R=R+1:Q=Q-1\n\
         300 PRINT R\n\
         310 EXIT IF R>8\n\
         320 END LOOP\n\
         330 END\n",
        Dialect::Tsb,
    );
    assert!(a.contains("__LOOP_TOP_"), "the loop is really a LOOP:\n{a}");
    assert!(
        has_run(&curlin_events(&a), &["LO $1E", "HI $00"]),
        "END LOOP's back-jump lands past L20's stamp:\n{a}"
    );
}

/// A non-literal `CGOTO` walks a runtime line table that can `JMP` at *any* `L<n>:`, so no line
/// in the program may hold either cache.
#[test]
fn runtime_computed_goto_turns_both_caches_off() {
    let src = "5 DATA 301\n\
               10 READ X\n\
               20 CGOTO X\n\
               300 PRINT 1/1\n\
               301 PRINT 1/Q\n";
    let a = asm_dialect(src, Dialect::Tsb);
    assert!(
        a.contains("__CGOTO_DISPATCH"),
        "target really is runtime:\n{a}"
    );
    assert!(
        !a.contains("INC $39"),
        "every line is reachable from the table; no deltas:\n{a}"
    );
    assert!(
        has_run(&curlin_events(&a), &["LO $2D", "HI $01"]),
        "L301 must re-state both bytes:\n{a}"
    );
}

#[test]
fn literal_computed_goto_is_just_a_branch_target() {
    let a = asm_dialect(
        "10 Z=0:CGOTO 13\n\
         11 PRINT 1/1\n\
         12 PRINT 2/1\n\
         13 PRINT 1/Z\n",
        Dialect::Tsb,
    );
    assert!(
        !a.contains("__CGOTO_DISPATCH"),
        "a literal target must not build a table:\n{a}"
    );
    assert!(
        curlin_events(&a).contains(&"LO $0D".to_string()),
        "L13 is the CGOTO's target and states its own low byte:\n{a}"
    );
}

/// `ON ERROR` / `RESUME` reach the same runtime line table.
#[test]
fn on_error_turns_both_caches_off() {
    let a = asm_dialect(
        "10 ON ERROR GOTO 300\n\
         11 PRINT 1/1\n\
         12 PRINT 2/1\n\
         13 PRINT 1/Q\n\
         300 RESUME NEXT\n",
        Dialect::Tsb,
    );
    assert!(a.contains("__TSB_GOTO_LINE"), "the table is present:\n{a}");
    assert!(
        !a.contains("INC $39"),
        "RESUME can land on any line; no deltas:\n{a}"
    );
}

#[test]
fn on_key_turns_both_caches_off() {
    let a = asm_dialect(
        "10 ON KEY \"Q\" GOSUB 300\n\
         11 PRINT 1/1\n\
         12 PRINT 2/1\n\
         13 PRINT 3/1\n\
         14 END\n\
         300 PRINT \"HIT\":RETURN\n",
        Dialect::Tsb,
    );
    assert!(a.contains("__ON_KEY_FIRE"), "the poll is present:\n{a}");
    assert!(
        !a.contains("INC $39"),
        "the handler can return between any two statements:\n{a}"
    );
    let ev = curlin_events(&a);
    for want in ["LO $0C", "LO $0D"] {
        assert!(
            ev.contains(&want.to_string()),
            "{want} must be a full stamp:\n{ev:#?}"
        );
    }
}

/// A returning `GOSUB` leaves the subroutine's line in `$39`.
#[test]
fn gosub_return_drops_the_low_byte_cache() {
    let a = asm("10 PRINT 1/1:GOSUB 300\n\
         11 PRINT 2/1\n\
         12 END\n\
         300 PRINT 3/1:RETURN\n");
    assert!(
        !a.contains("INC $39"),
        "L10 called out and back; $39 holds 300's low byte:\n{a}"
    );
    assert!(
        curlin_events(&a).contains(&"LO $0B".to_string()),
        "L11 must pay a full stamp:\n{a}"
    );
}

// The observability gate.

/// A program that reads CURLIN sees every elision.
#[test]
fn peeking_curlin_disables_the_delta_encoding() {
    let src = "10 A=PEEK(57)\n\
               11 B=PEEK(57)\n\
               12 C=PEEK(58)\n\
               13 PRINT A;B;C\n\
               14 PRINT PEEK(57);PEEK(58)\n";
    let a = asm(src);
    assert!(
        !a.contains("INC $39"),
        "PEEK(57) makes the low byte observable:\n{a}"
    );
    let ev = curlin_events(&a);
    for want in ["LO $0A", "LO $0B", "LO $0C", "LO $0D", "LO $0E"] {
        assert!(
            ev.contains(&want.to_string()),
            "{want} must pay the full stamp:\n{ev:#?}"
        );
    }

    let prg = common::compile_source(src).expect("compiles");
    let out = common::run_clean(&prg).expect("clean exit");
    assert_eq!(
        out.split_whitespace().collect::<Vec<_>>(),
        vec!["10", "11", "0", "14", "0"],
        "CURLIN as PEEKed must match the ROM:\n{out}"
    );
}

#[test]
fn a_line_that_reads_the_low_byte_keeps_its_low_byte_stamp() {
    let src = "10 A=PEEK(57)\n\
               11 B=PEEK(57)\n\
               12 C=PEEK(57)\n\
               13 PRINT A;B;C\n\
               14 PRINT PEEK(57);PEEK(58)\n";
    let ev = curlin_events(&asm(src));
    assert!(
        ev.contains(&"LO $0C".to_string()),
        "line 12 reads $39 itself:\n{ev:#?}"
    );
    let prg = common::compile_source(src).expect("compiles");
    let out = common::run_clean(&prg).expect("clean exit");
    assert_eq!(
        out.split_whitespace().collect::<Vec<_>>(),
        vec!["10", "11", "12", "14", "0"],
        "CURLIN as PEEKed must match the ROM:\n{out}"
    );
}

#[test]
fn poking_curlin_disables_the_delta_encoding() {
    let a = asm("10 POKE 58,0\n\
         11 PRINT 1/1\n\
         12 PRINT 2/1\n\
         13 PRINT 3/1\n");
    assert!(!a.contains("INC $39"), "POKE 58 is a CURLIN write:\n{a}");
}

#[test]
fn the_delta_encoding_is_what_makes_the_run_cheaper() {
    const RUN_SRC: &str = "10 Z=0\n11 PRINT 1/1\n12 PRINT 1/1\n13 PRINT 1/1\n\
                           14 PRINT 1/1\n15 PRINT 1/Z\n";
    const TENS_SRC: &str = "10 Z=0\n20 PRINT 1/1\n30 PRINT 1/1\n40 PRINT 1/1\n\
                            50 PRINT 1/1\n60 PRINT 1/Z\n";
    fn size(src: &str) -> usize {
        let prg = tokenize_program(src).expect("tokenizes");
        compile_with_options(&prg, CompileOptions::default())
            .expect("compiles")
            .machine_code
            .len()
    }

    let run_asm = asm(RUN_SRC);
    let tens_asm = asm(TENS_SRC);
    assert_eq!(
        run_asm.matches("INC $39").count(),
        4,
        "the run must pay four +1 deltas:\n{run_asm}"
    );
    assert!(
        !tens_asm.contains("INC $39"),
        "lines numbered in tens are not +1 apart, so nothing may be delta-encoded:\n{tens_asm}"
    );
    let (run, tens) = (size(RUN_SRC), size(TENS_SRC));
    assert_eq!(
        tens - run,
        2,
        "the delta encoding's 8 bytes less the 6 PH309 recovers from the tens \
         build's shared `STA $39`: run={run} tens={tens}"
    );
}

fn after_for_top(asm: &str, idx: usize) -> Vec<String> {
    let lines: Vec<&str> = asm.lines().map(str::trim).collect();
    let top = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.starts_with("__FOR_TOP_") && l.ends_with(':'))
        .map(|(i, _)| i)
        .nth(idx)
        .unwrap_or_else(|| panic!("no __FOR_TOP_ #{idx} in:\n{asm}"));
    lines[top + 1..]
        .iter()
        .filter(|l| !l.is_empty() && !l.starts_with(';'))
        .take(2)
        .map(|l| l.to_string())
        .collect()
}

#[test]
fn a_body_starting_on_the_header_line_restamps_at_the_loop_head() {
    let a = asm("10 FOR I=1 TO 2:IF I=2 THEN X=1/0\n\
                 20 NEXT\n");
    assert_eq!(
        after_for_top(&a, 0),
        vec!["LDA #$0A", "STA $39"],
        "the tail after `FOR` re-runs under line 20's stamp otherwise:\n{a}"
    );
}

#[test]
fn a_mid_line_body_restamps_under_a_gosub_too() {
    let a = asm("10 GOSUB 100\n\
                 20 END\n\
                 100 FOR I=1 TO 3:PRINT I;:IF I=2 THEN X=1/0\n\
                 110 NEXT:RETURN\n");
    assert_eq!(
        after_for_top(&a, 0),
        vec!["LDA #$64", "STA $39"],
        "line 100's low byte must be restated at the loop head:\n{a}"
    );

    let lines: Vec<&str> = a.lines().map(str::trim).collect();
    let top = lines
        .iter()
        .position(|l| l.starts_with("__FOR_TOP_") && l.ends_with(':'))
        .expect("a loop head is in the listing");
    assert!(
        !lines[top + 1..top + 5].contains(&"STA $3A"),
        "a whole-module high byte must not be restated per iteration:\n{a}"
    );
}

/// Read `PEEK(57)+PEEK(58)*256` ; the line the machine believes it is on ; at each point the program prints it.
fn curlin_trace(src: &str) -> String {
    let prg = tokenize_program(src).expect("tokenizes");
    let out = compile_with_options(&prg, CompileOptions::default()).expect("compiles");
    let r = emu64::run_prg_to_end(&out.prg_bytes, common::MAX_INSNS).expect("runs");
    assert!(r.clean_exit, "must run to the end:\n{src}");
    common::petscii_to_string(&r.output).trim_end().to_string()
}

#[test]
fn a_mid_line_body_restamps_the_high_byte_when_the_module_has_no_invariant() {
    let a = asm("10 GOTO 300\n\
                 300 FOR I=1 TO 3:PRINT I;:IF I=2 THEN X=1/0\n\
                 310 NEXT\n");
    let lines: Vec<&str> = a.lines().map(str::trim).collect();
    let top = lines
        .iter()
        .position(|l| l.starts_with("__FOR_TOP_") && l.ends_with(':'))
        .expect("a loop head is in the listing");
    assert_eq!(
        &lines[top + 1..top + 3],
        &["LDA #$2C", "STA $39"],
        "line 300's low byte must be restated at the loop head:\n{a}"
    );
    // The body runs under 300 on every iteration, not under 310.
    assert_eq!(
        curlin_trace(
            "10 GOTO 300\n\
             300 FOR I=1 TO 3:PRINT PEEK(57)+PEEK(58)*256;\n\
             310 NEXT\n"
        ),
        " 300  300  300"
    );
}

#[test]
fn a_loop_head_across_the_high_byte_boundary_keeps_its_stamp() {
    let a = asm("10 GOTO 250\n\
                 250 FOR I=1 TO 3:PRINT I;\n\
                 260 PRINT I;\n\
                 300 NEXT\n");
    let lines: Vec<&str> = a.lines().map(str::trim).collect();
    let top = lines
        .iter()
        .position(|l| l.starts_with("__FOR_TOP_") && l.ends_with(':'))
        .expect("a loop head is in the listing");
    assert_eq!(
        &lines[top + 1..top + 5],
        &["LDA #$FA", "STA $39", "LDA #$00", "STA $3A"],
        "the back edge from 300 arrives with $3A=$01, so the head must \
         restate both halves of 250:\n{a}"
    );
    assert_eq!(
        curlin_trace(
            "10 GOTO 250\n\
             250 FOR I=1 TO 3:PRINT PEEK(57)+PEEK(58)*256;\n\
             260 PRINT PEEK(57)+PEEK(58)*256;\n\
             300 NEXT\n"
        ),
        " 250  260  250  260  250  260"
    );
}

#[test]
fn a_for_alone_on_its_line_gets_no_loop_head_stamp() {
    let a = asm("10 FOR I=1 TO 2\n\
                 20 X=1/0\n\
                 30 NEXT\n");
    let head = after_for_top(&a, 0);
    assert_ne!(
        head.first().map(String::as_str),
        Some("LDA #$0A"),
        "line 20 is a re-entry line and stamps itself:\n{a}"
    );

    assert!(
        curlin_events(&a).contains(&"LO $14".to_string()),
        "line 20 must restate its low byte outright:\n{a}"
    );
}

#[test]
fn a_mid_line_body_that_cannot_raise_gets_no_loop_head_stamp() {
    let a = asm("10 FOR I=1 TO 2:POKE 1024,5\n\
                 20 NEXT\n");
    assert_eq!(
        after_for_top(&a, 0),
        vec!["LDA #$05", "STA $0400"],
        "the loop head must fall straight into the POKE:\n{a}"
    );
}

/// Condition 3 ; the loop must be closed on another line.
#[test]
fn a_single_line_loop_gets_no_loop_head_stamp() {
    let a = asm("10 FOR I=1 TO 2:X=1/0:NEXT\n\
                 20 END\n");
    assert_ne!(
        after_for_top(&a, 0).first().map(String::as_str),
        Some("LDA #$0A"),
        "nothing between the head and the back edge writes $39:\n{a}"
    );
}

    /// `RESET` restores to the first DATA line at or after its argument.
    /// The argument may be any expression, not only a literal.
    #[test]
    fn reset_targets_the_first_data_line_at_or_after_its_argument() {
        // DATA on 10 and 20 only; line 15 exists but carries none.
        let src = |arg: &str| {
            format!(
                "10 DATA 1,2,3\n15 REM\n20 DATA 4,5,6\n\
                 30 READ B\n40 {arg}\n50 READ B\n60 PRINT B\n70 END\n"
            )
        };

        // `RESET 20` and `RESET 15` both land on line 20's DATA.
        for arg in ["RESET 20", "RESET 15"] {
            let asm = asm_dialect(&src(arg), Dialect::Tsb);
            assert!(
                asm.contains("LDA #<__DATA_LINE_20"),
                "{arg} must point at line 20's DATA:\n{asm}"
            );
            assert!(
                !asm.contains("LDA #<__DATA_LINE_10"),
                "{arg} must not target line 10 -- that is the wrong DATA:
{asm}"
            );
        }

        // Past the last DATA line, the pointer is __DATA_END, so the next
        // READ raises ?OUT OF DATA.
        let asm = asm_dialect(&src("RESET 65000"), Dialect::Tsb);
        assert!(asm.contains("#<__DATA_END"), "past the end:\n{asm}");

        // A non-literal argument builds the runtime table and search.
        // PEEK keeps the argument out of const-prop; `X=20:RESET X` would fold.
        let asm = asm_dialect(&src("RESET 20+PEEK(198)"), Dialect::Tsb);
        assert!(asm.contains("JSR __RESET_FIND"), "runtime search:\n{asm}");
        assert!(asm.contains("__RESET_LO:"), "line table:\n{asm}");
        // Two parallel byte tables, so X can index them and the search
        // covers 255 DATA lines rather than the 127 a `.word` table allows.
        assert!(asm.contains("__RESET_PLO:"), "pointer table lo:\n{asm}");
        assert!(asm.contains("__RESET_PHI:"), "pointer table hi:\n{asm}");
        assert!(
            asm.contains(".byte <__DATA_LINE_20"),
            "the pointer table names the DATA-line labels:\n{asm}"
        );
        // GETADR, not the int island: `RESET -1` must raise ?ILLEGAL QUANTITY.
        assert!(
            asm.contains("JSR $B7F7"),
            "argument goes through GETADR:\n{asm}"
        );
    }

/// TSB boot colours and charset follow live extension use, not a statement
/// whitelist and not the dialect flag alone.
#[test]
fn the_boot_prologue_follows_the_dialect_not_the_statement_whitelist() {
    // `USE` is TSB, so this program gets the TSB prologue.
    let tsb = asm_dialect("10 USE \"[###]\",5\n", Dialect::Tsb);
    assert!(
        tsb.contains("LDA #$17") && tsb.contains("STA $D018"),
        "a TSB program gets TSB's charset:\n{tsb}"
    );
    assert!(
        tsb.contains("STA $D020"),
        "and TSB's boot colours:\n{tsb}"
    );

    // A TSB function counts too, even with no TSB statement.
    let expr_only = asm_dialect("10 A$=\"F\"
20 PRINT NRM(A$)
", Dialect::Tsb);
    assert!(
        expr_only.contains("STA $D018"),
        "a TSB function is TSB evidence too:
{expr_only}"
    );

    // No TSB construct keeps the KERNAL defaults, even with dialect TSB.
    let plain = asm_dialect("10 PRINT 5\n", Dialect::Tsb);
    assert!(
        !plain.contains("STA $D018") && !plain.contains("STA $D020"),
        "no extension token means no TSB prologue, whatever the flag says:\n{plain}"
    );

    // A token on a dead line does not decide the boot prologue.
    let dead = asm_dialect("10 PRINT 5\n20 END\n30 USE \"[###]\",5\n", Dialect::Tsb);
    assert!(
        !dead.contains("L30:"),
        "the line really is eliminated, or this proves nothing:\n{dead}"
    );
    assert!(
        !dead.contains("STA $D018"),
        "a token on a dead line must not decide the boot prologue:\n{dead}"
    );
}

/// `TI$` is rebuilt in one shared slot, so a compare of two reads snapshots
/// the left one before the right one overwrites it.
#[test]
fn a_compare_of_two_volatile_string_reads_snapshots_the_left_one() {
    let both = asm("10 IF TI$<>TI$ THEN PRINT \"NE\"\n");
    assert!(
        both.contains("JSR __STR_SNAP"),
        "two rebuild-in-place reads need the left one copied aside:\n{both}"
    );

    // An ordinary string compare needs no snapshot.
    let ordinary = asm("10 A$=\"X\":B$=\"Y\"\n20 IF A$<>B$ THEN PRINT \"NE\"\n");
    assert!(
        !ordinary.contains("JSR __STR_SNAP"),
        "an ordinary compare needs no snapshot:\n{ordinary}"
    );

    // A copy is already stable: `T$=TI$` cannot be overwritten by the next read.
    let copied = asm("10 T$=TI$\n20 IF T$<>TI$ THEN PRINT \"NE\"\n");
    assert!(
        !copied.contains("JSR __STR_SNAP"),
        "a copy is stable storage already:\n{copied}"
    );
}

/// TSB `USE <n>` stores the device in $BE. An expression `USE` reads that cell.
/// Only a leading digit takes the device path, so `USE D` is not a select.
#[test]
fn use_selects_a_device_in_the_cell_tsb_uses() {
    let set = asm_dialect("10 USE 0+9\n20 PRINT USE\n", Dialect::Tsb);
    assert!(
        set.contains("STA $BE"),
        "the device is stored where TSB stores it:\n{set}"
    );
    assert!(
        set.contains("LDA #$08"),
        "and the cell starts at TSB's boot value:\n{set}"
    );

    // No USE means no write to $BE. On a stock C64 that cell is the tape buffer.
    let plain = asm_dialect("10 PRINT 5\n", Dialect::Tsb);
    assert!(
        !plain.contains("STA $BE"),
        "no USE means no write to $BE:\n{plain}"
    );

    // Only a leading digit takes the device path, so `USE D` is a control string.
    let named = asm_dialect("10 D=9\n20 USE D\n", Dialect::Tsb);
    assert!(
        !named.contains("STA $BE"),
        "USE D is not a device select:\n{named}"
    );
}

/// A `USE` control string may be any string expression. A literal still uses
/// the compile-time path. A runtime scan keeps a pristine copy of the mask.
#[test]
fn a_use_control_string_may_be_a_string_expression() {
    let dynamic = asm_dialect("10 F$=\"[###]\"\n20 USE F$,12\n", Dialect::Tsb);
    assert!(
        dynamic.contains("JSR __USE_LOADV") && dynamic.contains("JSR __USE_FIND"),
        "a variable control string is scanned at run time:\n{dynamic}"
    );

    // The literal form must not drag the scanner in with it.
    let literal = asm_dialect("10 USE \"[###]\",12\n", Dialect::Tsb);
    assert!(
        literal.contains("JSR __USE_DESC"),
        "a literal keeps the pooled descriptor:\n{literal}"
    );
    assert!(
        !literal.contains("__USE_LOADV"),
        "and pays nothing for the runtime scanner:\n{literal}"
    );

    // The scanner reads a pristine copy: after field 1, `###` has become `123`.
    let two = asm_dialect("10 F$=\"[###][###]\"\n20 USE F$,1,2\n", Dialect::Tsb);
    assert!(
        two.contains("STA __USE_SRC,X"),
        "the control image is kept twice:\n{two}"
    );
}

/// Int-promotion must rename every read of a promoted variable, including
/// those inside `USE`.
#[test]
fn int_promotion_renames_reads_in_every_statement() {
    let looped = asm_dialect("10 FOR I=1 TO 3\n20 USE \"[###]\",I\n30 NEXT\n", Dialect::Tsb);
    assert!(
        !looped.contains("LDA #<V_I"),
        "the USE value reads the live counter, not the stale float slot:\n{looped}"
    );

    // Same read outside a loop: promotion comes from the write, not a FOR header.
    let plain = asm_dialect("10 N=7\n20 USE \"[###]\",N\n", Dialect::Tsb);
    assert!(
        !plain.contains("LDA #<V_N"),
        "a promoted scalar reads its integer slot too:\n{plain}"
    );
}

/// `RCOMP` guards the rest of the line using the last `IF` result. A bare
/// statement list is allowed, not only `THEN` or `GOTO`.
#[test]
fn rcomp_guards_a_bare_statement_list() {
    for src in [
        // a bare statement
        "10 A=1\n20 IF A=1 THEN PRINT\"T\"\n30 RCOMP PRINT\"R\"\n",
        // several statements: the whole rest of the line is one guarded block
        "10 A=1\n20 IF A=1 THEN PRINT\"T\"\n30 RCOMP PRINT\"A\":PRINT\"B\"\n40 PRINT\"C\"\n",
        // an assignment
        "10 A=1\n20 IF A=1 THEN PRINT\"T\"\n30 RCOMP B=2:C=3\n",
        // a colon straight after the token
        "10 A=1\n20 IF A=1 THEN PRINT\"T\"\n30 RCOMP:PRINT\"R\"\n",
        // GOTO
        "10 IF 1=1 THEN PRINT\"T\"\n20 RCOMP GOTO 40\n40 END\n",
    ] {
        let asm = asm_dialect(src, Dialect::Tsb);
        assert!(
            asm.contains("__LAST_IF"),
            "RCOMP compiles and tests the remembered IF result for {src:?}:\n{asm}"
        );
    }
}

/// `THEN` after `RCOMP` is a run-time ?SYNTAX ERROR, raised only if the
/// branch is taken. The program still compiles.
#[test]
fn then_after_rcomp_raises_only_when_the_branch_is_taken() {
    let src = "10 A=1\n20 IF A=1 THEN PRINT\"T\"\n30 RCOMP THEN PRINT\"R\"\n";
    let prg = tokenize_program_with(src, Dialect::Tsb).expect("tokenizes");
    let opts = CompileOptions {
        profile: Profile::Default,
        dialect: Dialect::Tsb,
        ..CompileOptions::default()
    };
    let out = compile_with_options(&prg, opts).expect("compiles rather than refusing the program");
    assert!(
        out.diagnostics
            .syntax_traps
            .iter()
            .any(|t| t.detail.contains("THEN after RCOMP")),
        "the trap is reported: {:?}",
        out.diagnostics.syntax_traps
    );
    assert!(
        out.asm.contains("__LAST_IF"),
        "and it is still guarded by the remembered IF result:\n{}",
        out.asm
    );
}

/// `INSERT` checks the box before it writes. Out of range raises ?BAD MODE.
/// A pattern that is not nine characters is a run-time ?ILLEGAL QUANTITY.
#[test]
fn insert_checks_its_box_before_it_writes_anything() {
    let asm = asm_dialect("10 R$=\"ABCDEFGHI\"\n20 INSERT R$,2,3,10,5,1\n", Dialect::Tsb);
    let guard = asm
        .find("JSR __INSERT_BOX_BOUNDS")
        .expect("the guard is emitted");
    let fill = asm.find("__TSB_RECT_FILL").expect("the rects are emitted");
    assert!(
        guard < fill,
        "the guard runs before the first write, colour fill included:\n{asm}"
    );
    assert!(
        asm.contains("__TSB_BAD_MODE"),
        "and an out-of-range box raises TSB's own error:\n{asm}"
    );

    let short = tokenize_program_with("10 R$=\"ABC\"\n20 INSERT R$,2,3,10,5,1\n", Dialect::Tsb)
        .expect("tokenizes");
    let opts = CompileOptions {
        profile: Profile::Default,
        dialect: Dialect::Tsb,
        ..CompileOptions::default()
    };
    assert!(
        compile_with_options(&short, opts).is_ok(),
        "a non-nine-character pattern must not refuse the program"
    );
}

/// TSB error 32 is not in the ROM message table, so the message is printed
/// here and control re-enters the ROM error tail at $A46C.
#[test]
fn bad_mode_prints_tsbs_own_message() {
    let asm = asm_dialect("10 R$=\"ABCDEFGHI\"\n20 INSERT R$,2,3,10,5,1\n", Dialect::Tsb);
    assert!(
        asm.contains("$0D,$3F,$42,$41,$44,$20,$4D,$4F,$44,$45,$20,$20,$45,$52,$52,$4F,$52,$00"),
        "the measured message bytes are emitted verbatim:\n{asm}"
    );
    assert!(
        asm.contains("JMP $A46C"),
        "and it re-enters the ROM error tail rather than the message table:\n{asm}"
    );

    // A program with no ?BAD MODE path must not pay for the printer.
    let plain = asm_dialect("10 PRINT 5\n", Dialect::Tsb);
    assert!(
        !plain.contains("__TSB_BAD_MODE"),
        "the printer is only emitted where it is reachable:\n{plain}"
    );
}

/// An accumulator with no proven bound stays a float. A later `IF r>32767`
/// guard does not bound the store on the previous line.
#[test]
fn an_unbounded_accumulator_stays_a_float() {
    // No bound exists: the interpreter counts up in floating point forever.
    let loop_only = asm("10 r=r+1000\n20 goto 10\n");
    assert!(
        !loop_only.contains("VI_R"),
        "an accumulator with no proven bound must not become an integer:\n{loop_only}"
    );

    // Every read is an integer context, but the store overshoots the later guard.
    let reported = asm(
        "10 a%=1524\n110 r=peek(56324)+r+a%\n120 if r>32767 then r=r-32767\n\
         130 p%=r and 7\n140 goto 110\n",
    );
    assert!(
        !reported.contains("VI_R"),
        "a guard bounds the read, not the store, so R stays a float:\n{reported}"
    );

    // A variable whose definitions do not mention it can still promote.
    let plain = asm("10 for i=1 to 10\n20 k=i+1\n30 poke 1024,k\n40 next\n");
    assert!(
        plain.contains("VI_K") || plain.contains("VI_I"),
        "ordinary bounded variables still promote:\n{plain}"
    );
}

/// A second separator or a mid-run sign starts a new field in the same run.
/// With one value, `[##+##]` reaches field two and raises ?BAD MODE.
#[test]
fn a_use_run_can_hold_more_than_one_field() {
    for src in [
        "10 USE \"[#,###,###]\",1\n",
        "10 USE \"[#,###.##]\",1.5\n",
        "10 USE \"[##+##]\",12\n",
        "10 USE \"[#.#,#]\",1.5\n",
        "10 USE \"[++##]\",1\n",
    ] {
        let prg = tokenize_program_with(src, Dialect::Tsb).expect("tokenizes");
        let opts = CompileOptions {
            profile: Profile::Default,
            dialect: Dialect::Tsb,
            ..CompileOptions::default()
        };
        let out = compile_with_options(&prg, opts).expect("compiles");
        assert!(
            out.diagnostics.syntax_traps.is_empty(),
            "a mask TSB accepts must not be trapped: {src:?} {:?}",
            out.diagnostics.syntax_traps
        );
        assert!(
            out.asm.contains("__TSB_BAD_MODE"),
            "and the second field has no value, so it reaches ?BAD MODE:\n{}",
            out.asm
        );
    }
}

/// Too few values: print nothing, then ?BAD MODE. Too many: print first, then
/// raise. The raise sits at the field; an echo branch jumps past it.
#[test]
fn bad_mode_prints_before_or_after_by_which_side_ran_out() {
    // The peephole may fold `JMP __TSB_BAD_MODE` into a fallthrough, so look
    // at the program body, everything before the helper block.
    let body = |src: &str| {
        let a = asm_dialect(src, Dialect::Tsb);
        let cut = a
            .find("; --- helper subroutines ---")
            .unwrap_or(a.len());
        (a[..cut].to_string(), a)
    };

    // Too few: the buffer is never flushed at all.
    let (few, few_all) = body("10 USE \"[###]\"
");
    assert!(
        few_all.contains("__BAD_MODE_MSG"),
        "it raises TSB own error:
{few_all}"
    );
    assert!(
        !few.contains("JSR __USE_SHOW"),
        "and prints nothing before it:
{few}"
    );

    // Too many: flush first, without the statement's own carriage return.
    let (many, many_all) = body("10 USE \"[###]\",1,2
");
    let show = many.find("JSR __USE_SHOW").expect("prints");
    assert!(
        many_all.contains("__BAD_MODE_MSG"),
        "and then raises:
{many_all}"
    );
    assert!(
        !many[show..].contains("LDA #$0D"),
        "no trailing carriage return between the flush and the error:
{}",
        &many[show..]
    );

    // A run cut into two fields stops at the first echo, so the second field
    // raise is jumped over rather than reached.
    let (echo, _) = body("10 USE \"[#,###,###]\",1
");
    assert!(
        echo.contains("LDA __UF_ECHO"),
        "the echo is tested between the fields of one run:
{echo}"
    );
}

/// `!` deletes a position, so `USE "!"` is empty and raises ?SYNTAX ERROR.
/// A trailing comma is a value: reached it is ?SYNTAX, unreached it is ?BAD MODE.
#[test]
fn a_control_string_that_is_all_bangs_is_empty() {
    for src in ["10 USE \"!\"\n", "10 USE \"!!\"\n", "10 USE \"!\",1\n"] {
        let asm = asm_dialect(src, Dialect::Tsb);
        assert!(
            asm.contains("__ILLEGAL") || asm.contains("JMP $A437") || asm.contains("__TSB_ERROR"),
            "an all-bang control string raises: {src:?}\n{asm}"
        );
        assert!(
            !asm.contains("JSR __USE_SHOW"),
            "and prints nothing: {src:?}\n{asm}"
        );
    }

    // Reached: ?SYNTAX, and no ?BAD MODE anywhere.
    let reached = asm_dialect("10 USE \"[###]\",\n", Dialect::Tsb);
    assert!(
        !reached.contains("__TSB_BAD_MODE"),
        "a trailing comma the mask reaches is ?SYNTAX, not ?BAD MODE:\n{reached}"
    );
    // Not reached: the statement prints, then ?BAD MODE.
    let spare = asm_dialect("10 USE \"###\",1,\n", Dialect::Tsb);
    assert!(
        spare.contains("__TSB_BAD_MODE") && spare.contains("JSR __USE_SHOW"),
        "a trailing comma past the last field prints first:\n{spare}"
    );
}

/// Spaces may sit between the two characters of a relational operator, as in
/// `1< >2`. CHRGET skips them, so the statement must compile.
#[test]
fn a_relational_operator_may_be_split_by_spaces() {
    for (src, expect) in [
        ("10 PRINT 1<>2\n", "matched already"),
        ("10 PRINT 1< >2\n", "the reported shape"),
        ("10 PRINT 1 < > 2\n", "spaces on both sides too"),
        ("10 PRINT 1> <2\n", "the reversed spelling"),
        ("10 PRINT 1< =2\n", "and <= as well"),
    ] {
        let prg = tokenize_program(src).expect("tokenizes");
        let out = compile_with_options(&prg, CompileOptions::default())
            .unwrap_or_else(|e| panic!("{expect}: {src:?} must compile: {e}"));
        // Compiling is not enough: a refusal is a recorded trap plus a jump
        // to the ROM error routine.
        assert!(
            out.diagnostics.syntax_traps.is_empty(),
            "{expect}: {src:?} must not be trapped: {:?}",
            out.diagnostics.syntax_traps
        );
        assert!(
            !out.asm.contains("JMP $A437"),
            "{expect}: {src:?} must not reduce to a jump to the ROM error routine:\n{}",
            out.asm
        );
    }
}

/// A type sigil binds tighter than an extension keyword. `DIR%=1` is DI%,
/// and `MAP$="x"` is MA$. Neither is TSB evidence for `Dialect::Auto`.
#[test]
fn a_type_sigil_keeps_an_extension_keyword_a_variable() {
    // A sigilled assignment is not extension evidence.
    let src = "10 OX%=10:DIR%=1\n20 IF DIR%<0 THEN DIR%=0-DIR%\n30 PRINT DIR%\n";
    let picked = resolved_dialect(src, Dialect::Auto);
    assert_eq!(
        picked,
        Dialect::BasicV2,
        "a sigilled assignment is not extension evidence"
    );
    let prg = tokenize_program_with(src, Dialect::Auto).expect("tokenizes");
    assert!(
        compile_with_options(&prg, CompileOptions::default()).is_ok(),
        "and the program compiles"
    );

    // The string sigil behaves the same way.
    assert_eq!(
        resolved_dialect("10 MAP$=\"X\"\n20 PRINT MAP$\n", Dialect::Auto),
        Dialect::BasicV2,
        "so does a string sigil"
    );

    // A bare extension keyword in statement position is still TSB evidence.
    assert_eq!(
        resolved_dialect("10 PRINT\"X\"\n20 DIR\n", Dialect::Auto),
        Dialect::Tsb,
        "a bare DIR in statement position is still TSB"
    );
    // An assignment with no sigil is a variable too.
    assert_eq!(
        resolved_dialect("10 MEM=5\n20 PRINT MEM\n", Dialect::Auto),
        Dialect::BasicV2,
        "an unsigilled assignment was already a variable"
    );
}
