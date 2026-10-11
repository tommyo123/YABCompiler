mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

#[track_caller]
fn check(src: &str, want: &str) {
    for profile in PROFILES {
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
        assert_eq!(
            common::petscii_to_string(&r.output).trim_end(),
            want,
            "({profile:?}):\n{src}"
        );
    }
}

#[test]
fn the_index_written_after_its_element_was_read() {
    check(
        "10 READ B:J%=1:D(1)=B\n20 J%=(B<>D(J%)):PRINT D(J%)\n30 DATA .25\n",
        " 0",
    );
}

#[test]
fn the_index_written_in_a_subroutine() {
    let src = "10 READ A,B:J%=1:D(1)=B\n20 GOSUB 100\n30 PRINT (A<D(J%))\n40 END\n\
               100 J%=J%*B+(A<D(J%))-.5:RETURN\n110 DATA 0,.25\n";
    for profile in PROFILES {
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
        assert!(
            r.error_banner().is_some_and(|b| b.contains("ILLEGAL QUANTITY")),
            "({profile:?}) expected ?ILLEGAL QUANTITY, got {:?} / {:?}",
            r.error_banner(),
            common::petscii_to_string(&r.output)
        );
    }
}
