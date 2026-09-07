//! The PRINT gap column is channel-dependent, and the compiler must track the channel.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const MAX_INSNS: u64 = 80_000_000;

fn raw_output(src: &str) -> Vec<u8> {
    let prg = common::compile_source(src).expect("compiles");
    let r = emu64::run_prg_to_end(&prg, MAX_INSNS).expect("runs");
    assert!(r.clean_exit, "program must reach a clean exit:\n{src}");
    r.output
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

/// The body of a labelled helper, up to (not including) the next label.
fn helper_body<'a>(asm: &'a str, label: &str) -> &'a str {
    let after = asm
        .split(&format!("\n{label}:\n"))
        .nth(1)
        .unwrap_or_else(|| panic!("{label} is emitted:\n{asm}"));
    match after.find("\n_") {
        Some(i) => &after[..i],
        None => after,
    }
}

/// Longest run of `byte` in `out`.
fn longest_run(out: &[u8], byte: u8) -> usize {
    let (mut best, mut cur) = (0, 0);
    for &b in out {
        cur = if b == byte { cur + 1 } else { 0 };
        best = best.max(cur);
    }
    best
}

const ALL_PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

#[test]
fn comma_pads_with_spaces_under_cmd() {
    let out = raw_output("10 OPEN 1,3\n20 CMD 1\n30 PRINT \"X\",\"Y\"\n40 PRINT#1\n50 CLOSE 1\n");
    assert_eq!(
        longest_run(&out, 0x20),
        9,
        "comma zone under CMD must pad with nine spaces: {out:02X?}"
    );
    assert_eq!(
        longest_run(&out, 0x1D),
        0,
        "no cursor-right may reach a redirected channel: {out:02X?}"
    );
}

/// Native `tab(10)` after one printed character, under CMD: nine `$20`.
#[test]
fn tab_pads_with_spaces_under_cmd() {
    let out =
        raw_output("10 OPEN 1,3\n20 CMD 1\n30 PRINT \"X\";TAB(10);\"Y\"\n40 PRINT#1\n50 CLOSE 1\n");
    assert_eq!(
        longest_run(&out, 0x20),
        9,
        "TAB( under CMD must pad with spaces: {out:02X?}"
    );
    assert_eq!(longest_run(&out, 0x1D), 0, "{out:02X?}");
}

/// Native `spc(5)` under CMD: five `$20`.
#[test]
fn spc_pads_with_spaces_under_cmd() {
    let out =
        raw_output("10 OPEN 1,3\n20 CMD 1\n30 PRINT \"X\";SPC(5);\"Y\"\n40 PRINT#1\n50 CLOSE 1\n");
    assert_eq!(
        longest_run(&out, 0x20),
        5,
        "SPC( under CMD must pad with spaces: {out:02X?}"
    );
    assert_eq!(longest_run(&out, 0x1D), 0, "{out:02X?}");
}

/// The trailing gap after a number.
#[test]
fn printed_number_gap_is_a_space_under_cmd() {
    let out = raw_output("10 OPEN 1,3\n20 CMD 1\n30 PRINT 1;2\n40 PRINT#1\n50 CLOSE 1\n");
    let body: Vec<u8> = out
        .iter()
        .copied()
        .filter(|&b| b == 0x20 || b == 0x1D || b == 0x31 || b == 0x32)
        .collect();
    assert_eq!(
        body,
        vec![0x20, 0x31, 0x20, 0x20, 0x32, 0x20],
        "PRINT 1;2 under CMD must be <sp>1<sp><sp>2<sp>: {out:02X?}"
    );
}

#[test]
fn print_file_scopes_the_redirect_to_one_statement() {
    let out = raw_output(
        "10 OPEN 1,3\n20 PRINT#1,\"A\",\"B\"\n30 CLOSE 1\n40 PRINT \"C\",\"D\"\n",
    );
    assert_eq!(
        longest_run(&out, 0x20),
        9,
        "the PRINT# comma must pad with spaces: {out:02X?}"
    );
    assert_eq!(
        longest_run(&out, 0x1D),
        9,
        "the plain PRINT afterwards must be back to cursor-right: {out:02X?}"
    );
    // Order matters as much as the counts: spaces first, then cursor-rights.
    let first_space = out.iter().position(|&b| b == 0x20).expect("a space run");
    let first_crsr = out.iter().position(|&b| b == 0x1D).expect("a cursor run");
    assert!(
        first_space < first_crsr,
        "the redirected run must come first: {out:02X?}"
    );
}

#[test]
fn cmd_redirect_persists_across_statements() {
    let out = raw_output(
        "10 OPEN 1,3\n20 CMD 1\n30 PRINT \"A\",\"B\"\n40 PRINT \"C\",\"D\"\n50 PRINT#1\n60 CLOSE 1\n",
    );
    assert_eq!(
        longest_run(&out, 0x1D),
        0,
        "CMD stays in effect for later PRINTs, so nothing may be \
         cursor-right: {out:02X?}"
    );
    assert!(
        out.windows(9).filter(|w| w.iter().all(|&b| b == 0x20)).count() >= 2,
        "both PRINTs must pad with spaces: {out:02X?}"
    );
}

#[test]
fn poke_19_alone_flips_the_gap_byte() {
    let out = raw_output("10 POKE 19,1\n20 PRINT \"X\",\"Y\"\n30 POKE 19,0\n");
    assert_eq!(
        longest_run(&out, 0x20),
        9,
        "POKE 19,1 must redirect the pad byte without any CMD in sight: \
         {out:02X?}"
    );
    assert_eq!(longest_run(&out, 0x1D), 0, "{out:02X?}");
}

#[test]
fn all_four_gaps_are_still_cursor_right_on_the_screen() {
    // comma zone
    let out = raw_output("10 PRINT \"X\",\"Y\"\n");
    assert_eq!(longest_run(&out, 0x1D), 9, "comma: {out:02X?}");
    assert_eq!(longest_run(&out, 0x20), 0, "comma: {out:02X?}");

    let out = raw_output("10 PRINT \"X\";TAB(10);\"Y\"\n");
    assert_eq!(longest_run(&out, 0x1D), 9, "TAB: {out:02X?}");
    assert_eq!(longest_run(&out, 0x20), 0, "TAB: {out:02X?}");

    let out = raw_output("10 PRINT \"X\";SPC(5);\"Y\"\n");
    assert_eq!(longest_run(&out, 0x1D), 5, "SPC: {out:02X?}");
    assert_eq!(longest_run(&out, 0x20), 0, "SPC: {out:02X?}");

    // trailing gap after a number: <sp>1<crsr><sp>2<crsr>
    let out = raw_output("10 PRINT 1;2\n");
    assert_eq!(
        out,
        vec![0x20, 0x31, 0x1D, 0x20, 0x32, 0x1D, 0x0D],
        "number gap: {out:02X?}"
    );
}

#[test]
fn clearing_channl_restores_cursor_right() {
    let out = raw_output("10 POKE 19,1\n20 PRINT \"X\",\"Y\"\n30 POKE 19,0\n40 PRINT \"A\",\"B\"\n");
    assert_eq!(longest_run(&out, 0x20), 9, "redirected run: {out:02X?}");
    assert_eq!(longest_run(&out, 0x1D), 9, "screen run: {out:02X?}");
    let first_space = out.iter().position(|&b| b == 0x20).expect("space run");
    let first_crsr = out.iter().position(|&b| b == 0x1D).expect("cursor run");
    assert!(first_space < first_crsr, "{out:02X?}");
}

#[test]
fn all_four_gap_sites_go_through_the_rom_routine() {
    let src = "10 PRINT 1;2\n20 PRINT \"A\",\"B\"\n30 PRINT TAB(5);\"C\";SPC(3);\"D\"\n";
    for profile in ALL_PROFILES {
        let asm = asm_at(src, profile);
        for label in ["__PRINT_FAC", "__TAB_LOOP", "__SPC_LOOP", "__PC_PAD"] {
            let body = helper_body(&asm, label);
            assert!(
                body.contains("$AB3B"),
                "{profile:?}: {label} must call the ROM gap routine:\n{body}"
            );
            assert!(
                !body.contains("LDA #$1D") && !body.contains("LDA #$20"),
                "{profile:?}: {label} must not hard-code a gap byte:\n{body}"
            );
        }
    }
}

/// The INPUT prompt gap is the one place that legitimately keeps the literal `$1D`.
#[test]
fn input_prompt_gap_stays_hard_coded() {
    let asm = asm_at("10 INPUT A\n20 PRINT A\n", Profile::Default);
    let prompt = asm
        .split("LDA #$3F")
        .nth(1)
        .expect("the INPUT prompt is emitted");
    assert!(
        prompt[..prompt.len().min(200)].contains("LDA #$1D"),
        "the '?' prompt gap is unconditionally cursor-right:\n{}",
        &prompt[..prompt.len().min(200)]
    );
}

#[test]
fn get_file_clears_channl_when_it_finishes() {
    let out = raw_output("10 POKE 19,1\n20 OPEN 1,3\n30 GET#1,A$\n40 PRINT \"X\",\"Y\"\n50 CLOSE 1\n");
    assert_eq!(
        longest_run(&out, 0x1D),
        9,
        "GET# must clear CHANNL, so the following PRINT is back to \
         cursor-right: {out:02X?}"
    );
    assert_eq!(
        longest_run(&out, 0x20),
        0,
        "no space padding may survive the GET#: {out:02X?}"
    );
}

#[test]
fn get_file_ends_a_cmd_redirect() {
    let out = raw_output(
        "10 OPEN 1,3\n20 CMD 1\n30 PRINT \"A\",\"B\"\n40 GET#1,C$\n\
         50 PRINT \"C\",\"D\"\n60 CLOSE 1\n",
    );
    assert_eq!(longest_run(&out, 0x20), 9, "the CMD arm: {out:02X?}");
    assert_eq!(
        longest_run(&out, 0x1D),
        9,
        "the arm after the GET#: {out:02X?}"
    );
    let first_space = out.iter().position(|&b| b == 0x20).expect("a space run");
    let first_crsr = out.iter().position(|&b| b == 0x1D).expect("a cursor run");
    assert!(
        first_space < first_crsr,
        "spaces first, then cursor-right: {out:02X?}"
    );
}

#[test]
fn without_the_get_file_the_cmd_redirect_survives() {
    let out = raw_output(
        "10 OPEN 1,3\n20 CMD 1\n30 PRINT \"A\",\"B\"\n50 PRINT \"C\",\"D\"\n\
         55 PRINT#1\n60 CLOSE 1\n",
    );
    assert_eq!(
        longest_run(&out, 0x1D),
        0,
        "CMD is still in effect for the second PRINT: {out:02X?}"
    );
    assert!(
        out.windows(9).filter(|w| w.iter().all(|&b| b == 0x20)).count() >= 2,
        "both PRINTs pad with spaces: {out:02X?}"
    );
}

#[test]
fn get_file_on_the_screen_path_is_unchanged() {
    let out = raw_output("10 OPEN 1,3\n20 GET#1,A$\n30 PRINT \"X\",\"Y\"\n40 CLOSE 1\n");
    assert_eq!(longest_run(&out, 0x1D), 9, "{out:02X?}");
    assert_eq!(longest_run(&out, 0x20), 0, "{out:02X?}");
}

#[test]
fn get_file_brackets_channl_in_the_rom_order() {
    for profile in ALL_PROFILES {
        let asm = asm_at("10 OPEN 1,3\n20 GET#1,A$\n30 CLOSE 1\n", profile);
        assert!(
            asm.contains("STX $13\n    JSR $FFC6"),
            "{profile:?}: GET# must set CHANNL immediately before CHKIN:\n{asm}"
        );
        assert!(
            asm.contains("JSR $FFCC\n    LDX #$00\n    STX $13"),
            "{profile:?}: GET# must clear CHANNL after CLRCHN:\n{asm}"
        );
    }
}

#[test]
fn input_file_brackets_channl_around_the_whole_statement() {
    for profile in ALL_PROFILES {
        let asm = asm_at("10 OPEN 1,3\n20 INPUT#1,A\n30 CLOSE 1\n", profile);
        assert!(
            asm.contains("STX $13\n    JSR $FFC6"),
            "{profile:?}: INPUT# must set CHANNL immediately before CHKIN:\n{asm}"
        );
        assert!(
            asm.contains("LDX #$00\n    STX $13"),
            "{profile:?}: INPUT# must clear CHANNL when the statement ends:\n{asm}"
        );
        // Ordering: the clear must come after the field cutter, not between the line read and the stores.
        let clear = asm
            .find("LDX #$00\n    STX $13")
            .expect("the clear is emitted");
        let field = asm
            .find("JSR __INPUT_FILE_FIELD")
            .expect("the field cutter is called");
        assert!(
            clear > field,
            "{profile:?}: CHANNL must stay set across the field stores:\n{asm}"
        );
    }
}

#[test]
fn input_prompt_is_suppressed_while_a_channel_is_open() {
    for profile in ALL_PROFILES {
        let asm = asm_at("10 INPUT A\n20 PRINT A\n", profile);
        let i = asm.find("LDA #$3F").expect("the prompt is emitted");

        let head = asm[..i].trim_end();
        let label = head.rsplit("BNE ").next().expect("a BNE").trim().to_string();
        assert!(
            label.starts_with("__IN_NOPROMPT"),
            "{profile:?}: the '?' prompt must be guarded by LDA $13 / BNE, \
             found …{}",
            &head[head.len().saturating_sub(120)..]
        );
        assert!(
            head.ends_with(&format!("LDA $13\n    BNE {label}")),
            "{profile:?}: the guard must test CHANNL and nothing else: …{}",
            &head[head.len().saturating_sub(120)..]
        );

        assert!(
            asm.contains(&format!("{label}:\n    JSR __INPUT_LINE\n")),
            "{profile:?}: the guard must jump past '?' AND its gap:\n{asm}"
        );
    }
}

#[test]
fn input_reprompt_is_suppressed_while_a_channel_is_open() {
    for profile in ALL_PROFILES {
        let asm = asm_at("10 INPUT A,B\n20 PRINT A;B\n", profile);
        assert!(
            asm.contains(
                "__EF_REPROMPT:\n    LDA $13\n    BNE __EF_NOPROMPT\n    LDA #$3F\n"
            ),
            "{profile:?}: the '??' reprompt must be guarded by LDA $13 / BNE:\n{asm}"
        );

        let body = asm
            .split("__EF_REPROMPT:")
            .nth(1)
            .and_then(|s| s.split("__EF_NOPROMPT:").next())
            .expect("the reprompt body");
        assert_eq!(
            body.matches("LDA #$3F").count(),
            2,
            "two question marks:\n{body}"
        );
        assert!(body.contains("LDA #$1D"), "and a cursor-right:\n{body}");
        assert!(!body.contains("LDA #$20"), "never a space:\n{body}");
    }
}

/// The third `$13` reader on the INPUT path, and the least obvious one: the closing carriage return.
#[test]
fn input_line_closing_cr_is_suppressed_while_a_channel_is_open() {
    for profile in ALL_PROFILES {
        let asm = asm_at("10 INPUT A\n20 PRINT A\n", profile);
        assert!(
            asm.contains(
                "    LDA $13\n    BNE __INPUT_NOCR\n    LDA #$0D\n    JSR $FFD2\n\
                 __INPUT_NOCR:\n    RTS\n"
            ),
            "{profile:?}: INLIN's closing CR must be guarded by LDA $13 / BNE:\n{asm}"
        );
    }
}

#[test]
fn the_whole_input_prompt_still_fires_on_the_screen_path() {
    let asm = asm_at("10 INPUT A,B\n20 PRINT A;B\n", Profile::Default);
    // First prompt: '?' + cursor-right, back to back through CHROUT.
    assert!(
        asm.contains("LDA #$3F\n    JSR $FFD2\n    LDA #$1D\n    JSR $FFD2\n"),
        "the guarded body of the first prompt is unchanged:\n{asm}"
    );
    // Continuation prompt: '?' '?' + cursor-right.
    assert!(
        asm.contains(
            "LDA #$3F\n    JSR $FFD2\n    LDA #$3F\n    JSR $FFD2\n    LDA #$1D\n    JSR $FFD2\n"
        ),
        "the guarded body of the reprompt is unchanged:\n{asm}"
    );
    // Closing CR.
    assert!(
        asm.contains("LDA #$0D\n    JSR $FFD2\n__INPUT_NOCR:"),
        "the guarded closing CR is unchanged:\n{asm}"
    );
}

#[test]
fn a_literal_input_prompt_is_not_gated() {
    let asm = asm_at("10 INPUT \"NAME\";A$\n20 PRINT A$\n", Profile::Default);
    let guard = asm.find("LDA $13\n    BNE __IN_NOPROMPT").expect("the guard");
    let literal = asm.find("__STR_PRINT").expect("the literal is printed");
    assert!(
        literal < guard,
        "the literal prompt must be printed before the '?' guard:\n{asm}"
    );
}

#[test]
fn cmd_and_print_file_maintain_channl() {
    for profile in ALL_PROFILES {
        let cmd = asm_at("10 OPEN 1,3\n20 CMD 1\n30 PRINT \"A\"\n", profile);
        assert!(
            cmd.contains("STX $13\n    JSR $FFC9"),
            "{profile:?}: CMD must set CHANNL immediately before CHKOUT"
        );
        assert!(
            !cmd.contains("LDX #$00\n    STX $13"),
            "{profile:?}: CMD must NOT clear CHANNL — the redirect persists"
        );

        let pf = asm_at("10 OPEN 1,3\n20 PRINT#1,\"A\"\n", profile);
        assert!(
            pf.contains("STX $13\n    JSR $FFC9") || pf.contains("STX $13\n    JSR __CHKOUT_CK"),
            "{profile:?}: PRINT# must set CHANNL immediately before CHKOUT"
        );

        assert!(
            !pf.contains("JSR __CHKOUT_CK") || pf.contains("__CHKOUT_CK:\n    JSR $FFC9"),
            "{profile:?}: __CHKOUT_CK must be a CHKOUT"
        );
        assert!(
            pf.contains("JSR $FFCC\n    LDX #$00\n    STX $13"),
            "{profile:?}: PRINT# must clear CHANNL after CLRCHN"
        );
    }
}

#[test]
fn extraram_routes_the_gap_through_the_jump_table() {
    let prg = tokenize_program("10 PRINT 1;2\n20 PRINT \"A\",\"B\"\n").expect("tokenizes");
    let opts = CompileOptions {
        extraram: true,
        ..CompileOptions::default()
    };
    let asm = compile_with_options(&prg, opts).expect("compiles").asm;
    assert!(
        asm.contains("__JT_PRINT_GAP"),
        "the gap call must be wrapped by the extraram jump table"
    );

    let direct = asm.matches("$AB3B").count();
    assert_eq!(
        direct, 1,
        "exactly one direct $AB3B reference (inside the JT entry) may \
         survive the extraram rewrite, found {direct}"
    );
}
