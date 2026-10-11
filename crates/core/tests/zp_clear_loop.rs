mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

type Outcome = (Vec<u8>, Option<u8>, Option<u16>);

const SRC: &str = "5 POKE 830,0\n\
    10 PRINT A%;B%;C%;D%;E%;F%;G%\n\
    20 FOR I=1 TO 3:A%=A%+I:B%=B%+A%:C%=C%+B%:D%=D%+C%:E%=E%+D%:F%=F%+E%:G%=G%+F%:NEXT\n\
    30 PRINT A%;B%;C%;D%;E%;F%;G%\n\
    40 POKE 830,PEEK(830)+1:IF PEEK(830)<2 THEN CLR:GOTO 10\n";

fn build(profile: Profile) -> yabcompiler_core::Compiled {
    let prg = tokenize_program(SRC).expect("tokenizes");
    compile_with_options(
        &prg,
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}"))
}

fn run(prg: &[u8]) -> Outcome {
    let r = emu64::run_prg_to_end(prg, 20_000_000).expect("runs");
    (r.output, r.error_code, r.error_line)
}

fn clear_loops(asm: &str) -> Vec<(u8, u8)> {
    let lines: Vec<&str> = asm.lines().map(str::trim).collect();
    let byte = |l: &str, op: &str| {
        l.strip_prefix(op)
            .and_then(|v| u8::from_str_radix(v, 16).ok())
    };
    let mut out = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        if l.starts_with("__ZP_CLEAR_") && l.ends_with(':') {
            let last = byte(lines[i - 1], "LDX #$").expect("LDX #last");
            let first = byte(lines[i + 3], "CPX #$").expect("CPX #first-1") + 1;
            out.push((first, last));
        }
    }
    out
}

#[test]
fn promotions_read_zero_at_the_start_and_after_clr() {
    let prg = tokenize_program(SRC).expect("tokenizes");
    let r = emu64::run_basic_to_end(&prg, 20_000_000).expect("runs in the interpreter");
    let want = (r.output, r.error_code, r.error_line);
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let got = run(&build(profile).prg_bytes);
        assert_eq!(
            (common::petscii_to_string(&got.0), got.1, got.2),
            (common::petscii_to_string(&want.0), want.1, want.2),
            "{profile:?}"
        );
    }
}

#[test]
fn a_run_of_owned_bytes_is_cleared_by_a_loop_that_stays_off_the_clock() {
    for profile in [Profile::Default, Profile::Size] {
        let asm = build(profile).asm;
        let loops = clear_loops(&asm);
        assert!(!loops.is_empty(), "{profile:?}:\n{asm}");
        for (first, last) in loops {
            assert!(last - first + 1 >= 6, "{profile:?}: ${first:02X}-${last:02X}");
            assert!(first > 0xA2 || last < 0xA0, "{profile:?}: ${first:02X}-${last:02X}");
        }
    }
    assert!(clear_loops(&build(Profile::Speed).asm).is_empty());
}
