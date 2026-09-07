

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn build(src: &str, profile: Profile, extraram: bool) -> Vec<u8> {
    let prg = tokenize_program(src).expect("tokenizes");
    let opts = CompileOptions {
        profile,
        extraram,
        force_extraram_off: !extraram,
        ..CompileOptions::default()
    };
    compile_with_options(&prg, opts)
        .unwrap_or_else(|e| panic!("compiles ({profile:?}, extraram={extraram}): {e}"))
        .prg_bytes
}

fn asm_of(src: &str) -> String {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(&prg, CompileOptions::default())
        .expect("compiles")
        .asm
}
fn check_all_variants(src: &str, expected: &str) {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        for extraram in [false, true] {
            let bytes = build(src, profile, extraram);
            let got = common::run_clean(&bytes).unwrap_or_else(|| {
                panic!("runs to a clean exit ({profile:?}, extraram={extraram}):\n{src}")
            });
            assert_eq!(
                got, expected,
                "{profile:?} extraram={extraram} mismatch for:\n{src}"
            );
        }
    }
}
fn check_stops_with(src: &str, expected: &str) {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        for extraram in [false, true] {
            let bytes = build(src, profile, extraram);
            let r = emu64::run_prg_to_end(&bytes, 8_000_000).expect("runs");
            let got = common::petscii_to_string(&r.output);
            assert_eq!(
                got, expected,
                "{profile:?} extraram={extraram} output mismatch for:\n{src}"
            );
            assert!(
                !r.clean_exit,
                "{profile:?} extraram={extraram}: the BASIC error must abort the \
                 program, not let it end normally:\n{src}"
            );
        }
    }
}

/// The instructions of one labelled block, up to the next label.
fn block(asm: &str, label: &str) -> String {
    let start = asm
        .find(&format!("\n{label}:\n"))
        .unwrap_or_else(|| panic!("no {label}: in\n{asm}"))
        + label.len()
        + 3;
    let rest = &asm[start..];
    let end = rest
        .lines()
        .scan(0usize, |off, l| {
            let at = *off;
            *off += l.len() + 1;
            Some((at, l))
        })
        .find(|(_, l)| l.ends_with(':') && !l.starts_with(' '))
        .map(|(at, _)| at)
        .unwrap_or(rest.len());
    rest[..end].to_string()
}
#[test]
fn gosub_entry_inside_an_unclosed_for_body_reads_the_variables_own_home() {
    let src = "10 for i=1 to 3\n\
               20 on (i and 1)+1 gosub 100,200\n\
               30 print \"-\";:next\n\
               40 print:end\n\
               100 for i=8 to 9:print \"e\";i;:return\n\
               200 print \"o\";i;:return\n";
    check_all_variants(src, "O 1 -E 8 -\n");
}
#[test]
fn goto_back_into_a_finished_for_body_reads_the_variables_own_home() {
    let src = "10 a=0:for i=3 to 0 step -1\n\
               20 print i:next\n\
               30 if a=0 then a=1:for i=2 to 1 step -.5\n\
               40 if i=2 then 20\n\
               50 print i:next\n";
    check_all_variants(src, " 3 \n 2 \n 1 \n 0 \n 2 \n 1.5 \n 1 \n");
}
#[test]
fn goto_back_into_a_finished_for_body_named_next_reads_the_variables_own_home() {
    let src = "10 a=0:for i=3 to 0 step -1\n\
               20 print i:next i\n\
               30 if a=0 then a=1:for i=2 to 1 step -.5\n\
               40 if i=2 then 20\n\
               50 print i:next i\n";
    check_all_variants(src, " 3 \n 2 \n 1 \n 0 \n 2 \n 1.5 \n 1 \n");
}
#[test]
fn goto_into_a_never_entered_for_body_reads_the_variables_own_home() {
    let src = "10 for i=2 to 2\n\
               20 if i=2 then 60\n\
               30 for i=0 to 3\n\
               40 print i;\n\
               50 next:next:end\n\
               60 print \"x\";:goto 40\n";
    check_stops_with(src, "X 2 ");
}
#[test]
fn goto_into_a_finished_body_pokes_through_the_variables_own_home() {
    let src = "5 poke 899,0\n\
               10 a=0:for i=3 to 0 step -1\n\
               20 poke 900+i,9:next\n\
               30 if a=0 then a=1:for i=2 to 1 step -.5\n\
               40 if i=2 then 20\n\
               50 next\n\
               60 print peek(899)\n";
    check_all_variants(src, " 0 \n");
}

/// The strength-reduction half, induction flavour.
#[test]
fn goto_into_a_finished_body_does_not_read_a_stale_induction_slot() {
    let src = "10 for i=1 to 10\n\
               20 x=i*7.5\n\
               30 next\n\
               40 print x;\n\
               50 if q=0 then q=1:goto 20\n";
    check_stops_with(src, " 75 ");
    let asm = asm_of(
        "10 for i=1 to 10\n\
         20 x=i*7.5\n\
         30 next\n\
         40 print x;\n\
         50 if q=0 then q=1:goto 20\n",
    );
    assert!(
        !asm.contains("FB_"),
        "an entered body must not keep a per-iteration induction slot:\n{asm}"
    );
}

/// `NEXT I%` against `FOR I`. v2 gives them separate storage, so FNDFOR cannot match and the
/// `NEXT` is `?NEXT WITHOUT FOR`.
#[test]
fn next_on_the_percent_twin_does_not_close_the_float_loop() {
    check_stops_with(
        "10 for i=1 to 2\n\
         20 next i%\n\
         30 print \"no\";i\n",
        "",
    );
}
#[test]
fn entered_body_routes_the_read_through_v_var() {
    let asm = asm_of(
        "10 for i=1 to 3\n\
         20 on (i and 1)+1 gosub 100,200\n\
         30 print \"-\";:next\n\
         40 print:end\n\
         100 for i=8 to 9:print \"e\";i;:return\n\
         200 print \"o\";i;\"q\";:return\n",
    );
    assert!(
        !block(&asm, "L200").contains("FU_"),
        "line 200 is a GOSUB entry point inside line 100's unclosed body; \
         its read of I must not come from the counter slot:\n{asm}"
    );
}
#[test]
fn still_reads_the_counter_slot_when_the_body_is_closed() {
    let asm = asm_of(
        "10 for i=1 to 3\n\
         20 on (i and 1)+1 gosub 100,200\n\
         30 print \"-\";:next\n\
         40 print:end\n\
         100 for i=8 to 9:print \"e\";i;:next:return\n\
         200 print \"o\";i;\"q\";:return\n",
    );
    assert!(
        block(&asm, "L100").contains("LDY FU_0"),
        "line 100's own body still reads I out of the u8 counter slot:\n{asm}"
    );
    assert!(
        !block(&asm, "L200").contains("FU_"),
        "line 200 is outside every body and reads V_I:\n{asm}"
    );
}

/// A plain loop nothing branches into keeps the u8 counter redirection.
#[test]
fn still_reads_the_counter_slot_in_a_plain_loop() {
    let src = "10 for i=1 to 3\n\
               20 print i;\n\
               30 next\n\
               40 i=i/2:print i\n";
    assert!(
        asm_of(src).contains("LDY FU_0"),
        "an unentered loop body still reads the u8 counter slot:\n{}",
        asm_of(src)
    );
    check_all_variants(src, " 1  2  3  2 \n");
}

/// A branch target *inside* the body is not an entry from outside.
#[test]
fn still_reads_the_counter_slot_when_the_branch_target_is_in_the_body() {
    let src = "10 for i=1 to 4\n\
               20 if i=3 then 40\n\
               30 print i;\n\
               40 next\n\
               50 i=i/2:print i\n";
    assert!(
        asm_of(src).contains("LDY FU_0"),
        "a jump from inside the body to inside the body keeps the counter \
         slot:\n{}",
        asm_of(src)
    );
    check_all_variants(src, " 1  2  4  2.5 \n");
}
#[test]
fn still_allocates_an_induction_slot_for_an_unentered_loop() {
    let src = "10 for i=1 to 10\n\
               20 x=i*7.5\n\
               30 next\n\
               40 print x\n";
    assert!(
        asm_of(src).contains("FB_"),
        "loop-induction strength reduction still fires:\n{}",
        asm_of(src)
    );
    check_all_variants(src, " 75 \n");
}
#[test]
fn still_allocates_array_pointer_inductions_for_an_unentered_loop() {
    let asm = asm_of(
        "10 dim a$(50),b$(50),c$(50)\n\
         20 for j=1 to 40\n\
         30 a$(j)=a$(j)+\"x\"\n\
         40 b$(j)=b$(j)+\"y\"\n\
         50 c$(j)=c$(j)+\"z\"\n\
         60 a$(j)=a$(j)+b$(j)\n\
         70 b$(j)=b$(j)+c$(j)\n\
         80 c$(j)=c$(j)+a$(j)\n\
         90 next j\n\
         100 print a$(3);b$(3);c$(3)\n",
    );
    for slot in ["AP_0", "AP_1", "AP_2"] {
        assert!(
            asm.contains(slot),
            "array-pointer induction stopped firing: no {slot}:\n{asm}"
        );
    }
}
#[test]
fn still_int_promotes_a_counter_whose_next_names_the_same_variable() {
    let src = "10 for i=1 to 2\n\
               20 next i\n\
               30 print \"no\";i\n";
    let asm = asm_of(src);
    assert!(
        asm.contains("VI_I"),
        "I still promotes to integer storage:\n{asm}"
    );
    assert!(
        !asm.contains("\nV_I:"),
        "and gets no float slot at all:\n{asm}"
    );
    check_all_variants(src, "NO 3 \n");
}
