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
