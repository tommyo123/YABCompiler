//! A variable inside `SYS addr,<text>` must reach the callee with its value, not with zero.

mod common;

use yabcompiler_core::{CompileOptions, compile_with_options, tokenize_program};

fn asm(src: &str) -> String {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(&prg, CompileOptions::default())
        .expect("compiles")
        .asm
}

fn diag(src: &str) -> yabcompiler_core::Diagnostics {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(&prg, CompileOptions::default())
        .expect("compiles")
        .diagnostics
}

/// The staging buffer only exists on programs that substitute.
fn builds_text(src: &str) -> bool {
    asm(src).contains("__SYS_TXTBUF")
}

#[test]
fn a_sys_with_no_parameters_is_untouched() {
    let a = asm("10 SYS 49152\n");
    assert!(!a.contains("__SYS_TXTBUF"), "{a}");
    assert!(!a.contains("__SYS_PARAMS"), "{a}");
}

#[test]
fn constant_parameters_still_stage_the_raw_bytes() {
    for src in [
        "10 SYS 828,15\n",
        "10 SYS 828,1,24,\"LOST 2.00 0.60\"\n",
        "10 SYS 49152,\"HELLO\"\n",
        "10 SYS 828,2+1\n",
        "10 SYS 828,1E3\n",
    ] {
        let a = asm(src);
        assert!(
            a.contains("__SYS_PARAMS_"),
            "the verbatim blob should still be there for {src:?}"
        );
        assert!(
            !a.contains("__SYS_TXTBUF"),
            "nothing to substitute, so no build for {src:?}:\n{a}"
        );
    }
}

#[test]
fn a_variable_outside_the_parameter_text_is_not_a_parameter_variable() {
    // X is live and printed, but the SYS text names no variable.
    let src = "10 X=3\n20 SYS 828,3\n30 PRINT X\n";
    assert!(!builds_text(src), "{}", asm(src));
    assert!(diag(src).sys_param_vars.is_empty());
}

#[test]
fn letters_inside_the_quoted_part_are_content() {
    let src = "10 SYS 828,1,24,\"BLOST 2.00 0.60B\"\n";
    assert!(diag(src).sys_param_vars.is_empty());
    assert!(!builds_text(src), "{}", asm(src));
}

#[test]
fn a_scalar_in_the_parameter_text_is_rendered_at_run_time() {
    let a = asm("10 X=3\n20 SYS 828,X\n");
    assert!(a.contains("__SYS_TXTBUF"), "{a}");
    // Rendered through the ROM's own number formatter.
    assert!(a.contains("JSR $BDDD"), "{a}");
    assert!(a.contains("JSR __SYS_PB_VAL"), "{a}");

    assert!(a.contains("LDA #<__SYS_TXTBUF"), "{a}");
    assert!(a.contains("LDA #>__SYS_TXTBUF"), "{a}");
}

#[test]
fn a_constant_value_is_folded_into_the_render() {
    // Carrying the reference as an `Expr` means constant propagation reaches it.
    let a = asm("10 X=3\n20 SYS 828,X\n");
    assert!(a.contains("__SYS_TXTBUF"), "{a}");
    assert!(a.contains("LDY #$03"), "the folded 3 must reach FAC:\n{a}");
    assert!(a.contains("JSR $BDDD"), "{a}");
}

#[test]
fn the_store_to_a_computed_variable_survives() {
    // The whole reason `param_vars` is in the IR.
    let a = asm("10 X=PEEK(53280)\n20 SYS 828,X\n");

    let slot = ["VI_X", "V_X"]
        .into_iter()
        .find(|s| a.lines().any(|l| l.trim().starts_with(&format!("STA {s}"))))
        .unwrap_or_else(|| panic!("no store to X survived:\n{a}"));
    assert!(
        a.lines()
            .any(|l| l.trim().ends_with(slot) && l.trim().starts_with("LD")),
        "the SYS site must read {slot} back:\n{a}"
    );
}

#[test]
fn the_expression_shape_is_the_drop_the_lot_line() {

    let a = asm("10 X=3\n20 SYS 49177,1,X-1,\"AB\"\n");
    assert!(a.contains("__SYS_TXTBUF"), "{a}");
    assert!(a.contains("STA __SYS_TXTBUF,X"), "{a}");
    // Two literal runs: `,1,` before and `-1,"AB"` after.
    let runs = a.matches("__SYS_PL_").count();
    assert!(runs >= 2, "expected a run on each side, got {runs}:\n{a}");
}

#[test]
fn every_scalar_in_one_site_is_substituted() {
    let d = diag("10 X=3:Y=7\n20 SYS 828,X,Y\n");
    assert_eq!(d.sys_param_vars.len(), 2);
    assert_eq!(d.sys_param_vars[0].var.base, "X");
    assert_eq!(d.sys_param_vars[1].var.base, "Y");
    assert!(d.sys_param_vars.iter().all(|s| s.var.is_plain_numeric_scalar()));
    assert_eq!(d.sys_param_vars[0].line, 20);
}

#[test]
fn integer_variables_are_substituted_too() {

    let src = "10 DD%=5\n20 SYS 828,DD%\n";
    let d = diag(src);
    assert_eq!(d.sys_param_vars.len(), 1);
    assert_eq!(d.sys_param_vars[0].var.base, "DD");
    assert!(builds_text(src), "{}", asm(src));
}

#[test]
fn string_variables_and_string_arrays_are_reported_as_lazy_reads() {
    let src = "10 DIM R$(3)\n20 A$=\"Z\"\n30 SYS 828,R$(1),A$\n";
    let d = diag(src);
    assert_eq!(d.sys_param_vars.len(), 2);
    assert!(
        d.sys_param_vars.iter().all(|s| s.substituted),
        "a string value is supplied by the lazy descriptor bridge"
    );
    let shapes: Vec<_> = d
        .sys_param_vars
        .iter()
        .map(|s| (s.var.subscripted, s.var.kind))
        .collect();
    assert_eq!(shapes[0], (true, yabcompiler_core::ast::VarKind::String));
    assert_eq!(shapes[1], (false, yabcompiler_core::ast::VarKind::String));
    assert!(builds_text(src), "{}", asm(src));
    assert!(asm(src).contains("__SYS_EVAL_ENTER:"));
}

#[test]
fn an_fn_call_in_the_parameter_text_is_reported_but_not_substituted() {
    let src = "10 DEF FN F(A)=A+1\n20 SYS 828,FN F(2)\n";
    let d = diag(src);
    assert_eq!(d.sys_param_vars.len(), 1);
    assert!(d.sys_param_vars[0].var.is_fn);
    assert!(!d.sys_param_vars[0].substituted);
    assert!(!builds_text(src), "{}", asm(src));
}

#[test]
fn a_numeric_array_element_is_rendered_at_run_time() {
    let src = "10 DIM P(9)\n20 P(1)=7\n30 SYS 828,P(1)\n";
    let d = diag(src);
    assert_eq!(d.sys_param_vars.len(), 1);
    assert!(d.sys_param_vars[0].var.subscripted);
    assert!(d.sys_param_vars[0].substituted);
    let a = asm(src);
    assert!(a.contains("__SYS_TXTBUF"), "{a}");
    assert!(a.contains("JSR $BDDD"), "rendered through FOUT:\n{a}");
}

#[test]
fn the_whole_subscript_text_is_replaced_not_just_the_name() {

    let src = "10 DIM P(9)\n20 P(1)=7\n30 SYS 828,P(1),9\n";
    let a = asm(src);
    let end: Vec<_> = a
        .lines()
        .filter_map(|l| l.trim().strip_prefix("CPY #$"))
        .collect();
    let start: Vec<_> = a
        .lines()
        .filter_map(|l| l.trim().strip_prefix("LDY #$"))
        .collect();
    // `,P(1),9` ; run 0 is byte 0..1 (`,`), run 1 is 5..7 (`,9`).
    assert!(start.contains(&"00"), "first run starts at 0:\n{a}");
    assert!(end.contains(&"01"), "first run ends before `P`:\n{a}");
    assert!(start.contains(&"05"), "second run resumes after `)`:\n{a}");
    assert!(end.contains(&"07"), "second run ends at the text end:\n{a}");
}

#[test]
fn a_variable_subscript_is_not_substituted_a_second_time() {

    let src = "10 DIM P(9)\n20 I=1:P(1)=7\n30 SYS 828,P(I)\n";
    let d = diag(src);
    assert_eq!(d.sys_param_vars.len(), 2, "both references are reported");
    assert!(d.sys_param_vars.iter().all(|s| s.substituted));
    let a = asm(src);
    // One value render, not two.
    assert_eq!(
        a.matches("JSR __SYS_PB_VAL").count(),
        1,
        "the nested `I` must not be rendered on its own:\n{a}"
    );
}

#[test]
fn multi_dimensional_and_integer_arrays_are_substituted() {
    for src in [
        "10 DIM B(3,3)\n20 B(1,2)=5\n30 SYS 828,B(1,2)\n",
        "10 DIM A%(5)\n20 A%(2)=6\n30 SYS 828,A%(2)\n",
    ] {
        let d = diag(src);
        assert!(
            d.sys_param_vars.iter().any(|s| s.substituted),
            "nothing substituted for {src:?}"
        );
        assert!(builds_text(src), "{src:?}:\n{}", asm(src));
    }
}

#[test]
fn the_store_to_an_array_element_survives() {
    // The array analogue of the scalar case: the SYS text is the only reader, so if the reference is not in the IR the store is dead.
    let src = "10 DIM P(9)\n20 P(1)=PEEK(53280)\n30 SYS 828,P(1)\n";
    let a = asm(src);
    assert!(a.contains("__SYS_TXTBUF"), "{a}");
    assert!(
        a.contains("JSR $BDDD"),
        "the element must be read back and rendered:\n{a}"
    );
}

#[test]
fn a_site_mixing_shapes_stages_numbers_and_registers_lazy_strings() {
    let src = "10 DIM P(9),R$(3)\n20 X=4\n30 SYS 828,X,P(1),R$(1)\n";
    let d = diag(src);
    assert_eq!(d.sys_param_vars.len(), 3);
    assert_eq!(d.sys_param_vars.iter().filter(|s| s.substituted).count(), 3);
    assert!(
        d.sys_param_vars.iter().last().unwrap().substituted,
        "the string array has a lazy descriptor"
    );
    assert!(builds_text(src), "{}", asm(src));
}

#[test]
fn an_ordinary_program_does_not_report_a_dropped_substitution() {

    assert!(!diag("10 X=3\n20 SYS 828,X\n").sys_param_substitution_dropped);
    assert!(!diag("10 SYS 828,3\n").sys_param_substitution_dropped);
}

#[test]
fn a_parameter_string_too_long_to_stage_keeps_the_verbatim_path() {

    let long = "9".repeat(240);
    let src = format!("10 X=3\n20 SYS 828,X,{long}\n");
    let a = asm(&src);
    assert!(
        !a.contains("__SYS_TXTBUF"),
        "an over-long site must decline to build text:\n{a}"
    );
    assert!(a.contains("__SYS_PARAMS_"), "{a}");
}

fn pb_val(a: &str) -> String {
    let start = a.find("__SYS_PB_VAL:").expect("helper emitted");
    let rest = &a[start..];
    let end = rest.find("__SYS_PB_VAL_D:").expect("helper terminates");
    rest[..end].to_string()
}

#[test]
fn the_appended_value_carries_the_minus_token_not_ascii() {
    let body = pb_val(&asm("10 X=0-1\n20 SYS 828,X\n"));
    assert!(
        body.contains("CMP #$2D") && body.contains("LDA #$AB"),
        "FOUT's ASCII minus must become the BASIC minus token:\n{body}"
    );
}

#[test]
fn the_minus_is_translated_before_it_is_stored() {

    let body = pb_val(&asm("10 X=0-1\n20 SYS 828,X\n"));
    let cmp = body.find("CMP #$2D").expect("translation present");
    let sta = body.find("STA __SYS_TXTBUF,X").expect("store present");
    assert!(
        cmp < sta,
        "the sign must be mapped before the store, not after:\n{body}"
    );
}

#[test]
fn the_nul_terminator_still_ends_the_copy_first() {

    let body = pb_val(&asm("10 X=0-1\n20 SYS 828,X\n"));
    let beq = body.find("BEQ __SYS_PB_VAL_D").expect("terminator check");
    let cmp = body.find("CMP #$2D").expect("translation present");
    assert!(beq < cmp, "$00 must end the copy before any test:\n{body}");
}

#[test]
fn only_the_minus_is_translated() {

    let body = pb_val(&asm("10 X=0-1\n20 SYS 828,X\n"));
    assert_eq!(
        body.matches("CMP #").count(),
        1,
        "exactly one byte value is translated:\n{body}"
    );
    for dead in ["CMP #$2B", "LDA #$AA", "CMP #$45", "CMP #$2E", "CMP #$20"] {
        assert!(!body.contains(dead), "{dead} is not needed:\n{body}");
    }
}

#[test]
fn a_minus_inside_the_quoted_template_is_copied_verbatim() {

    let a = asm("10 X=3\n20 SYS 828,\"A-B\",X\n");
    let blob = a
        .lines()
        .find(|l| l.trim_start().starts_with(".byte"))
        .expect("params blob");
    assert!(
        blob.contains("$2D"),
        "the quoted minus must survive into the blob: {blob}"
    );
    let start = a.find("__SYS_PL_").expect("literal run emitted");
    let run = &a[start..];
    let upto_store = &run[..run.find("STA __SYS_TXTBUF,X").expect("store")];
    assert!(
        !upto_store.contains("CMP #"),
        "the literal run must copy verbatim:\n{upto_store}"
    );
}

const ML_ECHO: &str = "32,253,174,32,158,173,32,221,189,32,30,171,96";

fn run_echo(setup: &str, param: &str) -> String {
    let src = format!(
        "10 FORI=0TO12:READB:POKE49152+I,B:NEXT\n\
         20 {setup}\n\
         30 SYS49152,{param}\n\
         40 END\n\
         50 DATA{ML_ECHO}\n"
    );
    let prg = tokenize_program(&src).expect("tokenizes");
    let compiled = compile_with_options(&prg, CompileOptions::default()).expect("compiles");
    let r = emu64::run_prg_to_end(&compiled.prg_bytes, 20_000_000).expect("runs");
    common::petscii_to_string(&r.output)
}

#[test]
fn a_negative_value_reaches_the_callee_as_a_number() {

    let out = run_echo("X=0-1", "X");
    assert!(out.contains("-1"), "callee must parse -1, got {out:?}");
}

#[test]
fn a_positive_value_is_unchanged_by_the_translation() {

    let out = run_echo("Y=2", "Y");
    assert!(out.contains("2"), "callee must parse 2, got {out:?}");
    assert!(!out.contains("-"), "no sign should appear, got {out:?}");
}

#[test]
fn a_negative_constant_in_the_template_was_always_right() {

    let a = asm("10 SYS 49152,-1\n");
    let blob = a
        .lines()
        .find(|l| l.trim_start().starts_with(".byte"))
        .expect("params blob");
    assert!(
        blob.contains("$AB"),
        "an authored minus is already a token: {blob}"
    );
    assert!(!a.contains("__SYS_TXTBUF"), "and needs no run-time build");
}
