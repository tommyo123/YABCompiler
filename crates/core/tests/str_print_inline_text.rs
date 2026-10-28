//! Single-use PRINT literals store their text directly behind the call.

mod common;

use yabcompiler_core::{CompileOptions, Compiled, Profile, compile_with_options, tokenize_program};

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

fn compile_at(src: &str, profile: Profile, extraram: bool) -> Compiled {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(
        &prg,
        CompileOptions {
            profile,
            extraram,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}\n{src}"))
}

fn outcome(c: &Compiled) -> Outcome {
    let r = emu64::run_prg_to_end(&c.prg_bytes, 200_000_000).expect("runs");
    (
        common::petscii_to_string(&r.output),
        r.error_code,
        r.error_line,
    )
}

/// Compare every profile with the ROM and return the default profile's build.
fn check(src: &str) -> Compiled {
    let want = interpreted(src);
    let mut default = None;
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        for extraram in [false, true] {
            let c = compile_at(src, profile, extraram);
            assert_eq!(outcome(&c), want, "{profile:?} extraram {extraram}:\n{src}");
            if profile == Profile::Default && !extraram {
                default = Some(c);
            }
        }
    }
    default.expect("default build")
}

fn text_sites(asm: &str) -> usize {
    asm.lines()
        .filter(|l| l.trim() == "JSR __STR_PRINT_T")
        .count()
}

fn pointer_sites(asm: &str) -> usize {
    common::expand_call_entries(asm)
        .lines()
        .filter(|l| l.trim() == "JSR __STR_PRINT_I")
        .count()
}

/// `n` lines from line `first` on, each printing its own literal.
fn prints_from(first: usize, n: usize) -> String {
    (0..n)
        .map(|i| format!("{} PRINT \"ROW {i}\"\n", first + i))
        .collect()
}

fn single_use_prints(n: usize) -> String {
    prints_from(10, n)
}

#[test]
fn a_literal_printed_once_moves_behind_its_call() {
    let c = check(&single_use_prints(30));
    assert_eq!(
        text_sites(&c.asm),
        29,
        "the last PRINT falls into __STR_PRINT"
    );
    assert!(
        !c.asm.contains("__STR_PRINT_I"),
        "no call is left on the pointer trampoline"
    );
    assert!(c.asm.contains("__STR_PRINT_T:"));
}

#[test]
fn a_literal_printed_twice_keeps_its_pointer() {
    let mut src = single_use_prints(30);
    for i in 0..10 {
        src.push_str(&format!("{} PRINT \"ROW {}\"\n", 500 + i, i * 3));
    }
    let c = check(&src);
    assert_eq!(
        pointer_sites(&c.asm),
        19,
        "ten shared literals, twenty calls, one falls through"
    );
    assert_eq!(text_sites(&c.asm), 20);
}

#[test]
fn the_text_form_prints_what_the_register_form_prints() {
    let c = compile_at(&single_use_prints(40), Profile::Default, false);
    let plain = yabcompiler_core::peephole::undo_str_print_inline_args(&c.asm);
    assert!(!plain.contains("__STR_PRINT_T"), "{plain}");
    let image = asm6502::Assembler6502::new()
        .assemble_bytes(&plain)
        .expect("the register form assembles");
    assert!(
        image.len() > c.machine_code.len(),
        "the text form is the smaller one"
    );
    let head = c.prg_bytes.len() - c.machine_code.len();
    let mut prg = c.prg_bytes[..head].to_vec();
    prg.extend_from_slice(&image);
    let a = emu64::run_prg_to_end(&c.prg_bytes, 80_000_000).expect("runs");
    let b = emu64::run_prg_to_end(&prg, 80_000_000).expect("runs");
    assert!(a.clean_exit && b.clean_exit);
    assert_eq!(a.output, b.output, "CHROUT streams must be identical");
}

#[test]
fn texts_of_every_length_return_to_the_next_statement() {
    // Lengths up to a full screen line, with code between the calls that has to run.
    let mut src = String::from("10 X=0\n");
    for i in 0..24usize {
        let text: String = (0..=i * 3)
            .map(|k| (b'A' + (k % 26) as u8) as char)
            .collect();
        src.push_str(&format!("{} PRINT \"{text}\";\n", 20 + i * 2));
        src.push_str(&format!("{} X=X+{}\n", 21 + i * 2, i + 1));
    }
    src.push_str("900 PRINT:PRINT X\n");
    let c = check(&src);
    assert!(text_sites(&c.asm) >= 20, "{}", c.asm);
}

#[test]
fn a_few_calls_stay_in_registers_outside_the_size_profile() {
    // Behind the call each print gets slower than the register form, so only size takes it.
    let src = single_use_prints(12);
    check(&src);
    assert_eq!(
        text_sites(&compile_at(&src, Profile::Default, false).asm),
        0
    );
    assert_eq!(text_sites(&compile_at(&src, Profile::Speed, false).asm), 0);
    let size = compile_at(&src, Profile::Size, false);
    if !size.asm.contains("__CP_BEGIN:") {
        assert_eq!(text_sites(&size.asm), 11, "{}", size.asm);
    }
}

#[test]
fn the_trampoline_follows_a_relocated_zero_page_pair() {
    let mut src = String::from("10 POKE 253,1\n");
    src.push_str(&prints_from(20, 25));
    src.push_str("900 PRINT PEEK(253)\n");
    let c = check(&src);
    assert!(text_sites(&c.asm) >= 17, "{}", c.asm);
    let lines: Vec<&str> = c.asm.lines().collect();
    let body = lines
        .iter()
        .position(|l| l.trim() == "__STR_PRINT:")
        .expect("helper");
    let staged = lines[body + 1]
        .trim()
        .strip_prefix("STA ")
        .unwrap()
        .to_string();
    assert_ne!(staged, "$FD", "the fixture must relocate the pair");
    let tramp = lines
        .iter()
        .position(|l| l.trim() == "__STR_PRINT_T:")
        .expect("trampoline");
    assert_eq!(lines[tramp + 2].trim(), format!("STA {staged}"));
}

#[test]
fn shared_and_heap_strings_still_work_around_it() {
    let mut src = String::from("10 DIM Q(200):A$=\"SEED\"\n");
    for i in 0..25 {
        src.push_str(&format!("{} PRINT \"H{i}\":A$=A$+\"{}\"\n", 20 + i, i % 10));
    }
    src.push_str("900 PRINT A$;LEN(A$)\n910 Q(199)=7:PRINT Q(199)\n");
    check(&src);
}
