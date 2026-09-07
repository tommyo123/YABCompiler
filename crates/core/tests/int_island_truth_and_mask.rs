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

/// `LDA $61` is FAC_EXP: the generic "is the FAC non-zero" fallback at the bottom of `emit_branch_skip_on_false`.
#[track_caller]
fn assert_stays_on_the_island(src: &str) {
    for profile in PROFILES {
        let asm = build(src, profile).asm;
        assert!(
            !asm.contains("LDA $61"),
            "{profile:?}: {src:?}\n  condition must not fall back to the FAC:\n{asm}"
        );
        assert!(
            asm.contains("    ORA "),
            "{profile:?}: {src:?}\n  expected the island's ORA of the two bytes:\n{asm}"
        );
    }
}

#[test]
fn a_sum_of_percent_vars_that_is_exactly_65536_is_true_on_the_rom() {
    // T01. 30000 + 30000 + 5536 = 65536: the ROM's FAC is non-zero and branches, the island's sixteen bits are all zero and does not.
    assert_prints(
        "10 READ A%,B%,C%\n20 IF A%+B%+C% THEN PRINT \"Y\"\n\
         30 PRINT \"Z\"\n40 DATA 30000,30000,5536\n",
        "Y\nZ\n",
    );
}

#[test]
fn the_negative_multiple_of_65536_is_reachable_with_two_operands() {

    assert_prints(
        "10 READ A%,B%\n20 IF A%+B% THEN PRINT \"Y\"\n\
         30 PRINT \"Z\"\n40 DATA -32768,-32768\n",
        "Y\nZ\n",
    );
}

#[test]
fn shadowed_float_vars_reach_the_same_multiple() {

    assert_prints(
        "10 POKE 2,0\n20 I=PEEK(2)+30000\n30 J=PEEK(2)+30000\n\
         40 K=PEEK(2)+5536\n50 IF I+J+K THEN PRINT \"Y\"\n60 PRINT \"Z\"\n",
        "Y\nZ\n",
    );
}

#[test]
fn a_range_that_straddles_the_multiple_is_refused() {

    let src = "10 POKE 2,0\n20 I=0-32768\n30 IF PEEK(2)=99 THEN I=32767\n\
               40 J=0-32768\n50 IF PEEK(2)=98 THEN J=32767\n\
               60 IF I+J THEN PRINT \"Y\"\n70 PRINT \"Z\"\n";
    assert_prints(src, "Y\nZ\n");

    for profile in PROFILES {
        let asm = build(src, profile).asm;
        assert!(
            asm.contains("LDA $61") && !asm.contains("    ORA "),
            "{profile:?}: this condition must leave the island for the FAC:\n{asm}"
        );
    }
}

#[test]
fn keeps_the_island_where_no_multiple_of_65536_is_reachable() {
    // Three ranges that all leave AYINT's window and none of which can hold a non-zero multiple of 65536.
    let straddles = "10 POKE 2,0\n20 I=0-32000\n30 IF PEEK(2)=99 THEN I=32000\n\
                     40 J=0-32000\n50 IF PEEK(2)=98 THEN J=32000\n\
                     60 IF I+J THEN PRINT \"Y\"\n70 PRINT \"Z\"\n";

    let negative = "10 POKE 2,0\n20 I=PEEK(2)-30000\n30 J=PEEK(2)-30000\n\
                    40 K=PEEK(2)-30000\n50 IF I+J+K THEN PRINT \"Y\"\n60 PRINT \"Z\"\n";
    // T09 ; `[60000, 60510]`, wholly between 0 and 65536.
    let positive = "10 POKE 2,0\n20 I=PEEK(2)+40000\n30 J=PEEK(2)+20000\n\
                    40 IF I+J THEN PRINT \"Y\"\n50 PRINT \"Z\"\n";
    for src in [straddles, negative, positive] {
        assert_prints(src, "Y\nZ\n");
        assert_stays_on_the_island(src);
    }
}

#[test]
fn an_in_window_sum_still_branches_on_the_island() {

    let src = "10 READ A%,B%\n20 IF A%+B% THEN PRINT \"Y\"\n\
               30 PRINT \"Z\"\n40 DATA 100,200\n";
    assert_prints(src, "Y\nZ\n");
    assert_stays_on_the_island(src);
}

#[test]
fn and_mask_over_a_float_sum_raises_where_the_rom_raises() {

    assert_illegal_quantity(
        "10 POKE 2,0\n20 I=PEEK(2)+30000\n30 J=PEEK(2)+30000\n\
         40 K=PEEK(2)+5601\n50 PRINT CHR$((I+J+K) AND 255)\n60 PRINT \"Z\"\n",
        50,
        "",
    );
}

#[test]
fn and_mask_over_a_percent_sum_raises_where_the_rom_raises() {

    assert_illegal_quantity(
        "10 READ A%,B%,C%\n20 POKE 1024,(A%+B%+C%) AND 255\n\
         30 PRINT \"Z\"\n40 DATA 30000,30000,5601\n",
        20,
        "",
    );
}

#[test]
fn a_mask_over_an_in_window_value_above_255_still_wraps() {
    // T10. 1 + 200 + 200 = 401, which AYINT accepts, so the mask is the user's own truncation and the answer is 145.
    let src = "10 POKE 2,200\n20 POKE 1024,(1+PEEK(2)+PEEK(2)) AND 255\n\
               30 PRINT PEEK(1024)\n";
    assert_prints(src, " 145 \n");
    for profile in PROFILES {
        let asm = build(src, profile).asm;
        assert!(
            !asm.contains("__AYINT") && !asm.contains("__FAC_BYTE"),
            "{profile:?}: an in-window masked value must stay on the byte path:\n{asm}"
        );
    }
}

#[test]
fn a_mask_over_a_plain_byte_expression_pays_nothing() {

    let src = "10 POKE 2,7\n20 POKE 1024,(PEEK(2)+1) AND 255\n30 PRINT PEEK(1024)\n";
    assert_prints(src, " 8 \n");
    for profile in PROFILES {
        let asm = build(src, profile).asm;
        assert!(
            !asm.contains("__AYINT") && !asm.contains("__FAC_BYTE"),
            "{profile:?}: a byte-sized masked value must not be narrowed:\n{asm}"
        );
    }
}

#[test]
fn a_percent_var_operand_is_accepted_structurally() {

    assert_prints("10 READ A%\n20 POKE 1024,A% AND 255\n30 PRINT PEEK(1024)\n40 DATA -1\n", " 255 \n");
}

#[test]
fn a_mask_above_255_raises_where_the_rom_raises() {

    for (mask, data) in [(511, 300), (1023, 1000), (32767, 300)] {
        assert_illegal_quantity(
            &format!(
                "10 READ A%\n20 POKE 1024,A% AND {mask}\n\
                 30 PRINT PEEK(1024)\n40 DATA {data}\n"
            ),
            20,
            "",
        );
    }
}

#[test]
fn a_mask_above_255_over_a_negative_operand_raises() {
    // Masking -1 with 511 produces 511, outside the byte range.
    assert_illegal_quantity(
        "10 READ A%\n20 POKE 1024,A% AND 511\n30 PRINT PEEK(1024)\n40 DATA -1\n",
        20,
        "",
    );
}

#[test]
fn the_other_byte_consumer_raises_too() {

    assert_illegal_quantity(
        "10 READ A%\n20 PRINT CHR$(A% AND 511)\n30 PRINT \"Z\"\n40 DATA 300\n",
        20,
        "",
    );
    assert_illegal_quantity(
        "10 POKE 2,200\n20 POKE 1024,(1+PEEK(2)+PEEK(2)) AND 511\n\
         30 PRINT PEEK(1024)\n",
        20,
        "",
    );
}

#[test]
fn a_mask_whose_low_byte_is_not_ff_changes_the_value_with_no_banner() {

    assert_prints(
        "10 READ A%\n20 POKE 1024,A% AND 256\n30 PRINT PEEK(1024)\n40 DATA 1\n",
        " 0 \n",
    );
    assert_prints(
        "10 READ A%\n20 POKE 1024,A% AND 3840\n30 PRINT PEEK(1024)\n40 DATA 255\n",
        " 0 \n",
    );
    assert_prints(
        "10 READ A%\n20 POKE 1024,A% AND 512\n30 PRINT PEEK(1024)\n40 DATA 300\n",
        " 0 \n",
    );
}

#[test]
fn a_mask_above_255_whose_result_is_legal_must_not_gain_a_banner() {

    assert_prints(
        "10 READ A%\n20 POKE 1024,A% AND 511\n30 PRINT PEEK(1024)\n40 DATA 100\n",
        " 100 \n",
    );
    assert_prints(
        "10 POKE 2,7\n20 POKE 1024,(PEEK(2)+1) AND 511\n30 PRINT PEEK(1024)\n",
        " 8 \n",
    );
    assert_prints(
        "10 READ A%\n20 POKE 1024,A% AND 32767\n30 PRINT PEEK(1024)\n40 DATA 65\n",
        " 65 \n",
    );
}

#[test]
fn a_negative_operand_under_a_wide_mask_raises() {

    for (mask, line) in [(511u32, 30u16), (256, 30)] {
        assert_illegal_quantity(
            &format!(
                "10 POKE 2,0\n20 N=PEEK(2)-1\n30 POKE 1024,N AND {mask}\n\
                 40 PRINT PEEK(1024)\n"
            ),
            line,
            "",
        );
    }

    assert_illegal_quantity(
        "10 POKE 2,0\n20 N=0-1\n30 IF PEEK(2)=99 THEN N=254\n\
         40 POKE 1024,N AND 511\n50 PRINT PEEK(1024)\n",
        40,
        "",
    );

    let ok = "10 POKE 2,0\n20 N=PEEK(2)-1\n30 POKE 1024,N AND 255\n40 PRINT PEEK(1024)\n";
    assert_prints(ok, " 255 \n");
    for profile in PROFILES {
        let asm = build(ok, profile).asm;
        assert!(
            !asm.contains("__AYINT") && !asm.contains("__FAC_BYTE"),
            "{profile:?}: mask 255 must not be dragged into the correction:\n{asm}"
        );
    }
}

#[test]
fn the_mask_is_part_of_the_program() {

    for profile in PROFILES {
        let a = build(
            "10 READ A%\n20 POKE 1024,A% AND 255\n30 PRINT PEEK(1024)\n40 DATA 300\n",
            profile,
        );
        for mask in [256, 511, 512, 1023, 3840, 32767] {
            let b = build(
                &format!(
                    "10 READ A%\n20 POKE 1024,A% AND {mask}\n\
                     30 PRINT PEEK(1024)\n40 DATA 300\n"
                ),
                profile,
            );
            assert_ne!(
                a.prg_bytes, b.prg_bytes,
                "{profile:?}: `AND 255` and `AND {mask}` compiled to the same image \
                 — the mask is being discarded, not applied"
            );
        }
    }
}

#[test]
fn a_wide_mask_over_an_out_of_window_operand_still_raises() {

    for mask in [511, 256] {
        assert_illegal_quantity(
            &format!(
                "10 READ A%,B%,C%\n20 POKE 1024,(A%+B%+C%) AND {mask}\n\
                 30 PRINT PEEK(1024)\n40 DATA 30000,30000,5601\n"
            ),
            20,
            "",
        );
    }
}

#[test]
fn a_product_with_no_range_fact_is_refused_by_truthiness() {

    let src = "10 READ A%,B%\n20 IF A%*B% THEN PRINT\"Y\"\n30 PRINT\"Z\"\n40 DATA 1024,64\n";
    assert_prints(src, "Y\nZ\n");
}

#[test]
fn a_product_that_cannot_reach_the_multiple_keeps_the_island() {

    let src = "10 READ A%,B%\n20 IF A%*B% THEN PRINT\"Y\"\n30 PRINT\"Z\"\n40 DATA 3,4\n";
    assert_prints(src, "Y\nZ\n");
}
