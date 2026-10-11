mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn same_as_the_rom(src: &str) -> Vec<String> {
    let prg = tokenize_program(src).expect("tokenizes");
    let r = emu64::run_basic_to_end(&prg, 20_000_000).expect("runs in the interpreter");
    let want = (common::petscii_to_string(&r.output), r.error_code, r.error_line);
    let mut asms = Vec::new();
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let c = compile_with_options(&prg, CompileOptions { profile, ..CompileOptions::default() })
            .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}"));
        let r = emu64::run_prg_to_end(&c.prg_bytes, 20_000_000).expect("runs");
        assert_eq!(
            (common::petscii_to_string(&r.output), r.error_code, r.error_line),
            want,
            "{profile:?}:\n{src}"
        );
        asms.push(c.asm);
    }
    asms
}

#[test]
fn byte_items_into_every_kind_of_target() {
    let asms = same_as_the_rom(
        "10 DIM A(5),B%(5)\n\
         20 FOR I=0 TO 5:READ A(I):NEXT:FOR I=0 TO 5:READ B%(I):NEXT\n\
         30 READ X,Y%:PRINT X;Y%:FOR I=0 TO 5:PRINT A(I);B%(I);:NEXT:PRINT\n\
         40 RESTORE:READ X:PRINT X:FOR I=1 TO 20:READ X:PRINT X;:NEXT\n\
         50 DATA 0,1,2,127,128,255,255,0,9,17,100,7\n\
         60 DATA 64,32\n",
    );
    for asm in asms {
        assert!(asm.contains("; --- DATA pool (binary u8"), "{asm}");
    }
}

#[test]
fn other_spellings_of_small_numbers() {
    same_as_the_rom("10 FOR I=1 TO 5:READ A:PRINT A;:NEXT\n20 DATA -0,1E2,007,+4,.0\n");
}

#[test]
fn a_value_past_255_keeps_two_bytes() {
    for asm in same_as_the_rom("10 READ A,B:PRINT A;B\n20 DATA 255,256\n") {
        assert!(asm.contains("; --- DATA pool (binary i16"), "{asm}");
    }
}

#[test]
fn a_poke_loader() {
    same_as_the_rom(
        "10 FOR I=0 TO 9:READ A:POKE 830+I,A:NEXT\n\
         20 FOR I=0 TO 9:PRINT PEEK(830+I);:NEXT\n\
         30 DATA 169,0,141,32,208,96,1,2,3,4\n",
    );
}
