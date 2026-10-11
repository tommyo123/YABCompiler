mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

type Outcome = (Vec<u8>, Option<u8>, Option<u16>);

fn build(src: &str, profile: Profile) -> yabcompiler_core::Compiled {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(
        &prg,
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}\n{src}"))
}

fn same_as_the_rom(src: &str) {
    let prg = tokenize_program(src).expect("tokenizes");
    let r = emu64::run_basic_to_end(&prg, 20_000_000).expect("runs in the interpreter");
    let want: Outcome = (r.output, r.error_code, r.error_line);
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let r = emu64::run_prg_to_end(&build(src, profile).prg_bytes, 20_000_000).expect("runs");
        assert_eq!(
            (common::petscii_to_string(&r.output), r.error_code, r.error_line),
            (common::petscii_to_string(&want.0), want.1, want.2),
            "{profile:?}:\n{src}"
        );
    }
}

#[test]
fn in_bounds_reads_on_either_side_of_and_and_or() {
    same_as_the_rom(
        "10 DIM A(5),B%(3,3):READ X,P,Q:A(2)=P:A(4)=Q:B%(1,2)=3\n\
         20 IF X=1 AND A(2)=P THEN PRINT \"1\";\n\
         30 IF X=0 AND A(2)=P THEN PRINT \"2\";\n\
         40 IF X=1 OR A(2)=0 THEN PRINT \"3\";\n\
         50 IF X=0 OR A(2)=P THEN PRINT \"4\";\n\
         60 IF X=0 AND B%(1,2)=3 THEN PRINT \"5\";\n\
         70 IF A(1)=0 AND X=0 OR A(3)=0 AND X=1 THEN PRINT \"6\";\n\
         80 FOR I=0 TO 5:IF X=1 AND A(I)>0 THEN PRINT I;\n\
         90 NEXT:Z=A(4):IF X=0 AND A(2)=P THEN PRINT \"7\";\n\
         95 PRINT A(2)+Z\n\
         99 DATA 1,7.5,9.25\n",
    );
}

#[test]
fn a_read_past_the_bounds_still_raises_bad_subscript() {
    same_as_the_rom("10 DIM A(5):X=0:K=9\n20 IF X=1 AND A(K)=0 THEN PRINT \"T\"\n30 PRINT \"E\"\n");
    same_as_the_rom("10 DIM A(5):X=1\n20 IF X=1 OR A(6)=0 THEN PRINT \"T\"\n30 PRINT \"E\"\n");
    same_as_the_rom("10 X=0\n20 IF X=1 AND A(11)=0 THEN PRINT \"T\"\n30 PRINT \"E\"\n");
    same_as_the_rom("10 X=0\n20 IF X=1 AND A(10)=0 THEN PRINT \"T\"\n30 PRINT \"E\"\n");
}

#[test]
fn an_array_dimensioned_at_run_time_is_still_read() {
    same_as_the_rom("10 READ N,X:DIM A(N)\n20 IF X=1 AND A(3)=0 THEN PRINT \"T\"\n30 PRINT \"E\"\n40 DATA 2,0\n");
}

#[test]
fn an_in_bounds_read_is_skipped_without_building_booleans() {
    let asm = build(
        "10 DIM A(5):READ X:IF X=1 AND A(2)=7 THEN PRINT \"T\"\n20 DATA 1\n",
        Profile::Default,
    )
    .asm;
    assert!(!asm.contains("__LOGIC_B") && !asm.contains("__BB_F_"), "{asm}");
}
