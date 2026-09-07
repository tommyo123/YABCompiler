use super::*;

const ORIGIN: u16 = 0x9D00;

fn options() -> CompileOptions {
    CompileOptions {
        custom_start_address: Some(ORIGIN),
        auto_reserve: false,
        ..CompileOptions::default()
    }
}

fn dim_program() -> Vec<u8> {
    crate::source::tokenize_program("10 N=PEEK(53280)\n20 DIM A(N)\n30 A(0)=1\n40 PRINT A(0)\n")
        .unwrap()
}

fn image(window: u32) -> Compiled {
    Compiled {
        asm: format!("{}:\n__ARR_HEAP_TOP:\n", extraram::MEMSIZ_APPLY),
        machine_code: vec![0; (0xD000 - window - u32::from(ORIGIN)) as usize],

        prg_bytes: vec![1, 2, 3],
        diagnostics: Diagnostics {
            extraram: ExtraRamDecision::AutoOn,
            ..Diagnostics::default()
        },
    }
}

#[test]
fn successful_default_dim_build_selects_more_headroom_from_size() {
    let bytes = dim_program();
    let initial = compile_at_profile(&bytes, options()).unwrap();
    let end = u32::from(ORIGIN) + initial.machine_code.len() as u32;
    let ceiling = u16::try_from(end + MIN_EXTRARAM_DIM_HEAP + 300).unwrap();
    let opts = CompileOptions {
        reserved_ranges: vec![(ceiling, ceiling + 255)],
        ..options()
    };
    let default = compile_at_profile(&bytes, opts.clone()).unwrap();
    let size = compile_at_profile(
        &bytes,
        CompileOptions {
            profile: Profile::Size,
            ..opts.clone()
        },
    )
    .unwrap();
    let Ok(Some(before)) = extraram_heap_window(&default, u32::from(ORIGIN)) else {
        panic!("fixture must have a verified DIM heap window");
    };
    let Ok(Some(after)) = extraram_heap_window(&size, u32::from(ORIGIN)) else {
        panic!("Size must keep the same managed DIM heap");
    };
    assert!(before >= MIN_EXTRARAM_DIM_HEAP && before < MIN_STRING_HEAP);
    assert!(after > before, "fixture must offer a real Size improvement");
    // Check the ordinary DIM-to-Size decision directly.
    let expected_size = size.prg_bytes.clone();
    let chosen = prefer_dim_heap_headroom(default, &opts, || Ok(size));
    assert_eq!(chosen.prg_bytes, expected_size);
    assert!(chosen.diagnostics.size_profile_fallback);
    assert_eq!(
        chosen.diagnostics.size_profile_heap_improvement,
        Some((before, after))
    );
}

#[test]
fn failed_size_attempt_retains_the_successful_default_image() {
    let first = image(700);
    let expected = first.prg_bytes.clone();
    let out = prefer_dim_heap_headroom(first, &options(), || {
        Err(CompileError::Assemble {
            message: "Size cannot fit this reserved layout".into(),
            asm: None,
        })
    });
    assert_eq!(out.prg_bytes, expected);
    assert!(!out.diagnostics.size_profile_fallback);
    assert_eq!(out.diagnostics.size_profile_heap_improvement, None);
}

#[test]
fn a_smaller_file_without_a_larger_verified_heap_is_not_selected() {
    for window in [700, 699, MIN_EXTRARAM_DIM_HEAP - 1] {
        let first = image(700);
        let mut retry = image(window);
        retry.prg_bytes.clear();
        let out = prefer_dim_heap_headroom(first, &options(), || Ok(retry));
        assert_eq!(out.prg_bytes, [1, 2, 3]);
        assert!(!out.diagnostics.size_profile_fallback);
    }
    // Size can emit a shorter image yet discover a lower reserved ceiling.
    let first = image(700);
    let mut retry = image(1200);
    retry.diagnostics.effective_reserved = vec![(0xD000 - 600, 0xD000 - 1)];
    retry.prg_bytes.clear();
    let out = prefer_dim_heap_headroom(first, &options(), || Ok(retry));
    assert_eq!(out.prg_bytes, [1, 2, 3]);
    assert!(!out.diagnostics.size_profile_fallback);
}

#[test]
fn soft_threshold_and_explicit_profiles_do_not_become_new_refusals() {
    let chosen = prefer_dim_heap_headroom(image(MIN_STRING_HEAP - 1), &options(), || {
        Ok(image(MIN_STRING_HEAP))
    });
    assert_eq!(
        chosen.diagnostics.size_profile_heap_improvement,
        Some((MIN_STRING_HEAP - 1, MIN_STRING_HEAP))
    );
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let window = if profile == Profile::Default {
            MIN_STRING_HEAP
        } else {
            700
        };
        let opts = CompileOptions {
            profile,
            ..options()
        };
        let out = prefer_dim_heap_headroom(image(window), &opts, || {
            panic!("roomy Default and explicit Speed/Size must not retry")
        });
        assert!(!out.diagnostics.size_profile_fallback);
    }
}

#[test]
fn unverified_alternative_heap_preserves_the_successful_original() {
    let first = image(700);
    let mut retry = image(1200);
    retry.asm.clear();
    let out = prefer_dim_heap_headroom(first, &options(), || Ok(retry));
    assert_eq!(extraram_heap_window(&out, u32::from(ORIGIN)), Ok(Some(700)));
    assert!(!out.diagnostics.size_profile_fallback);
}

#[test]
fn plain_layout_static_dim_and_self_managed_memsiz_skip_the_preference() {
    let dynamic = dim_program();
    let static_dim = crate::source::tokenize_program("10 DIM A(10):PRINT A(0)\n").unwrap();
    let self_managed = crate::source::tokenize_program(
        "10 POKE55,0:POKE56,208\n20 N=PEEK(53280):DIM A(N)\n30 PRINT A(0)\n",
    )
    .unwrap();
    for (bytes, opts) in [
        (dynamic, CompileOptions::default()),
        (static_dim, options()),
        (self_managed, options()),
    ] {
        let first = compile_at_profile(&bytes, opts.clone()).unwrap();
        let origin = opts
            .custom_start_address
            .map(u32::from)
            .unwrap_or(CODE_ORIGIN);
        assert_eq!(extraram_heap_window(&first, origin), Ok(None));
        let expected = first.prg_bytes.clone();
        let out = prefer_dim_heap_headroom(first, &opts, || {
            panic!("no relocatable runtime-DIM heap to improve")
        });
        assert_eq!(out.prg_bytes, expected);
        assert!(!out.diagnostics.size_profile_fallback);
    }
}
