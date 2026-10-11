mod common;

use yabcompiler_core::{
    CompileOptions, Compiled, ExtraRamDecision, compile_with_options, tokenize_program,
};

const MEMSIZ: u32 = 0xA000;
const ORIGIN: u16 = 0x9000;

fn program(n: u32) -> String {
    format!(
        "\
10 N={n}:GOSUB 100
20 DIM A(N)
30 A(N)=7.5:B$=CHR$(72)+CHR$(73)
40 PRINT A(N);B$
50 END
100 IF PEEK(2)=7 THEN N=5
110 RETURN
"
    )
}

fn build(n: u32, force_extraram_off: bool) -> Compiled {
    let prg = tokenize_program(&program(n)).expect("tokenizes");
    compile_with_options(
        &prg,
        CompileOptions {
            custom_start_address: Some(ORIGIN),
            force_extraram_off,
            ..CompileOptions::default()
        },
    )
    .expect("compiles")
}

fn room_below_memsiz(c: &Compiled) -> u32 {
    MEMSIZ - (u32::from(ORIGIN) + c.machine_code.len() as u32)
}

fn run(c: &Compiled) -> emu64::RunToEnd {
    emu64::run_prg_to_end(&c.prg_bytes, common::MAX_INSNS).expect("emu64 run")
}

#[test]
fn runtime_dims_the_heap_below_memsiz_cannot_hold_get_the_extraram_layout() {
    let off = build(1000, true);
    let room = room_below_memsiz(&off);
    assert!(room < 1001 * 5, "fixture premise: {room} bytes below MEMSIZ can't hold the array");
    assert_eq!(
        run(&off).error_banner().as_deref(),
        Some("?OUT OF MEMORY  ERROR IN 20"),
        "the extraram-off layout really is too small"
    );

    let auto = build(1000, false);
    assert!(
        matches!(auto.diagnostics.extraram, ExtraRamDecision::AutoOn),
        "the DIM estimate picks the RAM-under-ROM heap, got {:?}",
        auto.diagnostics.extraram
    );
    let r = run(&auto);
    assert!(r.clean_exit, "must reach END, got {:?}", r.error_banner());
    assert_eq!(common::petscii_to_string(&r.output).trim_end(), " 7.5 HI");
}

#[test]
fn runtime_dims_that_fit_keep_the_plain_layout() {
    let auto = build(10, false);
    assert!(room_below_memsiz(&auto) >= 0x0800 + 11 * 5, "fixture premise");
    assert!(
        matches!(auto.diagnostics.extraram, ExtraRamDecision::AutoOff),
        "got {:?}",
        auto.diagnostics.extraram
    );
}

#[test]
fn an_array_stock_basic_had_no_room_for_stays_out_of_memory() {
    let auto = build(9000, false);
    assert!(
        matches!(auto.diagnostics.extraram, ExtraRamDecision::AutoOff),
        "got {:?}",
        auto.diagnostics.extraram
    );
    assert_eq!(
        run(&auto).error_banner().as_deref(),
        Some("?OUT OF MEMORY  ERROR IN 20")
    );
}
