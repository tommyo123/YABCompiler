//! BASIC's two `INPUT` diagnostics ; `?REDO FROM START` and `?EXTRA IGNORED` ; and the control flow behind the first one.

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

fn asm_of(src: &str, profile: Profile) -> String {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(
        &prg,
        CompileOptions {
            profile,
            force_extraram_off: true,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}"))
    .asm
}

fn lines(asm: &str) -> Vec<String> {
    asm.lines().map(|l| l.trim().to_string()).collect()
}

fn body(asm: &str, label: &str) -> Vec<String> {
    let l = lines(asm);
    let start = l
        .iter()
        .position(|x| x == label)
        .unwrap_or_else(|| panic!("no {label} in:\n{asm}"));
    let end = l[start..]
        .iter()
        .position(|x| x == "RTS")
        .unwrap_or_else(|| panic!("{label} never returns:\n{asm}"));
    l[start..=start + end].to_vec()
}

fn pos(asm: &str, needle: &str) -> usize {
    lines(asm)
        .iter()
        .position(|l| l == needle)
        .unwrap_or_else(|| panic!("no `{needle}` in:\n{asm}"))
}

const NUMERIC: &str = "10 INPUT A\n20 PRINT \"A=\";A\n";

#[test]
fn the_keyboard_numeric_parser_runs_fin_and_then_checks_the_terminator() {
    for profile in PROFILES {
        let asm = asm_of(NUMERIC, profile);
        let b = body(&asm, "__INPUT_VAL:");
        let joined = b.join("\n");
        let chrgot: Vec<usize> = b
            .iter()
            .enumerate()
            .filter(|(_, l)| *l == "JSR $0079")
            .map(|(i, _)| i)
            .collect();
        let fin = b
            .iter()
            .position(|l| l == "JSR $BCF3")
            .unwrap_or_else(|| panic!("{profile:?}: no FIN call:\n{joined}"));
        assert_eq!(
            chrgot.len(),
            2,
            "{profile:?}: FIN needs CHRGOT before it (fetch) and after it \
             (the terminator), no more and no fewer:\n{joined}"
        );
        assert!(
            chrgot[0] < fin && fin < chrgot[1],
            "{profile:?}: the terminator CHRGOT must come after FIN:\n{joined}"
        );

        let branch = b
            .iter()
            .position(|l| l == "BEQ __IN_VAL_OK")
            .unwrap_or_else(|| panic!("{profile:?}: no terminator branch:\n{joined}"));
        let raise = b
            .iter()
            .position(|l| l == "INC __IN_REDO")
            .unwrap_or_else(|| panic!("{profile:?}: nothing ever sets the flag:\n{joined}"));
        assert!(
            chrgot[1] < branch && branch < raise,
            "{profile:?}: the flag must be raised on the branch-not-taken arm:\n{joined}"
        );
        // FIN stops on a NUL, so the field has to have one.
        assert!(
            b.iter().any(|l| l == "STA __FIELD_BUF,X"),
            "{profile:?}: the field is never NUL-terminated, so FIN would run \
             off the end of it:\n{joined}"
        );
        // TXTPTR is borrowed, so it has to be given back.
        assert!(
            b.iter().filter(|l| *l == "STA $7A").count() == 2
                && b.iter().filter(|l| *l == "STA $7B").count() == 2,
            "{profile:?}: TXTPTR must be saved and restored:\n{joined}"
        );
    }
}

#[test]
fn a_redo_jumps_back_in_front_of_the_prompt() {
    for profile in PROFILES {
        let asm = asm_of("10 INPUT \"NAME\";A\n20 PRINT A\n", profile);
        let l = lines(&asm);
        let top = l
            .iter()
            .position(|x| x.starts_with("__IN_TOP_") && x.ends_with(':'))
            .unwrap_or_else(|| panic!("{profile:?}: no restart label:\n{asm}"));
        let top_label = l[top].trim_end_matches(':').to_string();
        let jump = l
            .iter()
            .position(|x| *x == format!("JMP {top_label}"))
            .unwrap_or_else(|| panic!("{profile:?}: nothing jumps back to {top_label}:\n{asm}"));
        assert!(
            jump > top,
            "{profile:?}: the restart must be a BACKWARD jump:\n{asm}"
        );
        // The diagnostic runs first, and the flag test gates both.
        let say = pos(&asm, "JSR __IN_REDO_MSG");
        let test = pos(&asm, "LDA __IN_REDO");
        assert!(
            top < test && test < say && say < jump,
            "{profile:?}: expected label < flag test < message < jump back:\n{asm}"
        );

        let prompt = l
            .iter()
            .position(|x| x == "LDA #$3F")
            .unwrap_or_else(|| panic!("{profile:?}: no `?` prompt:\n{asm}"));
        assert!(
            top < prompt,
            "{profile:?}: the restart label is behind the prompt, so a redo would \
             re-read without asking again:\n{asm}"
        );
    }
}

/// Redirected input has nobody to redo it, so the ROM raises `?FILE DATA ERROR` instead.
#[test]
fn a_redo_under_a_redirected_channel_is_a_file_data_error() {
    for profile in PROFILES {
        let asm = asm_of(NUMERIC, profile);
        let b = body(&asm, "__IN_REDO_MSG:");
        let joined = b.join("\n");
        assert_eq!(b[1], "LDA $13", "{profile:?}: CHANNL is not tested:\n{joined}");
        assert!(
            b.iter().any(|l| l == "LDX #$18"),
            "{profile:?}: no ?FILE DATA ERROR arm:\n{joined}"
        );
        assert!(
            b.iter().any(|l| l == "LDA #$0C") && b.iter().any(|l| l == "LDY #$AD"),
            "{profile:?}: the message pointer is not the ROM's $AD0C:\n{joined}"
        );
    }
}

#[test]
fn input_from_a_file_keeps_the_tolerant_parser() {
    for profile in PROFILES {
        let asm = asm_of("10 OPEN 1,8,2,\"D\"\n20 INPUT#1,A\n30 CLOSE 1\n", profile);
        assert!(
            asm.contains("JSR __VAL_HELPER"),
            "{profile:?}: INPUT# should still go through __VAL_HELPER:\n{asm}"
        );
        assert!(
            !asm.contains("JSR __INPUT_VAL"),
            "{profile:?}: INPUT# picked up the keyboard redo check:\n{asm}"
        );
        assert!(
            !asm.contains("__IN_REDO"),
            "{profile:?}: INPUT# pulled in the redo machinery:\n{asm}"
        );
    }
}

#[test]
fn extra_ignored_is_driven_by_the_field_terminator() {
    for profile in PROFILES {
        let asm = asm_of(NUMERIC, profile);
        let b = body(&asm, "__IN_EXTRA:");
        let joined = b.join("\n");
        assert_eq!(
            b[1], "LDA __FIELD_COMMA",
            "{profile:?}: the terminator flag is not what gates the message:\n{joined}"
        );
        assert!(
            b.iter().any(|l| l == "LDA $13"),
            "{profile:?}: CHANNL must suppress it — INPUT# never says it:\n{joined}"
        );
        assert!(
            b.iter().any(|l| l == "LDA #$FC") && b.iter().any(|l| l == "LDY #$AC"),
            "{profile:?}: the message pointer is not the ROM's $ACFC:\n{joined}"
        );

        let l = lines(&asm);
        for exit in ["__EF_COMMA:", "__EF_DONE:", "__EF_EMPTY:"] {
            let at = pos(&asm, exit);
            let decides = l[at..(at + 5).min(l.len())]
                .iter()
                .any(|x| x.ends_with(" __FIELD_COMMA"));
            assert!(
                decides,
                "{profile:?}: the field cutter's `{exit}` exit leaves __FIELD_COMMA \
                 at whatever the previous field set:\n{}",
                l[at..(at + 5).min(l.len())].join("\n")
            );
        }
    }
}

#[test]
fn a_bare_return_does_not_reach_extra_ignored() {
    for profile in PROFILES {
        let asm = asm_of(NUMERIC, profile);
        let l = lines(&asm);
        let call = pos(&asm, "JSR __IN_EXTRA");
        let join = l
            .iter()
            .position(|x| x.starts_with("__IN_EMPTY_") && x.ends_with(':'))
            .unwrap_or_else(|| panic!("{profile:?}: no abandon join:\n{asm}"));
        assert!(
            call < join,
            "{profile:?}: `?EXTRA IGNORED` is on the bare-RETURN path:\n{asm}"
        );
    }
}

#[test]
fn a_string_only_input_gets_the_message_but_not_the_parser() {
    for profile in PROFILES {
        let asm = asm_of("10 INPUT A$\n20 PRINT A$\n", profile);
        assert!(
            asm.contains("JSR __IN_EXTRA") && asm.contains("__IN_EXTRA:"),
            "{profile:?}: string INPUT lost `?EXTRA IGNORED`:\n{asm}"
        );
        assert!(
            !asm.contains("__INPUT_VAL"),
            "{profile:?}: string-only INPUT paid for the numeric parser:\n{asm}"
        );
    }
}

/// Under `--extraram` the BASIC ROM is banked out and `$BCF3` is the image's own code.
#[test]
fn fin_is_thunked_under_extraram() {
    let prg = tokenize_program(NUMERIC).expect("tokenizes");
    let asm = compile_with_options(
        &prg,
        CompileOptions {
            extraram: true,
            ..CompileOptions::default()
        },
    )
    .expect("compiles under extraram")
    .asm;
    assert!(
        asm.contains("JSR __JT_FIN"),
        "the FIN call was not routed through a bank-in thunk:\n{asm}"
    );
    assert!(
        asm.contains("__JT_FIN:"),
        "no FIN thunk was emitted:\n{asm}"
    );
    // Exactly one direct call survives: the one inside the thunk.
    assert_eq!(
        asm.matches("JSR $BCF3").count(),
        1,
        "a direct FIN call escaped the thunk:\n{asm}"
    );
}
