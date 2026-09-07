use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn compile_asm(src: &str, profile: Profile) -> Result<String, String> {
    let prg = tokenize_program(src).expect("tokenizes");
    let opts = CompileOptions {
        profile,
        ..CompileOptions::default()
    };
    compile_with_options(&prg, opts)
        .map(|c| c.asm)
        .map_err(|e| format!("{e}"))
}

fn asm_of(src: &str) -> String {
    compile_asm(src, Profile::Default).unwrap_or_else(|e| panic!("compiles: {e}"))
}

/// `PEEK(200 + X%)` / `POKE 200 + X%`.
fn wrap_src() -> &'static str {
    "10 POKE 49152,120\n\
     20 A=49152\n\
     30 X%=PEEK(A)\n\
     40 POKE 320,111\n\
     50 PRINT PEEK(200+X%)\n"
}

#[test]
fn zero_page_indexed_wrap_is_refused_at_every_profile() {
    for profile in [Profile::Default, Profile::Size, Profile::Speed] {
        let err = compile_asm(wrap_src(), profile)
            .err()
            .unwrap_or_else(|| panic!("{profile:?}: a base of $00C8 under `,X` must be refused"));
        assert!(
            err.contains("$00C8"),
            "{profile:?}: the message must name the base it refused:\n{err}"
        );
        assert!(
            err.contains("zeropage,X"),
            "{profile:?}: the message must name the mode the assembler would pick:\n{err}"
        );
    }
}

#[test]
fn the_refused_program_differs_only_in_the_base() {
    let ok = wrap_src()
        .replace("POKE 320,111", "POKE 1344,111")
        .replace("PEEK(200+X%)", "PEEK(1224+X%)");
    let asm = asm_of(&ok);
    assert!(
        asm.contains("$04C8,X"),
        "the fast path must still fire for a base above zero page:\n{asm}"
    );
}

#[test]
fn abs_x_peek_and_poke_fast_paths_still_fire() {
    let asm = asm_of(
        "10 POKE 49152,120\n\
         20 A=49152\n\
         30 X%=PEEK(A)\n\
         40 PRINT PEEK(1024+X%)\n\
         50 POKE 1024+X%,7\n",
    );
    assert!(
        asm.contains("$0400,X"),
        "the const+u8var indexed tier must still fire:\n{asm}"
    );
    assert!(
        !asm.contains("__PB_"),
        "no self-modifying PEEK operand should be needed here:\n{asm}"
    );
}

#[test]
fn base_zero_indexed_reads_are_still_allowed() {
    let asm = asm_of(
        "10 DIM A(20)\n\
         20 FOR I=0 TO 20\n\
         30 A(I)=I*2\n\
         40 NEXT\n\
         50 PRINT A(7)\n",
    );
    assert!(
        asm.contains(",Y") || asm.contains(",X"),
        "an array program should emit some indexed access:\n{asm}"
    );
}

#[test]
fn indirect_y_fast_path_still_fires() {
    let asm = asm_of(
        "10 FOR I%=12800 TO 12871\n\
         20 POKE I%,7\n\
         30 NEXT\n",
    );
    assert!(
        asm.contains("STA (VI_I),Y"),
        "the ZP-pointer POKE tier must still fire:\n{asm}"
    );
    let equate = asm
        .lines()
        .find(|l| l.starts_with("VI_I = "))
        .unwrap_or_else(|| panic!("VI_I must be a zero-page equate:\n{asm}"));
    let hex = equate.trim_start_matches("VI_I = $").trim();
    let addr = u16::from_str_radix(hex, 16).expect("hex equate");
    assert!(addr < 0x00FF, "VI_I at ${addr:04X} is not a usable ZP pair");
}

#[test]
fn self_modifying_peek_operands_are_still_three_bytes() {
    let asm = asm_of(
        "10 A=1024\n\
         20 FOR I=0 TO 3\n\
         30 PRINT PEEK(A+I*40)\n\
         40 NEXT\n",
    );
    let patched: Vec<&str> = asm
        .lines()
        .map(str::trim)
        .filter(|l| l.contains("+1") && l.starts_with("STA __"))
        .collect();
    assert!(
        !patched.is_empty(),
        "this shape should still need a self-modifying operand:\n{asm}"
    );
    assert!(
        !asm.contains("LDA $0000\n"),
        "a self-modifying template must not use a placeholder that \
         narrows to zero page:\n{asm}"
    );
}
