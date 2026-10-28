//! Reserved-range bridges preserve inline PRINT pointers and text.

use yabcompiler_core::{CompileOptions, Compiled, compile_with_options, tokenize_program};

/// The literal on line `i`; `shared` gives every literal two lines.
fn literal(i: u32, shared: bool) -> String {
    let k = if shared { i / 2 } else { i };
    let c = (b'A' + (k as u8 % 26)) as char;
    format!("{c}{k}")
}

fn source(shared: bool) -> String {
    let mut s = String::new();
    for i in 0..24u32 {
        s.push_str(&format!(
            "{} PRINT \"{}\"\n",
            10 + i * 10,
            literal(i, shared)
        ));
    }
    s.push_str("500 END\n");
    s
}

fn expected_text(shared: bool) -> String {
    let mut s = String::new();
    for i in 0..24u32 {
        s.push_str(&format!("{}\n", literal(i, shared)));
    }
    s
}

/// Both fixtures: the text form, then the pointer form.
const FORMS: [bool; 2] = [false, true];

/// Lowest address the sweeps reserve.
const SWEEP_LO: u16 = 0x0810;

fn compile_reserved(reserved: Vec<(u16, u16)>, shared: bool) -> Compiled {
    let prg = tokenize_program(&source(shared)).expect("tokenizes");
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
        .filter(|l| matches!(l.trim(), "JSR __STR_PRINT_I" | "JSR __STR_PRINT_T"))
        .count()
}

fn has_trampoline(asm: &str) -> bool {
    asm.contains("__STR_PRINT_I:") || asm.contains("__STR_PRINT_T:")
}

#[test]
fn without_reservations_every_site_is_inline() {
    for shared in FORMS {
        let c = compile_reserved(Vec::new(), shared);
        assert!(
            inline_sites(&c) >= 20,
            "the fixture must clear the break-even point with room to spare; got {}",
            inline_sites(&c)
        );
        let pointer = c.asm.contains("JSR __STR_PRINT_I");
        assert_eq!(pointer, shared, "a literal printed twice keeps its pointer");
        assert!(!c.diagnostics.inline_str_args_undone);
        assert_eq!(run(&c), expected_text(shared));
    }
}

#[test]
fn a_reservation_the_image_never_reaches_keeps_every_site_inline() {
    for shared in FORMS {
        let plain = compile_reserved(Vec::new(), shared);
        let far = compile_reserved(vec![(0xD000, 0xD02E)], shared);
        assert_eq!(inline_sites(&far), inline_sites(&plain));
        assert!(!far.diagnostics.inline_str_args_undone);
        assert_eq!(
            far.prg_bytes.len(),
            plain.prg_bytes.len(),
            "a reservation outside the image must not change a single byte"
        );
        assert_eq!(run(&far), expected_text(shared));
    }
}

#[test]
fn a_reservation_inside_the_image_keeps_the_sites_it_does_not_split() {
    for shared in FORMS {
        let plain = compile_reserved(Vec::new(), shared);
        let end = 0x080Du16 + plain.machine_code.len() as u16;
        let want_sites = inline_sites(&plain);
        let mut kept = 0usize;
        for addr in (SWEEP_LO..end).step_by(7) {
            let c = compile_reserved(vec![(addr, addr)], shared);
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
}

#[test]
fn the_inline_convention_is_smaller_than_the_register_convention() {
    for shared in FORMS {
        let inline = compile_reserved(Vec::new(), shared);
        let undone = forced_fallback(shared);
        assert!(
            undone.diagnostics.inline_str_args_undone,
            "fixture must actually take the fallback"
        );
        let left = inline_sites(&undone);
        assert!(
            left < inline_sites(&inline),
            "the split site goes back to the register convention"
        );
        assert_eq!(
            has_trampoline(&undone.asm),
            left > 0,
            "the trampoline stays exactly as long as a site still uses it"
        );

        assert!(
            undone.machine_code.len() > inline.machine_code.len(),
            "the register convention is the bigger one: {} vs {}",
            undone.machine_code.len(),
            inline.machine_code.len()
        );
    }
}

fn forced_fallback(shared: bool) -> Compiled {
    let plain = compile_reserved(Vec::new(), shared);
    let end = 0x080Du16 + plain.machine_code.len() as u16;
    for addr in SWEEP_LO..end {
        let c = compile_reserved(vec![(addr, addr)], shared);
        if c.diagnostics.inline_str_args_undone {
            return c;
        }
    }
    panic!("no reservation in ${SWEEP_LO:04X}-${end:04X} split an inline argument");
}

#[test]
fn a_split_backs_out_only_the_sites_it_separates() {
    for shared in FORMS {
        let want = inline_sites(&compile_reserved(Vec::new(), shared));
        let c = forced_fallback(shared);
        let left = inline_sites(&c);
        assert!(
            left + 2 >= want,
            "a one-byte reservation should cost at most a couple of sites: \
             {left} of {want} still inline"
        );
    }
}

#[test]
fn the_fallback_still_prints_the_right_text() {
    for shared in FORMS {
        let c = forced_fallback(shared);
        assert_eq!(
            run(&c),
            expected_text(shared),
            "backing the inline form out must not change what the program prints"
        );
    }
}

#[test]
fn every_single_byte_reservation_inside_the_image_still_prints_the_right_text() {
    for shared in FORMS {
        let plain = compile_reserved(Vec::new(), shared);
        let end = 0x080Du16 + plain.machine_code.len() as u16;
        let want = expected_text(shared);
        let want_sites = inline_sites(&plain);
        let mut fell_back = 0usize;
        let mut kept = 0usize;
        for addr in SWEEP_LO..end {
            let c = compile_reserved(vec![(addr, addr)], shared);
            if c.diagnostics.inline_str_args_undone {
                let left = inline_sites(&c);
                assert!(left < want_sites);
                assert!(
                    left + 2 >= want_sites,
                    "reserving ${addr:04X} backed out {} sites, not just the split one",
                    want_sites - left
                );
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
        // Text fills most of the text form's image, so a reserved byte lands in it often.
        let ratio = if shared { 3 } else { 1 };
        assert!(
            kept > fell_back * ratio,
            "the fallback should be the rare case, not the common one: \
             {fell_back} fell back, {kept} kept the optimisation"
        );
    }
}

#[test]
fn a_failure_the_fallback_cannot_fix_keeps_its_own_message() {
    let compile_it = || {
        let prg = tokenize_program(&source(false)).expect("tokenizes");
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
    for shared in FORMS {
        let inline = compile_reserved(Vec::new(), shared);
        let undone = yabcompiler_core::peephole::undo_str_print_inline_args(&inline.asm);
        assert!(!has_trampoline(&undone));
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
}
