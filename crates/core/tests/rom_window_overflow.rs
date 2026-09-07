//! An extraram-OFF image may not reach into the `$A000-$BFFF` BASIC ROM window.

use yabcompiler_core::{CompileOptions, compile_with_options, tokenize_program};

/// A string-free program plus `items` unread numeric DATA entries.
fn padded(items: usize) -> String {
    let mut s = String::from(
        "\
10 FOR K=1 TO 3:READ V:PRINT V;:NEXT
20 PRINT
30 END
300 DATA 11,22,33
",
    );
    s.push_str(&format!("310 DATA {}\n", vec!["7"; items].join(",")));
    s
}

fn build(src: &str, opts: CompileOptions) -> Result<(u32, Vec<u8>), String> {
    let prg = tokenize_program(src).map_err(|e| format!("{e:?}"))?;
    compile_with_options(&prg, opts)
        .map(|c| (0x080Du32 + c.machine_code.len() as u32, c.prg_bytes))
        .map_err(|e| e.to_string())
}

fn forced_off() -> CompileOptions {
    CompileOptions {
        force_extraram_off: true,
        ..Default::default()
    }
}

/// Search for the smallest `items` whose forced-off image would end past MEMSIZ, by bisecting
/// on the *accepted* end address.
fn first_refused_items() -> usize {
    let mut lo = 1usize;
    let mut hi = 20_000usize;
    assert!(
        build(&padded(lo), forced_off()).is_ok(),
        "the small end must still build"
    );
    assert!(
        build(&padded(hi), forced_off()).is_err(),
        "the large end must be refused"
    );
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if build(&padded(mid), forced_off()).is_ok() {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    hi
}

#[test]
fn the_forced_off_layout_stops_at_memsiz() {
    let first_bad = first_refused_items();
    let (end, _) = build(&padded(first_bad - 1), forced_off()).expect("one step below builds");
    assert!(
        end <= 0xA000,
        "the last accepted forced-off image ends at ${end:04X}, past MEMSIZ"
    );
    assert!(
        end > 0x9F00,
        "the sweep never got near the window (last accepted end ${end:04X}); \
         the padding dial is not doing its job"
    );
    let err = build(&padded(first_bad), forced_off()).expect_err("one step above is refused");
    assert!(
        err.contains("$A000") && err.contains("BASIC ROM window"),
        "the refusal must name the ROM window, got: {err}"
    );
}

/// Decode the `$FFD2` stream the way the machine renders it.
fn rendered(output: &[u8]) -> String {
    output
        .iter()
        .filter_map(|&b| match b {
            0x0d => Some('\n'),
            0x1d => Some(' '),
            0x20..=0x5a => Some(b as char),
            0xc1..=0xda => Some((b - 0x80) as char),
            _ => None,
        })
        .collect()
}

#[test]
fn the_auto_predictor_never_ships_a_wrong_image() {
    let first_bad = first_refused_items();
    let mut shipped = 0;
    for items in [first_bad - 1, first_bad, first_bad + 1] {
        let Ok((end, prg)) = build(&padded(items), CompileOptions::default()) else {
            continue; // Rejected layouts produce no image.
        };
        shipped += 1;
        let r = emu64::run_prg_to_end(&prg, 80_000_000).expect("runs");
        assert_eq!(
            rendered(&r.output),
            " 11  22  33 \n",
            "items={items} end=${end:04X}"
        );
        assert!(r.clean_exit, "items={items} end=${end:04X}");
    }
    assert!(
        shipped > 0,
        "the auto predictor refused every size, so this test checked nothing"
    );
}

#[test]
fn images_below_the_window_are_untouched() {
    // Land the image around $9E00-$9F00, well inside the legal range but past the point where a
    // too-eager check would trip.
    let first_bad = first_refused_items();
    for back in [1usize, 2, 3, 64, 128, 256] {
        let items = first_bad - back;
        let (end, prg) = build(&padded(items), forced_off())
            .unwrap_or_else(|e| panic!("items={items} must build: {e}"));
        assert!(end <= 0xA000);
        let r = emu64::run_prg_to_end(&prg, 80_000_000).expect("runs");
        let out: String = r
            .output
            .iter()
            .filter_map(|&b| match b {
                0x0d => Some('\n'),
                0x1d => Some(' '),
                0x20..=0x5a => Some(b as char),
                0xc1..=0xda => Some((b - 0x80) as char),
                _ => None,
            })
            .collect();
        assert_eq!(out, " 11  22  33 \n", "items={items} end=${end:04X}");
        assert!(r.clean_exit, "items={items} end=${end:04X}");
    }
}

fn forced_on() -> CompileOptions {
    CompileOptions {
        extraram: true,
        ..Default::default()
    }
}

#[test]
fn the_extraram_layout_still_has_its_data_ceiling() {
    let err = build(&padded(20_000), forced_on()).expect_err("must be refused");
    assert!(
        err.contains("$A000"),
        "expected the data-ceiling message, got: {err}"
    );
}

#[test]
fn the_extraram_data_ceiling_is_the_rom_window_and_not_a_page_below_it() {
    // Bisect on the largest forced-on build the ceiling accepts.
    let (mut lo, mut hi) = (1usize, 20_000usize);
    assert!(build(&padded(lo), forced_on()).is_ok());
    assert!(build(&padded(hi), forced_on()).is_err());
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if build(&padded(mid), forced_on()).is_ok() {
            lo = mid;
        } else {
            hi = mid;
        }
    }

    let err = build(&padded(hi), forced_on()).expect_err("one step up is refused");
    assert!(err.contains("$A000"), "{err}");

    let (_, prg) = build(&padded(lo), forced_on()).expect("one step down builds");
    let r = emu64::run_prg_to_end(&prg, 80_000_000).expect("runs");
    assert_eq!(rendered(&r.output), " 11  22  33 \n");
    assert!(r.clean_exit);
    let (_, prg_margin) = build(&padded(lo - 100), forced_on()).expect("inside the old margin");
    let r = emu64::run_prg_to_end(&prg_margin, 80_000_000).expect("runs");
    assert_eq!(rendered(&r.output), " 11  22  33 \n");
    assert!(r.clean_exit);
}
