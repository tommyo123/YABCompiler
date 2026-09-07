//! String heap behavior near memory limits.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

/// Exercise string growth at the heap limit.
const CEILING_HIT: &str = "\
10 poke 56,13:clr
20 dim k$(40)
30 for i=0 to 40:k$(i)=str$(i)+\"abcdefghijklmnopqrstuvwxyz\":next
40 print \"no\"
";

/// Exercise garbage collection at the heap ceiling.
const SMALL_HEAP_LOW_CEILING: &str = "\
10 poke 56,13:clr
20 a$=\"1234567890\"
30 for i=1 to 40
40 b$=(a$+\"x\")+(str$(fre(0)*0)+(a$+\"y\"))
50 next
60 print len(b$);right$(b$,2)
";

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

fn runs_cleanly_printing(src: &str, out: &str) {
    for profile in PROFILES {
        let built = build(src, profile);
        let r = emu64::run_prg_to_end(&built.prg_bytes, common::MAX_INSNS).expect("run");
        assert_eq!(r.error_banner(), None, "({profile:?}):\n{src}");
        assert!(r.clean_exit, "({profile:?}):\n{src}");
        assert_eq!(
            common::petscii_to_string(&r.output).trim_end(),
            out,
            "({profile:?}):\n{src}"
        );
    }
}

#[test]
fn a_poked_ceiling_the_heap_grows_through_is_not_an_error_here() {
    runs_cleanly_printing(CEILING_HIT, "NO");
}

#[test]
fn a_small_heap_under_the_same_ceiling_must_keep_running() {

    runs_cleanly_printing(SMALL_HEAP_LOW_CEILING, " 15 0Y");
}

/// `POKE 56` with no `CLR` does not move the ROM's heap either ; it only copies MEMSIZ into FRETOP at `CLR` (or at a collection).
#[test]
fn poke_56_without_a_clr_moves_nothing_on_the_machine_either() {
    runs_cleanly_printing(
        "10 poke 56,13\n\
         20 dim k$(40)\n\
         30 for i=0 to 40:k$(i)=str$(i)+\"abcdefghijklmnopqrstuvwxyz\":next\n\
         40 print \"no\"\n",
        "NO",
    );
}

/// Structural: the statement bracket computes free space only to decide whether to collect, and hands control back either way.
#[test]
fn heap_ceiling_is_not_enforced() {
    let asm = build(CEILING_HIT, Profile::Default).asm;
    let body = |label: &str| {
        asm.split_once(label)
            .unwrap_or_else(|| panic!("{label} exists in:\n{asm}"))
            .1
            .split_once("\n    RTS")
            .unwrap_or_else(|| panic!("{label} ends in an RTS"))
            .0
            .to_string()
    };
    let begin = body("__HEAP_BEGIN_STMT:");
    assert!(
        begin.contains("JSR __HEAP_GC_IF_LOW"),
        "the bracket must still reach the collection trigger:\n{begin}"
    );
    let trigger = body("__HEAP_GC_IF_LOW:");
    assert!(
        trigger.contains("__GC_RUN"),
        "the trigger should still end in a collection:\n{trigger}"
    );
    for (name, b) in [("__HEAP_BEGIN_STMT", &begin), ("__HEAP_GC_IF_LOW", &trigger)] {
        assert!(
            !b.contains("LDX #$10"),
            "no ?OUT OF MEMORY may be raised from {name} — \
             read this file's module docs before adding one:\n{b}"
        );
    }
}
