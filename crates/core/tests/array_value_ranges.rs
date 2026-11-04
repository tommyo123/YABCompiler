//! Check array promotion and inferred element ranges.
//! Retain floats for fractional values or ranges exceeding i16.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

type Outcome = (String, Option<u8>, Option<u16>);

fn interpreted(src: &str) -> Outcome {
    let prg = tokenize_program(src).expect("tokenizes");
    let r = emu64::run_basic_to_end(&prg, 400_000_000).expect("runs in the interpreter");
    (common::petscii_to_string(&r.output), r.error_code, r.error_line)
}

fn compiled(src: &str, profile: Profile, fast_float: bool) -> (Outcome, String) {
    let prg = tokenize_program(src).expect("tokenizes");
    let c = compile_with_options(&prg, CompileOptions { profile, fast_float, ..CompileOptions::default() })
        .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}\n{src}"));
    let r = emu64::run_prg_to_end(&c.prg_bytes, 400_000_000).expect("runs");
    ((common::petscii_to_string(&r.output), r.error_code, r.error_line), c.asm)
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

/// Random positions and squares fill float arrays with small integers.
#[test]
fn integer_valued_float_arrays_become_integer_arrays() {
    let asm = same_as_the_rom(
        "10 DIM SY(20),Q(10):R=RND(-7):FOR I=0 TO 20:SY(I)=INT(RND(1)*25):NEXT\n\
         20 FOR I=0 TO 10:Q(I)=I*I-I+1:NEXT\n\
         30 S=0:FOR I=0 TO 20:S=S+40*SY(I):NEXT:PRINT S\n\
         40 FOR I=0 TO 10:PRINT Q(I);:NEXT\n",
    );
    assert!(asm.contains("__ARR_SY_I"), "{asm}");
    assert!(asm.contains("__ARR_Q_I"), "{asm}");
}

/// Retain float values for unbounded growth and fractional elements.
#[test]
fn growing_or_fractional_elements_stay_float() {
    same_as_the_rom(
        "10 DIM A(3),B(3):A(1)=1\n\
         20 FOR I=1 TO 40:A(1)=A(1)*2+B(2):B(2)=A(1)-1:NEXT\n\
         30 PRINT A(1);B(2)\n",
    );
    same_as_the_rom(
        "10 DIM X(5):FOR I=0 TO 5:X(I)=I/2:NEXT\n\
         20 FOR I=0 TO 5:PRINT X(I);:NEXT\n",
    );
    // A countdown that wraps through a reset keeps its transient value.
    same_as_the_rom(
        "10 DIM SX(9):FOR I=0 TO 9:SX(I)=I*4:NEXT\n\
         20 FOR N=1 TO 30:FOR I=0 TO 9:SX(I)=SX(I)-1:IF SX(I)<0 THEN SX(I)=39\n\
         30 NEXT:NEXT:FOR I=0 TO 9:PRINT SX(I);:NEXT\n",
    );
    // READ into an array and a value that leaves i16 through it.
    same_as_the_rom(
        "10 DIM D(4):FOR I=0 TO 4:READ D(I):NEXT:DATA 1,200,3000,-4,5\n\
         20 FOR I=0 TO 4:PRINT D(I)*D(I)*D(I);:NEXT\n",
    );
    // CLR in between clears the elements to zero.
    same_as_the_rom(
        "10 DIM E(3):E(1)=7:CLR:DIM E(3):PRINT E(1);:E(2)=E(1)+5:PRINT E(2)\n",
    );
}
