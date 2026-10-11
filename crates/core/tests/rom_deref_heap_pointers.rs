//! A BASIC ROM routine dereferences its pointer with BASIC ROM banked IN.

use yabcompiler_core::{CompileOptions, Compiled, compile_with_options, tokenize_program};

const ORIGIN: u32 = 0x0801;
/// First byte of the BASIC ROM window.
const ROM_WINDOW_LO: u32 = 0xA000;
/// First byte the BASIC ROM window does not cover.
const ROM_WINDOW_HI: u32 = 0xC000;

fn build_opts(src: &str, opts: CompileOptions) -> Compiled {
    let prg = tokenize_program(src).expect("tokenize");
    compile_with_options(&prg, opts).expect("compile")
}

fn build_extraram(src: &str) -> Compiled {
    build_opts(
        src,
        CompileOptions {
            extraram: true,
            ..CompileOptions::default()
        },
    )
}

/// Build with extraram on and the image lifted so that it ends inside the BASIC ROM window, which is where the string heap then starts.
fn build_extraram_high(src: &str) -> Compiled {
    build_opts(
        src,
        CompileOptions {
            extraram: true,
            reserved_ranges: vec![(0x0900, 0x9E00)],
            ..CompileOptions::default()
        },
    )
}

fn build_extraram_above_window(rows: usize) -> Compiled {

    let mut s = String::from(
        "\
10 FOR I=1 TO 3:A$=A$+CHR$(48+I):NEXT
20 PRINT VAL(A$)
",
    );
    for r in 0..rows {
        let body: Vec<String> = (0..20)
            .map(|j| {
                format!(
                    "POKE {},PEEK({})+{}",
                    49152 + (r * 20 + j) % 4000,
                    780 + (r + j) % 4,
                    (r * 20 + j) % 251
                )
            })
            .collect();
        s.push_str(&format!("{} {}\n", 1000 + r, body.join(":")));
    }
    s.push_str("30000 END\n");
    build_opts(
        &s,
        CompileOptions {
            extraram: true,
            ..CompileOptions::default()
        },
    )
}

fn build_default(src: &str) -> Compiled {
    build_opts(src, CompileOptions::default())
}

/// One past the last byte of the load image, which is where the string heap starts.
fn image_end(c: &Compiled) -> u32 {
    ORIGIN + c.machine_code.len() as u32
}

/// Resolve a label address by assembling the generated code.
fn label_addr(c: &Compiled, name: &str) -> u32 {
    use asm6502::Assembler6502;
    let mut a = Assembler6502::new();
    for (s, e) in &c.diagnostics.effective_reserved {
        a.add_reserved_range(*s, *e).expect("reservation accepted");
    }
    let image = a.assemble_bytes(&c.asm).expect("re-assembles");
    assert_eq!(
        image, c.machine_code,
        "re-assembling the emitted asm must reproduce the shipped image"
    );
    u32::from(
        a.lookup(name)
            .unwrap_or_else(|| panic!("label `{name}` was not resolved")),
    )
}

const VAL_ON_HEAP_STRING: &str = "\
10 FOR I=1 TO 3:A$=A$+CHR$(48+I):NEXT
20 PRINT VAL(A$)
30 END
";

/// `POKE` lines needed to lift the image end past `$C000`.
const POKE_PAD: usize = 150;

/// `__VAL_BUF`'s emitted body, as the list of byte values between its label and the next label.
fn val_buf_bytes(asm: &str) -> Vec<u8> {
    let start = asm
        .find("__VAL_BUF:\n")
        .expect("__VAL_BUF is emitted for a program that uses VAL");
    let body = &asm[start + "__VAL_BUF:\n".len()..];
    let mut out = Vec::new();
    for line in body.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix(".byte ") {
            for item in rest.split(',') {
                let item = item.trim();
                let hex = item.strip_prefix('$').expect("hex byte operand");
                out.push(u8::from_str_radix(hex, 16).expect("hex byte"));
            }
        } else if !t.is_empty() {
            // any non-`.byte` line ends the block
            break;
        }
    }
    out
}

#[test]
fn the_fixture_puts_its_string_heap_inside_the_rom_window() {
    let c = build_extraram_high(VAL_ON_HEAP_STRING);
    let end = image_end(&c);
    assert!(
        c.diagnostics.extraram.is_on(),
        "fixture did not build with extraram on"
    );
    assert!(
        end > ROM_WINDOW_LO && end < ROM_WINDOW_HI,
        "fixture's heap starts at ${end:04X}, outside the ${ROM_WINDOW_LO:04X}..\
         ${ROM_WINDOW_HI:04X} window it is supposed to exercise"
    );

    assert!(
        c.asm.contains("JSR __VAL_HELPER"),
        "fixture folded its VAL away, so it tests nothing"
    );
}

#[test]
fn val_buf_lands_below_the_rom_window() {
    let c = build_extraram_high(VAL_ON_HEAP_STRING);
    let addr = label_addr(&c, "__VAL_BUF");
    assert!(
        addr < ROM_WINDOW_LO,
        "__VAL_BUF assembled to ${addr:04X}, inside the BASIC ROM window — \
         VAL would parse ROM bytes"
    );

    let top = addr + val_buf_bytes(&c.asm).len() as u32;
    assert!(
        top <= ROM_WINDOW_LO,
        "__VAL_BUF ends at ${top:04X}, past the start of the BASIC ROM window"
    );

    assert!(
        image_end(&c) > ROM_WINDOW_LO,
        "fixture premise lost: the image no longer reaches the window"
    );
}

#[test]
fn val_buf_is_exactly_256_bytes() {
    let asm = build_extraram_high(VAL_ON_HEAP_STRING).asm;
    let body = val_buf_bytes(&asm);
    assert_eq!(
        body.len(),
        256,
        "__VAL_BUF must hold 255 characters plus the ROM's terminator byte"
    );
}

/// The copy loop counts a full 8-bit index down, so it can move a 255-character string.
#[test]
fn val_helper_copies_the_string_before_the_rom_call() {
    let c = build_extraram_high(VAL_ON_HEAP_STRING);
    let helper = val_helper_body(&c.asm);

    assert!(
        helper.contains("__VAL_COPY:\n    LDA ($03),Y\n    STA __VAL_BUF-1,Y\n    DEY\n    BNE __VAL_COPY\n"),
        "the length-counted copy into __VAL_BUF is missing:\n{helper}"
    );
    assert!(
        helper.contains("    LDA #<__VAL_BUF\n    STA $22\n    LDA #>__VAL_BUF\n    STA $23\n"),
        "INDEX1 is not pointed at __VAL_BUF:\n{helper}"
    );

    assert!(
        !helper.contains("    INC $22\n"),
        "__VAL_HELPER still walks the CALLER's pointer into INDEX1 — that is \
         the ROM-dereference defect:\n{helper}"
    );
    // The length has to survive the copy, which ends with Y = 0, so the copy body cannot use the TAY/TYA the in-place body uses.
    assert!(
        helper.contains("    PHA\n") && helper.contains("    PLA\n    JMP "),
        "the string length is not carried across the copy:\n{helper}"
    );
}

#[test]
fn an_unexposed_build_emits_the_pre_existing_helper_byte_for_byte() {
    let c = build_default(VAL_ON_HEAP_STRING);
    let helper = val_helper_body(&c.asm);
    assert_eq!(
        helper,
        "__VAL_HELPER:\n    LDY #$00\n    LDA ($03),Y\n    BEQ __VAL_ZERO\n    TAY\n\
         \x20   LDA $03\n    STA $22\n    LDA $04\n    STA $23\n    INC $22\n\
         \x20   BNE __VAL_PTR_OK\n    INC $23\n__VAL_PTR_OK:\n    LDX #$00\n\
         \x20   STX $0D\n    TYA\n    JMP $B7B5\n",
        "the in-place body drifted from the sequence it is supposed to reproduce"
    );
}

#[test]
fn an_extraram_image_above_the_window_pays_nothing() {
    let c = build_extraram_above_window(POKE_PAD);
    let end = image_end(&c);
    assert!(
        c.diagnostics.extraram.is_on(),
        "fixture did not build with extraram on"
    );
    assert!(
        end >= ROM_WINDOW_HI,
        "fixture premise lost: image ends at ${end:04X}, still below \
         ${ROM_WINDOW_HI:04X}, so it IS exposed and this tests nothing"
    );
    assert!(
        c.asm.contains("JSR __VAL_HELPER"),
        "fixture folded its VAL away, so it tests nothing"
    );
    assert!(
        !c.asm.contains("__VAL_BUF"),
        "an image whose heap starts above the ROM window is paying for a \
         copy it cannot need"
    );
    let helper = val_helper_body(&c.asm);
    assert!(
        helper.contains("    INC $22\n"),
        "the in-place body is not the one that survived:\n{helper}"
    );
}

/// Both bodies live in the asm codegen produces and exactly one is supposed to reach the assembler.
#[test]
fn no_variant_sentinel_reaches_the_shipped_image() {
    let builds = [
        build_default(VAL_ON_HEAP_STRING),
        build_extraram(VAL_ON_HEAP_STRING),
        build_extraram_high(VAL_ON_HEAP_STRING),
        build_extraram_above_window(POKE_PAD),
        build_default("10 INPUT A\n20 PRINT A\n30 END\n"),
    ];
    for (i, c) in builds.iter().enumerate() {
        assert!(
            !c.asm.contains("@@VAL_"),
            "build {i} shipped asm still containing a VAL variant sentinel"
        );

        let both = c.asm.contains("__VAL_COPY:") && c.asm.contains("    INC $22\n");
        assert!(!both, "build {i} shipped BOTH __VAL_HELPER bodies");
    }
}

/// `__VAL_HELPER`'s body, from its label to `__VAL_ZERO`.
fn val_helper_body(asm: &str) -> &str {
    let start = asm.find("__VAL_HELPER:\n").expect("__VAL_HELPER emitted");
    let body = &asm[start..];
    let end = body.find("__VAL_ZERO:").expect("__VAL_ZERO follows");
    &body[..end]
}

#[test]
fn the_hoisted_input_buffer_is_also_below_the_rom_window() {
    let c = build_extraram_high("10 INPUT A\n20 PRINT A\n30 END\n");
    assert!(
        image_end(&c) > ROM_WINDOW_LO,
        "fixture premise lost: image ends at ${:04X}",
        image_end(&c)
    );
    let addr = label_addr(&c, "__FIELD_BUF");
    assert!(
        addr < ROM_WINDOW_LO,
        "__FIELD_BUF assembled to ${addr:04X}, inside the BASIC ROM window"
    );
}

#[test]
fn val_of_a_heap_string_above_a000_parses_the_string_and_not_the_rom() {
    let c = build_extraram_high(VAL_ON_HEAP_STRING);
    assert!(
        image_end(&c) > ROM_WINDOW_LO && image_end(&c) < ROM_WINDOW_HI,
        "fixture premise lost: heap starts at ${:04X}",
        image_end(&c)
    );
    let r = emu64::run_prg_to_end(&c.prg_bytes, 80_000_000).expect("runs");
    assert!(r.clean_exit, "program did not reach a clean exit");
    let out = decode(&r.output);
    assert!(
        out.contains("123"),
        "VAL of a heap string above $A000 printed {out:?}, not 123 — the ROM \
         parsed banked-in ROM bytes instead of the string"
    );
}

#[test]
fn val_of_a_255_character_heap_string_is_parsed_whole() {
    let src = "\
10 Z$=\"\"
20 FOR I=1 TO 252:Z$=Z$+\"0\":NEXT
30 A$=Z$+CHR$(49)+CHR$(50)+CHR$(51)
40 PRINT LEN(A$)
50 PRINT VAL(A$)
60 END
";
    // On the exposed layout, so the 256-byte buffer is the thing under test and not merely present.
    let c = build_extraram_high(src);
    let r = emu64::run_prg_to_end(&c.prg_bytes, 80_000_000).expect("runs");
    assert!(r.clean_exit, "program did not reach a clean exit");
    let out = decode(&r.output);
    assert!(
        out.contains("255"),
        "fixture premise lost: the string is not 255 characters ({out:?})"
    );
    assert!(
        out.contains("123"),
        "a 255-character VAL argument parsed as {out:?}, not 123 — the copy \
         buffer is too small or the length is being clamped"
    );
}

/// PETSCII to something assertable.
fn decode(bytes: &[u8]) -> String {
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

/// A program with no `VAL` pays nothing for any of this.
#[test]
fn a_program_without_val_has_no_val_buf() {
    let asm = build_default("10 PRINT \"HI\"").asm;
    assert!(
        !asm.contains("__VAL_BUF"),
        "__VAL_BUF is emitted for a program that never calls VAL"
    );
}

const RUNTIME_FLOAT_ARRAY: &str = "\
10 N=PEEK(781)+40
20 DIM A(N)
30 FOR I=1 TO 3:A(I)=I*100:NEXT
40 FOR I=1 TO 3:PRINT A(I):NEXT
50 END
";

#[test]
fn ld_arr_still_hands_fd_fe_straight_to_movfm() {
    let asm = build_default(RUNTIME_FLOAT_ARRAY).asm;
    assert!(
        asm.contains("__LD_ARR:\n    LDA $FD\n    LDY $FE\n    JMP $BBA2\n"),
        "the documented MOVFM sequence is gone; revisit the hazard note on \
         extraram::HEAP_CEILING"
    );
}

#[test]
fn the_runtime_array_allocator_is_seeded_at_d000_and_carves_down() {
    let asm = build_extraram(RUNTIME_FLOAT_ARRAY).asm;
    assert!(
        asm.contains("__ARR_HEAP_TOP:"),
        "fixture does not produce a runtime array allocator"
    );
    assert!(
        asm.contains("__EXRAM_MEMSIZ_RAISE:\n    LDA #$00\n    STA $37\n    LDA #$D0\n    STA $38\n"),
        "the emitted runtime no longer raises MEMSIZ to $D000; the latency \
         argument on extraram::HEAP_CEILING depends on that seed"
    );
    // Carving downwards: __DIM_ALLOC subtracts the request from the top.
    let start = asm.find("__DIM_ALLOC:\n").expect("__DIM_ALLOC emitted");
    let body = &asm[start..(start + 240).min(asm.len())];
    assert!(
        body.contains("    SEC\n    LDA __ARR_HEAP_TOP\n    SBC __DIM_BYTES_LO\n"),
        "__DIM_ALLOC no longer carves downwards from the top:\n{body}"
    );
}

#[test]
fn a_runtime_array_needs_4096_bytes_of_allocation_to_reach_the_window() {

    assert_eq!(0xD000u32 - ROM_WINDOW_HI, 4096);
}
