use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn asm_of(src: &str, profile: Profile) -> String {
    let prg = tokenize_program(src).expect("tokenizes");
    let opts = CompileOptions {
        profile,
        force_extraram_off: true,
        ..CompileOptions::default()
    };
    compile_with_options(&prg, opts)
        .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}"))
        .asm
}

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

/// Every line of `__INPUT_LINE`, from its label to its `RTS`.
fn input_line_body(asm: &str) -> Vec<String> {
    let lines: Vec<&str> = asm.lines().map(str::trim).collect();
    let start = lines
        .iter()
        .position(|l| *l == "__INPUT_LINE:")
        .unwrap_or_else(|| panic!("no __INPUT_LINE helper:\n{asm}"));
    let end = lines[start..]
        .iter()
        .position(|l| *l == "RTS")
        .unwrap_or_else(|| panic!("__INPUT_LINE never returns:\n{asm}"));
    lines[start..=start + end]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

#[test]
fn input_line_never_writes_ndx() {
    for profile in PROFILES {
        let body = input_line_body(&asm_of("10 INPUT A$\n20 PRINT A$\n", profile));
        assert!(
            !body.iter().any(|l| l == "STA $C6" || l == "STX $C6" || l == "STY $C6"),
            "{profile:?}: __INPUT_LINE flushes the keyboard buffer:\n{}",
            body.join("\n")
        );
    }
}

/// The input helper initializes its buffer length.
#[test]
fn input_line_still_zeroes_its_length_before_the_read_loop() {
    for profile in PROFILES {
        let body = input_line_body(&asm_of("10 INPUT A$\n20 PRINT A$\n", profile));
        let zero = body
            .iter()
            .position(|l| l == "LDA #$00")
            .unwrap_or_else(|| panic!("{profile:?}: no zero loaded:\n{}", body.join("\n")));
        let store = body
            .iter()
            .position(|l| l == "STA __INPUT_LEN")
            .unwrap_or_else(|| panic!("{profile:?}: __INPUT_LEN never initialised:\n{}", body.join("\n")));
        let loop_at = body
            .iter()
            .position(|l| l == "__INPUT_LINE_LOOP:")
            .unwrap_or_else(|| panic!("{profile:?}: no read loop:\n{}", body.join("\n")));
        assert!(
            zero < store && store < loop_at,
            "{profile:?}: the length must be zeroed before the read loop:\n{}",
            body.join("\n")
        );
    }
}

#[test]
fn a_program_poking_198_still_writes_ndx() {
    for profile in PROFILES {
        let asm = asm_of(
            "10 POKE 631,53:POKE 632,13:POKE 198,2\n20 INPUT A\n30 PRINT \"A=\";A\n",
            profile,
        );
        let body = input_line_body(&asm);
        assert!(
            !body.iter().any(|l| l == "STA $C6"),
            "{profile:?}: __INPUT_LINE still flushes:\n{}",
            body.join("\n")
        );
        let whole: Vec<&str> = asm.lines().map(str::trim).collect();
        assert!(
            whole.iter().any(|l| *l == "STA $C6" || *l == "STA $00C6"),
            "{profile:?}: POKE 198,2 did not reach $C6 at all:\n{asm}"
        );
    }
}
