//! Tokeniser and parser fidelity against the real Commodore 64 ROM.

mod common;

use yabcompiler_core::{
    CompileOptions, Dialect, compile_with_options, tokenize_program, tokenize_program_with,
};

/// Compile `src` (plain ASCII BASIC), run it, return the PRINT output.
fn run(src: &str) -> String {
    let prg = common::compile_source(src).unwrap_or_else(|e| panic!("compile failed: {e}\n{src}"));
    let r = emu64::run_prg_to_end(&prg, 20_000_000).expect("runs");
    assert!(r.clean_exit, "program must reach the exit RTS:\n{src}");
    common::petscii_to_string(&r.output)
}
fn run_prg(body_lines: &[(u16, &[u8])]) -> String {
    let prg = build_prg(body_lines);
    let out = common::compile_prg(&prg).unwrap_or_else(|e| panic!("compile failed: {e}"));
    let r = emu64::run_prg_to_end(&out, 20_000_000).expect("runs");
    assert!(r.clean_exit, "program must reach the exit RTS");
    common::petscii_to_string(&r.output)
}

/// PRINT output split into lines with trailing blanks removed.
fn lines(out: &str) -> Vec<String> {
    out.lines().map(|l| l.trim_end().to_string()).collect()
}
fn build_prg(body_lines: &[(u16, &[u8])]) -> Vec<u8> {
    let mut out = vec![0x01, 0x08];
    let mut addr: u16 = 0x0801;
    for (number, body) in body_lines {
        let next = addr + 2 + 2 + body.len() as u16 + 1;
        out.extend_from_slice(&next.to_le_bytes());
        out.extend_from_slice(&number.to_le_bytes());
        out.extend_from_slice(body);
        out.push(0);
        addr = next;
    }
    out.extend_from_slice(&[0, 0]);
    out
}

/// The line bodies of a tokenized program, as `yabcompiler tokenize` produces them.
fn tokenized_bodies(src: &str) -> Vec<Vec<u8>> {
    bodies_of(&tokenize_program(src).expect("tokenizes"))
}

fn bodies_of(prg: &[u8]) -> Vec<Vec<u8>> {
    let p = yabcompiler_core::Program::parse(prg).expect("parses");
    p.lines.into_iter().map(|l| l.body).collect()
}

#[test]
fn t1_integer_literal_absorbs_embedded_spaces() {
    assert_eq!(run("10 A=1 2\n20 PRINT A\n30 END\n").trim(), "12");
}

#[test]
fn t1_decimal_point_and_digits_may_be_spaced_apart() {
    // `10 A=1 . 5 : PRINT A` prints 1.5 on hardware.
    assert_eq!(run("10 A=1 . 5\n20 PRINT A\n30 END\n").trim(), "1.5");
}

#[test]
fn t1_scientific_exponent_is_space_tolerant() {
    // Hardware: `A=1 e 3` is 1000, `B=2 5 e 2` is 2500.
    let out = run("10 A=1 E 3\n20 B=2 5 E 2\n30 PRINT A;B\n40 END\n");
    assert_eq!(out.split_whitespace().collect::<Vec<_>>(), ["1000", "2500"]);
}

#[test]
fn t1_spaced_digits_bind_before_operators() {
    let out = run("10 A=1 2 +3 4\n20 B=1 2 * 2\n30 PRINT A;B\n40 END\n");
    assert_eq!(out.split_whitespace().collect::<Vec<_>>(), ["46", "24"]);
}

#[test]
fn t1_negative_literal_absorbs_the_space_too() {
    assert_eq!(run("10 A=-1 2\n20 PRINT A\n30 END\n").trim(), "-12");
}

#[test]
fn t1_poke_address_may_be_spaced() {
    let out = run("10 POKE 8 2 8,7\n20 PRINT PEEK(8 2 8)\n30 END\n");
    assert_eq!(out.trim(), "7");
}

#[test]
fn t1_trailing_space_is_not_swallowed_into_the_number() {
    // `PRINT 1 A` is two PRINT items on hardware (prints " 1  5"), not the number 1 followed by
    // nothing.
    let out = run("10 A=5\n20 PRINT 1 A\n30 END\n");
    assert_eq!(out.split_whitespace().collect::<Vec<_>>(), ["1", "5"]);
}

#[test]
fn t1_lone_trailing_space_still_terminates_the_literal() {
    let out = run("10 A=1 \n20 B= 2 \n30 PRINT A;B\n40 END\n");
    assert_eq!(out.split_whitespace().collect::<Vec<_>>(), ["1", "2"]);
}

#[test]
fn t1_bare_and_trailing_decimal_points_keep_v2_values() {
    // Hardware: `A=.` is 0, `B=1 .` is 1.
    let out = run("10 A=.\n20 B=1 .\n30 PRINT A;B\n40 END\n");
    assert_eq!(out.split_whitespace().collect::<Vec<_>>(), ["0", "1"]);
}

#[test]
fn t1_trailing_e_is_an_empty_exponent_not_a_variable() {
    let out = run("10 E=7\n20 PRINT 1 0 E\n30 PRINT 5 E\n40 A=100E\n50 PRINT A\n60 END\n");
    assert_eq!(
        out.split_whitespace().collect::<Vec<_>>(),
        ["10", "5", "100"]
    );
}

#[test]
fn t1_well_formed_exponents_are_untouched() {
    let out = run("10 PRINT 1E2;1E3;1E10\n20 END\n");
    assert_eq!(
        out.split_whitespace().collect::<Vec<_>>(),
        ["100", "1000", "1E+10"]
    );
}

#[test]
fn t2_goto_target_absorbs_embedded_spaces() {
    // `10 GOTO 1 0 0` jumps to line 100 on hardware.
    let out = run("10 GOTO 1 0 0\n20 PRINT \"WRONG\"\n100 PRINT \"RIGHT\"\n110 END\n");
    assert_eq!(out.trim(), "RIGHT");
}

#[test]
fn t2_gosub_target_absorbs_embedded_spaces() {
    let out = run("10 GOSUB 1 0 0\n20 PRINT \"BACK\"\n30 END\n100 PRINT \"SUB\"\n110 RETURN\n");
    assert_eq!(out.split_whitespace().collect::<Vec<_>>(), ["SUB", "BACK"]);
}

#[test]
fn t2_if_then_line_target_absorbs_embedded_spaces() {
    let out = run("10 IF 1 THEN 1 0 0\n20 PRINT \"WRONG\"\n100 PRINT \"RIGHT\"\n110 END\n");
    assert_eq!(out.trim(), "RIGHT");
}

#[test]
fn t2_on_goto_targets_absorb_embedded_spaces() {
    // Hardware picks line 200 for X=2.
    let out = run("10 X=2\n20 ON X GOTO 1 0 0,2 0 0\n30 PRINT \"WRONG\"\n\
         100 PRINT \"ONE\":END\n200 PRINT \"TWO\"\n210 END\n");
    assert_eq!(out.trim(), "TWO");
}

#[test]
fn t2_plain_target_followed_by_colon_still_parses() {
    let out = run("10 GOTO 100 \n20 PRINT \"WRONG\"\n100 PRINT \"OK\":END\n");
    assert_eq!(out.trim(), "OK");
}

#[test]
fn t3_colon_inside_a_quoted_data_item_does_not_end_data() {
    let bodies = tokenized_bodies("10 data \"hello:world\",  spaced  ,\"q\"\n");
    assert_eq!(
        bodies[0],
        b"\x83 \"HELLO:WORLD\",  SPACED  ,\"Q\"".to_vec(),
        "tokenized DATA line does not match the ROM's own bytes"
    );
}

#[test]
fn t3_quoted_data_item_reads_back_with_its_colon() {
    let out = run("10 DATA \"hello:world\",  spaced  ,\"q\"\n\
         20 FOR I=1 TO 3:READ A$:PRINT \"<\";A$;\">\":NEXT\n30 END\n");
    // Native prints <HELLO:WORLD> <SPACED  > <Q>.
    assert_eq!(lines(&out), ["<HELLO:WORLD>", "<SPACED  >", "<Q>"]);
}

#[test]
fn t3_statement_after_a_quoted_data_item_is_still_crunched() {
    let bodies = tokenized_bodies("20 data \"x:y\":print \"z\"\n");
    assert_eq!(bodies[0], b"\x83 \"X:Y\":\x99 \"Z\"".to_vec());
}

#[test]
fn t3_unquoted_colon_still_ends_data_scope() {
    let bodies = tokenized_bodies("10 data one,two:print \"after\"\n");
    assert_eq!(bodies[0], b"\x83 ONE,TWO:\x99 \"AFTER\"".to_vec());
}

#[test]
fn t3_unquoted_data_words_are_not_crunched() {
    let bodies = tokenized_bodies("10 data boring,anders\n");
    assert_eq!(bodies[0], b"\x83 BORING,ANDERS".to_vec());
}

#[test]
fn t4_empty_data_yields_one_empty_item() {
    let out =
        run("10 DATA\n20 READ A$\n30 PRINT \"LEN=\";LEN(A$)\n40 PRINT \"<\";A$;\">\"\n50 END\n");
    assert_eq!(lines(&out), ["LEN= 0", "<>"]);
}

#[test]
fn t4_empty_data_does_not_swallow_the_next_data_statement() {
    // Hardware: A$ is "" from line 10 and B is 5 from line 20.
    let out = run("10 DATA\n20 DATA 5\n30 READ A$:PRINT \"A<\";A$;\">\"\n\
         40 READ B:PRINT \"B\";B\n50 END\n");
    assert_eq!(lines(&out), ["A<>", "B 5"]);
}

#[test]
fn t4_data_followed_by_a_colon_still_runs_the_next_statement() {
    // `10 DATA :PRINT "AFTER"` prints AFTER *and* leaves one empty item.
    let out = run("10 DATA :PRINT \"AFTER\"\n20 READ A$:PRINT \"<\";A$;\">\"\n30 END\n");
    assert_eq!(lines(&out), ["AFTER", "<>"]);
}

#[test]
fn t4_empty_data_survives_restore() {
    let out = run("10 DATA\n20 READ A$:PRINT \"1<\";A$;\">\"\n30 RESTORE\n\
         40 READ B$:PRINT \"2<\";B$;\">\"\n50 END\n");
    assert_eq!(lines(&out), ["1<>", "2<>"]);
}

#[test]
fn t4_populated_data_is_unchanged() {
    let out = run("10 DATA 1,2,3\n20 FOR I=1 TO 3:READ A:PRINT A;:NEXT\n30 END\n");
    assert_eq!(out.split_whitespace().collect::<Vec<_>>(), ["1", "2", "3"]);
}

#[test]
fn t4_trailing_comma_still_adds_exactly_one_empty_item() {
    let out = run("10 DATA X,\n20 READ A$:PRINT \"1<\";A$;\">\"\n\
         30 READ B$:PRINT \"2<\";B$;\">\"\n40 END\n");
    assert_eq!(lines(&out), ["1<X>", "2<>"]);
}

#[test]
fn t4_spaced_number_in_data_keeps_both_readings() {
    let out = run("10 DATA 1 2,1 2\n20 READ A$:PRINT \"<\";A$;\">\"\n30 READ B:PRINT B\n40 END\n");
    assert_eq!(lines(&out), ["<1 2>", " 12"]);
}

#[test]
fn t5_empty_program_compiles() {
    // `tokenize_program("")` is the 4-byte file `01 08 00 00`: load address plus the
    // end-of-program link.
    let prg = tokenize_program("").expect("tokenizes");
    assert_eq!(prg, vec![0x01, 0x08, 0x00, 0x00]);
    let out = compile_with_options(&prg, CompileOptions::default())
        .unwrap_or_else(|e| panic!("empty program must compile: {e}"));
    let r = emu64::run_prg_to_end(&out.prg_bytes, 100_000).expect("runs");
    assert!(r.clean_exit, "empty program must reach the exit RTS");
    assert!(r.output.is_empty(), "empty program must print nothing");
}

#[test]
fn t5_bare_load_address_is_also_an_empty_program() {
    let out = match compile_with_options(&[0x01, 0x08], CompileOptions::default()) {
        Ok(c) => c,
        Err(e) => panic!("2-byte .prg is an empty BASIC program: {e}"),
    };
    assert!(!out.prg_bytes.is_empty());
}

#[test]
fn t5_empty_programs_neighbours_still_compile() {
    for src in ["10 REM\n", "10 END\n", "10\n", "10 :\n", "10 ::\n"] {
        let prg = tokenize_program(src).expect("tokenizes");
        compile_with_options(&prg, CompileOptions::default())
            .unwrap_or_else(|e| panic!("{src:?} must compile: {e}"));
    }
}

#[test]
fn t5_machine_language_prg_is_still_rejected() {
    // A $C000 image whose "line links" are arbitrary opcodes.
    let ml = [
        0x00, 0xC0, 0xA9, 0x00, 0x8D, 0x20, 0xD0, 0xA2, 0xFF, 0x9A, 0x4C, 0x00, 0xC0, 0xEA, 0xEA,
        0xEA, 0xEA, 0x60,
    ];
    let msg = match compile_with_options(&ml, CompileOptions::default()) {
        Ok(_) => panic!("machine code must still be refused"),
        Err(e) => e.to_string(),
    };
    assert!(
        msg.contains("not a BASIC program"),
        "wrong diagnostic: {msg}"
    );
}

#[test]
fn t6_tokenizer_leaves_assigned_extension_words_as_letters() {
    // `LINE=5` is a BASIC v2 assignment to the variable `LI`.
    let bodies = tokenized_bodies("10 line=5\n20 print line\n");
    assert_eq!(bodies[0], b"LINE\xB2\x35".to_vec());
    assert_eq!(bodies[1], b"\x99 LINE".to_vec());
}

#[test]
fn t6_all_nine_reported_words_round_trip_as_variables() {
    for (word, tok_free) in [
        ("USE", &b"USE\xB2\x39"[..]),
        ("MOD", &b"MOD\xB2\x39"[..]),
        ("DIV", &b"DIV\xB2\x39"[..]),
        ("KEY", &b"KEY\xB2\x39"[..]),
        ("LINE", &b"LINE\xB2\x39"[..]),
        ("DO", &b"DO\xB2\x39"[..]),
        ("MAP", &b"MAP\xB2\x39"[..]),
        ("DIR", &b"DIR\xB2\x39"[..]),
        ("OUT", &b"OUT\xB2\x39"[..]),
    ] {
        let bodies = tokenized_bodies(&format!("10 {word}=9\n"));
        assert_eq!(bodies[0], tok_free.to_vec(), "{word} was crunched");
    }
}

#[test]
fn t6_demoted_word_still_meets_the_basic_v2_cruncher() {
    // `EXOR` is an extension keyword, but its letters contain the v2 keyword `OR`.
    let bodies = tokenized_bodies("10 exor=5\n");
    assert_eq!(bodies[0], b"EX\xB0\xB2\x35".to_vec());
}

#[test]
fn t6_demoted_word_does_not_expose_a_shorter_keyword() {
    let bodies = tokenized_bodies("10 line=5\n");
    assert!(!bodies[0].contains(&0x64), "extension prefix leaked in");
}

#[test]
fn t6_parser_reads_rom_bytes_for_those_words_as_variables() {
    let out = run_prg(&[
        (10, b"MOD\xB2\x33"),
        (20, b"\x99 MOD"),
        (30, b"\x80"), // END
    ]);
    assert_eq!(out.trim(), "3");
}

#[test]
fn t6_use_in_expression_position_is_a_variable_not_the_constant_8() {
    let out = run_prg(&[(10, b"USE\xB2\x39"), (20, b"\x99 USE"), (30, b"\x80")]);
    assert_eq!(out.trim(), "9");
}

#[test]
fn t6_div_map_dir_out_share_v2_two_character_names() {
    // BASIC v2 only distinguishes the first two characters, so DIV and DIR are the *same*
    // variable `DI`.
    let out = run_prg(&[
        (10, b"DIV\xB2\x31:MAP\xB2\x32:DIR\xB2\x33:OUT\xB2\x34"),
        (20, b"\x99 DIV;MAP;DIR;OUT"),
        (30, b"\x80"),
    ]);
    assert_eq!(
        out.split_whitespace().collect::<Vec<_>>(),
        ["3", "2", "3", "4"]
    );
}

#[test]
fn t6_extension_keywords_still_crunch_when_not_used_as_variables() {
    let bodies = tokenized_bodies("10 cls\n20 color 0,0,1\n30 print mod(10,3)\n");
    assert_eq!(bodies[0], vec![0x64, 0x3C]);
    assert_eq!(bodies[1], b"\x64\x1A 0,0,1".to_vec());
    assert_eq!(bodies[2], b"\x99 \x64\x52(10,3)".to_vec());
}

#[test]
fn t6_demotion_is_per_word_not_wholesale() {
    let bodies = tokenized_bodies("10 line=5\n20 cls\n");
    assert_eq!(bodies[0], b"LINE\xB2\x35".to_vec());
    assert_eq!(bodies[1], vec![0x64, 0x3C]);
}

#[test]
fn t6_tokenized_extension_program_still_parses_its_ascii_words() {
    let prg = build_prg(&[
        (10, &[0x64, 0x3C]),          // CLS, tokenized
        (20, b"\x99 \x64\x52(10,3)"), // PRINT MOD(10,3), tokenized
        (30, b"\x99 MOD(10,3)"),      // PRINT MOD(10,3), ASCII
        (40, b"\x80"),
    ]);
    let out = common::compile_prg(&prg).expect("mixed extension program compiles");
    let r = emu64::run_prg_to_end(&out, 20_000_000).expect("runs");
    let text = common::petscii_to_string(&r.output);
    assert_eq!(text.split_whitespace().collect::<Vec<_>>(), ["1", "1"]);
}

#[test]
fn t6_extension_still_runs_end_to_end() {
    let out = run("10 PRINT MOD(10,3);DIV(25,4)\n20 END\n");
    assert_eq!(out.split_whitespace().collect::<Vec<_>>(), ["1", "6"]);
}

#[test]
fn t6_forced_dialects_override_the_automatic_choice() {
    let v2 = bodies_of(&tokenize_program_with("10 cls\n", Dialect::BasicV2).expect("tokenizes"));
    assert_eq!(v2[0], b"CLS".to_vec());
    let tsb = bodies_of(&tokenize_program_with("10 line=5\n", Dialect::Tsb).expect("tokenizes"));
    assert_eq!(tsb[0], b"\x64\x03\xB2\x35".to_vec());
}

#[test]
fn t6_basic2_dialect_matches_the_rom_for_ambiguous_sources() {
    // `PRINT USE` / `PRINT MOD(2)` carry no assignment for `Auto` to key on, so they stay
    // extension keywords by default.
    let v2 = bodies_of(
        &tokenize_program_with("10 print use\n20 print mod(2)\n", Dialect::BasicV2)
            .expect("tokenizes"),
    );
    assert_eq!(v2[0], b"\x99 USE".to_vec());
    assert_eq!(v2[1], b"\x99 MOD(2)".to_vec());
}
