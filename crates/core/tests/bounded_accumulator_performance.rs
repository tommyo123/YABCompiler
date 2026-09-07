//! Trip-count promotion must retain BASIC's float range and evaluation.
mod common;

use yabcompiler_core::{compile_with_options, tokenize_program, CompileOptions, Profile};

fn check(src: &str, expected: &str, promoted: bool) {
    for profile in [Profile::Default, Profile::Size, Profile::Speed] {
        let input = tokenize_program(src).unwrap();
        let compiled = compile_with_options(
            &input,
            CompileOptions {
                profile,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            compiled.asm.contains("VI_S"),
            promoted,
            "{profile:?}:\n{src}\n{}",
            compiled.asm
        );
        let result = emu64::run_prg_to_end(&compiled.prg_bytes, 200_000_000).unwrap();
        assert!(
            result.clean_exit,
            "{profile:?}: did not exit cleanly\n{src}"
        );
        assert_eq!(
            common::petscii_to_string(&result.output),
            expected,
            "{profile:?}:\n{src}"
        );
    }
}

#[test]
fn inlined_gosub_accumulator_promotes_the_whole_split_family() {
    check("10 S=0\n20 FOR I=1 TO 100\n30 GOSUB 100:GOSUB 100:GOSUB 100:GOSUB 100\n40 NEXT\n50 PRINT S:END\n100 S=S+1:RETURN\n", " 400 \n", true);
}

#[test]
fn fractional_gosub_accumulator_does_not_reuse_a_stale_fac() {
    check("10 S=0\n20 FOR I=1 TO 100\n30 GOSUB 100:GOSUB 100:GOSUB 100:GOSUB 100\n40 NEXT\n50 PRINT S:END\n100 S=S+0.5:RETURN\n", " 200 \n", false);
}

#[test]
fn literal_accumulator_overflow_keeps_float_storage() {
    check(
        "10 S=0\n20 FOR I=1 TO 200\n30 S=S+200\n40 NEXT\n50 PRINT S\n",
        " 40000 \n",
        false,
    );
}

#[test]
fn overflowing_later_lifetime_does_not_demote_the_bounded_earlier_one() {
    let src = "10 S=32767\n20 S=S+1\n30 PRINT S\n";
    for profile in [Profile::Default, Profile::Size, Profile::Speed] {
        let compiled = compile_with_options(&tokenize_program(src).unwrap(), CompileOptions {
            profile, ..Default::default()
        }).unwrap();
        assert!(compiled.asm.contains("VI_S"));
        assert!(!compiled.asm.contains("VI_S__1"));
        let result = emu64::run_prg_to_end(&compiled.prg_bytes, 200_000_000).unwrap();
        assert!(result.clean_exit);
        assert_eq!(common::petscii_to_string(&result.output), " 32768 \n");
    }
}

#[test]
fn variable_accumulator_overflow_keeps_float_storage() {
    check("10 POKE 828,200:A=PEEK(828):S=0\n20 FOR I=1 TO 200\n30 S=S+A\n40 POKE 1024,S-I*A+I\n50 NEXT\n60 PRINT S\n", " 40000 \n", false);
}

#[test]
fn restarted_for_does_not_reset_the_accumulator_bound() {
    check("10 S=0:R=0\n20 FOR I=1 TO 100\n30 S=S+1:S=S+1\n40 NEXT\n50 R=R+1:IF R<200 THEN 20\n60 PRINT S\n", " 40000 \n", false);
}

#[test]
fn nested_decreasing_accumulator_gets_a_signed_bound() {
    check("10 S=1000\n20 FOR I=1 TO 10\n30 FOR J=20 TO 1 STEP -1\n40 S=S-1:S=S-1\n50 NEXT J\n60 NEXT I\n70 PRINT S\n", " 600 \n", true);
}

#[test]
fn unproved_masked_family_keeps_its_existing_arithmetic_semantics() {
    check("10 S=0\n20 FOR I=1 TO 100\n30 S=(S+1)AND1023:S=(S+1)AND1023:S=(S+1)AND1023:S=(S+1)AND1023\n40 NEXT\n50 PRINT S\n", " 400 \n", true);
}

#[test]
fn single_step_trap_preserves_both_distinct_integer_variables() {

    for (bytes, operation, old_source) in [
        ("255:POKE829,127", "+1", "32767"),
        ("0:POKE829,128", "-1", "-32768"),
    ] {
        let src = format!("10 ON ERROR GOTO100\n20 POKE828,{bytes}:A%=7\n30 B%=PEEK(828)+256*PEEK(829)\n35 IF B%<0 THEN 40\n40 A%=B%{operation}\n50 PRINT\"MISSED\":END\n100 PRINT A%;B%:END\n");

        let src = if operation == "-1" {
            src.replace(
                "B%=PEEK(828)+256*PEEK(829)",
                "B%=PEEK(828)+256*PEEK(829)-65536",
            )
        } else {
            src
        };
        for profile in [Profile::Default, Profile::Size, Profile::Speed] {
            let compiled = compile_with_options(
                &tokenize_program(&src).unwrap(),
                CompileOptions {
                    profile,
                    ..Default::default()
                },
            )
            .unwrap();
            assert!(
                compiled.asm.contains("__STEP_OK_"),
                "exercise distinct-variable trap:\n{}",
                compiled.asm
            );
            let mut machine = emu64::System::new();
            let entry = machine.load_prg(&compiled.prg_bytes).unwrap();
            machine.start_at(entry);
            machine.mem.ram[0x1ff] = 0xff;
            machine.mem.ram[0x1fe] = 0xfd;
            machine.cpu.regs.sp = 0xfd;
            for _ in 0..1_000_000 {
                if machine.cpu.regs.pc == 0xfffe || !machine.step() {
                    break;
                }
            }
            assert!(machine.cpu.regs.pc == 0xfffe || machine.ready_reached);
            let mut output = machine.output;
            output.extend(machine.error_output);
            let leading = if operation == "+1" { " " } else { "" };
            assert_eq!(
                common::petscii_to_string(&output),
                format!(" 7 {leading}{old_source} \n"),
                "{profile:?}:\n{src}"
            );
        }
    }
}
