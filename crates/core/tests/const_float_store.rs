//! Direct constant stores for float variables and loop exit values.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

type Outcome = (String, Option<u8>, Option<u16>);

fn interpreted(src: &str) -> Outcome {
    let prg = tokenize_program(src).expect("tokenizes");
    let r = emu64::run_basic_to_end(&prg, 100_000_000).expect("runs in the interpreter");
    (
        common::petscii_to_string(&r.output),
        r.error_code,
        r.error_line,
    )
}

fn compiled(src: &str, profile: Profile, extraram: bool) -> (Outcome, String) {
    let prg = tokenize_program(src).expect("tokenizes");
    let c = compile_with_options(
        &prg,
        CompileOptions {
            profile,
            extraram,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}\n{src}"));
    let r = emu64::run_prg_to_end(&c.prg_bytes, 100_000_000).expect("runs");
    (
        (
            common::petscii_to_string(&r.output),
            r.error_code,
            r.error_line,
        ),
        c.asm,
    )
}

fn check(src: &str) -> String {
    let want = interpreted(src);
    let mut speed_asm = String::new();
    for profile in [Profile::Speed, Profile::Default, Profile::Size] {
        for extraram in [false, true] {
            let (got, asm) = compiled(src, profile, extraram);
            assert_eq!(got, want, "{profile:?} extraram {extraram}:\n{src}");
            if profile == Profile::Speed && !extraram {
                speed_asm = asm;
            }
        }
    }
    speed_asm
}

#[test]
fn loop_exit_values_match_the_interpreter() {
    let asm = check(
        "10 FOR A=1 TO 50:NEXT:PRINT A:A=A/3:PRINT A\n\
         20 FOR B=-3 TO 300:NEXT:PRINT B:B=B/7:PRINT B\n\
         30 FOR C=10 TO 0 STEP -1:NEXT:PRINT C:C=C/3:PRINT C\n\
         40 FOR D=7 TO 0 STEP -2:NEXT:PRINT D:D=D*.5:PRINT D\n\
         50 FOR E=5 TO 1:NEXT:PRINT E:E=E/9:PRINT E\n\
         60 FOR F=-5 TO -300 STEP -7:NEXT:PRINT F:F=F/3:PRINT F\n\
         70 FOR G=0 TO 1000 STEP 3:NEXT:PRINT G:G=G/7:PRINT G\n\
         80 FOR H=1 TO 32766:NEXT:PRINT H:H=H/3:PRINT H\n",
    );
    for v in ["V_A", "V_B", "V_C", "V_G"] {
        assert!(asm.contains(&format!("    STA {v}+1\n")), "{v}:\n{asm}");
    }
}

#[test]
fn constants_match_the_interpreter() {
    check(
        "10 A=7:B=-1:C=0:D=1:E=.5:F=-32768:G=1E30:H=-1E-30:I=65535:J=3.14159265\n\
         20 PRINT A;B;C;D;E;F;G;H;I;J\n\
         30 A=A/3:B=B/3:C=C/3:D=D/3:E=E/3:F=F/3:G=G/3:H=H/3:I=I/3:J=J/3\n\
         40 PRINT A;B;C;D;E;F;G;H;I;J\n",
    );
}

#[test]
fn a_constant_store_does_not_leave_a_stale_fac() {
    check(
        "10 READ X:A=X+1:A=5:B=A+1:PRINT A;B:C=X*2:C=0:D=C-X:PRINT C;D\n\
         20 FOR I=1 TO 3:A=A+X:A=.25:B=B+A:NEXT:PRINT A;B;I\n\
         30 E=X:E=9:PRINT E+X;X+E\n\
         40 DATA 2.5\n",
    );
}
