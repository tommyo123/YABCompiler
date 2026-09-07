use yabcompiler_core::{CompileOptions, compile_with_options, tokenize_program};

mod common;

const MAX_INSNS: u64 = 20_000_000;

fn compile(src: &str) -> Vec<u8> {
    let prg = tokenize_program(src).expect("tokenize");
    compile_with_options(&prg, CompileOptions::default())
        .expect("compile")
        .prg_bytes
}

/// Freeze the KERNAL jiffy IRQ first so the value printed is exactly the value the store produced, then print the clock.
fn clock_after(assign: &str) -> String {
    let src = format!("10 poke 56333,127\n20 ti$={assign}\n30 print ti\n");
    let r = emu64::run_prg_to_end(&compile(&src), MAX_INSNS).expect("run");
    assert_eq!(
        r.error_code, None,
        "unexpected error: {:?}",
        r.error_banner()
    );
    common::petscii_to_string(&r.output).trim().to_string()
}

fn error_for(assign: &str) -> (Option<u8>, Option<u16>, Option<String>) {
    let src = format!("10 poke 56333,127\n20 ti$={assign}\n30 print 99\n");
    let r = emu64::run_prg_to_end(&compile(&src), MAX_INSNS).expect("run");
    (r.error_code, r.error_line, r.error_banner())
}

#[test]
fn six_real_digits_convert_to_the_rom_jiffy_count() {
    for (s, want) in [
        ("\"000000\"", "0"),
        ("\"000001\"", "60"),
        ("\"000100\"", "3600"),
        ("\"010000\"", "216000"),
        ("\"123456\"", "2717760"),
        ("\"235959\"", "5183940"),
    ] {
        assert_eq!(clock_after(s), want, "TI$={s}");
    }
}

#[test]
fn the_clock_write_survives_the_extraram_layout() {
    for (s, want) in [
        ("\"000000\"", "0"),
        ("\"010203\"", "223380"),
        ("\"235959\"", "5183940"),

        ("\"oooooo\"", "7795196"),
        ("\"OOOOOO\"", "503804"),
    ] {
        let src = format!("10 poke 56333,127\n20 ti$={s}\n30 print ti\n");
        let prg = tokenize_program(&src).expect("tokenize");
        let opts = CompileOptions {
            extraram: true,
            ..CompileOptions::default()
        };
        let out = compile_with_options(&prg, opts).expect("compile");
        let r = emu64::run_prg_to_end(&out.prg_bytes, MAX_INSNS).expect("run");
        assert_eq!(
            r.error_code,
            None,
            "TI$={s} under extraram: {:?}",
            r.error_banner()
        );
        assert_eq!(
            common::petscii_to_string(&r.output).trim(),
            want,
            "TI$={s} under extraram"
        );
    }
}

#[test]
fn letter_o_converts_as_a_pseudo_digit() {
    assert_eq!(clock_after("\"oooooo\""), "7795196");
}

#[test]
fn shifted_letter_o_converts_as_a_negative_pseudo_digit() {
    assert_eq!(clock_after("\"OOOOOO\""), "503804");
}

#[test]
fn pseudo_digit_values_match_the_rom_across_the_range() {
    for (chr, want) in [
        (58u16, "600"),
        (65, "1020"),
        (160, "6720"),
        (192, "16770496"),
        (255, "16774276"),
    ] {
        assert_eq!(
            clock_after(&format!("\"00000\"+chr$({chr})")),
            want,
            "chr$({chr})"
        );
    }
}

#[test]
fn a_character_below_dollar_30_still_raises() {
    let (code, line, banner) = error_for("\"00000\"+chr$(47)");
    assert_eq!(code, Some(14), "ILLEGAL QUANTITY");
    assert_eq!(line, Some(20));
    assert_eq!(
        banner.as_deref(),
        Some("?ILLEGAL QUANTITY  ERROR IN 20"),
        "the ROM's banner, two spaces and all"
    );
}

#[test]
fn the_length_must_be_exactly_six() {
    for bad in ["\"\"", "\"0\"", "\"00000\"", "\"0000000\"", "\"000000000\""] {
        let (code, line, banner) = error_for(bad);
        assert_eq!(code, Some(14), "TI$={bad} must raise ILLEGAL QUANTITY");
        assert_eq!(line, Some(20), "TI$={bad}");
        assert_eq!(
            banner.as_deref(),
            Some("?ILLEGAL QUANTITY  ERROR IN 20"),
            "TI$={bad}"
        );
    }
}

#[test]
fn a_clock_write_survives_when_only_ti_reads_it() {
    assert_eq!(clock_after("\"010000\""), "216000");
}

#[test]
fn a_clock_write_survives_when_only_peek_reads_it() {
    let r = emu64::run_prg_to_end(
        &compile(
            "10 poke 56333,127\n20 ti$=\"010000\"\n\
             30 print peek(160);peek(161);peek(162)\n",
        ),
        MAX_INSNS,
    )
    .expect("run");
    assert_eq!(r.error_code, None, "{:?}", r.error_banner());
    assert_eq!(
        common::petscii_to_string(&r.output).trim(),
        "3  75  192",
        "$A0/$A1/$A2 = $03/$4B/$C0"
    );
}

#[test]
fn an_unread_clock_write_still_raises_on_bad_input() {
    let r = emu64::run_prg_to_end(
        &compile("10 poke 56333,127\n20 ti$=\"zz\"\n30 print 99\n"),
        MAX_INSNS,
    )
    .expect("run");
    assert_eq!(r.error_code, Some(14), "{:?}", r.error_banner());
    assert_eq!(r.error_line, Some(20));
    assert_eq!(
        common::petscii_to_string(&r.output).trim(),
        "",
        "line 30 must never run"
    );
}

#[test]
fn an_unreached_bad_clock_write_costs_nothing() {
    let r = emu64::run_prg_to_end(
        &compile(
            "10 poke 56333,127\n20 ti$=\"010000\"\n30 goto 60\n\
             40 ti$=\"zz\"\n50 ti$=\"000000\"\n60 print ti\n",
        ),
        MAX_INSNS,
    )
    .expect("run");
    assert_eq!(
        r.error_code, None,
        "lines 40/50 never execute: {:?}",
        r.error_banner()
    );
    assert_eq!(
        common::petscii_to_string(&r.output).trim(),
        "216000",
        "line 50 must not have reset the clock"
    );
}

#[test]
fn a_reached_bad_clock_write_fires_at_its_own_line() {
    let r = emu64::run_prg_to_end(
        &compile("10 poke 56333,127\n20 print 11\n30 if 1 then ti$=\"zz\"\n40 print 22\n"),
        MAX_INSNS,
    )
    .expect("run");
    assert_eq!(r.error_code, Some(14), "{:?}", r.error_banner());
    assert_eq!(r.error_line, Some(30), "?... ERROR IN 30, not 20 or 40");
    assert_eq!(common::petscii_to_string(&r.output).trim(), "11");
}
