//! `TAB()`, `SPC()`, `POS()` and PRINT's comma zones all read the KERNAL's cursor column, PNTR
//! ($D3).

mod common;

/// Compile, run to a clean exit, and return the decoded PRINT output.
fn out(src: &str) -> String {
    let prg = common::compile_source(src).unwrap_or_else(|e| panic!("compile {src:?}: {e}"));
    common::run_clean(&prg).unwrap_or_else(|| panic!("no clean exit: {src:?}"))
}

#[test]
fn tab_counts_from_the_current_column() {
    // ROM: X then seven spaces, Y at column 8.
    assert_eq!(out("10 PRINT \"X\";TAB(8);\"Y\"\n"), "X       Y\n");
    // From column 0 the count is the whole argument.
    assert_eq!(
        out("10 PRINT TAB(15);\"INDENTED\"\n"),
        "               INDENTED\n"
    );

    assert_eq!(
        out("10 PRINT \"COL0\";TAB(10);\"COL10\";TAB(20);\"COL20\"\n"),
        "COL0      COL10     COL20\n"
    );
}

#[test]
fn tab_backwards_is_a_no_op() {
    assert_eq!(
        out("10 PRINT \"0123456789\";TAB(5);\"Z\"\n"),
        "0123456789Z\n"
    );
}

/// A logical line that has wrapped onto a second screen row keeps counting columns to 79, so
/// `TAB(45)` after 40 characters is five columns, not a full 45.
#[test]
fn tab_spans_a_wrapped_logical_line() {
    assert_eq!(
        out("10 PRINT \"0123456789012345678901234567890123456789\";TAB(45);\"Z\"\n"),
        "0123456789012345678901234567890123456789     Z\n"
    );
}

#[test]
fn spc_is_still_relative() {
    assert_eq!(
        out("10 PRINT \"A\";SPC(5);\"B\";SPC(3);\"C\"\n"),
        "A     B   C\n"
    );
}

#[test]
fn pos_reports_the_cursor_column() {
    assert_eq!(out("10 PRINT \"ABC\";POS(0)\n"), "ABC 3 \n");
    assert_eq!(out("10 PRINT POS(0)\n"), " 0 \n");
}

/// PRINT's comma advances to the next ten-column zone.
#[test]
fn print_comma_pads_to_the_next_zone() {
    assert_eq!(
        out("10 PRINT \"A\",\"B\",\"C\"\n"),
        "A         B         C\n"
    );
    assert_eq!(
        out("10 PRINT \"I\",\"I*I\",\"I*I*I\"\n"),
        "I         I*I       I*I*I\n"
    );
}

#[test]
fn print_comma_pads_past_column_thirty_instead_of_breaking_the_line() {
    let thirty_one = "0".repeat(31);
    assert_eq!(
        out(&format!("10 PRINT \"{thirty_one}\",\"Z\"\n")),
        format!("{thirty_one}{}Z\n", " ".repeat(9))
    );
}

#[test]
fn print_comma_at_a_zone_boundary_advances_a_full_zone() {
    assert_eq!(
        out("10 PRINT \"0123456789\",\"X\"\n20 PRINT ,\"Y\"\n"),
        "0123456789          X\n          Y\n"
    );
}

#[test]
fn print_comma_keeps_padding_on_a_wrapped_logical_line() {
    let forty_one = "0".repeat(41);
    assert_eq!(
        out(&format!("10 PRINT \"{forty_one}\",\"Z\"\n")),
        format!("{forty_one}{}Z\n", " ".repeat(9))
    );
}

#[test]
fn print_comma_walks_successive_zones_from_column_zero() {
    assert_eq!(out("10 PRINT ,\"A\",\"B\"\n"), "          A         B\n");
}
