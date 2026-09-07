//! SYS callees parse LEN(string reads) through the ordinary numeric bridge.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn source(setup: &str, param: &str) -> String {

    format!(
        "10 FORI=0TO12:READB:POKE49152+I,B:NEXT\n\
         20 {setup}\n30 SYS49152,{param}\n40 END\n\
         50 DATA32,253,174,32,158,173,32,221,189,32,30,171,96\n"
    )
}

#[test]
fn sys_string_lengths_keep_values_and_subscript_reads_alive() {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        for (setup, param, expected) in [
            ("DIM S$(3):S$(1)=\"AB\":I=1", "LEN(S$(I))", "2"),
            ("A$=\"A\"+CHR$(34)+CHR$(0)+\"B\"", "LEN(A$)", "4"),
            ("DIM S$(2,3):S$(1,2)=\"ABCDE\":I=1:J=2", "20-LEN(S$(I,J))/2", "17.5"),
            ("S$(0)=\"XYZ\":A$=\"Q\"", "LEN(S$(LEN(A$)-1))+LEN(A$)", "4"),
            ("DIM N(10):S$=\"AA\":N(2)=7", "N(LEN(S$))", "7"),
            ("A$=\"\"", "LEN(A$)", "0"),
        ] {
            let src = source(setup, param);
            let compiled = compile_with_options(
                &tokenize_program(&src).unwrap(),
                CompileOptions { profile, ..CompileOptions::default() },
            ).unwrap();
            assert!(compiled.diagnostics.sys_param_vars.iter().all(|s| s.substituted),
                "{profile:?}: missing read in {param}");
            let run = emu64::run_prg_to_end(&compiled.prg_bytes, 20_000_000).unwrap();
            assert!(run.clean_exit, "{profile:?}: {param}: {run:?}");
            assert_eq!(common::petscii_to_string(&run.output).trim(), expected,
                "{profile:?}: {param}");
        }
    }
}

#[test]
fn quoted_names_and_literal_only_lengths_do_not_acquire_runtime_staging() {
    for param in ["LEN(\"S$(A)\")", "LEN(\"\")", "\"LEN(S$(A))\""] {
        let src = format!("10 SYS828,{param}\n");
        let compiled = compile_with_options(&tokenize_program(&src).unwrap(), CompileOptions::default()).unwrap();
        assert!(!compiled.asm.contains("__SYS_TXTBUF"), "{param}");
        assert!(compiled.diagnostics.sys_param_vars.is_empty(), "{param}");
    }
}

#[test]
fn direct_string_parameters_use_lazy_descriptors() {
    let src = "10 DIM S$(3):A$=\"AB\"\n20 SYS828,S$(1),A$\n";
    let compiled = compile_with_options(&tokenize_program(src).unwrap(), CompileOptions::default()).unwrap();
    assert_eq!(compiled.diagnostics.sys_param_vars.len(), 2);
    assert!(compiled.diagnostics.sys_param_vars.iter().all(|s| s.substituted));
    assert!(compiled.asm.contains("__SYS_EVAL_ENTER:"));
}
