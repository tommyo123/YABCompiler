use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn asm_of(src: &str, profile: Profile) -> String {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(
        &prg,
        CompileOptions {
            profile,
            force_extraram_off: true,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}"))
    .asm
}

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];
const GET_PROG: &str = "10 PRINT \"PRESS\"\n20 GET R%:IF R%<>1 AND R%<>2 THEN 20\n30 PRINT \"GOT\";R%\n";

/// Lines from `__GET_ERR:` up to and including its `JMP`.
fn get_err_body(asm: &str) -> Vec<String> {
    let lines: Vec<&str> = asm.lines().map(str::trim).collect();
    let start = lines
        .iter()
        .position(|l| *l == "__GET_ERR:")
        .unwrap_or_else(|| panic!("no __GET_ERR arm:\n{asm}"));
    let end = lines[start..]
        .iter()
        .position(|l| l.starts_with("JMP "))
        .unwrap_or_else(|| panic!("__GET_ERR never jumps:\n{asm}"));
    lines[start..=start + end].iter().map(|s| s.to_string()).collect()
}

#[test]
fn a_bad_numeric_get_reports_without_a_line_number() {
    for profile in PROFILES {
        let body = get_err_body(&asm_of(GET_PROG, profile));
        let text = body.join("\n");
        assert!(
            body.iter().any(|l| l == "STA $39") && body.iter().any(|l| l == "STA $3A"),
            "{profile:?}: __GET_ERR does not stamp CURLIN as direct mode:\n{text}"
        );
        let load = body
            .iter()
            .position(|l| l == "LDA #$FF")
            .unwrap_or_else(|| panic!("{profile:?}: no $FF loaded:\n{text}"));
        let hi = body.iter().position(|l| l == "STA $3A").unwrap();
        let jmp = body.iter().position(|l| l.starts_with("JMP ")).unwrap();
        assert!(
            load < hi && hi < jmp,
            "{profile:?}: the stamp must precede the jump:\n{text}"
        );
        assert!(
            body.last().unwrap().contains("$AF08"),
            "{profile:?}: __GET_ERR no longer jumps to ERRSYN:\n{text}"
        );
    }
}

#[test]
fn the_successful_get_paths_do_not_touch_curlin() {
    for profile in PROFILES {
        let asm = asm_of(GET_PROG, profile);
        let lines: Vec<&str> = asm.lines().map(str::trim).collect();
        let start = lines.iter().position(|l| *l == "__GET_NUM:").unwrap();
        let err = lines.iter().position(|l| *l == "__GET_ERR:").unwrap();
        let before: Vec<&&str> = lines[start..err].iter().collect();
        assert!(
            !before.iter().any(|l| **l == "STA $39" || **l == "STA $3A"),
            "{profile:?}: a successful GET stamps CURLIN:\n{}",
            before.iter().map(|s| **s).collect::<Vec<_>>().join("\n")
        );
        // and the good paths are still there
        assert!(
            lines[start..err]
                .iter()
                .filter(|l| l.contains("$B3A2") || l.contains("__BYTEFAC"))
                .count()
                >= 2,
            "{profile:?}: the BYTEFAC conversions are gone:\n{asm}"
        );
    }
}

#[test]
fn other_runtime_traps_still_stamp_a_real_line() {
    for profile in PROFILES {
        let asm = asm_of("10 GET R%\n20 A=1/0\n", profile);
        let stamps: Vec<&str> = asm
            .lines()
            .map(str::trim)
            .filter(|l| l.starts_with("LDA #$") && l != &"LDA #$FF")
            .collect();
        assert!(
            !stamps.is_empty(),
            "{profile:?}: no non-$FF immediates at all, the program is empty:\n{asm}"
        );

        let ff_hi = asm
            .lines()
            .map(str::trim)
            .collect::<Vec<_>>()
            .windows(2)
            .filter(|w| w[0] == "LDA #$FF" && w[1] == "STA $39")
            .count();
        assert_eq!(
            ff_hi, 1,
            "{profile:?}: direct-mode stamping appears {ff_hi} times, it belongs only in __GET_ERR"
        );
    }
}
