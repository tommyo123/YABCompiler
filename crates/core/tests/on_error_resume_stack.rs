mod common;

use emu64::System;
use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn run(source: &str, profile: Profile, extraram: bool) -> (String, Option<u8>, bool) {
    let output = compile_with_options(
        &tokenize_program(source).unwrap(),
        CompileOptions {
            profile,
            extraram,
            ..CompileOptions::default()
        },
    )
    .unwrap();
    let mut system = System::new();
    let entry = system.load_prg(&output.prg_bytes).unwrap();
    system.start_at(entry);
    system.mem.ram[0x1ff] = 0xff;
    system.mem.ram[0x1fe] = 0xfd;
    system.cpu.regs.sp = 0xfd;
    for _ in 0..1_000_000 {
        if system.cpu.regs.pc == 0xfffe || !system.step() {
            break;
        }
    }
    let mut bytes = system.output;
    bytes.extend(system.error_output);
    (
        common::petscii_to_string(&bytes),
        system.error_code,
        system.cpu.regs.pc == 0xfffe || system.ready_reached,
    )
}

fn profiles(source: &str, expected: &str) {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        for extraram in [false, true] {
            let (output, error, terminal) = run(source, profile, extraram);
            assert!(
                terminal,
                "{profile:?}, extra={extraram}: no termination: {:?}",
                &output[..output.len().min(300)]
            );
            assert_eq!(error, None, "{profile:?}, extra={extraram}: {output:?}");
            assert_eq!(output.trim(), expected, "{profile:?}, extra={extraram}");
        }
    }
}

#[test]
fn helper_error_resume_next_preserves_two_ordinary_calls() {
    profiles(
        "10 ON ERROR GOTO900\n20 GOSUB100:GOSUB100\n30 PRINT\"DONE\":END\n100 READ A:PRINTCHR$(A)\n110 PRINT\"RETURN\":RETURN\n120 DATA256,256\n900 PRINT\"ERROR\";EL:RESUME NEXT\n",
        "ERROR 100 \nRETURN\nERROR 100 \nRETURN\nDONE",
    );
}

#[test]
fn helper_error_resume_next_preserves_on_key_call() {
    profiles(
        "10 ON ERROR GOTO900\n20 ON KEY \"Q\" GOSUB100\n30 POKE631,209:POKE198,1\n40 PRINT\"MAIN\":END\n100 READ A:PRINTCHR$(A)\n110 RETURN\n120 DATA256\n900 PRINT\"ERROR\";EL:RESUME NEXT\n",
        "ERROR 100 \nMAIN",
    );
}

#[test]
fn resume_from_nested_handler_discards_only_handler_calls() {
    profiles(
        "10 ON ERROR GOTO900\n20 GOSUB100:GOSUB100\n30 PRINT\"DONE\":END\n100 GOSUB200\n110 PRINT\"OUTER\":RETURN\n200 READ A:PRINTCHR$(A)\n210 PRINT\"INNER\":RETURN\n220 DATA256,256\n900 GOSUB950:GOSUB950\n910 PRINT\"BAD\":END\n950 PRINT\"HANDLER\"\n960 RESUME NEXT\n",
        "HANDLER\nINNER\nOUTER\nHANDLER\nINNER\nOUTER\nDONE",
    );
}

#[test]
fn resume_discards_handler_for_and_preserves_caller_loop() {
    profiles(
        "10 ON ERROR GOTO900\n20 FORI=1TO2\n30 GOSUB100:GOSUB100\n40 PRINTI:NEXT:END\n100 READ A:PRINTCHR$(A)\n110 RETURN\n120 DATA256,256,256,256\n900 FORJ=1TO3\n910 RESUME NEXT\n920 NEXTJ\n",
        "1 \n 2",
    );
}

#[test]
fn same_and_explicit_line_resume_keep_faulting_call() {
    profiles(
        "10 ON ERROR GOTO900\n20 GOSUB100:GOSUB100\n30 PRINT\"DONE\":END\n100 READ A:PRINTCHR$(A)\n110 RETURN\n120 DATA256,65,256,66\n900 RESUME\n",
        "A\nB\nDONE",
    );
    profiles(
        "10 ON ERROR GOTO900\n20 GOSUB100:GOSUB100\n30 PRINT\"DONE\":END\n100 ERROR14\n110 PRINT\"BAD\":END\n120 PRINT\"TARGET\":RETURN\n900 RESUME120\n",
        "TARGET\nTARGET\nDONE",
    );
}

#[test]
fn same_variable_handler_for_restores_the_original_frame() {
    for var in ["I", "I%"] {
        profiles(
            &format!(
                "10 ON ERROR GOTO900\n20 FOR{var}=1TO2\n30 A=PEEK(-1)\n40 PRINT{var}:NEXT:END\n900 FOR{var}=5TO9\n910 RESUME NEXT\n920 NEXT\n"
            ),
            "5",
        );
    }
}

#[test]
fn handler_end_run_and_repeated_clr_abandon_calls() {
    profiles(
        "10 ON ERROR GOTO900\n20 GOSUB100:GOSUB100\n30 PRINT\"BAD\":END\n100 A=PEEK(-1)\n110 RETURN\n900 PRINT\"END\":END\n",
        "END",
    );
    profiles(
        "5 IFPEEK(49152)THENPRINT\"RUN\":END\n10 ON ERROR GOTO900\n20 GOSUB100:GOSUB100\n30 PRINT\"BAD\":END\n100 A=PEEK(-1)\n110 RETURN\n900 POKE49152,1:RUN\n",
        "RUN",
    );
    profiles(
        "10 ON ERROR GOTO900\n20 IFPEEK(49152)=150THENPRINT\"CLR\":END\n30 GOSUB100:GOSUB100\n40 PRINT\"BAD\":END\n100 A=PEEK(-1)\n110 RETURN\n900 POKE49152,PEEK(49152)+1:CLR:GOTO20\n",
        "CLR",
    );
}

#[test]
fn clr_invalidates_resume_context() {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let (output, error, terminal) = run(
            "10 ON ERROR GOTO900\n20 ERROR14\n30 PRINT\"BAD\":END\n900 CLR:RESUME NEXT\n",
            profile,
            false,
        );
        assert!(terminal);
        assert_eq!(error, Some(14));
        assert!(!output.contains("BAD"));
    }
}

#[test]
fn resume_keeps_captured_for_limit_and_step() {
    profiles(
        "10 ON ERROR GOTO900\n20 B=3:S=1:FORI=1TOB STEPS\n30 IFPEEK(49152)=0THENA=PEEK(-1)\n40 PRINTI:NEXT:END\n900 B=1:S=4:I=2:POKE49152,1:RESUME NEXT\n",
        "2 \n 3",
    );
}

#[test]
fn disabled_nested_and_past_eof_errors_terminate() {
    for (source, expected) in [
        (
            "10 ON ERROR GOTO900\n20 NO ERROR\n30 A=PEEK(-1)\n900 PRINT\"BAD\":END\n",
            14,
        ),
        (
            "10 ON ERROR GOTO900\n20 ERROR14\n900 A=PEEK(-1)\n910 PRINT\"BAD\":END\n",
            14,
        ),
        (
            "10 ON ERROR GOTO50\n20 GOTO100\n50 RESUME NEXT\n100 ERROR14\n",
            17,
        ),
        ("10 RESUME30\n20 END\n30 PRINT\"BAD\":END\n", 14),
    ] {
        for profile in [Profile::Default, Profile::Speed, Profile::Size] {
            for extraram in [false, true] {
                let (output, error, terminal) = run(source, profile, extraram);
                assert!(terminal, "{profile:?}, extra={extraram}: {output:?}");
                assert_eq!(
                    error,
                    Some(expected),
                    "{profile:?}, extra={extraram}: {output:?}"
                );
                assert!(!output.contains("BAD"));
            }
        }
    }
}

#[test]
fn missing_run_target_does_not_resurrect_cleared_calls() {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        for extraram in [false, true] {
            let (output, error, terminal) = run(
                "10 ON ERROR GOTO900\n20 GOSUB100:GOSUB100\n30 PRINT\"BADRETURN\":END\n100 RUN999\n110 RETURN\n900 PRINT\"HANDLER\";ER;EL:NO ERROR:RESUME110\n",
                profile,
                extraram,
            );
            assert!(terminal, "{profile:?}, extra={extraram}: {output:?}");
            assert_eq!(error, Some(12), "{profile:?}, extra={extraram}: {output:?}");
            assert!(output.starts_with("HANDLER 17  100"), "{output:?}");
            assert!(!output.contains("BADRETURN"));
        }
    }
}

#[test]
fn fac_byte_errors_reach_handler_without_changing_rom_truncation() {

    for (value, expected) in [
        ("-.5", "ERROR 30 \n 0"),
        ("-1E-20", "ERROR 30 \n 0"),
        ("-0", "0"),
        ("0", "0"),
        (".5", "0"),
        ("255", "255"),
        ("255.5", "255"),
        ("255.999", "255"),
        ("256", "ERROR 30 \n 0"),
        ("65535.5", "ERROR 30 \n 0"),
        ("65536", "ERROR 30 \n 0"),
    ] {
        profiles(
            &format!(
                "10 ON ERROR GOTO900\n20 READ A\n30 POKE49152,A\n40 PRINTPEEK(49152):END\n50 DATA{value}\n900 PRINT\"ERROR\";EL:RESUME NEXT\n"
            ),
            expected,
        );
    }
}
