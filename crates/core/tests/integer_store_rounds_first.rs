mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

#[test]
fn an_integer_store_rounds_before_it_floors() {
    let src = "10 DIM B%(2):READ C,K\n20 A%=K*C:PRINT A%\n30 B%(1)=K*C:PRINT B%(1)\n\
               40 PRINT (K*C) AND -1\n50 PRINT -1 AND (K*C)\n60 PRINT NOT (K*C)\n\
               90 DATA -.9,40\n";
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let prg = tokenize_program(src).expect("tokenizes");
        let c = compile_with_options(
            &prg,
            CompileOptions {
                profile,
                ..CompileOptions::default()
            },
        )
        .unwrap_or_else(|e| panic!("compile ({profile:?}): {e}"));
        let r = emu64::run_prg_to_end(&c.prg_bytes, common::MAX_INSNS).expect("run");
        let out = common::petscii_to_string(&r.output);
        assert_eq!(
            out.split_whitespace().collect::<Vec<_>>().join(" "),
            "-37 -37 -36 -36 35",
            "({profile:?}), the ROM prints -37 -37 -36 -36 35"
        );
    }
}
