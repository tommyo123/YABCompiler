mod common;
use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn run(source: &str, profile: Profile, extra: bool) -> String {
    let c = compile_with_options(
        &tokenize_program(source).unwrap(),
        CompileOptions {
            profile,
            extraram: extra,
            ..CompileOptions::default()
        },
    )
    .unwrap();
    let mut s = emu64::System::new();
    s.mem.ram[0x54] = 0x4c; // cold-start function JMP used by ROM ASC/VAL
    s.mem.ram[0x380..0x386].copy_from_slice(&[0x4c, 0x86, 0xae, 0x4c, 0x8b, 0xe3]);
    s.mem.ram[0x30a] = 0x80;
    s.mem.ram[0x30b] = 3;
    s.mem.ram[0x300] = 0x83;
    s.mem.ram[0x301] = 3;
    let entry = s.load_prg(&c.prg_bytes).unwrap();
    s.start_at(entry);
    s.mem.ram[0x1ff] = 255;
    s.mem.ram[0x1fe] = 253;
    s.cpu.regs.sp = 253;
    for _ in 0..3_000_000 {
        if s.cpu.regs.pc == 0xfffe || !s.step() {
            break;
        }
    }
    assert!(
        s.cpu.regs.pc == 0xfffe || s.ready_reached,
        "did not terminate {profile:?}/{extra}"
    );
    assert_eq!(
        &s.mem.ram[0x30a..0x30c],
        &[0x80, 3],
        "IEVAL leaked {profile:?}/{extra}"
    );
    assert_eq!(
        &s.mem.ram[0x300..0x302],
        &[0x83, 3],
        "IERROR leaked {profile:?}/{extra}"
    );
    let mut bytes = s.output;
    bytes.extend(s.error_output);
    common::petscii_to_string(&bytes)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn all(source: &str, expected: &str) {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        for extra in [false, true] {
            assert_eq!(run(source, profile, extra), expected, "{profile:?}/{extra}");
        }
    }
}

#[test]
fn callbacks_and_rom_errors_restore_nested_gosub_continuations() {
    for (setup, param, error) in [
        ("DIM A$(1):A$(0)=\"KEEP\":K=2", "VAL(A$(K))", 18),
        (
            "DIM A$(1):A$(0)=\"1\"+CHR$(69)+\"99\":K=0",
            "VAL(A$(K))",
            15,
        ),
        ("DIM A$(1):A$(0)=\"\":K=0", "ASC(A$(K))", 14),
    ] {
        let src = format!(
            "10 FORI=0TO12:READB:POKE49152+I,B:NEXT\n20 ON ERROR GOTO900\n30 {setup}:B$=\"SAFE\"\n40 GOSUB100:GOSUB100\n50 PRINTB$:END\n100 GOSUB200\n110 PRINT\"BACK\":RETURN\n200 SYS49152,{param}\n210 PRINT\"RESUMED\":RETURN\n900 PRINTER;EL:RESUME NEXT\n1000 DATA32,253,174,32,158,173,32,221,189,32,30,171,96\n"
        );
        all(
            &src,
            &format!("{error} 200 RESUMED BACK {error} 200 RESUMED BACK SAFE"),
        );
    }
}

#[test]
fn no_error_disable_and_clear_do_not_save_our_own_vectors() {
    let src = "10 FORI=0TO5:READB:POKE49152+I,B:NEXT\n20 A$=\"FIRST\":SYS49152,A$\n30 CLR\n40 A$=\"SECOND\":SYS49152,A$\n50 ON ERROR GOTO900\n60 A$=\"\":SYS49152,ASC(A$)\n70 NO ERROR:A$=\"THIRD\":SYS49152,A$\n80 END\n900 PRINTER;EL:RESUME NEXT\n1000 DATA32,253,174,76,160,170\n";
    all(src, "FIRST SECOND 14 60 THIRD");
}

#[test]
fn ignored_argument_keeps_the_end_of_sys_syntax_error() {
    let src = "10 POKE49152,96\n20 ON ERROR GOTO900\n30 A$=\"1\"+CHR$(69)+\"99\"\n40 SYS49152,VAL(A$)\n50 PRINTA$:END\n900 PRINTER;EL:RESUME NEXT\n";
    all(src, "11 40 1E99");
}

#[test]
fn direct_callback_errors_reapply_the_compiled_heap_ceiling() {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        for (value, index, error) in [
            ("CHR$(PEEK(0))", 2, 18),
            ("\"1\"+CHR$(PEEK(2))+\"99\"", 0, 15),
        ] {
            let source = format!(
                "10 ON ERROR GOTO900\n20 FORI=0TO12:READB:POKE49152+I,B:NEXT\n30 POKE2,69:DIM A$(1):A$(0)={value}:K={index}\n40 PRINT\"BEFORE\"\n50 SYS49152,VAL(A$(K))\n60 PRINT\"AFTER\"\n70 END\n100 DATA32,253,174,32,158,173,32,221,189,32,30,171,96\n900 PRINTER;EL:RESUME NEXT\n"
            );
            let c = compile_with_options(
                &tokenize_program(&source).unwrap(),
                CompileOptions {
                    profile,
                    extraram: true,
                    reserved_ranges: vec![(0x1000, 0x9fff)],
                    ..CompileOptions::default()
                },
            )
            .unwrap();
            let mut a = asm6502::Assembler6502::new();
            for &(lo, hi) in &c.diagnostics.effective_reserved {
                a.add_reserved_range(lo, hi).unwrap();
            }
            a.assemble_bytes(&c.asm).unwrap();
            let symbols = a.symbols();
            assert!(
                (0xa000..0xc000).contains(&symbols["__SYS_EVAL_DISPATCH"]),
                "{profile:?}/{error}: dispatcher {:04X}",
                symbols["__SYS_EVAL_DISPATCH"]
            );
            let phases = [symbols["L40"], symbols["L900"], symbols["L60"]];
            let mut m = emu64::System::new();
            m.mem.ram[0x54] = 0x4c;
            let entry = m.load_prg(&c.prg_bytes).unwrap();
            m.start_at(entry);
            m.mem.ram[0x1ff] = 255;
            m.mem.ram[0x1fe] = 253;
            m.cpu.regs.sp = 253;
            let mut observed = vec![];
            for _ in 0..3_000_000 {
                if observed.len() < phases.len() && m.cpu.regs.pc == phases[observed.len()] {
                    observed.push(u16::from_le_bytes([m.mem.ram[0x37], m.mem.ram[0x38]]));
                }
                if m.cpu.regs.pc == 0xfffe || !m.step() {
                    break;
                }
            }
            assert_eq!(
                observed,
                vec![0xc000; 3],
                "{profile:?}/ER{error}: MEMSIZ after SYS abort"
            );
            let mut output = m.output.clone();
            output.extend(&m.error_output);
            assert_eq!(
                common::petscii_to_string(&output)
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
                format!("BEFORE {error} 50 AFTER")
            );
            assert!(m.cpu.regs.pc == 0xfffe || m.ready_reached);
            assert_eq!(&m.mem.ram[0x30a..0x30c], &[0x86, 0xae]);
            assert_eq!(&m.mem.ram[0x300..0x302], &[0x8b, 0xe3]);
        }
    }
}

#[test]
fn disabled_handler_keeps_the_fatal_sys_heap_policy() {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let source = "10 ON ERROR GOTO900:NO ERROR\n20 FORI=0TO12:READB:POKE49152+I,B:NEXT\n30 DIM A$(1):A$(0)=CHR$(PEEK(0)):K=2\n40 PRINT\"BEFORE\"\n50 SYS49152,VAL(A$(K))\n60 PRINT\"BAD\":END\n100 DATA32,253,174,32,158,173,32,221,189,32,30,171,96\n900 PRINT\"BAD\":END\n";
        let c = compile_with_options(
            &tokenize_program(source).unwrap(),
            CompileOptions {
                profile,
                extraram: true,
                reserved_ranges: vec![(0x1000, 0x9fff)],
                ..CompileOptions::default()
            },
        )
        .unwrap();
        let mut a = asm6502::Assembler6502::new();
        for &(lo, hi) in &c.diagnostics.effective_reserved {
            a.add_reserved_range(lo, hi).unwrap();
        }
        a.assemble_bytes(&c.asm).unwrap();
        let symbols = a.symbols();
        let mut m = emu64::System::new();
        m.mem.ram[0x54] = 0x4c;
        let entry = m.load_prg(&c.prg_bytes).unwrap();
        m.start_at(entry);
        m.mem.ram[0x1ff] = 255;
        m.mem.ram[0x1fe] = 253;
        m.cpu.regs.sp = 253;
        let mut observed = vec![];
        let phases = [symbols["L40"], symbols["__TSB_ERROR_DEFAULT"]];
        for _ in 0..3_000_000 {
            if observed.len() < phases.len() && m.cpu.regs.pc == phases[observed.len()] {
                observed.push(u16::from_le_bytes([m.mem.ram[0x37], m.mem.ram[0x38]]));
            }
            if m.cpu.regs.pc == 0xfffe || !m.step() {
                break;
            }
        }
        assert_eq!(observed, vec![0xc000, 0xa000], "{profile:?}");
        assert_eq!((m.error_code, m.error_line), (Some(18), Some(50)));
        assert_eq!(
            u16::from_le_bytes([m.mem.ram[0x37], m.mem.ram[0x38]]),
            0xa000
        );
        assert_eq!(&m.mem.ram[0x30a..0x30c], &[0x86, 0xae]);
        assert_eq!(&m.mem.ram[0x300..0x302], &[0x8b, 0xe3]);
    }
}
