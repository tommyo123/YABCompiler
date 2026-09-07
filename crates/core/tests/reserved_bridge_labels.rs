use yabcompiler_core::{CompileOptions, Compiled, compile_with_options, tokenize_program};

/// Eight distinct PRINTs: enough string-pool entries that a reserved byte can be dropped
/// anywhere inside the pool.
const SRC: &str = "\
10 PRINT \"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\"
20 PRINT \"BBBBBBBBBBBBBBBBBBBBBBBBBBBBBB\"
30 PRINT \"CCCCCCCCCCCCCCCCCCCCCCCCCCCCCC\"
40 PRINT \"DDDDDDDDDDDDDDDDDDDDDDDDDDDDDD\"
50 PRINT \"EEEEEEEEEEEEEEEEEEEEEEEEEEEEEE\"
60 PRINT \"FFFFFFFFFFFFFFFFFFFFFFFFFFFFFF\"
70 PRINT \"GGGGGGGGGGGGGGGGGGGGGGGGGGGGGG\"
80 PRINT \"HHHHHHHHHHHHHHHHHHHHHHHHHHHHHH\"
";

fn expected_text() -> String {
    ["A", "B", "C", "D", "E", "F", "G", "H"]
        .iter()
        .map(|c| format!("{}\n", c.repeat(30)))
        .collect()
}

fn compile_reserved(reserved: Vec<(u16, u16)>) -> Compiled {
    let prg = tokenize_program(SRC).expect("tokenizes");
    let mut options = CompileOptions::default();
    options.reserved_ranges = reserved;
    compile_with_options(&prg, options).expect("compiles")
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

/// Independent re-check of the invariant, straight off the finished image: every `LABEL:`
/// followed by a literal `.byte` run must be linked at those bytes.
fn assert_labels_own_their_bytes(compiled: &Compiled) {
    use asm6502::Assembler6502;
    const ORIGIN: u16 = 0x080D; // default build: code starts past the SYS stub
    let mut a = Assembler6502::new();
    for (s, e) in &compiled.diagnostics.effective_reserved {
        a.add_reserved_range(*s, *e).expect("reservation accepted");
    }
    let image = a.assemble_bytes(&compiled.asm).expect("re-assembles");
    assert_eq!(
        image, compiled.machine_code,
        "re-assembling the emitted asm must reproduce the shipped image"
    );

    let mut pending: Vec<&str> = Vec::new();
    let mut checked = 0usize;
    for raw in compiled.asm.lines() {
        let line = raw.split(';').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if let Some(name) = line.strip_suffix(':') {
            if !name.is_empty() && !name.contains(char::is_whitespace) {
                pending.push(name);
                continue;
            }
        }
        let Some(rest) = line.strip_prefix(".byte ") else {
            pending.clear();
            continue;
        };
        let want: Vec<u8> = rest
            .split(',')
            .map_while(|t| u8::from_str_radix(t.trim().strip_prefix('$')?, 16).ok())
            .take(4)
            .collect();
        if want.is_empty() {
            pending.clear();
            continue;
        }
        for name in pending.drain(..) {
            let Some(addr) = a.lookup(name) else { continue };
            let Some(off) = usize::from(addr).checked_sub(usize::from(ORIGIN)) else {
                continue;
            };
            let Some(got) = image.get(off..off + want.len()) else {
                continue;
            };
            assert_eq!(
                got,
                want.as_slice(),
                "`{name}:` is linked at ${addr:04X} but the image holds {got:02X?} \
                 there, not the {want:02X?} it names"
            );
            checked += 1;
        }
    }
    assert!(
        checked >= 8,
        "expected to check the whole pool, got {checked}"
    );
}

#[test]
fn a_reservation_inside_the_string_pool_keeps_every_label_on_its_own_bytes() {
    // $0920 lands in the middle of this program's string pool.
    let c = compile_reserved(vec![(0x0920, 0x0920)]);
    assert!(
        c.diagnostics.bridge_shims >= 1,
        "this reservation is supposed to exercise the repair; got {} shims",
        c.diagnostics.bridge_shims
    );
    assert_labels_own_their_bytes(&c);
}

#[test]
fn a_reservation_inside_the_string_pool_still_prints_the_right_text() {
    let plain = compile_reserved(Vec::new());
    let reserved = compile_reserved(vec![(0x0920, 0x0920)]);
    assert_eq!(run(&plain), expected_text());
    assert_eq!(
        run(&reserved),
        expected_text(),
        "a reserved byte inside the string pool must not change what the \
         program prints"
    );
}

#[test]
fn every_single_byte_reservation_inside_the_image_still_prints_the_right_text() {
    let plain = compile_reserved(Vec::new());
    let end = 0x080Du16 + plain.machine_code.len() as u16;
    let want = expected_text();
    let mut exercised = 0usize;
    // $0900 is the lowest address auto-reserve will ever hand over (below it live the SYS stub
    // and the code origin), so it is the lowest a real program can produce.
    for addr in 0x0900..end {
        let c = compile_reserved(vec![(addr, addr)]);
        assert_labels_own_their_bytes(&c);
        assert_eq!(
            run(&c),
            want,
            "reserving ${addr:04X} changed the program's output"
        );
        exercised += usize::from(c.diagnostics.bridge_shims > 0);
    }
    assert!(
        exercised > 0,
        "the sweep must actually hit the repair path at least once"
    );
}

#[test]
fn a_program_without_reservations_needs_no_shim() {
    let c = compile_reserved(Vec::new());
    assert_eq!(c.diagnostics.bridge_shims, 0);
    assert!(!c.asm.contains("layout: the assembler skips reserved"));
}

#[test]
fn a_reservation_the_image_never_reaches_costs_nothing() {
    // Above the image: the assembler never inserts a bridge, so the repair must not fire and
    // must not add a byte.
    let plain = compile_reserved(Vec::new());
    let far = compile_reserved(vec![(0xC000, 0xC0FF)]);
    assert_eq!(far.diagnostics.bridge_shims, 0);
    assert_eq!(
        far.machine_code, plain.machine_code,
        "a reservation past the end of the image must not change the image"
    );
}

#[test]
fn the_repair_is_deterministic() {
    let a = compile_reserved(vec![(0x0920, 0x0920)]);
    let b = compile_reserved(vec![(0x0920, 0x0920)]);
    assert_eq!(a.machine_code, b.machine_code);
    assert_eq!(a.diagnostics.bridge_shims, b.diagnostics.bridge_shims);
}

#[test]
fn asm6502_still_strands_a_label_in_front_of_a_bridged_item() {
    use asm6502::Assembler6502;
    let mut asm = String::from("*=$0800\n");
    for _ in 0..0xF0 {
        asm.push_str("    NOP\n");
    }
    asm.push_str("MYDATA:\n    .byte ");
    let bytes: Vec<String> = (1..=32u32).map(|n| format!("${n:02X}")).collect();
    asm.push_str(&bytes.join(","));
    asm.push('\n');

    let mut a = Assembler6502::new();
    a.add_reserved_range(0x0900, 0x09FF).unwrap();
    let out = a.assemble_bytes(&asm).expect("assembles");
    assert_eq!(
        &out[0x0A00 - 0x0800..0x0A04 - 0x0800],
        &[0x01, 0x02, 0x03, 0x04],
        "the data itself is re-emitted past the reservation"
    );
    assert_eq!(
        a.lookup("MYDATA"),
        Some(0x08F0),
        "…but the label stays behind on the bridge JMP. If this now \
         reports $0A00, asm6502 has been fixed and `insert_bridge_shim` \
         in compile.rs is dead code."
    );
    assert_eq!(
        &out[0x08F0 - 0x0800..0x08F3 - 0x0800],
        &[0x4C, 0x00, 0x0A],
        "…and what the label points at is the bridge"
    );
}
