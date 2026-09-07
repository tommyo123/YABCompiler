//! Pass trailing SYS text to the machine-code callee.

use yabcompiler_core::{CompileOptions, compile_with_options, tokenize_program};

mod common;

const MAX_INSNS: u64 = 40_000_000;

fn opts(sys_register_args: bool) -> CompileOptions {
    CompileOptions {
        sys_register_args,
        ..CompileOptions::default()
    }
}

fn compile_with(src: &str, sys_register_args: bool) -> Vec<u8> {
    let prg = tokenize_program(src).expect("tokenize");
    compile_with_options(&prg, opts(sys_register_args))
        .expect("compile")
        .prg_bytes
}

fn asm_with(src: &str, sys_register_args: bool) -> String {
    let prg = tokenize_program(src).expect("tokenize");
    compile_with_options(&prg, opts(sys_register_args))
        .expect("compile")
        .asm
}

fn run_with(src: &str, sys_register_args: bool) -> emu64::RunToEnd {
    emu64::run_prg_to_end(&compile_with(src, sys_register_args), MAX_INSNS).expect("run")
}

fn run(src: &str) -> emu64::RunToEnd {
    run_with(src, false)
}

/// Assert the program prints `out` and stops with no error banner.
fn check_ok(src: &str, out: &str) {
    let r = run(src);
    assert_eq!(r.error_banner(), None, "unexpected banner:\n{src}");
    assert_eq!(common::petscii_to_string(&r.output).trim_end(), out, "{src}");
}

/// Assert the program dies with `banner`, having printed `out` first.
fn check_error(src: &str, out: &str, banner: &str) {
    let r = run(src);
    assert_eq!(r.error_banner().as_deref(), Some(banner), "{src}");
    assert_eq!(
        common::petscii_to_string(&r.output).trim_end(),
        out,
        "output before the banner:\n{src}"
    );
}

const CHKCOM_GETBYT: &str = "32,253,174,32,158,183,142,60,3,96";

/// `STA $033C : STX $033D : STY $033E : RTS` ; parses nothing, and records whatever the ROM handed it in A/X/Y.
const RECORD_AXY: &str = "141,60,3,142,61,3,140,62,3,96";

/// `JSR CHKCOM : JSR FRMEVL ($AD9E) : JSR FRESTR ($B6A3) : STA $033C : RTS` ; evaluates a string argument off TXTPTR and records its length.
const FRMEVL_STR: &str = "32,253,174,32,158,173,32,163,182,141,60,3,96";

/// Load `ml` (a DATA-style byte list) at $C000.
fn loader(ml: &str) -> String {
    let n = ml.split(',').count() - 1;
    format!("10 for i=0 to {n}: read a: poke 49152+i,a: next\n20 data {ml}\n")
}

#[test]
fn a_comma_argument_is_parsed_by_the_callee() {
    check_ok(
        &format!("{}30 poke 828,0\n40 sys 49152,15\n50 print\"got\";peek(828)\n", loader(CHKCOM_GETBYT)),
        "GOT 15",
    );
}

/// A string after the comma is text too.
#[test]
fn a_string_argument_reaches_frmevl_instead_of_type_mismatching() {
    check_ok(
        &format!("{}30 poke 828,0\n40 sys 49152,\"hello\"\n50 print\"len\";peek(828)\n", loader(FRMEVL_STR)),
        "LEN 5",
    );
}

#[test]
fn every_argument_the_callee_consumes_leaves_a_clean_statement() {

    let ml = "32,253,174,32,158,183,142,60,3,32,253,174,32,158,183,142,61,3,96";
    check_ok(
        &format!("{}30 sys 49152,3,4\n40 print\"ab\";peek(828);peek(829)\n", loader(ml)),
        "AB 3  4",
    );
}

#[test]
fn the_statement_after_a_param_sys_on_the_same_line_still_runs() {
    check_ok(
        &format!("{}30 sys 49152,15: print\"next\";peek(828)\n", loader(CHKCOM_GETBYT)),
        "NEXT 15",
    );
}

#[test]
fn two_param_sys_calls_do_not_share_a_buffer() {
    check_ok(
        &format!(
            "{}30 sys 49152,7\n40 print\"a\";peek(828)\n50 sys 49152,9\n60 print\"b\";peek(828)\n",
            loader(CHKCOM_GETBYT)
        ),
        "A 7 \nB 9",
    );
}

/// A `:` inside a string literal is content, not the end of the statement.
#[test]
fn a_colon_inside_a_string_argument_does_not_end_the_statement() {
    check_ok(
        &format!("{}30 poke 828,0\n40 sys 49152,\"a:b\"\n50 print\"len\";peek(828)\n", loader(FRMEVL_STR)),
        "LEN 3",
    );
}

#[test]
fn the_unquoted_colon_after_a_string_argument_still_ends_it() {
    check_ok(
        &format!(
            "{}30 poke 828,0\n40 sys 49152,\"a:b\": print\"len\";peek(828)\n",
            loader(FRMEVL_STR)
        ),
        "LEN 3",
    );
}

#[test]
fn a_space_before_the_separator_is_not_leftover_text() {
    check_ok(
        &format!("{}30 sys 49152,15 : print\"got\";peek(828)\n", loader(CHKCOM_GETBYT)),
        "GOT 15",
    );
}

/// `;` is not a statement separator after `SYS`.
#[test]
fn a_semicolon_after_sys_is_callee_text_not_a_separator() {
    check_error(
        &format!("{}30 poke 828,0\n40 sys 49152,15;99\n50 print\"got\";peek(828)\n", loader(CHKCOM_GETBYT)),
        "",
        "?SYNTAX  ERROR IN 40",
    );
    check_error(
        "10 poke 49152,96\n20 sys 49152;\n30 print\"after\"\n",
        "",
        "?SYNTAX  ERROR IN 20",
    );
}

#[test]
fn poke_780_781_782_still_load_a_x_and_y() {
    check_ok(
        &format!(
            "{}30 poke 780,11: poke 781,22: poke 782,33\n40 sys 49152\n50 print\"r\";peek(828);peek(829);peek(830)\n",
            loader(RECORD_AXY)
        ),
        "R 11  22  33",
    );
}

#[test]
fn a_callee_that_consumes_nothing_still_raises_syntax_error() {
    check_error(
        "10 poke 49152,96\n20 sys 49152,15\n30 print\"after\"\n",
        "",
        "?SYNTAX  ERROR IN 20",
    );
}

#[test]
fn text_the_callee_leaves_behind_raises_syntax_error() {
    check_error(
        &format!("{}30 poke 828,0\n40 sys 49152,15,99\n50 print\"got\";peek(828)\n", loader(CHKCOM_GETBYT)),
        "",
        "?SYNTAX  ERROR IN 40",
    );
}

#[test]
fn the_check_cites_the_sys_line_and_kills_the_rest() {
    check_error(
        "10 print\"a\"\n20 poke 49152,96\n30 sys 49152,1: print\"b\"\n40 print\"c\"\n",
        "A",
        "?SYNTAX  ERROR IN 30",
    );
}

#[test]
fn the_register_form_is_a_syntax_error_under_the_default_dialect() {
    check_error(
        &format!(
            "{}30 sys 49152,11,22,33\n40 print\"r\";peek(828);peek(829);peek(830)\n",
            loader(RECORD_AXY)
        ),
        "",
        "?SYNTAX  ERROR IN 30",
    );
}

#[test]
fn the_register_form_still_works_behind_its_flag() {
    let src = format!(
        "{}30 sys 49152,11,22,33\n40 print\"r\";peek(828);peek(829);peek(830)\n",
        loader(RECORD_AXY)
    );
    let r = run_with(&src, true);
    assert_eq!(r.error_banner(), None, "unexpected banner");
    assert_eq!(
        common::petscii_to_string(&r.output).trim_end(),
        "R 11  22  33"
    );
}

#[test]
fn the_flag_switches_between_two_disjoint_codegen_shapes() {
    let src = "10 sys 49152,11,22,33\n";
    let before_call = |asm: &str| -> String {
        asm.split("; YAB_SYS_WRAP_IN")
            .next()
            .expect("the SYS wrap marker is emitted")
            .to_string()
    };

    let ext = asm_with(src, true);
    let ext_pre = before_call(&ext);
    assert!(ext_pre.contains("STA $030C"), "A store expected:\n{ext}");
    assert!(ext_pre.contains("STA $030D"), "X store expected:\n{ext}");
    assert!(ext_pre.contains("STA $030E"), "Y store expected:\n{ext}");
    assert!(
        !ext.contains("__SYS_PARAMS"),
        "no TXTPTR buffer under the flag:\n{ext}"
    );

    let v2 = asm_with(src, false);
    assert!(
        v2.contains("__SYS_PARAMS"),
        "the arguments are text in v2:\n{v2}"
    );
    let v2_pre = before_call(&v2);
    // The A/X/Y loads before the JSR are the ROM's own ($E12A reads $030C-$030E unconditionally); it is the pre-JSR STOREs that mark the extension.
    assert!(
        !v2_pre.contains("STA $030C")
            && !v2_pre.contains("STA $030D")
            && !v2_pre.contains("STA $030E"),
        "v2 must not stamp the save area before the call:\n{v2}"
    );
    assert!(
        v2_pre.contains("LDA $030C") && v2_pre.contains("LDX $030D"),
        "…but it must still honour what POKE 780/781 left there:\n{v2}"
    );
}

#[test]
fn a_bare_sys_stages_no_buffer_and_checks_nothing() {
    for src in [
        "10 sys 49152\n",
        "10 a=49152: sys a\n",
        "10 sys 49152: print\"x\"\n",
        "10 sys 49152 \n",
    ] {
        let asm = asm_with(src, false);
        assert!(
            !asm.contains("__SYS_PARAMS"),
            "no trailing text, so no buffer:\n{src}\n{asm}"
        );
    }
}

#[test]
fn a_bare_sys_program_is_identical_with_and_without_the_flag() {
    let src = "10 poke 49152,96\n20 sys 49152\n30 print\"ok\"\n";
    assert_eq!(compile_with(src, false), compile_with(src, true));
    check_ok(src, "OK");
}

#[test]
fn the_shared_tail_appears_only_once_it_pays_for_itself() {
    let one = asm_with("10 sys 49152,1\n", false);
    let two = asm_with("10 sys 49152,1\n20 sys 49152,2\n", false);
    let three = asm_with("10 sys 49152,1\n20 sys 49152,2\n30 sys 49152,3\n", false);
    assert!(!one.contains("__SYS_TXT_OUT"), "1 site is inline:\n{one}");
    assert!(!two.contains("__SYS_TXT_OUT"), "2 sites are inline:\n{two}");
    assert!(
        three.contains("__SYS_TXT_OUT:"),
        "3 sites share a tail:\n{three}"
    );

    assert!(
        !three.contains("__SYS_PARAMS_OK_"),
        "the inline check should not survive alongside the helper:\n{three}"
    );
}

/// A program past the threshold still parses its arguments, still restores TXTPTR, and still runs the statements after each `SYS`.
#[test]
fn many_param_sys_calls_all_work_through_the_shared_tail() {
    let src = format!(
        "{}30 sys 49152,1: print\"a\";peek(828)\n\
         40 sys 49152,2: print\"b\";peek(828)\n\
         50 sys 49152,3: print\"c\";peek(828)\n\
         60 sys 49152,4: print\"d\";peek(828)\n\
         70 print\"end\"\n",
        loader(CHKCOM_GETBYT)
    );
    assert!(asm_with(&src, false).contains("__SYS_TXT_OUT:"));
    check_ok(&src, "A 1 \nB 2 \nC 3 \nD 4 \nEND");
}

#[test]
fn the_shared_tail_still_raises_on_unconsumed_text() {
    let src = format!(
        "{}30 sys 49152,1\n40 sys 49152,2\n50 sys 49152,3,99\n60 print\"end\"\n",
        loader(CHKCOM_GETBYT)
    );
    assert!(asm_with(&src, false).contains("__SYS_TXT_OUT:"));
    check_error(&src, "", "?SYNTAX  ERROR IN 50");
}

#[test]
fn the_inline_and_shared_shapes_agree() {
    let body = "30 sys 49152,5: print\"x\";peek(828)\n40 sys 49152,6: print\"y\";peek(828)\n";
    let inline = format!("{}{body}", loader(CHKCOM_GETBYT));
    let shared = format!("{}{body}50 sys 49152,7: print\"z\";peek(828)\n", loader(CHKCOM_GETBYT));
    assert!(!asm_with(&inline, false).contains("__SYS_TXT_OUT"));
    assert!(asm_with(&shared, false).contains("__SYS_TXT_OUT:"));
    check_ok(&inline, "X 5 \nY 6");
    check_ok(&shared, "X 5 \nY 6 \nZ 7");
}

#[test]
fn the_wedge_runs_and_the_data_pointer_survives_it() {

    let ml = "32,241,183,142,60,3,169,255,133,251,133,252,133,253,133,254,96";
    check_ok(
        &format!(
            "{}25 read p\n30 sys 49152,15\n40 read q\n50 print\"w\";peek(828);p;q\n60 data 61,62\n",
            loader(ml)
        ),
        "W 15  61  62",
    );
}

const RETURN_AXY: &str = "169,17,162,34,160,51,96";

/// `TSX : STX $033C : RTS` ; records the stack pointer the callee is entered with.
const RECORD_SP: &str = "186,142,60,3,96";

const RECORD_CARRY: &str = "169,0,144,2,169,1,141,60,3,96";

/// `SEC : RTS` ; sets a flag the return path has to bring back into `$030F`.
const SET_CARRY: &str = "56,96";

/// Two or more sites take the subroutines; one stays inline.
#[test]
fn the_register_helper_appears_only_once_it_pays_for_itself() {
    let one = asm_with("10 sys 49152\n", false);
    let two = asm_with("10 sys 49152\n20 sys 49152\n", false);
    assert!(
        !one.contains("__SYS_REGS_IN"),
        "1 site is inline:\n{one}"
    );
    assert!(
        two.contains("__SYS_REGS_IN:") && two.contains("__SYS_REGS_OUT:"),
        "2 sites share the handoff:\n{two}"
    );

    let after_helper = two.split("__SYS_REGS_IN:").next().unwrap().to_string();
    assert!(
        !after_helper.contains("LDA $030F"),
        "the inline handoff should not survive alongside the helper:\n{two}"
    );
}

#[test]
fn the_register_helper_carries_a_x_y_both_ways() {

    let src = format!(
        "{}30 poke780,7:poke781,8:poke782,9\n\
         40 sys 49152\n\
         50 print\"in\";peek(828);peek(829);peek(830)\n\
         60 sys 49152\n\
         70 print\"out\";peek(780);peek(781);peek(782)\n",
        loader(RECORD_AXY)
    );
    assert!(asm_with(&src, false).contains("__SYS_REGS_IN:"));
    check_ok(&src, "IN 7  8  9 \nOUT 7  8  9");

    let src2 = format!(
        "{}30 sys 49152\n40 sys 49152\n50 print\"r\";peek(780);peek(781);peek(782)\n",
        loader(RETURN_AXY)
    );
    assert!(asm_with(&src2, false).contains("__SYS_REGS_OUT:"));
    check_ok(&src2, "R 17  34  51");
}

#[test]
fn the_register_helper_carries_the_status_register_both_ways() {
    let carry_in = |v: u8| {
        format!(
            "{}30 poke 783,{v}\n40 sys 49152\n50 sys 49152\n60 print\"c\";peek(828)\n",
            loader(RECORD_CARRY)
        )
    };
    assert!(asm_with(&carry_in(1), false).contains("__SYS_REGS_IN:"));
    check_ok(&carry_in(1), "C 1");
    check_ok(&carry_in(0), "C 0");

    let src = format!(
        "{}30 poke 783,0\n40 sys 49152\n50 sys 49152\n60 print\"s\";peek(783)and1\n",
        loader(SET_CARRY)
    );
    assert!(asm_with(&src, false).contains("__SYS_REGS_OUT:"));
    check_ok(&src, "S 1");
}

#[test]
fn the_register_helper_does_not_change_the_callees_stack_depth() {
    let inline = format!(
        "{}30 sys 49152\n40 print\"sp\";peek(828)\n",
        loader(RECORD_SP)
    );
    let shared = format!(
        "{}30 sys 49152\n35 sys 49152\n40 print\"sp\";peek(828)\n",
        loader(RECORD_SP)
    );
    assert!(!asm_with(&inline, false).contains("__SYS_REGS_IN"));
    assert!(asm_with(&shared, false).contains("__SYS_REGS_IN:"));

    let a = common::petscii_to_string(&run(&inline).output);
    let b = common::petscii_to_string(&run(&shared).output);
    assert_eq!(
        a.trim_end(),
        b.trim_end(),
        "the callee saw a different stack pointer once the handoff moved into a \
         subroutine — inline {a:?}, shared {b:?}"
    );
}

#[test]
fn the_inline_and_shared_register_shapes_agree() {
    let body = "30 poke780,3:sys 49152:print\"a\";peek(828);peek(829);peek(830)\n";
    let inline = format!("{}{body}", loader(RECORD_AXY));
    let shared = format!("{}{body}40 sys 49152\n", loader(RECORD_AXY));
    assert!(!asm_with(&inline, false).contains("__SYS_REGS_IN"));
    assert!(asm_with(&shared, false).contains("__SYS_REGS_IN:"));
    let a = common::petscii_to_string(&run(&inline).output);
    let b = common::petscii_to_string(&run(&shared).output);
    assert_eq!(a.trim_end(), b.trim_end());
    assert_eq!(a.trim_end(), "A 3  0  0");
}

#[test]
fn the_register_and_text_helpers_compose() {
    let src = format!(
        "{}30 sys 49152,1: print\"a\";peek(828)\n\
         40 sys 49152,2: print\"b\";peek(828)\n\
         50 sys 49152,3: print\"c\";peek(828)\n\
         60 print\"end\"\n",
        loader(CHKCOM_GETBYT)
    );
    let asm = asm_with(&src, false);
    assert!(asm.contains("__SYS_REGS_IN:") && asm.contains("__SYS_TXT_OUT:"));
    check_ok(&src, "A 1 \nB 2 \nC 3 \nEND");
}

#[test]
fn a_sys_call_is_never_shared_with_the_return_helper() {
    // Six run-time-target sites ; well past the factorer's threshold of four ; each followed by the register-return helper.
    let mut src = String::from("10 z=828\n");
    for i in 0..6 {
        src.push_str(&format!("{} sys z\n", 20 + i * 10));
    }
    let asm = asm_with(&src, false);
    assert!(asm.contains("__SYS_REGS_OUT:"), "the helper is in use:\n{asm}");

    for stub in asm.split("__JSR2_") {
        assert!(
            !stub.starts_with(|c: char| c.is_ascii_digit()) || !stub.contains("JSR $FFFF"),
            "a patched SYS call was factored into a shared stub:\n{asm}"
        );
    }
    assert!(
        !asm.contains("JSR $FFFF\n    JMP __SYS_REGS_OUT"),
        "the SYS call and the return helper were tail-call chained:\n{asm}"
    );
}
