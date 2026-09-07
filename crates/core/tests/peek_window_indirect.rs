//! `PEEK`/`POKE` through a zero-page base at a constant displacement.

use yabcompiler_core::{CompileOptions, compile_with_options, tokenize_program};

mod common;

const MAX_INSNS: u64 = 60_000_000;

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

fn out(src: &str) -> String {
    let r = emu64::run_prg_to_end(&compile(src), MAX_INSNS).expect("run");
    assert_eq!(
        r.error_code, None,
        "unexpected BASIC error {:?}",
        r.error_banner()
    );
    common::petscii_to_string(&r.output)
}

/// A program that seeds nine bytes at `896..904`, points an integer at the middle of them, and reads the neighbourhood.
fn neighbourhood(body: &str) -> String {
    format!(
        "10 for i=0 to 8: poke 896+i,i*10+1: next i\n\
         20 read bb%\n\
         25 data 900\n\
         30 {body}\n"
    )
}

#[test]
fn positive_displacements_read_the_right_bytes_fac_path() {
    assert_eq!(
        out(&neighbourhood(
            "print peek(bb%);peek(bb%+1);peek(bb%+2);peek(bb%+3);peek(bb%+4)"
        )),
        " 41  51  61  71  81 \n"
    );
}

/// Same claim, int-island path. 41+51+61+71+81 = 305.
#[test]
fn positive_displacements_read_the_right_bytes_int_path() {
    assert_eq!(
        out(&neighbourhood(
            "c%=peek(bb%)+peek(bb%+1)+peek(bb%+2)+peek(bb%+3)+peek(bb%+4)\n\
             40 print c%"
        )),
        " 305 \n"
    );
}

/// The rebase tier, FAC path.
#[test]
fn negative_displacements_read_the_right_bytes_fac_path() {
    assert_eq!(
        out(&neighbourhood(
            "print peek(bb%-4);peek(bb%-3);peek(bb%-2);peek(bb%-1);peek(bb%)"
        )),
        " 1  11  21  31  41 \n"
    );
}

#[test]
fn negative_displacements_read_the_right_bytes_int_path() {
    assert_eq!(
        out(&neighbourhood(
            "c%=peek(bb%-4)+peek(bb%-3)+peek(bb%-2)+peek(bb%-1)+peek(bb%)\n\
             40 print c%"
        )),
        " 105 \n"
    );
}

/// Mix direct and rebased pointer reads in one expression.
#[test]
fn mixed_signs_in_one_expression_sum_correctly() {

    assert_eq!(
        out(&neighbourhood(
            "c%=peek(bb%-4)+peek(bb%-3)+peek(bb%-2)+peek(bb%-1)\
             +peek(bb%+1)+peek(bb%+2)+peek(bb%+3)+peek(bb%+4)\n\
             40 print c%"
        )),
        " 328 \n"
    );
}

#[test]
fn poke_displacement_survives_a_float_valued_right_hand_side() {
    assert_eq!(
        out("10 read bb%\n\
             15 data 896\n\
             20 v=7.9\n\
             30 poke bb%+5,v*10\n\
             40 print peek(901)\n"),
        " 79 \n"
    );
}

#[test]
fn a_span_wider_than_a_page_still_reads_correctly() {
    assert_eq!(
        out("10 poke 899,5: poke 1200,9\n\
             20 read bb%\n\
             25 data 900\n\
             30 c%=peek(bb%-1)+peek(bb%+300)\n\
             40 print c%\n"),
        " 14 \n"
    );
}

const ARRAY_GUARD_SRC: &str = "10 poke 896,1: poke 899,31\n\
     20 dim a%(40)\n\
     30 read bb%,j%\n\
     35 data 900,7\n\
     40 a%(j%)=100\n\
     50 c%=peek(bb%-4)+(a%(j%)and255)+peek(bb%-1)+peek(bb%-2)+peek(bb%-3)\n\
     60 print c%\n";

#[test]
fn array_subscript_in_the_same_expression_is_refused() {
    let asm = asm_for(ARRAY_GUARD_SRC);
    assert_base_is_in_zero_page(&asm);

    assert!(
        asm.contains("__PEEK_INT_"),
        "precondition: the sites must be on the int-island path:\n{asm}"
    );

    assert_eq!(out(ARRAY_GUARD_SRC), " 132 \n");
}

#[test]
fn usr_between_sites_is_refused() {
    // 169,255,133,253,133,254,96 = LDA #$FF / STA $FD / STA $FE / RTS
    assert_eq!(
        out("10 for i=0 to 8: poke 896+i,i*10+1: next i\n\
             20 for i=0 to 6: read d: poke 828+i,d: next i\n\
             30 data 169,255,133,253,133,254,96\n\
             40 poke 785,60: poke 786,3\n\
             50 read bb%\n\
             55 data 900\n\
             60 c%=peek(bb%-4)+usr(0)*0+peek(bb%-1)\n\
             70 print c%\n"),
        " 32 \n"
    );
}

#[test]
fn counterweight_displacement_tier_actually_fires() {
    let asm = asm_for(&neighbourhood(
        "c%=peek(bb%+1)+peek(bb%+2)+peek(bb%+3)+peek(bb%+40)",
    ));
    assert!(
        !asm.contains("__PEEK_INT_"),
        "every site should have become (zp),Y; asm still has an SMC operand:\n{asm}"
    );
    assert!(
        asm.contains("),Y"),
        "expected an indirect-Y load in:\n{asm}"
    );

    assert!(
        asm.contains("LDY #$28"),
        "expected LDY #$28 for the +40 site in:\n{asm}"
    );
}

/// The rebase tier fires: the pointer is materialised into `$FD/$FE` and the negative sites read through it.
#[test]
fn counterweight_rebase_tier_actually_fires() {
    let asm = asm_for(
        "10 poke 859,1: poke 899,31\n\
         20 read bb%\n\
         25 data 900\n\
         30 c%=peek(bb%-41)+peek(bb%-40)+peek(bb%-39)+peek(bb%-1)\n\
         40 print c%\n",
    );
    assert_base_is_in_zero_page(&asm);
    assert!(
        !asm.contains("__PEEK_INT_"),
        "every site should have become (zp),Y; asm still has an SMC operand:\n{asm}"
    );
    assert!(
        asm.contains("STA $FD") && asm.contains("STA $FE"),
        "expected the rebased pointer to be materialised in $FD/$FE:\n{asm}"
    );
    assert!(
        asm.contains("LDA ($FD),Y"),
        "expected reads through the rebased pointer:\n{asm}"
    );
    // Bottom of the span is -41 = $FFD7.
    assert!(
        asm.contains("ADC #$D7"),
        "expected the pointer to be rebased to bb-41:\n{asm}"
    );
}

fn assert_base_is_in_zero_page(asm: &str) {
    assert!(
        asm.contains("VI_BB = $") || asm.contains("ZSI_V_BB = $"),
        "fixture precondition failed: the base did not get a ZP slot, so no \
         indirect addressing is possible and nothing below is being tested:\n{asm}"
    );
}

#[test]
fn counterweight_a_lone_out_of_reach_site_does_not_open_a_window() {
    // Four in-reach sites earn the base its ZP slot; exactly one site (-1) is out of reach, which is one short of what a pointer costs.
    let asm = asm_for(&neighbourhood(
        "c%=peek(bb%-1)+peek(bb%+1)+peek(bb%+2)+peek(bb%+3)+peek(bb%+4)",
    ));
    assert_base_is_in_zero_page(&asm);
    assert!(
        !asm.contains("STA $FD"),
        "one out-of-reach site is not worth a pointer:\n{asm}"
    );

    assert!(
        asm.contains("__PEEK_INT_"),
        "the -1 site should have stayed on the SMC path:\n{asm}"
    );

    assert!(
        asm.contains("LDA (VI_BB),Y"),
        "the in-reach sites should use the base pair:\n{asm}"
    );
}

#[test]
fn counterweight_refusing_the_window_keeps_the_free_tier() {
    let asm = asm_for(
        "10 dim a%(3)\n\
         20 read bb%\n\
         25 data 900\n\
         30 c%=peek(bb%+1)+a%(1)+peek(bb%+2)+peek(bb%+3)+peek(bb%+4)\n",
    );
    assert_base_is_in_zero_page(&asm);

    assert_eq!(
        asm.matches("LDA (VI_BB),Y").count(),
        4,
        "all four in-reach sites must still be indirect-Y even though the \
         array reference refused the window:\n{asm}"
    );
}
