mod common;

use yabcompiler_core::{CompileOptions, Compiled, compile_with_options, tokenize_program};

fn try_build(src: &str) -> Result<Compiled, String> {
    let prg = tokenize_program(src).expect("tokenize");
    compile_with_options(
        &prg,
        CompileOptions {
            extraram: true,
            ..CompileOptions::default()
        },
    )
    .map_err(|e| e.to_string())
}

fn build(src: &str) -> Compiled {
    try_build(src).expect("compile")
}

/// A program with a string heap, `PRINT`, and a runtime `DIM`, plus `pad` unread 200-character `DATA` lines to dial the image size.
fn heap_program(pad: usize) -> String {
    let mut s = String::from(
        "\
10 N=50
20 DIM A(N)
30 A(7)=1234
40 B$=CHR$(72)+CHR$(69)+CHR$(65)+CHR$(80)
50 PRINT A(7);B$
60 END
",
    );
    let mut ln = 9000;
    for _ in 0..pad {
        s.push_str(&format!("{ln} DATA {}\n", "A".repeat(200)));
        ln += 10;
    }
    s
}

/// [`heap_program`] with one more statement spliced in before the `PRINT`.
fn heap_program_with(line: &str) -> String {
    heap_program(0).replace("50 PRINT", &format!("45 {line}\n50 PRINT"))
}

/// Last byte of the image, inclusive.
fn image_end(c: &Compiled) -> u32 {
    0x080D + c.machine_code.len() as u32 - 1
}

#[test]
fn a_heap_program_relocates_memsiz_and_puts_it_back() {
    let asm = build(&heap_program(4)).asm;
    for want in [
        "    LDA $37\n    STA __EXRAM_MEMSIZ_SAVE\n",
        "    LDA $38\n    STA __EXRAM_MEMSIZ_SAVE+1\n",
        "    JSR __EXRAM_MEMSIZ_APPLY\n",
        "__EXRAM_MEMSIZ_RAISE:\n    LDA #$00\n    STA $37\n    LDA #$D0\n    STA $38\n",
        "__EXRAM_MEMSIZ_RESTORE:\n",
        "    JSR __EXRAM_MEMSIZ_RESTORE\n",
    ] {
        assert!(
            asm.contains(want),
            "the extraram preamble is missing:\n{want}\ngot:\n{}",
            preamble(&asm)
        );
    }
    // The restore has to sit between the body and the RTS that hands control back to BASIC.
    let exit = asm.find("    JSR __EXRAM_CODE_START\n").expect("body call");
    assert!(
        asm[exit..].starts_with(
            "    JSR __EXRAM_CODE_START\n    JSR __EXRAM_MEMSIZ_RESTORE\n    INC $01\n    RTS\n"
        ),
        "the exit sequence does not restore MEMSIZ between the body and the RTS:\n{}",
        &asm[exit..(exit + 120).min(asm.len())]
    );
}

#[test]
fn the_error_and_warm_start_jt_entries_restore_memsiz_first() {
    // A runtime `DIM` brings `__DIM_OOM` with it, and that is a `JMP $A437` ; so this fixture has an error JT entry to inspect.
    let asm = build(&heap_program(4)).asm;
    let jt = asm
        .find("__JT_BASIC_ERROR:\n")
        .expect("the error JT entry is emitted for this program");
    let body = &asm[jt..];
    let inc = body.find("    INC $01\n").expect("bank-in");
    let restore = body
        .find("    JSR __EXRAM_MEMSIZ_RESTORE\n")
        .expect("the error JT entry must put MEMSIZ back");
    assert!(
        restore < inc,
        "__JT_BASIC_ERROR banks ROM in before restoring MEMSIZ:\n{}",
        &body[..120.min(body.len())]
    );
}

#[test]
fn a_sys_site_costs_one_byte_and_swaps_memsiz_both_ways() {
    let asm = build(&heap_program_with("SYS 49152")).asm;
    assert!(
        asm.contains("__EXRAM_MEMSIZ_SAVE"),
        "the fixture is not relocating MEMSIZ, so it cannot test the SYS wrappers"
    );
    assert!(
        asm.contains("__EXRAM_SYS_IN:\n") && asm.contains("__EXRAM_SYS_OUT:\n"),
        "no SYS wrappers were emitted:\n{}",
        preamble(&asm)
    );
    assert!(
        asm.contains("    JSR __EXRAM_SYS_IN\n") && asm.contains("    JSR __EXRAM_SYS_OUT\n"),
        "the SYS site was not routed through the wrappers"
    );

    assert!(
        asm.contains(
            "    JSR __EXRAM_SYS_IN\n    JSR __EXRAM_SYSJT_0\n    JSR __EXRAM_SYS_OUT\n"
        ),
        "the SYS call is not bracketed by the MEMSIZ swap:\n{}",
        preamble(&asm)
    );

    let out = &asm[asm.find("__EXRAM_SYS_OUT:\n").unwrap()..];
    assert!(
        out.starts_with("__EXRAM_SYS_OUT:\n    PHP\n    PHA\n    JSR __EXRAM_MEMSIZ_APPLY\n"),
        "the bank-out wrapper does not go through the conditional:\n{}",
        &out[..90.min(out.len())]
    );

    for (i, line) in asm.lines().enumerate() {
        if line.trim() != "INC $01" {
            continue;
        }
        let next = asm.lines().nth(i + 1).unwrap_or("").trim();
        assert!(
            next.starts_with("JSR $") || next == "RTS",
            "a bare `INC $01` that is not a JT entry survived at line {i}: next is {next:?}"
        );
    }
    // Registers and flags are what make this fiddly: the wrapper runs between the `SYS a,x,y,sr` register setup and the call.
    let win = &asm[asm.find("__EXRAM_SYS_IN:\n").unwrap()..];
    assert!(
        win.starts_with("__EXRAM_SYS_IN:\n    PHP\n    PHA\n"),
        "the bank-in wrapper does not preserve A and the flags:\n{}",
        &win[..80.min(win.len())]
    );
}

#[test]
fn a_program_without_sys_carries_no_sys_wrappers() {
    let asm = build(&heap_program(4)).asm;
    assert!(!asm.contains("__EXRAM_SYS_IN:"));
    assert!(!asm.contains("__EXRAM_SYS_OUT:"));
}

/// First refusal: no heap, no relocation.
#[test]
fn a_program_with_no_heap_is_left_alone() {
    let asm = build("10 FOR I=1 TO 3\n20 PRINT I\n30 NEXT\n").asm;
    assert!(
        !asm.contains("__HEAP_BOTTOM:"),
        "this fixture was meant to have no runtime heap; it now has one, \
         so it no longer tests the refusal"
    );
    assert!(!asm.contains("__EXRAM_MEMSIZ_SAVE"));
}

#[test]
fn a_program_that_pokes_memsiz_keeps_its_own_answer() {
    let src = "10 POKE55,0:POKE56,32:CLR\n20 B$=\"X\"\n30 N=3:DIM A(N)\n40 PRINT PEEK(56);B$;A(1)\n";
    let built = build(src);
    assert!(
        built.asm.contains("__HEAP_BOTTOM:"),
        "the fixture must have a heap, or it is caught by the other refusal"
    );
    assert!(
        !built.asm.contains("__EXRAM_MEMSIZ_SAVE"),
        "MEMSIZ was relocated behind the back of a program that pokes it"
    );

    let peek_only = build("10 B$=\"X\"\n20 N=3:DIM A(N)\n30 PRINT PEEK(56);B$;A(1)\n");
    assert!(
        !peek_only.asm.contains("__EXRAM_MEMSIZ_SAVE"),
        "MEMSIZ was relocated under a program that reads it"
    );
}

#[test]
fn a_reservation_above_the_data_ceiling_lowers_the_ceiling() {
    let asm = build(&heap_program_with("POKE 49152,1:POKE 49153,2")).asm;
    assert!(
        asm.contains("__EXRAM_MEMSIZ_SAVE"),
        "the fixture must be relocating MEMSIZ at all"
    );
    assert!(
        asm.contains("__EXRAM_MEMSIZ_RAISE:\n    LDA #$00\n    STA $37\n    LDA #$C0\n    STA $38\n"),
        "the ceiling did not come down to the $C000 reservation:\n{}",
        preamble(&asm)
    );
}

#[test]
fn a_small_image_keeps_the_machines_own_ceiling() {
    // A heap, and nothing printed but the number under test.
    let src = "10 B$=CHR$(72)+CHR$(69)\n20 PRINT FRE(0)\n30 IF B$=\"\" THEN PRINT B$\n";
    let built = build(src);
    let end = image_end(&built);
    assert!(end < 0xA000, "the control must be a small image");
    let asm = &built.asm;
    // Structural: the raise is reachable only through the compare.
    let apply = asm
        .find("__EXRAM_MEMSIZ_APPLY:\n")
        .expect("the conditional is emitted");
    let raise = asm.find("__EXRAM_MEMSIZ_RAISE:\n").expect("the raise");
    let guard = &asm[apply..raise];
    assert!(
        guard.contains("    SBC #<__HEAP_BOTTOM\n")
            && guard.contains("    SBC #>__HEAP_BOTTOM\n")
            && guard.contains("    BCC __EXRAM_MEMSIZ_RAISE\n"),
        "the raise is not guarded by a compare against __HEAP_BOTTOM:\n{guard}"
    );

    assert!(
        guard.contains(&format!(
            "    CMP #${:02X}\n    BCC __EXRAM_MEMSIZ_RAISE\n",
            yabcompiler_core::compile::MIN_STRING_HEAP >> 8
        )),
        "the guard does not require MIN_STRING_HEAP bytes of heap:\n{guard}"
    );
    // Behavioural: `FRE(0)` reports against `$A000`, not `$D000`.
    let out = common::run_clean(&built.prg_bytes).expect("runs to a clean exit");
    let got: i32 = out
        .split_whitespace()
        .find_map(|w| w.parse::<i32>().ok())
        .unwrap_or_else(|| panic!("no number in FRE output {out:?}"));
    let signed = |free: i64| if free > 32767 { free - 65536 } else { free } as i32;
    let at_a000 = signed(0xA000 - end as i64);
    let at_d000 = signed(0xD000 - end as i64);
    assert!(
        (got - at_a000).abs() < 600,
        "FRE reported {got}; the $A000 ceiling predicts about {at_a000} and the \
         $D000 ceiling about {at_d000}"
    );
    assert!(
        (got - at_d000).abs() > 600,
        "the two ceilings predict indistinguishable FRE values here ({at_a000} vs \
         {at_d000}); this fixture cannot tell them apart"
    );
}

#[test]
fn an_image_above_the_rom_window_can_still_dim_and_print() {
    // Find a pad that puts the image past `$A000`.
    let mut pad = 0usize;
    let built = loop {
        assert!(
            pad <= 260,
            "260 pad lines did not push the extraram image past $A000; \
             the dial is not doing its job"
        );
        let c = try_build(&heap_program(pad)).unwrap_or_else(|e| {
            panic!("the dial overran the layout at {pad} pad lines before crossing $A000: {e}")
        });
        if image_end(&c) > 0xA000 {
            break c;
        }
        pad += 8;
    };
    let end = image_end(&built);
    assert!(
        end > 0xA000 && end < 0xD000,
        "the fixture image ends at ${end:04X}, outside the window under test"
    );
    let out = common::run_clean(&built.prg_bytes).unwrap_or_else(|| {
        panic!("the compiled image did not run to a clean exit (image ends ${end:04X})")
    });
    assert!(
        out.contains("1234") && out.contains("HEAP"),
        "an image ending at ${end:04X} lost its DIM and its heap string; got {out:?}"
    );

    let small = build(&heap_program(0));
    assert!(image_end(&small) < 0xA000, "the control must be a small image");
    let small_out =
        common::run_clean(&small.prg_bytes).expect("the small build runs to a clean exit");
    assert_eq!(
        small_out.trim(),
        out.trim(),
        "the big and small builds of the same program disagree"
    );
}

/// Everything from the JT header to the end of the preamble, for failure messages.
fn preamble(asm: &str) -> &str {
    let start = asm.find("; --- extraram:").unwrap_or(0);
    let end = asm
        .find("; --- end extraram preamble ---")
        .map_or(asm.len(), |e| e + 31);
    &asm[start..end]
}
