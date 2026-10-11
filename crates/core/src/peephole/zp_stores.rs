//! Compact consecutive stores that initialize compiler-owned zero-page aliases.
use super::{Item, ZpEquates, insn_is_smc_target, is_sys_wrap_marker, smc_patched_labels};

pub(super) fn compact(items: &mut Vec<Item>) -> bool {
    let equ = ZpEquates::scan(items);
    let smc = smc_patched_labels(items);
    let private = super::structured::private_instruction_sites(items);
    let mut seen = [false; 256];
    let mut drop = Vec::new();
    for (idx, item) in items.iter().enumerate() {
        match item {
            Item::Blank => continue,
            Item::Comment(_) if !is_sys_wrap_marker(item) => continue,
            Item::Insn(insn)
                if insn.mnem == "STA" && private[idx] && !insn_is_smc_target(items, idx, &smc) =>
            {
                if let Some(addr) = insn.operand.as_deref().and_then(|op| owned_slot(op, &equ)) {
                    if seen[addr] {
                        drop.push(idx);
                    } else {
                        seen[addr] = true;
                    }
                    continue;
                }
            }
            _ => {}
        }
        // Includes labels, indexed/indirect writes, CPU banking registers, SMC templates and SYS markers that will become banking instructions.
        seen.fill(false);
    }
    let changed = !drop.is_empty();
    for idx in drop.into_iter().rev() {
        items.remove(idx);
    }
    changed
}

fn owned_slot(op: &str, equ: &ZpEquates) -> Option<usize> {
    let (base, offset) = op.strip_suffix("+1").map_or((op, 0), |base| (base, 1));
    if !["VI_", "FI_", "FU_", "ZSI_"]
        .iter()
        .any(|p| base.strip_prefix(p).is_some_and(|rest| !rest.is_empty()))
    {
        return None;
    }
    let addr = equ.map.get(base)?.checked_add(offset)?;
    (2..256).contains(&addr).then_some(usize::from(addr))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peephole::{parse, render};

    #[test]
    fn alias_runs_preserve_every_value_register_flag_and_stack() {
        let input = concat!(
            "*=$2000\nFU_0 = $F7\nFU_1 = $F7\nVI_I = $F6\nVI_J = $F8\n",
            "STA FU_0\nSTA VI_J\nSTA FU_1\nSTA VI_I+1\nJMP $FFFE\n"
        );
        let mut items = parse(input);
        assert!(compact(&mut items));
        let output = render(&items);
        assert_eq!(output.matches("STA ").count(), 2);
        assert!(!compact(&mut items), "idempotent");
        let mut a = asm6502::Assembler6502::new();
        a.set_origin(0x2000);
        let before = a.assemble_bytes(input).unwrap();
        let mut a = asm6502::Assembler6502::new();
        a.set_origin(0x2000);
        let after = a.assemble_bytes(&output).unwrap();
        assert_eq!(before.len() - after.len(), 4);
        for value in 0..=255 {
            for flags in [0x20, 0x21, 0xEA, 0xEF] {
                let execute = |bytes: &[u8]| {
                    let mut machine = emu64::System::new();
                    machine.mem.ram[0x2000..0x2000 + bytes.len()].copy_from_slice(bytes);
                    machine.mem.ram[0xF6..0xFA].fill(0xAB);
                    machine.start_at(0x2000);
                    machine.cpu.regs.a = value;
                    machine.cpu.regs.x = 0x79;
                    machine.cpu.regs.y = 0x13;
                    machine.cpu.regs.p = flags;
                    machine.cpu.regs.sp = 0xF0;
                    for _ in 0..10 {
                        if machine.cpu.regs.pc == 0xFFFE {
                            break;
                        }
                        assert!(machine.step());
                    }
                    assert_eq!(machine.cpu.regs.pc, 0xFFFE);
                    let r = &machine.cpu.regs;
                    (
                        machine.mem.ram[0xF6..0xFA].to_vec(),
                        r.a,
                        r.x,
                        r.y,
                        r.p,
                        r.sp,
                    )
                };
                assert_eq!(execute(&before), execute(&after));
            }
        }
    }

    #[test]
    fn calls_reads_writes_labels_and_raw_addresses_end_the_run() {
        for barrier in [
            "LDA #$01",
            "LDA FU_1",
            "JSR external",
            "STX FU_1",
            "STA (VI_I),Y",
            "STA VI_I,X",
            "STA $F7",
            "STA unknown",
            "entry:",
            ".byte $EA",
            "; YAB_SYS_WRAP_IN",
            "; YAB_SYS_WRAP_OUT",
            "*=$2100",
        ] {
            let mut items = parse(&format!(
                "FU_0 = $F7\nFU_1 = $F7\nSTA FU_0\n{barrier}\nSTA FU_1\n"
            ));
            assert!(!compact(&mut items), "crossed {barrier}");
        }
    }

    #[test]
    fn banking_io_nonprivate_and_out_of_page_equates_are_opaque() {
        for (name, addr, operand) in [
            ("VI_A", "$00", "VI_A"),
            ("VI_A", "$01", "VI_A"),
            ("VI_A", "$D020", "VI_A"),
            ("VI_A", "$FF", "VI_A+1"),
            ("OTHER", "$F7", "OTHER"),
        ] {
            let mut items = parse(&format!("{name} = {addr}\nSTA {operand}\nSTA {operand}\n"));
            assert!(!compact(&mut items), "changed {name} = {addr}");
        }
    }

    #[test]
    fn self_modifying_templates_remain() {
        let mut items = parse(concat!(
            "FU_0 = $F7\nFU_1 = $F7\nLDA #$F8\nSTA patch+1\n",
            "STA FU_0\npatch:\nSTA FU_1\nSTA FU_0\n"
        ));
        assert!(!compact(&mut items));
    }

    #[test]
    fn address_visible_instruction_suffixes_remain() {
        let mut items = parse(concat!(
            "FU_0 = $F7\nFU_1 = $F7\n.word entry+5\n",
            "entry:\nLDA #$00\nSTA FU_0\nSTA FU_1\nRTS\n"
        ));
        assert!(!compact(&mut items));
    }

    #[test]
    fn old_profiles_and_options_keep_their_output() {
        use crate::codegen::Profile;
        use crate::peephole::{Options, run_with_options};
        let source = concat!(
            "*=$2000\nFU_0 = $F7\nFU_1 = $F7\n",
            "STA FU_0\nSTA FU_1\nRTS\n"
        );
        for profile in [Profile::Default, Profile::Speed] {
            assert_eq!(
                run_with_options(source, profile, Options::default()),
                run_with_options(
                    source,
                    profile,
                    Options {
                        compact_size_dedup: true,
                        ..Options::default()
                    }
                )
            );
        }
        let before = run_with_options(source, Profile::Size, Options::default());
        let after = run_with_options(
            source,
            Profile::Size,
            Options {
                compact_size_dedup: true,
                ..Options::default()
            },
        );
        for out in [&before, &after] {
            assert_eq!(out.matches("STA FU_0").count(), 1, "{out}");
            assert!(!out.contains("STA FU_1"), "{out}");
        }
    }
}
