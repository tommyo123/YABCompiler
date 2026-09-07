mod common;

/// Compile, run to a clean exit, return the decoded PRINT output.
fn out(src: &str) -> String {
    let prg = common::compile_source(src).unwrap_or_else(|e| panic!("compile {src:?}: {e}"));
    common::run_clean(&prg).unwrap_or_else(|| panic!("no clean exit: {src:?}"))
}

fn probe(poke: u16, div: u16) -> String {
    format!("10 POKE 2,{poke}\n20 A=PEEK(2)/{div}\n30 POKE 2,A\n40 PRINT A\n50 END\n")
}

#[test]
fn print_after_poke_of_the_same_float_reads_the_float_not_the_qint_residue() {

    assert_eq!(out(&probe(11, 2)), " 5.5 \n", "FAC clobbered across __FAC_BYTE");
}

#[test]
fn the_clobber_hits_whole_numbers_too_not_just_fractions() {
    for (poke, div, want) in [(11u16, 2u16, " 5.5 \n"), (7, 2, " 3.5 \n"), (35, 4, " 8.75 \n"), (12, 2, " 6 \n")] {
        assert_eq!(out(&probe(poke, div)), want, "poke={poke} div={div}");
    }
}

#[test]
fn bare_getadr_alone_in_the_window_keeps_the_reload() {
    let src = "10 POKE 2,11\n20 POKE 8197,77\n30 B=8192+PEEK(2)/2\n40 POKE B,PEEK(B)\n50 PRINT PEEK(8197)\n";
    assert_eq!(out(src), " 77 \n", "second GETADR converted the first one's QINT residue");
}

#[test]
fn tab_fac_alone_in_the_window_keeps_the_reload() {
    let src = "10 POKE 2,11\n20 A=PEEK(2)/2\n30 PRINT TAB(A);\n40 PRINT A\n50 END\n";
    assert_eq!(out(src), "      5.5 \n", "FAC clobbered across __TAB_FAC");
}

/// Tripwire: `__SPC_FAC` is the only call between the store and the reload.
#[test]
fn spc_fac_alone_in_the_window_keeps_the_reload() {
    let src = "10 POKE 2,11\n20 A=PEEK(2)/2\n30 PRINT SPC(A);\n40 PRINT A\n50 END\n";
    assert_eq!(out(src), "      5.5 \n", "FAC clobbered across __SPC_FAC");
}

#[test]
fn a_literal_print_between_the_store_and_the_reload_is_not_a_hazard() {
    let src = "10 POKE 2,11\n20 A=PEEK(2)/2\n30 PRINT \"ZQWERTY\";\n40 PRINT A\n50 END\n";
    assert_eq!(out(src), "ZQWERTY 5.5 \n");
}

#[test]
fn tab_then_literal_is_the_shape_that_hid_the_mutation() {
    let src = "10 POKE 2,11\n20 A=PEEK(2)/2\n30 PRINT TAB(A);\"Z\"\n40 PRINT A\n50 END\n";
    assert_eq!(out(src), "     Z\n 5.5 \n");
}
