//! Constant range facts simplify FOR bounds and ON selectors.

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
fn a_loader_with_bounds_in_variables() {
    let src = "\
10 A=49152:B=49170:FOR X=A TO B:READ D:POKE X,D:NEXT:PRINT X;PEEK(49152);PEEK(49170)
20 DATA 1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20
";
    same_as_the_rom(src);
    assert!(
        compiled(src, Profile::Default)
            .1
            .contains("JSR __GIVAYF_U16")
    );
}

#[test]
fn bounds_from_expressions_and_a_changed_end() {
    same_as_the_rom(
        "\
10 S=1:E=5:FOR I=S TO E:PRINT I;:NEXT:PRINT I
20 K=3:FOR I=K*2 TO K*2+3 STEP K-2:PRINT I;:NEXT
30 N=10:FOR I=1 TO N:N=2:NEXT:PRINT I;N
",
    );
}

#[test]
fn on_with_an_integer_selector() {
    same_as_the_rom(
        "\
10 FOR K=0 TO 4:A%=K:ON A% GOTO 20,30,40
15 PRINT \"N\";:GOTO 50
20 PRINT \"A\";:GOTO 50
30 PRINT \"B\";:GOTO 50
40 PRINT \"C\";
50 NEXT:PRINT
60 B%=-1:ON B% GOTO 20
",
    );
    same_as_the_rom("10 B%=300:ON B% GOTO 20\n20 PRINT \"X\"\n");
}
