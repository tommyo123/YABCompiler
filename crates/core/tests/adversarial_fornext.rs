

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const FORTY: &str = "\
5 goto 10
6 next:if a=0 then end
7 goto 50
8 next:goto 50
10 for i=0 to 10
20 print i;
30 goto 80
40 a=0:goto 6
50 print:for i=11 to 15
60 print i;
70 goto 40
80 a=1:on (i and 1)+1 goto 6,8
";

const FORTY2: &str = "\
10 a=0:for i=0 to 5
20 print i:next
30 if a=0 then a=1:for i=6 to 10
40 if i=8 then 20
50 print i:next
";

const FORTY3: &str = "\
10 p=-1:for i=0 to 4
20 print i;:goto 50
50 next:if i<6 then 100
60 if p=-1 then for p=11 to 12:print p;:goto 50
70 print \"end!\":end
100 for i=5 to 10:print i;:goto 50
";

fn build(src: &str, profile: Profile, extraram: bool) -> (Vec<u8>, String) {
    let prg = tokenize_program(src).expect("tokenizes");
    let opts = CompileOptions {
        profile,
        extraram,
        force_extraram_off: !extraram,
        ..CompileOptions::default()
    };
    let c = compile_with_options(&prg, opts)
        .unwrap_or_else(|e| panic!("compiles ({profile:?}, extraram={extraram}): {e}"));
    (c.prg_bytes, c.asm)
}
fn runtime_frames(asm: &str) -> (usize, usize) {
    let pushes = asm
        .lines()
        .filter(|l| l.trim() == "STA __FOR_STACK,X")
        .count();
    let handlers = asm
        .lines()
        .filter(|l| l.starts_with("__FOR_NEXT_HANDLER_"))
        .count();
    (pushes, handlers)
}

/// Every FOR in these programs is reached by a NEXT that cannot be paired statically, so each
/// one must get its own runtime frame and handler.
fn assert_every_for_has_a_frame(name: &str, src: &str, for_sites: usize) {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        for extraram in [false, true] {
            let (_, asm) = build(src, profile, extraram);
            let (pushes, handlers) = runtime_frames(&asm);
            assert_eq!(
                (pushes, handlers),
                (for_sites, for_sites),
                "{name} ({profile:?}, extraram={extraram}): {for_sites} FOR statements in \
                 source but {pushes} runtime pushes and {handlers} handlers. A FOR that gets \
                 no frame leaves its NEXT dispatching on whatever happens to be on top of \
                 __FOR_STACK."
            );
        }
    }
}
fn normalize(s: &str) -> String {
    s.lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_string()
}

fn assert_all_variants(name: &str, src: &str, expected: &str) {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        for extraram in [false, true] {
            let (bytes, _) = build(src, profile, extraram);
            let got = common::run_clean(&bytes).unwrap_or_else(|| {
                panic!("{name} ({profile:?}, extraram={extraram}) did not reach a clean exit")
            });
            assert_eq!(
                normalize(&got),
                normalize(expected),
                "{name} ({profile:?}, extraram={extraram})"
            );
        }
    }
}

// Structural: no emulator, no ROM, no golden.

#[test]
fn forty_gives_every_for_a_runtime_frame() {
    assert_every_for_has_a_frame("forty", FORTY, 2);
}

#[test]
fn forty2_gives_every_for_a_runtime_frame() {
    assert_every_for_has_a_frame("forty2", FORTY2, 2);
}

#[test]
fn forty3_gives_every_for_a_runtime_frame() {
    assert_every_for_has_a_frame("forty3", FORTY3, 3);
}

// Behavioural: expectations taken from the real ROM, not from the compiler.

#[test]
fn forty2_matches_the_rom() {
    assert_all_variants(
        "forty2",
        FORTY2,
        " 0\n 1\n 2\n 3\n 4\n 5\n 6\n 7\n 8\n 9\n 10",
    );
}

#[test]
fn forty_matches_the_rom() {
    assert_all_variants(
        "forty",
        FORTY,
        " 0  1  2  3  4  5  6  7  8  9  10\n 11  12  13  14  15",
    );
}

#[test]
fn forty3_matches_the_rom() {
    assert_all_variants(
        "forty3",
        FORTY3,
        " 0  1  2  3  4  5  6  7  8  9  10  11  12 END!",
    );
}
