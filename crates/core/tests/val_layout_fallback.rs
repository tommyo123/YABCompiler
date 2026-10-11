//! A failed low-memory VAL copy must never admit the exposed in-place body.

use asm6502::Assembler6502;
use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const SOURCE: &str = "10 FOR I=1 TO 3:A$=A$+CHR$(48+I):NEXT\n\
    20 PRINT VAL(A$):PRINT A$:PRINT \"DONE\":END\n";

#[test]
fn automatic_original_text_layout_also_repairs_the_failed_val_copy() {

    let mut source = String::from("10 PRINT PEEK(PEEK(43)+256*PEEK(44))\n\
        20 FOR I=1 TO 3:A$=A$+CHR$(48+I):NEXT\n\
        30 PRINT VAL(A$):PRINT A$:PRINT \"DONE\":END\n");
    for i in 0..102 {
        source.push_str(&format!("{} DATA \"{i:03}{}\"\n", 100 + i, "1".repeat(180)));
    }
    let input = tokenize_program(&source).unwrap();
    let built = compile_with_options(&input, CompileOptions::default()).unwrap();
    assert!(built.diagnostics.extraram.is_on());
    assert!(built.diagnostics.effective_reserved.is_empty());
    assert!(!built.asm.contains("__VAL_COPY:"));
    assert!(built.asm.contains("Linked fill keeps VAL"));
    assert_eq!(0x080d + built.machine_code.len(), 0xc000);
    // The boot trampoline replaces the first BASIC line; later source bytes remain where the original-text materialization contract put them.
    assert_eq!(&built.prg_bytes[40..input.len()], &input[40..]);
}

#[test]
fn failed_val_copy_lifts_the_heap_with_real_linked_bytes() {
    let input = tokenize_program(SOURCE).unwrap();
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let options = CompileOptions {
            profile,
            extraram: true,
            reserved_ranges: vec![(0x0900, 0x9f40)],
            ..CompileOptions::default()
        };
        let built = compile_with_options(&input, options).unwrap();
        assert!(built.diagnostics.extraram.is_on());
        assert!(built.asm.contains("__VAL_HELPER:"));
        assert!(!built.asm.contains("__VAL_COPY:"));
        assert!(built.asm.contains("Linked fill keeps VAL"));
        assert!(!built.asm.contains("whole DATA pool before string constants"),
            "the ordinary layout, not only the manual DataFirst rescue, must be safe");
        let mut a = Assembler6502::new();
        for &(start, end) in &built.diagnostics.effective_reserved {
            a.add_reserved_range(start, end).unwrap();
        }
        let linked = a.assemble_bytes(&built.asm).unwrap();
        assert_eq!(linked, built.machine_code, "same reservations reproduce the image");
        assert_eq!(a.lookup("__HEAP_BOTTOM"), Some(0xc000));
        assert_eq!(0x080d + linked.len(), 0xc000, "padding is present in the load image");
        assert!(a.lookup("__EXRAM_DATA_END").unwrap() <= 0xa000);
        assert_eq!(&built.prg_bytes[14..], linked);
    }
}

#[test]
fn failed_val_copy_cannot_return_an_unsafe_image_when_the_high_heap_is_reserved() {
    let input = tokenize_program(SOURCE).unwrap();
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let options = CompileOptions {
            profile,
            extraram: true,
            reserved_ranges: vec![(0x0900, 0x9f40), (0xc000, 0xcfff)],
            ..CompileOptions::default()
        };
        assert!(compile_with_options(&input, options).is_err(),
            "neither a low copy nor the high heap fits: in-place VAL is not a fallback");
    }
}
