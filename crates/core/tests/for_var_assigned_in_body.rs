//! Assigning to a `FOR` variable inside its own body, the classic BASIC V2 early-exit idiom.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

/// Assert `src` compiles on every profile, runs clean, and prints exactly `out` ; the ROM's own answer.
fn check(src: &str, out: &str) {
    for profile in PROFILES {
        let prg = tokenize_program(src).expect("tokenizes");
        let built = compile_with_options(
            &prg,
            CompileOptions {
                profile,
                ..CompileOptions::default()
            },
        )
        .unwrap_or_else(|e| panic!("must compile ({profile:?}): {e}\n{src}"))
        .prg_bytes;
        let r = emu64::run_prg_to_end(&built, common::MAX_INSNS).expect("run");
        assert_eq!(
            r.error_banner(),
            None,
            "no error expected ({profile:?}):\n{src}"
        );
        assert!(r.clean_exit, "clean exit expected ({profile:?}):\n{src}");
        assert_eq!(
            common::petscii_to_string(&r.output).trim_end(),
            out,
            "({profile:?}):\n{src}"
        );
    }
}

/// Shoving the counter to the limit ends the loop on the next `NEXT`, and leaves the variable at limit + step ; not at the limit.
#[test]
fn assigning_the_limit_to_the_loop_var_exits_the_loop() {
    check(
        "10 n=0\n20 for k=1 to 5\n30 n=n+1\n40 if k=2 then k=5\n50 next k\n60 print n;k\n",
        " 2  6",
    );
}

#[test]
fn assigning_below_the_limit_makes_the_loop_run_longer() {
    check(
        "10 n=0\n20 for k=1 to 6\n30 n=n+1\n40 if k=1 then k=4\n50 next k\n60 print n;k\n",
        " 3  7",
    );
}

#[test]
fn a_loop_with_no_assignment_is_unchanged() {
    check(
        "10 n=0\n20 for k=1 to 5\n30 n=n+1\n50 next k\n60 print n;k\n",
        " 5  6",
    );
}

#[test]
fn the_early_exit_is_local_to_its_own_loop_inside_a_gosub() {
    check(
        "10 c=0\n20 for j=0 to 3\n30 for i=0 to 10 step 2\n40 gosub 100\n50 next i\n60 next j\n\
         70 print c:end\n100 for k=1 to 5\n110 if k=2 then k=5\n120 next k\n130 c=c+1\n140 return\n",
        " 24",
    );
}

/// A non-unit `STEP` still governs the exit: `K` goes 1, 4, then is shoved to 9, and `NEXT` takes it to 12 ; past the limit of 10.
#[test]
fn the_step_still_governs_the_exit_after_an_assignment() {
    check(
        "10 n=0\n20 for k=1 to 10 step 3\n30 n=n+1\n40 if k=4 then k=9\n50 next k\n60 print n;k\n",
        " 2  12",
    );
}
