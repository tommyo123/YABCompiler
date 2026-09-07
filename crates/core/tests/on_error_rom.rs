mod common;
use emu64::System;
use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn run(source: &str, profile: Profile, extra: bool, previous: u16) -> (String, bool, u16) {
    run_layout(source, profile, extra, previous, false)
}

fn run_layout(
    source: &str,
    profile: Profile,
    extra: bool,
    previous: u16,
    shadow: bool,
) -> (String, bool, u16) {
    let compiled = compile_with_options(
        &tokenize_program(source).unwrap(),
        CompileOptions {
            profile,
            extraram: extra,
            reserved_ranges: if shadow {
                vec![(0x0A00, 0x9FFF)]
            } else {
                vec![]
            },
            ..CompileOptions::default()
        },
    )
    .unwrap();
    if shadow {
        let mut assembler = asm6502::Assembler6502::new();
        assembler.add_reserved_range(0x0A00, 0x9FFF).unwrap();
        assembler.assemble_bytes(&compiled.asm).unwrap();
        assert!(assembler.symbols()["__EXRAM_IERROR_ENTRY"] < 0x0A00);
        assert!(
            (0xA000..0xC000).contains(&assembler.symbols()["__TSB_ERROR"]),
            "dispatcher at {:04X}",
            assembler.symbols()["__TSB_ERROR"]
        );
    }
    let mut m = System::new();

    if previous != 0xE38B {
        m.mem.ram[previous as usize..previous as usize + 3].copy_from_slice(&[0x4C, 0x8B, 0xE3]);
    }
    m.mem.ram[0x300] = previous as u8;
    m.mem.ram[0x301] = (previous >> 8) as u8;
    let entry = m.load_prg(&compiled.prg_bytes).unwrap();
    m.start_at(entry);
    m.mem.ram[0x1FF] = 0xFF;
    m.mem.ram[0x1FE] = 0xFD;
    m.cpu.regs.sp = 0xFD;
    let mut heap_markers = Vec::new();
    for _ in 0..1_000_000 {
        let marker = m.mem.ram[0xC002];
        if shadow
            && matches!(marker, 1 | 2)
            && heap_markers.last().is_none_or(|(seen, _)| *seen != marker)
        {
            heap_markers.push((
                marker,
                u16::from(m.mem.ram[0x37]) | u16::from(m.mem.ram[0x38]) << 8,
            ));
        }
        if m.cpu.regs.pc == 0xFFFE || !m.step() {
            break;
        }
    }
    let terminal = m.cpu.regs.pc == 0xFFFE || m.ready_reached;
    let vector = u16::from(m.mem.ram[0x300]) | u16::from(m.mem.ram[0x301]) << 8;
    if shadow && source.contains("POKE49154,1") {
        assert_eq!(heap_markers.len(), 2, "both semantic markers must execute");
        assert_eq!(
            heap_markers[0].1, heap_markers[1].1,
            "SYS error left the interpreter's MEMSIZ installed"
        );
    }

    let mut bytes = m.output;
    bytes.extend(m.error_output);
    (
        common::petscii_to_string(&bytes)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "),
        terminal,
        vector,
    )
}

fn all(source: &str, expected: &str) {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        for extra in [false, true] {
            for previous in [0xE38B, 0x0380] {
                let (text, terminal, vector) = run(source, profile, extra, previous);
                assert!(terminal, "{profile:?}/{extra}: {text}");
                assert_eq!(text, expected, "{profile:?}/{extra}");
                assert_eq!(vector, previous, "IERROR chain leaked: {profile:?}/{extra}");
            }
        }
    }
}

#[test]
fn catches_rom_arithmetic_and_restores_nested_ordinary_calls() {
    for (expr, code) in [("1/A", 20), ("1E38*10+A", 15), ("SQR(A-1)", 14)] {
        let source = format!(
            "10 ON ERROR GOTO900\n20 GOSUB100:GOSUB100\n30 PRINT\"DONE\":END\n100 GOSUB200\n110 PRINT\"OUTER\":RETURN\n200 A=PEEK(49152):PRINT{expr}\n210 PRINT\"INNER\":RETURN\n900 PRINTER;EL:GOSUB950:GOSUB950\n910 PRINT\"BAD\":END\n950 RESUME NEXT\n"
        );
        all(
            &source,
            &format!("{code} 200 INNER OUTER {code} 200 INNER OUTER DONE"),
        );
    }
}

#[test]
fn val_overflow_restores_the_terminator_and_text_pointer_before_resume() {
    all(
        "10 ON ERROR GOTO900\n20 A$=\"1e39\":PRINT VAL(A$)\n30 PRINT\"DONE\":END\n900 PRINT ER;EL:RESUME NEXT\n",
        "15 20 DONE",
    );
    all(
        "10 ON ERROR GOTO900\n20 GOSUB100:GOSUB100\n30 PRINT\"DONE\":END\n100 A$=\"1e39\":PRINT VAL(A$)\n110 PRINT VAL(\"2.5e1\"):RETURN\n900 PRINT ER;EL:RESUME NEXT\n",
        "15 100 25 15 100 25 DONE",
    );
}

#[test]
fn catches_rom_errors_in_keyboard_handler_and_preserves_for_state() {
    all(
        "10 ON ERROR GOTO900:ON KEY \"Q\" GOSUB100\n20 FORI%=1TO2\n30 POKE631,209:POKE198,1\n40 PRINTI%:NEXT:END\n100 PRINT1/PEEK(49152)\n110 RETURN\n900 FORJ=1TO3:PRINT ER;EL\n910 RESUME NEXT\n920 NEXT\n",
        "20 100 1 20 100 2",
    );
}

#[test]
fn same_and_explicit_resume_and_reinstallation_do_not_reenter_the_hook() {
    all(
        "10 ON ERROR GOTO900\n20 ON ERROR GOTO900\n30 READ A:PRINT1/A\n40 PRINT\"DONE\":END\n50 DATA0,2\n900 PRINT ER;EL:RESUME\n",
        "20 30 .5 DONE",
    );
    all(
        "10 ON ERROR GOTO900\n20 PRINT1/PEEK(49152)\n30 PRINT\"BAD\":END\n40 PRINT\"TARGET\":END\n900 NO ERROR:RESUME40\n",
        "TARGET",
    );
}

#[test]
fn reset_and_terminal_paths_release_or_keep_the_owned_vector_as_required() {
    all(
        "10 ON ERROR GOTO900\n20 IFPEEK(49152)=3THENPRINT\"DONE\":END\n30 PRINT1/PEEK(49153)\n40 PRINT\"BAD\":END\n900 POKE49152,PEEK(49152)+1:CLR:GOTO20\n",
        "DONE",
    );
    all(
        "5 IFPEEK(49152)THENPRINT\"RUN\":END\n10 ON ERROR GOTO900\n20 PRINT1/PEEK(49152)\n900 POKE49152,1:RUN\n",
        "RUN",
    );
    all(
        "10 ON ERROR GOTO900\n20 PRINT1/PEEK(49152)\n900 PRINT\"END\":END\n",
        "END",
    );
    all(
        "10 ON ERROR GOTO900\n20 PRINT1/PEEK(49152)\n900 PRINT\"FALLTHROUGH\"\n",
        "FALLTHROUGH",
    );
    all(
        "10 ON ERROR GOTO900\n20 STOP\n900 PRINT\"BAD\":END\n",
        "BREAK IN 20",
    );
}

#[test]
fn disabled_nested_and_invalid_resume_errors_are_fatal_once() {
    for (source, error) in [
        (
            "10 ON ERROR GOTO900\n20 NO ERROR\n30 PRINT1/PEEK(49152)\n900 PRINT\"BAD\":END\n",
            "DIVISION BY ZERO ERROR IN 30",
        ),
        (
            "10 ON ERROR GOTO900\n20 PRINT1/PEEK(49152)\n900 PRINTSQR(-1)\n910 PRINT\"BAD\":END\n",
            "ILLEGAL QUANTITY ERROR IN 900",
        ),
        (
            "10 ON ERROR GOTO50\n20 GOTO100\n50 RESUME NEXT\n100 PRINT1/PEEK(49152)\n",
            "UNDEF'D STATEMENT ERROR IN 50",
        ),
        (
            "10 ON ERROR GOTO900\n20 PRINT1/PEEK(49152)\n900 RESUME999\n",
            "UNDEF'D STATEMENT ERROR IN 900",
        ),
    ] {
        for p in [Profile::Default, Profile::Speed, Profile::Size] {
            for extra in [false, true] {
                let (text, terminal, vector) = run(source, p, extra, 0x0380);
                assert!(terminal, "{p:?}/{extra}: {text}");
                assert!(text.contains(error), "{p:?}/{extra}: {text}");
                assert_eq!(text.matches("ERROR").count(), 1, "{text}");
                assert_eq!(vector, 0x0380);
            }
        }
    }
}

#[test]
fn rom_error_from_sys_restores_compiled_banking_and_previous_error_chain() {

    all(
        "10 POKE832,162:POKE833,22:POKE834,76:POKE835,55:POKE836,164\n20 ON ERROR GOTO900\n30 SYS832\n40 PRINT\"BACK\":END\n900 PRINT ER;EL:RESUME NEXT\n",
        "22 30 BACK",
    );
}

#[test]
fn real_rom_can_enter_a_dispatcher_hidden_under_basic_rom() {
    let source = "10 ON ERROR GOTO900\n20 GOSUB100:GOSUB100\n30 PRINT\"DONE\":END\n100 GOSUB200\n110 PRINT\"OUTER\":RETURN\n200 A=PEEK(49152):PRINT1/A\n210 PRINT\"INNER\":RETURN\n900 PRINT ER;EL:GOSUB950:GOSUB950\n910 PRINT\"BAD\":END\n950 RESUME NEXT\n";
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let (text, terminal, vector) = run_layout(source, profile, true, 0xE38B, true);
        assert!(terminal, "{profile:?}: {text}");
        assert_eq!(text, "20 200 INNER OUTER 20 200 INNER OUTER DONE");
        assert_eq!(vector, 0xE38B);
    }
}

#[test]
fn sys_error_reestablishes_the_existing_compiled_heap_ceiling() {
    let source = "10 POKE832,162:POKE833,22:POKE834,76:POKE835,55:POKE836,164\n20 ON ERROR GOTO900:POKE49154,1\n30 SYS832\n40 PRINT\"DONE\":END\n900 POKE49154,2:A$=STR$(PEEK(49152))+CHR$(65):PRINT A$;ER;EL:RESUME NEXT\n";
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let (text, terminal, vector) = run_layout(source, profile, true, 0xE38B, true);
        assert!(terminal, "{profile:?}: {text}");
        assert_eq!(text, "0A 22 30 DONE");
        assert_eq!(vector, 0xE38B);
    }
}
