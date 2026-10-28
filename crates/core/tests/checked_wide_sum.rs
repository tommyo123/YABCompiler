//! Checked 24-bit loop sums and float fallbacks for unsupported terms.

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

fn check(src: &str) -> String {
    let want = interpreted(src);
    let mut speed = String::new();
    for profile in [Profile::Speed, Profile::Default, Profile::Size] {
        for extraram in [false, true] {
            let (got, asm) = compiled(src, profile, extraram);
            assert_eq!(got, want, "{profile:?} extraram {extraram}:\n{src}");
            if profile == Profile::Speed && !extraram {
                speed = asm;
            }
        }
    }
    speed
}

/// `items` after a first DATA item of 1.5, which keeps READ targets from being proved whole.
fn data(items: &[String]) -> String {
    format!("90 DATA 1.5,{}\n", items.join(","))
}

#[test]
fn a_loader_checksum_runs_in_24_bits() {
    let items: Vec<String> = (0..192).map(|k| ((k * 37) % 256).to_string()).collect();
    let asm = check(&format!(
        "5 READ F\n10 FOR J=828 TO 1019:READ A:POKE J,A:X=X+A:NEXT J:PRINT X;F\n\
         20 FOR J=828 TO 1019:S=S+PEEK(J):NEXT:PRINT S\n{}",
        data(&items)
    ));
    assert!(asm.contains("STA FE_0+3"), "{asm}");
}

#[test]
fn terms_that_are_not_whole_bytes_hand_back_to_float() {
    for bad in [
        "2.5", "-3", "256", "300", ".5", "1E-3", "255.99", "65535", "-.5",
    ] {
        let mut items: Vec<String> = (0..20).map(|k| (k * 13 % 256).to_string()).collect();
        items[7] = bad.to_string();
        check(&format!(
            "5 READ F\n10 FOR J=1 TO 20:READ A:X=X+A:Y=Y-A:NEXT:PRINT X;Y;X/7;Y/3\n{}",
            data(&items)
        ));
    }
}

#[test]
fn starts_that_are_not_small_whole_numbers_keep_float() {
    for start in [
        "0", "7", "-7", ".5", "-1.25", "4194303", "4194304", "-4194303", "1E9", "8388000",
    ] {
        let mut items: Vec<String> = (0..10).map(|k| (k * 29 % 256).to_string()).collect();
        items.insert(0, start.to_string());
        let asm = check(&format!(
            "5 READ F,X\n10 FOR J=1 TO 10:READ A:X=X+A:NEXT:PRINT X;X/3\n\
             20 FOR J=10 TO 1 STEP -1:RESTORE:READ F,A,B:X=X-A+B:NEXT:PRINT X\n{}",
            data(&items)
        ));
        assert!(asm.contains("JSR __WS_LOAD"), "{asm}");
    }
}

#[test]
fn sums_across_several_loops_and_two_terms_match_the_interpreter() {
    let items: Vec<String> = (0..80).map(|k| (k * 101 % 256).to_string()).collect();
    check(&format!(
        "5 READ F:CK=0\n10 FOR W=1 TO 20 STEP 2:READ Q,R:CK=CK+Q+R:NEXT:PRINT CK\n\
         20 FOR W=1 TO 30:READ Q:CK=CK+Q:NEXT:PRINT CK\n\
         30 FOR W=0 TO 1 STEP .1:READ Q:CK=CK-Q:NEXT:PRINT CK;W\n{}",
        data(&items)
    ));
}

#[test]
fn a_start_that_fails_the_check_reads_the_term_again() {
    // The term was just stored to an array, so FAC holds it when the loop takes the float path.
    let items: Vec<String> = (0..14).map(|k| (k * 41 % 256).to_string()).collect();
    for start in ["0", ".5", "-2.25", "5E6"] {
        let mut data_items = items.clone();
        data_items.insert(0, start.to_string());
        check(&format!(
            "1 DIM M(20)\n5 READ F,X\n10 FOR I=0 TO 12:READ A:M(I)=A:X=A+X:NEXT:PRINT X;X/7;I\n{}",
            data(&data_items)
        ));
    }
}
