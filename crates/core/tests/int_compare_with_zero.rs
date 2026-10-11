mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

type Outcome = (Vec<u8>, Option<u8>, Option<u16>);

const BUDGET: u64 = 40_000_000;

fn interpreted(src: &str) -> Outcome {
    let prg = tokenize_program(src).expect("tokenizes");
    let r = emu64::run_basic_to_end(&prg, BUDGET).expect("runs in the interpreter");
    assert!(r.ready_reached, "the interpreter reaches READY:\n{src}");
    (r.output, r.error_code, r.error_line)
}

fn build(src: &str, profile: Profile) -> yabcompiler_core::Compiled {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(
        &prg,
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}\n{src}"))
}

fn compiled(src: &str, profile: Profile) -> Outcome {
    let c = build(src, profile);
    let r = emu64::run_prg_to_end(&c.prg_bytes, BUDGET).expect("runs");
    assert!(r.clean_exit || r.error_code.is_some(), "ends ({profile:?}):\n{src}");
    (r.output, r.error_code, r.error_line)
}

const OPS: [&str; 6] = ["<", "<=", ">", ">=", "=", "<>"];

fn program(x: &str, cmp: &str, values: &[i32]) -> String {
    let mut src = String::from("10 READ K:IF K<0 THEN END\n20 GOSUB 500:PRINT K;\n");
    let mut line = 100;
    let mut letter = b'A';
    for op in OPS {
        for form in [format!("{cmp}{op}0"), format!("0{op}{cmp}")] {
            let l = letter as char;
            src.push_str(&format!("{line} IF {form} THEN PRINT \"{l}\";\n"));
            src.push_str(&format!("{} IF {form} THEN {}\n", line + 1, line + 3));
            src.push_str(&format!("{} PRINT \"{}\";\n", line + 2, l.to_ascii_lowercase()));
            src.push_str(&format!("{} REM\n", line + 3));
            line += 5;
            letter += 1;
        }
    }
    src.push_str(&format!("{line} PRINT:GOTO 10\n"));
    src.push_str(&format!("500 {x}=0\n"));
    for (k, v) in values.iter().enumerate() {
        src.push_str(&format!("{} IF K={k} THEN {x}={v}\n", 501 + k));
    }
    src.push_str("599 RETURN\n");
    let ks: Vec<String> = (0..values.len()).map(|k| k.to_string()).collect();
    src.push_str(&format!("900 DATA {},-1\n", ks.join(",")));
    src
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

const WORD: [i32; 14] = [
    -32768, -32767, -256, -255, -129, -128, -1, 0, 1, 127, 128, 255, 256, 32767,
];

#[test]
fn a_two_byte_integer() {
    same_as_the_rom(&program("E", "E", &WORD));
}

#[test]
fn a_percent_variable() {
    same_as_the_rom(&program("E%", "E%", &WORD));
}

#[test]
fn a_sum_in_a_temporary() {
    let src = program("E", "E+E", &[-16384, -16383, -128, -1, 0, 1, 127, 128, 16383]);
    same_as_the_rom(&src);
}

fn one_byte_and_unshifted(src: &str) -> String {
    let asm = build(src, Profile::Default).asm;
    assert!(asm.contains("VI_E") && !asm.contains("VI_E+1"), "E is one byte:\n{asm}");
    assert!(!asm.contains("EOR #$80") && !asm.contains("SBC #"), "{asm}");
    asm
}

#[test]
fn an_unsigned_byte() {
    let src = program("E", "E", &[0, 1, 127, 128, 254, 255]);
    same_as_the_rom(&src);
    one_byte_and_unshifted(&src);
}

#[test]
fn a_signed_byte_negative_by_bit_7() {
    let src = program("E", "E", &[-128, -127, -1, 0, 1, 126, 127]);
    same_as_the_rom(&src);
    let asm = one_byte_and_unshifted(&src);
    let main = &asm[..asm.find("; --- helper subroutines ---").unwrap_or(asm.len())];
    assert!(!main.contains("    ASL\n"), "the sign is the load's N flag:\n{asm}");
}

#[test]
fn a_signed_byte_negative_below_a_threshold() {
    let src = program("E", "E", &[-40, -39, -1, 0, 1, 127, 128, 139, 140]);
    same_as_the_rom(&src);
    let asm = one_byte_and_unshifted(&src);
    assert!(asm.contains("CMP #$D8"), "E's negative bytes start at $D8:\n{asm}");
}

#[test]
fn no_compare_with_zero_subtracts() {
    let asm = build(&program("E", "E", &WORD), Profile::Default).asm;
    assert!(!asm.contains("__IF_NV_"), "{asm}");
    assert!(!asm.contains("__FOR_DIFF_LO"), "{asm}");
}
