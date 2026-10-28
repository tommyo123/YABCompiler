//! Direct byte access through ASC and MID$, including invalid indices.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

type Outcome = (Vec<u8>, Option<u8>, Option<u16>);

const BUDGET: u64 = 20_000_000;

fn interpreted(src: &str) -> Outcome {
    let prg = tokenize_program(src).expect("tokenizes");
    let r = emu64::run_basic_to_end(&prg, BUDGET).expect("runs in the interpreter");
    (r.output, r.error_code, r.error_line)
}

fn compiled(src: &str, profile: Profile) -> (Outcome, String) {
    let prg = tokenize_program(src).expect("tokenizes");
    let c = compile_with_options(
        &prg,
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}\n{src}"));
    let r = emu64::run_prg_to_end(&c.prg_bytes, BUDGET).expect("runs");
    ((r.output, r.error_code, r.error_line), c.asm)
}

fn same_as_the_rom(src: &str) -> Vec<String> {
    let want = interpreted(src);
    let mut asms = Vec::new();
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let (got, asm) = compiled(src, profile);
        assert_eq!(
            (common::petscii_to_string(&got.0), got.1, got.2),
            (common::petscii_to_string(&want.0), want.1, want.2),
            "{profile:?}:\n{src}"
        );
        asms.push(asm);
    }
    asms
}

const SETUP: &str = "10 READ A$,B$(2)\n\
    20 FOR I=1 TO 5:PRINT ASC(MID$(A$,I,1));ASC(MID$(B$(2),I));ASC(MID$(A$+B$(2),I+3,2));:NEXT\n\
    30 FOR I=1.5 TO 5.9:X=ASC(MID$(A$,I,1)):POKE 1024+I,ASC(MID$(A$,I)):PRINT X;:NEXT\n\
    40 J%=3:PRINT ASC(MID$(A$,J%,1));ASC(MID$(\"XYZ\",J%-1,1));ASC(MID$(A$,LEN(A$+B$(2))-9,1))\n\
    50 IF ASC(MID$(A$,2,1))=69 THEN PRINT \"E\"\n\
    90 DATA HELLO,WORLD\n";

#[test]
fn reads_the_byte_at_the_start() {
    let asms = same_as_the_rom(SETUP);
    for asm in asms {
        assert!(asm.contains("JSR __ASC_MID"), "{asm}");
        assert!(!asm.contains("JSR __STR_MID"), "no MID$ copy left:\n{asm}");
    }
}

#[test]
fn start_zero_past_the_end_or_out_of_range_is_illegal_quantity() {
    for tail in [
        "60 PRINT ASC(MID$(A$,0,1))",
        "60 PRINT ASC(MID$(A$,6,1))",
        "60 PRINT ASC(MID$(A$,5,1))",
        "60 PRINT ASC(MID$(C$,1,1))",
        "60 PRINT ASC(MID$(A$,256,1))",
        "60 PRINT ASC(MID$(A$,-1,1))",
        "60 PRINT ASC(MID$(A$,.5))",
        "60 PRINT ASC(MID$(\"\",1))",
        "60 PRINT ASC(MID$(B$(4),1,1))",
        "60 PRINT ASC(MID$(TI$,7))",
    ] {
        same_as_the_rom(&format!("{SETUP}{tail}\n"));
    }
}
