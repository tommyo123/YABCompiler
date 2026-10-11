//! Small hot-path changes with explicit carry, value and profile controls.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

#[test]
fn rom_one_is_the_exact_mflpt_constant() {
    let rom = include_bytes!("../../emu64/roms/basic.bin");
    let offset = yabcompiler_core::runtime::FLOAT_ONE as usize - 0xa000;
    assert_eq!(&rom[offset..offset + 5], &[0x81, 0, 0, 0, 0]);
}

fn compile(src: &str, profile: Profile) -> (String, Vec<u8>) {
    let out = compile_with_options(
        &tokenize_program(src).unwrap(),
        CompileOptions { profile, ..CompileOptions::default() },
    ).unwrap();
    (out.asm, out.prg_bytes)
}

fn output(src: &str, profile: Profile) -> (String, String) {
    let (asm, prg) = compile(src, profile);
    let run = emu64::run_prg_to_end(&prg, 80_000_000).unwrap();
    assert!(run.clean_exit, "{profile:?} did not finish: {src}");
    (asm, common::petscii_to_string(&run.output))
}

#[test]
fn address_patch_preserves_the_carry_over_two_pages() {
    let src = "10 W=12288\n20 FOR I=0 TO 511:POKE W+I,(I AND 31)+1:NEXT\n30 S=0:FOR I=0 TO 511:S=S+PEEK(W+I):NEXT\n40 PRINT S;PEEK(W+255);PEEK(W+256);PEEK(W+511)\n";
    for profile in PROFILES {
        let (asm, out) = output(src, profile);
        assert_eq!(out.split_whitespace().collect::<Vec<_>>(), ["8448", "32", "1", "32"]);
        let body = asm.split("__FOR_TOP_0:").nth(1).expect("first loop");
        let body = body.split("__FOR_EXIT_0:").next().unwrap();
        assert!(!body.contains("STA T"), "address/value must not stage a float temp: {body}");
    }
}

#[test]
fn address_patch_preserves_signed_byte_extension_and_borrow() {
    let src = "10 W=13056\n20 FOR I=-127 TO 127:POKE W+I,(I AND 31)+1:NEXT\n30 PRINT PEEK(W-127);PEEK(W-1);PEEK(W);PEEK(W+127)\n40 FOR I=0 TO 511:POKE W-I,(I AND 31)+1:NEXT\n50 PRINT PEEK(W);PEEK(W-255);PEEK(W-256);PEEK(W-511)\n";
    for profile in PROFILES {
        let (_, out) = output(src, profile);
        assert_eq!(out.split_whitespace().collect::<Vec<_>>(), ["2", "32", "1", "32", "1", "32", "1", "32"]);
    }
}

#[test]
fn signed_byte_address_leaf_materializes_sign_before_the_carry_chain() {
    let src = "10 POKE 1024,0:I%=(PEEK(1024) AND 127)-127\n20 POKE 13056+I%,42:POKE 13056-I%,17\n30 PRINT PEEK(12929);PEEK(13183)\n";
    for profile in PROFILES {
        let (asm, out) = output(src, profile);
        assert_eq!(out.split_whitespace().collect::<Vec<_>>(), ["42", "17"]);
        assert!(asm.contains("__SXT_TMP_"), "control must exercise a signed-byte integer home: {asm}");
    }
}

#[test]
fn hardware_peek_stays_inside_the_fill_loop() {
    let src = "10 W=1024\n20 FOR I=0 TO 499:POKE W+I,(PEEK(53280) AND 31)+1:NEXT\n";
    for profile in PROFILES {
        let (asm, _) = compile(src, profile);
        let body = asm.split("__FOR_TOP_0:").nth(1).expect("read must stay in a loop");
        let body = body.split("__FOR_EXIT_0:").next().unwrap();
        assert!(body.contains("LDA $D020"), "hardware PEEK must run each iteration: {body}");
        assert!(!body.contains("STA T"), "address must patch directly: {body}");
        assert!(!asm.contains("JSR __POKE_FILL"), "a one-read fill changes the program");
    }
}

#[test]
fn fill_loop_reloads_memory_that_an_earlier_iteration_changed() {
    let src = "10 W=1024:POKE W,0\n20 FOR I=0 TO 499:POKE W+I,(PEEK(1024) AND 31)+1:NEXT\n30 PRINT PEEK(W);PEEK(W+1);PEEK(W+499)\n";
    for profile in PROFILES {
        let (_, out) = output(src, profile);
        assert_eq!(out.split_whitespace().collect::<Vec<_>>(), ["1", "2", "2"]);
    }
}

#[test]
fn pooled_small_float_literals_keep_fractional_accumulation_exact() {
    let src = "10 A=.5\n20 FOR I=1 TO 4:A=1+A:NEXT\n30 PRINT A\n";
    for profile in PROFILES {
        let (asm, out) = output(src, profile);
        assert_eq!(out.split_whitespace().collect::<Vec<_>>(), ["4.5"]);
        assert!(asm.contains("LDA #$BC") && asm.contains("LDY #$B9"), "literal one must use the ROM constant: {asm}");
    }
}

#[test]
fn rom_float_constant_load_banks_basic_in_with_extraram() {
    let src = "10 A=.5\n20 FOR I=1 TO 4:A=1+A:NEXT\n30 PRINT A\n";
    for profile in PROFILES {
        let compiled = compile_with_options(
            &tokenize_program(src).unwrap(),
            CompileOptions { profile, extraram: true, ..CompileOptions::default() },
        ).unwrap();
        assert!(compiled.asm.contains("__JT_MOVFM:"));
        let run = emu64::run_prg_to_end(&compiled.prg_bytes, 80_000_000).unwrap();
        assert!(run.clean_exit);
        assert_eq!(common::petscii_to_string(&run.output).trim(), "4.5");
    }
}

#[test]
fn speed_pool_loads_preserve_signed_integer_constants_in_fac() {
    let src = "10 A=.5:B=.5\n20 FOR I=1 TO 4:A=2+A:B=-2+B:NEXT\n30 PRINT A;B\n";
    let mut sizes = Vec::new();
    for profile in PROFILES {
        let (_, out) = output(src, profile);
        assert_eq!(out.split_whitespace().collect::<Vec<_>>(), ["8.5", "-7.5"]);
        sizes.push(compile(src, profile).1.len());
    }

    assert!(sizes[1] > sizes[0]);
    let stand_in = asm6502::Assembler6502::new()
        .assemble_bytes(yabcompiler_core::codegen::FAST_BYTE_GIVAYF)
        .unwrap()
        .len();
    assert_eq!(sizes[0], sizes[2] + stand_in);
}

#[test]
fn lastvar_capture_keeps_owned_copies_and_three_append_chains() {
    let src = "10 A$=\"\":B$=\"\":C$=\"\":I=0\n20 A$=A$+\"A\":B$=B$+\"B\":C$=C$+\"C\":D$=A$:I=I+1\n30 IF I<5 THEN 20\n40 A$=A$+\"!\":PRINT A$;\":\";B$;\":\";C$;\":\";D$\n";
    for profile in PROFILES {
        let (asm, out) = output(src, profile);
        assert_eq!(out.trim(), "AAAAA!:BBBBB:CCCCC:AAAAA");
        if profile == Profile::Speed {
            assert!(!asm.contains("__LVAR_UPDATE:"), "Speed keeps the address update inline");
        } else {
            let helper = asm.split("__LVAR_UPDATE:").nth(1).expect("several update sites");
            let helper = helper.split("RTS").next().unwrap();
            assert!(helper.contains("($FD)"), "many sites retain the smaller indirect helper");
        }
    }
}

#[test]
fn three_lastvar_sites_capture_the_pointer_before_bookkeeping() {
    let src = "10 A$=\"\":B$=\"\":C$=\"\":I=0\n20 A$=A$+\"A\":B$=B$+\"B\":C$=C$+\"C\":I=I+1\n30 IF I<5 THEN 20\n40 PRINT A$;\":\";B$;\":\";C$\n";
    for profile in PROFILES {
        let (asm, out) = output(src, profile);
        assert_eq!(out.trim(), "AAAAA:BBBBB:CCCCC");
        if let Some(helper) = asm.split("__LVAR_UPDATE:").nth(1) {
            let helper = helper.split("RTS").next().unwrap();
            assert!(!helper.contains("($FD)"), "live pointer capture avoids an indirect reread");
        }
    }
}
