use yabcompiler_core::{
    CompileError, CompileOptions, Profile, compile_with_options, tokenize_program,
};

const BODY: &str = "\
10 DEF FN A(X)=X*X
20 DEF FN B(X)=X/2
30 DEF FN C(X)=X+3
40 PRINT FN A(5)
50 PRINT FN B(10)
60 PRINT FN C(7)
70 PRINT FN A(FN C(2))
80 FOR I=1 TO 5
90 PRINT FN A(I);
100 NEXT
110 PRINT
";

/// Widest pad that fits comfortably inside a 255-byte BASIC line.
const PER_LINE: usize = 200;

fn src(lines: usize, last: usize) -> String {
    let mut s = String::from(BODY);
    let mut ln = 9000;
    for _ in 0..lines {
        s.push_str(&format!("{ln} DATA {}\n", "A".repeat(PER_LINE)));
        ln += 10;
    }
    if last > 0 {
        s.push_str(&format!("{ln} DATA {}\n", "A".repeat(last)));
    }
    s
}

fn opts(profile: Profile) -> CompileOptions {
    CompileOptions {
        profile,
        force_extraram_off: true,
        ..Default::default()
    }
}

#[derive(Debug)]
struct Built {
    bytes: Vec<u8>,
    code_len: usize,
    fell_back: bool,
}

fn build(lines: usize, last: usize, profile: Profile) -> Result<Built, String> {
    let prg = tokenize_program(&src(lines, last)).expect("the dial program tokenizes");
    match compile_with_options(&prg, opts(profile)) {
        Ok(c) => Ok(Built {
            bytes: c.prg_bytes,
            code_len: c.machine_code.len(),
            fell_back: c.diagnostics.size_profile_fallback,
        }),
        Err(e) => Err(e.to_string()),
    }
}

/// The flat dial: `n` pad characters, laid out as whole `PER_LINE` lines plus a remainder.
fn pad(n: usize, profile: Profile) -> Result<Built, String> {
    build(n / PER_LINE, n % PER_LINE, profile)
}

/// The smallest pad `Profile::Size` refuses.
fn size_cut() -> usize {
    static CUT: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CUT.get_or_init(|| {
        let mut hi = PER_LINE;
        loop {
            assert!(
                hi <= 512 * PER_LINE,
                "the pad stopped growing the image: {hi} characters still fit under \
                 MEMSIZ at Profile::Size"
            );
            if pad(hi, Profile::Size).is_err() {
                break;
            }
            hi *= 2;
        }
        let mut lo = hi / 2;
        assert!(
            pad(lo, Profile::Size).is_ok(),
            "the coarse search's lower bound {lo} does not fit, so the dial is not \
             monotone and nothing below can be believed"
        );
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if pad(mid, Profile::Size).is_ok() {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        assert!(pad(lo, Profile::Size).is_ok() && pad(hi, Profile::Size).is_err());
        hi
    })
}

#[test]
fn the_dial_is_exact_and_size_is_smaller() {

    let base = build(4, 1, Profile::Default)
        .expect("Default fits well below the Size cut")
        .code_len;
    for step in 1..=5 {
        let got = build(4, 1 + step, Profile::Default)
            .expect("still fits")
            .code_len;
        assert_eq!(
            got,
            base + step,
            "one more DATA character must be worth exactly one image byte"
        );
    }

    let cut = size_cut();
    let biggest = pad(cut - 1, Profile::Size).expect("the pad below the cut fits at Size");
    let dflt = pad(cut - 1, Profile::Default).expect("Default falls back rather than failing");
    assert!(
        dflt.fell_back,
        "Default built the largest Size-accepted pad ({} chars) on its own, so the two \
         profiles are the same size here and there is no window for the fallback to \
         land in",
        cut - 1
    );
    assert_eq!(
        dflt.bytes, biggest.bytes,
        "the fallback must hand back exactly the Size build"
    );
}

#[test]
fn a_program_that_only_fits_at_size_is_rebuilt_there_instead_of_refused() {
    let cut = size_cut();
    let mut rescued = 0usize;
    let mut n = cut - 1;
    loop {
        let want = pad(n, Profile::Size).expect("Size still fits below its own cut");
        match pad(n, Profile::Default) {
            Ok(b) if b.fell_back => {
                assert_eq!(
                    b.bytes, want.bytes,
                    "pad {n}: the fallback must hand back exactly the Size build"
                );
                rescued += 1;
            }

            Ok(_) => break,
            Err(e) => panic!("pad {n}: Size fits but Default was refused outright: {e}"),
        }
        assert!(
            rescued < 256,
            "the window is at least 256 pads wide, which means Default is not \
             refusing anything and the search is not measuring a window"
        );
        n -= 1;
    }
    assert!(
        rescued >= 2,
        "the window was {rescued} pad(s) wide; the test proved almost nothing"
    );
}

#[test]
fn a_program_that_fits_is_untouched_and_unflagged() {
    for (lines, last) in [(0usize, 1usize), (0, PER_LINE), (4, 50)] {
        let got = build(lines, last, Profile::Default).expect("well under the cut");
        assert!(
            !got.fell_back,
            "({lines},{last}) fits at Default; the fallback must not have run"
        );
        let sized = build(lines, last, Profile::Size).expect("Size fits too");
        assert_ne!(
            got.bytes, sized.bytes,
            "({lines},{last}): the Default build is byte-identical to the Size build, \
             so this case cannot distinguish 'did not fall back' from 'fell back \
             invisibly'"
        );
    }
}

#[test]
fn a_program_that_fits_at_no_profile_reports_the_requested_profiles_error() {
    let over = size_cut() + 8 * PER_LINE;
    let asked = pad(over, Profile::Default)
        .err()
        .expect("fits at neither profile");
    let sized = pad(over, Profile::Size)
        .err()
        .expect("Size is refused too");
    assert!(
        asked.contains("$A000") && asked.contains("BASIC ROM window"),
        "the refusal must still name the gate: {asked}"
    );
    assert_ne!(
        asked, sized,
        "the two profiles overrun by different amounts, so their messages must differ \
         — if they are equal this test cannot tell which one was returned"
    );
}

#[test]
fn an_explicit_size_request_never_reports_a_fallback() {
    assert!(!build(0, 1, Profile::Size).expect("builds").fell_back);
}

/// The retry is keyed on `Assemble`.
#[test]
fn a_non_layout_failure_is_not_retried() {

    let ml = [0x00u8, 0xC0, 0xA9, 0x00, 0x8D, 0x20, 0xD0, 0x60, 0xEA, 0xEA];
    let err = match compile_with_options(&ml, opts(Profile::Default)) {
        Err(e) => e,
        Ok(_) => panic!("a machine-language .prg must be refused"),
    };
    assert!(
        matches!(err, CompileError::NotBasic { .. }),
        "this fixture is meant to fail before assembly; it now fails elsewhere, so it \
         no longer tests the non-retry path: {err}"
    );
}

