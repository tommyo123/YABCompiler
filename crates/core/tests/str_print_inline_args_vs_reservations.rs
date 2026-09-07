//! PH114's inline `__STR_PRINT` argument, and the reserved-range bridge that can cut it loose.

use yabcompiler_core::{CompileOptions, Compiled, compile_with_options, tokenize_program};

fn source() -> String {
    let mut s = String::new();
    for i in 0..24u32 {
        let c = (b'A' + (i as u8 % 26)) as char;
        s.push_str(&format!("{} PRINT \"{}{}\"\n", 10 + i * 10, c, i));
    }
    s.push_str("500 END\n");
    s
}

fn expected_text() -> String {
    let mut s = String::new();
    for i in 0..24u32 {
        let c = (b'A' + (i as u8 % 26)) as char;
        s.push_str(&format!("{c}{i}\n"));
    }
    s
}

/// Lowest address the sweeps reserve.
const SWEEP_LO: u16 = 0x0810;

fn compile_reserved(reserved: Vec<(u16, u16)>) -> Compiled {
    let prg = tokenize_program(&source()).expect("tokenizes");
    compile_with_options(
        &prg,
        CompileOptions {
            reserved_ranges: reserved,

            auto_reserve: false,
            ..CompileOptions::default()
        },
    )
    .expect("compiles")
}

/// Decode a CHROUT byte stream the way the corpus tests do.
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
    let r = emu64::run_prg_to_end(&compiled.prg_bytes, 20_000_000).expect("runs");
    assert!(r.clean_exit, "program must reach its final RTS");
    petscii(&r.output)
}

fn inline_sites(compiled: &Compiled) -> usize {
    compiled
        .asm
        .lines()
        .filter(|l| l.trim() == "JSR __STR_PRINT_I")
        .count()
}

#[test]
fn without_reservations_every_site_is_inline() {
    let c = compile_reserved(Vec::new());
    assert!(
        inline_sites(&c) >= 20,
        "the fixture must clear STR_PRINT_INLINE_MIN_SITES with room to \
         spare; got {}",
        inline_sites(&c)
    );
    assert!(!c.diagnostics.inline_str_args_undone);
    assert_eq!(run(&c), expected_text());
}

#[test]
fn a_reservation_the_image_never_reaches_keeps_every_site_inline() {
    let plain = compile_reserved(Vec::new());
    let far = compile_reserved(vec![(0xD000, 0xD02E)]);
    assert_eq!(inline_sites(&far), inline_sites(&plain));
    assert!(!far.diagnostics.inline_str_args_undone);
    assert_eq!(
        far.prg_bytes.len(),
        plain.prg_bytes.len(),
        "a reservation outside the image must not change a single byte"
    );
    assert_eq!(run(&far), expected_text());
}

#[test]
fn a_reservation_inside_the_image_keeps_the_sites_it_does_not_split() {
    let plain = compile_reserved(Vec::new());
    let end = 0x080Du16 + plain.machine_code.len() as u16;
    let want_sites = inline_sites(&plain);
    let mut kept = 0usize;
    for addr in (SWEEP_LO..end).step_by(7) {
        let c = compile_reserved(vec![(addr, addr)]);
        if c.diagnostics.inline_str_args_undone {
            continue;
        }
        assert_eq!(
            inline_sites(&c),
            want_sites,
            "reserving ${addr:04X} did not force a fallback, so every site \
             must still be inline"
        );
        kept += 1;
    }
    assert!(
        kept > 10,
        "the overwhelming majority of reservations must keep the \
         optimisation; only {kept} did"
    );
}

#[test]
fn the_inline_convention_is_smaller_than_the_register_convention() {
    let inline = compile_reserved(Vec::new());
    let undone = forced_fallback();
    assert!(
        undone.diagnostics.inline_str_args_undone,
        "fixture must actually take the fallback"
    );
    assert_eq!(inline_sites(&undone), 0, "the fallback removes every site");
    assert!(
        !undone.asm.contains("__STR_PRINT_I"),
        "the trampoline must go with the sites that used it"
    );

    assert!(
        undone.machine_code.len() > inline.machine_code.len(),
        "the register convention is the bigger one: {} vs {}",
        undone.machine_code.len(),
        inline.machine_code.len()
    );
}

fn forced_fallback() -> Compiled {
    let plain = compile_reserved(Vec::new());
    let end = 0x080Du16 + plain.machine_code.len() as u16;
    for addr in SWEEP_LO..end {
        let c = compile_reserved(vec![(addr, addr)]);
        if c.diagnostics.inline_str_args_undone {
            return c;
        }
    }
    panic!("no reservation in ${SWEEP_LO:04X}-${end:04X} split an inline argument");
}

#[test]
fn the_fallback_still_prints_the_right_text() {
    let c = forced_fallback();
    assert_eq!(
        run(&c),
        expected_text(),
        "backing PH114 out must not change what the program prints"
    );
}

#[test]
fn every_single_byte_reservation_inside_the_image_still_prints_the_right_text() {
    let plain = compile_reserved(Vec::new());
    let end = 0x080Du16 + plain.machine_code.len() as u16;
    let want = expected_text();
    let want_sites = inline_sites(&plain);
    let mut fell_back = 0usize;
    let mut kept = 0usize;
    for addr in SWEEP_LO..end {
        let c = compile_reserved(vec![(addr, addr)]);
        if c.diagnostics.inline_str_args_undone {
            assert_eq!(inline_sites(&c), 0);
            fell_back += 1;
        } else {
            assert_eq!(inline_sites(&c), want_sites);
            kept += 1;
        }
        assert_eq!(
            run(&c),
            want,
            "reserving ${addr:04X} changed the program's output"
        );
    }
    assert!(
        fell_back > 0,
        "the sweep is supposed to exercise the fallback; it never fired"
    );
    assert!(
        kept > fell_back * 3,
        "the fallback should be the rare case, not the common one: \
         {fell_back} fell back, {kept} kept the optimisation"
    );
}

#[test]
fn a_failure_the_fallback_cannot_fix_keeps_its_own_message() {
    let compile_it = || {
        let prg = tokenize_program(&source()).expect("tokenizes");
        compile_with_options(
            &prg,
            CompileOptions {
                // Swallows the whole code region, so neither convention can be placed.
                reserved_ranges: vec![(0x0810, 0xCFFF)],
                auto_reserve: false,
                ..CompileOptions::default()
            },
        )
        .err()
        .map(|e| e.to_string())
    };
    let msg = compile_it().expect("nothing fits with the address space reserved");
    assert!(
        !msg.contains("internal error"),
        "the fallback must not turn a layout refusal into an internal \
         error: {msg}"
    );
    assert!(
        msg.contains("Reduce data") || msg.to_lowercase().contains("reserved"),
        "expected one of the compiler's own layout diagnostics: {msg}"
    );
    assert_eq!(compile_it(), Some(msg), "and the same one every time");
}

#[test]
fn the_undo_is_a_no_op_when_the_optimisation_did_not_fire() {
    let prg = tokenize_program("10 PRINT \"HI\"\n").expect("tokenizes");
    let c = compile_with_options(&prg, CompileOptions::default()).expect("compiles");
    assert_eq!(inline_sites(&c), 0, "one site is below the break-even point");
    assert_eq!(
        yabcompiler_core::peephole::undo_str_print_inline_args(&c.asm),
        c.asm
    );
}

#[test]
fn the_undo_restores_the_register_convention() {
    let inline = compile_reserved(Vec::new());
    let undone = yabcompiler_core::peephole::undo_str_print_inline_args(&inline.asm);
    assert!(!undone.contains("__STR_PRINT_I"));
    assert!(!undone.contains(".word S"));
    // Every site came back as the three-instruction form.
    let triples = undone
        .lines()
        .filter(|l| l.trim() == "JSR __STR_PRINT")
        .count();
    assert!(
        triples >= inline_sites(&inline),
        "expected the {} sites back on `JSR __STR_PRINT`, got {triples}",
        inline_sites(&inline)
    );

    let mut a = asm6502::Assembler6502::new();
    let image = a.assemble_bytes(&undone).expect("re-assembles");
    assert!(
        image.len() > inline.machine_code.len(),
        "the register convention is the bigger one"
    );
}
