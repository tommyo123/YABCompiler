//! The DATA pool is one unbroken run of bytes and `READ` walks it with nothing but a pointer ; no relocation, no per-entry index.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn compile_asm(src: &str) -> String {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(&prg, CompileOptions::default())
        .expect("compiles")
        .asm
}

fn output_at(src: &str, profile: Profile) -> String {
    let prg = tokenize_program(src).expect("tokenizes");
    let opts = CompileOptions {
        profile,
        ..Default::default()
    };
    let c = compile_with_options(&prg, opts).expect("compiles");
    let r = emu64::run_prg_to_end(&c.prg_bytes, common::MAX_INSNS).expect("runs");
    assert!(r.clean_exit, "must reach the final RTS:\n{src}");
    common::petscii_to_string(&r.output)
}

/// Index of the line that is exactly `want`, or `None`.
fn line_of(asm: &str, want: &str) -> Option<usize> {
    asm.lines().position(|l| l.trim_end() == want)
}

const ZERO_TAILED: &str = "\
10 FORN=0TO63:READK:POKE12288+N,K:NEXT
20 S=0:FORN=0TO63:S=S+PEEK(12288+N):NEXT
30 PRINTS
40 PRINTPEEK(12288);PEEK(12310);PEEK(12311)
50 END
100 DATA1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16
110 DATA17,18,19,20,21,22,23,0,0,0,0,0,0,0,0,0
120 DATA0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0
130 DATA0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0
";

#[test]
fn a_zero_tailed_pool_reads_back_whole() {
    for profile in [Profile::Default, Profile::Size, Profile::Speed] {
        assert_eq!(
            output_at(ZERO_TAILED, profile),
            " 276 \n 1  23  0 \n",
            "profile {profile:?}"
        );
    }
}

/// The structural invariant behind it: nothing may sit between `__DATA:` and the end of the pool.
#[test]
fn the_bss_anchor_never_lands_inside_the_pool() {
    let asm = compile_asm(ZERO_TAILED);
    let data = line_of(&asm, "__DATA:").expect("pool emitted");
    let end = line_of(&asm, "__DATA_END:").expect("pool bound emitted");
    assert!(data < end, "__DATA_END must follow __DATA:\n{asm}");
    if let Some(bss) = line_of(&asm, "__BSS_START:") {
        assert!(
            bss < data || bss > end,
            "__BSS_START landed inside the DATA pool (lines {data}..{end}, anchor {bss}); \
             the peephole splices helper code in front of it:\n{asm}"
        );
    }
    for label in ["__DATA_LINE_100:", "__DATA_LINE_110:", "__DATA_LINE_130:"] {
        let at = line_of(&asm, label).unwrap_or_else(|| panic!("{label} emitted:\n{asm}"));
        assert!(
            at > data && at < end,
            "{label} must stay inside the pool it names:\n{asm}"
        );
    }
}

#[test]
fn the_out_of_data_bound_travels_with_a_hoisted_pool() {
    let asm = compile_asm(ZERO_TAILED);
    let hoisted = line_of(&asm, "__DATA_HOISTED_END:").expect(
        "this program's POKE 12288+N reserves $3000-$303F, which is what hoists the pool",
    );
    let end = line_of(&asm, "__DATA_END:").expect("pool bound emitted");
    assert!(
        end < hoisted,
        "__DATA_END was left behind at the pool's old address:\n{asm}"
    );
}

#[test]
fn data_behind_a_return_on_the_same_line_is_still_in_the_pool() {
    let src = "\
10 GOSUB100
20 READA,B,C:PRINTA;B;C
30 END
100 X=1:RETURN:DATA7,8,9
";
    assert_eq!(output_at(src, Profile::Default), " 7  8  9 \n");
}

#[test]
fn data_behind_a_then_goto_is_still_in_the_pool() {
    let src = "\
10 Q=1
20 IFQTHENP=0:GOTO40:DATA4,5,6
30 PRINT\"NOTTAKEN\"
40 READA,B,C:PRINTA;B;C
50 END
";
    assert_eq!(output_at(src, Profile::Default), " 4  5  6 \n");
}

#[test]
fn data_behind_a_then_line_number_is_still_in_the_pool() {
    let src = "\
10 Q=1
20 IFQTHEN40:DATA4,5,6
30 PRINT\"NOTTAKEN\"
40 READA,B,C:PRINTA;B;C
50 END
";
    assert_eq!(output_at(src, Profile::Default), " 4  5  6 \n");
}

#[test]
fn non_data_behind_a_then_line_number_is_still_discarded() {
    let src = "\
10 Q=0
20 IFQTHEN40:PRINT\"NEVER\"
30 PRINT\"FELLTHROUGH\"
40 END
";
    let asm = compile_asm(src);
    assert!(
        !asm.contains("NEVER"),
        "the THEN <line> tail is dead on both paths:\n{asm}"
    );
    assert_eq!(output_at(src, Profile::Default), "FELLTHROUGH\n");
}

#[test]
fn rescued_data_keeps_its_position_in_the_pool() {
    let src = "\
10 GOSUB100
20 FORI=1TO6:READA:PRINTA;:NEXT
30 PRINT\"\"
40 END
100 RETURN:DATA1,2:DATA3,4
110 DATA5,6
";
    assert_eq!(output_at(src, Profile::Default), " 1  2  3  4  5  6 \n");
}

#[test]
fn executable_code_after_a_transfer_is_still_removed() {
    let src = "\
10 GOTO30
20 PRINT\"NEVER\"
30 PRINT\"LIVE\":GOTO50:PRINT\"ALSONEVER\"
50 END
";
    let asm = compile_asm(src);
    assert!(
        !asm.contains("ALSONEVER"),
        "dead code after GOTO should still be trimmed:\n{asm}"
    );
    assert_eq!(output_at(src, Profile::Default), "LIVE\n");
}

#[test]
fn the_bss_tail_fill_still_installs_below_the_pool() {
    let src = "\
10 DIMA(400)
20 READX:PRINTX+A(399)
30 END
100 DATA5
";
    let asm = compile_asm(src);
    assert!(
        asm.contains("__BSS_START:"),
        "a 2 KB zeroed array should still buy the BSS tail fill:\n{asm}"
    );
    let bss = line_of(&asm, "__BSS_START:").unwrap();
    let data = line_of(&asm, "__DATA:").unwrap();
    assert!(bss > data, "the array's zeros come after the pool:\n{asm}");
    assert_eq!(output_at(src, Profile::Default), " 5 \n");
}

#[test]
fn the_pool_end_marker_survives_the_peephole() {
    let src = "\
10 DIMT(3)
15 T(1)=2
20 READA:PRINTA+T(1)
30 END
100 DATA5,6
";
    let asm = compile_asm(src);
    assert!(
        asm.contains("\n__DATA_END:\n"),
        "the pool-end marker was swept:\n{asm}"
    );
    assert_eq!(output_at(src, Profile::Default), " 7 \n");
}

#[test]
fn the_pool_bound_is_emitted_even_with_no_runtime_check() {
    let asm = compile_asm(
        "\
10 READA,B:PRINTA+B
20 END
100 DATA11,22
",
    );
    assert!(asm.contains("\n__DATA_END:\n"), "{asm}");
}

fn big_pool_program() -> String {
    let mut src = String::from(
        "10 FORN=0TO7:READK:POKE3000+N,K:NEXT\n\
         20 S=0:FORN=0TO7:S=S+PEEK(3000+N):NEXT\n\
         30 PRINTS\n\
         40 END\n",
    );
    let mut items: Vec<u32> = (1..=8).collect();
    items.extend(std::iter::repeat_n(9, 992));
    for (i, chunk) in items.chunks(16).enumerate() {
        let line = 100 + i;
        let vals: Vec<String> = chunk.iter().map(|v| v.to_string()).collect();
        src.push_str(&format!("{line} DATA{}\n", vals.join(",")));
    }
    src
}

fn compile_extraram(src: &str) -> yabcompiler_core::Compiled {
    let prg = tokenize_program(src).expect("tokenizes");
    let opts = CompileOptions {
        extraram: true,
        ..Default::default()
    };
    compile_with_options(&prg, opts).expect("compiles")
}

#[test]
fn a_reservation_inside_an_extraram_pool_moves_the_pool_not_the_entries() {
    let c = compile_extraram(&big_pool_program());
    assert!(
        c.asm.contains("dead fill so the DATA pool starts above every reserved range"),
        "the clearance fill is what makes this program build"
    );
    // The fill goes in front of the pool, never between two entries.
    let fill = c.asm.find("dead fill").expect("fill emitted");
    let data = c.asm.find("\n__DATA:\n").expect("pool emitted");
    assert!(fill < data, "the fill must precede `__DATA:`");
    let r = emu64::run_prg_to_end(&c.prg_bytes, common::MAX_INSNS).expect("runs");
    assert!(r.clean_exit, "must reach the final RTS");
    assert_eq!(common::petscii_to_string(&r.output), " 36 \n");
}

#[test]
fn an_extraram_pool_clear_of_every_reservation_takes_no_fill() {
    let src = big_pool_program().replace("POKE3000+N", "POKE53280+N");
    let c = compile_extraram(&src);
    assert!(
        !c.asm.contains("dead fill so the DATA pool starts"),
        "nothing to clear, so nothing to pay for"
    );
}

#[test]
fn a_pool_with_no_reservations_is_not_hoisted() {
    let src = "\
10 READA,B:PRINTA+B
20 END
100 DATA11,22
";
    let asm = compile_asm(src);
    assert!(
        !asm.contains("JMP __DATA_HOISTED_END"),
        "nothing to hoist past:\n{asm}"
    );
    assert_eq!(output_at(src, Profile::Default), " 33 \n");
}
