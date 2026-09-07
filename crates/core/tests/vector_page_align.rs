mod common;

use yabcompiler_core::{
    CompileOptions, Dialect, Profile, compile_with_options, tokenize_program, tokenize_program_with,
};

/// A dispatching-NEXT program whose image length is tunable.
fn dispatch_program(pad: usize) -> String {
    format!(
        "10 FOR I=1 TO 3\n\
         20 PRINT \"A\";I\n\
         30 GOTO 100\n\
         40 NEXT\n\
         50 PRINT \"DONE I=\";I:END\n\
         100 PRINT \"{}\"\n\
         110 GOTO 40\n",
        "X".repeat(pad)
    )
}

/// What a real C64 prints for [`dispatch_program`], as a CHROUT stream.
fn expected_output(pad: usize) -> String {
    let x = "X".repeat(pad);
    format!("A 1 \n{x}\nA 2 \n{x}\nA 3 \n{x}\nDONE I= 4 \n")
}

fn compile(src: &str, profile: Profile) -> yabcompiler_core::Compiled {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(
        &prg,
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .expect("compiles")
}

/// Every image length in a full page, for all three profiles: no indirect-JMP vector may start
/// on the last byte of a page.
#[test]
fn no_indirect_vector_starts_on_a_page_boundary() {
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        for pad in 0..=260usize {
            let c = compile(&dispatch_program(pad), profile);
            let vectors = &c.diagnostics.indirect_vectors;
            assert!(
                !vectors.is_empty(),
                "pad={pad} {profile:?}: the dispatch shape stopped emitting any \
                 indirect JMP, so this sweep no longer covers anything"
            );
            for (name, addr) in vectors {
                assert_ne!(
                    addr & 0x00FF,
                    0x00FF,
                    "pad={pad} {profile:?}: JMP ({name}) vector at ${addr:04X} — an NMOS \
                     6502 reads its high byte from ${:04X}, not ${:04X}",
                    addr & 0xFF00,
                    addr.wrapping_add(1),
                );
            }
        }
    }
}

#[test]
fn sweep_actually_crosses_the_hazard() {
    let mut padded_by_vector: Vec<String> = Vec::new();
    for pad in 0..=260usize {
        let c = compile(&dispatch_program(pad), Profile::Default);
        if c.diagnostics.vector_pads == 0 {
            continue;
        }
        for (name, addr) in &c.diagnostics.indirect_vectors {
            if addr.wrapping_sub(1) & 0x00FF == 0x00FF && !padded_by_vector.contains(name) {
                padded_by_vector.push(name.clone());
            }
        }
    }
    padded_by_vector.sort();
    assert_eq!(
        padded_by_vector,
        vec!["__FOR_DISP_VEC".to_string(), "__FOR_EXIT_VEC".to_string()],
        "the 261-length sweep must land a vector on $xxFF for both vectors; \
         if it stopped doing so the guard above is no longer being exercised"
    );
}

#[test]
fn padded_images_produce_the_rom_answer() {
    let mut ran = 0;
    for pad in 0..=260usize {
        let c = compile(&dispatch_program(pad), Profile::Default);
        if c.diagnostics.vector_pads == 0 {
            continue;
        }
        let out = common::run_clean(&c.prg_bytes)
            .unwrap_or_else(|| panic!("pad={pad}: padded image did not reach a clean exit"));
        assert_eq!(out, expected_output(pad), "pad={pad}");
        ran += 1;
    }
    assert!(ran >= 2, "expected the sweep to exercise both vectors");
}

/// Padding is not a rewrite.
#[test]
fn dispatch_still_goes_through_the_vector() {
    let c = compile(&dispatch_program(3), Profile::Default);
    assert!(
        c.asm.contains("JMP (__FOR_DISP_VEC)"),
        "the dispatching NEXT still selects its handler through the vector"
    );
    assert!(
        c.asm.contains("JMP (__FOR_EXIT_VEC)"),
        "the handler still returns through the exit vector"
    );
    let names: Vec<&str> = c
        .diagnostics
        .indirect_vectors
        .iter()
        .map(|(n, _)| n.as_str())
        .collect();
    assert!(
        names.contains(&"__FOR_DISP_VEC") && names.contains(&"__FOR_EXIT_VEC"),
        "both vectors are accounted for: {names:?}"
    );
}

#[test]
fn programs_clear_of_the_boundary_are_untouched() {
    let mut clean = 0;
    for pad in 0..=64usize {
        let c = compile(&dispatch_program(pad), Profile::Default);
        if c.diagnostics.vector_pads > 0 {
            continue;
        }
        assert!(
            !c.asm.contains("NMOS 6502"),
            "pad={pad}: no pad was needed, so no pad comment should have been emitted"
        );
        clean += 1;
    }
    assert!(clean > 50, "the common case is still the common case");
}

/// Programs with no dispatching NEXT and no ON KEY have no vectors at all, so the pass has
/// nothing to look at and cannot perturb them.
#[test]
fn a_program_without_vectors_needs_no_pass() {
    let c = compile(
        "10 FOR I=1 TO 3\n20 PRINT I\n30 NEXT\n40 END\n",
        Profile::Default,
    );
    assert!(
        c.diagnostics.indirect_vectors.is_empty(),
        "static FOR/NEXT emits no indirect jump: {:?}",
        c.diagnostics.indirect_vectors
    );
    assert_eq!(c.diagnostics.vector_pads, 0);
}

/// Padding must not cost determinism: the same source has to give the same bytes every time,
/// pad and all.
#[test]
fn padding_is_deterministic() {
    for pad in [18usize, 20, 137] {
        let src = dispatch_program(pad);
        let a = compile(&src, Profile::Default);
        let b = compile(&src, Profile::Default);
        assert_eq!(a.prg_bytes, b.prg_bytes, "pad={pad}");
        assert_eq!(a.diagnostics.vector_pads, b.diagnostics.vector_pads);
        assert_eq!(
            a.diagnostics.indirect_vectors,
            b.diagnostics.indirect_vectors
        );
    }
}

/// The ON KEY trap dispatches through `__ON_KEY_TARGET`, a third vector in a different part of
/// the image.
#[test]
fn on_key_target_is_covered_too() {
    let src = "10 ON KEY \"A\" GOTO 100\n20 FOR I=1 TO 10\n30 PRINT \"{}\"\n\
               40 NEXT\n50 END\n100 PRINT \"K\":END\n";
    let mut hazards = 0;
    for pad in 0..=260usize {
        let prg = tokenize_program_with(&src.replace("{}", &"X".repeat(pad)), Dialect::Tsb)
            .expect("tokenizes as extended BASIC");
        let c = compile_with_options(
            &prg,
            CompileOptions {
                dialect: Dialect::Tsb,
                ..CompileOptions::default()
            },
        )
        .expect("compiles");
        assert!(c.asm.contains("JMP (__ON_KEY_TARGET)"), "pad={pad}");
        let (_, addr) = c
            .diagnostics
            .indirect_vectors
            .iter()
            .find(|(n, _)| n == "__ON_KEY_TARGET")
            .unwrap_or_else(|| panic!("pad={pad}: the ON KEY vector is in the checked set"));
        assert_ne!(addr & 0x00FF, 0x00FF, "pad={pad}: at ${addr:04X}");
        hazards += usize::from(c.diagnostics.vector_pads > 0);
    }
    assert!(
        hazards > 0,
        "the sweep must land a vector on $xxFF at least once, or it proves nothing"
    );
}

// $D000-$DFFF

/// The image the compiler hands back must never need bytes stored in the I/O block.
#[test]
fn an_image_running_into_the_io_area_is_refused() {
    let prg = tokenize_program("10 PRINT \"HI\"\n20 END\n").expect("tokenizes");
    let msg = match compile_with_options(
        &prg,
        CompileOptions {
            custom_start_address: Some(0xCFF0),
            ..CompileOptions::default()
        },
    ) {
        Err(e) => e.to_string(),
        Ok(c) => panic!(
            "an image reaching into $D000 must be refused, not reported as a success \
             (got ${:04X}..{} bytes)",
            c.diagnostics.start_address,
            c.prg_bytes.len()
        ),
    };
    for want in ["$D000", "I/O", "KERNAL LOAD"] {
        assert!(msg.contains(want), "diagnostic names {want}: {msg}");
    }
}

#[test]
fn an_image_stopping_below_the_io_area_still_compiles() {
    let prg = tokenize_program("10 PRINT \"HI\"\n20 END\n").expect("tokenizes");
    let c = compile_with_options(
        &prg,
        CompileOptions {
            custom_start_address: Some(0xC000),
            ..CompileOptions::default()
        },
    )
    .expect("a program that fits below $D000 compiles");
    let last = c.diagnostics.start_address as u32 + c.prg_bytes.len() as u32 - 3;
    assert!(last < 0xD000, "image really does end below the I/O block");
}
