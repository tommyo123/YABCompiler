mod common;

use yabcompiler_core::{CompileOptions, Compiled, Profile, compile_with_options, tokenize_program};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

fn compile(src: &str, profile: Profile) -> Compiled {
    compile_with_options(
        &tokenize_program(src).expect("tokenizes"),
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("{profile:?}: {e}\n{src}"))
}

fn check(src: &str, expected: &str) {
    for profile in PROFILES {
        let c = compile(src, profile);
        assert_eq!(
            common::run_clean(&c.prg_bytes).as_deref(),
            Some(expected),
            "{profile:?}\n{src}"
        );
    }
}

fn direct(src: &str, profile: Profile) -> (String, Vec<u8>) {

    let tokenized = tokenize_program(src).unwrap();
    let parsed = yabcompiler_core::Program::parse(&tokenized).unwrap();
    let ast = yabcompiler_core::parse::program(&parsed).unwrap();
    let ir = yabcompiler_core::ir::lower(&ast).unwrap();
    let asm = yabcompiler_core::codegen::emit_with_profile(&ir, profile).unwrap();
    let asm = yabcompiler_core::peephole::run(&asm, profile);
    let bytes = asm6502::Assembler6502::new().assemble_bytes(&asm).unwrap();
    (asm, yabcompiler_core::pack::pack(&bytes))
}

const LOOP: &str = "10 M=240\n20 M=M-1\n30 POKE 1025,M AND 255\n40 POKE 1026,M+1\n50 POKE 1027,M+2\n60 IF M>200 THEN 20\n";

#[test]
fn a_dead_hot_mirror_disappears_and_the_loop_stays_fast() {
    // Remains a Float scalar with a shadow: the promotion interval fixpoint cannot bound this backwards-branch guard.
    let src = "10 X=0\n20 X=X+1:POKE1024+X,1:IFX<200THEN20\n";
    for profile in PROFILES {
        let c = compile(src, profile);
        assert!(
            c.asm.contains("ZSI_V_X"),
            "lost the tested shadow: {}",
            c.asm
        );
        assert!(
            !c.asm.lines().any(|line| line.trim() == "V_X:"),
            "dead slot kept: {}",
            c.asm
        );
        assert!(!c.asm.contains("JSR $B391"), "dead GIVAYF kept: {}", c.asm);
        assert!(!c.asm.contains("JSR $BBD4"), "dead MOVMF kept: {}", c.asm);
        let run = emu64::run_prg_to_end(&c.prg_bytes, 8_000).expect("runs");
        assert!(run.clean_exit, "mirror cost returned ({profile:?})");
        assert_eq!(&run.screen[1..201], &[1; 200]);
    }
}

#[test]
fn mixed_fac_memory_operands_keep_their_current_value() {
    check(
        &format!("{LOOP}80 POKE 820,6:Y=PEEK(820)/2\n90 PRINT M-Y\n"),
        " 197 \n",
    );
}

#[test]
fn the_optimistic_poke_island_hole_keeps_its_mirror() {
    check(
        "10 M=240\n20 M=M-1:IF M=0 THEN 80\n30 Y=PEEK(53281)/7\n40 POKE1024,M-Y\n45 POKE1027,M-Y\n50 POKE1025,M AND255\n60 POKE1026,M+1\n70 GOTO20\n80 PRINT PEEK(1024)\n",
        " 1 \n",
    );
}

#[test]
fn float_comparison_and_zero_comparison_see_current_values() {
    check(
        &format!(
            "{LOOP}80 POKE820,6:Y=PEEK(820)/2\n85 IF M>Y THEN PRINT\"BIG\"\n90 IF M<>0 THEN PRINT M\n"
        ),
        "BIG\n 200 \n",
    );
}

#[test]
fn speed_profile_byte_copy_keeps_its_source_slot() {
    check(&format!("{LOOP}80 Z=M:PRINT Z\n"), " 200 \n");
}

#[test]
fn transcendental_and_string_conversion_reads_remain_correct() {
    check(
        &format!("{LOOP}80 PRINT ABS(M)+SQR(4)\n90 PRINT LEN(STR$(M))\n"),
        " 202 \n 4 \n",
    );
}

#[test]
fn def_fn_reads_and_parameter_save_restore_keep_their_values() {
    check(
        &format!("5 DEF FNA(Q)=Q+M\n{LOOP}80 PRINT FNA(1)\n90 Q=9:PRINT FNA(2);Q\n"),
        " 201 \n 202  9 \n",
    );
}

#[test]
fn general_multiply_and_nested_array_indices_are_not_assumed_integer_islands() {
    check(
        &format!("5 DIM A(3,4)\n{LOOP}80 N=PEEK(1027)-200\n90 A(N,3)=M*N\n100 PRINT A(2,3)\n"),
        " 400 \n",
    );
}

#[test]
fn an_omitted_conversion_cannot_seed_a_fake_fac_cache_value() {
    check(
        "10 X=0\n20 X=X+1:POKE1024+X,1:IF X<20 THEN20\n30 PRINT X;X+1;X/2\n40 Z=X:PRINT Z\n",
        " 20  21  10 \n 20 \n",
    );
}

#[test]
fn multiple_shadows_and_a_cold_float_path_are_kept_consistent() {
    check(
        "10 X=0:Y=0\n20 X=X+1:Y=Y+2\n30 POKE1024+X,Y:IF X<40 THEN20\n40 IF PEEK(1064)=80 THEN PRINT X/2;Y/2\n",
        " 20  40 \n",
    );
}

#[test]
fn clr_resets_the_retained_shadow_too() {
    check(
        "10 X=0\n20 X=X+1:POKE1024+X,1:IF X<20 THEN20\n30 CLR\n40 PRINT X\n",
        " 0 \n",
    );
}

#[test]
fn read_writeback_and_unsigned_float_for_fallback_remain_correct() {
    check(
        "10 DATA 2,3,4\n20 FOR I=51200 TO51202\n30 READ X:POKE I,X:POKE I+10,X:POKE I+20,X:POKE I+30,X\n40 GOSUB100:NEXT\n50 PRINT I:END\n100 PRINT X;:RETURN\n",
        " 2  3  4  51203 \n",
    );
}

#[test]
fn sys_text_substitution_still_has_a_live_numeric_source() {

    check(
        &format!(
            "{LOOP}80 FOR J=0 TO9:READ B:POKE828+J,B:NEXT\n90 SYS828,M\n95 PRINT PEEK(679)\n100 DATA32,253,174,32,158,183,142,167,2,96\n"
        ),
        " 200 \n",
    );
}

#[test]
fn actual_float_shadow_readers_restore_mirrors_after_the_probe() {
    for (tail, expected) in [
        (
            "80 POKE820,6:Y=PEEK(820)/2\n90 POKE1024,M-Y:PRINT PEEK(1024)\n",
            " 197 \n",
        ),
        (
            "80 POKE820,6:Y=PEEK(820)/2\n90 IF M>Y THEN PRINT\"BIG\"\n",
            "BIG\n",
        ),
        ("80 Z=M:PRINT Z\n", " 200 \n"),
    ] {
        for profile in PROFILES {
            let src = format!("{LOOP}{tail}");
            let (asm, bytes) = direct(&src, profile);
            assert!(
                asm.contains("ZSI_V_M"),
                "test did not create a shadow: {asm}"
            );

            if !tail.starts_with("80 Z=M") {
                assert!(asm.contains("V_M:"), "live float slot was omitted: {asm}");
            }
            assert_eq!(
                common::run_clean(&bytes).as_deref(),
                Some(expected),
                "{profile:?}\n{src}"
            );
        }
    }
}

#[test]
fn assembler_rejects_a_reader_of_an_omitted_slot() {
    assert!(
        asm6502::Assembler6502::new()
            .assemble_bytes("*=$080D\nLDA #<V_M\nLDY #>V_M\nJSR $BBA2\nRTS\n")
            .is_err()
    );
}

#[test]
fn omitted_givayf_does_not_cache_a_same_statement_float_copy() {
    let src = "10 X=0\n20 X=X+1:Z=X:POKE1024+X,1:IF X<5 THEN20\n30 PRINT Z\n";
    for profile in PROFILES {
        let (asm, bytes) = direct(src, profile);
        assert!(
            asm.contains("ZSI_V_X"),
            "test did not create a shadow: {asm}"
        );
        assert_eq!(
            common::run_clean(&bytes).as_deref(),
            Some(" 5 \n"),
            "{profile:?}: {asm}"
        );
    }
}
