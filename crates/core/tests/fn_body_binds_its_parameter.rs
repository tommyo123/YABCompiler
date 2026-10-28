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
fn nested_calls_and_a_parameter_that_is_also_read_outside() {
    same_as_the_rom(
        "10 DEF FN A(X)=X*X+X/2-SIN(X)\n20 DEF FN B(X)=FN A(X+1)*2+X\n\
         30 X=7:FOR Q=1 TO 3:PRINT FN A(Q);FN B(Q);X:NEXT Q\n40 PRINT FN B(FN A(2))\n",
    );
    same_as_the_rom(
        "10 DEF FN A(X)=X*X*X+X\n20 DEF FN B(Y)=FN A(Y)+FN A(Y+1)+Y\n\
         30 FOR I=1 TO 3:PRINT FN B(I);:NEXT\n",
    );
}

#[test]
fn the_loop_variable_survives_a_call_on_it() {
    same_as_the_rom("10 DEF FN A(Q)=Q*3+Q*Q-1\n20 FOR Q=113 TO 116:PRINT FN A(8);Q:NEXT\n");
}

#[test]
fn runaway_recursion_is_out_of_memory() {
    same_as_the_rom("10 DEF FN R(X)=FN R(X+1)+1\n20 PRINT \"A\":PRINT FN R(1)\n");
}

#[test]
fn a_redefined_function_binds_the_same_way() {
    same_as_the_rom(
        "10 DEF FN A(X)=X+1+X*2+X*X\n20 PRINT FN A(1);\n30 DEF FN A(X)=X*10+X*X+X\n\
         40 PRINT FN A(2);X\n",
    );
}

#[test]
fn an_inlined_body_ignores_the_outer_variables_range() {
    same_as_the_rom("10 X=1:DEF FN A(X)=INT(X)\n20 PRINT FN A(2.5):X=X+1:IF X<3 THEN 20\n");
}
