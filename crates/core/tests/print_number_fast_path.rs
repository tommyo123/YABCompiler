mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn run(src: &str, profile: Profile) -> (Vec<u8>, Option<String>) {
    run_with(src, profile, false)
}

fn run_with(src: &str, profile: Profile, extraram: bool) -> (Vec<u8>, Option<String>) {
    let prg = tokenize_program(src).expect("tokenizes");
    let c = compile_with_options(
        &prg,
        CompileOptions {
            profile,
            extraram,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}"));
    let r = emu64::run_prg_to_end(&c.prg_bytes, common::MAX_INSNS).expect("runs");
    let err = r.error_banner();
    (r.output, err)
}

fn same_as_fout(src: &str) {
    let (want, want_err) = run(src, Profile::Size);
    for profile in [Profile::Speed, Profile::Default] {
        let (got, got_err) = run(src, profile);
        assert_eq!(
            common::petscii_to_string(&got),
            common::petscii_to_string(&want),
            "{profile:?} must print what FOUT prints:\n{src}"
        );
        assert_eq!(got_err, want_err);
    }
}

#[test]
fn integer_variables_and_literals_print_as_fout_prints_them() {
    same_as_fout(
        "10 FOR I=-12 TO 12:PRINT I;:NEXT:PRINT\n\
         20 A%=-32768:B%=32767:C%=0:D%=-1:PRINT A%;B%;C%;D%\n\
         30 PRINT 0;-0;1;-1;9;10;99;100;1000;10000;-10000;32767;-32768\n\
         40 FOR I=1 TO 30000 STEP 997:PRINT I;:NEXT:PRINT\n",
    );
}

#[test]
fn floats_holding_integers_print_as_fout_prints_them() {
    same_as_fout(
        "10 A=100:B=-7:C=65535:D=-65535:E=65536:F=123456:G=0:H=-0\n\
         20 PRINT A;B;C;D;E;F;G;H\n\
         30 PRINT A*2;B*3;A/4;A/3;C+1;C-1;-C;2^15;2^16;-2^15\n\
         40 FOR X=0 TO 4 STEP .5:PRINT X;X*10;:NEXT:PRINT\n\
         50 PRINT 1.5;-2.25;1E9;1E10;.001;1/3;-1/3;32767.5;65535.4;4.99999999\n",
    );
}

#[test]
fn a_number_printed_to_a_file_channel_goes_the_same_way() {
    same_as_fout("10 OPEN 1,3:PRINT#1,12;-34;5.5:CLOSE 1:PRINT 7\n");
}

#[test]
fn a_sweep_across_both_signs_prints_as_fout_prints_it() {
    let src = "10 FOR I=-32768 TO 32767 STEP 61:PRINT I;:NEXT\n\
               20 FOR X=-70000 TO 70000 STEP 487.25:PRINT X;INT(X);:NEXT\n";
    let prg = tokenize_program(src).expect("tokenizes");
    for profile in [Profile::Speed, Profile::Default] {
        let asm = compile_with_options(
            &prg,
            CompileOptions {
                profile,
                ..CompileOptions::default()
            },
        )
        .expect("compiles")
        .asm;
        assert!(asm.contains("__PN_SIGN:"), "the {profile:?} build uses the fast printers");
    }
    same_as_fout(src);
}

#[test]
fn the_fast_printers_reach_the_rom_through_the_extraram_jump_table() {
    let src = "10 A%=-123:B=4567:PRINT A%;B;B/2;-B\n";
    let (want, _) = run_with(src, Profile::Size, true);
    let (got, err) = run_with(src, Profile::Speed, true);
    assert_eq!(err, None);
    assert_eq!(common::petscii_to_string(&got), common::petscii_to_string(&want));
}

#[test]
fn default_prints_a_number_outside_any_loop_through_fout() {
    let prg = tokenize_program("10 A%=-123:B=4567:PRINT A%;B;B/2;-B\n").expect("tokenizes");
    let asm = compile_with_options(&prg, CompileOptions::default())
        .expect("compiles")
        .asm;
    assert!(!asm.contains("__PN_SIGN:"), "no loop prints a number:\n{asm}");
}
