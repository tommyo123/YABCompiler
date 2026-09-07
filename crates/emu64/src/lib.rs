//! Minimal C64 / 6510 emulator.
//!
//! [`System`] is the high-level entry point: load a PRG, jump to its
//! BASIC `SYS` target, and step until a screen-RAM pattern matches or
//! the instruction budget runs out.

pub mod cpu;
pub mod memory;
pub mod system;

pub use cpu::Cpu;
pub use memory::C64Mem;
pub use system::System;

/// Run a PRG until the expected byte pattern appears anywhere in
/// screen RAM ($0400..$07E8) or the instruction budget is exhausted.
pub fn run_until_screen_pattern(
    prg: &[u8],
    pattern: &[u8],
    max_instructions: u64,
) -> Result<RunResult, String> {
    let mut sys = System::new();
    let entry = sys.load_prg(prg)?;
    sys.start_at(entry);

    // Poll the screen periodically rather than every instruction.
    const POLL_EVERY: u64 = 4096;
    let mut next_poll = POLL_EVERY;

    for i in 0..max_instructions {
        if !sys.step() {
            return final_result(&sys, pattern, i, "cpu halted (BRK/JAM)");
        }
        if i + 1 == next_poll {
            next_poll += POLL_EVERY;
            if let Some(at) = find_pattern(&sys.screen_bytes(), pattern) {
                return Ok(RunResult {
                    instructions: i + 1,
                    matched_row: at / 40,
                    matched_col: at % 40,
                    output: sys.output,
                });
            }
        }
    }
    final_result(
        &sys,
        pattern,
        max_instructions,
        "instruction budget exhausted",
    )
}

fn final_result(
    sys: &System,
    pattern: &[u8],
    insns: u64,
    reason: &str,
) -> Result<RunResult, String> {
    if let Some(at) = find_pattern(&sys.screen_bytes(), pattern) {
        Ok(RunResult {
            instructions: insns,
            matched_row: at / 40,
            matched_col: at % 40,
            output: sys.output.clone(),
        })
    } else {
        Err(format!("{reason} (no pattern match after {insns} insns)"))
    }
}

fn find_pattern(screen: &[u8; 1000], pattern: &[u8]) -> Option<usize> {
    if pattern.is_empty() {
        return None;
    }
    for row in 0..25 {
        let row_start = row * 40;
        let row_end = row_start + 40;
        let row_bytes = &screen[row_start..row_end];
        if row_bytes.len() >= pattern.len() {
            for col in 0..=(row_bytes.len() - pattern.len()) {
                if row_bytes[col..col + pattern.len()] == *pattern {
                    return Some(row_start + col);
                }
            }
        }
    }
    None
}

#[derive(Debug)]
pub struct RunResult {
    pub instructions: u64,
    pub matched_row: usize,
    pub matched_col: usize,
    pub output: Vec<u8>,
}

/// Outcome of running a PRG to completion via [`run_prg_to_end`].
#[derive(Debug)]
pub struct RunToEnd {
    /// CHROUT bytes captured at `$FFD2`, in PETSCII order — what the
    /// program PRINTed.
    pub output: Vec<u8>,
    /// Final text-screen RAM ($0400) — for programs that POKE the
    /// screen directly rather than PRINT.
    pub screen: [u8; 1000],
    /// True if the program's outermost `RTS` returned to the caller
    /// (clean exit); false if it halted (BRK/JAM) or hit the budget.
    pub clean_exit: bool,
    pub instructions: u64,
    pub error_code: Option<u8>,
    /// BASIC error line, or None for a direct-mode error or no error.
    pub error_line: Option<u16>,
    /// Exactly the PETSCII the ROM's error reporter emitted, held apart from `output` so a
    /// transcript stays "what the program printed".
    pub error_output: Vec<u8>,
    /// Execution reached BASIC's READY prompt.
    pub ready_reached: bool,
}

impl RunToEnd {
    /// The `?<MSG> ERROR IN <line>` banner the ROM printed, normalised to a single line.
    pub fn error_banner(&self) -> Option<String> {
        self.error_code?;
        let mut line = String::new();
        for &b in &self.error_output {
            match b {
                0x0d if line.starts_with('?') => return Some(trim_end(&line)),
                0x0d => line.clear(),
                0x20..=0x5a => line.push(b as char),
                0xc1..=0xda => line.push((b - 0x80) as char),
                _ => {}
            }
        }
        if line.starts_with('?') {
            return Some(trim_end(&line));
        }
        Some(format!("?ERROR {}", self.error_code.unwrap()))
    }
}

fn trim_end(s: &str) -> String {
    s.trim_end().to_string()
}

/// Run a PRG from its BASIC `SYS` entry until it returns to the caller,
/// halts, or the instruction budget runs out — capturing everything it
/// PRINTs (CHROUT) plus the final screen.
///
/// A sentinel return address is pushed before the first instruction so
/// the program's outermost `RTS` lands on a known PC we detect as a
/// clean exit.
pub fn run_prg_to_end(prg: &[u8], max_instructions: u64) -> Result<RunToEnd, String> {
    /// An address compiled code never jumps to; the program's final
    /// `RTS` lands here and we stop before executing it.
    const SENTINEL: u16 = 0xFFFE;

    let mut sys = System::new();
    let entry = sys.load_prg(prg)?;
    sys.start_at(entry);

    // Mimic the JSR frame BASIC would have left: push (SENTINEL-1) as
    // the return address (RTS adds 1), high byte first then low.
    let ret = SENTINEL.wrapping_sub(1);
    let sp0 = sys.cpu.regs.sp;
    sys.mem.ram[0x0100 | sp0 as usize] = (ret >> 8) as u8;
    sys.mem.ram[0x0100 | sp0.wrapping_sub(1) as usize] = (ret & 0xFF) as u8;
    sys.cpu.regs.sp = sp0.wrapping_sub(2);

    let mut clean_exit = false;
    let mut instructions = 0u64;
    for i in 0..max_instructions {
        if sys.cpu.regs.pc == SENTINEL {
            clean_exit = true;
            instructions = i;
            break;
        }
        instructions = i + 1;
        if !sys.step() {
            break;
        }
    }

    Ok(RunToEnd {
        output: sys.output.clone(),
        screen: sys.screen_bytes(),
        clean_exit,
        instructions,
        error_code: sys.error_code,
        error_line: sys.error_line,
        error_output: sys.error_output.clone(),
        ready_reached: sys.ready_reached,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_program(bytes: &[u8], entry: u16, max_insns: u64) -> System {
        let mut sys = System::new();
        for (i, &b) in bytes.iter().enumerate() {
            sys.mem.ram[entry as usize + i] = b;
        }
        sys.start_at(entry);
        for _ in 0..max_insns {
            if !sys.step() {
                break;
            }
        }
        sys
    }

    #[test]
    fn lda_sta_brk() {
        // LDA #$42; STA $1000; BRK
        let sys = run_program(&[0xA9, 0x42, 0x8D, 0x00, 0x10, 0x00], 0xC000, 100);
        assert_eq!(sys.mem.ram[0x1000], 0x42);
        assert_eq!(sys.cpu.regs.a, 0x42);
        assert!(sys.cpu.brk_hit);
    }

    #[test]
    fn loop_with_inx_until_x_is_5() {
        // LDX #$00; loop: INX; CPX #$05; BNE loop; BRK
        let sys = run_program(
            &[0xA2, 0x00, 0xE8, 0xE0, 0x05, 0xD0, 0xFB, 0x00],
            0xC000,
            100,
        );
        assert_eq!(sys.cpu.regs.x, 5);
    }

    #[test]
    fn jsr_rts_round_trip() {
        // C000: JSR $C100; BRK
        // C100: LDA #$77; RTS
        let mut sys = System::new();
        sys.mem.ram[0xC000] = 0x20;
        sys.mem.ram[0xC001] = 0x00;
        sys.mem.ram[0xC002] = 0xC1;
        sys.mem.ram[0xC003] = 0x00;
        sys.mem.ram[0xC100] = 0xA9;
        sys.mem.ram[0xC101] = 0x77;
        sys.mem.ram[0xC102] = 0x60;
        sys.start_at(0xC000);
        for _ in 0..50 {
            if !sys.step() {
                break;
            }
        }
        assert_eq!(sys.cpu.regs.a, 0x77);
    }

    #[test]
    fn adc_with_carry_overflow_and_flags() {
        // LDA #$50; CLC; ADC #$50  -> A=$A0, V set (positive overflow)
        let sys = run_program(&[0xA9, 0x50, 0x18, 0x69, 0x50, 0x00], 0xC000, 20);
        assert_eq!(sys.cpu.regs.a, 0xA0);
        assert!(sys.cpu.flag(crate::cpu::FLAG_V));
        assert!(!sys.cpu.flag(crate::cpu::FLAG_C));
    }

    #[test]
    fn ffd2_chrout_trap_captures_byte() {
        // LDA #$41; JSR $FFD2; BRK
        let sys = run_program(&[0xA9, 0x41, 0x20, 0xD2, 0xFF, 0x00], 0xC000, 50);
        assert_eq!(sys.output, vec![0x41]);
    }

    #[test]
    fn fff0_plot_sets_screen_ptr_for_row() {
        // CLC; LDX #2 (row); LDY #0 (col); JSR $FFF0; BRK
        let sys = run_program(
            &[0x18, 0xA2, 0x02, 0xA0, 0x00, 0x20, 0xF0, 0xFF, 0x00],
            0xC000,
            50,
        );
        // SCREEN_PTR ($D1-$D2) = $0400 + 2 * 40 = $0450
        let p = (sys.mem.ram[0xD1] as u16) | ((sys.mem.ram[0xD2] as u16) << 8);
        assert_eq!(p, 0x0450);
    }

    // Cursor bookkeeping.

    /// Push `text` through the trapped CHROUT and hand back the machine.
    fn chrout_seq(text: &[u8]) -> System {
        let mut code = Vec::new();
        for &b in text {
            code.extend_from_slice(&[0xA9, b, 0x20, 0xD2, 0xFF]); // LDA #b; JSR $FFD2
        }
        code.push(0x00); // BRK
        run_program(&code, 0xC000, 20 * text.len() as u64 + 10)
    }

    fn cursor(sys: &System) -> (u8, u8, u8) {
        (sys.mem.ram[0xD3], sys.mem.ram[0xD5], sys.mem.ram[0xD6])
    }

    #[test]
    fn printable_characters_advance_the_column() {
        let sys = chrout_seq(b"AAAA");
        assert_eq!(cursor(&sys), (4, 39, 0), "PRINT \"AAAA\"; -> PNTR 4");
        assert_eq!(sys.output, b"AAAA".to_vec());
    }

    #[test]
    fn carriage_return_resets_the_column() {
        let sys = chrout_seq(b"AAAA\rBB");
        assert_eq!(cursor(&sys), (2, 39, 1));
    }

    #[test]
    fn forty_characters_link_a_second_row_into_one_logical_line() {
        let sys = chrout_seq(&[b'X'; 40]);
        assert_eq!(cursor(&sys), (40, 79, 1));
        let sys = chrout_seq(&[b'X'; 46]);
        assert_eq!(cursor(&sys), (46, 79, 1));
        let sys = chrout_seq(&[b'X'; 85]);
        assert_eq!(cursor(&sys), (5, 39, 2));
    }

    #[test]
    fn cursor_control_codes_move_the_column() {
        // CHR$(29) cursor-right counts as a column, CHR$(157) steps back.
        assert_eq!(chrout_seq(b"AB\x1D\x1DC").mem.ram[0xD3], 5);
        assert_eq!(chrout_seq(b"AB\x9D").mem.ram[0xD3], 1);
        // HOME parks at the top left.
        let sys = chrout_seq(b"AB\rCD\x13");
        assert_eq!(cursor(&sys), (0, 39, 0));
    }

    #[test]
    fn colour_and_reverse_codes_move_nothing() {
        // PRINT "AB";CHR$(18);CHR$(5);  -> PNTR 2 on real hardware.
        assert_eq!(chrout_seq(b"AB\x12\x05").mem.ram[0xD3], 2);
        assert_eq!(chrout_seq(b"AB\x92\x1c\x9e\x0e").mem.ram[0xD3], 2);
    }
    #[test]
    fn cursor_tracking_does_not_disturb_the_transcript() {
        let text = b"HI\r\x1D\x1DTHERE\x12!";
        let sys = chrout_seq(text);
        assert_eq!(sys.output, text.to_vec());
        assert!(sys.error_output.is_empty());
        assert!(sys.error_code.is_none());
    }

    // Runtime error reporting.

    /// Build a program that sets CURLIN and invokes the BASIC error handler.
    fn raise_error_prg(err: u8, line: u16) -> Vec<u8> {
        let mut prg = vec![0x00, 0xC0];
        prg.extend_from_slice(&[
            0xA9,
            line as u8,
            0x85,
            0x39,
            0xA9,
            (line >> 8) as u8,
            0x85,
            0x3A,
            0xA2,
            err,
            0x4C,
            0x37,
            0xA4,
        ]);
        prg
    }

    #[test]
    fn runtime_error_is_named() {
        let r = run_prg_to_end(&raise_error_prg(10, 40), 200_000).unwrap();
        assert_eq!(r.error_code, Some(10));
        assert_eq!(r.error_line, Some(40));
        assert!(!r.clean_exit);
        assert!(r.ready_reached, "the ROM should have got back to READY");
        assert_eq!(
            r.error_banner().as_deref(),
            Some("?NEXT WITHOUT FOR  ERROR IN 40")
        );
        let other = run_prg_to_end(&raise_error_prg(16, 40), 200_000).unwrap();
        assert_eq!(
            other.error_banner().as_deref(),
            Some("?OUT OF MEMORY  ERROR IN 40")
        );
    }

    /// The banner is kept out of `output`, so a transcript stays "what the program printed" and
    /// no golden picks up ROM prompt text.
    #[test]
    fn error_text_does_not_leak_into_the_program_transcript() {
        let mut prg = vec![0x00, 0xC0];
        prg.extend_from_slice(&[
            0xA9, 0x41, 0x20, 0xD2, 0xFF, // LDA #'A'; JSR $FFD2
            0xA9, 0x0D, 0x20, 0xD2, 0xFF, // LDA #CR;  JSR $FFD2
            0xA9, 0x14, 0x85, 0x39, 0xA9, 0x00, 0x85, 0x3A, // CURLIN = 20
            0xA2, 0x0C, 0x4C, 0x37, 0xA4, // LDX #12; JMP $A437
        ]);
        let r = run_prg_to_end(&prg, 200_000).unwrap();
        assert_eq!(r.output, vec![0x41, 0x0D]);
        assert_eq!(
            r.error_banner().as_deref(),
            Some("?RETURN WITHOUT GOSUB  ERROR IN 20")
        );
        assert!(!r.error_output.is_empty());
    }
    #[test]
    fn clean_program_reports_no_error() {
        let mut prg = vec![0x00, 0xC0];
        prg.extend_from_slice(&[0xA9, 0x41, 0x20, 0xD2, 0xFF, 0x60]);
        let r = run_prg_to_end(&prg, 10_000).unwrap();
        assert!(r.clean_exit);
        assert!(!r.ready_reached);
        assert_eq!(r.error_code, None);
        assert_eq!(r.error_banner(), None);
        assert_eq!(r.output, vec![0x41]);
    }

    /// The indirect vectors a cold start installs.
    #[test]
    fn cold_start_installs_the_rom_vectors() {
        let sys = System::new();
        let v = |a: usize| (sys.mem.ram[a] as u16) | ((sys.mem.ram[a + 1] as u16) << 8);
        assert_eq!(v(0x0300), 0xE38B, "IERROR");
        assert_eq!(v(0x0302), 0xA483, "IMAIN");
        assert_eq!(v(0x0308), 0xA7E4, "IGONE");
        assert_eq!(v(0x0318), 0xFE47, "NMI");
        assert_eq!(v(0x031A), 0xF34A, "OPEN");
        assert_eq!(v(0x0322), 0xF333, "CLRCHN");
        assert_eq!(v(0x0326), 0xF1CA, "CHROUT");
        assert_eq!(sys.mem.ram[0x9A], 3, "DFLTO = screen");
        assert_eq!(sys.mem.ram[0xD5], 39, "LNMX");
    }

    #[test]
    fn prg_load_parses_sys_target() {
        // Minimal cc65-style stub at $0801: "10 SYS 2061"
        // Bytes from a real PRG: 0B 08 0A 00 9E 32 30 36 31 00 00 00 NOP at 2061
        let mut prg = vec![0x01, 0x08]; // load addr $0801
        prg.extend_from_slice(&[
            0x0B, 0x08, // next-line ptr
            0x0A, 0x00, // line number 10
            0x9E, // SYS token
            b'2', b'0', b'6', b'1', 0x00, // end-of-line
            0x00, 0x00, // end-of-program
        ]);
        // Pad until $080D, then put LDA #$33; STA $0400; BRK.
        while prg.len() < 2 + (0x080D - 0x0801) {
            prg.push(0x00);
        }
        prg.extend_from_slice(&[0xA9, 0x33, 0x8D, 0x00, 0x04, 0x00]);

        let r = run_until_screen_pattern(&prg, &[0x33], 1000).unwrap();
        assert_eq!(r.matched_row, 0);
        assert_eq!(r.matched_col, 0);
    }
}
