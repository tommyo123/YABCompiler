//! Static array bounds across DIM, CLR, RUN, and control-flow joins.

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

/// Compare every profile with the ROM and return the default profile's asm.
fn check(src: &str) -> String {
    let want = interpreted(src);
    let mut asm = String::new();
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        for extraram in [false, true] {
            let (got, a) = compiled(src, profile, extraram);
            assert_eq!(got, want, "{profile:?} extraram {extraram}:\n{src}");
            if profile == Profile::Default && !extraram {
                asm = a;
            }
        }
    }
    asm
}

#[test]
fn a_clr_on_one_path_takes_the_bound_away() {
    for src in [
        "10 DIM Q(300)\n20 Q(255)=5\n30 IF A=0 THEN CLR\n40 PRINT Q(255)\n",
        "10 DIM Q(300)\n20 Q(255)=5\n30 IF A=1 THEN CLR\n40 PRINT Q(255)\n",
        "10 DIM Q(300)\n20 Q(255)=5\n30 IF A=0 THEN CLR:PRINT \"C\"\n40 PRINT Q(5):PRINT Q(255)\n",
        "10 DIM Q(300)\n20 Q(255)=5:IF A=0 THEN 50\n30 PRINT \"X\"\n50 PRINT Q(255)\n60 CLR:A=1:GOTO 50\n",
        "10 DIM Q%(300),R$(300)\n20 Q%(255)=5:R$(200)=\"A\"\n30 IF A=0 THEN CLR\n40 PRINT Q%(255)\n",
        "10 DIM Q%(300),R$(300)\n20 Q%(255)=5:R$(200)=\"A\"\n30 IF A=0 THEN CLR\n40 PRINT R$(200)\n",
        "10 DIM Q(300)\n20 Q(255)=5\n30 IF A=0 THEN RUN 40\n40 PRINT Q(255)\n",
        "10 DIM M(20,30):M(15,25)=7\n20 IF Z=0 THEN CLR\n30 PRINT M(5,5):PRINT M(15,25)\n",
        "10 DIM A%(100):FOR I=0 TO 100:A%(I)=I:NEXT\n20 IF Z=0 THEN CLR\n30 FOR I=0 TO 100 STEP 25:PRINT A%(I);:NEXT\n",
        "10 DIM A%(100):FOR I=0 TO 100:A%(I)=I:NEXT\n20 IF Z=0 THEN CLR\n30 FOR I=0 TO 9:PRINT A%(I);:NEXT:PRINT A%(50)\n",
        "10 DIM A(5):A(5)=1\n20 IF Z=0 THEN CLR\n30 A(8)=2:PRINT A(8):PRINT A(5)\n",
    ] {
        check(src);
    }
}

#[test]
fn the_bound_is_picked_at_run_time_where_paths_meet() {
    let asm = check(
        "10 DIM A(50):A(40)=1\n\
         20 IF X=0 THEN X=1:CLR:X=1\n\
         30 IF X=1 THEN PRINT A(5):X=2:GOTO 20\n\
         40 FOR I=0 TO 50 STEP 10:PRINT A(I);:NEXT\n",
    );
    assert!(asm.contains("_ST_B0\n"), "{asm}");
    check(
        "10 DIM M%(20,30),S$(40):M%(15,25)=7:S$(30)=\"X\"\n\
         20 Z=1:IF Z=1 THEN 40\n\
         30 CLR\n\
         40 PRINT M%(15,25);S$(30)\n\
         50 IF Z=1 THEN Z=2:CLR:Z=2:GOTO 40\n",
    );
}

#[test]
fn a_reference_before_the_dim_creates_the_array() {
    for src in [
        "10 PRINT Q(255)\n20 DIM Q(300)\n",
        "10 A(5)=1\n20 IF Z=0 THEN DIM A(10)\n30 PRINT A(5)\n",
        "10 DIM Q(300)\n20 Q(255)=5\n30 IF A=0 THEN CLR\n35 PRINT Q(5)\n40 DIM Q(300):PRINT Q(255)\n",
        "10 DIM A(100)\n20 FOR I=0 TO 100:A(I)=I:NEXT\n30 IF T=0 THEN T=1:GOTO 10\n",
        "10 PRINT \"X\":IF Y=0 THEN Y=1:DIM A(100):Y=1\n20 A(50)=1:PRINT A(50):N=N+1:IF N<3 THEN 10\n",
        "10 DIM A(10)\n20 IF Z=0 THEN Z=1:CLR:Z=1\n30 A(5)=1:IF Z=1 THEN Z=2:GOTO 20\n40 PRINT \"DONE\"\n",
        "10 DIM A(10)\n20 IF Z=0 THEN Z=1:CLR:Z=1\n30 IF Z=1 THEN Z=2:GOTO 10\n40 PRINT \"DONE\"\n",
    ] {
        check(src);
    }
}

#[test]
fn a_function_body_sees_the_state_at_its_call() {
    check(
        "10 DEF FN F(X)=Q(X)+1\n\
         20 DIM Q(300):Q(200)=4\n\
         30 PRINT FN F(200)\n\
         40 IF R=0 THEN R=1:CLR:DEF FN F(X)=Q(X)+1:R=1\n\
         50 PRINT FN F(5):PRINT FN F(200)\n",
    );
}

#[test]
fn a_dim_reached_once_costs_nothing() {
    let asm = check(
        "10 GOSUB 500:DIM A(50)\n\
         20 FOR I=1 TO 50:A(I)=I*1.5:NEXT\n\
         30 GOSUB 500:PRINT A(I-1):K=K+1:IF K<3 THEN 30\n\
         40 END\n\
         500 PRINT \"S\":RETURN\n",
    );
    assert!(!asm.contains("_ST:"), "{asm}");
    // The subroutine that DIMs shares its tail and RETURN with one called before the DIM.
    let asm = check(
        "1 GOSUB 1000\n\
         2 FOR I=0 TO 50:A(I)=I*1.5:NEXT:GOSUB 1100\n\
         3 PRINT A(40):END\n\
         1000 PRINT \"T\":GOSUB 1100\n\
         1010 GOSUB 2000\n\
         1100 IF K=0 THEN K=1\n\
         1110 RETURN\n\
         2000 DIM A(50):RETURN\n",
    );
    assert!(!asm.contains("_ST:"), "{asm}");
}
