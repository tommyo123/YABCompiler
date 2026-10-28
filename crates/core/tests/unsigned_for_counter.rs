//! Unsigned FOR counters cover literal bounds within 0..65535.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

type Outcome = (Vec<u8>, Option<u8>, Option<u16>);

const BUDGET: u64 = 20_000_000;

fn interpreted(src: &str) -> Outcome {
    let prg = tokenize_program(src).expect("tokenizes");
    let r = emu64::run_basic_to_end(&prg, BUDGET).expect("runs in the interpreter");
    assert!(r.ready_reached, "the interpreter reaches READY:\n{src}");
    (r.output, r.error_code, r.error_line)
}

fn compiled(src: &str, profile: Profile) -> (Outcome, String) {
    let prg = tokenize_program(src).expect("tokenizes");
    let c = compile_with_options(
        &prg,
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}\n{src}"));
    let r = emu64::run_prg_to_end(&c.prg_bytes, BUDGET).expect("runs");
    assert!(
        r.clean_exit || r.error_code.is_some(),
        "ends ({profile:?}):\n{src}"
    );
    ((r.output, r.error_code, r.error_line), c.asm)
}

fn same_as_the_rom(src: &str) {
    let want = interpreted(src);
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let (got, _) = compiled(src, profile);
        assert_eq!(
            (common::petscii_to_string(&got.0), got.1, got.2),
            (common::petscii_to_string(&want.0), want.1, want.2),
            "{profile:?}:\n{src}"
        );
    }
}

#[test]
fn a_loader_loop_counts_without_float_math() {
    let src = "\
10 FOR J=49152 TO 49159:READ A:POKE J,A:NEXT J
20 PRINT J;PEEK(49152);PEEK(49159)
30 DATA 1,2,3,4,5,6,7,8
";
    same_as_the_rom(src);
    let (_, asm) = compiled(src, Profile::Default);
    assert!(asm.contains("JSR __GIVAYF_U16"), "{asm}");
    assert!(!asm.contains("JSR $B867"), "no FADD left:\n{asm}");
}

#[test]
fn the_counter_keeps_its_value_after_the_loop() {
    same_as_the_rom("10 FOR J=40000 TO 40010 STEP 3:NEXT:PRINT J\n");
    same_as_the_rom("10 FOR J=65530 TO 65534 STEP 2:POKE J-16000,1:NEXT:PRINT J\n");
    same_as_the_rom("10 FOR J=50000 TO 40000:PRINT J;:NEXT:PRINT J\n");
    same_as_the_rom("10 FOR J=65535 TO 65530 STEP -1:NEXT:PRINT J\n");
}

#[test]
fn bounds_across_32767() {
    same_as_the_rom("10 FOR J=32760 TO 32780 STEP 5:PRINT J;:NEXT:PRINT J\n");
}

#[test]
fn an_end_whose_next_value_leaves_65535_stays_a_float_loop() {
    same_as_the_rom("10 FOR J=65530 TO 65535:NEXT:PRINT J\n");
}

#[test]
fn reads_outside_an_address_see_the_current_value() {
    same_as_the_rom(
        "\
10 FOR J=40000 TO 40003:IF J=40002 THEN PRINT \"TWO\";
20 PRINT J+1;J*2;-J;STR$(J);:X=J/3:Y%=J-30000:NEXT:PRINT X;Y%
30 FOR J=50000 TO 50002:A(1)=J:NEXT:PRINT A(1)
",
    );
}

#[test]
fn offsets_and_nested_loops() {
    same_as_the_rom(
        "\
10 FOR J=49152 TO 49160 STEP 4:FOR K=0 TO 3:POKE J+K,K+J-49150:NEXT K,J
20 S=0:FOR J=49152 TO 49163:S=S+PEEK(J):NEXT:PRINT S;J
30 FOR J=49152 TO 49162:POKE J+200,PEEK(J+3):NEXT:PRINT PEEK(49352);PEEK(49362);J
",
    );
}

#[test]
fn an_address_past_65535_is_still_illegal_quantity() {
    same_as_the_rom("10 FOR J=65000 TO 65010:X=PEEK(J+600):NEXT\n");
}

#[test]
fn subroutines_and_functions_see_the_counter() {
    same_as_the_rom(
        "\
10 FOR J=49152 TO 49155:GOSUB 100:NEXT:PRINT J
20 DEF FN F(X)=X+J:FOR J=40000 TO 40002:PRINT FN F(1);:NEXT
30 FOR J=40000 TO 40002:ON 1 GOSUB 100:NEXT
40 FOR J=33000 TO 33010:IF J>33005 THEN 60
50 NEXT
60 PRINT J
70 END
100 PRINT J;:RETURN
",
    );
}
