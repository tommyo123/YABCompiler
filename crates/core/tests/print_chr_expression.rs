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
fn a_failing_concatenation_prints_nothing() {
    same_as_the_rom("10 DIM S$(3):I=4\n20 PRINT CHR$(66)+S$(I);\n");
}

#[test]
fn a_comparison_prints_its_truth_value() {
    same_as_the_rom("10 PRINT CHR$(65)=\"A\";CHR$(65)<\"B\";CHR$(66)>\"A\"\n");
}

#[test]
fn whole_items_and_concatenations_print_alike() {
    same_as_the_rom(
        "10 A$=\"Q\":PRINT CHR$(65);CHR$(66),CHR$(67)\n\
         20 PRINT CHR$(65)+\"B\";CHR$(65)+CHR$(66);CHR$(ASC(\"(\"))+A$\n",
    );
}
