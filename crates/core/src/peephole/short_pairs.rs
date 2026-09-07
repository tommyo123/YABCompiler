//! Share short instruction sequences when ordinary layout cannot fit.
use super::{Item, JsrPolicy, ph301_with_window_min_sites};

pub(super) fn compact(items: &mut Vec<Item>) -> bool {
    let mut changed = false;
    for window in (2..=16).rev() {
        changed |= ph301_with_window_min_sites(items, window, JsrPolicy::CompactRefused, 2);
    }
    for window in (2..=16).rev() {
        changed |= ph301_with_window_min_sites(items, window, JsrPolicy::CompactLeafCalls, 2);
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peephole::{parse, render};



    #[test]
    fn leaf_call_pairs_preserve_actual_6502_values_flags_and_stack() {
        let mut source = String::from("*=$2000\n");
        for i in 0..6 {
            source.push_str(&format!(
                "LDA $0408\nJSR __SEQ_777\nSTA $3A\nSTA ${:04X}\n",
                0x400 + i
            ));
        }
        source.push_str("JMP $FFFE\n__SEQ_777:\nADC #$11\nSTA $39\nRTS\n__HEAP_BOTTOM:\n");
        let mut items = parse(&source);
        assert!(ph301_with_window_min_sites(
            &mut items,
            2,
            JsrPolicy::CompactLeafCalls,
            2
        ));
        let after = render(&items);
        let assemble = |source: &str| {
            asm6502::Assembler6502::new()
                .assemble_bytes(source)
                .unwrap()
        };
        let before = assemble(&source);
        let after = assemble(&after);
        assert!(after.len() < before.len());
        for value in 0..=255 {
            for flags in [0x20, 0x21, 0xEA, 0xEF] {
                let run = |bytes: &[u8]| {
                    let mut m = emu64::System::new();
                    m.mem.ram[0x2000..0x2000 + bytes.len()].copy_from_slice(bytes);
                    m.mem.ram[0x408] = value;
                    m.start_at(0x2000);
                    m.cpu.regs.p = flags;
                    m.cpu.regs.sp = 0x60;
                    m.cpu.regs.x = 17;
                    m.cpu.regs.y = 29;
                    for _ in 0..300 {
                        if m.cpu.regs.pc == 0xFFFE {
                            break;
                        }
                        assert!(m.step());
                    }
                    let r = &m.cpu.regs;
                    assert_eq!(r.pc, 0xFFFE);
                    assert_eq!(r.sp, 0x60);
                    (
                        m.mem.ram[0x400..0x406].to_vec(),
                        m.mem.ram[0x39..0x3b].to_vec(),
                        r.a,
                        r.x,
                        r.y,
                        r.p,
                    )
                };
                assert_eq!(run(&before), run(&after), "value={value}, flags={flags}");
            }
        }
    }

    #[test]
    fn longer_leaf_windows_preserve_carry_chains_on_actual_6502() {
        for window in [3, 4, 8, 16] {
            let mut source = String::from("*=$2000\n");
            for i in 0..6 {
                source.push_str("LDA $0408\nJSR __SEQ_777\n");
                for _ in 1..window {
                    source.push_str("ADC #$11\n");
                }
                source.push_str(&format!("STA ${:04X}\n", 0x400 + i));
            }
            source.push_str("JMP $FFFE\n__SEQ_777:\nEOR #$5A\nRTS\n__HEAP_BOTTOM:\n");
            let mut items = parse(&source);
            assert!(ph301_with_window_min_sites(
                &mut items,
                window,
                JsrPolicy::CompactLeafCalls,
                2
            ));
            let output = render(&items);
            let assemble = |s: &str| asm6502::Assembler6502::new().assemble_bytes(s).unwrap();
            let before = assemble(&source);
            let after = assemble(&output);
            assert!(after.len() < before.len());
            for value in 0..=255 {
                for flags in [0x20, 0x21, 0xEA, 0xEF] {
                    let run = |bytes: &[u8]| {
                        let mut m = emu64::System::new();
                        m.mem.ram[0x2000..0x2000 + bytes.len()].copy_from_slice(bytes);
                        m.mem.ram[0x408] = value;
                        m.start_at(0x2000);
                        m.cpu.regs.p = flags;
                        m.cpu.regs.sp = 0x60;
                        for _ in 0..500 {
                            if m.cpu.regs.pc == 0xFFFE {
                                break;
                            }
                            assert!(m.step());
                        }
                        let r = &m.cpu.regs;
                        assert_eq!(r.pc, 0xFFFE);
                        assert_eq!(r.sp, 0x60);
                        (m.mem.ram[0x400..0x406].to_vec(), r.a, r.x, r.y, r.p, r.sp)
                    };
                    assert_eq!(
                        run(&before),
                        run(&after),
                        "window={window}, value={value}, flags={flags}"
                    );
                }
            }
        }
    }

    #[test]
    fn leaf_call_proof_refuses_frame_memory_and_control_hazards() {
        for body in [
            "JSR $BBA2\nRTS",
            "JMP $BBA2",
            "TSX\nRTS",
            "PHA\nPLA\nRTS",
            "LDA ($03),Y\nRTS",
            "LDA $0100,X\nRTS",
            "INC $01\nRTS",
            "LDA $FE,X\nRTS",
            "BNE __DONE\n__DONE:\nRTS",
            "LDA #1\n; YAB_SYS_WRAP_IN\nSTA $39\n; YAB_SYS_WRAP_OUT\nRTS",
            "STA __PATCH+1\nRTS\n__PATCH:\nLDA #1\nRTS",
            "STA VI_FAKE\nRTS\nVI_FAKE:\nNOP\nRTS",
            "STA VI_FAKE\nRTS\nVI_FAKE = $2005",
            "__SYS_TXT_OUT:\nRTS",
        ] {
            let mut source = String::from("*=$2000\n");
            for i in 0..8 {
                source.push_str(&format!("site{i}:\nJSR __SEQ_777\nSTA $39\n"));
            }
            source.push_str(&format!("RTS\n__SEQ_777:\n{body}\n__HEAP_BOTTOM:\n"));
            assert!(
                !ph301_with_window_min_sites(
                    &mut parse(&source),
                    2,
                    JsrPolicy::CompactLeafCalls,
                    2
                ),
                "{body}"
            );
        }
        for name in [
            "__FOR_BYTES_CHK",
            "__GOSUB_CHK",
            "__SYS_TXT_OUT",
            "__STR_PRINT_I",
            "__UNKNOWN",
        ] {
            let mut source = String::from("*=$2000\n");
            for i in 0..8 {
                source.push_str(&format!("site{i}:\nJSR {name}\nSTA $39\n"));
            }
            source.push_str("RTS\n");
            if name != "__UNKNOWN" {
                source.push_str(&format!("{name}:\nRTS\n"));
            }
            assert!(
                !ph301_with_window_min_sites(
                    &mut parse(&source),
                    2,
                    JsrPolicy::CompactLeafCalls,
                    2
                ),
                "{name}"
            );
        }
    }

    #[test]
    fn leaf_call_proof_preserves_address_visible_code_aliases_and_callsite_markers() {
        let sites = (0..8)
            .map(|i| format!("site{i}:\nJSR __SEQ_777\nSTA $39\n"))
            .collect::<String>();
        for source in [
            format!("*=$2000\nLDA #<__SEQ_777\n{sites}RTS\n__SEQ_777:\nLDA #1\nRTS\n"),
            format!("*=$2000\nSTA __SEQ_777+1\n{sites}RTS\n__SEQ_777:\nLDA #1\nRTS\n"),
            format!("*=$2000\n{sites}RTS\n__SYS_TXT_OUT:\n__SEQ_777:\nLDA #1\nRTS\n"),
            format!(
                "*=$2000\n{sites}RTS\n__SEQ_777:\n{}RTS\n",
                "CLC\n".repeat(17)
            ),
            format!(
                "*=$2000\n{}RTS\n__SEQ_777:\nLDA #1\nRTS\n",
                sites.replace("STA $39", "; YAB_SYS_WRAP_IN\nSTA $39\n; YAB_SYS_WRAP_OUT")
            ),
        ] {
            assert!(
                !ph301_with_window_min_sites(
                    &mut parse(&source),
                    2,
                    JsrPolicy::CompactLeafCalls,
                    2
                ),
                "{source}"
            );
        }
    }

    #[test]
    fn declared_integer_and_string_homes_preserve_all_byte_values() {
        for prefix in ["VI_", "VS_"] {
            for equated in [false, true] {
                let name = format!("{prefix}VALUE");
                let mut source = String::from("*=$2000\n");
                for i in 0..6 {
                    source.push_str(&format!("LDA $0408\nSTA {name}\nSTX {name}+1\nLDA {name}\nLDY {name}+1\nSTA ${:04X}\n", 0x0400+i));
                }
                source.push_str("JMP $FFFE\n");
                if equated {
                    source.push_str(&format!("{name} = $F7\n"));
                } else {
                    source.push_str(&format!("{name}:\n.byte 0,0\n"));
                }
                let mut original = parse(&source);
                assert!(
                    !ph301_with_window_min_sites(&mut original, 4, JsrPolicy::Refused, 2),
                    "ordinary policy changed"
                );
                let mut items = parse(&source);
                assert!(compact(&mut items));
                let output = render(&items);
                let prepare = |source: &str| {
                    let mut a = asm6502::Assembler6502::new();
                    let bytes = a.assemble_bytes(source).unwrap();
                    let addr = a.symbols()[&name];
                    (bytes, addr)
                };
                let before = prepare(&source);
                let after = prepare(&output);
                assert!(after.0.len() < before.0.len());
                for value in 0..=255 {
                    for flags in [0x20, 0x21, 0x60, 0xE1] {
                        let execute = |(bytes, home): &(Vec<u8>, u16)| {
                            let mut m = emu64::System::new();
                            m.mem.ram[0x2000..0x2000 + bytes.len()].copy_from_slice(bytes);
                            m.mem.ram[0x0408] = value;
                            m.start_at(0x2000);
                            m.cpu.regs.p = flags;
                            m.cpu.regs.sp = 0xE0;
                            m.cpu.regs.x = 0xA5;
                            for _ in 0..300 {
                                if m.cpu.regs.pc == 0xFFFE {
                                    break;
                                }
                                assert!(m.step());
                            }
                            let r = &m.cpu.regs;
                            assert_eq!(r.pc, 0xFFFE);
                            assert_eq!(r.sp, 0xE0);
                            (
                                m.mem.ram[0x0400..0x0406].to_vec(),
                                m.mem.ram[usize::from(*home)..usize::from(*home) + 2].to_vec(),
                                r.a,
                                r.x,
                                r.y,
                                r.p,
                            )
                        };
                        assert_eq!(execute(&before), execute(&after));
                    }
                }
            }
        }
    }

    #[test]
    fn short_pairs_keep_inputs_outputs_flags_and_stack() {
        let mut source = String::from("*=$2000\n");
        for i in 0..6 {
            source.push_str(&format!(
                "LDA #${:02X}\nADC #$11\nSTA $61\nSTA ${:04X}\n",
                i * 43,
                0x400 + i
            ));
        }
        source.push_str(concat!("JMP $FFFE\n", "__HEAP_BOTTOM:\n"));
        let mut items = parse(&source);
        assert!(compact(&mut items));
        let output = render(&items);
        for flags in [0x20, 0x21, 0xEA, 0xEF] {
            let execute = |source: &str| {
                let mut a = asm6502::Assembler6502::new();
                a.set_origin(0x2000);
                let bytes = a.assemble_bytes(source).unwrap();
                let mut m = emu64::System::new();
                m.mem.ram[0x2000..0x2000 + bytes.len()].copy_from_slice(&bytes);
                m.start_at(0x2000);
                m.cpu.regs.p = flags;
                m.cpu.regs.x = 17;
                m.cpu.regs.y = 29;
                m.cpu.regs.sp = 0xE0;
                for _ in 0..300 {
                    if m.cpu.regs.pc == 0xFFFE {
                        break;
                    }
                    assert!(m.step());
                }
                assert_eq!(m.cpu.regs.pc, 0xFFFE);
                let r = &m.cpu.regs;
                (
                    bytes.len(),
                    (m.mem.ram[0x400..0x406].to_vec(), r.a, r.x, r.y, r.p, r.sp),
                )
            };
            let (before_bytes, before) = execute(&source);
            let (after_bytes, after) = execute(&output);
            assert!(after_bytes < before_bytes);
            assert_eq!(before, after);
        }
    }

    #[test]
    fn short_pairs_preserve_print_eligibility_and_banking_markers() {
        for pair in [
            "LDY #>S0\nJSR __STR_PRINT\n",
            "STA $39\n; YAB_SYS_WRAP_IN\nLDA #$02\n",
            "STA $39\n; YAB_SYS_WRAP_OUT\nLDA #$02\n",
            "LDA $61\nJSR __SYS_TXT_OUT\n",
        ] {
            let mut source = String::new();
            for i in 0..8 {
                source.push_str(&format!("site{i}:\n{pair}"));
            }
            source.push_str("RTS\n__HEAP_BOTTOM:\n");
            let mut items = parse(&source);
            assert!(
                !compact(&mut items),
                "unsafe or codec-costly pair:\n{source}"
            );
        }
    }
}
