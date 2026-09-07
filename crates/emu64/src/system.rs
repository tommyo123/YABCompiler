//! Wires CPU + memory + ROMs together.
//!
//! KERNAL boot is skipped. Banking is set so BASIC/KERNAL/I/O are all
//! visible, conio zeropage state is pre-initialized, and a handful of
//! KERNAL entry points are trapped so callers see plausible behaviour
//! without a real boot.

use crate::cpu::Cpu;
use crate::memory::C64Mem;

const ROM_BASIC: &[u8] = include_bytes!("../roms/basic.bin");
const ROM_KERNAL: &[u8] = include_bytes!("../roms/kernal.bin");
const ROM_CHARGEN: &[u8] = include_bytes!("../roms/chargen.bin");

/// BSOUT / CHROUT — write A to output, RTS.
pub const FFD2: u16 = 0xFFD2;
/// STOP — return A=0 (no stop key), Z=1.
pub const FFE1: u16 = 0xFFE1;
/// GETIN — return A=0 (no key).
pub const FFE4: u16 = 0xFFE4;
/// CHRIN — return A=0.
pub const FFCF: u16 = 0xFFCF;
/// PLOT — get or set cursor position and recompute screen / colour
/// pointers from the row.
pub const FFF0: u16 = 0xFFF0;

/// BASIC's ERROR entry.
pub const BASIC_ERROR: u16 = 0xA437;
/// The KERNAL-side default IERROR handler a cold start installs.
pub const IERROR_ROM_HANDLER: u16 = 0xE38B;
/// BASIC's READY prompt.
pub const BASIC_READY: u16 = 0xA474;

// Conio zeropage variables.
const ZP_RVS: u16 = 0xC7;
const ZP_SCREEN_PTR: u16 = 0xD1;
/// Cursor column within the current logical line.
const ZP_CURS_X: u16 = 0xD3;
/// Last logical-line column: 39 for one screen row, 79 for two.
const ZP_LNMX: u16 = 0xD5;
const ZP_CURS_Y: u16 = 0xD6;
const ZP_CRAM_PTR: u16 = 0xF3;
const ZP_CHARCOLOR: u16 = 0x0286;

/// Bottom text row.
const LAST_ROW: u8 = 24;
/// Last column of an unwrapped logical line.
const LINE_END: u8 = 39;
/// Last column of a logical line that has wrapped onto a second row.
const WRAPPED_LINE_END: u8 = 79;

pub struct System {
    pub cpu: Cpu,
    pub mem: C64Mem,
    /// Bytes the *program* wrote through CHROUT, in PETSCII.
    pub output: Vec<u8>,
    /// Bytes the ROM's error reporter wrote once the program raised an error.
    pub error_output: Vec<u8>,
    pub error_code: Option<u8>,
    pub error_line: Option<u16>,
    /// Whether execution has reached BASIC's READY prompt.
    pub ready_reached: bool,
}

impl System {
    pub fn new() -> Self {
        let mut mem = C64Mem::new();
        mem.basic_rom.copy_from_slice(ROM_BASIC);
        mem.kernal_rom.copy_from_slice(ROM_KERNAL);
        mem.chargen_rom.copy_from_slice(ROM_CHARGEN);

        // Banking: BASIC + KERNAL + I/O all visible.
        mem.ram[0x0000] = 0x2F;
        mem.ram[0x0001] = 0x37;

        // BASIC zero-page pointers a cold start would set up. Compiled
        // programs that print numbers go through STROUT ($AB1E), which
        // allocates string space via GETSPA ($B4F4); without these the
        // allocator walks off into garbage and jams. TXTTAB=$0801,
        // VARTAB/ARYTAB/STREND just past it, FRETOP/MEMSIZ at the top of
        // BASIC RAM ($A000) so there is room to allocate downward.
        let set16 = |mem: &mut C64Mem, zp: usize, v: u16| {
            mem.ram[zp] = v as u8;
            mem.ram[zp + 1] = (v >> 8) as u8;
        };
        set16(&mut mem, 0x2B, 0x0801); // TXTTAB - start of BASIC text
        set16(&mut mem, 0x2D, 0x0803); // VARTAB - start of variables
        set16(&mut mem, 0x2F, 0x0803); // ARYTAB - start of arrays
        set16(&mut mem, 0x31, 0x0803); // STREND - end of arrays
        set16(&mut mem, 0x33, 0xA000); // FRETOP - bottom of string space
        set16(&mut mem, 0x37, 0xA000); // MEMSIZ - top of BASIC RAM

        // TEMPPT ($16): pointer into the temporary string-descriptor
        // stack ($0019..$0021). A cold start sets it to $19; if left 0,
        // STROUT's descriptor write (STA $00,X) lands on the 6510 port
        // at $00/$01 and kills the ROM banking.
        mem.ram[0x16] = 0x19;

        // CHRGET: the cold start copies this 24-byte routine into
        // $0073..$008A. The ROM number parser FIN ($BCF3) used by VAL
        // and numeric READ/DATA fetches characters through it; without
        // it those calls JSR into uninitialized zero page and hit BRK.
        // The LDA operand at $0079/$007A/$007B is TXTPTR and is
        // self-modified at runtime, so its initial value is irrelevant.
        const CHRGET: [u8; 24] = [
            0xE6, 0x7A, // INC $7A
            0xD0, 0x02, // BNE $0079
            0xE6, 0x7B, // INC $7B
            0xAD, 0x00, 0x08, // LDA $0800 (TXTPTR, self-modified)
            0xC9, 0x3A, // CMP #$3A   (":")
            0xB0, 0x0A, // BCS $008A
            0xC9, 0x20, // CMP #$20   (" ", skip spaces)
            0xF0, 0xEF, // BEQ $0073
            0x38, // SEC
            0xE9, 0x30, // SBC #$30
            0x38, // SEC
            0xE9, 0xD0, // SBC #$D0
            0x60, // RTS
        ];
        mem.ram[0x0073..0x0073 + CHRGET.len()].copy_from_slice(&CHRGET);

        // Conio zeropage defaults that a real KERNAL boot would set.
        mem.ram[ZP_SCREEN_PTR as usize] = 0x00;
        mem.ram[(ZP_SCREEN_PTR + 1) as usize] = 0x04;
        mem.ram[ZP_CRAM_PTR as usize] = 0x00;
        mem.ram[(ZP_CRAM_PTR + 1) as usize] = 0xD8;
        mem.ram[ZP_CURS_X as usize] = 0;
        mem.ram[ZP_CURS_Y as usize] = 0;
        mem.ram[ZP_LNMX as usize] = LINE_END;
        mem.ram[ZP_RVS as usize] = 0;
        mem.ram[ZP_CHARCOLOR as usize] = 0x0E;

        // Indirect vectors.
        let rom_copy = |mem: &mut C64Mem, src: u16, dst: u16, len: usize| {
            for i in 0..len {
                let b = mem.kernal_rom[(src as usize - 0xE000) + i];
                mem.ram[dst as usize + i] = b;
            }
        };
        rom_copy(&mut mem, 0xE447, 0x0300, 12);
        rom_copy(&mut mem, 0xFD30, 0x0314, 32);
        debug_assert_eq!(
            (mem.ram[0x0300] as u16) | ((mem.ram[0x0301] as u16) << 8),
            IERROR_ROM_HANDLER,
            "unexpected BASIC vector table in basic.bin"
        );

        // Default I/O channels: keyboard in, screen out.
        mem.ram[0x99] = 0; // DFLTN - keyboard
        mem.ram[0x9A] = 3; // DFLTO - screen

        Self {
            cpu: Cpu::new(),
            mem,
            output: Vec::new(),
            error_output: Vec::new(),
            error_code: None,
            error_line: None,
            ready_reached: false,
        }
    }

    /// Load a PRG (first two bytes are the little-endian load address).
    /// Returns the BASIC `SYS` target if one is found in a stub at
    /// $0801, otherwise the load address itself.
    pub fn load_prg(&mut self, bytes: &[u8]) -> Result<u16, String> {
        let (start, _) = self.mem.load_prg(bytes)?;
        let sys_target = if start == 0x0801 {
            self.parse_sys_target(start)
        } else {
            None
        };
        Ok(sys_target.unwrap_or(start))
    }

    fn parse_sys_target(&self, start: u16) -> Option<u16> {
        for i in 0..32u16 {
            let addr = start.wrapping_add(i);
            if self.mem.ram[addr as usize] == 0x9E {
                let mut p = addr.wrapping_add(1);
                while self.mem.ram[p as usize] == b' ' {
                    p = p.wrapping_add(1);
                }
                let mut num: u32 = 0;
                let mut any = false;
                while self.mem.ram[p as usize].is_ascii_digit() {
                    num = num * 10 + (self.mem.ram[p as usize] - b'0') as u32;
                    p = p.wrapping_add(1);
                    any = true;
                }
                if any && num <= 0xFFFF {
                    return Some(num as u16);
                }
            }
        }
        None
    }

    /// Reset the CPU and set PC to `entry`.
    pub fn start_at(&mut self, entry: u16) {
        self.cpu.run_setup(entry);
        self.cpu.regs.sp = 0xFF;
    }

    /// Execute one instruction, dispatching to a trap handler if PC sits
    /// on a trapped KERNAL entry. Returns false on BRK / JAM.
    pub fn step(&mut self) -> bool {
        let pc = self.cpu.regs.pc;
        if pc == BASIC_ERROR && self.error_code.is_none() {
            self.error_code = Some(self.cpu.regs.x);
            let curlin = (self.mem.ram[0x39] as u16) | ((self.mem.ram[0x3A] as u16) << 8);
            self.error_line = (curlin != 0xFFFF).then_some(curlin);
        }
        if pc == BASIC_READY {
            self.ready_reached = true;
            return false;
        }
        match pc {
            FFD2 => {
                let ch = self.cpu.regs.a;
                if self.error_code.is_some() {
                    self.error_output.push(ch);
                } else {
                    self.output.push(ch);
                }
                self.track_cursor(ch);
                // Real CHROUT returns carry clear on success; KERNAL
                // callers (e.g. $E10C) do `BCS error` afterwards, so a
                // stale carry would send them into the error handler.
                self.cpu.set_flag(crate::cpu::FLAG_C, false);
                self.rts();
                return true;
            }
            FFE4 | FFCF | FFE1 => {
                self.cpu.regs.a = 0;
                self.cpu.set_flag(crate::cpu::FLAG_Z, true);
                // Carry clear == no error, matching the KERNAL contract.
                self.cpu.set_flag(crate::cpu::FLAG_C, false);
                self.rts();
                return true;
            }
            FFF0 => {
                self.handle_plot();
                self.rts();
                return true;
            }
            _ => {}
        }
        self.cpu.step(&mut self.mem)
    }

    // Cursor bookkeeping for the trapped CHROUT.

    /// Apply the cursor movement a byte sent to CHROUT would cause.
    fn track_cursor(&mut self, ch: u8) {
        match ch {
            // CR and shifted CR both end the logical line.
            0x0D | 0x8D => self.cursor_crlf(),
            // CLR and HOME both park the cursor at the top left; CLR additionally wipes the
            // screen, which nothing here reads.
            0x93 | 0x13 => {
                self.mem.ram[ZP_CURS_X as usize] = 0;
                self.mem.ram[ZP_CURS_Y as usize] = 0;
                self.mem.ram[ZP_LNMX as usize] = LINE_END;
                self.refresh_line_ptr();
            }
            0x1D => self.cursor_advance(),
            // Cursor-left and DELETE both step back a column.
            0x9D | 0x14 => self.cursor_left(),
            0x11 => self.cursor_row(1),
            0x91 => self.cursor_row(-1),
            // Printable PETSCII.
            0x20..=0x7F | 0xA0..=0xFF => self.cursor_advance(),
            _ => {}
        }
    }

    /// Move one column right, wrapping the way the screen editor does.
    fn cursor_advance(&mut self) {
        let col = self.mem.ram[ZP_CURS_X as usize].wrapping_add(1);
        let lnmx = self.mem.ram[ZP_LNMX as usize];
        if col <= lnmx {
            self.mem.ram[ZP_CURS_X as usize] = col;
            return;
        }
        if lnmx < WRAPPED_LINE_END {
            // Ran off the first row: link the next one into the same logical line.
            self.mem.ram[ZP_LNMX as usize] = WRAPPED_LINE_END;
            self.mem.ram[ZP_CURS_X as usize] = col;
            self.bump_row(1);
        } else {
            self.mem.ram[ZP_LNMX as usize] = LINE_END;
            self.mem.ram[ZP_CURS_X as usize] = 0;
            self.bump_row(1);
            self.refresh_line_ptr();
        }
    }

    fn cursor_left(&mut self) {
        let col = self.mem.ram[ZP_CURS_X as usize];
        if col > 0 {
            self.mem.ram[ZP_CURS_X as usize] = col - 1;
        } else if self.mem.ram[ZP_CURS_Y as usize] > 0 {
            self.bump_row(-1);
            self.mem.ram[ZP_CURS_X as usize] = LINE_END;
            self.mem.ram[ZP_LNMX as usize] = LINE_END;
            self.refresh_line_ptr();
        }
    }

    fn cursor_crlf(&mut self) {
        self.mem.ram[ZP_CURS_X as usize] = 0;
        self.mem.ram[ZP_LNMX as usize] = LINE_END;
        self.bump_row(1);
        self.refresh_line_ptr();
    }

    fn cursor_row(&mut self, delta: i8) {
        self.bump_row(delta);
        self.refresh_line_ptr();
    }

    /// Move the cursor row, clamped to the screen.
    fn bump_row(&mut self, delta: i8) {
        let row = self.mem.ram[ZP_CURS_Y as usize] as i16 + delta as i16;
        self.mem.ram[ZP_CURS_Y as usize] = row.clamp(0, LAST_ROW as i16) as u8;
    }

    /// Point PNT ($D1) / colour ($F3) at the start of the current logical line, which is one
    /// row up while the line is wrapped.
    fn refresh_line_ptr(&mut self) {
        let wrapped = self.mem.ram[ZP_LNMX as usize] > LINE_END;
        let row = self.mem.ram[ZP_CURS_Y as usize].saturating_sub(wrapped as u8);
        let screen = 0x0400u16 + (row as u16) * 40;
        let cram = 0xD800u16 + (row as u16) * 40;
        self.mem.ram[ZP_SCREEN_PTR as usize] = screen as u8;
        self.mem.ram[(ZP_SCREEN_PTR + 1) as usize] = (screen >> 8) as u8;
        self.mem.ram[ZP_CRAM_PTR as usize] = cram as u8;
        self.mem.ram[(ZP_CRAM_PTR + 1) as usize] = (cram >> 8) as u8;
    }

    /// Carry clear: set cursor from X (row) and Y (col), update screen
    /// and colour pointers. Carry set: return current position in X and
    /// Y.
    fn handle_plot(&mut self) {
        if self.cpu.flag(crate::cpu::FLAG_C) {
            self.cpu.regs.x = self.mem.ram[ZP_CURS_Y as usize];
            self.cpu.regs.y = self.mem.ram[ZP_CURS_X as usize];
        } else {
            let row = self.cpu.regs.x;
            let col = self.cpu.regs.y;
            self.mem.ram[ZP_CURS_Y as usize] = row;
            self.mem.ram[ZP_CURS_X as usize] = col;
            // PLOT lands on a physical row, so the logical line it puts the cursor on is a
            // single row again.
            self.mem.ram[ZP_LNMX as usize] = LINE_END;
            let screen_base = 0x0400u16 + (row as u16) * 40;
            let cram_base = 0xD800u16 + (row as u16) * 40;
            self.mem.ram[ZP_SCREEN_PTR as usize] = screen_base as u8;
            self.mem.ram[(ZP_SCREEN_PTR + 1) as usize] = (screen_base >> 8) as u8;
            self.mem.ram[ZP_CRAM_PTR as usize] = cram_base as u8;
            self.mem.ram[(ZP_CRAM_PTR + 1) as usize] = (cram_base >> 8) as u8;
        }
    }

    /// Pop the JSR return address from the hardware stack and resume at
    /// `addr + 1`, matching the 6510 RTS semantics.
    fn rts(&mut self) {
        let lo = self.pop_stack() as u16;
        let hi = self.pop_stack() as u16;
        let ret = ((hi << 8) | lo).wrapping_add(1);
        self.cpu.regs.pc = ret;
    }
    fn pop_stack(&mut self) -> u8 {
        self.cpu.regs.sp = self.cpu.regs.sp.wrapping_add(1);
        self.mem.read_byte(0x0100u16 | (self.cpu.regs.sp as u16))
    }

    /// Copy of the 40×25 byte text-screen region at $0400.
    pub fn screen_bytes(&self) -> [u8; 1000] {
        let mut out = [0u8; 1000];
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = self.mem.ram[0x0400 + i];
        }
        out
    }
}

impl Default for System {
    fn default() -> Self {
        Self::new()
    }
}
