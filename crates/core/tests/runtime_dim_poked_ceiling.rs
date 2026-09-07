//! The runtime-array allocator's ceiling, and what a poked MEMSIZ is allowed to do to it.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

struct Built {
    prg_bytes: Vec<u8>,
    asm: String,
}

fn build(src: &str, profile: Profile) -> Built {
    let prg = tokenize_program(src).expect("tokenizes");
    let c = compile_with_options(
        &prg,
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}\n{src}"));
    Built {
        prg_bytes: c.prg_bytes,
        asm: c.asm,
    }
}

fn run(src: &str, profile: Profile) -> (Option<String>, String) {
    let built = build(src, profile);
    let r = emu64::run_prg_to_end(&built.prg_bytes, common::MAX_INSNS).expect("run");
    (
        r.error_banner(),
        common::petscii_to_string(&r.output).trim_end().to_string(),
    )
}

fn runs_cleanly_printing(src: &str, out: &str) {
    for profile in PROFILES {
        let (banner, got) = run(src, profile);
        assert_eq!(banner, None, "({profile:?}) unexpected error:\n{src}");
        assert_eq!(got, out, "({profile:?}):\n{src}");
    }
}

fn raises_out_of_memory(src: &str) {
    for profile in PROFILES {
        let (banner, out) = run(src, profile);
        let banner = banner.unwrap_or_else(|| {
            panic!("({profile:?}) expected ?OUT OF MEMORY, program ran clean printing {out:?}:\n{src}")
        });
        assert!(
            banner.contains("OUT OF MEMORY"),
            "({profile:?}) expected ?OUT OF MEMORY, got {banner:?}:\n{src}"
        );
    }
}

const FILECOPY_SHAPE: &str = "\
1 poke55,0:poke56,32:clr
10 mx=peek(56)+10
20 dim f$(mx),f(mx),fs(mx),t(mx)
30 f(3)=7:f$(2)=\"ok\"
40 print f$(2);f(3)
";

#[test]
fn a_ceiling_poked_below_the_image_does_not_break_runtime_dim() {
    runs_cleanly_printing(FILECOPY_SHAPE, "OK 7");
}

#[test]
fn arrays_under_a_discarded_ceiling_still_store_and_load() {
    runs_cleanly_printing(
        "1 poke55,0:poke56,32:clr\n\
         10 n=40\n\
         20 dim a(n),b$(n)\n\
         30 for i=0 to n:a(i)=i*3:b$(i)=chr$(65+i/2):next\n\
         40 s=0:for i=0 to n:s=s+a(i):next\n\
         50 print s;b$(0);b$(n)\n",
        " 2460 AU",
    );
}

#[test]
fn an_honourable_lowered_ceiling_is_still_used() {
    runs_cleanly_printing(
        "1 poke55,0:poke56,40:clr\n\
         10 n=5\n\
         20 dim a(n)\n\
         30 a(4)=9\n\
         40 print peek(56);a(4)\n",
        " 40  9",
    );
}

#[test]
fn an_honourable_lowered_ceiling_still_runs_out_of_memory() {
    raises_out_of_memory(
        "1 poke55,0:poke56,40:clr\n\
         10 n=20000\n\
         20 dim a(n)\n\
         30 print a(1)\n",
    );
}

#[test]
fn an_impossible_dim_is_still_out_of_memory_without_any_poke() {
    raises_out_of_memory(
        "10 n=20000\n\
         20 dim a(n)\n\
         30 print a(1)\n",
    );
}

#[test]
fn the_boot_ceiling_is_captured_before_the_first_heap_init() {
    for profile in PROFILES {
        let asm = build(FILECOPY_SHAPE, profile).asm;
        let capture = asm
            .find("    STA __ARR_HEAP_BOOT\n")
            .unwrap_or_else(|| panic!("({profile:?}) no boot-ceiling capture:\n{asm}"));
        let first_init = asm
            .find("    JSR __HEAP_INIT\n")
            .unwrap_or_else(|| panic!("({profile:?}) no heap init call"));
        assert!(
            capture < first_init,
            "({profile:?}) the boot ceiling is captured after __HEAP_INIT has already run"
        );
    }
}

/// Structural, and the cost control: a program with no runtime `DIM` pays nothing for any of this.
#[test]
fn a_program_without_a_runtime_dim_pays_nothing() {
    for profile in PROFILES {
        let asm = build(
            "10 dim a(20)\n20 a(3)=1:b$=\"x\"\n30 print a(3);b$\n",
            profile,
        )
        .asm;
        assert!(
            !asm.contains("__ARR_HEAP_TOP"),
            "({profile:?}) fixture is not what it claims: it has a runtime DIM"
        );
        assert!(
            !asm.contains("__ARR_HEAP_BOOT"),
            "({profile:?}) boot-ceiling slot emitted for a program with no runtime DIM"
        );
    }
}

/// Omit heap-ceiling checks when the program cannot change MEMSIZ.
#[test]
fn a_runtime_dim_without_a_memsiz_store_pays_nothing() {
    for profile in PROFILES {
        let asm = build(
            // Dynamic array subscripts require runtime allocation without changing MEMSIZ.
            "10 n=peek(1024)\n20 dim a(n)\n30 a(3)=1\n40 print a(3)\n",
            profile,
        )
        .asm;
        assert!(
            asm.contains("__ARR_HEAP_TOP"),
            "({profile:?}) fixture is not what it claims: no runtime DIM"
        );
        assert!(
            !asm.contains("__ARR_HEAP_BOOT"),
            "({profile:?}) a program that cannot lower MEMSIZ is still paying \
             for the fallback:\n{asm}"
        );
        assert!(
            !asm.contains("__HI_MEMSIZ_OK"),
            "({profile:?}) the ceiling guard is emitted where MEMSIZ is never stored"
        );
    }
}

#[test]
fn peeking_memsiz_alone_does_not_pull_in_the_guard() {
    for profile in PROFILES {
        let asm = build(
            "10 n=peek(56)/8\n20 dim a(n)\n30 a(1)=2\n40 print a(1)\n",
            profile,
        )
        .asm;
        assert!(
            asm.contains("__ARR_HEAP_TOP"),
            "({profile:?}) fixture is not what it claims: no runtime DIM"
        );
        assert!(
            !asm.contains("__ARR_HEAP_BOOT"),
            "({profile:?}) reading MEMSIZ pulled in the fallback:\n{asm}"
        );
    }
}

/// Structural: the discard is a comparison against `__HEAP_BOTTOM`, not an unconditional overwrite.
#[test]
fn the_fallback_is_guarded_by_a_compare_against_heap_bottom() {
    let asm = build(FILECOPY_SHAPE, Profile::Default).asm;
    let init = asm
        .split_once("__HEAP_INIT:")
        .expect("__HEAP_INIT is emitted")
        .1;
    let init = &init[..init.find("\n    RTS").unwrap_or(init.len())];
    assert!(
        init.contains("SBC #<__HEAP_BOTTOM") && init.contains("SBC #>__HEAP_BOTTOM"),
        "the ceiling is no longer compared against __HEAP_BOTTOM:\n{init}"
    );
    assert!(
        init.contains("LDA __ARR_HEAP_BOOT"),
        "the boot ceiling is never read back:\n{init}"
    );
}
