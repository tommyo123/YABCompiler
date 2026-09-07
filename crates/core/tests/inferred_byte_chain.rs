//! Compound byte conversions preserve carries until after cancellation.
mod common;
use yabcompiler_core::{compile_with_options, tokenize_program, CompileOptions, Profile};

fn build(source: &str, profile: Profile, banked: bool) -> yabcompiler_core::Compiled {
    compile_with_options(
        &tokenize_program(source).unwrap(),
        CompileOptions {
            profile,
            extraram: banked,
            force_extraram_off: !banked,
            ..CompileOptions::default()
        },
    )
    .unwrap()
}

fn check(source: &str, expected: Option<i32>, marker: bool) {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        for banked in [false, true] {
            let compiled = build(source, profile, banked);
            if marker {
                assert!(
                    compiled.asm.contains("__I24BYTE_OK_") || compiled.asm.contains("__I32BYTE_OK_"),
                    "missing checked path: {profile:?}/{banked}: {source}"
                );
            }
            let result = emu64::run_prg_to_end(&compiled.prg_bytes, common::MAX_INSNS).unwrap();
            if let Some(expected) = expected {
                assert_eq!(
                    result.error_banner(),
                    None,
                    "{profile:?}/{banked}: {source}"
                );
                assert_eq!(
                    common::petscii_to_string(&result.output),
                    format!(" {expected} \n"),
                    "{profile:?}/{banked}: {source}"
                );
            } else {
                assert_eq!(
                    result.error_banner(),
                    Some("?ILLEGAL QUANTITY  ERROR IN 30".into()),
                    "{profile:?}/{banked}: {source}"
                );
            }
        }
    }
}

#[test]
fn inferred_scalar_chains_keep_positive_negative_and_cancelled_carries() {
    for (expr, expected) in [
        ("A+B+B", None),
        ("A+(B+B)", None),
        ("-A-B-B", None),
        ("-(A+B+B)", None),
        ("A+B+B-A", Some(2)),
        ("A-(A+B+B)+2", Some(0)),
        ("(A+B+B)-(A+B+B)+65", Some(65)),
        ("A+B+B-A-B-B+255", Some(255)),
        ("A+B+B-A-B-B-1", None),
        ("A+B+B-(A+B)+65", Some(66)),
        ("A+B+B+D+D+256", None),
        ("A+B+B-A+D+D+258", Some(4)),
    ] {
        let source = format!("10 POKE 2,255:POKE 3,1:POKE 4,0\n20 A=PEEK(2)*257:B=PEEK(3) AND 1:D=PEEK(4)-128\n30 POKE 1024,{expr}\n40 PRINT PEEK(1024)\n");
        check(&source, expected, true);
    }
}

#[test]
fn the_same_wide_static_range_preserves_each_legal_byte_edge() {
    for (a, b, c, expected) in [
        (0, 0, 0, 0),
        (0, 1, 0, 2),
        (0, 0, 255, 255),
        (0, 1, 255, 257),
        (255, 1, 0, 65537),
    ] {
        let source = format!("10 POKE 2,{a}:POKE 3,{b}:POKE 4,{c}\n20 A=PEEK(2)*257:B=PEEK(3) AND 1:C=PEEK(4)\n30 POKE 1024,A+B+B+C\n40 PRINT PEEK(1024)\n");
        check(&source, (expected <= 255).then_some(expected), true);
    }
}

#[test]
fn existing_declared_and_non_aliasing_consumers_keep_their_paths() {
    check("10 READ A%,B%,C%\n20 REM SOURCE DECLARED CONTROL\n30 POKE 1024,A%+B%+C%\n40 PRINT PEEK(1024)\n50 DATA 30000,30000,5537\n", None, false);
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let source = "10 POKE 2,0\n20 A=PEEK(2)+30000\n30 POKE 1024,A+30000\n40 PRINT PEEK(1024)\n";
        let compiled = build(source, profile, false);
        assert!(!compiled.asm.contains("__I24BYTE_OK_"));
        assert!(!compiled.asm.contains("__FAC_BYTE"));
    }
}

#[test]
fn widening_reads_volatile_leaves_once_in_source_order() {

    for (expr, expected) in [
        ("A+A+A+PEEK(56333)-A-A-A-PEEK(56333)+65", " 66 \n"),
        ("A+A+A-(PEEK(56333)-PEEK(56333))-A-A-A+65", " 64 \n"),
        ("-(A+A+A+PEEK(56333))+A+A+A+PEEK(56333)+65", " 64 \n"),
        ("A+A+A+(PEEK(56333) AND 1)-(PEEK(56333) AND 1)-A-A-A+65", " 66 \n"),
        ("A+A+A-((PEEK(56333) AND 1)-(PEEK(56333) AND 1))-A-A-A+65", " 64 \n"),
        ("A+A+A+(PEEK(56333) AND 0)-PEEK(56333)-A-A-A+65", " 65 \n"),
    ] {
        let source =
            format!("10 POKE 2,255\n20 A=PEEK(2)*128\n30 POKE 1024,{expr}\n40 PRINT PEEK(1024)\n");
        for profile in [Profile::Default, Profile::Speed, Profile::Size] {
            for banked in [false, true] {
                let compiled = build(&source, profile, banked);
                if !expr.contains("AND 0") {
                    assert!(compiled.asm.contains("__I24BYTE_OK_"), "{profile:?}/{banked}: {expr}\n{}", compiled.asm);
                }
                let mut sys = emu64::System::new();
                let entry = sys.load_prg(&compiled.prg_bytes).unwrap();
                sys.start_at(entry);
                let sp = sys.cpu.regs.sp;
                sys.mem.ram[0x100 | sp as usize] = 0xFF;
                sys.mem.ram[0x100 | sp.wrapping_sub(1) as usize] = 0xFD;
                sys.cpu.regs.sp = sp.wrapping_sub(2);
                let mut reads = 0;
                for _ in 0..common::MAX_INSNS {
                    let pc = sys.cpu.regs.pc;
                    if pc == 0xFFFE {
                        break;
                    }
                    if matches!(sys.mem.read_byte(pc), 0xAD | 0xAC | 0xAE)
                        && sys.mem.read_byte(pc.wrapping_add(1)) == 0x0D
                        && sys.mem.read_byte(pc.wrapping_add(2)) == 0xDC
                    {
                        sys.mem.io[0xC0D] = u8::from(reads == 0);
                        reads += 1;
                    }
                    assert!(sys.step());
                }
                assert_eq!(sys.cpu.regs.pc, 0xFFFE);
                assert_eq!(reads, 2);
                assert_eq!(sys.error_code, None);
                assert_eq!(
                    common::petscii_to_string(&sys.output),
                    expected,
                    "{profile:?}/{banked}: {expr}"
                );
            }
        }
    }
}

#[test]
fn byte_overflow_does_not_preempt_a_later_arithmetic_error() {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        for banked in [false, true] {
            let source = "10 POKE 2,255:POKE 3,1:POKE 4,0\n20 A=PEEK(2)*257:B=PEEK(3) AND 1\n30 POKE 1024,A+B+B+1/PEEK(4)\n40 PRINT PEEK(1024)\n";
            let compiled = build(source, profile, banked);
            let result = emu64::run_prg_to_end(&compiled.prg_bytes, common::MAX_INSNS).unwrap();
            assert_eq!(
                result.error_banner(),
                Some("?DIVISION BY ZERO  ERROR IN 30".into())
            );
        }
    }
}

#[test]
fn bounded_products_keep_sign_zero_and_cancellation() {
    for (expr, expected) in [
        ("A*A", None), ("A*A+1", None),
        ("(-A)*(-A)", None), ("(-A)*A", None),
        ("A*A-A*A+65", Some(65)),
        ("A*A*B", Some(0)),
        ("C*C*4", None),
        ("A*A*C", None),
        ("A*A+C*C*4-(A*A)-C*C*4+65", Some(65)),
    ] {
        let source = format!("10 POKE 2,255:POKE 3,0:POKE 4,0\n20 A=PEEK(2)+1:B=PEEK(3) AND 1:C=PEEK(4)-128\n30 POKE 1024,{expr}\n40 PRINT PEEK(1024)\n");
        check(&source, expected, expr != "(-A)*A");
    }
}

#[test]
fn exact_width_expands_before_its_boundary_then_uses_real_fac() {
    for (expr, marker, expected) in [
        ("A*A", "__I24BYTE_OK_", None),
        ("A*A*A", "__I32BYTE_OK_", None),
        ("A*A*A-A*A*A+65", "__I32BYTE_OK_", Some(65)),
        ("A*A*A*A", "__FAC_BYTE", None),
        ("A*A*A*A-A*A*A*A+65", "__FAC_BYTE", Some(65)),
        ("A*A*A*A+1-A*A*A*A+65", "__FAC_BYTE", Some(67)),
    ] {
        let source = format!("10 POKE 2,255\n20 A=PEEK(2)+1\n30 POKE 1024,{expr}\n40 PRINT PEEK(1024)\n");
        check(&source, expected, false);
        for profile in [Profile::Default, Profile::Speed, Profile::Size] {
            let compiled = build(&source, profile, false);
            assert!(compiled.asm.contains(marker), "{profile:?}: {expr}");
            if marker == "__FAC_BYTE" {
                assert!(!compiled.asm.contains("__I24BYTE_OK_") && !compiled.asm.contains("__I32BYTE_OK_"));
            }
        }
    }
}

#[test]
fn signed32_endpoints_and_fac_overflow_remain_observable() {
    for (expr, marker) in [("A*A*A*C", "__I32BYTE_OK_"), ("A*A*A*(C+1)", "__FAC_BYTE")] {
        let source = format!("10 POKE 2,255:POKE 3,0\n20 A=PEEK(2)+1:C=PEEK(3)-128\n30 POKE 1024,{expr}\n40 PRINT PEEK(1024)\n");
        check(&source, None, false);
        for profile in [Profile::Default, Profile::Speed, Profile::Size] {
            assert!(build(&source, profile, false).asm.contains(marker));
        }
    }
    // Even a final zero factor does not erase the earlier BASIC overflow.
    let product = std::iter::repeat_n("A", 16).collect::<Vec<_>>().join("*");
    let source = format!("10 POKE 2,255:POKE 3,0\n20 A=PEEK(2)+1:B=PEEK(3) AND 1\n30 POKE 1024,{product}*B\n40 PRINT PEEK(1024)\n");
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        for banked in [false,true] {
            let result = emu64::run_prg_to_end(&build(&source, profile, banked).prg_bytes, common::MAX_INSNS).unwrap();
            assert_eq!(result.error_banner(), Some("?OVERFLOW  ERROR IN 30".into()));
        }
    }
}

#[test]
fn constant_masked_signed_leaves_keep_the_outer_carry() {
    for (expr, expected) in [
        ("PEEK(2)*257+(PEEK(3) AND 1)+(PEEK(3) AND 1)", None),
        ("A+(PEEK(3) AND 1)+(1 AND PEEK(3))-A", Some(2)),
        ("A+(C AND 255)+(255 AND C)-A-3", Some(255)),
        ("A+(C AND 32767)-(C AND 32767)-A+65", Some(65)),
    ] {
        let source = format!("10 POKE 2,255:POKE 3,1:POKE 4,1\n20 A=PEEK(2)*257:C=PEEK(4)-128\n30 POKE 1024,{expr}\n40 PRINT PEEK(1024)\n");
        check(&source, expected, true);
    }

    check("10 POKE 2,255:POKE 3,1\n20 A=PEEK(2)+1\n30 POKE 1024,A*A*A*A+(PEEK(3) AND 1)-A*A*A*A+65\n40 PRINT PEEK(1024)\n", Some(67), false);
}

#[test]
fn unsafe_mask_operands_keep_their_ayint_error() {
    for expr in ["(A AND 1)+1/PEEK(4)", "(PEEK(3) AND 32768)+1/PEEK(4)"] {
        let source = format!("10 POKE 2,255:POKE 3,1:POKE 4,0\n20 A=PEEK(2)*257\n30 POKE 1024,{expr}\n40 PRINT PEEK(1024)\n");
        check(&source, None, false);
    }
}
