mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

fn banner(n: i32, dim: &str) -> Vec<(Profile, Option<String>)> {
    let src = format!(
        "10 N={n}:GOSUB 100\n20 DIM {dim}\n30 PRINT \"DONE\"\n40 END\n\
         100 IF PEEK(2)=7 THEN N=5\n110 RETURN\n"
    );
    let prg = tokenize_program(&src).expect("tokenizes");
    PROFILES
        .iter()
        .map(|&profile| {
            let c = compile_with_options(
                &prg,
                CompileOptions {
                    profile,
                    ..CompileOptions::default()
                },
            )
            .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}"));
            let r = emu64::run_prg_to_end(&c.prg_bytes, common::MAX_INSNS).expect("runs");
            (profile, r.error_banner())
        })
        .collect()
}

fn expect(n: i32, dim: &str, want: &str) {
    for (profile, got) in banner(n, dim) {
        assert_eq!(got.as_deref(), Some(want), "({profile:?}) N={n}: DIM {dim}");
    }
}

#[test]
fn a_size_past_sixteen_bits_is_out_of_memory() {
    expect(20000, "A(N):A(1)=1.5", "?OUT OF MEMORY  ERROR IN 20");
    expect(300, "A(N,N):A(1,1)=1.5", "?OUT OF MEMORY  ERROR IN 20");
}

#[test]
fn a_subscript_intidx_rejects_is_illegal_quantity() {
    expect(32768, "A%(N)", "?ILLEGAL QUANTITY  ERROR IN 20");
    expect(40000, "B$(N)", "?ILLEGAL QUANTITY  ERROR IN 20");
    expect(-1, "A(N)", "?ILLEGAL QUANTITY  ERROR IN 20");
}

#[test]
fn the_largest_subscript_intidx_accepts_still_reaches_the_size_check() {
    expect(32767, "A%(N)", "?OUT OF MEMORY  ERROR IN 20");
}
