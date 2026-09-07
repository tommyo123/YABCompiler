use super::*;

fn source(opaque_sys: bool) -> String {
    let mut source = if opaque_sys {
        "1 POKE 247,0:POKE 248,0:POKE 249,0:POKE 250,0:POKE 251,0:POKE 252,0:POKE 253,52:POKE 254,18:POKE 828,96:SYS 828\n".to_owned()
    } else {
        "1 POKE 253,52:POKE 254,18\n".to_owned()
    };
    for i in 0..24u8 {
        let letter = char::from(b'A' + i);
        source.push_str(&format!(
            "{} PRINT \"{}AB{letter}\";\n",
            10 + u32::from(i) * 10,
            letter.to_string().repeat(177)
        ));
    }
    source.push_str("500 A$=\"UNCHANGED\":PRINT A$;CHR$(253);CHR$(254):END\n");
    source
}

#[test]
fn both_packed_print_layouts_follow_forced_zp_relocation_through_banking() {
    for opaque_sys in [false, true] {
        let input = crate::tokenize_program(&source(opaque_sys)).unwrap();
        for extraram in [false, true] {
            let options = CompileOptions {
                profile: Profile::Size,
                extraram,
                force_extraram_off: !extraram,
                auto_reserve: false,
                ..CompileOptions::default()
            };
            let ordinary = compile_at_profile(&input, options.clone()).unwrap();
            let raw = emu64::run_prg_to_end(&ordinary.prg_bytes, 10_000_000).unwrap();
            assert!(raw.clean_exit);
            assert!(
                !ordinary.asm.contains("__STR_PRINT:\n    STA $FD\n"),
                "the source must force relocation, SYS {opaque_sys}, bank {extraram}"
            );
            for rescue in [LayoutRescue::PackedPrint, LayoutRescue::Compact] {
                let packed = compile_at_profile_rescue(&input, options.clone(), rescue).unwrap();
                let table = if matches!(rescue, LayoutRescue::Compact) {
                    "__CP_COUNTS:"
                } else {
                    "__CP_LEFT:"
                };
                assert!(
                    packed.asm.contains(table),
                    "the requested codec must be active"
                );
                let result = emu64::run_prg_to_end(&packed.prg_bytes, 10_000_000).unwrap();
                assert!(result.clean_exit, "{:?}", result.error_output);
                assert_eq!(
                    raw.output, result.output,
                    "opaque SYS {opaque_sys}, bank {extraram}, table {table}"
                );
            }
        }
    }
}
