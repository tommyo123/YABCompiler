mod common;

/// Raw CHROUT bytes.
fn chrout(src: &str) -> Vec<u8> {
    let prg = common::compile_source(src).unwrap_or_else(|e| panic!("compile {src:?}: {e}"));
    let r = emu64::run_prg_to_end(&prg, 40_000_000).expect("runs");
    assert!(r.clean_exit, "no clean exit for:\n{src}");
    r.output
}

fn cursor_rights(src: &str) -> usize {
    chrout(src).iter().filter(|&&b| b == 0x1D).count()
}

/// Keep the gap count unknown until runtime so the helper is exercised.
fn prog(var: &str, poke: u16, xform: &str, prefix_len: usize, func: &str) -> String {
    let pre: String = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789ABCD".chars().cycle().take(prefix_len).collect();
    format!("10 POKE 2,{poke}\n20 {var}=PEEK(2){xform}\n30 PRINT \"{pre}\";{func}({var});\"Z\"\n40 END\n")
}

fn both(poke: u16, xform: &str, prefix_len: usize, func: &str, want: usize) {
    for var in ["A", "A%"] {
        let src = prog(var, poke, xform, prefix_len, func);
        assert_eq!(
            cursor_rights(&src), want,
            "{func}({var}) poke={poke}{xform} col={prefix_len}\n{src}"
        );
    }
}

#[test]
fn tab_from_column_zero_emits_the_whole_argument() {
    both(0, "", 0, "TAB", 0);
    both(5, "", 0, "TAB", 5);
    both(39, "", 0, "TAB", 39);
    both(40, "", 0, "TAB", 40);
}

/// TAB never moves the cursor backwards: at or past the target it emits nothing, and BASIC V2 has no way to express a negative pad.
#[test]
fn tab_below_or_at_the_current_column_emits_nothing() {
    both(3, "", 10, "TAB", 0);
    both(9, "", 10, "TAB", 0);
    both(10, "", 10, "TAB", 0);
    both(11, "", 10, "TAB", 1);
    both(20, "", 10, "TAB", 10);
}

#[test]
fn tab_measures_across_the_forty_column_wrap() {
    both(45, "", 40, "TAB", 5);
    both(39, "", 40, "TAB", 0);
    both(20, "", 45, "TAB", 0);
    both(79, "", 45, "TAB", 34);
    both(5, "", 79, "TAB", 0);
}

#[test]
fn a_fractional_argument_truncates_it_does_not_round() {
    both(17, "/2", 0, "TAB", 8);
    both(15, "/2", 0, "TAB", 7);
    both(35, "/4", 0, "TAB", 8);
    both(37, "/4", 0, "TAB", 9);
    both(81, "/2", 0, "TAB", 40);
    both(1, "/2", 0, "TAB", 0);
}

#[test]
fn spc_counts_from_here_not_from_column_zero() {
    both(5, "", 0, "SPC", 5);
    both(5, "", 10, "SPC", 5);
    both(5, "", 40, "SPC", 5);
    both(40, "", 0, "SPC", 40);
    both(17, "/2", 0, "SPC", 8);
}
