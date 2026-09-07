use yabcompiler_core::{CompileError, CompileOptions, Compiled, compile_with_options};

struct Line {
    number: u16,
    body: Vec<u8>,
}

fn line(number: u16, body: &[u8]) -> Line {
    Line {
        number,
        body: body.to_vec(),
    }
}

impl Line {
    fn clone_body(&self) -> Line {
        Line {
            number: self.number,
            body: self.body.clone(),
        }
    }
}

fn build_prg(lines: &[Line]) -> Vec<u8> {
    build_prg_at(0x0801, lines)
}

fn build_prg_at(load: u16, lines: &[Line]) -> Vec<u8> {
    let mut addr = load;
    let mut starts = Vec::new();
    for l in lines {
        starts.push(addr);
        addr += 5 + l.body.len() as u16;
    }
    let mut out = load.to_le_bytes().to_vec();
    for (i, l) in lines.iter().enumerate() {
        let next = starts.get(i + 1).copied().unwrap_or(addr);
        out.extend_from_slice(&next.to_le_bytes());
        out.extend_from_slice(&l.number.to_le_bytes());
        out.extend_from_slice(&l.body);
        out.push(0);
    }
    out.extend_from_slice(&[0, 0]);
    out
}

fn compile(prg: &[u8]) -> Compiled {
    compile_with_options(prg, CompileOptions::default()).expect("compiles")
}

fn petscii(bytes: &[u8]) -> String {
    let mut s = String::new();
    for &b in bytes {
        match b {
            0x0d => s.push('\n'),
            0x20..=0x5a => s.push(b as char),
            0xc1..=0xda => s.push((b - 0x80) as char),
            _ => {}
        }
    }
    s
}

fn run(compiled: &Compiled) -> String {
    let r = emu64::run_prg_to_end(&compiled.prg_bytes, 40_000_000).expect("runs");
    assert!(r.clean_exit, "program must reach its final RTS");
    petscii(&r.output)
}

/// `IF PEEK(43)=1 THEN PRINT"A"` / `IF PEEK(44)=8 THEN PRINT"B"`.
fn compare_only_program() -> Vec<u8> {
    build_prg(&[
        line(10, b"\x8b\xc2(43)\xb21\xa7\x99\"A\""),
        line(20, b"\x8b\xc2(44)\xb28\xa7\x99\"B\""),
    ])
}

#[test]
fn comparing_the_pointer_reads_what_the_interpreter_would() {

    assert_eq!(run(&compile(&compare_only_program())).trim(), "A\nB");
}

#[test]
fn comparing_the_pointer_carries_no_copy_of_the_text() {
    let prg = compare_only_program();
    let c = compile(&prg);
    let src = &prg[2..];
    assert!(
        !c.prg_bytes.windows(src.len()).any(|w| w == src),
        "a program that only compares the pointer must not pay for a copy of its text"
    );
}

#[test]
fn dereferencing_the_pointer_reads_the_original_text() {

    let prg = build_prg(&[
        line(10, b"\x8fQ"),
        line(20, b"\x99\xc7(\xc2(\xc2(43)\xaa256\xac\xc2(44)\xaa5))"),
    ]);
    let c = compile(&prg);

    let src = &prg[2..];
    assert_eq!(
        &c.prg_bytes[2 + 15..2 + src.len()],
        &src[15..],
        "the original text has to be materialised at its own address"
    );
    assert!(
        c.prg_bytes.windows(15).any(|w| w == &src[..15]),
        "the saved copy of $0801-$080F has to be in the image"
    );

    assert_eq!(run(&c).trim(), "Q");
}

#[test]
fn dereferencing_the_pointer_still_reads_1_and_8() {

    let prg = build_prg(&[
        line(10, b"\x8fQ"),
        line(20, b"\x99\xc2(43);\xc2(44)"),
        line(30, b"\x99\xc7(\xc2(\xc2(43)\xaa256\xac\xc2(44)\xaa5))"),
    ]);
    // Compared as whitespace-separated tokens: BASIC's numeric field padding is not what is under test here, the two numbers are.
    let out = run(&compile(&prg));
    let got: Vec<&str> = out.split_whitespace().collect();
    assert_eq!(got, vec!["1", "8", "Q"], "full output was {out:?}");
}

fn txtptr_target(image: &[u8]) -> Option<u16> {
    let body = &image[2..];
    let at = body.windows(8).position(|w| {
        w[0] == 0xA9 && w[2] == 0x85 && w[3] == 0x7A && w[4] == 0xA9 && w[6] == 0x85 && w[7] == 0x7B
    })?;
    Some(u16::from_le_bytes([body[at + 1], body[at + 5]]))
}

#[test]
fn a_materialised_program_does_not_run_twice() {

    let prg = build_prg(&[
        line(10, b"\x99\"HI\""),
        line(20, b"\x99\xc7(\xc2(\xc2(43)\xaa256\xac\xc2(44)\xaa6))"),
    ]);
    let c = compile(&prg);
    let target = txtptr_target(&c.prg_bytes)
        .expect("the prologue has to point TXTPTR somewhere of its own");

    let off = 2 + (target as usize - 0x0801);
    assert_eq!(
        &c.prg_bytes[off..off + 3],
        &[0, 0, 0],
        "TXTPTR must point at an end-of-program terminator, not ${target:04X} = {:?}",
        &c.prg_bytes[off..off + 3]
    );
    // The program still runs and still reads its own text.
    let out = run(&c);
    let got: Vec<&str> = out.split_whitespace().collect();
    assert_eq!(got, vec!["HI", "H"], "full output was {out:?}");
}

#[test]
fn a_sys_into_image_build_keeps_the_old_prologue() {

    let prg = build_prg(&[
        line(10, b"\x99\"AB\""),
        line(20, b"\x9e2065"),
        line(30, b"\x8fPADDINGPADDINGPADDINGPADDING"),
    ]);
    let c = compile_with_options(&prg, CompileOptions::default()).expect("compiles");
    assert_eq!(
        stamps(&c.prg_bytes),
        (false, true),
        "a SYS into the image must still be materialised in place"
    );
    assert!(
        txtptr_target(&c.prg_bytes).is_none(),
        "a SYS-into-image build must not carry the TXTPTR terminator"
    );
}

#[test]
fn a_program_with_no_pointer_read_is_not_materialised() {

    let prg = build_prg(&[line(10, b"\x99\"HI\""), line(20, b"\x99\"OK\"")]);
    let c = compile(&prg);
    let src = &prg[2..];
    assert!(
        !c.prg_bytes.windows(src.len()).any(|w| w == src),
        "a program that never reads TXTTAB must not carry a copy of its text"
    );
    let out = run(&c);
    let got: Vec<&str> = out.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    assert_eq!(got, vec!["HI", "OK"], "full output was {out:?}");
}

/// The stamp each layout leaves in the image: `LDA #$01 / STA $2B / LDA #$hi / STA $2C`.
fn stamps(image: &[u8]) -> (bool, bool) {
    let pad = [0xA9, 0x01, 0x85, 0x2B, 0xA9, 0x80, 0x85, 0x2C];
    let inplace = [0xA9, 0x01, 0x85, 0x2B, 0xA9, 0x08, 0x85, 0x2C];
    (
        image.windows(8).any(|w| w == pad),
        image.windows(8).any(|w| w == inplace),
    )
}

#[test]
fn a_load_address_other_than_0801_still_gets_the_landing_pad() {

    let deref = line(20, b"\x99\xc7(\xc2(\xc2(43)\xaa256\xac\xc2(44)\xaa5))");
    let at_0801 = compile(&build_prg(&[line(10, b"\x8fQ"), deref.clone_body()]));
    let at_1001 = compile(&build_prg_at(
        0x1001,
        &[line(10, b"\x8fQ"), deref.clone_body()],
    ));
    assert_eq!(
        stamps(&at_0801.prg_bytes),
        (false, true),
        "a $0801 load must be materialised in place"
    );
    assert_eq!(
        stamps(&at_1001.prg_bytes),
        (true, false),
        "a non-$0801 load must fall back to the $8001 pad"
    );
}

#[test]
fn an_image_that_would_reach_the_io_area_is_refused() {

    let mut lines = vec![line(5, b"\x99\xc7(\xc2(\xc2(43)\xaa256\xac\xc2(44)\xaa5))")];
    for i in 0..2200u16 {
        let mut body = vec![0x8Fu8];
        body.extend_from_slice(b"PADDINGPADDINGPADDIN");
        lines.push(line(100 + i, &body));
    }
    let prg = build_prg(&lines);

    assert!(
        0x0801 + prg.len() > 0xD000,
        "the fixture shrank: {}",
        prg.len()
    );
    match compile_with_options(&prg, CompileOptions::default()) {
        Err(CompileError::Assemble { message, .. }) => {
            // The refusal has to name an address and say what the program did to earn the layout, or it is not actionable.
            assert!(message.contains("?OUT OF MEMORY"), "{message}");
            // The shared refusal also covers SYS and payload-copy triggers.
            assert!(message.contains("original loaded image"), "{message}");
            assert!(message.contains("$0801"), "{message}");
            assert!(
                message.contains("$A000") || message.contains("$D000"),
                "{message}"
            );
        }
        other => panic!("expected a refusal, got {:?}", other.map(|c| c.prg_bytes.len())),
    }
}
