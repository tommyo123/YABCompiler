//! `PRINT#` must raise when the channel cannot be opened for output, before it prints anything.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

fn build(src: &str, profile: Profile) -> Vec<u8> {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(
        &prg,
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("must compile ({profile:?}): {e}\n{src}"))
    .prg_bytes
}

#[track_caller]
fn check_banner(src: &str, banner: &str, printed: &str) {
    for profile in PROFILES {
        let prg = build(src, profile);
        let r = emu64::run_prg_to_end(&prg, common::MAX_INSNS).expect("run");
        assert_eq!(
            r.error_banner().as_deref(),
            Some(banner),
            "({profile:?}):\n{src}"
        );

        let out = common::petscii_to_string(&r.output);
        let head = out.split('?').next().unwrap_or("").trim().to_string();
        assert_eq!(head, printed, "nothing may print first ({profile:?}):\n{src}");
    }
}

#[track_caller]
fn check_ok(src: &str, out: &str) {
    for profile in PROFILES {
        let prg = build(src, profile);
        let r = emu64::run_prg_to_end(&prg, common::MAX_INSNS).expect("run");
        assert_eq!(r.error_banner(), None, "({profile:?}):\n{src}");
        assert!(r.clean_exit, "clean exit expected ({profile:?}):\n{src}");
        assert_eq!(
            common::petscii_to_string(&r.output).trim(),
            out,
            "({profile:?}):\n{src}"
        );
    }
}

#[test]
fn print_to_unopened_file_is_file_not_open() {
    check_banner(
        "10 print#1,\"x\"\n20 print \"after\"\n",
        "?FILE NOT OPEN  ERROR IN 10",
        "",
    );
}



#[test]
fn nothing_in_a_multi_item_list_escapes() {
    check_banner(
        "10 a=7\n20 print#1,\"x\";a;\"y\"\n30 print \"after\"\n",
        "?FILE NOT OPEN  ERROR IN 20",
        "",
    );
}

/// The line in the banner is the `PRINT#`'s own, not the `OPEN`'s and not the next statement's.
#[test]
fn banner_names_the_print_statements_line() {
    check_banner(
        "10 print \"a\"\n20 print \"b\"\n99 print#4,\"x\"\n",
        "?FILE NOT OPEN  ERROR IN 99",
        "A\nB",
    );
}

#[test]
fn open_to_the_screen_still_prints() {
    check_ok(
        "10 open 1,3,0\n20 print#1,\"x\"\n30 print \"after\"\n",
        "X\nAFTER",
    );
}

#[test]
fn open_to_the_screen_prints_a_full_item_list() {
    check_ok(
        "10 open 1,3,0\n20 a=7\n30 print#1,\"x\";a;\"y\"\n40 print \"z\"\n",
        "X 7 Y\nZ",
    );
}

#[test]
fn channel_is_restored_for_the_following_print() {
    check_ok(
        "10 open 1,3,0\n20 print#1,\"a\"\n30 print \"b\"\n40 print \"c\"\n",
        "A\nB\nC",
    );
}

#[test]
fn repeated_prints_through_one_channel() {
    check_ok(
        "10 open 1,3,0\n20 print#1,\"a\"\n30 print#1,\"b\"\n40 print#1,\"c\"\n",
        "A\nB\nC",
    );
}

#[test]
fn plain_print_is_untouched() {
    check_ok("10 print \"a\";1;\"b\"\n", "A 1 B");
}

#[test]
fn the_raise_survives_extraram() {
    let src = "10 print#1,\"x\"\n20 print \"after\"\n";
    let prg = tokenize_program(src).expect("tokenizes");
    let img = compile_with_options(
        &prg,
        CompileOptions {
            extraram: true,
            ..CompileOptions::default()
        },
    )
    .expect("compiles")
    .prg_bytes;
    let r = emu64::run_prg_to_end(&img, common::MAX_INSNS).expect("run");
    assert_eq!(
        r.error_banner().as_deref(),
        Some("?FILE NOT OPEN  ERROR IN 10"),
        "extraram build must raise the same error"
    );
    let out = common::petscii_to_string(&r.output);
    assert_eq!(
        out.split('?').next().unwrap_or("").trim(),
        "",
        "nothing may print first under extraram either:\n{out}"
    );
}
