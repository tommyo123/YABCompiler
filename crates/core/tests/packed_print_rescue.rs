//! Packing is admitted only after the existing layout ladder fails.
use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn source() -> String {
    let mut s = String::new();
    for i in 0..24 {
        let letter = char::from(b'A' + i);
        s.push_str(&format!("{} PRINT \"{}AB{letter}\";\n", 10 + u32::from(i) * 10, letter.to_string().repeat(177)));
    }
    s.push_str("500 A$=\"UNCHANGED\":PRINT LEN(A$);A$:END\n");
    s
}

fn run(prg: &[u8]) -> Vec<u8> {
    let result = emu64::run_prg_to_end(prg, 10_000_000).unwrap();
    assert!(result.clean_exit, "{:?}", result.error_output);
    result.output
}

#[test]
fn tight_reservation_rescues_print_pool_through_complete_layout() {
    let input = tokenize_program(&source()).unwrap();
    for (profile, extraram) in [Profile::Default, Profile::Speed, Profile::Size]
        .into_iter().flat_map(|p| [(p, false), (p, true)]) {
        let ordinary = compile_with_options(&input, CompileOptions { profile, force_extraram_off: true, ..Default::default() }).unwrap();
        assert!(!ordinary.asm.contains("__CP_"));
        let packed = compile_with_options(&input, CompileOptions {
            profile, extraram, force_extraram_off: !extraram, auto_reserve: false,
            reserved_ranges: vec![(0x1000, 0xcfff)], ..Default::default()
        }).unwrap();
        assert!(packed.asm.contains("__CP_PACKED:"), "fixture must require the packing retry");
        assert!(packed.prg_bytes.len() < ordinary.prg_bytes.len());
        assert_eq!(run(&packed.prg_bytes), run(&ordinary.prg_bytes));
    }
}

#[test]
fn still_impossible_layout_retains_original_error() {
    let input = tokenize_program(&source()).unwrap();
    let result = compile_with_options(&input, CompileOptions {
        profile: Profile::Default, force_extraram_off: true, auto_reserve: false,
        reserved_ranges: vec![(0x0900, 0xffff)], ..Default::default()
    });
    let error = result.err().expect("layout must be refused");
    assert!(matches!(error, yabcompiler_core::CompileError::Assemble { .. }));

    if let yabcompiler_core::CompileError::Assemble { asm: Some(asm), .. } = error {
        assert!(!asm.contains("__CP_"));
    }
}

#[test]
fn resident_decoder_survives_real_bss_suffix_trimming() {
    let source = format!("5 DIM Q(100)\n{}", source()).replace(
        "500 A$=\"UNCHANGED\":PRINT LEN(A$);A$:END",
        "500 GET A$:A$=A$+\"END\":PRINT LEN(A$);A$;Q(100):END",
    );
    let input = tokenize_program(&source).unwrap();
    let options = CompileOptions { force_extraram_off: true, auto_reserve: false, ..Default::default() };
    let ordinary = compile_with_options(&input, options.clone()).unwrap();
    let packed = compile_with_options(&input, CompileOptions {
        reserved_ranges: vec![(ordinary.diagnostics.end_address - 1000, 0xcfff)], ..options
    }).unwrap();
    assert!(packed.asm.contains("__CP_PACKED:"));
    assert!(packed.diagnostics.bss_truncated > 0, "fixture must trim a real BSS suffix");
    assert!(packed.asm.find("__CP_PACKED:").unwrap() < packed.asm.find("__BSS_START:").unwrap());
    assert_eq!(run(&packed.prg_bytes), run(&ordinary.prg_bytes));
}
