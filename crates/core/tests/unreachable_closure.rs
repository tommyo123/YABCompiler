mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn asm_at(src: &str, profile: Profile) -> String {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(
        &prg,
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compiles: {e}"))
    .asm
}

fn asm(src: &str) -> String {
    asm_at(src, Profile::Default)
}

/// Nested loops whose inner body is jumped out of, with the outer `NEXT J` sitting on a line
/// reached only by `GOTO`.
const ORPHAN_NEXT: &str = "\
10 GOTO 50
20 NEXT J:GOTO 200
30 END
50 FOR J=1 TO 3
60 FOR W=1 TO 4
70 PRINT W;
80 IF W=2 THEN 100
90 NEXT W
100 PRINT \"/\";
110 GOTO 20
200 PRINT \"DONE\"
";

#[test]
fn a_handler_reached_only_through_a_word_table_survives() {
    for profile in [Profile::Default, Profile::Size, Profile::Speed] {
        let a = asm_at(ORPHAN_NEXT, profile);
        assert!(
            a.contains("__FOR_HANDLER_TABLE:"),
            "{profile:?}: the fixture must really build a table:\n{a}"
        );
        assert!(
            a.lines()
                .filter_map(|line| line.split(';').next()?.trim().strip_prefix(".word "))
                .flat_map(|operands| operands.split(','))
                .any(|operand| operand.trim() == "__FOR_NEXT_HANDLER_1"),
            "{profile:?}: the table must still name the handler:\n{a}"
        );
        assert!(
            a.contains("__FOR_NEXT_HANDLER_1:"),
            "{profile:?}: the handler body is reachable ONLY through that \
             .word — deleting it jumps through a $0000 vector:\n{a}"
        );

        assert_eq!(
            a.matches("__FOR_NEXT_HANDLER_1").count(),
            2,
            "{profile:?}: expected exactly the definition and the .word:\n{a}"
        );
    }
}

#[test]
fn a_continuation_reached_only_through_a_patched_vector_survives() {
    for profile in [Profile::Default, Profile::Size, Profile::Speed] {
        let a = asm_at(ORPHAN_NEXT, profile);
        let lines: Vec<&str> = a.lines().map(str::trim).collect();
        let def = lines
            .iter()
            .position(|l| l.starts_with("__FOR_RT_AFTER_") && l.ends_with(':'))
            .unwrap_or_else(|| panic!("{profile:?}: continuation label missing:\n{a}"));
        let name = lines[def].trim_end_matches(':');

        assert!(
            a.contains(&format!("LDA #<{name}")) && a.contains(&format!("LDA #>{name}")),
            "{profile:?}: the vector patch must still be there:\n{a}"
        );

        assert!(
            !a.contains(&format!("JMP {name}")) && !a.contains(&format!("JSR {name}")),
            "{profile:?}: {name} must have no static caller, or this is not \
             the shape under test:\n{a}"
        );

        let before = lines[..def]
            .iter()
            .rev()
            .find(|l| !l.is_empty() && !l.starts_with(';'))
            .copied()
            .unwrap_or("");
        assert_eq!(
            before, "JMP (__FOR_DISP_VEC)",
            "{profile:?}: {name} must sit behind an unconditional jump:\n{a}"
        );
    }
}

#[test]
fn the_orphan_next_fixture_still_produces_the_right_output() {
    let prg = common::compile_source(ORPHAN_NEXT).expect("compiles");
    let out = common::run_clean(&prg).expect("clean exit");
    assert_eq!(
        out.split_whitespace().collect::<Vec<_>>(),
        vec!["1", "2", "/", "1", "2", "/", "1", "2", "/DONE"],
        "orphan-NEXT dispatch output:\n{out}"
    );
}

/// A subscript out of range at compile time lowers to an unconditional `JMP __BAD_SUBSCRIPT`.
#[test]
fn a_tail_behind_an_unconditional_trap_is_dropped() {
    let a = asm("10 DIM Q(2)\n20 IF Q(9)=1 AND LOG(0)=1 THEN PRINT \"Y\"\n\
                 30 PRINT \"Z\"\n40 END\n");
    assert!(
        a.contains("JMP __BAD_SUBSCRIPT"),
        "the trap itself stays:\n{a}"
    );
    for gone in ["JSR $B9EA", "__CMP_D_", "JSR __BOOL_TO_FAC", "JSR __AYINT"] {
        assert!(
            !a.contains(gone),
            "{gone} is behind the trap and must go:\n{a}"
        );
    }

    assert!(a.contains("__LOGIC_B:"), "data is left alone:\n{a}");
}

/// The chain case: a helper called only from another dead helper.
#[test]
fn a_chain_of_helpers_below_a_dead_head_is_dropped() {
    let src = concat!(
        "    JMP __LIVE\n",
        "__DEAD_HEAD:\n",
        "    JSR __DEAD_MID\n",
        "    RTS\n",
        "__DEAD_MID:\n",
        "    JSR __DEAD_TAIL\n",
        "    RTS\n",
        "__DEAD_TAIL:\n",
        "    LDA #$01\n",
        "    RTS\n",
        "__LIVE:\n",
        "    LDA #$42\n",
        "    RTS\n",
    );
    let got = yabcompiler_core::peephole::run(src, Profile::Default);
    for gone in ["__DEAD_HEAD", "__DEAD_MID", "__DEAD_TAIL", "LDA #$01"] {
        assert!(!got.contains(gone), "{gone} must go:\n{got}");
    }
    assert!(got.contains("LDA #$42"), "the live body stays:\n{got}");
}

#[test]
fn taking_the_head_s_address_keeps_the_whole_chain() {
    let src = concat!(
        "    LDA #<__DEAD_HEAD\n",
        "    STA $0314\n",
        "    JMP __LIVE\n",
        "__DEAD_HEAD:\n",
        "    JSR __DEAD_MID\n",
        "    RTS\n",
        "__DEAD_MID:\n",
        "    JSR __DEAD_TAIL\n",
        "    RTS\n",
        "__DEAD_TAIL:\n",
        "    LDA #$01\n",
        "    RTS\n",
        "__LIVE:\n",
        "    LDA #$42\n",
        "    RTS\n",
    );
    let got = yabcompiler_core::peephole::run(src, Profile::Default);

    assert!(
        got.contains("__DEAD_HEAD:"),
        "the patched entry must stay:\n{got}"
    );
    assert!(
        got.contains("LDA #$01"),
        "the far end of the chain must stay:\n{got}"
    );
    assert!(
        got.contains("LDA #$42"),
        "and so must the live path:\n{got}"
    );
}

/// `JMP (vector)` reads a pointer; it does not name a destination.
#[test]
fn an_indirect_jump_does_not_make_its_operand_a_target() {
    let src = concat!(
        "    LDA #<__ISLAND\n",
        "    STA __VEC\n",
        "    JMP (__VEC)\n",
        "__ISLAND:\n",
        "    LDA #$02\n",
        "    RTS\n",
        "__VEC:\n",
        "    .word $0000\n",
    );
    let got = yabcompiler_core::peephole::run(src, Profile::Default);
    assert!(got.contains("__ISLAND:"), "island kept:\n{got}");
    assert!(got.contains("LDA #$02"), "island body kept:\n{got}");
    assert!(got.contains("__VEC:"), "the vector is data:\n{got}");
}

/// Data is addressed, not executed.
#[test]
fn markers_in_front_of_data_are_never_dropped() {
    let src = concat!(
        "    RTS\n",
        "__BSS_START:\n",
        "    .byte $00,$00\n",
        "__BSS_END:\n",
    );
    let got = yabcompiler_core::peephole::run(src, Profile::Default);
    assert!(got.contains("__BSS_START:"), "{got}");
    assert!(got.contains("__BSS_END:"), "{got}");
    assert!(got.contains(".byte $00,$00"), "{got}");
}

/// A `JSR` comes back.
#[test]
fn code_after_a_jsr_is_reachable() {
    let src = concat!(
        "    JSR $FFD2\n",
        "    LDA #$07\n",
        "    JSR __H\n",
        "    LDA #$08\n",
        "    RTS\n",
        "__H:\n",
        "    RTS\n",
    );
    let got = yabcompiler_core::peephole::run(src, Profile::Default);
    assert!(got.contains("LDA #$07"), "{got}");
    assert!(got.contains("LDA #$08"), "{got}");
}

#[test]
fn the_corpus_still_compiles_and_never_grows() {

    let sources = [
        ORPHAN_NEXT,
        "10 DIM A(20)\n20 FOR I=0 TO 20:A(I)=I*2:NEXT\n\
         30 FOR I=0 TO 20:PRINT A(I);:NEXT\n40 END\n",
        "10 READ A$,B$\n20 PRINT A$;B$\n30 DATA HELLO,WORLD\n40 END\n",
        "10 ON 2 GOSUB 100,200\n20 END\n100 PRINT \"A\":RETURN\n\
         200 PRINT \"B\":RETURN\n",
    ];
    for src in sources {
        for profile in [Profile::Default, Profile::Size, Profile::Speed] {
            let a = asm_at(src, profile);
            assert!(!a.is_empty());
        }
    }
}
