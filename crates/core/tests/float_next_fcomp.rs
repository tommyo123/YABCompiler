mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn same_as_the_rom(src: &str) -> Vec<String> {
    let prg = tokenize_program(src).expect("tokenizes");
    let r = emu64::run_basic_to_end(&prg, 40_000_000).expect("runs in the interpreter");
    let want = (common::petscii_to_string(&r.output), r.error_code, r.error_line);
    let mut asms = Vec::new();
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let c = compile_with_options(&prg, CompileOptions { profile, ..CompileOptions::default() })
            .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}"));
        let r = emu64::run_prg_to_end(&c.prg_bytes, 40_000_000).expect("runs");
        assert_eq!(
            (common::petscii_to_string(&r.output), r.error_code, r.error_line),
            want,
            "{profile:?}:\n{src}"
        );
        asms.push(c.asm);
    }
    asms
}

#[test]
fn fractional_and_negative_steps() {
    let asms = same_as_the_rom(
        "10 FOR I=0 TO 1 STEP .1:PRINT I;:NEXT:PRINT I\n\
         20 FOR I=1 TO 0 STEP -.1:PRINT I;:NEXT:PRINT I\n\
         30 N=0:FOR I=0 TO 10 STEP .01:N=N+1:NEXT:PRINT N,I\n\
         40 N=0:FOR I=5 TO -5 STEP -.3:N=N+1:NEXT:PRINT N,I\n\
         50 FOR I=1E9 TO 1E9+5:NEXT:PRINT I\n\
         60 FOR I=.5 TO 3:PRINT I;:NEXT:PRINT I\n",
    );
    for asm in asms {
        assert!(asm.contains("JSR $BC5B"), "{asm}");
    }
}

#[test]
fn zero_and_run_time_steps() {
    same_as_the_rom(
        "10 FOR I=1 TO 1 STEP 0:PRINT \"Z\";I:NEXT\n\
         20 FOR K=1 TO 6:READ S,E\n\
         30 C=0:FOR I=2 TO E STEP S:C=C+1:IF C>20 THEN PRINT \"STOP\":GOTO 50\n\
         40 NEXT I\n\
         50 PRINT S;E;C;I:NEXT K\n\
         60 DATA .5,4,-.75,-1,0,2,0,3,3,1,-2,3\n",
    );
}

#[test]
fn counters_the_body_writes_or_shadows() {
    same_as_the_rom(
        "10 FOR I=1 TO 10:I=I+.5:PRINT I;:NEXT:PRINT I\n\
         20 FOR J=0 TO 30 STEP 3:POKE 1024+J,J:PRINT PEEK(1024+J);:NEXT:PRINT J\n\
         30 FOR K=1 TO 4:PRINT K*3;K*3+1;:NEXT:PRINT K\n\
         40 E=3:FOR L=1 TO E:E=0:PRINT L;:NEXT:PRINT L\n\
         50 FOR M=-3 TO 3 STEP 1.5:PRINT M;:NEXT:PRINT M\n",
    );
}

#[test]
fn values_near_the_float_limits() {
    same_as_the_rom(
        "10 FOR I=3E-39 TO 9E-39 STEP 3E-39:PRINT I;:NEXT:PRINT I\n\
         20 FOR I=1.7E38 TO 1.69E38 STEP -1E36:PRINT I;:NEXT:PRINT I\n\
         30 FOR I=1 TO 1.0000001:PRINT I;:NEXT:PRINT I\n\
         40 FOR I=16777215 TO 16777218:PRINT I;:NEXT:PRINT I\n",
    );
}
