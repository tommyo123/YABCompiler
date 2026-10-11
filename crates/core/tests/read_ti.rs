mod common;

use yabcompiler_core::{CompileOptions, compile_with_options, tokenize_program};

const CLOCKS: [(u32, u32, u32); 9] = [
    (0, 0, 0),
    (0, 0, 1),
    (0, 1, 0),
    (1, 0, 0),
    (0, 255, 255),
    (18, 52, 86),
    (79, 26, 0),
    (128, 0, 1),
    (255, 255, 255),
];

fn source() -> String {
    let mut src = String::new();
    for (n, (h, m, l)) in CLOCKS.iter().enumerate() {
        src.push_str(&format!("{} POKE 160,{h}:POKE 161,{m}:POKE 162,{l}:PRINT TI\n", 10 + n));
    }
    src.push_str("90 TI$=\"012345\":PRINT TI$;TI;TI/60;-TI\n");
    src
}

fn expected() -> String {
    let mut out = String::new();
    for (h, m, l) in CLOCKS {
        out.push_str(&format!(" {} \n", h * 65536 + m * 256 + l));
    }
    out.push_str("012345 301500  5025 -301500 \n");
    out
}

fn ti_string_source() -> String {
    let mut src = String::new();
    for (n, (h, m, l)) in CLOCKS.iter().enumerate() {
        src.push_str(&format!("{} POKE 160,{h}:POKE 161,{m}:POKE 162,{l}:PRINT TI$\n", 10 + n));
    }
    src.push_str("90 X=5:A$=TI$+\"!\":PRINT A$;X+LEN(TI$);X;MID$(TI$,3,2)\n");
    src
}

fn hhmmss(jiffies: u32) -> String {
    let s = jiffies / 60;
    format!("{:02}{:02}{:02}", s / 3600, s / 60 % 60, s % 60)
}

fn ti_string_expected() -> String {
    let mut out = String::new();
    for (h, m, l) in CLOCKS {
        out.push_str(&hhmmss(h * 65536 + m * 256 + l));
        out.push('\n');
    }
    let last = hhmmss(0xFF_FFFF);
    out.push_str(&format!("{last}! 11  5 {}\n", &last[2..4]));
    out
}

fn run(src: &str, extraram: bool) -> String {
    let prg = tokenize_program(src).expect("tokenizes");
    let c = compile_with_options(
        &prg,
        CompileOptions {
            extraram,
            ..CompileOptions::default()
        },
    )
    .expect("compiles");
    let r = emu64::run_prg_to_end(&c.prg_bytes, common::MAX_INSNS).expect("runs");
    assert_eq!(r.error_banner(), None);
    common::petscii_to_string(&r.output)
}

#[test]
fn ti_is_the_24_bit_jiffy_count() {
    assert_eq!(run(&source(), false), expected());
}

#[test]
fn ti_reads_the_same_with_basic_banked_out() {
    assert_eq!(run(&source(), true), expected());
}

#[test]
fn ti_string_is_the_count_as_hours_minutes_and_seconds() {
    assert_eq!(run(&ti_string_source(), false), ti_string_expected());
}

#[test]
fn ti_string_reads_the_same_with_basic_banked_out() {
    assert_eq!(run(&ti_string_source(), true), ti_string_expected());
}
