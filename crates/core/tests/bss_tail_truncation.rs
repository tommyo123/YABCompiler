use yabcompiler_core::{CompileOptions, Compiled, Profile, compile_with_options, tokenize_program};

fn compile(src: &str) -> Compiled {
    compile_at(src, CompileOptions::default())
}

fn compile_at(src: &str, options: CompileOptions) -> Compiled {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(&prg, options).expect("compiles")
}

/// Decode a CHROUT byte stream the way the corpus tests do.
fn petscii(bytes: &[u8]) -> String {
    let mut s = String::new();
    for &b in bytes {
        match b {
            0x0d => s.push('\n'),
            0x1d => s.push(' '),
            0x20..=0x5a => s.push(b as char),
            0xc1..=0xda => s.push((b - 0x80) as char),
            _ => {}
        }
    }
    s
}

fn run(compiled: &Compiled) -> String {
    let r = emu64::run_prg_to_end(&compiled.prg_bytes, 80_000_000).expect("runs");
    assert!(r.clean_exit, "program must reach its final RTS");
    petscii(&r.output)
}

/// A program with a big enough statically-DIMmed array that the trailing zero run clears the
/// 64-byte floor by a wide margin.
const BIG_BSS: &str = "\
10 DIM A(200)
20 FOR I=0 TO 200
30 A(I)=I*2
40 NEXT I
50 PRINT A(0);A(7);A(200)
60 PRINT \"DONE\"
";

const BIG_BSS_OUT: &str = " 0  14  400 \nDONE\n";

#[test]
fn a_big_bss_is_dropped_from_the_prg() {
    let c = compile(BIG_BSS);
    assert!(
        c.diagnostics.bss_truncated > 0,
        "a 200-element array is 1005 bytes of trailing zeros; none were dropped"
    );
    assert_eq!(
        c.prg_bytes.len() + c.diagnostics.bss_truncated,
        c.machine_code.len() + 14,
        "prg = load address + SYS stub + stored machine code"
    );
    assert!(
        c.asm.contains("__BSS_START:") && c.asm.contains("__BSS_END:"),
        "both brackets must be emitted when a fill is installed"
    );
}

#[test]
fn a_truncated_program_produces_the_same_output_as_an_untruncated_one() {
    let truncated = compile(BIG_BSS);
    assert!(truncated.diagnostics.bss_truncated > 0);
    assert_eq!(run(&truncated), BIG_BSS_OUT);
}

#[test]
fn the_dropped_bytes_were_all_zero() {
    let c = compile(BIG_BSS);
    let kept = c.prg_bytes.len() - 14;
    assert!(
        c.machine_code[kept..].iter().all(|b| *b == 0),
        "truncation must only ever remove $00 bytes"
    );
    assert!(
        c.machine_code.len() - kept == c.diagnostics.bss_truncated,
        "the reported count must match what came off the end"
    );
}

#[test]
fn hello_does_not_grow() {
    // The 37-byte loop against a handful of variable slots is a straight loss.
    let c = compile("10 PRINT \"HELLO\"\n");
    assert_eq!(c.diagnostics.bss_truncated, 0);
    assert!(
        !c.asm.contains("__BSS_START"),
        "no anchor, no fill, no marker left behind:\n{}",
        c.asm
    );
    assert_eq!(c.prg_bytes.len(), 48);
    assert_eq!(run(&c), "HELLO\n");
}

#[test]
fn a_handful_of_scalars_is_below_the_floor() {
    let c = compile("10 A=1:B=2:C=3\n20 PRINT A+B+C\n");
    assert_eq!(
        c.diagnostics.bss_truncated, 0,
        "three float slots is 15 bytes; the loop costs 37"
    );
    assert_eq!(run(&c), " 6 \n");
}

#[test]
fn no_program_is_left_holding_the_placeholder() {
    // The marker line is rewritten into the loop or deleted; it must never reach the assembler.
    for src in [
        "10 PRINT \"HELLO\"\n",
        "10 A=1\n20 PRINT A\n",
        BIG_BSS,
        "10 DIM Z%(400)\n20 PRINT Z%(0)\n",
    ] {
        let c = compile(src);
        assert!(
            !c.asm.contains("@@BSS_TAIL_FILL@@"),
            "placeholder survived into the asm"
        );
    }
}
fn fill_region(asm: &str) -> Option<(usize, usize)> {
    let start = asm.lines().position(|l| l.trim() == "__BSS_START:")?;
    let end = asm.lines().position(|l| l.trim() == "__BSS_END:")?;
    Some((start, end))
}

/// Every `.byte`/`.word` line between `__BSS_START:` and `__BSS_END:`.
fn non_zero_bytes_inside_the_fill(asm: &str) -> Vec<(usize, String)> {
    let Some((start, end)) = fill_region(asm) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (n, line) in asm.lines().enumerate() {
        if n <= start || n >= end {
            continue;
        }
        let code = line.split(';').next().unwrap_or("").trim();
        if !code.starts_with(".byte") && !code.starts_with(".word") {
            continue;
        }
        let operands = code.split_once(char::is_whitespace).map_or("", |x| x.1);
        let all_zero = operands.split(',').all(|o| {
            o.trim()
                .strip_prefix('$')
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                == Some(0)
        });
        if !all_zero {
            out.push((n, code.to_string()));
        }
    }
    out
}
const NON_ZERO_DEFAULT_CASES: [(&str, &str); 4] = [
    (
        "__MUSIC_TEMPO",
        "10 MUSIC 6,\"CDE\"\n20 INPUT A$\n30 PRINT A$\n",
    ),
    (
        "__BFLASH_LAST_JIFFY",
        "10 BFLASH 5,1,2\n20 INPUT A$\n30 PRINT A$\n",
    ),
    ("__HIRES_COLOR", "10 HIRES\n20 INPUT A$\n30 PRINT A$\n"),
    (
        "__SCR_LOAD_DEF_PTR",
        "10 SCRLD DEF 49152\n20 INPUT A$\n30 PRINT A$\n",
    ),
];

#[test]
fn the_fill_never_covers_a_non_zero_default() {
    let mut installed = 0;
    for (label, src) in NON_ZERO_DEFAULT_CASES {
        let c = compile(src);
        assert!(
            c.asm.contains(&format!("{label}:")),
            "fixture for {label} no longer pulls the slot in"
        );
        let Some(_) = fill_region(&c.asm) else {
            panic!("fixture for {label} no longer installs a fill, so it proves nothing");
        };
        installed += 1;
        let bad = non_zero_bytes_inside_the_fill(&c.asm);
        assert!(bad.is_empty(), "{label}: the entry fill would zero {bad:?}");
    }
    assert_eq!(installed, 4);
}

#[test]
fn every_non_zero_default_sits_ahead_of_the_anchor() {
    for (label, src) in NON_ZERO_DEFAULT_CASES {
        let c = compile(src);
        let lines: Vec<&str> = c.asm.lines().collect();
        let slot = lines
            .iter()
            .position(|l| l.trim() == format!("{label}:"))
            .expect("slot emitted");
        let anchor = lines
            .iter()
            .position(|l| l.trim() == "__BSS_START:")
            .expect("fill installed");
        assert!(
            slot < anchor,
            "{label} holds a non-zero default and must sit ahead of __BSS_START \
             (slot line {slot}, anchor line {anchor})"
        );
    }
}

#[test]
fn the_fill_runs_before_saved_sp_is_captured() {
    // `RUN` and `END` both roll the 6502 stack pointer back to `__SAVED_SP`.
    let src = "10 DIM Q(200)\n20 GOSUB 100\n30 END\n100 PRINT \"S\"\n110 RETURN\n";
    let c = compile(src);
    let fill = c
        .asm
        .find("; --- BSS zero-fill ---")
        .expect("array this size installs a fill");
    let save = c
        .asm
        .find("STX __SAVED_SP")
        .expect("GOSUB pulls in SAVED_SP");
    assert!(
        fill < save,
        "the zero-fill must precede the __SAVED_SP capture"
    );
}

#[test]
fn end_inside_a_gosub_still_returns_to_basic() {
    // The runtime half of the ordering check.
    let src = "10 DIM Q(200)\n20 GOSUB 100\n30 PRINT \"NO\"\n100 PRINT \"IN\"\n110 END\n";
    let c = compile(src);
    assert!(
        c.diagnostics.bss_truncated > 0,
        "fixture must install a fill"
    );
    assert_eq!(run(&c), "IN\n");
}

#[test]
fn the_fill_is_the_first_thing_after_cld() {
    let c = compile(BIG_BSS);
    let body: Vec<&str> = c
        .asm
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with(';') && !l.starts_with("*="))
        .collect();
    // Skip the equates (`NAME = $XX`), code that takes no room.
    let body: Vec<&str> = body.into_iter().filter(|l| !l.contains(" = $")).collect();
    let first = 0;
    assert_eq!(body[first], "CLD");
    // The page loop stages its pointer; the stores page by page clear A first.
    assert!(
        body[first + 1].starts_with("LDA #<__BSS_START")
            || (body[first + 1] == "LDA #$00"
                && body[first + 2..first + 5]
                    .iter()
                    .any(|l| l.contains("__BSS_START"))),
        "the fill has to be the first thing that runs, got `{}`",
        body[first + 1]
    );
}

#[test]
fn the_fill_follows_a_relocated_zero_page_pair() {
    // The loop is emitted ahead of `relocate_zp_operands`, so its pointer moves with the rest
    // when POKE uses its pair. Fifteen pages exceed the default inline-fill limit.
    let c = compile("10 DIM Q(2000)\n20 POKE 251,3\n30 PRINT PEEK(251);Q(0)\n");
    assert!(c.diagnostics.bss_truncated > 0);
    let staged = c
        .asm
        .lines()
        .skip_while(|l| l.trim() != "LDA #<__BSS_START")
        .nth(1)
        .expect("fill installed")
        .trim()
        .to_string();
    assert_ne!(
        staged, "STA $FB",
        "fixture must actually relocate the pair, else it proves nothing"
    );
    assert_eq!(run(&c), " 3  0 \n");
}

#[test]
fn outlined_helpers_land_before_the_bss() {
    let mut src = String::from("10 DIM Q(200)\n");
    for i in 0..40 {
        src.push_str(&format!("{} PRINT \"L{}\"\n", 20 + i, i));
    }
    let c = compile(&src);
    let anchor = c
        .asm
        .find("\n__BSS_START:")
        .expect("big array installs the anchor");
    let helpers = c
        .asm
        .find("; --- peephole-factored helpers ---")
        .expect("40 repeated PRINT shapes factor into helpers");
    assert!(
        helpers < anchor,
        "peephole helpers were spliced after __BSS_START"
    );
    assert!(c.diagnostics.bss_truncated > 0);
}

#[test]
fn the_anchor_does_not_separate_a_data_label_from_its_bytes() {
    // `__BSS_START` is the splice point, so anything between it and the first data line gets a
    // helper block pushed in front of it.
    let mut src = String::from("10 DIM Q(200)\n");
    for i in 0..40 {
        src.push_str(&format!("{} PRINT \"L{}\"\n", 20 + i, i));
    }
    src.push_str("500 Q(5)=7\n510 PRINT Q(5)\n");
    let c = compile(&src);
    let lines: Vec<&str> = c.asm.lines().collect();
    let anchor = lines
        .iter()
        .position(|l| l.trim() == "__BSS_START:")
        .expect("anchor present");
    let above = lines[anchor - 1].trim();
    assert!(
        !above.ends_with(':') || above.ends_with("_END:"),
        "`{above}` names the bytes below the anchor and would be stranded \
         on whatever gets spliced in"
    );
    let mut want: String = (0..40).map(|i| format!("L{i}\n")).collect();
    want.push_str(" 7 \n");
    assert_eq!(run(&c), want);
}

#[test]
fn extraram_removes_the_fill_and_its_brackets() {
    let mut options = CompileOptions::default();
    options.extraram = true;
    let c = compile_at(BIG_BSS, options);
    assert!(!c.asm.contains("__BSS_START"), "anchor must be stripped");
    assert!(!c.asm.contains("__BSS_END"), "end bracket must be stripped");
    assert!(
        !c.asm.contains("BSS zero-fill"),
        "the loop itself must be stripped"
    );
    assert_eq!(c.diagnostics.bss_truncated, 0);
    assert_eq!(run(&c), BIG_BSS_OUT);
}

#[test]
fn every_profile_truncates_and_agrees_on_the_output() {
    for profile in [Profile::Default, Profile::Size, Profile::Speed] {
        let mut options = CompileOptions::default();
        options.profile = profile;
        let c = compile_at(BIG_BSS, options);
        assert!(
            c.diagnostics.bss_truncated > 0,
            "{profile:?} left the BSS in the file"
        );
        assert_eq!(run(&c), BIG_BSS_OUT, "{profile:?}");
    }
}

#[test]
fn two_compiles_produce_the_same_file() {
    let a = compile(BIG_BSS);
    let b = compile(BIG_BSS);
    assert_eq!(a.prg_bytes, b.prg_bytes);
    assert_eq!(a.diagnostics.bss_truncated, b.diagnostics.bss_truncated);
}

#[test]
fn a_fill_that_buys_no_truncation_is_dropped_again() {
    // `embed_original_basic_text` appends a copy of the tokenised source past the end of the
    // image, so the zero run stops being a suffix and `bss_tail_keep_len` refuses.
    let src = "10 DIM Q(300)\n20 SYS PEEK(43)+256*PEEK(44)\n30 PRINT \"X\"\n";
    let prg = tokenize_program(src).expect("tokenizes");
    let Ok(c) = compile_with_options(&prg, CompileOptions::default()) else {
        return;
    };
    if c.diagnostics.bss_truncated == 0 {
        assert!(
            !c.asm.contains("BSS zero-fill"),
            "the loop was kept even though nothing was truncated"
        );
    }
}

#[test]
fn a_shipped_fill_always_bought_a_truncation() {
    // TROLL at --profile=speed ends at $2FFB with a $3000-$37FF reservation right above it.
    let plain = compile(BIG_BSS);
    assert!(plain.diagnostics.bss_truncated > 0);
    let end = plain.diagnostics.end_address;
    let mut with_fill = 0;
    let mut without_fill = 0;
    for delta in [-64i32, -8, 0, 8, 40, 96] {
        let start = (i32::from(end) + delta).clamp(0x0900, 0xCF00) as u16;
        let mut options = CompileOptions::default();
        options.reserved_ranges = vec![(start, start + 0x7F)];
        let c = compile_at(BIG_BSS, options);
        if c.asm.contains("BSS zero-fill") {
            with_fill += 1;
            assert!(
                c.diagnostics.bss_truncated > 0,
                "reservation ${start:04X}: the loop shipped but nothing was truncated"
            );
        } else {
            without_fill += 1;
        }
        assert_eq!(run(&c), BIG_BSS_OUT, "reservation at ${start:04X}");
    }
    assert!(
        with_fill > 0 && without_fill > 0,
        "the sweep has to exercise both outcomes: {with_fill} with, {without_fill} without"
    );
}

#[test]
fn speed_clears_page_by_page_on_every_run() {
    // 1,501 integers: 11 pages and a tail, each page one store in the Y loop.
    let src = "\
10 DIM A(1500):S=0:FOR I=0 TO 1500 STEP 100:S=S+A(I):NEXT:PRINT S;A(1500)
20 IF PEEK(828)=7 THEN END
30 POKE 828,7:FOR I=0 TO 1500 STEP 7:A(I)=I:NEXT:RUN
";
    let c = compile_at(
        src,
        CompileOptions {
            profile: Profile::Speed,
            ..CompileOptions::default()
        },
    );
    assert!(
        c.diagnostics.bss_truncated > 2900,
        "{}",
        c.diagnostics.bss_truncated
    );
    assert!(c.asm.contains("STA __BSS_START+$0A00,Y"), "{}", c.asm);
    assert_eq!(run(&c), " 0  0 \n 0  0 \n");
}
