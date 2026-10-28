//! Bounds from INT and RND permit integer variables for every random result.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

type Outcome = (Vec<u8>, Option<u8>, Option<u16>);

const BUDGET: u64 = 20_000_000;

fn interpreted(src: &str) -> Outcome {
    let prg = tokenize_program(src).expect("tokenizes");
    let r = emu64::run_basic_to_end(&prg, BUDGET).expect("runs in the interpreter");
    assert!(r.ready_reached, "the interpreter reaches READY:\n{src}");
    (r.output, r.error_code, r.error_line)
}

fn compiled(src: &str, profile: Profile) -> (Outcome, String) {
    let prg = tokenize_program(src).expect("tokenizes");
    let c = compile_with_options(
        &prg,
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}\n{src}"));
    let r = emu64::run_prg_to_end(&c.prg_bytes, BUDGET).expect("runs");
    assert!(
        r.clean_exit || r.error_code.is_some(),
        "ends ({profile:?}):\n{src}"
    );
    ((r.output, r.error_code, r.error_line), c.asm)
}

fn same_as_the_rom(src: &str) {
    let want = interpreted(src);
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let (got, _) = compiled(src, profile);
        assert_eq!(
            (common::petscii_to_string(&got.0), got.1, got.2),
            (common::petscii_to_string(&want.0), want.1, want.2),
            "{profile:?}:\n{src}"
        );
    }
}

#[test]
fn values_stay_inside_the_bounds() {
    same_as_the_rom(
        "\
10 FOR K=1 TO 200:Z=INT(RND(1)*26+65):IF Z<65 OR Z>90 THEN PRINT \"BAD\";Z
20 NEXT
30 FOR K=1 TO 100:X=INT(RND(1)*40):Y=INT(RND(0)*25):POKE 1024+Y*40+X,42:NEXT
40 FOR K=1 TO 50:D=INT(RND(1)*4)-1:IF D<-1 OR D>2 THEN PRINT \"BAD\";D
50 NEXT
60 A=INT(RND(1)*6)+1:B=INT(6*RND(1)+1):C=INT(RND(1)/2*10):PRINT A>0;B<7;C<5
70 Z=INT(RND(1)*10):FOR T=1 TO 50+Z:NEXT:PRINT T>50;T<61
",
    );
}

#[test]
fn the_variable_becomes_an_integer() {
    let src = "10 FOR K=1 TO 9:X=INT(RND(1)*40):POKE 1024+X,1:NEXT\n";
    assert!(compiled(src, Profile::Default).1.contains("VI_X"));
    assert!(compiled(src, Profile::Speed).1.contains("VI_X"));
}

#[test]
fn an_exact_lower_bound_keeps_the_variable_unsigned() {
    let src = "10 FOR K=1 TO 9:X=INT(RND(1)*40):POKE 1024+X,1:NEXT\n";
    assert!(!compiled(src, Profile::Default).1.contains("__SXT_"));
}
