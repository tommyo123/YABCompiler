//! Cached float addresses preserve conversion and range checks.

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

fn check(src: &str, cached: bool) {
    let want = interpreted(src);
    let prg = tokenize_program(src).expect("tokenizes");
    for profile in [Profile::Speed, Profile::Default, Profile::Size] {
        for extraram in [false, true] {
            let c = compile_with_options(
                &prg,
                CompileOptions {
                    profile,
                    extraram,
                    force_extraram_off: !extraram,
                    ..CompileOptions::default()
                },
            )
            .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}\n{src}"));
            let r = emu64::run_prg_to_end(&c.prg_bytes, 200_000_000).expect("runs");
            let got = (
                common::petscii_to_string(&r.output),
                r.error_code,
                r.error_line,
            );
            assert_eq!(got, want, "{profile:?}, extraram={extraram}:\n{src}");
            if profile == Profile::Speed {
                assert_eq!(
                    c.asm.contains("JSR __AC_"),
                    cached,
                    "cache in speed:\n{src}"
                );
            }
        }
    }
}

#[test]
fn a_changing_address_is_converted_again() {
    check(
        "10 FOR K=1 TO 6:READ P:FOR T=1 TO 3:POKE P,T+K:S=S+PEEK(P):NEXT:NEXT:PRINT S\n\
         20 DATA 1024,1024,1030.7,1031,1031.2,2047\n",
        true,
    );
}

#[test]
fn an_address_plus_a_constant_matches_the_float_sum() {
    check(
        "10 FOR K=1 TO 5:READ P:FOR T=1 TO 2:POKE P+54272,T:S=S+PEEK(P+54272):NEXT:NEXT:PRINT S\n\
         20 DATA 1024,1024.5,1999.9,0,11263\n",
        true,
    );
    check(
        "10 P=11263:FOR T=1 TO 3:POKE P+54272,T:P=P+.5:NEXT\n",
        false,
    );
    check(
        "10 READ P:FOR T=1 TO 3:POKE P+54272,T:NEXT\n20 DATA 11264,.5\n",
        true,
    );
}

#[test]
fn the_zeroed_cache_stands_for_zero() {
    check(
        "10 FOR T=1 TO 2:PRINT PEEK(P+1024);:POKE P+1025,3:NEXT:PRINT PEEK(1025)\n",
        true,
    );
    check(
        "10 READ P:FOR T=1 TO 2:POKE P,5:NEXT:CLR:FOR T=1 TO 2:POKE P+1024,T:NEXT:PRINT PEEK(1024)\n\
         20 DATA 1024,.5\n",
        true,
    );
}

#[test]
fn bad_addresses_raise_the_rom_error_on_their_line() {
    check(
        "10 READ P:FOR T=1 TO 2:POKE P,1:NEXT:READ P\n20 FOR T=1 TO 2:POKE P,1:NEXT\n30 DATA 1024,-1,.5\n",
        true,
    );
    check(
        "10 READ P:FOR T=1 TO 2:X=PEEK(P):NEXT:READ P\n20 FOR T=1 TO 2:X=PEEK(P):NEXT\n30 DATA 1024,65536\n",
        true,
    );
    check(
        "10 READ P:FOR T=1 TO 2:POKE P,T:NEXT:PRINT PEEK(0)\n20 DATA -.5\n",
        true,
    );
}

#[test]
fn outside_loops_and_in_loops_that_move_the_address_nothing_is_cached() {
    check("10 P=1024:POKE P,1:PRINT PEEK(P)\n", false);
    check(
        "10 P=1024:FOR T=1 TO 3:POKE P,T:P=P+1:NEXT:PRINT PEEK(1026)\n",
        false,
    );
}
