//! SYS's ROM parser must see numeric results computed from compiled strings.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn source(setup: &str, param: &str) -> String {

    format!(
        "10 FORI=0TO12:READB:POKE49152+I,B:NEXT\n\
         20 {setup}\n30 SYS49152,{param}\n40 END\n\
         50 DATA32,253,174,32,158,173,32,221,189,32,30,171,96\n"
    )
}

fn run(prg: &[u8]) -> (bool, Vec<u8>, Option<u8>, Option<u16>) {
    let mut s = emu64::system::System::new();
    // The ROM cold start also seeds its function-dispatch JMP at $0054.
    s.mem.ram[0x54] = 0x4c;
    let entry = s.load_prg(prg).unwrap();
    s.start_at(entry);
    s.mem.ram[0x1ff] = 0xff;
    s.mem.ram[0x1fe] = 0xfd;
    s.cpu.regs.sp = 0xfd;
    for _ in 0..20_000_000 {
        if s.cpu.regs.pc == 0xfffe {
            return (true, s.output, s.error_code, s.error_line);
        }
        if !s.step() {
            break;
        }
    }
    (false, s.output, s.error_code, s.error_line)
}

#[test]
fn descriptor_and_snapshot_buffers_remain_atomic_across_reserved_holes() {
    let src = source("A$=\"123\"", "VAL(A$)");
    let prg = tokenize_program(&src).unwrap();
    for extra in [false, true] {
        let build = |reserved_ranges| {
            compile_with_options(
                &prg,
                CompileOptions {
                    extraram: extra,
                    reserved_ranges,
                    ..CompileOptions::default()
                },
            )
            .unwrap()
        };
        let linked = |c: &yabcompiler_core::Compiled| {
            let mut a = asm6502::Assembler6502::new();
            for &(lo, hi) in &c.diagnostics.effective_reserved {
                a.add_reserved_range(lo, hi).unwrap();
            }
            a.assemble_bytes(&c.asm).unwrap();
            a.symbols().clone()
        };
        let baseline = build(vec![]);
        let symbols = linked(&baseline);
        for (name, len) in [
            ("__SYS_EVAL_ZP", 256),
            ("__SYS_EVAL_HOMES", 256),
            ("__SYS_STR_VALUE_0", 259),
        ] {
            // Put the reservation inside the original buffer, past the first four bytes checked by the general literal-anchor verifier.
            let lo = symbols[name] as u16 + 17;
            let hi = lo + 31;
            let c = build(vec![(lo, hi)]);
            let new = linked(&c);
            let start = new[name] as usize;
            assert!(
                start > usize::from(hi) || start + len <= usize::from(lo),
                "split {name}"
            );
            let following = c.asm.split_once(&format!("{name}:\n")).unwrap().1;
            let line = following
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap()
                .trim();
            assert_eq!(line.strip_prefix(".byte ").unwrap().split(',').count(), len);
            let load = u16::from_le_bytes([c.prg_bytes[0], c.prg_bytes[1]]) as usize;
            assert!(
                c.prg_bytes[start - load + 2..start - load + 2 + len]
                    .iter()
                    .all(|&b| b == 0)
            );
            let executed = run(&c.prg_bytes);
            assert!(executed.0, "{name}/{extra}: {executed:?}");
            assert_eq!(common::petscii_to_string(&executed.1).trim(), "123");
        }
    }
}

#[test]
fn numeric_consumers_read_compiled_strings_through_sys() {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        for (setup, param, expected) in [
            ("A$=CHR$(34)+CHR$(0)", "ASC(A$)", "34"),
            ("A$=CHR$(0)+\"Q\"", "ASC(A$)", "0"),
            ("DIM S$(3):S$(2)=\"73.5\":K=2", "VAL(S$(K))", "73.5"),
            (
                "DIM S$(2,3):S$(1,2)=\"-7.25\"+CHR$(69)+\"1\":K=1:J=2",
                "VAL(S$(K,J))",
                "-72.5",
            ),
            ("A$=\"  1.25\"+CHR$(69)+\"2 REST\"", "VAL(A$)+1", "126"),
            ("A$=\"17\"+CHR$(0)+\"99\"", "VAL(A$)", "17"),
            ("A$=\"\"", "VAL(A$)", "0"),
            ("DIM N(9):N(3)=7:A$=\"3\"", "N(VAL(A$))", "7"),
            ("A$=\"5\"", "VAL(A$)+VAL(A$)+ASC(A$)", "63"),
        ] {
            let compiled = compile_with_options(
                &tokenize_program(&source(setup, param)).unwrap(),
                CompileOptions {
                    profile,
                    ..CompileOptions::default()
                },
            )
            .unwrap();
            assert!(
                compiled
                    .diagnostics
                    .sys_param_vars
                    .iter()
                    .all(|s| s.substituted),
                "{profile:?}: missing read in {param}"
            );
            let run = run(&compiled.prg_bytes);
            assert!(run.0, "{profile:?}: {param}: {run:?}");
            assert_eq!(
                common::petscii_to_string(&run.1).trim(),
                expected,
                "{profile:?}: {param}"
            );
        }
    }
}

#[test]
fn sys_numeric_string_errors_keep_the_sys_source_line() {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        for (setup, param, error) in [
            ("A$=\"\"", "ASC(A$)", 14),
            ("DIM A$(1):K=2", "VAL(A$(K))", 18),
            ("A$=\"1\"+CHR$(69)+\"99\"", "VAL(A$)", 15),
        ] {
            let compiled = compile_with_options(
                &tokenize_program(&source(setup, param)).unwrap(),
                CompileOptions {
                    profile,
                    ..CompileOptions::default()
                },
            )
            .unwrap();
            let run = run(&compiled.prg_bytes);
            assert_eq!(
                (run.2, run.3),
                (Some(error), Some(30)),
                "{profile:?}: {param}: {run:?}"
            );
        }
    }
}

#[test]
fn literal_only_numeric_consumers_keep_verbatim_sys_text() {
    for param in ["ASC(\"A\")", "VAL(\"17\")", "\"VAL(A$) ASC(B$)\""] {
        let compiled = compile_with_options(
            &tokenize_program(&format!("10 SYS828,{param}\n")).unwrap(),
            CompileOptions::default(),
        )
        .unwrap();
        assert!(!compiled.asm.contains("__SYS_TXTBUF"), "{param}");
        assert!(compiled.diagnostics.sys_param_vars.is_empty(), "{param}");
    }
}

#[test]
fn string_leaves_are_lazy_even_when_the_callee_ignores_them() {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        for (setup, param) in [
            ("A$=\"\"", "ASC(A$)"),
            ("A$=\"1\"+CHR$(69)+\"99\"", "VAL(A$)"),
            ("DIM A$(1):K=2", "A$(K)"),
        ] {
            let src = format!("10 POKE49152,96\n20 {setup}\n30 SYS49152,{param}\n40 END\n");
            let c = compile_with_options(
                &tokenize_program(&src).unwrap(),
                CompileOptions {
                    profile,
                    ..CompileOptions::default()
                },
            )
            .unwrap();
            let r = run(&c.prg_bytes);
            assert_eq!(
                (r.2, r.3),
                (Some(11), Some(30)),
                "{profile:?}: {param}: {r:?}"
            );
        }
    }
}

#[test]
fn native_print_keeps_distinct_live_descriptors_and_empty_strings() {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        for (setup, param, expected) in [
            ("A$=\"LEFT\":B$=\"RIGHT\"", "A$+B$+A$", "LEFTRIGHTLEFT"),
            ("K=2", "ZZ$;ZZ$(K);\"EMPTY\"", "EMPTY"),
            (
                "DIM N$(3):N$(2)=\"ROB\":A=2",
                "N$(A);\"'S TURN\"",
                "ROB'S TURN",
            ),
        ] {
            let src = format!(
                "10 FORI=0TO5:READB:POKE49152+I,B:NEXT\n20 {setup}\n30 SYS49152,{param}\n40 END\n50 DATA32,253,174,76,160,170\n"
            );
            let c = compile_with_options(
                &tokenize_program(&src).unwrap(),
                CompileOptions {
                    profile,
                    ..CompileOptions::default()
                },
            )
            .unwrap();
            let r = run(&c.prg_bytes);
            assert!(r.0, "{profile:?}: {param}: {r:?}");
            assert_eq!(
                common::petscii_to_string(&r.1).trim(),
                expected,
                "{profile:?}: {param}"
            );
        }
    }
}

#[test]
fn effectful_array_indices_are_not_admitted_to_the_callback() {
    for param in [
        "S$(INT(RND(1)*2))",
        "S$(USR(0))",
        "S$(FNA(1))",
        "S$(PEEK(2))",
    ] {
        let src = format!("10 DIM S$(3)\n20 SYS49152,{param}\n");
        let c = compile_with_options(&tokenize_program(&src).unwrap(), CompileOptions::default())
            .unwrap();
        assert!(!c.asm.contains("__SYS_EVAL_ENTER:"), "{param}");
        assert!(
            c.diagnostics
                .sys_param_vars
                .iter()
                .any(|s| s.var.kind == yabcompiler_core::ast::VarKind::String && !s.substituted),
            "{param}"
        );
    }
}

#[test]
fn required_string_bridges_cannot_be_dropped_or_exceed_staging() {
    for param in ["A$", "ASC(A$)", "LEN(A$)", "ZZ$(1)", "VAL(ZZ$(1))"] {
        let src = format!("10 A$=\"ABC\"\n20 SYS49152,{param}\n");
        let err = compile_with_options(
            &tokenize_program(&src).unwrap(),
            CompileOptions {
                no_sys_param_substitution: true,
                ..CompileOptions::default()
            },
        )
        .err()
        .expect("required bridge must be retained");
        assert!(
            err.to_string().contains("required compiled-string SYS"),
            "{param}: {err}"
        );
    }
    let src = format!("10 A$=\"ABC\"\n20 SYS49152,A$,\"{}\"\n", "X".repeat(250));
    let err = compile_with_options(&tokenize_program(&src).unwrap(), CompileOptions::default())
        .err()
        .expect("oversized required bridge must be refused");
    assert!(
        err.to_string().contains("staging beyond 250 bytes"),
        "{err}"
    );
    let src = format!(
        "10 DIM ZZ$(1)\n20 SYS49152,ZZ$(1),\"{}\"\n",
        "X".repeat(250)
    );
    let err = compile_with_options(&tokenize_program(&src).unwrap(), CompileOptions::default())
        .err()
        .expect("oversized array-only bridge must be refused");
    assert!(
        err.to_string().contains("staging beyond 250 bytes"),
        "{err}"
    );
    // The pre-existing numeric-only compatibility option keeps its behavior.
    let c = compile_with_options(
        &tokenize_program("10 A=3\n20 SYS49152,A\n").unwrap(),
        CompileOptions {
            no_sys_param_substitution: true,
            ..CompileOptions::default()
        },
    )
    .unwrap();
    assert!(!c.asm.contains("__SYS_TXTBUF"));
}
