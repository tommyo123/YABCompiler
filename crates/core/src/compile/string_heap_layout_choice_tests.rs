use super::*;

const ORIGIN: u16 = 0x9D00;

fn options() -> CompileOptions {
    CompileOptions {
        custom_start_address: Some(ORIGIN),
        auto_reserve: false,
        ..CompileOptions::default()
    }
}

fn image(window: u32) -> Compiled {
    Compiled {
        asm: format!("{}:\n__HEAP_PTR:\n__HEAP_BOTTOM:\n", extraram::MEMSIZ_APPLY),
        machine_code: vec![0; (0xD000 - window - u32::from(ORIGIN)) as usize],
        prg_bytes: vec![1, 2, 3],
        diagnostics: Diagnostics {
            extraram: ExtraRamDecision::AutoOn,
            ..Diagnostics::default()
        },
    }
}

#[test]
fn string_only_automatic_success_can_get_more_heap_without_a_dim_floor() {
    for already_size in [false, true] {
        let mut first = image(456);
        first.diagnostics.size_profile_fallback = already_size;
        assert_eq!(extraram_heap_window(&first, u32::from(ORIGIN)), Ok(None));
        let mut compact = image(2500);
        compact.prg_bytes = vec![4, 5];
        let chosen = prefer_compact_heap_headroom(first, &options(), || Ok(compact));
        assert_eq!(chosen.prg_bytes, [4, 5]);
        assert!(chosen.diagnostics.size_profile_fallback);
        assert!(chosen.diagnostics.compact_heap_layout);
        assert_eq!(
            chosen.diagnostics.size_profile_heap_improvement,
            Some((456, 2500))
        );
    }
}

#[test]
fn failed_compact_retry_keeps_success_and_its_existing_diagnostics() {
    let mut first = image(118);
    first.diagnostics.size_profile_fallback = true;
    first.diagnostics.size_profile_heap_improvement = Some((90, 118));
    let expected_asm = first.asm.clone();
    let expected_code = first.machine_code.clone();
    let chosen = prefer_compact_heap_headroom(first, &options(), || {
        Err(CompileError::Assemble {
            message: "reserved layout does not fit".into(),
            asm: None,
        })
    });
    assert_eq!(chosen.prg_bytes, [1, 2, 3]);
    assert_eq!(chosen.asm, expected_asm);
    assert_eq!(chosen.machine_code, expected_code);
    assert!(chosen.diagnostics.size_profile_fallback);
    assert!(!chosen.diagnostics.compact_heap_layout);
    assert_eq!(
        chosen.diagnostics.size_profile_heap_improvement,
        Some((90, 118))
    );
}

#[test]
fn equal_smaller_or_unmanaged_window_keeps_the_original_image() {
    for window in [118, 117, 0] {
        let mut compact = image(window);
        compact.prg_bytes.clear(); // A shorter file is not more free RAM.
        compact.diagnostics.bss_truncated = 900;
        let chosen = prefer_compact_heap_headroom(image(118), &options(), || Ok(compact));
        assert_eq!(chosen.prg_bytes, [1, 2, 3]);
        assert_eq!(chosen.diagnostics.size_profile_heap_improvement, None);
    }
    let mut compact = image(2500);
    compact.asm.clear();
    let chosen = prefer_compact_heap_headroom(image(118), &options(), || Ok(compact));
    assert_eq!(chosen.prg_bytes, [1, 2, 3]);
}

#[test]
fn each_layout_uses_its_own_reserved_ceiling_and_full_occupied_image() {
    let mut compact = image(2000);

    compact.diagnostics.effective_reserved = vec![(0xD000 - 1900, 0xD000 - 1800)];
    compact.prg_bytes.clear();
    let chosen = prefer_compact_heap_headroom(image(456), &options(), || Ok(compact));
    assert_eq!(chosen.prg_bytes, [1, 2, 3]);
    assert_eq!(
        managed_extraram_heap_window(&chosen, u32::from(ORIGIN)),
        Some(456)
    );
}

#[test]
fn soft_threshold_explicit_profiles_and_unmanaged_programs_do_not_retry() {
    let chosen = prefer_compact_heap_headroom(image(MIN_STRING_HEAP - 1), &options(), || {
        Ok(image(MIN_STRING_HEAP))
    });
    assert_eq!(
        chosen.diagnostics.size_profile_heap_improvement,
        Some((2047, 2048))
    );
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let window = if profile == Profile::Default {
            MIN_STRING_HEAP
        } else {
            456
        };
        let opts = CompileOptions {
            profile,
            ..options()
        };
        let chosen = prefer_compact_heap_headroom(image(window), &opts, || panic!("no retry"));
        assert_eq!(chosen.prg_bytes, [1, 2, 3]);
    }
    let mut ordinary = image(456);
    ordinary.diagnostics.extraram = ExtraRamDecision::ForcedOff;
    let mut self_managed = image(456);
    self_managed.asm = "__HEAP_PTR:\n__HEAP_BOTTOM:\n".into();
    let mut no_heap = image(456);
    no_heap.asm = format!("{}:\n", extraram::MEMSIZ_APPLY);
    for first in [ordinary, self_managed, no_heap] {
        let chosen = prefer_compact_heap_headroom(first, &options(), || panic!("no managed heap"));
        assert_eq!(chosen.prg_bytes, [1, 2, 3]);
    }
}

#[test]
fn automatic_string_heap_choice_uses_the_checked_compact_layout() {
    let mut source = String::from("10 A$=STR$(PEEK(828))\n");
    for i in 0..24u8 {
        let letter = char::from(b'A' + i);
        source.push_str(&format!(
            "{} PRINT \"{}AB{letter}\";\n",
            20 + u32::from(i) * 10,
            letter.to_string().repeat(177)
        ));
    }
    source.push_str("500 PRINT A$\n");
    let bytes = crate::tokenize_program(&source).unwrap();
    let roomy = CompileOptions {
        custom_start_address: Some(0x8C00),
        extraram: true,
        auto_reserve: false,
        ..CompileOptions::default()
    };
    let ordinary = compile_at_profile(&bytes, roomy.clone()).unwrap();
    let ceiling = ordinary.diagnostics.end_address + 701;
    let opts = CompileOptions {
        reserved_ranges: vec![(ceiling, 0xCFFF)],
        ..roomy
    };
    let first = compile_at_profile(&bytes, opts.clone()).unwrap();
    assert_eq!(managed_extraram_heap_window(&first, 0x8C00), Some(700));
    assert!(!first.asm.contains("__ARR_HEAP_TOP:"));
    let compact = compile_at_profile_rescue(
        &bytes,
        CompileOptions {
            profile: Profile::Size,
            ..opts.clone()
        },
        LayoutRescue::Compact,
    )
    .unwrap();
    let after = managed_extraram_heap_window(&compact, 0x8C00).unwrap();
    assert!(after > 700, "Compact must create more room: {after}");
    let chosen = compile_with_options(&bytes, opts).unwrap();
    assert_eq!(chosen.prg_bytes, compact.prg_bytes);
    assert!(chosen.diagnostics.compact_heap_layout);
    assert_eq!(
        chosen.diagnostics.size_profile_heap_improvement,
        Some((700, after))
    );
}

#[test]
fn early_rescue_successes_can_offer_one_checked_compact_improvement() {
    for rescue in [LayoutRescue::Dedup, LayoutRescue::PackedPrint] {
        let mut first = image(15);
        first.diagnostics.size_profile_fallback = true;
        let mut calls = 0;
        let chosen = prefer_rescued_heap_headroom(first, &options(), rescue, || {
            calls += 1;
            let mut compact = image(2897);
            compact.prg_bytes = vec![4, 5];
            Ok(compact)
        });
        assert_eq!(calls, 1);
        assert_eq!(chosen.prg_bytes, [4, 5]);
        assert!(chosen.diagnostics.compact_heap_layout);
        assert_eq!(
            chosen.diagnostics.size_profile_heap_improvement,
            Some((15, 2897))
        );
    }
}

#[test]
fn completed_compact_attempts_and_explicit_rescue_profiles_do_not_retry() {
    for (rescue, profile) in [
        (LayoutRescue::Compact, Profile::Default),
        (LayoutRescue::DataFirst, Profile::Default),
        (LayoutRescue::Dedup, Profile::Size),
        (LayoutRescue::PackedPrint, Profile::Speed),
    ] {
        let opts = CompileOptions {
            profile,
            ..options()
        };
        let chosen = prefer_rescued_heap_headroom(image(15), &opts, rescue, || {
            panic!("Compact is not an additional attempt for this choice")
        });
        assert_eq!(chosen.prg_bytes, [1, 2, 3]);
        assert!(!chosen.diagnostics.compact_heap_layout);
    }
}

#[test]
fn unavailable_or_non_improving_compact_keeps_the_early_rescue() {
    for rescue in [LayoutRescue::Dedup, LayoutRescue::PackedPrint] {
        for alternative in [None, Some(15), Some(14)] {
            let mut first = image(15);
            first.diagnostics.size_profile_fallback = true;
            let chosen = prefer_rescued_heap_headroom(first, &options(), rescue, || {
                alternative
                    .map(image)
                    .ok_or_else(|| CompileError::Assemble {
                        message: "Compact cannot fit this reserved image".into(),
                        asm: None,
                    })
            });
            assert_eq!(chosen.prg_bytes, [1, 2, 3]);
            assert!(chosen.diagnostics.size_profile_fallback);
            assert!(!chosen.diagnostics.compact_heap_layout);
            assert_eq!(chosen.diagnostics.size_profile_heap_improvement, None);
        }
    }
}
