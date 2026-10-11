mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn same_as_the_rom(src: &str) {
    let prg = tokenize_program(src).expect("tokenizes");
    let r = emu64::run_basic_to_end(&prg, 20_000_000).expect("runs in the interpreter");
    let want = (common::petscii_to_string(&r.output), r.error_code, r.error_line);
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let c = compile_with_options(
            &prg,
            CompileOptions {
                profile,
                ..CompileOptions::default()
            },
        )
        .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}\n{src}"));
        let r = emu64::run_prg_to_end(&c.prg_bytes, 20_000_000).expect("runs");
        assert_eq!(
            (common::petscii_to_string(&r.output), r.error_code, r.error_line),
            want,
            "{profile:?}:\n{src}"
        );
    }
}

#[test]
fn an_integer_array_read_after_the_loop_is_out_of_bounds() {
    same_as_the_rom("10 DIM E%(4)\n15 FOR I=0 TO 4:E%(I)=3-I:NEXT:J%=A\n20 PRINT E%(I)\n");
}

#[test]
fn a_float_array_read_after_the_loop_is_out_of_bounds() {
    same_as_the_rom("10 DIM E(4)\n15 FOR I=0 TO 4:E(I)=3-I:NEXT:J%=A\n20 PRINT E(I)\n");
}

#[test]
fn a_compare_after_the_loop_reads_out_of_bounds() {
    same_as_the_rom(
        "10 DIM E%(4)\n15 FOR I=0 TO 4:X=E%(I):NEXT:J%=A\n20 IF E%(I)<>J% THEN PRINT \"T\"\n\
         30 PRINT \"E\"\n",
    );
}

#[test]
fn fuzz_a_long_condition_after_the_loop() {
    same_as_the_rom(
        "10 DIM D(4),E%(4),F(3,2)\n15 FOR I=0 TO 4:E%(I)=3-I:NEXT:J%=A\n\
         20 IF B>A OR .5>E%(J%) AND E%(I)<>F(J%,0) AND F(3,2)=F(0,0) THEN PRINT \"20\";\n",
    );
}

#[test]
fn the_counter_is_still_in_range_inside_the_body() {
    same_as_the_rom("10 DIM E%(4)\n15 FOR I=0 TO 4:E%(I)=I:PRINT E%(I);:NEXT:PRINT I\n");
}
