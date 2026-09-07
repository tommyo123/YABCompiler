//! Command-line output, exit status, and compilation tests.

use std::path::PathBuf;
use std::process::{Command, Output};

fn yabc() -> Command {
    Command::new(env!("CARGO_BIN_EXE_yabcompiler"))
}

fn run(args: &[&str]) -> Output {
    yabc().args(args).output().expect("run yabcompiler")
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}
fn code(o: &Output) -> i32 {
    o.status.code().expect("terminated by signal")
}

#[test]
fn help_exits_zero_and_prints_usage_on_stdout() {
    for flag in ["--help", "-h", "help"] {
        let o = run(&[flag]);
        assert_eq!(code(&o), 0, "`{flag}` should succeed");
        let s = out(&o);
        assert!(s.contains("Usage:"), "`{flag}` should print usage to stdout, got {s:?}");
        assert!(s.contains("compile <in> <out>"), "`{flag}` usage should list the commands");
        assert!(err(&o).is_empty(), "`{flag}` should not write to stderr, got {:?}", err(&o));
    }
}

#[test]
fn version_exits_zero_and_prints_the_version_on_stdout() {
    for flag in ["--version", "-V"] {
        let o = run(&[flag]);
        assert_eq!(code(&o), 0, "`{flag}` should succeed");
        let s = out(&o);
        assert!(s.contains(yabcompiler_core::config::VERSION), "`{flag}` should print the version, got {s:?}");
    }
}

#[test]
fn no_arguments_is_a_usage_error_on_stderr() {
    let o = run(&[]);
    assert_eq!(code(&o), 2, "no arguments should exit 2");
    assert!(err(&o).contains("Usage:"), "usage belongs on stderr here");
    assert!(out(&o).is_empty(), "nothing should go to stdout");
}

#[test]
fn an_unknown_command_is_a_usage_error_on_stderr() {
    let o = run(&["frobnicate"]);
    assert_eq!(code(&o), 2, "an unknown command should exit 2");
    assert!(err(&o).contains("Usage:"), "usage belongs on stderr here");
    assert!(out(&o).is_empty(), "nothing should go to stdout");
}

#[test]
fn compile_without_an_output_path_is_a_usage_error() {
    let o = run(&["compile"]);
    assert_eq!(code(&o), 2, "`compile` with no paths should exit 2");
    assert!(err(&o).contains("compile:"), "should name the command, got {:?}", err(&o));
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join("yabc_cli_surface");
    std::fs::create_dir_all(&d).expect("scratch dir");
    d.join(name)
}

#[test]
fn compile_writes_a_loadable_image_and_exits_zero() {
    let src = scratch("hello.bas");
    let img = scratch("hello.prg");
    std::fs::write(&src, "10 PRINT \"HI\"\n20 END\n").expect("write source");
    let _ = std::fs::remove_file(&img);

    let o = yabc()
        .args(["compile", src.to_str().unwrap(), img.to_str().unwrap()])
        .output()
        .expect("run yabcompiler");
    assert_eq!(code(&o), 0, "compile should succeed: {}", err(&o));

    let bytes = std::fs::read(&img).expect("compile should have written the image");
    assert!(bytes.len() > 2, "image is empty");
    assert_eq!(
        (bytes[0], bytes[1]),
        (0x01, 0x08),
        "a .prg with no --start-address loads at $0801"
    );
}

#[test]
fn list_round_trips_a_source_file_to_its_listing() {
    let src = scratch("listing.bas");
    std::fs::write(&src, "10 PRINT \"HI\"\n").expect("write source");
    let o = run(&["list", src.to_str().unwrap()]);
    assert_eq!(code(&o), 0, "list should succeed: {}", err(&o));
    assert!(out(&o).contains("PRINT"), "listing should come back, got {:?}", out(&o));
}

#[test]
fn a_missing_input_file_fails_without_writing_an_image() {
    let img = scratch("never.prg");
    let _ = std::fs::remove_file(&img);
    let o = run(&["compile", "no/such/file.bas", img.to_str().unwrap()]);
    assert_ne!(code(&o), 0, "a missing input must not succeed");
    assert!(!img.exists(), "no image should be written for an input that cannot be read");
}
