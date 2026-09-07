mod common;

use emu64::System;
use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const MARKER: usize = 0xc000;
const SENTINEL: u16 = 0xfffe;

struct Observation {
    output: String,
    error: Option<u8>,
    error_line: Option<u16>,
    terminal: bool,
    injected: usize,
    drained: usize,
    marker_sp: Vec<u8>,
    ram: [u8; 8],
    asm: String,
}

fn run(source: &str, profile: Profile, keys: &[u8], every_marker: bool) -> Observation {
    let compiled = compile_with_options(
        &tokenize_program(source).unwrap(),
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .unwrap();
    let mut system = System::new();
    let entry = system.load_prg(&compiled.prg_bytes).unwrap();
    system.start_at(entry);

    system.mem.ram[0x1ff] = 0xff;
    system.mem.ram[0x1fe] = 0xfd;
    system.cpu.regs.sp = 0xfd;
    let mut last_marker = 0;
    let mut injected = 0;
    let mut drained = 0;
    let mut pending = false;
    let mut marker_sp = Vec::new();
    for _ in 0..5_000_000 {
        if system.cpu.regs.pc == SENTINEL {
            break;
        }
        let running = system.step();
        if pending && system.mem.ram[0xc6] == 0 {
            drained += 1;
            pending = false;
        }
        let marker = system.mem.ram[MARKER];
        if marker != last_marker {
            last_marker = marker;
            marker_sp.push(system.cpu.regs.sp);

            if system.cpu.regs.sp < 24 {
                break;
            }
            if injected < keys.len() && (every_marker || keys[injected] == marker) {
                assert!(!pending, "previous key was not drained");
                assert_eq!(system.mem.ram[0xc6], 0, "keyboard buffer not empty");
                // Uppercase source "A" is shifted PETSCII $C1.
                system.mem.ram[0x277] = 0xc1;
                system.mem.ram[0xc6] = 1;
                pending = true;
                injected += 1;
            }
        }
        if !running {
            break;
        }
    }
    Observation {
        output: common::petscii_to_string(&system.output),
        error: system.error_code,
        error_line: system.error_line,
        terminal: system.cpu.regs.pc == SENTINEL || system.ready_reached,
        injected,
        drained,
        marker_sp,
        ram: system.mem.ram[MARKER..MARKER + 8].try_into().unwrap(),
        asm: compiled.asm,
    }
}

fn profiles(mut test: impl FnMut(Profile)) {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        test(profile);
    }
}

fn clean(result: &Observation, expected: &str, keys: usize) {
    assert!(result.terminal, "no terminal state: {:?}", result.output);
    assert_eq!(
        result.error, None,
        "{:?}, line {:?}: {:?}",
        result.error, result.error_line, result.output
    );
    assert_eq!(result.output.trim(), expected);
    assert_eq!(result.injected, keys);
    assert_eq!(
        result.drained, keys,
        "zero/undrained presses cannot verify ON KEY"
    );
}

#[test]
fn return_resumes_interrupted_statement_and_preserves_nested_calls() {
    let programs = [
        (
            "10 ON KEY \"A\" GOSUB1000\n20 POKE49152,1:PRINT\"MAIN\":END\n1000 PRINT\"HANDLER\":RETURN\n",
            "HANDLER\nMAIN",
        ),
        (
            "10 ON KEY \"A\" GOSUB1000\n20 POKE49152,1\n30 PRINT\"MAIN\":END\n1000 GOTO1100\n1100 PRINT\"HANDLER\":RETURN\n",
            "HANDLER\nMAIN",
        ),
        (
            "10 ON KEY \"A\" GOSUB1000\n20 GOSUB500\n30 PRINT\"MAIN\":END\n500 POKE49152,1\n510 PRINT\"CALLER\"\n520 RETURN\n1000 PRINT\"HANDLER\":GOSUB1100:RETURN\n1100 PRINT\"NESTED\"\n1110 RETURN\n",
            "HANDLER\nNESTED\nCALLER\nMAIN",
        ),
    ];
    profiles(|p| {
        for (source, expected) in programs {
            clean(&run(source, p, &[1], false), expected, 1);
        }
    });
}

#[test]
fn end_in_handler_terminates_the_whole_program() {
    profiles(|p| {
        clean(
            &run(
                "10 ON KEY \"A\" GOSUB1000\n20 POKE49152,1\n30 PRINT\"BADMAIN\":END\n1000 PRINT\"HANDLER\":END\n",
                p,
                &[1],
                false,
            ),
            "HANDLER",
            1,
        )
    });
}

#[test]
fn handler_goto_main_preserves_one_frame_without_poisoning_200_ordinary_calls() {
    profiles(|p| {
        let result = run(
            "10 ON KEY \"A\" GOSUB1000\n20 POKE49152,1\n30 PRINT\"BADMAIN\":END\n100 FORI=1TO200:GOSUB200:NEXT:PRINT\"DONE\";C:END\n200 C=C+1\n210 RETURN\n1000 GOTO100\n",
            p,
            &[1],
            false,
        );
        assert!(
            result.asm.contains("JSR __GOSUB_CHK")
                && result.asm.contains("L200:")
                && (result.asm.contains("JSR L200") || result.asm.contains("JMP L200")),
            "ordinary calls were inlined away"
        );
        clean(&result, "DONE 200", 1);
    });
}

#[test]
fn unbalanced_handler_gotos_raise_controlled_oom_before_stack_wrap() {
    profiles(|p| {
        let result = run(
            "10 ON KEY \"A\" GOSUB1000\n20 POKE49152,PEEK(49152)+1\n30 GOTO20\n1000 POKE49154,PEEK(49154)+1:GOTO20\n",
            p,
            &[0; 80],
            true,
        );
        assert!(result.terminal, "did not reach controlled OOM");
        assert_eq!(result.error, Some(16), "expected ROM OUT OF MEMORY");
        assert!(result.drained > 0 && result.drained < 80);
        assert_eq!(result.injected, result.drained);
        assert!(
            result.marker_sp.iter().all(|&sp| sp >= 32),
            "hardware stack approached wrap: {:?}",
            result.marker_sp
        );
    });
}

#[test]
fn goto_traps_and_clr_restarts_do_not_grow_the_hardware_stack() {
    profiles(|p| {
        for (action, count, handler) in [
            ("GOTO", 200, "1000 POKE49154,PEEK(49154)+1:GOTO20"),
            ("GOSUB", 150, "1000 POKE49154,PEEK(49154)+1:CLR:GOTO10"),
        ] {
            let source = format!(
                "10 ON KEY \"A\" {action}1000\n20 IFPEEK(49154)={count}THENPRINT\"DONE\":END\n30 POKE49152,PEEK(49152)+1\n40 GOTO20\n{handler}\n"
            );
            let result = run(&source, p, &vec![0; count], true);
            clean(&result, "DONE", count);
            assert_eq!(result.ram[2], count as u8);
            assert!(
                result
                    .marker_sp
                    .iter()
                    .all(|sp| Some(sp) == result.marker_sp.first()),
                "stack growth: {:?}",
                result.marker_sp
            );
        }
    });
}

#[test]
fn reentrant_presses_return_through_both_handler_frames() {
    profiles(|p| {
        clean(
            &run(
                "10 ON KEY \"A\" GOSUB1000\n20 POKE49152,1\n30 PRINT\"MAIN\":END\n1000 POKE49152,2\n1010 PRINT\"HANDLER\":RETURN\n",
                p,
                &[1, 2],
                false,
            ),
            "HANDLER\nHANDLER\nMAIN",
            2,
        )
    });
}

#[test]
fn clr_discards_ordinary_calls_reached_from_a_key_goto() {
    profiles(|p| {
        let result = run(
            "10 ON KEY \"A\" GOTO1000\n20 POKE49152,PEEK(49152)+1\n30 PRINT\"BADMAIN\":END\n1000 GOSUB2000\n1010 GOSUB2000\n1020 END\n2000 POKE49153,PEEK(49153)+1\n2010 IFPEEK(49153)=150THENPRINT\"DONE\":END\n2020 CLR\n2030 GOTO20\n",
            p,
            &vec![0; 150],
            true,
        );
        clean(&result, "DONE", 150);
        assert_eq!(result.ram[1], 150);
        assert_eq!(result.asm.matches("JSR L2000").count(), 2);
        assert!(
            result
                .marker_sp
                .iter()
                .all(|sp| Some(sp) == result.marker_sp.first()),
            "stack growth: {:?}",
            result.marker_sp
        );
    });
}

#[test]
fn disable_keeps_pending_key_until_explicit_rearm() {
    profiles(|p| {
        let result = run(
            "10 ON KEY \"A\" GOSUB1000\n20 POKE49152,1\n30 POKE49152,2\n40 POKE49155,PEEK(198)\n50 ON KEY \"A\" GOSUB1100\n60 PRINT\"MAIN\":END\n1000 PRINT\"FIRST\":DISABLE:RETURN\n1100 PRINT\"REARM\":DISABLE:RETURN\n",
            p,
            &[1, 2],
            false,
        );
        clean(&result, "FIRST\nREARM\nMAIN", 2);
        assert_eq!(result.ram[3], 1);
    });
}

#[test]
fn handler_for_barrier_rejects_caller_next_and_discards_its_own_loop() {
    profiles(|p| {
        let blocked = run(
            "10 ON KEY \"A\" GOSUB1000\n20 FORI=1TO3\n30 POKE49152,1\n40 PRINT\"BADMAIN\":NEXT:END\n1000 NEXTI:RETURN\n",
            p,
            &[1],
            false,
        );
        assert!(blocked.terminal);
        assert_eq!(blocked.drained, 1);
        assert_eq!(blocked.error, Some(10));
        assert!(!blocked.output.contains("BADMAIN"));
        let returned = run(
            "10 ON KEY \"A\" GOSUB1000\n20 FORI=1TO3\n30 POKE49152,1\n40 PRINT\"MAIN\";I:NEXT:END\n1000 FORJ=1TO9:PRINT\"HANDLER\":RETURN\n",
            p,
            &[1],
            false,
        );
        clean(&returned, "HANDLER\nMAIN 1 \nMAIN 2 \nMAIN 3", 1);
        for variable in ["I", "I%"] {
            let source = format!(
                "10 ON KEY \"A\" GOSUB1000\n20 FOR{variable}=1TO2\n30 POKE49152,1\n40 PRINT\"MAIN\";{variable}:NEXT:END\n1000 {variable}=5:RETURN\n"
            );
            clean(&run(&source, p, &[1], false), "MAIN 5", 1);
        }
    });
}

#[test]
fn clr_discards_the_callback_gosub_frame() {
    profiles(|p| {
        let result = run(
            "10 ON KEY \"A\" GOSUB1000\n20 POKE49152,1\n30 PRINT\"BADMAIN\":END\n1000 CLR:RETURN\n",
            p,
            &[1],
            false,
        );
        assert!(result.terminal);
        assert_eq!(result.drained, 1);
        assert_eq!(result.error, Some(12));
        assert!(!result.output.contains("BADMAIN"));
    });
}

#[test]
fn returning_restores_both_curlin_bytes_and_resume_next_target() {
    profiles(|p| {
        let read = run(
            "10 ON KEY \"A\" GOSUB1000\n20 POKE49152,1\n30 PRINT\"LINE\";PEEK(57)+256*PEEK(58):END\n1000 RETURN\n",
            p,
            &[1],
            false,
        );
        clean(&read, "LINE 30", 1);
        for (source, line) in [
            (
                "10 ON KEY \"A\" GOSUB1000\n20 POKE49152,1\n30 A=PEEK(-1):END\n1000 RETURN\n",
                30,
            ),
            (
                "10 ON KEY \"A\" GOSUB1000\n20 POKE49152,1:A=PEEK(-1):END\n1000 RETURN\n",
                20,
            ),
        ] {
            let error = run(source, p, &[1], false);
            assert!(error.terminal);
            assert_eq!(error.drained, 1);
            assert_eq!(error.error, Some(14));
            assert_eq!(error.error_line, Some(line));
        }
        let resume = run(
            "5 ON ERROR GOTO2000\n10 ON KEY \"A\" GOSUB1000\n20 POKE49152,1\n30 A=PEEK(-1)\n40 PRINT\"RESUMED\":END\n1000 RETURN\n1010 PRINT\"BADHANDLER\":END\n2000 PRINT\"ERROR\";EL:RESUME NEXT\n",
            p,
            &[1],
            false,
        );
        clean(&resume, "ERROR 30 \nRESUMED", 1);
    });
}
