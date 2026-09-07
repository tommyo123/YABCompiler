//! Supply compiled string descriptors during ROM evaluation of SYS text.

use super::*;

pub(super) fn string_value(value: &Expr) -> Option<StrExpr> {
    match value {
        Expr::String(bytes) => Some(StrExpr::Literal(bytes.clone())),
        Expr::Var(v) if v.kind == VarKind::String => Some(StrExpr::Var(v.clone())),
        Expr::ArrayRef(v, indices) if v.kind == VarKind::String => {
            Some(StrExpr::ArrayRef(v.clone(), indices.clone()))
        }
        _ => None,
    }
}

impl Codegen {
    pub(super) fn emit_sys_string_callbacks(
        &mut self,
        id: usize,
        vars: &[ir::SysParamVar],
    ) -> Result<(), CodegenError> {
        let mut slot = 0;
        for var in vars {
            let Some(value) = string_value(&var.value) else {
                continue;
            };
            writeln!(self.code, "__SYS_STRING_CALLBACK_{id}_{slot}:").unwrap();
            self.invalidate_fac_cache();
            self.invalidate_array_addr_cache();
            self.emit_strexpr_pointer_to_ay(&value)?;
            writeln!(self.code, "    STA $FD\n    STY $FE\n    ORA $FE\n    BEQ __SYS_STRING_LENGTH_{id}_{slot}\n    LDY #$00\n    LDA ($FD),Y\n__SYS_STRING_LENGTH_{id}_{slot}:\n    STA __SYS_STR_VALUE_{slot}\n    TAY\n    BEQ __SYS_STRING_COPIED_{id}_{slot}").unwrap();
            writeln!(self.code, "__SYS_STRING_COPY_{id}_{slot}:\n    LDA ($FD),Y\n    STA __SYS_STR_VALUE_{slot}+2,Y\n    DEY\n    BNE __SYS_STRING_COPY_{id}_{slot}").unwrap();
            writeln!(self.code, "__SYS_STRING_COPIED_{id}_{slot}:\n    LDA #<(__SYS_STR_VALUE_{slot}+3)\n    STA __SYS_STR_VALUE_{slot}+1\n    LDA #>(__SYS_STR_VALUE_{slot}+3)\n    STA __SYS_STR_VALUE_{slot}+2").unwrap();
            writeln!(self.code, "    LDA #<__SYS_STR_VALUE_{slot}\n    STA __SYS_EVAL_RESULT\n    LDA #>__SYS_STR_VALUE_{slot}\n    STA __SYS_EVAL_RESULT+1\n    RTS").unwrap();
            slot += 1;
        }
        self.sys_string_slots = self.sys_string_slots.max(slot);
        self.invalidate_fac_cache();
        self.invalidate_array_addr_cache();
        Ok(())
    }

    pub(super) fn emit_sys_string_position(&mut self, id: usize, slot: usize, len: usize) {
        writeln!(self.code, "    TXA\n    CLC\n    ADC #<__SYS_TXTBUF\n    STA __SYS_EVAL_START_LO+{slot}\n    LDA #>__SYS_TXTBUF\n    ADC #$00\n    STA __SYS_EVAL_START_HI+{slot}").unwrap();
        writeln!(self.code, "    LDA #${len:02X}\n    STA __SYS_EVAL_LENGTH+{slot}\n    LDA #<__SYS_STRING_CALLBACK_{id}_{slot}\n    STA __SYS_EVAL_CALLBACK_LO+{slot}\n    LDA #>__SYS_STRING_CALLBACK_{id}_{slot}\n    STA __SYS_EVAL_CALLBACK_HI+{slot}").unwrap();
    }

    pub(super) fn emit_sys_string_runtime(&mut self) {

        let homes = self.collect_safe_sys_cells();
        let outside: Vec<_> = homes
            .iter()
            .copied()
            .filter(|b| !(2..0x90).contains(b))
            .collect();
        self.code.push_str(
            r#"
__SYS_EVAL_INSTALL:
    LDA $7A
    STA __SYS_EVAL_OUTER_TEXT
    LDA $7B
    STA __SYS_EVAL_OUTER_TEXT+1
    LDA $030A
    STA __SYS_EVAL_PREVIOUS
    LDA $030B
    STA __SYS_EVAL_PREVIOUS+1
    LDA $0300
    STA __SYS_EVAL_OLD_ERROR
    LDA $0301
    STA __SYS_EVAL_OLD_ERROR+1
    LDA #<__SYS_EVAL_ENTER
    STA $030A
    LDA #>__SYS_EVAL_ENTER
    STA $030B
    LDA #<__SYS_EVAL_IERROR
    STA $0300
    LDA #>__SYS_EVAL_IERROR
    STA $0301
    LDA #$01
    STA __SYS_EVAL_ACTIVE
"#,
        );
        for cell in &homes {
            writeln!(
                self.code,
                "    LDA ${cell:02X}\n    STA __SYS_EVAL_HOMES+{cell}"
            )
            .unwrap();
        }
        self.code.push_str("    RTS\n__SYS_EVAL_LOAD_HOMES:\n");
        for cell in &homes {
            writeln!(
                self.code,
                "    LDA __SYS_EVAL_HOMES+{cell}\n    STA ${cell:02X}"
            )
            .unwrap();
        }
        self.code.push_str(
            r#"    RTS
__SYS_EVAL_ENTER:
    PHP
    PHA
    TXA
    PHA
    TYA
    PHA
    JMP __SYS_EVAL_DISPATCH
__SYS_EVAL_DISPATCH:
    CLD
    LDY #$01
__SYS_EVAL_SPACES:
    LDA ($7A),Y
    CMP #$20
    BNE __SYS_EVAL_SEEK
    INY
    BNE __SYS_EVAL_SPACES
    JMP __SYS_EVAL_CHAIN
__SYS_EVAL_SEEK:
    TYA
    CLC
    ADC $7A
    STA __SYS_EVAL_SEEK_PTR
    LDA $7B
    ADC #$00
    STA __SYS_EVAL_SEEK_PTR+1
    LDX #$00
__SYS_EVAL_SEARCH:
    LDA __SYS_EVAL_START_LO,X
    CMP __SYS_EVAL_SEEK_PTR
    BNE __SYS_EVAL_NEXT
    LDA __SYS_EVAL_START_HI,X
    CMP __SYS_EVAL_SEEK_PTR+1
    BEQ __SYS_EVAL_FOUND
__SYS_EVAL_NEXT:
    INX
    CPX __SYS_EVAL_COUNT
    BNE __SYS_EVAL_SEARCH
    JMP __SYS_EVAL_CHAIN
__SYS_EVAL_FOUND:
    LDA __SYS_EVAL_LENGTH,X
    CLC
    ADC __SYS_EVAL_SEEK_PTR
    STA __SYS_EVAL_END
    LDA __SYS_EVAL_SEEK_PTR+1
    ADC #$00
    STA __SYS_EVAL_END+1
    LDA __SYS_EVAL_CALLBACK_LO,X
    STA __SYS_EVAL_CALL+1
    LDA __SYS_EVAL_CALLBACK_HI,X
    STA __SYS_EVAL_CALL+2
    JSR __SYS_EVAL_SNAPSHOT
    LDA #$01
    STA __SYS_EVAL_BUSY
    JSR __SYS_EVAL_LOAD_HOMES
__SYS_EVAL_CALL:
    JSR $FFFF
    JSR __SYS_EVAL_RESTORE
    LDA #$00
    STA __SYS_EVAL_BUSY
    STA $70
    LDA #$FF
    STA $0D
    LDA __SYS_EVAL_RESULT
    STA $64
    LDA __SYS_EVAL_RESULT+1
    STA $65
    LDA __SYS_EVAL_END
    STA $7A
    LDA __SYS_EVAL_END+1
    STA $7B
    JMP __SYS_EVAL_RETURN
__SYS_EVAL_CHAIN:
    PLA
    TAY
    PLA
    TAX
    PLA
    PLP
    JMP (__SYS_EVAL_PREVIOUS)
__SYS_EVAL_RETURN:
    PLA
    TAY
    PLA
    TAX
    PLA
    PLP
    RTS
__SYS_EVAL_SNAPSHOT:
    LDX #$02
__SYS_EVAL_SAVE_ZP:
    LDA $0000,X
    STA __SYS_EVAL_ZP,X
    INX
    CPX #$90
    BNE __SYS_EVAL_SAVE_ZP
"#,
        );
        for cell in &outside {
            writeln!(
                self.code,
                "    LDA ${cell:02X}\n    STA __SYS_EVAL_ZP+{cell}"
            )
            .unwrap();
        }
        self.code.push_str(
            r#"    RTS
__SYS_EVAL_RESTORE:
    LDX #$02
__SYS_EVAL_RESTORE_ZP:
    LDA __SYS_EVAL_ZP,X
    STA $0000,X
    INX
    CPX #$90
    BNE __SYS_EVAL_RESTORE_ZP
"#,
        );
        for cell in &outside {
            writeln!(
                self.code,
                "    LDA __SYS_EVAL_ZP+{cell}\n    STA ${cell:02X}"
            )
            .unwrap();
        }
        self.code.push_str(
            r#"    RTS
__SYS_EVAL_IERROR:
    JSR __SYS_EVAL_ABORT
    JMP (__SYS_EVAL_OLD_ERROR)
__SYS_EVAL_ABORT:
    LDA __SYS_EVAL_ACTIVE
    BEQ __SYS_EVAL_ABORT_DONE
    TXA
    PHA
    LDA __SYS_EVAL_BUSY
    BEQ __SYS_EVAL_ABORT_TEXT
    JSR __SYS_EVAL_RESTORE
__SYS_EVAL_ABORT_TEXT:
    LDA __SYS_EVAL_OUTER_TEXT
    STA $7A
    LDA __SYS_EVAL_OUTER_TEXT+1
    STA $7B
    JSR __SYS_EVAL_FINISH
    PLA
    TAX
__SYS_EVAL_ABORT_DONE:
    RTS
__SYS_EVAL_FINISH:
    LDA __SYS_EVAL_PREVIOUS
    STA $030A
    LDA __SYS_EVAL_PREVIOUS+1
    STA $030B
    LDA __SYS_EVAL_OLD_ERROR
    STA $0300
    LDA __SYS_EVAL_OLD_ERROR+1
    STA $0301
    LDA #$00
    STA __SYS_EVAL_ACTIVE
    STA __SYS_EVAL_BUSY
    JMP __SYS_EVAL_LOAD_HOMES
"#,
        );
    }

    pub(super) fn emit_sys_string_storage(&mut self) {
        for (label, size) in [
            ("COUNT", 1),
            ("ACTIVE", 1),
            ("BUSY", 1),
            ("PORT", 1),
            ("PREVIOUS", 2),
            ("OLD_ERROR", 2),
            ("OUTER_TEXT", 2),
            ("SEEK_PTR", 2),
            ("END", 2),
            ("RESULT", 2),
            ("ZP", 256),
            ("HOMES", 256),
            ("START_LO", self.sys_string_slots),
            ("START_HI", self.sys_string_slots),
            ("LENGTH", self.sys_string_slots),
            ("CALLBACK_LO", self.sys_string_slots),
            ("CALLBACK_HI", self.sys_string_slots),
        ] {
            writeln!(self.code, "__SYS_EVAL_{label}:").unwrap();
            emit_byte_run(&mut self.code, &vec![0; size], false);
        }
        // Slots are shared across SYS statements, but distinct within one expression: native concatenation may hold both descriptors live.
        for slot in 0..self.sys_string_slots {
            writeln!(self.code, "__SYS_STR_VALUE_{slot}:").unwrap();
            emit_byte_run(&mut self.code, &vec![0; 259], false);
        }
    }
}
