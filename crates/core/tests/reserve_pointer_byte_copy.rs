//! A post-tested full-pointer walk must reserve its partial final page too.
use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn kernel(source: u16, destination: u16, limit: u16, src: u8, dst: u8) -> Vec<u8> {
    let [sl, sh] = source.to_le_bytes();
    let [dl, dh] = destination.to_le_bytes();
    let [el, eh] = limit.to_le_bytes();
    vec![
        0xA9,
        sl,
        0x85,
        src,
        0xA9,
        sh,
        0x85,
        src.wrapping_add(1),
        0xA9,
        dl,
        0x85,
        dst,
        0xA9,
        dh,
        0x85,
        dst.wrapping_add(1),
        0xA0,
        0x00,
        0xB1,
        src,
        0x91,
        dst,
        0xE6,
        src,
        0xD0,
        0x02,
        0xE6,
        src.wrapping_add(1),
        0xE6,
        dst,
        0xD0,
        0x02,
        0xE6,
        dst.wrapping_add(1),
        0xA5,
        src,
        0xC9,
        el,
        0xD0,
        0xEA,
        0xA5,
        src.wrapping_add(1),
        0xC9,
        eh,
        0xD0,
        0xE4,
        0x60,
    ]
}

fn image(code: &[u8]) -> Vec<u8> {
    let mut prg = tokenize_program("10 SYS3840\n20 END\n").unwrap();
    prg.resize(2 + 0x0F00 - 0x0801, 0xAA);
    // CHROUT makes the independent all-writes analysis decline.
    prg.extend([0xA9, 8, 0x20, 0xD2, 0xFF, 0x4C, 0x70, 0x0F]);
    prg.resize(2 + 0x0F70 - 0x0801, 0xAA);
    prg.extend(code);
    prg
}

fn ranges(code: &[u8], profile: Profile) -> Vec<(u16, u16)> {
    compile_with_options(
        &image(code),
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .unwrap()
    .diagnostics
    .auto_reserved
}

#[test]
fn reserves_every_byte_of_the_loony_walk_and_no_extra_byte() {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let got = ranges(&kernel(0x75C0, 0xC000, 0x7FF0, 0xFB, 0xFD), profile);
        assert!(got.contains(&(0xC000, 0xCA2F)), "{profile:?}: {got:X?}");
        assert!(!got.iter().any(|&(a, b)| a <= 0xCA30 && b >= 0xCA30));
    }
}

#[test]
fn supports_single_bytes_unaligned_endpoints_and_other_pointer_cells() {
    for (source, destination, limit, last) in [
        (0x3000, 0xC123, 0x3001, 0xC123),
        (0x30F0, 0xC1F8, 0x3101, 0xC208),
        (0x3017, 0xC123, 0x322A, 0xC335),
    ] {
        let got = ranges(
            &kernel(source, destination, limit, 0xF7, 0xF9),
            Profile::Default,
        );
        assert!(got.contains(&(destination, last)), "{got:X?}");
        assert!(!got.iter().any(|&(a, b)| a <= last + 1 && b >= last + 1));
    }
}

#[test]
fn refuses_incomplete_pointer_and_branch_proofs() {
    let good = kernel(0x75C0, 0xC000, 0x7FF0, 0xFB, 0xFD);
    for (offset, replacement) in [
        (17, 1),    // nonzero Y
        (25, 3),    // source carry skips the wrong number of bytes
        (27, 0xFA), // increments an unrelated source-high cell
        (33, 0xFA), // increments an unrelated destination-high cell
        (35, 0xFD), // compares destination low against source limit
        (39, 0xE9), // low mismatch branches into the wrong instruction
        (41, 0xFE), // compares destination high
        (45, 0xE3), // high mismatch branches into the wrong instruction
        (2, 0xA5),  // source-low initialization is a load, not a store
    ] {
        let mut code = good.clone();
        code[offset] = replacement;
        let got = ranges(&code, Profile::Default);
        assert!(
            !got.iter().any(|&(_, b)| b >= 0xC001),
            "offset {offset}: {got:X?}"
        );
    }
}

#[test]
fn refuses_aliases_wraparound_zero_distance_and_out_of_window_destinations() {
    for code in [
        kernel(0x75C0, 0xC000, 0x7FF0, 0xFB, 0xFC),
        kernel(0x75C0, 0xC000, 0x7FF0, 0xFF, 0xFD),
        kernel(0x75C0, 0xC000, 0x7FF0, 0x00, 0xFD),
        kernel(0x7FF0, 0xC000, 0x75C0, 0xFB, 0xFD),
        kernel(0x75C0, 0xC000, 0x75C0, 0xFB, 0xFD),
        kernel(0x3000, 0xCFFF, 0x3002, 0xFB, 0xFD),
        kernel(0x3000, 0xFFF0, 0x3020, 0xFB, 0xFD),
    ] {
        let got = ranges(&code, Profile::Default);
        assert!(!got.iter().any(|&(_, b)| b >= 0xC001), "{got:X?}");
    }
}

#[test]
fn disabling_auto_reservation_and_unexecuted_image_bytes_do_not_claim_the_copy() {
    let code = kernel(0x75C0, 0xC000, 0x7FF0, 0xFB, 0xFD);
    let got = compile_with_options(
        &image(&code),
        CompileOptions {
            auto_reserve: false,
            ..CompileOptions::default()
        },
    )
    .unwrap()
    .diagnostics
    .auto_reserved;
    assert!(got.is_empty(), "{got:X?}");

    let mut prg = tokenize_program("10 END\n").unwrap();
    prg.resize(2 + 0x0F70 - 0x0801, 0xAA);
    prg.extend(code);
    let got = compile_with_options(&prg, CompileOptions::default())
        .unwrap()
        .diagnostics
        .auto_reserved;
    assert!(!got.iter().any(|&(_, b)| b >= 0xC001), "{got:X?}");
}
