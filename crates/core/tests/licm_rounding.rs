mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn build(src: &str, profile: Profile) -> yabcompiler_core::Compiled {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(&prg, CompileOptions { profile, ..CompileOptions::default() })
        .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}"))
}

fn same_as_the_rom(src: &str) {
    let prg = tokenize_program(src).expect("tokenizes");
    let r = emu64::run_basic_to_end(&prg, 20_000_000).expect("runs in the interpreter");
    let want = (common::petscii_to_string(&r.output), r.error_code, r.error_line);
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let r = emu64::run_prg_to_end(&build(src, profile).prg_bytes, 20_000_000).expect("runs");
        assert_eq!(
            (common::petscii_to_string(&r.output), r.error_code, r.error_line),
            want,
            "{profile:?}:\n{src}"
        );
    }
}

#[test]
fn unrounded_uses_stay_in_the_loop() {
    same_as_the_rom(
        "10 READ X:DATA -2\n20 X=X/3\n\
         30 FOR L=1 TO 2:I%=X*2+X:PRINT I%;:NEXT\n\
         40 FOR L=1 TO 2:PRINT X*2+X;INT(X*2+X);1+(X*2+X);:NEXT\n\
         50 FOR L=1 TO 2:J%=1-(X*2+X):PRINT J%;:NEXT\n",
    );
}

/// Keep fallible expressions after PRINT to preserve output and error lines.
#[test]
fn a_hoist_never_moves_an_error_ahead() {
    for src in [
        "10 B=1E30:C=1E30:D=5:IF D=3 THEN B=1:C=2\n20 FOR I=1 TO 3:PRINT I;:A=B*C+D:NEXT\n",
        "10 B=1E30:C=1E30:D=5:IF D=3 THEN B=1:C=2\n20 FOR I=1 TO 3\n30 A=B*C+D\n40 NEXT\n",
        "10 Z=0:B=1E30:IF B=3 THEN Z=1\n20 FOR I=1 TO 1/Z:A=B*B:NEXT\n",
        "10 B=1E30:C=1E30:Z=0:IF Z=3 THEN B=1\n20 FOR I=1 TO 3:A=I/Z+B*C:NEXT\n",
    ] {
        same_as_the_rom(src);
    }
}

#[test]
fn rounded_uses_still_hoist() {
    // Reading L keeps the LET in the loop; its invariant subexpression can move.
    let src = "10 READ X:DATA -2\n20 X=X/3\n\
               30 FOR L=1 TO 2:A=X*2+X+L:B=(X*2+X)*3:PRINT A;B;:NEXT\n";
    same_as_the_rom(src);
    assert!(build(src, Profile::Default).asm.contains("__LICM_"), "the hoist is gone");
    // Leading invariant LETs run once before the FOR.
    same_as_the_rom(
        "10 READ X:DATA -2\n20 X=X/3\n\
         30 FOR L=1 TO 2:A=X*2+X:B=(X*2+X)*3:PRINT A;B;:NEXT\n",
    );
}
