mod common;

use yabcompiler_core::{CompileOptions, compile_with_options, tokenize_program};

fn chrout_bytes(src: &str) -> Vec<u8> {
    let prg = common::compile_source(src).unwrap_or_else(|e| panic!("compile {src:?}: {e}"));
    let r = emu64::run_prg_to_end(&prg, 40_000_000).expect("runs");
    assert!(r.clean_exit, "no clean exit for:\n{src}");
    r.output
}

fn asm_of(src: &str) -> String {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(&prg, CompileOptions::default())
        .unwrap_or_else(|e| panic!("compiles: {e}"))
        .asm
}

const CR: u8 = 0x0d;
const RIGHT: u8 = 0x1d;
const SPACE: u8 = 0x20;

/// PRINT's comma pads to the next 10-column zone with cursor-rights.
#[test]
fn print_comma_pads_with_cursor_right_not_space() {
    let out = chrout_bytes("10 PRINT \"x\",\"y\"\n");
    let mut want = vec![b'X'];
    want.extend(std::iter::repeat_n(RIGHT, 9));
    want.extend([b'Y', CR]);
    assert_eq!(out, want, "got {out:02x?}");
    assert!(!out.contains(&SPACE), "comma pad still emits a space");
}

#[test]
fn print_comma_zone_arithmetic_is_unchanged() {
    // Column 10 exactly: a full extra zone, ten cursor-rights.
    let out = chrout_bytes("10 PRINT \"0123456789\",\"x\"\n");
    let mut want = b"0123456789".to_vec();
    want.extend(std::iter::repeat_n(RIGHT, 10));
    want.extend([b'X', CR]);
    assert_eq!(out, want, "got {out:02x?}");

    // Column 31: nine more cursor-rights, landing on column 40.
    let thirty_one = "0".repeat(31);
    let out = chrout_bytes(&format!("10 PRINT \"{thirty_one}\",\"z\"\n"));
    let mut want = thirty_one.into_bytes();
    want.extend(std::iter::repeat_n(RIGHT, 9));
    want.extend([b'Z', CR]);
    assert_eq!(out, want, "got {out:02x?}");
    assert!(!out.contains(&CR) || out[out.len() - 1] == CR, "unexpected CR in the pad");
}

#[test]
fn print_comma_pad_count_matches_the_rom_sweep_for_every_column() {
    for n in 0..=44usize {
        let xs = "x".repeat(n);
        let out = chrout_bytes(&format!("10 PRINT \"{xs}\",\"*\"\n"));
        let pad = 10 - (n % 10);
        let mut want = vec![b'X'; n];
        want.extend(std::iter::repeat_n(RIGHT, pad));
        want.extend([b'*', CR]);
        assert_eq!(out, want, "column {n}: got {out:02x?}");
    }
}

#[test]
fn tab_spc_and_numeric_gaps_still_emit_cursor_right() {
    assert_eq!(
        chrout_bytes("10 PRINT \"x\";TAB(4);\"y\"\n"),
        vec![b'X', RIGHT, RIGHT, RIGHT, b'Y', CR]
    );
    assert_eq!(
        chrout_bytes("10 PRINT \"x\";SPC(3);\"y\"\n"),
        vec![b'X', RIGHT, RIGHT, RIGHT, b'Y', CR]
    );

    assert_eq!(
        chrout_bytes("10 PRINT 1;2\n"),
        vec![SPACE, b'1', RIGHT, SPACE, b'2', RIGHT, CR]
    );
}

/// The `?` prompt and the `??` continuation prompt both end in CHR$(29).
#[test]
fn input_prompt_and_reprompt_gaps_are_cursor_right() {
    let asm = asm_of("10 INPUT A,B\n20 PRINT A;B\n");
    // The prompt: '?' then cursor-right, back to back through CHROUT.
    let prompt = "    LDA #$3F\n    JSR $FFD2\n    LDA #$1D\n    JSR $FFD2\n";
    assert!(asm.contains(prompt), "INPUT prompt is not '?' + CHR$(29):\n{asm}");
    // The continuation prompt: '?' '?' then cursor-right.
    let reprompt = "__EF_REPROMPT:\n    LDA $13\n    BNE __EF_NOPROMPT\n\
                    \x20   LDA #$3F\n    JSR $FFD2\n    LDA #$3F\n    JSR $FFD2\n    LDA #$1D\n";
    assert!(asm.contains(reprompt), "INPUT reprompt is not '??' + CHR$(29):\n{asm}");

    let ef = asm
        .split("__EF_REPROMPT:")
        .nth(1)
        .and_then(|s| s.split("__EF_EMPTY:").next())
        .expect("reprompt block present");
    assert!(!ef.contains("LDA #$20"), "reprompt block still emits a space:\n{ef}");
}

#[test]
fn print_comma_helper_is_still_pulled_in_on_demand_only() {
    assert!(!asm_of("10 PRINT \"x\";\"y\"\n").contains("__PRINT_COMMA"));
    assert!(asm_of("10 PRINT \"x\",\"y\"\n").contains("__PRINT_COMMA"));
}
