use yabcompiler_core::{CompileOptions, Dialect, compile_with_options, tokenize_program_with};

/// `10 COLOR3,4` as a C64 stores it: `43 4F 4C B0 33 2C 34`.
const SPLICED: &str = "10 COLOR3,4\n20 END\n";

const SPLICED_SP: &str = "10 COLOR 3,4\n20 END\n";

fn asm(src: &str, tokenized_as: Dialect, compiled_as: Dialect) -> String {
    let tok = tokenize_program_with(src, tokenized_as)
        .unwrap_or_else(|e| panic!("tokenize {src:?} as {tokenized_as:?}: {e:?}"));
    compile_with_options(
        &tok,
        CompileOptions {
            dialect: compiled_as,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compile {src:?} as {compiled_as:?}: {e}"))
    .asm
}

/// Does the program set the border/background registers from the two literals of `COLOR 3,4`?
fn ran_color_statement(asm: &str) -> bool {
    asm.contains("LDA #$03\n    STA $D020") && asm.contains("LDA #$04\n    STA $D021")
}

/// Does the line compile to BASIC v2's own `?SYNTAX ERROR` trap?
fn raises_v2_syntax_error(asm: &str) -> bool {
    asm.contains("LDX #$0B") && asm.contains("JMP $A437")
}

#[test]
fn spliced_color_is_a_v2_variable_not_a_keyword() {
    for src in [SPLICED, SPLICED_SP] {
        for d in [Dialect::Auto, Dialect::BasicV2] {
            let a = asm(src, Dialect::BasicV2, d);
            assert!(
                !ran_color_statement(&a),
                "{src:?} under {d:?} must not reach $D020/$D021\n{a}"
            );
            assert!(
                raises_v2_syntax_error(&a),
                "{src:?} under {d:?} must raise v2's ?SYNTAX ERROR\n{a}"
            );
        }
    }
}

#[test]
fn spliced_color_still_works_under_an_explicit_tsb_dialect() {
    for src in [SPLICED, SPLICED_SP] {
        let a = asm(src, Dialect::BasicV2, Dialect::Tsb);
        assert!(
            ran_color_statement(&a),
            "--dialect=tsb must still read the spliced COLOR\n{a}"
        );
        assert!(
            !raises_v2_syntax_error(&a),
            "--dialect=tsb must not turn the spliced COLOR into a trap\n{a}"
        );
    }
}

#[test]
fn auto_reads_the_splice_once_the_program_carries_a_real_token() {

    let src = "10 HIRES1,0\n20 COLOR3,4\n30 END\n";
    let tok = tokenize_program_with(src, Dialect::Tsb).unwrap();
    assert!(
        tok.windows(2).any(|w| w == [0x64, 0x01]),
        "the fixture must actually carry a $64 token"
    );

    let mut spliced = Vec::with_capacity(tok.len() + 2);
    let mut i = 0;
    let mut replaced = 0;
    while i < tok.len() {
        if tok[i] == 0x64 && tok.get(i + 1) == Some(&0x1A) {
            spliced.extend_from_slice(&[b'C', b'O', b'L', 0xB0]);
            replaced += 1;
            i += 2;
        } else {
            spliced.push(tok[i]);
            i += 1;
        }
    }
    assert_eq!(replaced, 1, "exactly one COLOR token to splice");

    let a = compile_with_options(
        &fix_links(&spliced),
        CompileOptions {
            dialect: Dialect::Auto,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compile: {e}"))
    .asm;
    assert!(
        ran_color_statement(&a),
        "Auto must keep the ASCII surface on for a program with a $64 token\n{a}"
    );
}

fn fix_links(prg: &[u8]) -> Vec<u8> {
    let load = u16::from_le_bytes([prg[0], prg[1]]);
    let mut out = prg.to_vec();
    // Collect line starts by walking on the terminators, not the links.
    let mut starts = Vec::new();
    let mut i = 2usize;
    while i + 4 <= out.len() && !(out[i] == 0 && out[i + 1] == 0) {
        starts.push(i);
        let mut j = i + 4;
        while j < out.len() && out[j] != 0 {
            j += 1;
        }
        i = j + 1;
    }
    for (n, &s) in starts.iter().enumerate() {
        let next = starts.get(n + 1).copied().unwrap_or(i);
        let addr = load as usize + next - 2;
        out[s] = (addr & 0xFF) as u8;
        out[s + 1] = (addr >> 8) as u8;
    }
    out
}

#[test]
fn the_unspliced_spellings_are_unchanged() {
    for word in ["COLOR", "COLOUR"] {
        // A tokeniser that does not crunch (`Dialect::Tsb` emits the token; force plain letters by spelling it past the table).
        let src = format!("10 {word}3,4\n20 END\n");
        let tsb = asm(&src, Dialect::Tsb, Dialect::Tsb);
        assert!(
            ran_color_statement(&tsb),
            "{word} must remain a statement under tsb\n{tsb}"
        );
    }
}

#[test]
fn the_real_token_is_gated_by_basic2_and_only_by_basic2() {
    let gated = asm(SPLICED, Dialect::Tsb, Dialect::BasicV2);
    assert!(
        !ran_color_statement(&gated),
        "--dialect=basic2 must not reach $D020/$D021 from a `$64 $1A` token\n{gated}"
    );
    assert!(
        raises_v2_syntax_error(&gated),
        "--dialect=basic2 must raise v2's own ?SYNTAX ERROR on `$64`\n{gated}"
    );

    for d in [Dialect::Tsb, Dialect::Auto] {
        let a = asm(SPLICED, Dialect::Tsb, d);
        assert!(
            ran_color_statement(&a),
            "{d:?} must still read the `$64 $1A` token as COLOR\n{a}"
        );
        assert!(
            !raises_v2_syntax_error(&a),
            "{d:?} must not turn the `$64 $1A` token into a trap\n{a}"
        );
    }
}

#[test]
fn the_compiler_only_splices_stay_ungated() {
    let a = asm("10 DPOKE828,4660\n20 END\n", Dialect::BasicV2, Dialect::Auto);
    assert!(
        !raises_v2_syntax_error(&a),
        "DPOKE is a compiler extension with no $64 token; it is not gated\n{a}"
    );
    assert!(
        a.contains("STA $033C") && a.contains("STA $033D"),
        "DPOKE must still write both halves\n{a}"
    );

    let b = asm("10 BORDER 5\n20 END\n", Dialect::BasicV2, Dialect::Auto);
    assert!(
        b.contains("LDA #$05\n    STA $D020"),
        "BORDER is a compiler extension and is not gated\n{b}"
    );
}

#[test]
fn every_extension_spelling_is_a_usable_v2_variable_name() {
    const WORDS: &[&str] = &[
        "ANGL", "ARC", "AT", "BCKGNDS", "BFLASH", "BLOCK", "CENTER", "CENTRE", "CHAR", "CIRCLE",
        "CLS", "CMOB", "COL", "COPY", "CSET", "DELAY", "DESIGN", "DETECT", "DISK", "DISPLAY",
        "DIV", "DRAW", "DUP", "ENVELOPE", "FCHR", "FCOL", "FETCH", "FILL", "FLASH", "GLOBAL",
        "GRAPHICS", "HI", "HIRES", "INKEY", "INSERT", "INST", "INV", "KEY", "LIN", "LINE",
        "LOCAL", "LOW", "MEM", "MMOB", "MOB", "MOBCOL", "MOD", "MOVE", "MULTI", "MUSIC", "NRM",
        "NULL", "PAGE", "PAUSE", "PLACE", "PLAY", "PLOT", "POT", "REC", "RESET", "RETRACE",
        "RLOCMOB", "ROT", "SCRLD", "SCRSV", "SECURE", "SOUND", "TEXT", "TRACE", "USE", "VOL",
        "WAVE", "DIR", "DUMP", "FIND", "OLD", "TEST", "COLD", "HRDCPY", "MERGE", "RENUMBER",
        "ERR", "OUT", "MAP", "JOY", "PENX", "PENY", "CHECK", "FRAC", "UPB", "UPW", "LEFTW",
        "LEFTB", "DOWNB", "DOWNW", "RIGHTB", "RIGHTW", "REPEAT", "UNTIL", "DO", "LOOP", "EXIT",
        "PROC", "CALL", "EXEC", "RESUME", "DISABLE", "ELSE", "RCOMP", "OFF",
    ];
    let mut bad = Vec::new();
    let mut checked = 0usize;
    for w in WORDS {
        let src = format!("10 {w}=1\n20 END\n");
        let tok = tokenize_program_with(&src, Dialect::BasicV2).unwrap();
        // Only the words that survive the crunch as plain letters.
        if !tok[6..].starts_with(w.as_bytes()) {
            continue;
        }
        checked += 1;
        for d in [Dialect::Auto, Dialect::BasicV2] {
            match compile_with_options(
                &tok,
                CompileOptions {
                    dialect: d,
                    ..CompileOptions::default()
                },
            ) {
                Err(e) => bad.push(format!("{w} {d:?}: compile error {e}")),
                Ok(o) if raises_v2_syntax_error(&o.asm) => {
                    bad.push(format!("{w} {d:?}: became a ?SYNTAX ERROR trap"));
                }
                Ok(_) => {}
            }
        }
    }
    assert!(
        checked >= 100,
        "the corpus must not shrink to nothing: only {checked} spellings survived the crunch"
    );
    assert!(bad.is_empty(), "extension leaked past the dialect gate:\n{bad:#?}");
}

#[test]
fn the_extension_words_are_still_keywords_under_tsb() {
    for (src, needle) in [
        ("10 FLASH2,3\n20 END\n", "__TSB_IRQ_INSTALL"),
        ("10 BFLASH2,0,6\n20 END\n", "__BFLASH_SPEED"),
        ("10 MUSIC4,\"CDE\"\n20 PLAYON\n30 END\n", "__MUSIC"),
        ("10 CLS\n20 END\n", "LDA #$93"),
    ] {
        let a = asm(src, Dialect::Tsb, Dialect::Tsb);
        assert!(
            a.contains(needle),
            "{src:?} must still compile the extension ({needle})\n{a}"
        );
    }
}
