//! Embedded float routines produce matching program output.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

type Outcome = (String, Option<u8>, Option<u16>);

fn interpreted(src: &str) -> Outcome {
    let prg = tokenize_program(src).expect("tokenizes");
    let r = emu64::run_basic_to_end(&prg, 400_000_000).expect("runs in the interpreter");
    (
        common::petscii_to_string(&r.output),
        r.error_code,
        r.error_line,
    )
}

fn build(src: &str, profile: Profile, fast_float: bool) -> yabcompiler_core::Compiled {
    let prg = tokenize_program(src).expect("tokenizes");
    let compiled = compile_with_options(
        &prg,
        CompileOptions {
            profile,
            fast_float,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}\n{src}"));
    assert!(
        !compiled.diagnostics.fast_float_dropped,
        "fast float was left out ({profile:?}):\n{src}"
    );
    compiled
}

fn run(c: &yabcompiler_core::Compiled) -> Outcome {
    let r = emu64::run_prg_to_end(&c.prg_bytes, 400_000_000).expect("runs");
    (
        common::petscii_to_string(&r.output),
        r.error_code,
        r.error_line,
    )
}

/// Arithmetic across signs and exponent gaps, including table initialization.
const PROGRAMS: &[&str] = &[
    "10 S=0:X=1.000123\n\
     20 FOR I=1 TO 200:S=S+X*I/3:X=X*1.0071-I*.0001:NEXT\n\
     30 PRINT S;X\n\
     40 A=3.14159:B=-2.5:PRINT A*B;A+B;A-B;B*B*B;1/3*3\n",
    "10 X=.1:FOR I=1 TO 60:X=X*1.37+.003:IF X>1E6 THEN X=X*1E-9\n\
     20 Y=X-INT(X):PRINT Y;:NEXT\n",
    "10 A=1:B=1:FOR I=1 TO 80:C=A+B:A=B:B=C:PRINT C/A;:NEXT\n",
    "10 FOR I=-40 TO 40:A=2^(I/4)*1.1:B=-A/7:C=A+B:D=A-B:E=A*B:PRINT C;D;E;:NEXT\n",
    "10 X=123456789:Y=.000123456:FOR I=1 TO 40:X=X*.97+Y:Y=Y*1.5-X*1E-12:NEXT:PRINT X;Y\n",
    "10 FOR I=1 TO 50:A=I*.1:B=A*A-A*.2+.01:PRINT INT(B*100);:NEXT\n",
    "10 X=0:FOR I=0 TO 1 STEP .05:X=X+I*I:NEXT:PRINT X;I\n",
    "10 A=1E38:B=A*3:PRINT B\n",
    "10 A=1E-38:B=A*1E-3:PRINT B;A-A;A+A\n",
    "10 FOR I=1 TO 30:X=I*1.7:PRINT SQR(X);LOG(X);EXP(X/9);X^1.5;(-X)^3;X^.5;:NEXT\n",
    "10 PRINT SQR(0);EXP(0);2^0;0^2;EXP(-100);EXP(88);10^-38;LOG(1)\n",
    "10 FOR I=-5 TO 5:A=I*.5:B=(I+.3)*2:C=I*256:D=(I*.1)*.0009765625:E=I*1/3*4:PRINT A;B;C;D;E;:NEXT\n\
     20 X=1E-38:Y=X*.5:Z=X*.25:PRINT Y;Z;X*2:X=1.7E38:PRINT X*.5:PRINT X*2\n",
];

#[test]
fn prints_what_the_rom_prints() {
    for src in PROGRAMS {
        let want = interpreted(src);
        for profile in [Profile::Default, Profile::Speed, Profile::Size] {
            assert_eq!(run(&build(src, profile, true)), want, "{profile:?}:\n{src}");
        }
    }
}

/// The tables survive a second RUN.
#[test]
fn a_second_run_reuses_the_tables() {
    let src = "10 S=0:FOR I=1 TO 50:S=S+I*1.25*I:NEXT:PRINT S\n\
               20 IF PEEK(828)=7 THEN END\n\
               30 POKE 828,7:RUN\n";
    let want = interpreted(src);
    assert_eq!(run(&build(src, Profile::Default, true)), want);
}

/// The table area takes RAM, not file space.
#[test]
fn the_tables_are_not_in_the_file() {
    let src = PROGRAMS[0];
    let fast = build(src, Profile::Default, true);
    assert!(fast.asm.contains("__FF_TBL:"), "{}", fast.asm);
    let stored = fast.prg_bytes.len() - 2;
    assert!(
        fast.diagnostics.bss_truncated >= 6 * 256,
        "{stored} bytes stored"
    );
}
