//! A DATA item's spelling decided whether two analyses could see its value, and both of them then claimed a range the program did not obey.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

fn build(src: &str, profile: Profile) -> yabcompiler_core::Compiled {
    let prg = tokenize_program(src).unwrap_or_else(|e| panic!("tokenize: {e:?}"));
    compile_with_options(
        &prg,
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compile: {e}"))
}

/// `(clean_exit, decoded PRINT output)`.
fn run(src: &str, profile: Profile) -> (bool, String) {
    let b = build(src, profile);
    let r = emu64::run_prg_to_end(&b.prg_bytes, common::MAX_INSNS).expect("emu64 run");
    (r.clean_exit, common::petscii_to_string(&r.output))
}

fn promoted(asm: &str, base: &str) -> bool {
    let label = format!("__ARR_{base}_I:");
    asm.lines().any(|l| l.trim_end() == label)
}

const CIVIL_WAR_SHAPE: &str = "10 dim m(3)\n\
                               20 for d=1 to 3:read m(d):next\n\
                               30 print m(1);m(2);m(3)\n\
                               40 data 18000,40000.,44894.\n";

#[test]
fn an_out_of_range_data_spelling_is_not_an_integer_array() {
    for p in PROFILES {
        let asm = build(CIVIL_WAR_SHAPE, p).asm;
        assert!(
            !promoted(&asm, "M"),
            "{p:?}: M() was narrowed to i16 even though DATA holds 40000 — \
             READ will raise ?ILLEGAL QUANTITY where the ROM stores the value"
        );
    }
}

#[test]
fn a_read_of_an_out_of_range_spelling_does_not_trap() {
    for p in PROFILES {
        let (clean, out) = run(CIVIL_WAR_SHAPE, p);
        assert!(clean, "{p:?}: the program trapped: {out:?}");
        assert!(
            out.contains("18000") && out.contains("40000") && out.contains("44894"),
            "{p:?}: values lost: {out:?}"
        );
    }
}

#[test]
fn a_negative_sentinel_is_visible_to_the_data_range() {
    const SRC: &str = "10 i=0:z=0\n\
                       20 read a:if a<0 then 50\n\
                       30 poke 828+i,a:z=z+a:i=i+1:goto 20\n\
                       50 print \"z\";z\n\
                       60 data 11,22,33,-1\n";
    for p in PROFILES {
        let asm = build(SRC, p).asm;

        let dead = asm
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .windows(2)
            .filter(|w| w[0].trim() == "CMP #$00" && w[1].trim().starts_with("BCC "))
            .count();
        assert_eq!(
            dead, 0,
            "{p:?}: `IF A<0` compiled to an unsigned 8-bit compare that can \
             never branch — the -1 sentinel is invisible to the DATA range again"
        );
        let (clean, out) = run(SRC, p);
        assert!(clean, "{p:?}: trapped instead of reaching the sentinel: {out:?}");
        assert!(
            out.contains("66"),
            "{p:?}: the loop did not stop at the sentinel with Z=66: {out:?}"
        );
    }
}

/// `dataflow::compute_data_range`.
#[test]
fn a_fractional_spelling_is_not_covered_by_the_data_range() {
    const SRC: &str = "10 for i=1 to 3:read p:print int(p):next\n\
                       20 data 1,2,2.7000\n";
    for p in PROFILES {
        let (clean, out) = run(SRC, p);
        assert!(clean, "{p:?}: the program trapped: {out:?}");
        // The ROM prints ` 1`, ` 2`, ` 2` ; INT(2.7) is 2, not 2.7.
        assert!(
            !out.contains("2.7"),
            "{p:?}: INT(2.7) came back as 2.7 — the DATA range claimed an \
             integer the pool does not hold: {out:?}"
        );
    }
}

#[test]
fn canonical_integer_data_still_promotes() {
    const SRC: &str = "10 dim m(3)\n\
                       20 for d=1 to 3:read m(d):next\n\
                       30 print m(1);m(2);m(3)\n\
                       40 data 1,2,3\n";
    for p in PROFILES {
        let asm = build(SRC, p).asm;
        assert!(
            promoted(&asm, "M"),
            "{p:?}: an all-integer DATA set no longer promotes M()"
        );
    }
}

#[test]
fn a_non_canonical_spelling_that_fits_still_promotes() {
    const SRC: &str = "10 dim m(3)\n\
                       20 for d=1 to 3:read m(d):next\n\
                       30 print m(1);m(2);m(3)\n\
                       40 data 1e1,007,- 5\n";
    for p in PROFILES {
        let asm = build(SRC, p).asm;
        assert!(
            promoted(&asm, "M"),
            "{p:?}: 1e1 / 007 / - 5 are integers in range and must not cost \
             M() its promotion"
        );
    }
    let (clean, out) = run(SRC, Profile::Default);
    assert!(clean, "trapped: {out:?}");
    assert!(
        out.contains("10") && out.contains("7") && out.contains("-5"),
        "values wrong: {out:?}"
    );
}

#[test]
fn text_data_does_not_block_promotion() {
    const SRC: &str = "10 dim m(3),n$(3)\n\
                       20 for d=1 to 3:read n$(d),m(d):next\n\
                       30 print n$(1);m(1);n$(3);m(3)\n\
                       40 data \"bull run\",1,siloh,2,\"antietam\",3\n";
    for p in PROFILES {
        let asm = build(SRC, p).asm;
        assert!(
            promoted(&asm, "M"),
            "{p:?}: unreadable-as-number text items cost M() its promotion"
        );
    }
}

/// A DATA set with no numeric items at all still leaves the pass free ; there is no numeric READ to constrain.
#[test]
fn a_pool_of_only_text_leaves_the_pass_free() {
    const SRC: &str = "10 dim m(3)\n\
                       20 for d=1 to 3:m(d)=d:next\n\
                       30 read a$:print a$;m(2)\n\
                       40 data hello\n";
    for p in PROFILES {
        let asm = build(SRC, p).asm;
        assert!(
            promoted(&asm, "M"),
            "{p:?}: a text-only pool must not block promotion"
        );
    }
}

#[test]
fn the_i16_boundary_is_read_from_the_spelling() {
    const FITS: &str = "10 dim m(1)\n\
                        20 read m(1)\n\
                        30 print m(1)\n\
                        40 data 3.2767e4\n";
    const OVER: &str = "10 dim m(1)\n\
                        20 read m(1)\n\
                        30 print m(1)\n\
                        40 data 3.2768e4\n";
    assert!(
        promoted(&build(FITS, Profile::Default).asm, "M"),
        "32767 spelled 3.2767e4 fits i16 and must still promote"
    );
    assert!(
        !promoted(&build(OVER, Profile::Default).asm, "M"),
        "32768 spelled 3.2768e4 does not fit i16 and must not promote"
    );
    let (clean, out) = run(OVER, Profile::Default);
    assert!(clean, "trapped: {out:?}");
    assert!(out.contains("32768"), "value lost: {out:?}");
}
