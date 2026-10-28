//! The reserved-hole cliff, named.

use yabcompiler_core::{
    CompileOptions, ReservedOvershoot, compile_with_options, reserved_tail_overshoots,
    tokenize_program,
};

/// A listing of `k` `PRINT "<60 unique chars>"` lines.
fn filler(k: usize) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut st: u32 = 12345;
    let mut rnd = || {
        st = st.wrapping_mul(1103515245).wrapping_add(12345);
        A[((st >> 16) % 36) as usize] as char
    };
    let mut out = String::new();
    let mut n = 10;
    for _ in 0..k {
        out.push_str(&format!("{n} PRINT \""));
        for _ in 0..60 {
            out.push(rnd());
        }
        out.push_str("\"\n");
        n += 1;
    }
    out.push_str(&format!("{n} END\n"));
    out
}

fn compile(src: &str, reserved: Vec<(u16, u16)>) -> yabcompiler_core::Compiled {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(
        &prg,
        CompileOptions {
            reserved_ranges: reserved,
            auto_reserve: false,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compiles: {e}"))
}

// The predicate itself.

#[test]
fn a_hole_just_below_the_end_is_reported() {
    let got = reserved_tail_overshoots(0x0801, 0x407C, &[(0x3000, 0x3FFF)]);
    assert_eq!(
        got,
        vec![ReservedOvershoot {
            range: (0x3000, 0x3FFF),
            overshoot: 0x7D,
            hole: 0x1000,
        }]
    );
    assert_eq!(got[0].recoverable(), 0x1000 + 0x7D);
}

/// A hole the image never reaches is not the program's problem.
#[test]
fn a_hole_above_the_end_is_not_reported() {
    assert!(reserved_tail_overshoots(0x0801, 0x2F27, &[(0x3000, 0x3FFF)]).is_empty());

    assert!(reserved_tail_overshoots(0x0801, 0x3FFF, &[(0x3000, 0x3FFF)]).is_empty());
}

/// Nor is a hole *below* the load address.
#[test]
fn a_hole_below_the_image_is_not_reported() {
    assert!(reserved_tail_overshoots(0x0801, 0x0850, &[(0x0400, 0x07E7)]).is_empty());
    // A custom origin moves the line with it.
    assert!(reserved_tail_overshoots(0xC000, 0xC050, &[(0x0400, 0x07E7)]).is_empty());
}

#[test]
fn a_distant_end_is_not_reported() {
    assert!(!reserved_tail_overshoots(0x0801, 0x40FF, &[(0x3000, 0x3FFF)]).is_empty());
    assert!(reserved_tail_overshoots(0x0801, 0x4100, &[(0x3000, 0x3FFF)]).is_empty());
}

#[test]
fn a_hole_smaller_than_the_overshoot_is_not_reported() {
    // 32-byte hole, 100 bytes above it: negative leverage.
    assert!(reserved_tail_overshoots(0x0801, 0x3063, &[(0x3000, 0x301F)]).is_empty());

    let got = reserved_tail_overshoots(0x0801, 0x302F, &[(0x3000, 0x301F)]);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].recoverable(), 32 + 16);
}

#[test]
fn holes_are_ranked_by_what_there_is_to_gain() {
    let got = reserved_tail_overshoots(0x0801, 0x30C4, &[(0x3090, 0x30BF), (0x3000, 0x307F)]);
    assert_eq!(
        got.iter()
            .map(|o| (o.range, o.recoverable()))
            .collect::<Vec<_>>(),
        vec![((0x3000, 0x307F), 128 + 69), ((0x3090, 0x30BF), 48 + 5)],
        "the 128-byte hole is worth more than the 48-byte one: {got:#?}"
    );

    assert_eq!(got[0].overshoot, 0x30C4 - 0x307F);
    assert_eq!(got[1].overshoot, 0x30C4 - 0x30BF);
}

#[test]
fn the_cap_is_applied_per_hole() {
    let got = reserved_tail_overshoots(0x0801, 0x4020, &[(0x3000, 0x37FF), (0x3F00, 0x3FFF)]);
    assert_eq!(
        got.iter().map(|o| o.range).collect::<Vec<_>>(),
        vec![(0x3F00, 0x3FFF)],
        "$3000-$37FF ends 2081 bytes back — too far to ask about: {got:#?}"
    );
}

// End to end, against real compiler output.
#[test]
fn a_real_overshooting_image_reports_its_hole() {
    let c = compile(&filler(157), vec![(0x3000, 0x3FFF)]);
    let o = c
        .diagnostics
        .reserved_overshoots
        .first()
        .unwrap_or_else(|| panic!("expected an overshoot, got {:#?}", c.diagnostics));
    assert_eq!(o.range, (0x3000, 0x3FFF));
    assert_eq!(o.hole, 4096);
    assert!(
        o.overshoot > 0 && o.overshoot <= 256,
        "overshoot out of the reported band: {o:#?}"
    );
    assert_eq!(o.recoverable(), o.hole + o.overshoot);
}

#[test]
fn an_image_that_clears_the_hole_says_nothing() {
    let c = compile(&filler(156), vec![(0x3000, 0x3FFF)]);
    assert!(
        c.diagnostics.reserved_overshoots.is_empty(),
        "nothing to report at {:04X}: {:#?}",
        c.diagnostics.end_address,
        c.diagnostics.reserved_overshoots
    );
    assert!(
        c.diagnostics.end_address < 0x3000,
        "the fixture must really clear the hole: ${:04X}",
        c.diagnostics.end_address
    );
}

#[test]
fn the_cliff_is_real() {
    let over = compile(&filler(157), vec![(0x3000, 0x3FFF)]);
    let under = compile(&filler(156), vec![(0x3000, 0x3FFF)]);
    let o = over.diagnostics.reserved_overshoots[0];
    let jump = over.prg_bytes.len() - under.prg_bytes.len();
    assert!(
        jump >= o.recoverable() as usize,
        "one line of source must be worth at least the reported {} bytes, \
         measured {jump}",
        o.recoverable()
    );
    assert!(
        jump > 4000,
        "the point of the diagnostic is that this is a cliff, not a slope: {jump}"
    );

    let code_delta = over.machine_code.len() - under.machine_code.len() - o.hole as usize;
    assert!(
        code_delta < 256,
        "one PRINT line should be worth well under a page of code: {code_delta}"
    );
}

/// A program with no reservations at all is untouched by any of this.
#[test]
fn no_reservations_means_no_report() {
    let c = compile(&filler(145), vec![]);
    assert!(c.diagnostics.reserved_overshoots.is_empty());
}
