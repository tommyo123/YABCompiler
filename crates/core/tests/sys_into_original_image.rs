//! Preserve original image bytes reached by a SYS call.

use yabcompiler_core::compile::{LowCut, low_trim_facts};
use yabcompiler_core::{CompileOptions, Compiled, compile_with_options};

/// `LDA #<ch> : JSR $FFD2 : RTS` ; six bytes, none of them `$00`, so the run can sit inside a BASIC line without terminating it.
fn print_char_ml(ch: u8) -> Vec<u8> {
    vec![0xA9, ch, 0x20, 0xD2, 0xFF, 0x60]
}

/// One tokenised BASIC line, ready to be chained.
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

fn build_prg(lines: &[Line], tail: &[u8]) -> (Vec<u8>, Vec<u16>) {
    const LOAD: u16 = 0x0801;

    let mut addr = LOAD;
    let mut starts = Vec::new();
    let mut body_addrs = Vec::new();
    for l in lines {
        starts.push(addr);
        body_addrs.push(addr + 4);
        addr += 5 + l.body.len() as u16; // link + number + body + $00
    }
    let mut out = LOAD.to_le_bytes().to_vec();
    for (i, l) in lines.iter().enumerate() {
        let next = starts.get(i + 1).copied().unwrap_or(addr);
        out.extend_from_slice(&next.to_le_bytes());
        out.extend_from_slice(&l.number.to_le_bytes());
        out.extend_from_slice(&l.body);
        out.push(0);
    }
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(tail);
    (out, body_addrs)
}

/// `SYS <n>` as tokenised bytes.
fn sys(n: u16) -> Vec<u8> {
    let mut v = vec![0x9E];
    v.extend_from_slice(n.to_string().as_bytes());
    v
}

/// `<var>=<n>`, tokenised.
fn assign(var: &str, n: u16) -> Vec<u8> {
    let mut v = var.as_bytes().to_vec();
    v.push(0xB2);
    v.extend_from_slice(n.to_string().as_bytes());
    v
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

#[test]
fn ml_in_a_rem_line_runs() {

    let mut rem = vec![0x8F];
    rem.extend_from_slice(&print_char_ml(b'A'));
    let probe = build_prg(
        &[
            line(10, &rem),
            line(20, &[0x99, b'"', b'B', b'"', b';']),
            line(30, &sys(2049)),
        ],
        &[],
    );
    // The ML sits one byte past the start of line 10's body (past the REM token).
    let ml_addr = probe.1[0] + 1;
    assert_eq!(ml_addr.to_string().len(), 4, "operand width must be stable");
    let (prg, addrs) = build_prg(
        &[
            line(10, &rem),
            line(20, &[0x99, b'"', b'B', b'"', b';']),
            line(30, &sys(ml_addr)),
        ],
        &[],
    );
    assert_eq!(addrs[0] + 1, ml_addr, "layout must not have shifted");

    let c = compile(&prg);
    assert_eq!(
        c.diagnostics.original_image,
        Some((0x0801, 0x0801 + (prg.len() - 2) as u16 - 1)),
        "a SYS into the text must materialise the original image"
    );
    assert_eq!(run(&c), "BA", "the embedded ML must run after the PRINT");
}

#[test]
fn ml_appended_above_the_link_chain_runs() {

    let probe = build_prg(&[line(10, &sys(2049))], &print_char_ml(b'Z'));

    let tail_addr = 0x0801 + (probe.0.len() - 2 - print_char_ml(b'Z').len()) as u16;
    assert_eq!(tail_addr.to_string().len(), 4);
    let (prg, _) = build_prg(&[line(10, &sys(tail_addr))], &print_char_ml(b'Z'));

    let c = compile(&prg);
    assert_eq!(
        c.diagnostics.original_image,
        Some((0x0801, 0x0801 + (prg.len() - 2) as u16 - 1)),
        "an appended block a SYS reaches must be materialised too"
    );
    assert_eq!(run(&c), "Z");
}

#[test]
fn image_is_byte_identical_to_the_source() {
    let mut rem = vec![0x8F];
    rem.extend_from_slice(&print_char_ml(b'A'));
    let (_, addrs) = build_prg(&[line(10, &rem), line(20, &sys(2049))], &[]);
    let ml_addr = addrs[0] + 1;
    let (prg, _) = build_prg(&[line(10, &rem), line(20, &sys(ml_addr))], &[]);

    let c = compile(&prg);
    assert!(c.diagnostics.original_image.is_some());
    let src = &prg[2..];
    let out = &c.prg_bytes[2..];
    assert!(out.len() > src.len(), "the compiled code goes above it");

    assert_eq!(&out[15..src.len()], &src[15..], "image must be verbatim");
    assert!(
        out.windows(15).any(|w| w == &src[..15]),
        "the displaced 15-byte head must be carried for the prologue"
    );
}

#[test]
fn the_last_assignment_is_not_missed() {
    let mut rem = vec![0x8F];
    rem.extend_from_slice(&print_char_ml(b'A'));
    let probe = build_prg(
        &[
            line(10, &rem),
            line(20, &assign("UP%", 1146)),
            line(30, &assign("UP%", 2000)),
            line(40, &[0x9E, b'U', b'P', b'%']),
        ],
        &[],
    );
    let ml_addr = probe.1[0] + 1;
    assert_eq!(ml_addr.to_string().len(), 4);
    let (prg, _) = build_prg(
        &[
            line(10, &rem),
            line(20, &assign("UP%", 1146)),
            line(30, &assign("UP%", ml_addr)),
            line(40, &[0x9E, b'U', b'P', b'%']),
        ],
        &[],
    );
    let c = compile(&prg);
    assert!(
        c.diagnostics.original_image.is_some(),
        "the second assignment is the one that matters"
    );
    assert_eq!(run(&c), "A");
}

#[test]
fn sys_expression_is_resolved() {
    let mut rem = vec![0x8F];
    rem.extend_from_slice(&print_char_ml(b'A'));
    let probe = build_prg(
        &[
            line(10, &rem),
            line(20, &assign("UU", 1000)),
            // 30 SYS UU+<delta>
            line(30, &[0x9E, b'U', b'U', 0xAA, b'0', b'0', b'0', b'0']),
        ],
        &[],
    );
    let ml_addr = probe.1[0] + 1;
    let delta = ml_addr - 1000;
    assert_eq!(delta.to_string().len(), 4, "keep the line width fixed");
    let mut sys_uu = vec![0x9E, b'U', b'U', 0xAA];
    sys_uu.extend_from_slice(delta.to_string().as_bytes());
    let (prg, _) = build_prg(
        &[
            line(10, &rem),
            line(20, &assign("UU", 1000)),
            line(30, &sys_uu),
        ],
        &[],
    );
    let c = compile(&prg);
    assert!(c.diagnostics.original_image.is_some(), "UU+delta resolves");
    assert_eq!(run(&c), "A");
}

#[test]
fn sys_below_the_load_address_is_left_alone() {
    let (prg, _) = build_prg(&[line(10, &sys(828))], &[]);
    assert_eq!(compile(&prg).diagnostics.original_image, None);
}

#[test]
fn sys_above_the_image_is_left_alone() {
    let (prg, _) = build_prg(&[line(10, &sys(49152))], &[]);
    assert_eq!(compile(&prg).diagnostics.original_image, None);
}

/// A SYS just past the end of a file with no appended payload.
#[test]
fn sys_past_a_bare_terminator_is_left_alone() {
    let probe = build_prg(&[line(10, &sys(2049))], &[]);
    let just_past = 0x0801 + (probe.0.len() - 2) as u16;
    assert_eq!(just_past.to_string().len(), 4);
    let (prg, _) = build_prg(&[line(10, &sys(just_past))], &[]);
    assert_eq!(compile(&prg).diagnostics.original_image, None);
}

#[test]
fn unresolvable_sys_is_left_alone() {
    // 10 A=PEEK(2): 20 SYS A   ; `PEEK` is $C2, `:` is $3A.
    let mut l = b"A".to_vec();
    l.push(0xB2);
    l.push(0xC2);
    l.extend_from_slice(b"(2)");
    let (prg, _) = build_prg(&[line(10, &l), line(20, &[0x9E, b'A'])], &[]);
    assert_eq!(compile(&prg).diagnostics.original_image, None);
}

#[test]
fn txttab_idiom_is_materialised_in_place() {
    // 10 SYS PEEK(43)+256*PEEK(44) $C2 = PEEK, $AA = +, $AC = *
    let mut l = vec![0x9E, 0xC2];
    l.extend_from_slice(b"(43)");
    l.push(0xAA);
    l.extend_from_slice(b"256");
    l.push(0xAC);
    l.push(0xC2);
    l.extend_from_slice(b"(44)");
    let (prg, _) = build_prg(&[line(10, &l)], &[]);
    let c = compile(&prg);

    assert_eq!(
        c.diagnostics.original_image,
        Some((0x0801, 0x0801 + (prg.len() - 2) as u16 - 1))
    );

    let src = &prg[2..];
    assert_eq!(&c.prg_bytes[2 + 15..2 + src.len()], &src[15..]);
}

fn rem_ml_program() -> Vec<u8> {
    let mut rem = vec![0x8F];
    rem.extend_from_slice(&print_char_ml(b'A'));
    let probe = build_prg(
        &[
            line(10, &rem),
            line(20, &[0x99, b'"', b'B', b'"', b';']),
            line(30, &sys(2049)),
        ],
        &[],
    );
    let ml_addr = probe.1[0] + 1;
    assert_eq!(ml_addr.to_string().len(), 4, "operand width must be stable");
    build_prg(
        &[
            line(10, &rem),
            line(20, &[0x99, b'"', b'B', b'"', b';']),
            line(30, &sys(ml_addr)),
        ],
        &[],
    )
    .0
}

#[test]
fn extraram_is_honoured_and_the_embedded_ml_still_runs() {
    let prg = rem_ml_program();
    let opts = CompileOptions {
        extraram: true,
        ..CompileOptions::default()
    };
    let c = compile_with_options(&prg, opts).expect("compiles with extraram");
    assert!(
        c.diagnostics.extraram.is_on(),
        "the flag must not be overridden any more, got {:?}",
        c.diagnostics.extraram
    );
    assert!(
        c.diagnostics.original_image.is_some(),
        "the image is still materialised"
    );
    assert!(
        c.asm.contains("LDA #$36"),
        "the extraram preamble must bank the BASIC ROM out"
    );
    assert_eq!(run(&c), "BA", "the embedded ML must run under extraram too");
}

#[test]
fn extraram_keeps_the_image_at_its_own_load_address() {
    let prg = rem_ml_program();
    let opts = CompileOptions {
        extraram: true,
        ..CompileOptions::default()
    };
    let c = compile_with_options(&prg, opts).expect("compiles with extraram");
    let src = &prg[2..];
    let out = &c.prg_bytes[2..];
    assert!(out.len() > src.len(), "the compiled code goes above it");
    assert_eq!(
        &out[15..src.len()],
        &src[15..],
        "image must be verbatim under extraram as well"
    );
    assert!(
        out.windows(15).any(|w| w == &src[..15]),
        "the displaced 15-byte head must still be carried for the prologue"
    );

    let jt = c
        .asm
        .find("extraram: bank BASIC ROM out")
        .expect("the extraram preamble");
    let hoist = c
        .asm
        .find("extraram: hoisted data sections")
        .expect("the hoisted-data marker");
    let pin = c.asm.find("__BT_INPLACE_HEAD:").expect("the pinned head");
    assert!(pin < jt, "the image must precede the extraram preamble");
    assert!(pin < hoist, "the image must precede the hoisted data");
}

/// Enable extra RAM when a preserved image cannot fit below $A000.
#[test]
fn a_d4_build_that_overruns_a000_reaches_for_extraram() {

    const PAYLOAD: usize = 34 * 1024;

    let mut lines: Vec<Line> = Vec::new();
    // DIM A(800) ; 4,806 bytes of static float array. $86 is DIM.
    let mut dim = vec![0x86];
    dim.extend_from_slice(b"A(800)");
    lines.push(line(5000, &dim));
    // Code bulk: 55 float expressions, ~66 bytes of compiled code each. $AC = *, $AA = +, $AD = /, $AB = -, $B2 = =.
    for i in 0..55u16 {
        let text = format!(
            "B{}=B{}*3.25+A({})/7.5-B{}",
            i % 26,
            (i + 1) % 26,
            i % 100,
            (i + 2) % 26
        );
        let body: Vec<u8> = text
            .bytes()
            .map(|c| match c {
                b'*' => 0xAC,
                b'+' => 0xAA,
                b'/' => 0xAD,
                b'-' => 0xAB,
                b'=' => 0xB2,
                other => other,
            })
            .collect();
        lines.push(line(6000 + i, &body));
    }
    // The payload: the ML at its foot, then filler.
    let mut ml = print_char_ml(b'A');
    ml.resize(PAYLOAD, 0xEA);

    lines.push(line(9010, &sys(1000)));
    let probe = build_prg(&lines, &ml);
    // The payload starts where the file ends minus its own length, counted from the load address past the 2-byte header.
    let ml_addr = 0x0801u16 + (probe.0.len() - 2 - ml.len()) as u16;
    assert_eq!(ml_addr.to_string().len(), 4, "operand width must be stable");
    let n = lines.len();
    lines[n - 1] = line(9010, &sys(ml_addr));
    let (prg, _) = build_prg(&lines, &ml);

    assert_eq!(
        low_trim_facts(&prg, &CompileOptions::default())
            .expect("materialises")
            .decision,
        LowCut::Refused("min-saving"),
        "the fixture must not be low-trimmable, or it is not asking this question",
    );

    let plain = compile_with_options(
        &prg,
        CompileOptions {
            force_extraram_off: true,
            ..CompileOptions::default()
        },
    );
    let Err(err) = plain else {
        panic!("the plain layout must overrun $A000");
    };
    assert!(
        format!("{err}").contains("OUT OF MEMORY"),
        "expected an out-of-memory refusal, got: {err}"
    );

    let c = compile(&prg);
    assert!(
        c.diagnostics.extraram.is_on(),
        "the auto-predictor must switch to extraram, got {:?}",
        c.diagnostics.extraram
    );
    assert!(c.diagnostics.original_image.is_some());
    assert!(
        c.diagnostics.end_address > 0xA000,
        "the rescue is only meaningful if the code really uses the window, \
         ends at ${:04X}",
        c.diagnostics.end_address
    );
    let src = &prg[2..];
    assert_eq!(
        &c.prg_bytes[2 + 15..2 + src.len()],
        &src[15..],
        "the image must survive the rescue verbatim"
    );
}

#[test]
fn a_d4_build_that_fits_stays_on_the_plain_layout() {
    let c = compile(&rem_ml_program());
    assert!(
        !c.diagnostics.extraram.is_on(),
        "a small D4 program must not be moved to extraram, got {:?}",
        c.diagnostics.extraram
    );
}

fn ml_low_with_filler(n_filler: u16) -> Vec<u8> {
    let mut rem = vec![0x8F];
    rem.extend_from_slice(&print_char_ml(b'A'));
    let filler = vec![b'X'; 240];
    let build = |ml_addr: u16| {
        let mut lines = vec![line(10, &rem)];
        for i in 0..n_filler {
            let mut body = vec![0x8F];
            body.extend_from_slice(&filler);
            lines.push(line(100 + i, &body));
        }
        lines.push(line(9000, &sys(ml_addr)));
        build_prg(&lines, &[])
    };
    let probe = build(2049);
    let ml_addr = probe.1[0] + 1;
    assert_eq!(ml_addr.to_string().len(), 4, "operand width must be stable");
    build(ml_addr).0
}

#[test]
fn an_unreachable_image_tail_is_not_materialised() {
    let prg = ml_low_with_filler(60);
    let src_len = prg.len() - 2;
    assert!(
        src_len > 14_000,
        "the fixture must be big enough for the saving to be the point, is {src_len}"
    );
    let c = compile(&prg);
    let (lo, hi) = c
        .diagnostics
        .original_image
        .expect("a SYS resolves into the image");
    assert_eq!(lo, 0x0801);
    let kept = hi as usize - lo as usize + 1;
    assert!(
        kept < 1024,
        "the ML is in line 10 and nothing else is entered, so the kept prefix should be \
         a few hundred bytes; kept {kept} of {src_len}"
    );

    let whole = compile(&ml_low_with_filler(0));
    assert!(
        c.prg_bytes.len() < whole.prg_bytes.len() + 2048,
        "60 filler REM lines must not cost 14 KB of image: {} vs {}",
        c.prg_bytes.len(),
        whole.prg_bytes.len()
    );
}

#[test]
fn the_kept_prefix_is_still_verbatim() {
    let prg = ml_low_with_filler(60);
    let c = compile(&prg);
    let (_, hi) = c.diagnostics.original_image.expect("a D4 build");
    let kept = hi as usize - 0x0801 + 1;
    assert_eq!(
        &c.prg_bytes[2 + 15..2 + kept],
        &prg[2 + 15..2 + kept],
        "every byte of the kept prefix must be the source's"
    );
}

#[test]
fn an_entry_point_at_the_top_of_the_image_keeps_its_payload() {
    let ml = print_char_ml(b'A');
    let filler = vec![b'X'; 240];
    let build = |ml_addr: u16| {
        let mut lines = Vec::new();

        for i in 0..60u16 {
            let mut body = vec![0x8F];
            body.extend_from_slice(&filler);
            lines.push(line(100 + i, &body));
        }
        lines.push(line(9000, &sys(ml_addr)));
        build_prg(&lines, &ml)
    };
    let probe = build(10000);
    let ml_addr = 0x0801u16 + (probe.0.len() - 2 - ml.len()) as u16;
    assert_eq!(ml_addr.to_string().len(), 5, "operand width must be stable");
    let prg = build(ml_addr).0;
    let c = compile(&prg);
    let (lo, hi) = c.diagnostics.original_image.expect("a D4 build");
    assert_eq!(lo, 0x0801);
    assert_eq!(
        hi as usize,
        0x0801 + prg.len() - 2 - 1,
        "the window still spans the whole image: the payload's address is fixed by it"
    );

    let cut = match low_trim_facts(&prg, &CompileOptions::default())
        .expect("materialises its image")
        .decision
    {
        LowCut::Take(n) => n,
        LowCut::Refused(why) => panic!("this fixture is meant to be trimmable, refused: {why}"),
    };
    assert!(
        usize::from(ml_addr - 0x0801) >= cut,
        "the cut must sit below the entry point",
    );
    // The half that must never move.
    assert_eq!(
        &c.prg_bytes[2 + cut..2 + prg.len() - 2],
        &prg[2 + cut..],
        "everything from the cut up — the payload above all — must survive verbatim"
    );

    assert_ne!(
        &c.prg_bytes[2 + 15..2 + cut],
        &prg[2 + 15..2 + cut],
        "the dead text below the cut must not still be embedded"
    );
    assert!(
        c.diagnostics.end_address < 0x0801 + prg.len() as u16,
        "the image end must drop below where the untrimmed blob alone would have put it"
    );
}

#[test]
fn an_unanalysable_routine_keeps_the_whole_image() {
    // JMP ($0334) ; a vector the scan cannot resolve.
    let mut rem = vec![0x8F, 0x6C, 0x34, 0x03];
    rem.extend_from_slice(&print_char_ml(b'A'));
    let filler = vec![b'X'; 240];
    let build = |ml_addr: u16| {
        let mut lines = vec![line(10, &rem)];
        for i in 0..20u16 {
            let mut body = vec![0x8F];
            body.extend_from_slice(&filler);
            lines.push(line(100 + i, &body));
        }
        lines.push(line(9000, &sys(ml_addr)));
        build_prg(&lines, &[])
    };
    let probe = build(2049);
    let ml_addr = probe.1[0] + 1;
    assert_eq!(ml_addr.to_string().len(), 4, "operand width must be stable");
    let prg = build(ml_addr).0;
    let c = compile(&prg);
    let (_, hi) = c.diagnostics.original_image.expect("a D4 build");
    assert_eq!(
        hi as usize,
        0x0801 + prg.len() - 2 - 1,
        "the scan cannot follow JMP ($0334), so it must keep everything"
    );
}

#[test]
fn a_literal_peek_into_the_tail_keeps_it() {
    let mut rem = vec![0x8F];
    rem.extend_from_slice(&print_char_ml(b'A'));
    let filler = vec![b'X'; 240];
    // Address inside the last filler line: 20 lines of 245 bytes each start at $0801 + line(10) and run to roughly $1400.
    let peek_at = 0x1000u16;
    let build = |ml_addr: u16| {
        let mut lines = vec![line(10, &rem)];
        for i in 0..20u16 {
            let mut body = vec![0x8F];
            body.extend_from_slice(&filler);
            lines.push(line(100 + i, &body));
        }
        // 8000 PRINT PEEK(4096)  ; $99 is PRINT, $C2 is PEEK.
        let mut p = vec![0x99, 0xC2];
        p.push(b'(');
        p.extend_from_slice(peek_at.to_string().as_bytes());
        p.push(b')');
        lines.push(line(8000, &p));
        lines.push(line(9000, &sys(ml_addr)));
        build_prg(&lines, &[])
    };
    let probe = build(2049);
    let ml_addr = probe.1[0] + 1;
    assert_eq!(ml_addr.to_string().len(), 4, "operand width must be stable");
    let prg = build(ml_addr).0;
    assert!(
        prg.len() - 2 > peek_at as usize - 0x0801,
        "the PEEK must land inside the image for the test to mean anything"
    );
    let c = compile(&prg);
    let (_, hi) = c.diagnostics.original_image.expect("a D4 build");
    assert!(
        hi >= peek_at,
        "the byte the program PEEKs must still be the source's; kept up to ${hi:04X}, \
         PEEK is at ${peek_at:04X}"
    );
}

#[test]
fn an_appended_payload_is_never_truncated() {
    // The payload: a routine that returns immediately, then 4 KB of data no instruction names.
    let mut tail = print_char_ml(b'A');
    tail.extend(std::iter::repeat_n(0x5A, 4096));
    let filler = vec![b'X'; 240];
    let build = |ml_addr: u16| {
        let mut lines = Vec::new();
        for i in 0..8u16 {
            let mut body = vec![0x8F];
            body.extend_from_slice(&filler);
            lines.push(line(100 + i, &body));
        }
        lines.push(line(9000, &sys(ml_addr)));
        build_prg(&lines, &tail)
    };
    let probe = build(2049);
    let ml_addr = 0x0801u16 + (probe.0.len() - 2 - tail.len()) as u16;
    assert_eq!(ml_addr.to_string().len(), 4, "operand width must be stable");
    let prg = build(ml_addr).0;
    let c = compile(&prg);
    let (_, hi) = c.diagnostics.original_image.expect("a D4 build");
    assert_eq!(
        hi as usize,
        0x0801 + prg.len() - 2 - 1,
        "the 4 KB of data above the payload's RTS is not reachable from any \
         instruction, and must be kept anyway"
    );
}
