//! Native integer arithmetic traps when exact results leave the signed 16-bit range.

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

/// Every pair from the edge values, added, subtracted and multiplied, one per run.
#[test]
fn sums_and_differences_at_the_edges() {
    let edges = [
        "32767", "-32768", "-32767", "-1", "0", "1", "16384", "-16384", "32766", "181", "182",
        "-128", "256",
    ];
    for a in edges {
        for b in edges {
            for op in ["+", "-", "*"] {
                same_as_the_rom(&format!(
                    "10 READ A%,B%\n20 PRINT \"S\";\n30 C%=A%{op}B%\n40 PRINT C%\n50 DATA {a},{b}\n"
                ));
            }
        }
    }
}

#[test]
fn the_target_is_also_an_operand() {
    same_as_the_rom(
        "10 READ A%,B%\n20 A%=A%+B%:PRINT A%\n30 A%=A%+B%:PRINT A%\n40 DATA 20000,10000\n",
    );
    same_as_the_rom(
        "10 READ A%,B%\n20 B%=A%-B%:PRINT B%\n30 B%=B%-A%:PRINT B%\n40 DATA -20000,20000\n",
    );
}

#[test]
fn a_loop_that_overflows_late() {
    same_as_the_rom(
        "10 READ K%\n20 FOR I=1 TO 100\n30 C%=C%+K%\n40 NEXT\n50 PRINT C%\n60 DATA 400\n",
    );
}
