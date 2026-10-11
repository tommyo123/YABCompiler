mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn same_as_the_rom(src: &str) {
    let prg = tokenize_program(src).expect("tokenizes");
    let r = emu64::run_basic_to_end(&prg, 20_000_000).expect("runs in the interpreter");
    let want = (common::petscii_to_string(&r.output), r.error_code, r.error_line);
    for profile in [Profile::Speed, Profile::Default, Profile::Size] {
        let c = compile_with_options(&prg, CompileOptions { profile, ..CompileOptions::default() })
            .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}"));
        let r = emu64::run_prg_to_end(&c.prg_bytes, 20_000_000).expect("runs");
        assert_eq!(
            (common::petscii_to_string(&r.output), r.error_code, r.error_line),
            want,
            "{profile:?}:\n{src}"
        );
    }
}

#[test]
fn a_copy_keeps_the_destination_shadow_current() {
    same_as_the_rom("10 READ B:DATA 7\n30 GOSUB 800\n50 X=(-B<40)\n790 PRINT B;X:END\n800 B=B\n900 RETURN\n");
}

#[test]
fn copies_between_shadowed_and_plain_variables() {
    same_as_the_rom(
        "10 READ A,B:DATA 3,-2.5\n20 C=A:D=B:A=D:B=C\n\
         30 FOR I=1 TO 3:E=A:A=B:B=E:PRINT A;B;E;A+1;B*2:NEXT\n40 PRINT A AND 7;B<0\n",
    );
}
