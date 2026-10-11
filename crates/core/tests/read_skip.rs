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
fn dead_string_reads_skip_their_items() {
    same_as_the_rom(
        "10 DIM B(20):FOR I=1 TO 3:READ A$:NEXT:GET A$:PRINT \"[\"A$\"]\"\n\
         20 READ A$,B(LEN(A$)):PRINT B(2),B(0)\n\
         30 READ C$:C$=\"X\":PRINT C$\n\
         40 READ D$,D$:PRINT D$\n\
         50 FOR I=1 TO 3:READ E$:NEXT\n\
         60 DATA 1,HELLO,3,AB,7,Q,R,S,T\n",
    );
}

#[test]
fn a_target_read_after_return_is_kept() {
    same_as_the_rom("10 GOSUB 100:PRINT A$:GOSUB 100:PRINT A$:END\n100 READ A$:RETURN\n110 DATA 5,X\n");
}

#[test]
fn a_skipped_read_past_the_end_is_out_of_data() {
    same_as_the_rom("10 FOR I=1 TO 5:READ A$:NEXT\n20 DATA 1,2\n");
}

#[test]
fn skipped_reads_leave_a_number_pool_binary() {
    for asm in same_as_the_rom("10 FOR I=1 TO 3:READ A$:NEXT:GET A$:READ X:PRINT X\n20 DATA 1,2,3,4\n") {
        assert!(asm.contains("; --- DATA pool (binary u8"), "{asm}");
    }
}
