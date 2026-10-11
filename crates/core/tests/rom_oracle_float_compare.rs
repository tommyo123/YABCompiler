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
        "the compiled program ends ({profile:?}):\n{src}"
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
        assert_eq!(got.0, want.0, "output bytes ({profile:?}):\n{src}");
    }
}

#[test]
fn a_power_on_the_right_counts_its_rounding_byte() {
    same_as_the_rom(
        "10 READ A:DATA 1.5\n20 IF A*A<>1.5^2 THEN PRINT \"NE\";\n\
         30 PRINT A*A<>1.5^2;A*A<1.5^2;A*A>=1.5^2\n",
    );
}

#[test]
fn an_if_then_next_on_a_float_self_compare() {
    same_as_the_rom(
        "10 READ B,I%:DATA 1.5,40\n\
         30 FOR L=1 TO 3:PRINT L;:IF B^2-I%=B^2-I% THEN NEXT L\n40 PRINT \"E\"\n",
    );
}

#[test]
fn commuted_sums_and_products_compare_as_in_the_rom() {
    same_as_the_rom(
        "10 FOR I=1 TO 40:READ A,B:IF A+B<>B+A THEN N=N+1\n\
         20 IF A*B>B*A THEN G=G+1\n30 NEXT:PRINT N;G\n\
         40 DATA 1.1,1.3,2.7,3.1,.3,.7,1.7,1.9,3.3,4.1,5.1,.9,7.7,.11,1.01,1.03,.37,.73,9.1,1.1\n\
         41 DATA 2.2,3.3,4.4,5.5,6.6,7.7,8.8,9.9,.12,.34,.56,.78,1.23,4.56,7.89,.12,3.14,2.72\n\
         42 DATA 1.41,1.73,.1,.2,.3,.4,.5,.6,.7,.8,.9,.15,.25,.35,.45,.55,.65,.75,.85,.95,1.05,1.15\n\
         43 DATA 11.1,13.3,17.7,19.9,23.3,29.9,31.1,37.7,41.1,43.3,47.7,53.3,59.9,61.1,67.7,71.1\n\
         44 DATA 73.3,79.9,83.3,89.9\n",
    );
}

#[test]
fn a_stored_sum_against_the_same_sum_computed_again() {
    same_as_the_rom(
        "10 FOR I=1 TO 40:READ A,B:X=A+B:Z=0\n12 IF X=A+B THEN E=E+1\n\
         15 IF X<A+B THEN L=L+1\n16 IF X>A+B THEN G=G+1\n20 NEXT:PRINT E;L;G\n\
         40 DATA 1.1,1.3,2.7,3.1,.3,.7,1.7,1.9,3.3,4.1,5.1,.9,7.7,.11,1.01,1.03,.37,.73,9.1,1.1\n\
         41 DATA 2.2,3.3,4.4,5.5,6.6,7.7,8.8,9.9,.12,.34,.56,.78,1.23,4.56,7.89,.12,3.14,2.72\n\
         42 DATA 1.41,1.73,.1,.2,.3,.4,.5,.6,.7,.8,.9,.15,.25,.35,.45,.55,.65,.75,.85,.95,1.05,1.15\n\
         43 DATA 11.1,13.3,17.7,19.9,23.3,29.9,31.1,37.7,41.1,43.3,47.7,53.3,59.9,61.1,67.7,71.1\n\
         44 DATA 73.3,79.9,83.3,89.9\n",
    );
}
