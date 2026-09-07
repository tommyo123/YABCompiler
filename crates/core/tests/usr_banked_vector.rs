//! USR is external machine code and may call BASIC ROM.
mod common;
use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn source(bytes: &[u8], body: &str) -> String {
    let data = bytes
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "10 FOR I=0 TO {}:READ D:POKE 49152+I,D:NEXT\n20 DATA {data}\n30 POKE785,0:POKE786,192\n{body}",
        bytes.len() - 1
    )
}

#[test]
fn a_rom_call_in_usr_preserves_argument_and_result_in_each_layout() {

    let src = source(
        &[32, 247, 183, 164, 20, 200, 165, 21, 76, 145, 179],
        "40 PRINT USR(1234);USR(2345)\n50 END\n",
    );
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        for extraram in [false, true] {
            let built = compile_with_options(
                &tokenize_program(&src).unwrap(),
                CompileOptions {
                    profile,
                    extraram,
                    ..CompileOptions::default()
                },
            )
            .unwrap();
            let run = emu64::run_prg_to_end(&built.prg_bytes, 2_000_000).unwrap();
            assert!(
                run.clean_exit,
                "{profile:?}/{extraram}: {:?}",
                run.error_banner()
            );
            assert_eq!(
                common::petscii_to_string(&run.output),
                " 1235  2346 \n",
                "{profile:?}/{extraram}"
            );
            let banked = built.asm.contains("__EXRAM_USRJT:");
            assert_eq!(banked, extraram);
        }
    }
}

#[test]
fn usr_sees_rom_bank_and_heap_strings_survive_its_return() {
    // Read $01 into FAC1.
    let src = source(
        &[165, 1, 168, 169, 0, 76, 145, 179],
        "40 B$=CHR$(65)+STR$(USR(7)):PRINT B$\n50 PRINT USR(8)\n60 END\n",
    );
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        for extraram in [false, true] {
            let built = compile_with_options(
                &tokenize_program(&src).unwrap(),
                CompileOptions {
                    profile,
                    extraram,
                    ..CompileOptions::default()
                },
            )
            .unwrap();
            let run = emu64::run_prg_to_end(&built.prg_bytes, 2_000_000).unwrap();
            assert!(
                run.clean_exit,
                "{profile:?}/{extraram}: {:?}",
                run.error_banner()
            );
            assert_eq!(
                common::petscii_to_string(&run.output),
                "A 55\n 55 \n",
                "{profile:?}/{extraram}"
            );
            if extraram {
                assert!(built.asm.contains("__EXRAM_MEMSIZ_SAVE:"));
            }
        }
    }
}
