mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

type Outcome = (Vec<u8>, Option<u8>, Option<u16>);

fn build(src: &str, profile: Profile) -> yabcompiler_core::Compiled {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(
        &prg,
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}\n{src}"))
}

fn same_as_the_rom(src: &str) {
    let prg = tokenize_program(src).expect("tokenizes");
    let r = emu64::run_basic_to_end(&prg, 20_000_000).expect("runs in the interpreter");
    let want: Outcome = (r.output, r.error_code, r.error_line);
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let r = emu64::run_prg_to_end(&build(src, profile).prg_bytes, 20_000_000).expect("runs");
        assert_eq!(
            (common::petscii_to_string(&r.output), r.error_code, r.error_line),
            (common::petscii_to_string(&want.0), want.1, want.2),
            "{profile:?}:\n{src}"
        );
    }
}

#[test]
fn bytes_made_without_and_through_fac_land_where_the_rom_puts_them() {
    same_as_the_rom(
        "10 READ A,B:C%=7:DIM D%(3):D%(2)=9\n\
         20 POKE A,65:POKE A+B,B*2:POKE A*1+1,C%:POKE A+2,PEEK(A)+1:POKE A+4,A/1000\n\
         30 POKE A+5,D%(2):POKE A+6,C%+B:POKE A+7,PEEK(A+B)\n\
         40 FOR I=0 TO 7:PRINT PEEK(A+I);:NEXT\n\
         50 PRINT PEEK(A/2+A/2)+PEEK(A+B)\n\
         60 DATA 49152.5,3\n",
    );
}

#[test]
fn a_byte_past_255_is_illegal_quantity() {
    same_as_the_rom("10 READ A:POKE A,256\n20 DATA 49152\n");
}

#[test]
fn an_address_past_65535_or_below_0_is_illegal_quantity() {
    same_as_the_rom("10 READ A:POKE A,1\n20 DATA 65536\n");
    same_as_the_rom("10 READ A:PRINT PEEK(A)\n20 DATA -1\n");
}

#[test]
fn a_plain_byte_stores_through_linnum_and_a_fac_byte_copies_the_address() {
    let plain = build("10 READ A:POKE A,65\n20 DATA 49152\n", Profile::Default).asm;
    assert!(plain.contains("STA ($14),Y"), "{plain}");
    assert!(!plain.contains("__POKE_"), "{plain}");
    let fac = build("10 READ A:POKE A,A/1000\n20 DATA 49152\n", Profile::Default).asm;
    assert!(fac.contains("STA __POKE_"), "{fac}");
    let peek = build("10 READ A:PRINT PEEK(A)\n20 DATA 49152\n", Profile::Default).asm;
    assert!(peek.contains("LDA ($14),Y") && !peek.contains("__PEEK_"), "{peek}");
}
