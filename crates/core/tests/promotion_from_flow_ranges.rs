//! Flow analysis bounds writes through conditions, masks, division, and READ.

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

fn compiled(src: &str, profile: Profile) -> Outcome {
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
    (r.output, r.error_code, r.error_line)
}

fn same_as_the_rom(src: &str) {
    let want = interpreted(src);
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let got = compiled(src, profile);
        assert_eq!(
            (common::petscii_to_string(&got.0), got.1, got.2),
            (common::petscii_to_string(&want.0), want.1, want.2),
            "{profile:?}:\n{src}"
        );
    }
}

#[test]
fn a_read_target_with_an_end_marker() {
    same_as_the_rom(
        "10 S=49177\n20 READ A:IF A=-1 THEN 50\n30 POKE S,A:S=S+1:GOTO 20\n50 PRINT S;PEEK(49180)\n60 DATA 32,253,174,32,-1\n",
    );
}

#[test]
fn a_read_target_from_fractional_data() {
    same_as_the_rom("10 FOR I=1 TO 3:READ A:B=B+A*2:NEXT:PRINT A;B\n20 DATA 1,2.5,3\n");
}

#[test]
fn a_get_target() {
    same_as_the_rom("10 FOR I=1 TO 5:GET A:B=B+A+1:NEXT:PRINT B\n");
}

#[test]
fn a_masked_sum() {
    same_as_the_rom("10 FOR I=1 TO 300:C=(C+I*7) AND 1023:POKE 1024+(C AND 255),1:NEXT:PRINT C\n");
}

#[test]
fn a_halved_value() {
    same_as_the_rom("10 FOR I=1 TO 300:H=INT(I/2):S=S+H:POKE 1024,H AND 255:NEXT:PRINT H;S\n");
    same_as_the_rom("10 FOR I=-300 TO 300 STEP 7:H=INT(I/4):S=S+H:NEXT:PRINT H;S\n");
}

#[test]
fn a_counter_with_a_guard() {
    same_as_the_rom("10 N=N+1:IF N<3000 THEN 10\n20 PRINT N\n");
}

#[test]
fn a_target_written_only_by_read() {
    same_as_the_rom(
        "10 FOR M=0 TO 2:S=0:FOR N=0 TO 4:READ A:POKE 1024+M*5+N,A:S=S+A:NEXT\n20 READ A:IF S<>A THEN PRINT \"BAD\";M\n30 NEXT:PRINT S\n40 DATA 1,2,3,4,5,15,10,20,30,40,50,150,7,7,7,7,7,36\n",
    );
}
