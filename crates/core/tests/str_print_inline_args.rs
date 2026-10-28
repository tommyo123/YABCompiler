//! Inline PRINT pointers for literals shared by multiple call sites.

mod common;

use yabcompiler_core::{CompileOptions, Compiled, Profile, compile_with_options, tokenize_program};

fn register_form(inline: &Compiled) -> Vec<u8> {
    let asm = yabcompiler_core::peephole::undo_str_print_inline_args(&inline.asm);
    assert!(!asm.contains("__STR_PRINT_I"), "every site must be back");
    asm6502::Assembler6502::new()
        .assemble_bytes(&asm)
        .expect("the register form assembles")
}

fn as_prg(inline: &Compiled, image: &[u8]) -> Vec<u8> {
    let head = inline.prg_bytes.len() - inline.machine_code.len();
    assert_eq!(
        head, 14,
        "expected a 2-byte load address plus the 12-byte SYS stub"
    );
    let mut prg = inline.prg_bytes[..head].to_vec();
    prg.extend_from_slice(image);
    prg
}

fn compile(src: &str) -> Compiled {
    compile_at(src, CompileOptions::default())
}

fn compile_at(src: &str, options: CompileOptions) -> Compiled {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(&prg, options).expect("compiles")
}

fn petscii(bytes: &[u8]) -> String {
    let mut s = String::new();
    for &b in bytes {
        match b {
            0x0d => s.push('\n'),
            0x1d => s.push(' '),
            0x20..=0x5a => s.push(b as char),
            0xc1..=0xda => s.push((b - 0x80) as char),
            _ => {}
        }
    }
    s
}

fn run(compiled: &Compiled) -> String {
    let r = emu64::run_prg_to_end(&compiled.prg_bytes, 80_000_000).expect("runs");
    assert!(r.clean_exit, "program must reach its final RTS");
    petscii(&r.output)
}

/// `n` PRINTs, two to a literal, so each literal keeps its pool record and its pointer.
fn many_prints(n: usize) -> (String, String) {
    let mut src = String::new();
    let mut want = String::new();
    for i in 0..n {
        src.push_str(&format!("{} PRINT \"LINE{}\"\n", 10 + i, i / 2));
        want.push_str(&format!("LINE{}\n", i / 2));
    }
    (src, want)
}

fn convertible(n: usize) -> usize {
    n - 1
}

fn inline_sites(asm: &str) -> usize {
    common::expand_call_entries(asm)
        .lines()
        .filter(|l| l.trim() == "JSR __STR_PRINT_I")
        .count()
}

#[test]
fn a_print_heavy_program_uses_the_inline_convention() {
    let (src, want) = many_prints(30);
    let c = compile(&src);
    assert_eq!(
        inline_sites(&c.asm),
        convertible(30),
        "every callable pooled-literal PRINT should have converted"
    );
    assert_eq!(c.asm.matches("__STR_PRINT_I:").count(), 1);
    assert_eq!(run(&c), want);
}

#[test]
fn the_inline_form_is_smaller_than_the_register_form() {

    let (src, _) = many_prints(40);
    let inline = compile(&src);
    let register = register_form(&inline);
    assert!(
        inline.machine_code.len() < register.len(),
        "inline {} bytes vs register {} bytes",
        inline.machine_code.len(),
        register.len()
    );
    assert_eq!(
        register.len() - inline.machine_code.len(),
        convertible(40) * 2 - 34,
        "2 bytes a site against a 34-byte trampoline"
    );
}

#[test]
fn the_two_conventions_print_the_same_bytes() {
    // A CHROUT-stream diff, which is the only thing that can catch an off-by-one in the
    // trampoline's return-address arithmetic.
    let (src, want) = many_prints(30);
    let inline = compile(&src);
    assert!(inline_sites(&inline.asm) > 0);
    let register = as_prg(&inline, &register_form(&inline));
    let a = emu64::run_prg_to_end(&inline.prg_bytes, 80_000_000).expect("runs");
    let b = emu64::run_prg_to_end(&register, 80_000_000).expect("runs");
    assert!(a.clean_exit && b.clean_exit, "both must reach their RTS");
    assert_eq!(a.output, b.output, "CHROUT streams must be identical");
    assert_eq!(petscii(&a.output), want);
}

#[test]
fn the_call_returns_past_its_own_argument() {

    let mut src = String::from("10 X=0\n");
    let mut want = String::new();
    for i in 0..25 {
        src.push_str(&format!("{} PRINT \"T{}\"\n", 20 + i * 2, i / 2));
        src.push_str(&format!("{} X=X+{}\n", 21 + i * 2, i));
        want.push_str(&format!("T{}\n", i / 2));
    }
    src.push_str("900 PRINT X\n");
    want.push_str(" 300 \n");
    let c = compile(&src);
    assert!(inline_sites(&c.asm) >= 17);
    assert_eq!(run(&c), want);
}

#[test]
fn every_argument_is_the_line_after_its_call() {

    let (src, _) = many_prints(30);
    let c = compile(&src);
    let lines: Vec<&str> = c.asm.lines().collect();
    let mut seen = 0;
    for (i, line) in lines.iter().enumerate() {
        if line.trim() != "JSR __STR_PRINT_I" {
            continue;
        }
        seen += 1;
        assert!(
            lines[i + 1].trim().starts_with(".word "),
            "line {i} call is followed by `{}`",
            lines[i + 1]
        );
    }
    assert_eq!(seen, convertible(30));
}

#[test]
fn hello_must_not_grow() {
    // One site saves 2 bytes and the trampoline costs 34.
    let c = compile("10 PRINT \"HELLO\"\n");
    assert_eq!(inline_sites(&c.asm), 0);
    assert!(!c.asm.contains("__STR_PRINT_I"));

    assert_eq!(c.prg_bytes.len(), 48);
    assert_eq!(run(&c), "HELLO\n");
}

#[test]
fn sixteen_sites_decline_and_seventeen_accept() {

    let (below, _) = many_prints(17); // 16 convertible
    let (at, _) = many_prints(18); // 17 convertible
    assert_eq!(inline_sites(&compile(&below).asm), 0);
    assert_eq!(inline_sites(&compile(&at).asm), 17);
}

#[test]
fn a_program_that_declines_is_byte_identical_to_one_compiled_with_it_off() {
    let (src, _) = many_prints(10);
    let on = compile(&src);
    let mut off = CompileOptions::default();
    off.reserved_ranges = vec![(0xCF00, 0xCFFF)];
    let with_reservation = compile_at(&src, off);

    assert_eq!(on.prg_bytes, with_reservation.prg_bytes);
}

#[test]
fn a_reservation_that_misses_every_site_keeps_the_convention_on() {

    let (src, want) = many_prints(30);
    for range in [(0x1000u16, 0x10FF), (0x3000, 0x37FF), (0xC000, 0xC000)] {
        let mut options = CompileOptions::default();
        options.reserved_ranges = vec![range];
        let c = compile_at(&src, options);
        assert_eq!(
            inline_sites(&c.asm),
            convertible(30),
            "reservation ${:04X}-${:04X} does not touch a site, so every \
             site must stay inline",
            range.0,
            range.1
        );
        assert!(!c.diagnostics.inline_str_args_undone);
        assert_eq!(run(&c), want);
    }
}

#[test]
fn a_poke_derived_auto_reservation_no_longer_turns_it_off() {
    // `discover_reserved_ranges` reads POKE/PEEK literals out of the IR, so a program can acquire a reservation without the user asking for one.
    let (mut src, mut want) = many_prints(30);
    src.push_str("900 POKE 12288,7\n910 PRINT PEEK(12288)\n");
    want.push_str(" 7 \n");
    let c = compile(&src);
    assert!(
        !c.diagnostics.effective_reserved.is_empty(),
        "fixture must acquire an auto reservation"
    );
    assert!(inline_sites(&c.asm) >= 17, "the convention must still fire");
    assert!(!c.diagnostics.inline_str_args_undone);
    assert_eq!(run(&c), want);
}

/// The instruction the program falls into `__STR_PRINT:` from, if any.
fn falls_into_str_print(asm: &str) -> Option<String> {
    let lines: Vec<&str> = asm.lines().collect();
    let at = lines.iter().position(|l| l.trim() == "__STR_PRINT:")?;
    let prev = lines[..at]
        .iter()
        .rev()
        .map(|l| l.trim())
        .find(|l| !l.is_empty() && !l.starts_with(';') && !l.ends_with(':'))?;
    let mnem = prev.split_whitespace().next().unwrap_or("");
    (!matches!(mnem, "RTS" | "JMP" | "RTI" | "*=")).then(|| prev.to_string())
}

#[test]
fn falling_through_into_str_print_is_a_real_shape() {

    let c = compile("10 PRINT \"X\"\n");
    assert_eq!(
        falls_into_str_print(&c.asm).as_deref(),
        Some("LDY #>S0"),
        "the fall-through shape is what makes the trampoline's placement load-bearing"
    );
    assert_eq!(run(&c), "X\n");
}

#[test]
fn nothing_is_ever_spliced_above_str_print() {
    // Because the fall-through above is real, `__STR_PRINT_I:` must never be the item
    // immediately preceding `__STR_PRINT:`.
    let long = many_prints(30).0;
    let mut with_both = String::from("10 A$=\"Z\"\n");
    for i in 0..25 {
        with_both.push_str(&format!("{} PRINT \"F{}\"\n", 20 + i, i / 2));
    }
    with_both.push_str("900 PRINT A$\n");
    for src in [
        "10 PRINT \"X\"\n",
        "10 PRINT CHR$(65)\n20 PRINT \"Y\"\n",
        "10 A$=\"AB\":IF A$<\"B\" THEN PRINT \"LT\"\n",
        long.as_str(),
        with_both.as_str(),
    ] {
        let c = compile(src);
        let lines: Vec<&str> = c.asm.lines().collect();
        let Some(at) = lines.iter().position(|l| l.trim() == "__STR_PRINT:") else {
            continue;
        };
        let above = lines[..at]
            .iter()
            .rev()
            .map(|l| l.trim())
            .find(|l| !l.is_empty() && !l.starts_with(';'));
        assert_ne!(
            above,
            Some("__STR_PRINT_I:"),
            "the trampoline is immediately above __STR_PRINT and would be \
             fallen into.\n  source: {src}"
        );

        if let Some(t) = lines.iter().position(|l| l.trim() == "__STR_PRINT_I:") {
            let before = lines[..t]
                .iter()
                .rev()
                .map(|l| l.trim())
                .find(|l| !l.is_empty() && !l.starts_with(';') && !l.ends_with(':'))
                .unwrap_or("*=");
            let mnem = before.split_whitespace().next().unwrap_or("");
            assert!(

                matches!(mnem, "RTS" | "JMP" | "RTI" | "*=") || mnem.starts_with('.'),
                "`{before}` falls through into the trampoline.\n  source: {src}"
            );
        }
    }
}

#[test]
fn a_fall_through_print_and_inline_sites_coexist() {

    let mut src = String::from("10 A$=\"TAIL\"\n");
    let mut want = String::new();
    for i in 0..25 {
        src.push_str(&format!("{} PRINT \"F{}\"\n", 20 + i, i / 2));
        want.push_str(&format!("F{}\n", i / 2));
    }
    src.push_str("900 PRINT A$\n");
    want.push_str("TAIL\n");
    let c = compile(&src);
    assert!(inline_sites(&c.asm) >= 17, "PH114 must have fired");
    assert_eq!(run(&c), want);
}

#[test]
fn the_chr_corpus_program_still_matches_its_golden() {

    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test_corpus/chr.prg");
    let bytes = std::fs::read(&path).expect("test_corpus/chr.prg");
    let c = compile_with_options(&bytes, CompileOptions::default()).expect("compiles");
    let golden_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../test_corpus/emu_golden/chr.out");
    let golden = std::fs::read_to_string(&golden_path).expect("golden");
    assert_eq!(run(&c), golden);
}

#[test]
fn the_trampoline_is_never_inside_the_heap_or_the_bss() {

    let mut src = String::from("10 DIM Q(200)\n20 A$=\"SEED\"\n");
    let mut want = String::new();
    for i in 0..30 {
        src.push_str(&format!("{} PRINT \"H{}\"\n", 30 + i, i / 2));
        want.push_str(&format!("H{}\n", i / 2));
    }
    src.push_str("900 A$=A$+\"X\"\n910 PRINT A$\n");
    want.push_str("SEEDX\n");
    let c = compile(&src);
    assert!(inline_sites(&c.asm) >= 17);
    let tramp = c.asm.find("__STR_PRINT_I:").expect("trampoline emitted");
    for anchor in ["__BSS_START:", "__HEAP_BOTTOM:"] {
        if let Some(at) = c.asm.find(anchor) {
            assert!(tramp < at, "the trampoline landed at or after {anchor}");
        }
    }
    assert_eq!(run(&c), want);
}

#[test]
fn a_heap_program_with_both_optimizations_still_runs() {
    // BSS truncation and the inline convention share the insertion point, so this is the shape
    // where they can break each other.
    let mut src = String::from("10 DIM Q(300)\n20 B$=\"\"\n");
    let mut want = String::new();
    for i in 0..25 {
        src.push_str(&format!("{} PRINT \"M{}\"\n", 30 + i * 2, i / 2));
        src.push_str(&format!("{} B$=B$+\"{}\"\n", 31 + i * 2, i % 10));
        want.push_str(&format!("M{}\n", i / 2));
    }
    src.push_str("900 Q(299)=42\n910 PRINT LEN(B$);Q(299)\n");
    want.push_str(" 25  42 \n");
    let c = compile(&src);
    assert!(inline_sites(&c.asm) >= 17, "inline convention must fire");
    assert!(
        c.diagnostics.bss_truncated > 0,
        "BSS truncation must fire too"
    );
    assert_eq!(run(&c), want);
}

#[test]
fn the_trampoline_follows_a_relocated_zero_page_pair() {

    let mut src = String::from("10 POKE 253,1\n");
    let mut want = String::new();
    for i in 0..25 {
        src.push_str(&format!("{} PRINT \"Z{}\"\n", 20 + i, i / 2));
        want.push_str(&format!("Z{}\n", i / 2));
    }
    src.push_str("900 PRINT PEEK(253)\n");
    want.push_str(" 1 \n");
    let c = compile(&src);
    assert!(
        inline_sites(&c.asm) >= 17,
        "PH114 must fire on this fixture"
    );

    let lines: Vec<&str> = c.asm.lines().collect();
    let body = lines
        .iter()
        .position(|l| l.trim() == "__STR_PRINT:")
        .expect("helper emitted");
    let staged = lines[body + 1]
        .trim()
        .strip_prefix("STA ")
        .unwrap()
        .to_string();
    assert_ne!(
        staged, "$FD",
        "fixture must actually relocate the pair, else it proves nothing"
    );
    let tramp = lines
        .iter()
        .position(|l| l.trim() == "__STR_PRINT_I:")
        .expect("trampoline emitted");
    assert_eq!(
        lines[tramp + 2].trim(),
        format!("STA {staged}"),
        "the trampoline must stage the pointer where __STR_PRINT reads it"
    );
    assert_eq!(run(&c), want);
}

#[test]
fn every_profile_agrees() {
    let (src, want) = many_prints(30);
    for profile in [Profile::Default, Profile::Size, Profile::Speed] {
        let mut options = CompileOptions::default();
        options.profile = profile;
        let c = compile_at(&src, options);
        assert!(inline_sites(&c.asm) >= 17, "{profile:?}");
        assert_eq!(run(&c), want, "{profile:?}");
    }
}

#[test]
fn two_compiles_produce_the_same_file() {
    let (src, _) = many_prints(30);
    assert_eq!(compile(&src).prg_bytes, compile(&src).prg_bytes);
}
