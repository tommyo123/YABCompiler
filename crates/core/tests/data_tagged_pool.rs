mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn same_as_the_rom_tagged(src: &str) {
    let prg = tokenize_program(src).expect("tokenizes");
    let r = emu64::run_basic_to_end(&prg, 40_000_000).expect("runs in the interpreter");
    let want = (common::petscii_to_string(&r.output), r.error_code, r.error_line);
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let c = compile_with_options(&prg, CompileOptions { profile, ..CompileOptions::default() })
            .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}"));
        assert!(c.asm.contains("__READ_TAGGED:"), "{profile:?}: the pool is not tagged");
        let r = emu64::run_prg_to_end(&c.prg_bytes, 40_000_000).expect("runs");
        assert_eq!(
            (common::petscii_to_string(&r.output), r.error_code, r.error_line),
            want,
            "{profile:?}:\n{src}"
        );
    }
}

fn filler() -> String {
    let n: Vec<String> = (0..160).map(|i| (i * 37 % 300).to_string()).collect();
    format!("990 DATA {}\n", n.join(","))
}

#[test]
fn numbers_around_the_tag_boundaries() {
    same_as_the_rom_tagged(&format!(
        "10 DIM E(9),F%(9):FOR I=0 TO 9:READ E(I):NEXT:FOR I=0 TO 9:PRINT E(I);:NEXT:PRINT\n\
         20 RESTORE:FOR I=0 TO 8:READ F%(I):PRINT F%(I);:NEXT:PRINT\n\
         30 RESTORE:READ A,B,C,D:PRINT A;B;C;D:S=0:FOR I=1 TO 166:READ X:S=S+X:NEXT:PRINT S\n\
         900 DATA 0,61,62,255,256,32767,1,2,32768,1.5\n{}999 DATA NEVER\n",
        filler()
    ));
}

#[test]
fn string_reads_of_binary_items() {
    same_as_the_rom_tagged(&format!(
        "5 Z$=MID$(STR$(RND(-7)),2):PRINT LEN(Z$)\n\
         10 DIM Q$(9):FOR I=0 TO 9:READ Q$(I):NEXT:FOR I=0 TO 9:PRINT Q$(I);\"/\";LEN(Q$(I));:NEXT:PRINT\n\
         20 RESTORE:FOR I=0 TO 9:READ A$:READ B:PRINT A$;B;VAL(A$);:NEXT:PRINT\n\
         30 C$=Q$(1)+Q$(4):PRINT C$;FRE(0)>0\n\
         40 FOR I=1 TO 4:READ A$:PRINT A$;LEN(A$);:NEXT:PRINT\n\
         900 DATA 0,61,62,255,256,32767,HI,,007,32768\n\
         910 DATA 1,2,3,4,5,6,7,8,9,10\n\
         915 DATA -0,1E3,+4,1.0\n{}",
        filler()
    ));
}

#[test]
fn a_bad_item_after_binary_ones() {
    same_as_the_rom_tagged(&format!(
        "10 FOR I=1 TO 5:READ X:PRINT X;:NEXT\n900 DATA 7,300,0,1.5,4X\n{}",
        filler()
    ));
}
