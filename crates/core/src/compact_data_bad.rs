//! Compact metadata for DATA intervals rejected by numeric READ.

#[derive(Clone, Debug)]
pub(crate) struct CompactDataBad {
    pub start_offset: usize,
    pub first_line: u16,
    pub bytes: Vec<u8>,
}

impl CompactDataBad {
    pub fn new(lengths: &[usize], runs: &[(usize, usize, u16)]) -> Option<Self> {
        let &(first, _, first_line) = runs.first()?;
        let mut offsets = Vec::with_capacity(lengths.len() + 1);
        offsets.push(0usize);
        for &len in lengths {
            offsets.push(offsets.last()?.checked_add(len)?);
        }
        let start_offset = *offsets.get(first)?;
        let mut previous_end = start_offset;
        let mut previous_line = first_line;
        let mut bytes = Vec::new();
        for &(first, last, line) in runs {
            let start = *offsets.get(first)?;
            let end = *offsets.get(last.checked_add(1)?)?;
            let span = u8::try_from(end.checked_sub(start)?).ok()?;
            if span == 0 || span > 127 {
                return None;
            }
            let gap = u8::try_from(start.checked_sub(previous_end)?).ok()?;
            let delta = u8::try_from(line.checked_sub(previous_line)?).ok()?;
            bytes.extend([span | if gap == 0 { 0 } else { 128 }, delta]);
            if gap != 0 {
                bytes.push(gap);
            }
            previous_end = end;
            previous_line = line;
        }
        bytes.push(0);
        // Pay for the larger decoder and five private scratch bytes before offering the format.
        if bytes.len().checked_add(128)? >= runs.len().checked_mul(6)?.checked_add(2)? {
            return None;
        }
        Some(Self {
            start_offset,
            first_line,
            bytes,
        })
    }

    pub fn helper(&self, cursor: u8, ptr_lo: &str, ptr_hi: &str) -> String {
        let cursor_hi = cursor + 1;
        let anchor = self.start_offset;
        let line_lo = self.first_line as u8;
        let line_hi = (self.first_line >> 8) as u8;

        format!(
            "__READ_NUM:
    LDA #<__DATA_BAD
    STA ${cursor:02X}
    LDA #>__DATA_BAD
    STA ${cursor_hi:02X}
    LDA #<(__DATA+{anchor})
    STA __RDN_POS
    LDA #>(__DATA+{anchor})
    STA __RDN_POS+1
    LDA #${line_lo:02X}
    STA __RDN_LINE
    LDA #${line_hi:02X}
    STA __RDN_LINE+1
__RDN_LOOP:
    LDY #$00
    LDA (${cursor:02X}),Y
    BNE __RDN_RECORD
__RDN_VALID:
    JMP __VAL_HELPER
__RDN_RECORD:
    STA __RDN_SPAN
    INY
    LDA (${cursor:02X}),Y
    CLC
    ADC __RDN_LINE
    STA __RDN_LINE
    BCC __RDN_LINE_OK
    INC __RDN_LINE+1
__RDN_LINE_OK:
    BIT __RDN_SPAN
    BPL __RDN_GAP_OK
    INY
    LDA (${cursor:02X}),Y
    CLC
    ADC __RDN_POS
    STA __RDN_POS
    BCC __RDN_GAP_OK
    INC __RDN_POS+1
__RDN_GAP_OK:
    TYA
    SEC
    ADC ${cursor:02X}
    STA ${cursor:02X}
    BCC __RDN_CURSOR_OK
    INC ${cursor_hi:02X}
__RDN_CURSOR_OK:
    LDA {ptr_lo}
    CMP __RDN_POS
    LDA {ptr_hi}
    SBC __RDN_POS+1
    BCC __RDN_VALID
    LDA __RDN_SPAN
    AND #$7F
    CLC
    ADC __RDN_POS
    STA __RDN_POS
    BCC __RDN_END_OK
    INC __RDN_POS+1
__RDN_END_OK:
    LDA {ptr_lo}
    CMP __RDN_POS
    LDA {ptr_hi}
    SBC __RDN_POS+1
    BCC __RDN_HIT
    JMP __RDN_LOOP
__RDN_HIT:
    LDA __RDN_LINE
    STA $39
    LDA __RDN_LINE+1
    STA $3A
    LDX #$0B
"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_unrepresentable_metadata_and_unpaid_decoder() {
        let lengths = vec![4; 80];
        let runs: Vec<_> = (0..80).map(|i| (i, i, 200 + i as u16 * 5)).collect();
        assert!(CompactDataBad::new(&lengths, &runs).is_some());
        assert!(CompactDataBad::new(&lengths, &runs[..2]).is_none());
        let mut wide = lengths.clone();
        wide[40] = 128;
        assert!(CompactDataBad::new(&wide, &runs).is_none());
        let mut gap = lengths.clone();
        gap[40] = 256;
        let skipped: Vec<_> = runs.iter().copied().filter(|r| r.0 != 40).collect();
        assert!(CompactDataBad::new(&gap, &skipped).is_none());
        let mut jump = runs.clone();
        jump[40].2 += 256;
        assert!(CompactDataBad::new(&lengths, &jump).is_none());
    }

    #[test]
    fn actual_6502_decoder_matches_every_interval_boundary_and_preserves_caller() {
        let lengths: Vec<_> = (0..240).map(|i| 2 + i % 13).collect();
        let runs: Vec<_> = (1..239)
            .step_by(3)
            .map(|i| (i, i + 1, 250 + i as u16 * 3))
            .collect();
        let plan = CompactDataBad::new(&lengths, &runs).unwrap();
        let helper = plan.helper(5, "$03", "$04");
        let asm = format!(
            "*=$080D\n{helper}    JMP __ERROR\n__VAL_HELPER:\n    RTS\n__ERROR:\n    RTS\n__RDN_POS:\n    .byte 0,0\n__RDN_LINE:\n    .byte 0,0\n__RDN_SPAN:\n    .byte 0\n__DATA_BAD:\n    .byte {}\n__DATA:\n    .byte 0\n",
            plan.bytes
                .iter()
                .map(u8::to_string)
                .collect::<Vec<_>>()
                .join(",")
        );
        let mut a = asm6502::Assembler6502::new();
        a.set_origin(0x080d);
        let bytes = a.assemble_bytes(&asm).unwrap();
        let mut offsets = vec![0usize];
        for len in &lengths {
            offsets.push(offsets.last().unwrap() + len);
        }
        for offset in 0..=*offsets.last().unwrap() {
            let expected = runs
                .iter()
                .find(|&&(first, last, _)| offsets[first] <= offset && offset < offsets[last + 1])
                .map(|r| r.2);
            let mut sys = emu64::System::new();
            for (i, b) in bytes.iter().enumerate() {
                sys.mem.ram[0x080d + i] = *b;
            }
            assert_eq!(
                sys.mem.ram[a.lookup("__DATA_BAD").unwrap() as usize],
                plan.bytes[0],
                "assembled table load"
            );
            let ptr = a.lookup("__DATA").unwrap() + offset as u16;
            sys.mem.ram[3..5].copy_from_slice(&ptr.to_le_bytes());
            sys.mem.ram[0x39..0x3b].copy_from_slice(&777u16.to_le_bytes());
            sys.mem.ram[0xfd..0xff].copy_from_slice(&[0xaa, 0xbb]);
            sys.cpu.regs.pc = a.lookup("__READ_NUM").unwrap();
            let mut finished = false;
            for _ in 0..20000 {
                if sys.cpu.regs.pc == a.lookup("__VAL_HELPER").unwrap()
                    || sys.cpu.regs.pc == a.lookup("__ERROR").unwrap()
                {
                    finished = true;
                    break;
                }
                sys.cpu.step(&mut sys.mem);
            }
            assert!(finished, "decoder did not finish offset {offset}");
            assert_eq!(
                sys.cpu.regs.pc == a.lookup("__ERROR").unwrap(),
                expected.is_some(),
                "offset {offset}"
            );
            let line = u16::from_le_bytes([sys.mem.ram[0x39], sys.mem.ram[0x3a]]);
            assert_eq!(line, expected.unwrap_or(777), "offset {offset}");
            assert_eq!(&sys.mem.ram[3..5], &ptr.to_le_bytes());
            assert_eq!(&sys.mem.ram[0xfd..0xff], &[0xaa, 0xbb]);
        }
    }

    #[test]
    fn generated_read_programs_use_the_compact_decoder_on_all_profiles() {
        use crate::{Profile, codegen, extraram, peephole};
        let data = (0..80)
            .map(|i| format!("{} DATA \"S{i}\",{}\n", 250 + i * 5, i + 100))
            .collect::<String>();
        let cases = [
            ("first", "10 READ A\n20 END\n".to_string(), Some(250)),
            ("second", "10 READ A$,B:READ C\n20 END\n".to_string(), Some(255)),
            ("last", "10 FOR I=1 TO 79:READ A$,B:NEXT:READ C\n20 END\n".to_string(), Some(645)),
            ("valid", "10 READ F$,B:FOR I=2 TO 80:READ A$,B:NEXT\n20 PRINT F$;A$;B:END\n".to_string(), None),
            ("array", "10 DIM A$(79),B(79):FOR I=0 TO 79:READ A$(I),B(I):NEXT\n20 PRINT A$(0);A$(79);B(79):END\n".to_string(), None),
            ("integer_array", "10 DIM B%(79):FOR I=0 TO 79:READ A$,B%(I):NEXT\n20 PRINT A$;B%(79):END\n".to_string(), None),
            ("restore", "10 READ A$,B:RESTORE:READ C$,D\n20 PRINT A$;B;C$;D:END\n".to_string(), None),
            ("exhausted", "10 FOR I=1 TO 80:READ A$,B:NEXT\n20 READ C\n".to_string(), None),
        ];
        for (name, program, error_line) in cases {
            let src = program + &data;
            let native = crate::tokenize_program(&src).unwrap();
            let prg = crate::Program::parse(&native).unwrap();
            let ast = crate::parse::program(&prg).unwrap();
            for profile in [Profile::Default, Profile::Speed, Profile::Size] {
                let module = crate::lower_and_optimize(&ast, profile).unwrap();
                for banked in [false, true] {
                    let (asm, _) = codegen::emit_with_profile_at_opts_diag_and_data_rescue(
                        &module,
                        profile,
                        0x080d,
                        false,
                        &Default::default(),
                        &Default::default(),
                        codegen::SourceIntegerVars::from_module(&module),
                        true,
                        false,
                        None,
                    )
                    .unwrap();
                    assert!(
                        asm.contains("__RDN_SPAN:"),
                        "{name}: codec must actually activate"
                    );
                    assert!(!asm.contains("__DBAD_S"));
                    let asm = peephole::run_with_options(
                        &asm,
                        profile,
                        peephole::Options {
                            aggressive_size_dedup: true,
                            ..Default::default()
                        },
                    );
                    let asm = if banked { extraram::inject(&asm) } else { asm };
                    let mut a = asm6502::Assembler6502::new();
                    a.set_origin(0x080d);
                    let bytes = a.assemble_bytes(&asm).unwrap();
                    let mut image = vec![1, 8, 11, 8, 10, 0, 0x9e, b'2', b'0', b'6', b'1', 0, 0, 0];
                    image.extend(bytes);

                    let result = emu64::run_prg_to_end(&image, 5_000_000).unwrap();
                    if let Some(line) = error_line {
                        let output = [result.output.clone(), result.error_output.clone()].concat();
                        let text = String::from_utf8_lossy(&output);
                        assert!(
                            text.contains(&format!("ERROR IN {line}")),
                            "{name}/{profile:?}/{banked}: {text}"
                        );
                    } else if name != "exhausted" {
                        assert!(
                            result.clean_exit,
                            "{name}/{profile:?}/{banked}: error {:?} line {:?}, steps {} output {:?}",
                            result.error_code,
                            result.error_line,
                            result.instructions,
                            result.output
                        );
                    }

                }
            }
        }
    }
}
