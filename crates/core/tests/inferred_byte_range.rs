//! Preserve full BASIC evaluation when a proved non-byte range can wrap to a byte.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

fn build(source: &str, profile: Profile) -> yabcompiler_core::Compiled {
    compile_with_options(
        &tokenize_program(source).unwrap(),
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .unwrap()
}

#[test]
fn inferred_ranges_proved_outside_the_byte_window_cannot_wrap_into_it() {
    for source in [
        "10 POKE 2,0\n20 A=PEEK(2)+30000\n30 POKE 1024,A+32767+2770\n40 PRINT PEEK(1024)\n",
        "10 POKE 2,0\n20 A=PEEK(2)-32767\n30 POKE 1024,A+(-32768)\n40 PRINT PEEK(1024)\n",
        "10 POKE 2,0\n20 A=PEEK(2)+65000\n30 POKE 1024,A+536\n40 PRINT PEEK(1024)\n",
        "10 POKE 2,0\n20 A=PEEK(2)+30000\n30 PRINT CHR$(A+32767+2770)\n40 PRINT \"Z\"\n",
    ] {
        for profile in PROFILES {
            let result =
                emu64::run_prg_to_end(&build(source, profile).prg_bytes, common::MAX_INSNS)
                    .unwrap();
            assert_eq!(
                result.error_banner(),
                Some("?ILLEGAL QUANTITY  ERROR IN 30".into()),
                "{profile:?}: {source}"
            );
        }
    }
    // Include both ends of the negative alias interval and its adjacent values.
    for byte in [-1, 0, 1, 65, 255, 256] {
        let source = format!(
            "10 POKE 2,0\n20 A=PEEK(2)-32768\n30 POKE 1024,A+A+({byte})\n40 PRINT PEEK(1024)\n"
        );
        for profile in PROFILES {
            let result =
                emu64::run_prg_to_end(&build(&source, profile).prg_bytes, common::MAX_INSNS)
                    .unwrap();
            assert_eq!(
                result.error_banner(),
                Some("?ILLEGAL QUANTITY  ERROR IN 30".into()),
                "{profile:?}: {source}"
            );
        }
    }
}

#[test]
fn byte_proof_keeps_non_aliasing_integer_paths_and_legal_cancellation() {
    for source in [
        "10 POKE 2,0\n20 A=PEEK(2)+30000\n30 POKE 1024,A+30000\n40 PRINT \"Z\"\n",
        "10 POKE 2,0\n20 A=PEEK(2)-32512\n30 POKE 1024,A+(-32768)\n40 PRINT \"Z\"\n",
    ] {
        for profile in PROFILES {
            let compiled = build(source, profile);
            assert!(!compiled.asm.contains("__FAC_BYTE"), "{profile:?}");
            let result = emu64::run_prg_to_end(&compiled.prg_bytes, common::MAX_INSNS).unwrap();
            assert_eq!(
                result.error_banner(),
                Some("?ILLEGAL QUANTITY  ERROR IN 30".into())
            );
        }
    }
    for source in [
        "10 READ A,B,C,D\n20 POKE 1024,A+B+C+D\n30 PRINT PEEK(1024)\n40 DATA 32767,32767,-32767,-32702\n",
        "10 DIM A(2)\n20 READ A(0),A(1),A(2)\n30 POKE 1024,A(0)+A(1)+A(2)\n40 PRINT PEEK(1024)\n50 DATA 20,30,15\n",
    ] {
        for profile in PROFILES {
            let result =
                emu64::run_prg_to_end(&build(source, profile).prg_bytes, common::MAX_INSNS)
                    .unwrap();
            assert_eq!(result.error_banner(), None, "{profile:?}");
            assert_eq!(common::petscii_to_string(&result.output), " 65 \n");
        }
    }
}

#[test]
fn local_product_bounds_can_exceed_the_global_fact_cap() {
    // 64 cubed is 262144, but the global range disappears because 319 cubed exceeds its one-million fact cap.
    for value in ["A*A*A", "A*A*A+65", "-(A*A*A)+65"] {
        let source =
            format!("10 POKE 2,0\n20 A=PEEK(2)+64\n30 POKE 1024,{value}\n40 PRINT PEEK(1024)\n");
        for profile in PROFILES {
            let result =
                emu64::run_prg_to_end(&build(&source, profile).prg_bytes, common::MAX_INSNS)
                    .unwrap();
            assert_eq!(
                result.error_banner(),
                Some("?ILLEGAL QUANTITY  ERROR IN 30".into()),
                "{profile:?}: {value}"
            );
        }
    }
}

#[test]
fn positive_product_error_proof_survives_an_unbounded_upper_range() {
    for value in ["A*A*A*A*A", "A*A*A*A*A*(PEEK(3)+1)"] {
        let source = format!(
            "10 POKE 2,0:POKE 3,0\n20 A=PEEK(2)+64\n30 POKE 1024,{value}\n40 PRINT PEEK(1024)\n"
        );
        for profile in PROFILES {
            let result =
                emu64::run_prg_to_end(&build(&source, profile).prg_bytes, common::MAX_INSNS)
                    .unwrap();
            assert_eq!(
                result.error_banner(),
                Some("?ILLEGAL QUANTITY  ERROR IN 30".into()),
                "{profile:?}: {value}"
            );
        }
    }
}

#[test]
fn large_product_proof_preserves_zero_factors_and_cancellation() {
    for (value, expected) in [
        ("A*A*A*A*A*PEEK(3)", " 0 \n"),
        ("A*A*A*A*A-A*A*A*A*A+65", " 65 \n"),
    ] {
        let source = format!(
            "10 POKE 2,0:POKE 3,0\n20 A=PEEK(2)+64\n30 POKE 1024,{value}\n40 PRINT PEEK(1024)\n"
        );
        for profile in PROFILES {
            let result =
                emu64::run_prg_to_end(&build(&source, profile).prg_bytes, common::MAX_INSNS)
                    .unwrap();
            assert_eq!(result.error_banner(), None, "{profile:?}: {value}");
            assert_eq!(common::petscii_to_string(&result.output), expected);
        }
    }
}

#[test]
fn unsigned_word_byte_addition_keeps_the_final_carry() {
    for input in [0, 1, 254, 255] {
        for delta in [1, 255, 256, 257] {
            let source = format!(
                "10 POKE 2,{input}\n20 A=PEEK(2)*257\n30 POKE 1024,A+{delta}\n40 PRINT PEEK(1024)\n"
            );
            let expected = input * 257 + delta;
            for profile in PROFILES {
                let result =
                    emu64::run_prg_to_end(&build(&source, profile).prg_bytes, common::MAX_INSNS)
                        .unwrap();
                if expected <= 255 {
                    assert_eq!(result.error_banner(), None, "{profile:?}: {source}");
                    assert_eq!(
                        common::petscii_to_string(&result.output),
                        format!(" {expected} \n")
                    );
                } else {
                    assert_eq!(
                        result.error_banner(),
                        Some("?ILLEGAL QUANTITY  ERROR IN 30".into()),
                        "{profile:?}: {source}"
                    );
                }
            }
        }
    }
}

#[test]
fn unsigned_word_byte_subtraction_keeps_the_final_borrow() {
    for input in [0, 1, 254, 255] {
        let source = format!(
            "10 POKE 2,{input}\n20 A=PEEK(2)*257\n30 POKE 1024,0-A\n40 PRINT PEEK(1024)\n"
        );
        for profile in PROFILES {
            let result =
                emu64::run_prg_to_end(&build(&source, profile).prg_bytes, common::MAX_INSNS)
                    .unwrap();
            if input == 0 {
                assert_eq!(result.error_banner(), None, "{profile:?}");
                assert_eq!(common::petscii_to_string(&result.output), " 0 \n");
            } else {
                assert_eq!(
                    result.error_banner(),
                    Some("?ILLEGAL QUANTITY  ERROR IN 30".into()),
                    "{profile:?}: {source}"
                );
            }
        }
    }
}

#[test]
fn inferred_array_linear_arithmetic_preserves_overflow_and_cancellation() {
    for (values, expected) in [
        (vec![30000, 30000, 5601], None),
        (vec![-32768, -32768], None),
        (vec![-32768, -32768, 0], None),
        (vec![20, 30, 15], Some(65)),
        (vec![30000, 30000, 5601, -32768, -32768], Some(65)),
    ] {
        let refs: Vec<_> = (0..values.len()).map(|i| format!("A({i})")).collect();
        let data = values.iter().map(i32::to_string).collect::<Vec<_>>().join(",");
        let source = format!(
            "10 DIM A({})\n20 READ {}\n30 POKE 1024,{}\n40 PRINT PEEK(1024)\n50 DATA {data}\n",
            values.len() - 1,
            refs.join(","),
            refs.join("+")
        );
        for profile in PROFILES {
            let result =
                emu64::run_prg_to_end(&build(&source, profile).prg_bytes, common::MAX_INSNS)
                    .unwrap();
            if let Some(expected) = expected {
                assert_eq!(result.error_banner(), None, "{profile:?}: {source}");
                assert_eq!(common::petscii_to_string(&result.output), format!(" {expected} \n"));
            } else {
                assert_eq!(
                    result.error_banner(),
                    Some("?ILLEGAL QUANTITY  ERROR IN 30".into()),
                    "{profile:?}: {source}"
                );
            }
        }
    }
}

#[test]
fn inferred_array_byte_fallback_preserves_destructive_index_read_order() {

    for address in ["1024", "1024+I", "N", "1024+PEEK(2)"] {
        let source = format!(
            "10 DIM A(1),B(1),C(0):POKE 2,0:I=PEEK(2):N=PEEK(2)+1024\n\
             20 A(0)=20:A(1)=30:B(0)=40:B(1)=60:C(0)=5\n\
             30 POKE {address},A(PEEK(56333) AND 1)+B(PEEK(56333) AND 1)+C(0)\n\
             40 PRINT PEEK(1024)\n"
        );
        for profile in PROFILES {
            let mut sys = emu64::System::new();
            let entry = sys.load_prg(&build(&source, profile).prg_bytes).unwrap();
            sys.start_at(entry);
            let sp = sys.cpu.regs.sp;
            sys.mem.ram[0x100 | sp as usize] = 0xFF;
            sys.mem.ram[0x100 | sp.wrapping_sub(1) as usize] = 0xFD;
            sys.cpu.regs.sp = sp.wrapping_sub(2);
            let mut reads = 0;
            let mut completed = false;
            for _ in 0..common::MAX_INSNS {
                let pc = sys.cpu.regs.pc;
                if pc == 0xFFFE {
                    completed = true;
                    break;
                }
                if matches!(sys.mem.read_byte(pc), 0xAD | 0xAC | 0xAE)
                    && sys.mem.read_byte(pc.wrapping_add(1)) == 0x0D
                    && sys.mem.read_byte(pc.wrapping_add(2)) == 0xDC
                {
                    sys.mem.io[0xC0D] = u8::from(reads == 0);
                    reads += 1;
                }
                if !sys.step() {
                    break;
                }
            }
            assert!(completed, "{profile:?}: {address}");
            assert_eq!(reads, 2, "{profile:?}: {address}");
            assert_eq!(sys.error_code, None, "{profile:?}: {address}");
            assert_eq!(common::petscii_to_string(&sys.output), " 75 \n");
        }
    }
}

#[test]
fn inferred_array_byte_fallback_preserves_the_first_index_error() {

    let source = "10 DIM A(0),B(0),C(0):POKE 2,255\n\
                  20 A(0)=20:B(0)=40:C(0)=5:I=PEEK(2)*257\n\
                  30 POKE 1024,A(I)+B(1)+C(0)\n40 PRINT PEEK(1024)\n";
    for profile in PROFILES {
        let result = emu64::run_prg_to_end(&build(source, profile).prg_bytes, common::MAX_INSNS)
            .unwrap();
        assert_eq!(
            result.error_banner(),
            Some("?ILLEGAL QUANTITY  ERROR IN 30".into()),
            "{profile:?}"
        );
    }
}
