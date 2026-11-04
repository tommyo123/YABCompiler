//! Check that leading invariant LETs preserve interpreter values and errors.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

type Outcome = (String, Option<u8>, Option<u16>);

fn interpreted(src: &str) -> Outcome {
    let prg = tokenize_program(src).expect("tokenizes");
    let r = emu64::run_basic_to_end(&prg, 200_000_000).expect("runs in the interpreter");
    (
        common::petscii_to_string(&r.output),
        r.error_code,
        r.error_line,
    )
}

fn compiled(src: &str, profile: Profile, fast_float: bool) -> (Outcome, String) {
    let prg = tokenize_program(src).expect("tokenizes");
    let c = compile_with_options(
        &prg,
        CompileOptions {
            profile,
            fast_float,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}\n{src}"));
    let r = emu64::run_prg_to_end(&c.prg_bytes, 200_000_000).expect("runs");
    (
        (
            common::petscii_to_string(&r.output),
            r.error_code,
            r.error_line,
        ),
        c.asm,
    )
}

fn same_as_the_rom(src: &str) -> String {
    let want = interpreted(src);
    let mut default_asm = String::new();
    for (profile, fast_float) in [
        (Profile::Default, false),
        (Profile::Speed, false),
        (Profile::Size, false),
        (Profile::Default, true),
        (Profile::Speed, true),
    ] {
        let (got, asm) = compiled(src, profile, fast_float);
        assert_eq!(got, want, "{profile:?} fast float {fast_float}:\n{src}");
        if profile == Profile::Default && !fast_float {
            default_asm = asm;
        }
    }
    default_asm
}

/// Hoist an invariant LET in a loop with conditional NEXT and RETURN exits.
#[test]
fn a_sphere_loop_computes_its_length_once() {
    let src = "10 DIM KX(2):KX(0)=3:KX(1)=-2:KX(2)=7:RX=.3:RY=-.7:RZ=1.1:OX=.5\n\
               20 FOR N=1 TO 3:GOSUB 100:PRINT K;T:RX=RX+.1:NEXT:END\n\
               100 K=-1:T=99:FOR I=0 TO 2:ZW=RX*RX+RY*RY+RZ*RZ:P=(OX-KX(I))/ZW:IF P<0 THEN NEXT:RETURN\n\
               110 IF P<T THEN T=P:K=I\n\
               120 NEXT:RETURN\n";
    let asm = same_as_the_rom(src);
    // Store ZW before loop entry.
    let sub = asm.find("L100:").expect("the subroutine");
    let top = sub + asm[sub..].find("__FOR_TOP").expect("a loop");
    let zw = sub + asm[sub..].find("#<V_ZW").expect("ZW is stored");
    assert!(zw < top, "{asm}");
}

/// Hoist division, SQR and later LETs while preserving the first error.
#[test]
fn values_and_errors_stay_the_interpreters() {
    same_as_the_rom("10 A=4:B=9:FOR I=1 TO 4:Q=SQR(A*B)/(B-A):PRINT Q*I;:NEXT:PRINT Q\n");
    same_as_the_rom("10 A=0:FOR I=1 TO 4:Q=5/A:PRINT I;:NEXT\n");
    same_as_the_rom("10 A=-4:FOR I=1 TO 4:Q=SQR(A):PRINT I;:NEXT\n");
    same_as_the_rom("10 B=3:C=2:FOR S=1 TO 5:Z1=S*B+C:Z2=S*C-B:Z3=B*C+1:PRINT Z1;Z2;Z3;:NEXT:PRINT Z3\n");
    // Overflow in an earlier LET and in a hoisted one stops with the same error.
    same_as_the_rom("10 B=1E38:C=1E30:FOR S=1 TO 5:Z1=S*B*10:Z3=C*C:PRINT S;:NEXT\n");
}

/// Retain LETs with changed inputs, earlier reads, header dependencies or prior division.
#[test]
fn a_changing_or_early_read_value_stays_in_the_loop() {
    same_as_the_rom("10 A=1:FOR I=1 TO 4:Q=A*2:A=A+Q:PRINT Q;:NEXT\n");
    same_as_the_rom("10 Q=7:A=2:FOR I=1 TO 3:Z=Q+1:Q=A*3:PRINT Z;Q;:NEXT\n");
    same_as_the_rom("10 Q=2:FOR I=1 TO Q:Q=5:PRINT I;:NEXT\n");
    same_as_the_rom("10 A=0:B=2:FOR I=1 TO 3:Z=I/A:Q=B*B:PRINT Q;:NEXT\n");
    same_as_the_rom("10 A=3:FOR I=1 TO 3:Q=A*A:PRINT Q;:IF I=2 THEN A=5\n20 NEXT\n");
}
