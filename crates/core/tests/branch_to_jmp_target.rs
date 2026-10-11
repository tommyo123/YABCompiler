mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

type Outcome = (Vec<u8>, Option<u8>, Option<u16>);

fn interpreted(src: &str) -> Outcome {
    let prg = tokenize_program(src).expect("tokenizes");
    let r = emu64::run_basic_to_end(&prg, 20_000_000).expect("runs in the interpreter");
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
    let r = emu64::run_prg_to_end(&c.prg_bytes, 20_000_000).expect("runs");
    (r.output, r.error_code, r.error_line)
}

#[test]
fn near_and_far_line_targets_both_ways() {
    let mut src = String::from(
        "10 READ V:IF V<0 THEN END\n20 IF V=1 THEN 40\n30 PRINT \"N\";\n40 IF V=2 THEN 900\n",
    );
    for line in 50..80 {
        src.push_str(&format!("{line} PRINT \"{line}\";V*{line};\n"));
    }
    src.push_str("80 GOTO 10\n900 PRINT \"F\";:GOTO 10\n990 DATA 0,1,2,3,-1\n");
    let want = interpreted(&src);
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let got = compiled(&src, profile);
        assert_eq!(
            (common::petscii_to_string(&got.0), got.1, got.2),
            (common::petscii_to_string(&want.0), want.1, want.2),
            "{profile:?}"
        );
    }
}
