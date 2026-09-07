mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

fn build(src: &str, profile: Profile) -> yabcompiler_core::Compiled {
    let tok = tokenize_program(src).unwrap_or_else(|e| panic!("tokenize: {e:?}"));
    compile_with_options(
        &tok,
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compile ({profile:?}): {e}"))
}

/// Run at every profile and require the ROM's banner, at `line`.
#[track_caller]
fn assert_illegal_quantity(src: &str, line: u16, prefix: &str) {
    for profile in PROFILES {
        let c = build(src, profile);
        let r = emu64::run_prg_to_end(&c.prg_bytes, common::MAX_INSNS).expect("emu64 runs");
        let out = common::petscii_to_string(&r.output);
        let want = format!("?ILLEGAL QUANTITY  ERROR IN {line}");
        assert_eq!(
            r.error_banner().as_deref(),
            Some(want.as_str()),
            "{profile:?}: {src:?}\n  must raise {want:?}, got {:?} with output {out:?}",
            r.error_banner()
        );
        assert!(
            out.starts_with(prefix),
            "{profile:?}: {src:?}\n  output before the banner must start {prefix:?}, got {out:?}"
        );
    }
}

/// Run at every profile and require a clean exit printing `want`.
#[track_caller]
fn assert_prints(src: &str, want: &str) {
    for profile in PROFILES {
        let c = build(src, profile);
        let r = emu64::run_prg_to_end(&c.prg_bytes, common::MAX_INSNS).expect("emu64 runs");
        let out = common::petscii_to_string(&r.output);
        assert_eq!(
            r.error_banner(),
            None,
            "{profile:?}: {src:?} must NOT raise an error, got {:?} / {out:?}",
            r.error_banner()
        );
        assert_eq!(out, want, "{profile:?}: {src:?}");
    }
}

#[test]
fn poke_value_from_a_negative_var_is_illegal_quantity() {

    assert_illegal_quantity(
        "10 POKE2,0:N=PEEK(2)-1:POKE 1024,N:PRINT PEEK(1024)\n",
        10,
        "",
    );
}

#[test]
fn left_dollar_length_from_a_negative_var_is_illegal_quantity() {

    assert_illegal_quantity("10 POKE2,0:N=PEEK(2)-1:PRINT LEFT$(\"ABCDEF\",N)\n", 10, "");
}

#[test]
fn the_minus_128_lower_bound_is_illegal_quantity_too() {

    assert_illegal_quantity(
        "10 POKE2,127:N=PEEK(2)-128:PRINT LEFT$(\"ABCDEF\",N)\n",
        10,
        "",
    );
}

#[test]
fn right_dollar_and_mid_dollar_share_the_byte_path() {

    assert_illegal_quantity(
        "10 POKE2,0:N=PEEK(2)-1:PRINT RIGHT$(\"ABCDEF\",N)\n",
        10,
        "",
    );
    assert_illegal_quantity(
        "10 POKE2,0:N=PEEK(2)-1:PRINT MID$(\"ABCDEF\",1,N)\n",
        10,
        "",
    );
}

#[test]
fn the_error_is_attributed_to_the_poke_line() {

    assert_illegal_quantity("10 POKE 2,0\n20 N=PEEK(2)-1\n30 POKE 1024,N\n", 30, "");
}

#[test]
fn poke_and_left_accept_the_high_bytes_of_a_signed_var() {
    // N is proved `[-1, 254]`, so its slot is one byte and the byte $C7 means 199 ; a perfectly legal POKE value.
    assert_prints(
        "10 POKE2,200:N=PEEK(2)-1:POKE 1024,N:PRINT PEEK(1024)\n",
        " 199 \n",
    );

    assert_prints(
        "10 POKE2,255:N=PEEK(2)-1:POKE 1024,N:PRINT PEEK(1024)\n",
        " 254 \n",
    );

    assert_prints(
        "10 POKE2,200:N=PEEK(2)-1:PRINT LEFT$(\"ABCDEF\",N)\n",
        "ABCDEF\n",
    );
}

#[test]
fn a_plain_unsigned_byte_var_pays_nothing() {
    // No negative value anywhere in the write set, so the var is not in `signed_byte_int_vars` and no compare is emitted at all.
    assert_prints("10 POKE2,7:N=PEEK(2):POKE 1024,N:PRINT PEEK(1024)\n", " 7 \n");
    for profile in PROFILES {
        let asm = build("10 POKE2,7:N=PEEK(2):POKE 1024,N\n", profile).asm;
        assert!(
            !asm.contains("__INTBY_OK_"),
            "{profile:?}: an unsigned byte var must not pay for a range check:\n{asm}"
        );
    }
}

#[test]
fn zero_is_a_legal_byte_and_a_legal_length() {

    assert_prints("10 POKE2,1:N=PEEK(2)-1:POKE 1024,N:PRINT PEEK(1024)\n", " 0 \n");
    assert_prints("10 POKE2,1:N=PEEK(2)-1:PRINT LEFT$(\"ABCDEF\",N);\"X\"\n", "X\n");
}

#[test]
fn the_bound_is_the_union_of_every_write_not_the_last_one() {

    assert_illegal_quantity(
        "10 POKE 2,4\n20 N=PEEK(2)-10\n30 IF PEEK(2)=99 THEN N=0-5\n\
         40 POKE 1024,N\n50 PRINT PEEK(1024)\n",
        40,
        "",
    );

    assert_prints(
        "10 POKE 2,255\n20 N=PEEK(2)-10\n30 IF PEEK(2)=99 THEN N=0-5\n\
         40 POKE 1024,N\n50 PRINT PEEK(1024)\n",
        " 245 \n",
    );
}

#[test]
fn adding_one_to_32767_is_illegal_quantity() {

    assert_illegal_quantity("10 A%=32767\n20 A%=A%+1:PRINT A%\n", 20, "");
}

#[test]
fn a_product_that_leaves_the_window_is_illegal_quantity() {

    assert_illegal_quantity("10 POKE2,200:A%=PEEK(2)*200:PRINT A%\n", 10, "");
    assert_illegal_quantity("10 POKE2,1:A%=32768*PEEK(2):PRINT A%\n", 10, "");
}

#[test]
fn the_loop_prints_what_fits_and_then_raises() {

    assert_illegal_quantity(
        "10 FOR I=1 TO 2:A%=I*20000:PRINT A%:NEXT\n",
        10,
        " 20000 \n",
    );
}

#[test]
fn an_integer_array_element_is_narrowed_like_a_scalar() {

    assert_illegal_quantity("10 A%(0)=32767:A%(0)=A%(0)+1:PRINT A%(0)\n", 10, "");
}

#[test]
fn a_bitwise_operand_is_narrowed_before_the_combine() {

    assert_illegal_quantity("10 POKE2,1:IF 32768*PEEK(2) AND 1 THEN PRINT 1\n", 10, "");
    assert_illegal_quantity("10 POKE2,1:PRINT 1 OR 32768*PEEK(2)\n", 10, "");
    assert_illegal_quantity("10 POKE2,1:PRINT NOT(32768*PEEK(2))\n", 10, "");
}

#[test]
fn a_not_operand_that_wraps_back_into_range_still_raises() {

    assert_illegal_quantity(
        "10 POKE2,1:POKE 1024,NOT(0-1-256*(256*PEEK(2))):PRINT PEEK(1024)\n",
        10,
        "",
    );

    assert_prints("10 POKE2,0:POKE 1024,NOT(0-1-256*(256*PEEK(2))):PRINT PEEK(1024)\n", " 0 \n");
}

#[test]
fn the_error_is_attributed_to_the_arithmetic_line() {

    assert_illegal_quantity("10 A%=32767\n20 A%=A%+1\n30 PRINT CHR$(A%)\n", 20, "");
}

#[test]
fn the_negative_end_of_the_window_is_where_the_rom_puts_it() {
    // AYINT's one asymmetry: it compares the exponent against 32768 and then permits a FAC that equals exactly -32768.
    assert_prints("10 A%=-32768:PRINT A%\n", "-32768 \n");
    assert_prints("10 A%=-32767:A%=A%-1:PRINT A%\n", "-32768 \n");
    assert_illegal_quantity("10 A%=-32768:A%=A%-1:PRINT A%\n", 10, "");
}

#[test]
fn ordinary_percent_arithmetic_still_runs() {
    assert_prints("10 A%=100:A%=A%+1:PRINT A%\n", " 101 \n");
    assert_prints("10 A%=32766:A%=A%+1:PRINT A%\n", " 32767 \n");
    assert_prints("10 POKE2,10:A%=PEEK(2)*200:PRINT A%\n", " 2000 \n");
    assert_prints("10 A%(0)=5:A%(0)=A%(0)+1:PRINT A%(0)\n", " 6 \n");
    assert_prints("10 POKE2,1:IF 3*PEEK(2) AND 1 THEN PRINT 1\n", " 1 \n");
    assert_prints(
        "10 FOR I=1 TO 3:A%=I*100:PRINT A%:NEXT\n",
        " 100 \n 200 \n 300 \n",
    );
}

#[test]
fn plain_increment_keeps_the_inc_chain() {

    let src = "10 POKE2,5:A%=PEEK(2):A%=A%+1:PRINT A%\n";
    assert_prints(src, " 6 \n");
    for profile in PROFILES {
        let asm = build(src, profile).asm;
        assert!(
            !asm.contains("__AYINT"),
            "{profile:?}: a provably in-window store must stay on the island:\n{asm}"
        );
        assert!(
            asm.contains("INC "),
            "{profile:?}: expected the INC chain:\n{asm}"
        );
    }
}

#[test]
fn a_percent_to_percent_copy_stays_on_the_island() {

    assert_prints("10 A%=1234:B%=A%:PRINT B%\n", " 1234 \n");
    for profile in PROFILES {
        let asm = build("10 A%=1234:B%=A%:PRINT B%\n", profile).asm;
        assert!(
            !asm.contains("__AYINT"),
            "{profile:?}: a `%`-to-`%` copy needs no narrowing:\n{asm}"
        );
    }
}
