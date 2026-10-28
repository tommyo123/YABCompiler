//! Skip counter start stores only when a straight loop guarantees an exit store.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn check(src: &str) {
    let prg = tokenize_program(src).expect("tokenizes");
    let rom = emu64::run_basic_to_end(&prg, 50_000_000).expect("runs in the interpreter");
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let c = compile_with_options(
            &prg,
            CompileOptions {
                profile,
                ..CompileOptions::default()
            },
        )
        .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}\n{src}"));
        let r = emu64::run_prg_to_end(&c.prg_bytes, 50_000_000).expect("runs");
        assert_eq!(
            (
                common::petscii_to_string(&r.output),
                r.error_code,
                r.error_line
            ),
            (
                common::petscii_to_string(&rom.output),
                rom.error_code,
                rom.error_line
            ),
            "{profile:?}:\n{src}"
        );
    }
}

fn stores_to(src: &str, var: &str, profile: Profile) -> usize {
    let prg = tokenize_program(src).expect("tokenizes");
    let c = compile_with_options(
        &prg,
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .expect("compiles");
    let asm = common::expand_call_entries(&c.asm);
    // MOVMF calls, plus constants stored byte by byte (one store of the exponent byte each).
    ["$BBD4", "__MOVMF"]
        .iter()
        .map(|movmf| {
            asm.matches(&format!(
                "    LDX #<{var}\n    LDY #>{var}\n    JSR {movmf}\n"
            ))
            .count()
        })
        .sum::<usize>()
        + asm.matches(&format!("    STA {var}\n")).count()
}

#[test]
fn a_delay_loop_stores_its_variable_once() {
    // A stays float through `A/3`: the loop's exit store and that LET are the only stores.
    let src = "10 FOR A=1 TO 50:NEXT:PRINT A\n20 A=A/3:PRINT A\n";
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        assert_eq!(stores_to(src, "V_A", profile), 2, "{profile:?}");
    }
    check(src);
}

#[test]
fn the_exit_value_replaces_an_earlier_store() {
    check("10 C=99:FOR C=5 TO 9:NEXT:PRINT C\n20 FOR A=1 TO 50:NEXT:PRINT A\n");
    check("10 FOR C=1 TO 300:NEXT:PRINT C:FOR D=-5 TO 5 STEP 2:NEXT:PRINT D\n");
    check("10 FOR E=40000 TO 40010:NEXT:PRINT E\n");
    check("10 FOR C=1 TO 3:PRINT \"X\";:POKE 828,7:NEXT:PRINT C\n");
}

#[test]
fn loops_left_another_way_keep_the_start_value() {
    check("10 FOR A=1 TO 2:FOR C=5 TO 9:NEXT A:PRINT C;A\n");
    check("10 X=1:FOR A=1 TO 3:C=7:FOR C=5 TO 9:IF X THEN NEXT A\n20 NEXT C:PRINT C;A\n");
    check("10 C=99:FOR C=5 TO 9:PRINT C;:END:NEXT\n");
    check("10 FOR C=1 TO 3\n20 PRINT \"X\";\n30 NEXT\n40 PRINT C:IF C<10 THEN C=20:GOTO 20\n");
    check("10 C=99:FOR C=5 TO 9:POKE 828,256:NEXT\n20 PRINT C\n");
    check("10 DEF FN F(Q)=C*2\n20 FOR C=1 TO 3:B=B+FN F(1):NEXT:PRINT B;C\n");
}
