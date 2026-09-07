//! `USR(x)` and the BASIC USR vector at $0310-$0312 (POKE 785/786).

use yabcompiler_core::{CompileOptions, compile_with_options, tokenize_program};

mod common;

const MAX_INSNS: u64 = 20_000_000;

fn compile(src: &str) -> Vec<u8> {
    let prg = tokenize_program(src).expect("tokenize");
    compile_with_options(&prg, CompileOptions::default())
        .expect("compile")
        .prg_bytes
}

fn asm_for(src: &str, opts: CompileOptions) -> String {
    let prg = tokenize_program(src).expect("tokenize");
    compile_with_options(&prg, opts).expect("compile").asm
}

#[test]
fn usr_with_the_default_vector_raises_illegal_quantity() {
    let prg = compile("10 print \"a\";usr(10);\"b\"\n20 print \"done\"\n");
    let r = emu64::run_prg_to_end(&prg, MAX_INSNS).expect("run");

    assert_eq!(
        r.error_code,
        Some(14),
        "expected ILLEGAL QUANTITY (14), banner was {:?}",
        r.error_banner()
    );
    assert_eq!(r.error_line, Some(10), "?... ERROR IN 10");
    assert_eq!(
        r.error_banner().as_deref(),
        Some("?ILLEGAL QUANTITY  ERROR IN 10"),
        "the ROM's own banner, double space and all"
    );
    // Everything before the USR still printed; nothing after it did.
    let out = common::petscii_to_string(&r.output);
    assert_eq!(out, "A", "output before the error only, got {out:?}");
}

#[test]
fn usr_through_a_poked_vector_still_calls_the_routine() {
    let prg = compile(concat!(
        "10 for i=0 to 6:read d:poke 49152+i,d:next\n",
        "20 data 169,0,160,5,76,145,179\n",
        "30 poke 785,0:poke 786,192\n",
        "40 print \"u=\";usr(7)\n",
        "50 print \"done\"\n",
    ));
    let r = emu64::run_prg_to_end(&prg, MAX_INSNS).expect("run");

    assert_eq!(
        r.error_code, None,
        "a POKEd vector must not raise, banner {:?}",
        r.error_banner()
    );
    assert!(r.clean_exit, "program should run to its RTS");
    assert_eq!(
        common::petscii_to_string(&r.output),
        "U= 5 \nDONE\n",
        "the POKEd routine's answer, then the rest of the program"
    );
}

#[test]
fn usr_poked_back_to_the_rom_stub_still_raises() {
    let prg = compile("10 poke 785,72:poke 786,178\n20 print \"a\";usr(10)\n30 print \"done\"\n");
    let r = emu64::run_prg_to_end(&prg, MAX_INSNS).expect("run");
    assert_eq!(r.error_code, Some(14), "banner {:?}", r.error_banner());
    assert_eq!(r.error_line, Some(20));
    assert_eq!(common::petscii_to_string(&r.output), "A");
}

#[test]
fn startup_never_writes_the_usr_vector() {
    let asm = asm_for("10 print usr(1)\n", CompileOptions::default());
    for store in ["STA $0310", "STA $0311", "STA $0312"] {
        assert!(
            !asm.contains(store),
            "startup must leave the cold-boot USR vector alone ({store})\n{asm}"
        );
    }
    assert!(
        !asm.contains("__USR_RTS"),
        "the silent-RTS default stub should be gone\n{asm}"
    );
    assert!(
        asm.contains("JMP ($0311)"),
        "a POKEd vector is still dispatched indirectly\n{asm}"
    );
}

#[test]
fn extraram_routes_the_default_vector_through_a_bank_in_thunk() {
    let opts = CompileOptions {
        extraram: true,
        ..CompileOptions::default()
    };
    let asm = asm_for("10 print usr(1)\n", opts);
    assert!(
        asm.contains("__USR_CALL:"),
        "USR helper should be emitted\n{asm}"
    );
    assert!(
        !asm.contains("JMP $B248") && !asm.contains("JSR $B248"),
        "must never jump into banked-out ROM space\n{asm}"
    );
    let call = &asm[asm.find("__USR_CALL:").unwrap()..];
    let call = &call[..call.find("__USR_VECTORED:").unwrap()];
    assert!(
        call.contains("LDX #$0E") && call.contains("__JT_"),
        "the default arm should raise via the extraram thunk, got:\n{call}"
    );
}
