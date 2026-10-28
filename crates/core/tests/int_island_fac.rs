//! Bounded integer expressions reach FAC through one conversion.

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
fn sums_of_known_integers_skip_float_arithmetic() {
    let asm = check(
        "10 READ A%,B%:X=A%+B%:Y=A%*40+B%:Z=(A%-B%)*2:W=-(A%+3)\n\
         20 PRINT X;Y;Z;W:X=X/7:Y=Y/7:Z=Z/7:W=W/7:PRINT X;Y;Z;W\n\
         30 DATA 200,13\n",
    );
    // Line 20 starts where CURLIN becomes 20.
    let line = &asm[..asm.find("    LDA #$14\n    STA $39\n").expect("line 20")];
    assert!(!line.contains("FADD") && !line.contains("FMULT"), "{line}");
}

#[test]
fn loop_counters_and_peeks_match_the_interpreter() {
    check(
        "10 FOR I=0 TO 5:FOR J=0 TO 3:X=I*40+J-100:PRINT X;:NEXT:NEXT:PRINT\n\
         20 FOR K=828 TO 831:POKE K,K-700:NEXT\n\
         30 X=PEEK(828)*4+PEEK(829)-PEEK(830)*3:PRINT X;X/3\n\
         40 FOR I=-20 TO 20 STEP 5:X=I*3-I*8:Y=-(I+I):PRINT X;Y;1/(X+1E-3):NEXT\n",
    );
}

#[test]
fn zero_results_and_signs_match_the_interpreter() {
    check(
        "10 FOR I=-3 TO 3:X=I-I:Y=-(I*2-I*2):Z=I*5-15:PRINT X;Y;Z;SGN(Y);SGN(Z):NEXT\n\
         20 FOR I=0 TO 3:IF I*2-3>0 THEN PRINT \"P\";I\n\
         30 IF -(I-2)<0 THEN PRINT \"N\";I\n\
         40 NEXT\n",
    );
}

#[test]
fn values_outside_the_proved_range_keep_float_arithmetic() {
    check(
        "10 READ A%,B%:X=A%+B%:Y=A%*3-B%:Z=-(A%-B%):PRINT X;Y;Z\n\
         20 DATA 32767,32767\n",
    );
    check(
        "10 FOR I=32000 TO 32767 STEP 767:FOR J=0 TO 1000 STEP 1000:X=I+J:Y=I*4:PRINT X;Y:NEXT:NEXT\n",
    );
}

#[test]
fn unsigned_results_and_byte_pairs_match_the_interpreter() {
    let asm = check(
        "10 FOR K=828 TO 835:POKE K,(K*37)AND255:NEXT\n\
         20 FOR X=0 TO 3:CG=57344+X*8:D=PEEK(828)+PEEK(829)*256:E=256*PEEK(831)+PEEK(830)\n\
         30 F=PEEK(832)*256+PEEK(833)-PEEK(834):PRINT CG;D;E;F;CG/3;D/3:NEXT\n\
         40 FOR X=0 TO 255 STEP 51:G=X*256+X:H=65535-X:PRINT G;H;G/7:NEXT\n",
    );
    assert!(asm.contains("JSR __GIVAYF_U16"), "{asm}");
}

#[test]
fn asc_and_len_terms_match_the_interpreter() {
    check(
        "10 READ A$,B$,X1:A=ASC(A$)-48:B=ASC(MID$(B$,X1,1))-65:C=LEN(B$)*2+1\n\
         20 D=1000-LEN(A$+B$):E=ASC(B$)+X1*0:PRINT A;B;C;D;E;A/3;B/3\n\
         30 FOR I=1 TO LEN(B$):PRINT ASC(MID$(B$,I,1))-64;:NEXT:PRINT\n\
         40 READ A$:PRINT LEN(A$)-1:PRINT ASC(A$)-48\n\
         50 DATA 7,HELLO,2,\"\"\n",
    );
}
