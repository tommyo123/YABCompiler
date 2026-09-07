//! Where the live `DATA` cursor lives, and why a `SYS` moves it.

use yabcompiler_core::{CompileOptions, compile_with_options, tokenize_program};

mod common;

const MAX_INSNS: u64 = 40_000_000;

fn compile(src: &str) -> Vec<u8> {
    let prg = tokenize_program(src).expect("tokenize");
    compile_with_options(&prg, CompileOptions::default())
        .expect("compile")
        .prg_bytes
}

fn asm_for(src: &str) -> String {
    let prg = tokenize_program(src).expect("tokenize");
    compile_with_options(&prg, CompileOptions::default())
        .expect("compile")
        .asm
}

fn run(src: &str) -> emu64::RunToEnd {
    emu64::run_prg_to_end(&compile(src), MAX_INSNS).expect("run")
}

const CLOBBER_ALL: &str = "169,255,133,251,133,252,133,253,133,254,96";

#[test]
fn read_survives_machine_code_that_owns_fb_through_fe() {
    let r = run(&format!(
        "10 for i=0 to 10: read d: poke 49152+i,d: next\n\
         20 data {CLOBBER_ALL}\n\
         30 read a: print \"a=\";a\n\
         40 sys 49152\n\
         50 read b: print \"b=\";b\n\
         60 read c: print \"c=\";c\n\
         70 data 11,22,33\n"
    ));
    assert_eq!(
        r.error_code,
        None,
        "no error expected, banner {:?}",
        r.error_banner()
    );
    assert_eq!(
        common::petscii_to_string(&r.output),
        "A= 11 \nB= 22 \nC= 33 \n",
        "all three READs must survive the SYS"
    );
}

#[test]
fn float_data_survives_the_same_clobber() {
    let r = run(&format!(
        "10 for i=0 to 10: read d: poke 49152+i,d: next\n\
         20 data {CLOBBER_ALL}\n\
         30 sys 49152\n\
         40 read a: read b: print a+b\n\
         50 data 1.5, 2.25\n"
    ));
    assert_eq!(r.error_code, None, "banner {:?}", r.error_banner());
    assert_eq!(common::petscii_to_string(&r.output), " 3.75 \n");
}

/// A string item forces `AsciiTagged`, whose advance re-reads the length byte through the staging pair after the value is consumed.
#[test]
fn string_data_survives_the_same_clobber() {
    let r = run(&format!(
        "10 for i=0 to 10: read d: poke 49152+i,d: next\n\
         20 data {CLOBBER_ALL}\n\
         30 read a$: print a$\n\
         40 sys 49152\n\
         50 read b$: print b$\n\
         60 read c$: print c$\n\
         70 data one, two, three\n"
    ));
    assert_eq!(r.error_code, None, "banner {:?}", r.error_banner());
    assert_eq!(common::petscii_to_string(&r.output), "ONE\nTWO\nTHREE\n");
}

#[test]
fn read_into_an_array_survives_the_same_clobber() {
    let r = run(&format!(
        "10 for i=0 to 10: read d: poke 49152+i,d: next\n\
         20 data {CLOBBER_ALL}\n\
         30 dim v(3)\n\
         40 sys 49152\n\
         50 for i=0 to 2: read v(i): next\n\
         60 print v(0);v(1);v(2)\n\
         70 data 7,8,9\n"
    ));
    assert_eq!(r.error_code, None, "banner {:?}", r.error_banner());
    assert_eq!(common::petscii_to_string(&r.output), " 7  8  9 \n");
}

/// `RESTORE` re-arms the cursor.
#[test]
fn restore_after_a_sys_rewinds_the_bss_cursor() {
    let r = run(&format!(
        "10 for i=0 to 10: read d: poke 49152+i,d: next\n\
         20 data {CLOBBER_ALL}\n\
         30 read a: sys 49152: read b\n\
         40 print a;b\n\
         50 restore\n\
         60 for i=0 to 10: read d: next\n\
         70 read a: read b: print a;b\n\
         80 data 41,42\n"
    ));
    assert_eq!(r.error_code, None, "banner {:?}", r.error_banner());
    assert_eq!(
        common::petscii_to_string(&r.output),
        " 41  42 \n 41  42 \n",
        "the second pass must see the same two values"
    );
}

#[test]
fn out_of_data_is_still_raised_past_the_end_of_the_pool() {
    let r = run(&format!(
        "10 for i=0 to 10: read d: poke 49152+i,d: next\n\
         20 data {CLOBBER_ALL}\n\
         30 sys 49152\n\
         40 read a: print a\n\
         50 read b: print b\n\
         60 data 5\n"
    ));
    assert_eq!(
        r.error_code,
        Some(13),
        "expected OUT OF DATA, banner {:?}",
        r.error_banner()
    );
    assert_eq!(r.error_line, Some(50), "at the READ that ran past the end");
    assert_eq!(common::petscii_to_string(&r.output), " 5 \n");
}

/// The slot is ordinary BSS, so a program big enough to earn the tail zero-fill puts the cursor inside the truncated run.
#[test]
fn the_cursor_survives_being_inside_the_truncated_bss_run() {
    let src = format!(
        "10 dim z(200)\n\
         20 for i=0 to 10: read d: poke 49152+i,d: next\n\
         30 data {CLOBBER_ALL}\n\
         40 sys 49152\n\
         50 read a: read b: print a;b\n\
         60 data 77,88\n"
    );
    let asm = asm_for(&src);
    let bss_fill = asm.find("; --- BSS zero-fill ---").expect("fill installed");
    let arm = asm.find("JSR __DATA_INIT").expect("cursor armed");
    assert!(bss_fill < arm, "the zero-fill must precede __DATA_INIT");
    let start = asm.find("__BSS_START:").expect("run bracketed");
    let slot = asm.find("\n__DATA_CUR:\n").expect("slot emitted");
    assert!(
        start < slot,
        "this test is only meaningful while the slot is inside the run"
    );

    let r = run(&src);
    assert_eq!(r.error_code, None, "banner {:?}", r.error_banner());
    assert_eq!(common::petscii_to_string(&r.output), " 77  88 \n");
}

#[test]
fn usr_moves_the_cursor_as_well_as_sys() {

    let r = run(concat!(
        "10 for i=0 to 12: read d: poke 49152+i,d: next\n",
        "20 data 169,255,133,251,133,252,169,0,160,7,76,145,179\n",
        "30 poke 785,0: poke 786,192\n",
        "40 print usr(1)\n",
        "50 read a: read b: print a;b\n",
        "60 data 61,62\n",
    ));
    assert_eq!(r.error_code, None, "banner {:?}", r.error_banner());
    assert_eq!(common::petscii_to_string(&r.output), " 7 \n 61  62 \n");
}

#[test]
fn a_sys_free_program_keeps_the_cursor_in_zero_page() {
    let asm = asm_for("10 read a: read b: print a+b\n20 data 4,5\n");
    assert!(
        !asm.contains("__DATA_CUR"),
        "no SYS, so no BSS cursor should be emitted:\n{asm}"
    );
    assert!(
        asm.contains("    STA $FB") && asm.contains("    LDA $FB"),
        "the cursor should still live at $FB/$FC:\n{asm}"
    );
}

#[test]
fn a_sys_free_program_still_reads_its_data() {
    let r = run("10 read a: read b$: read c: print a;b$;c\n20 data 4,hi,5.5\n");
    assert_eq!(r.error_code, None, "banner {:?}", r.error_banner());
    assert_eq!(common::petscii_to_string(&r.output), " 4 HI 5.5 \n");
}

#[test]
fn a_sys_without_data_emits_no_cursor_slot() {
    let asm = asm_for("10 sys 49152\n20 print \"x\"\n");
    assert!(
        !asm.contains("__DATA_CUR"),
        "nothing to point at, so no slot:\n{asm}"
    );
}

#[test]
fn a_sys_parks_the_cursor_in_absolute_bss() {
    let asm = asm_for("10 sys 49152\n20 read a: print a\n30 data 9\n");
    assert_eq!(
        asm.matches("\n__DATA_CUR:\n").count(),
        1,
        "exactly one definition of the slot:\n{asm}"
    );
    assert!(
        asm.contains("    STA __DATA_CUR\n") && asm.contains("    STA __DATA_CUR+1\n"),
        "__DATA_INIT must write the BSS slot:\n{asm}"
    );

    for gone in ["    STA $FB\n", "    LDA $FB\n", "    INC $FC\n"] {
        assert!(
            !asm.contains(gone),
            "no live cursor traffic through $FB/$FC should remain, found {gone:?}:\n{asm}"
        );
    }
}

#[test]
fn the_bss_cursor_is_never_an_indirect_base() {
    for src in [
        // BinaryI16 ; the layout with indirect reads.
        "10 sys 49152\n20 read a: print a\n30 data 9,8\n",
        // AsciiTagged ; indirect read plus indirect length re-read.
        "10 sys 49152\n20 read a$: print a$\n30 data hello\n",
    ] {
        let asm = asm_for(src);
        assert!(
            !asm.contains("(__DATA_CUR)"),
            "no indirect through the absolute slot:\n{asm}"
        );
        assert!(
            asm.contains("__DATA_CUR"),
            "...but the slot is still in use:\n{asm}"
        );
    }
}

#[test]
fn save_keeps_its_zero_page_start_pointer_slot() {
    let asm = asm_for("10 sys 49152\n20 save \"x\",8\n");
    assert!(
        asm.contains("    LDA #$FB\n"),
        "SAVE must still hand the KERNAL a zero-page slot number:\n{asm}"
    );
    assert!(
        asm.contains("    STA $FB\n") && asm.contains("    STA $FC\n"),
        "...and still fill it:\n{asm}"
    );
}

#[test]
fn safe_sys_calls_and_the_bss_cursor_compose() {
    let src = format!(
        "10 for i=0 to 10: read d: poke 49152+i,d: next\n\
         20 data {CLOBBER_ALL}\n\
         30 sys 49152\n\
         40 read a: read b: print a;b\n\
         50 data 31,32\n"
    );
    let prg = tokenize_program(&src).expect("tokenize");
    let opts = CompileOptions {
        safe_sys_calls: true,
        ..CompileOptions::default()
    };
    let out = compile_with_options(&prg, opts).expect("compile");
    assert!(out.asm.contains("__DATA_CUR"), "cursor still relocated");
    let r = emu64::run_prg_to_end(&out.prg_bytes, MAX_INSNS).expect("run");
    assert_eq!(r.error_code, None, "banner {:?}", r.error_banner());
    assert_eq!(common::petscii_to_string(&r.output), " 31  32 \n");
}
