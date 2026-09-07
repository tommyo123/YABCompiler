mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

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
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let (bytes, _) = build(src, profile);
        let got = common::run_clean(&bytes)
            .unwrap_or_else(|| panic!("runs to a clean exit ({profile:?}):\n{src}"));
        assert_eq!(got, expected, "{profile:?} output mismatch for:\n{src}");
    }
}

fn check_stops_with(src: &str, expected: &str) {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let (bytes, _) = build(src, profile);
        let r = emu64::run_prg_to_end(&bytes, 6_000_000).expect("runs");
        let got = common::petscii_to_string(&r.output);
        assert_eq!(got, expected, "{profile:?} output mismatch for:\n{src}");
        assert!(
            !r.clean_exit,
            "{profile:?}: the BASIC error must abort the program:\n{src}"
        );
    }
}

/// Every `__HEAP_PTR` reference must be backed by a definition *and* by a prologue that
/// initialises it.
fn assert_heap_pointer_is_sound(asm: &str, label: &str) {
    if !asm.contains("__HEAP_PTR") {
        return;
    }
    assert!(
        asm.contains("__HEAP_PTR:"),
        "{label}: reads __HEAP_PTR but never defines it"
    );
    assert!(
        asm.contains("JSR __HEAP_INIT"),
        "{label}: reads __HEAP_PTR but the prologue never initialises it — \
         the allocator would hand out chunks at $0000"
    );
    assert!(
        asm.contains("__HEAP_INIT:"),
        "{label}: calls __HEAP_INIT but never defines it"
    );
}

/// Every `T<n>` operand must have a matching `T<n>:` definition.
fn assert_temp_slots_are_defined(asm: &str, label: &str) {
    let mut referenced = Vec::new();
    let mut defined = Vec::new();
    for line in asm.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix('T')
            && let Some(n) = rest.strip_suffix(':')
            && n.chars().all(|c| c.is_ascii_digit())
            && !n.is_empty()
        {
            defined.push(n.parse::<usize>().unwrap());
            continue;
        }
        for (i, _) in t.match_indices('T') {
            let before = t[..i].chars().last();
            if matches!(before, Some(c) if c.is_ascii_alphanumeric() || c == '_') {
                continue;
            }
            let digits: String = t[i + 1..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if digits.is_empty() {
                continue;
            }
            referenced.push(digits.parse::<usize>().unwrap());
        }
    }
    for r in &referenced {
        assert!(
            defined.contains(r),
            "{label}: T{r} is referenced but never defined \
             (defined: {defined:?})"
        );
    }
}

// The silent half: the heap pointer existed but nothing ever set it.

#[test]
fn on_goto_over_val_mid_initialises_the_heap() {
    let src = "10 V$=\"21\":I=1\n\
               20 PRINT I;:I=I+1:IF I>2 THEN END\n\
               30 ON VAL(MID$(V$,I-1,1)) GOTO 20,20\n";
    check_all_profiles(src, " 1  2 ");
    assert_heap_pointer_is_sound(&asm_of(src), "on_goto_over_val_mid");
}

#[test]
fn on_goto_over_val_mid_into_an_orphan_next() {
    let src = "5 V$=\"21212\":GOTO 20\n\
               6 NEXT:GOTO 30\n\
               8 NEXT:GOTO 30\n\
               20 FOR I=1 TO 5\n\
               25 PRINT I;\n\
               30 IF I>5 THEN PRINT \"v\":END\n\
               35 ON VAL(MID$(V$,I,1)) GOTO 6,8\n";
    check_all_profiles(src, " 1  2  3  4  5 V\n");
    assert_heap_pointer_is_sound(&asm_of(src), "on_goto_over_val_mid_orphan_next");
}

#[test]
fn on_goto_over_chr_defines_and_initialises_the_heap_pointer() {
    let src = "10 FOR I=1 TO 2\n\
               20 ON LEN(CHR$(48+I)) GOTO 30\n\
               30 NEXT\n\
               40 PRINT \"z\"\n";
    check_all_profiles(src, "Z\n");
    assert_heap_pointer_is_sound(&asm_of(src), "on_goto_over_chr");
}

#[test]
fn on_goto_over_asc_chr_dispatch_chain() {
    let src = "5 GOTO 20\n\
               10 NEXT:GOTO 30\n\
               12 NEXT:GOTO 30\n\
               20 FOR I=1 TO 4\n\
               25 PRINT CHR$(48+I);\n\
               30 IF I>4 THEN PRINT \"c\":END\n\
               35 ON (ASC(CHR$(48+I)) AND 1)+1 GOTO 10,12\n";
    check_all_profiles(src, "1234C\n");
    assert_heap_pointer_is_sound(&asm_of(src), "on_goto_over_asc_chr");
}

#[test]
fn on_gosub_over_chr_initialises_the_heap() {
    let src = "10 FOR I=1 TO 2\n\
               20 ON LEN(CHR$(48+I)) GOSUB 50\n\
               30 NEXT\n\
               40 END\n\
               50 PRINT \"g\";:RETURN\n";
    check_all_profiles(src, "GG");
    assert_heap_pointer_is_sound(&asm_of(src), "on_gosub_over_chr");
}

// The temp-slot half.

#[test]
fn a_runtime_for_handler_gets_its_temp_slot() {
    let src = "5 DATA 1,2,3\n\
               10 FOR I=1 TO 3:READ A:PRINT A;:IF I=2 THEN CLR\n\
               20 NEXT\n";
    check_stops_with(src, " 1  2 ");
    let asm = asm_of(src);
    assert!(
        asm.contains("__FOR_NEXT_HANDLER_"),
        "the shape under test needs a runtime handler to exist at all"
    );
    assert_temp_slots_are_defined(&asm, "a_runtime_for_handler_gets_its_temp_slot");
}

/// The shapes above plus a spread of other statement forms that can host a heap-allocating
/// expression.
#[test]
fn the_heap_and_temp_invariants_hold_across_shapes() {
    let cases: &[(&str, &str)] = &[
        (
            "on_goto_chr",
            "10 FOR I=1 TO 2\n20 ON LEN(CHR$(48+I)) GOTO 30,30\n30 NEXT\n",
        ),
        (
            "on_goto_chr_single_target",
            "10 FOR I=1 TO 2\n20 ON LEN(CHR$(48+I)) GOTO 30\n30 NEXT\n",
        ),
        (
            "on_goto_chr_no_for",
            "10 I=1\n20 ON LEN(CHR$(48+I)) GOTO 30,30\n30 I=I+1:IF I<3 THEN 20\n",
        ),
        (
            "on_goto_asc_chr",
            "10 FOR I=1 TO 2\n20 PRINT CHR$(48+I);\n30 ON ASC(CHR$(48+I)) GOTO 40,40\n40 NEXT\n",
        ),
        (
            "on_goto_val_mid_literal",
            "10 FOR I=1 TO 2\n20 ON VAL(MID$(\"21\",I,1)) GOTO 30,30\n30 NEXT\n40 PRINT \"q\"\n",
        ),
        (
            "on_goto_val_left",
            "10 FOR I=1 TO 2\n20 ON LEN(LEFT$(\"AB\",I)) GOTO 30,30\n30 NEXT\n",
        ),
        (
            "on_goto_str",
            "10 FOR I=1 TO 2\n20 ON LEN(STR$(I)) GOTO 30,30\n30 NEXT\n",
        ),
        (
            "on_gosub_chr",
            "10 FOR I=1 TO 2\n20 ON LEN(CHR$(48+I)) GOSUB 50\n30 NEXT\n40 END\n50 RETURN\n",
        ),
        (
            "chr_in_a_def_fn_body",
            "10 DEF FN F(X)=LEN(CHR$(48+X))\n20 FOR I=1 TO 2:PRINT FN F(I);:NEXT\n",
        ),
        (
            "runtime_dim_over_chr",
            "10 N=2\n20 DIM A(LEN(CHR$(64+N)))\n30 PRINT \"d\"\n",
        ),
        (
            "clr_inside_a_read_loop",
            "5 DATA 1,2,3\n10 FOR I=1 TO 3:READ A:PRINT A;:IF I=2 THEN CLR\n20 NEXT\n",
        ),
        (
            "orphan_next_with_a_float_step",
            "5 GOTO 20\n10 NEXT:GOTO 30\n20 FOR I=1 TO 2 STEP 0.5\n25 PRINT I;\n\
             30 IF I>2 THEN END\n35 ON 1 GOTO 10\n",
        ),
    ];
    for (name, src) in cases {
        for profile in [Profile::Default, Profile::Speed, Profile::Size] {
            let (_, asm) = build(src, profile);
            let label = format!("{name} ({profile:?})");
            assert_heap_pointer_is_sound(&asm, &label);
            assert_temp_slots_are_defined(&asm, &label);
        }
    }
}

#[test]
fn the_prewalk_keeps_compilation_deterministic() {
    for src in [
        "10 FOR I=1 TO 2\n20 ON LEN(CHR$(48+I)) GOTO 30,30\n30 NEXT\n",
        "5 DATA 1,2,3\n10 FOR I=1 TO 3:READ A:PRINT A;:IF I=2 THEN CLR\n20 NEXT\n",
    ] {
        let (a, asm_a) = build(src, Profile::Default);
        let (b, asm_b) = build(src, Profile::Default);
        assert_eq!(a, b, "two compiles differ:\n{src}");
        assert_eq!(asm_a, asm_b, "two compiles differ in asm:\n{src}");
    }
}

/// A plain numeric loop has no string in it anywhere.
#[test]
fn a_heap_free_program_still_skips_the_heap_prologue() {
    let asm = asm_of("10 FOR I=1 TO 3:PRINT I:NEXT\n");
    assert!(
        !asm.contains("__HEAP"),
        "a heap-free program must not pay for the heap:\n{asm}"
    );
}

#[test]
fn an_on_goto_without_a_heap_expression_still_skips_the_heap_prologue() {
    let asm = asm_of("10 I=1\n20 ON I GOTO 30,40\n30 PRINT \"a\":END\n40 PRINT \"b\"\n");
    assert!(
        !asm.contains("__HEAP"),
        "ON over a plain variable must not force heap init:\n{asm}"
    );
}

/// A string *literal* PRINT is served straight out of the string pool and never allocates.
#[test]
fn literal_only_string_work_still_skips_the_heap_prologue() {
    for src in [
        "10 PRINT \"hello\"\n",
        "10 ON 1 GOTO 20,20\n20 PRINT \"a\"\n",
        "10 ON LEN(CHR$(65)) GOTO 20\n20 PRINT \"a\"\n",
    ] {
        let asm = asm_of(src);
        assert!(
            !asm.contains("JSR __HEAP_INIT"),
            "must not force heap init:\n{src}\n{asm}"
        );
    }
}

#[test]
fn the_temp_slot_block_is_still_omitted_when_nothing_needs_one() {
    for src in [
        "10 FOR I=1 TO 3:PRINT I:NEXT\n",
        // Orphan NEXT -> runtime handler, but an integer-narrowed loop whose handler is
        // `emit_next_int` and needs no temp.
        "5 GOTO 20\n10 NEXT:GOTO 30\n20 FOR I=1 TO 3\n25 PRINT I;\n\
         30 IF I>3 THEN END\n35 ON 1 GOTO 10\n",
    ] {
        let asm = asm_of(src);
        assert!(
            !asm.contains("\nT0:"),
            "no temp is needed here, so no slot should be emitted:\n{src}\n{asm}"
        );
    }
}

#[test]
fn the_prewalk_does_not_inflate_the_temp_count() {
    let src = "5 DATA 1,2,3\n\
               10 FOR I=1 TO 3:READ A:PRINT A;:IF I=2 THEN CLR\n\
               20 NEXT\n";
    let asm = asm_of(src);
    let defined = asm
        .lines()
        .filter(|l| {
            let t = l.trim();
            t.starts_with('T')
                && t.ends_with(':')
                && t[1..t.len() - 1].chars().all(|c| c.is_ascii_digit())
                && t.len() > 2
        })
        .count();
    assert_eq!(
        defined, 1,
        "one handler temp, reused — not one slot per emission pass:\n{asm}"
    );
}

#[test]
fn an_ordinary_loop_still_keeps_the_static_next() {
    let asm = asm_of("10 FOR I=1 TO 10:PRINT I;:NEXT\n");
    assert!(
        !asm.contains("__FOR_NEXT_HANDLER_"),
        "a statically paired NEXT must not acquire a runtime handler:\n{asm}"
    );
    assert!(
        !asm.contains("__FOR_STACK"),
        "a statically paired NEXT must not acquire the runtime frame stack:\n{asm}"
    );
}
