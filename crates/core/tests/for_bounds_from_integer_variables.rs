mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn same_as_the_rom(src: &str) -> Vec<String> {
    let prg = tokenize_program(src).expect("tokenizes");
    let r = emu64::run_basic_to_end(&prg, 40_000_000).expect("runs in the interpreter");
    let want = (common::petscii_to_string(&r.output), r.error_code, r.error_line);
    let mut asms = Vec::new();
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let c = compile_with_options(&prg, CompileOptions { profile, ..CompileOptions::default() })
            .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}"));
        let r = emu64::run_prg_to_end(&c.prg_bytes, 40_000_000).expect("runs");
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
fn ends_and_starts_from_integer_variables() {
    let asms = same_as_the_rom(
        "10 N=PEEK(828)+300:B=PEEK(829)+200:A=PEEK(830)-5:C=PEEK(831)+7:D=PEEK(832)+1000\n\
         20 FOR K=N TO N+3:PRINT K;:NEXT:PRINT K\n\
         30 FOR K=1 TO B:NEXT:PRINT K\n\
         40 FOR K=A TO 2:PRINT K;:NEXT:PRINT K\n\
         50 FOR K=C TO 1 STEP -2:PRINT K;:NEXT:PRINT K\n\
         60 FOR K=D TO D:PRINT K;:NEXT:PRINT K\n",
    );
    for asm in asms {
        assert!(asm.contains("    LDA VI_B\n    STA FE_"), "B is copied into the end slot:\n{asm}");
    }
}

#[test]
fn bounds_from_the_counters_of_enclosing_loops() {
    same_as_the_rom(
        "10 FOR I=1 TO 5:FOR J=I TO 7:PRINT J;:NEXT:PRINT:NEXT\n\
         20 FOR I=0 TO 3:FOR J=0 TO I:PRINT I*10+J;:NEXT:NEXT:PRINT\n\
         30 FOR I=300 TO 302:FOR J=I-2 TO I:PRINT J;:NEXT:NEXT:PRINT\n\
         40 FOR I=3 TO 1 STEP -1:FOR J=I TO 3:PRINT J;:NEXT:FOR J=1 TO I:PRINT -J;:NEXT:NEXT\n",
    );
}

#[test]
fn bound_variables_written_inside_the_loop() {
    same_as_the_rom(
        "10 E=5:FOR K=1 TO E:E=E+10:PRINT K;E;:NEXT:PRINT K\n\
         20 S=2:FOR K=S TO 6:S=0:PRINT K;:NEXT:PRINT S\n\
         30 FOR I=1 TO 3:E=I*2:FOR K=I TO E:PRINT K;:NEXT:NEXT:PRINT\n",
    );
}
