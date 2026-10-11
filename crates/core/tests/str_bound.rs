mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn same_as_the_rom(src: &str) {
    let prg = tokenize_program(src).expect("tokenizes");
    let r = emu64::run_basic_to_end(&prg, 20_000_000).expect("runs in the interpreter");
    let want = (common::petscii_to_string(&r.output), r.error_code, r.error_line);
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
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
fn a_long_str_result_stays_in_its_buffer() {
    same_as_the_rom(
        "5 Z$=MID$(STR$(RND(-7)),2):PRINT LEN(Z$)\n20 READ T,A$:PRINT T;A$\n900 DATA 1,HELLO\n",
    );
}

#[test]
fn the_longest_str_results() {
    same_as_the_rom(
        "10 FOR I=1 TO 4:READ X:A$=STR$(X):PRINT A$;LEN(A$):NEXT:READ B$:PRINT B$\n\
         20 DATA -1.23456789E-38,-.000123456789,-1.98765432E-10,1.70141183E+38,AFTER\n",
    );
}
