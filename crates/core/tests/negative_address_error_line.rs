//! Negative POKE and PEEK addresses report the executing BASIC line.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn check(src: &str) {
    let prg = tokenize_program(src).expect("tokenizes");
    let rom = emu64::run_basic_to_end(&prg, 20_000_000).expect("runs in the interpreter");
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let c = compile_with_options(
            &prg,
            CompileOptions {
                profile,
                ..CompileOptions::default()
            },
        )
        .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}\n{src}"));
        let r = emu64::run_prg_to_end(&c.prg_bytes, 20_000_000).expect("runs");
        assert_eq!(
            (r.error_code, r.error_line),
            (rom.error_code, rom.error_line),
            "{profile:?}:\n{src}"
        );
        assert_eq!(
            common::petscii_to_string(&r.output),
            common::petscii_to_string(&rom.output),
            "{profile:?}:\n{src}"
        );
    }
}

#[test]
fn a_negative_integer_address_names_its_own_line() {
    check("10 P=1024:POKE P,1:P=-1\n20 POKE P,1\n");
    check("10 READ P\n20 POKE P,1\n30 DATA -1\n");
    check("10 P%=-1\n20 X=PEEK(P%)\n");
    check("10 READ P\n20 X=PEEK(P)\n30 DATA -1\n");
}

#[test]
fn a_non_negative_integer_address_still_runs() {
    check("10 READ P\n20 POKE P,1:PRINT PEEK(P)\n30 DATA 1024\n");
}
