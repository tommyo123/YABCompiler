//! CURLIN's low byte ($39): the per-line delta encoding, and the set of lines it may not be
//! held across.

mod common;

use yabcompiler_core::{
    CompileOptions, Dialect, Profile, compile_with_options, tokenize_program, tokenize_program_with,
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
        a.contains("__CGOTO_NEXT_"),
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
        !a.contains("__CGOTO_NEXT_"),
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
fn poking_curlin_disables_the_delta_encoding() {
    let a = asm("10 POKE 58,0\n\
         11 PRINT 1/1\n\
         12 PRINT 2/1\n\
         13 PRINT 3/1\n");
    assert!(!a.contains("INC $39"), "POKE 58 is a CURLIN write:\n{a}");
}

#[test]
fn the_delta_encoding_is_what_makes_the_run_cheaper() {
    fn size(src: &str) -> usize {
        let prg = tokenize_program(src).expect("tokenizes");
        compile_with_options(&prg, CompileOptions::default())
            .expect("compiles")
            .machine_code
            .len()
    }
    let run = size(
        "10 Z=0\n11 PRINT 1/1\n12 PRINT 1/1\n13 PRINT 1/1\n\
         14 PRINT 1/1\n15 PRINT 1/Z\n",
    );
    let tens = size(
        "10 Z=0\n20 PRINT 1/1\n30 PRINT 1/1\n40 PRINT 1/1\n\
         50 PRINT 1/1\n60 PRINT 1/Z\n",
    );
    assert_eq!(
        tens - run,
        8,
        "four +1 deltas at 2 bytes each: run={run} tens={tens}"
    );
}
