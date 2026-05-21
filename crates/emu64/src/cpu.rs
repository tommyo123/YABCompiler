//! MOS 6510 / 6502 CPU emulator. All addressing modes and documented
//! opcodes plus the common illegal opcodes (LAX, SAX, DCP, ISC, SLO,
//! RLA, SRE, RRA). Single big match in `execute`.
//!
//! Cycle counting is nominal — one tick per instruction. Adequate for
//! instruction budgets and timeout fencing, not for cycle-accurate
//! hardware emulation.

use crate::memory::C64Mem;

// Status flag bits
pub const FLAG_C: u8 = 0x01;
pub const FLAG_Z: u8 = 0x02;
pub const FLAG_I: u8 = 0x04;
pub const FLAG_D: u8 = 0x08;
pub const FLAG_B: u8 = 0x10;
pub const FLAG_U: u8 = 0x20;
pub const FLAG_V: u8 = 0x40;
pub const FLAG_N: u8 = 0x80;

#[derive(Debug, Clone, Copy)]
pub struct Regs {
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub sp: u8,
    pub pc: u16,
    pub p: u8,
}

pub struct Cpu {
    pub regs: Regs,
    pub cycles: u64,
    pub jammed: bool,
    pub brk_hit: bool,
}

impl Cpu {
    pub fn new() -> Self {
        Self {
            regs: Regs {
                a: 0,
                x: 0,
                y: 0,
                sp: 0xFF,
                pc: 0,
                p: FLAG_U,
            },
            cycles: 0,
            jammed: false,
            brk_hit: false,
        }
    }

    pub fn run_setup(&mut self, pc: u16) {
        self.regs = Regs {
            a: 0,
            x: 0,
            y: 0,
            sp: 0xFF,
            pc,
            p: FLAG_U,
        };
        self.cycles = 0;
        self.jammed = false;
        self.brk_hit = false;
    }

    #[inline]
    pub fn flag(&self, m: u8) -> bool {
        (self.regs.p & m) != 0
    }
    #[inline]
    pub fn set_flag(&mut self, m: u8, v: bool) {
        if v {
            self.regs.p |= m;
        } else {
            self.regs.p &= !m;
        }
    }
    #[inline]
    fn set_nz(&mut self, v: u8) {
        self.set_flag(FLAG_Z, v == 0);
        self.set_flag(FLAG_N, (v & 0x80) != 0);
    }

    fn push(&mut self, mem: &mut C64Mem, v: u8) {
        let addr = 0x0100u16 | (self.regs.sp as u16);
        mem.write_byte(addr, v);
        self.regs.sp = self.regs.sp.wrapping_sub(1);
    }
    fn pop(&mut self, mem: &C64Mem) -> u8 {
        self.regs.sp = self.regs.sp.wrapping_add(1);
        mem.read_byte(0x0100u16 | (self.regs.sp as u16))
    }
    fn push_word(&mut self, mem: &mut C64Mem, w: u16) {
        self.push(mem, (w >> 8) as u8);
        self.push(mem, w as u8);
    }
    fn pop_word(&mut self, mem: &C64Mem) -> u16 {
        let lo = self.pop(mem) as u16;
        let hi = self.pop(mem) as u16;
        (hi << 8) | lo
    }

    fn fetch_byte(&mut self, mem: &C64Mem) -> u8 {
        let v = mem.read_byte(self.regs.pc);
        self.regs.pc = self.regs.pc.wrapping_add(1);
        v
    }
    fn fetch_word(&mut self, mem: &C64Mem) -> u16 {
        let lo = self.fetch_byte(mem) as u16;
        let hi = self.fetch_byte(mem) as u16;
        (hi << 8) | lo
    }

    // --------------------------------------------------- addressing modes

    #[inline]
    fn am_zp(&mut self, mem: &C64Mem) -> u16 {
        self.fetch_byte(mem) as u16
    }
    #[inline]
    fn am_zpx(&mut self, mem: &C64Mem) -> u16 {
        let b = self.fetch_byte(mem) as u16;
        (b.wrapping_add(self.regs.x as u16)) & 0xFF
    }
    #[inline]
    fn am_zpy(&mut self, mem: &C64Mem) -> u16 {
        let b = self.fetch_byte(mem) as u16;
        (b.wrapping_add(self.regs.y as u16)) & 0xFF
    }
    #[inline]
    fn am_abs(&mut self, mem: &C64Mem) -> u16 {
        self.fetch_word(mem)
    }
    #[inline]
    fn am_abx(&mut self, mem: &C64Mem) -> u16 {
        self.fetch_word(mem).wrapping_add(self.regs.x as u16)
    }
    #[inline]
    fn am_aby(&mut self, mem: &C64Mem) -> u16 {
        self.fetch_word(mem).wrapping_add(self.regs.y as u16)
    }
    fn am_indx(&mut self, mem: &C64Mem) -> u16 {
        let zp = self.fetch_byte(mem).wrapping_add(self.regs.x) as u16;
        let lo = mem.read_byte(zp & 0xFF) as u16;
        let hi = mem.read_byte((zp.wrapping_add(1)) & 0xFF) as u16;
        (hi << 8) | lo
    }
    fn am_indy(&mut self, mem: &C64Mem) -> u16 {
        let zp = self.fetch_byte(mem) as u16;
        let lo = mem.read_byte(zp & 0xFF) as u16;
        let hi = mem.read_byte((zp.wrapping_add(1)) & 0xFF) as u16;
        let base = (hi << 8) | lo;
        base.wrapping_add(self.regs.y as u16)
    }

    // --------------------------------------------------- ALU ops

    fn adc(&mut self, v: u8) {
        let a = self.regs.a as u16;
        let m = v as u16;
        let c = if self.flag(FLAG_C) { 1 } else { 0 };
        if self.flag(FLAG_D) {
            let mut lo = (a & 0x0F) + (m & 0x0F) + c;
            if lo > 0x09 {
                lo += 0x06;
            }
            let mut hi = (a >> 4) + (m >> 4) + if lo > 0x0F { 1 } else { 0 };
            if hi > 0x09 {
                hi += 0x06;
            }
            let result = ((hi & 0x0F) << 4) | (lo & 0x0F);
            self.set_flag(FLAG_C, hi > 0x0F);
            self.regs.a = result as u8;
            self.set_nz(self.regs.a);
        } else {
            let sum = a + m + c;
            let result = (sum & 0xFF) as u8;
            self.regs.a = result;
            self.set_flag(FLAG_C, sum > 0xFF);
            let av = a as u8;
            let mv = m as u8;
            self.set_flag(FLAG_V, ((av ^ result) & (mv ^ result) & 0x80) != 0);
            self.set_nz(self.regs.a);
        }
    }
    fn sbc(&mut self, v: u8) {
        let a = self.regs.a as i32;
        let m = v as i32;
        let c = if self.flag(FLAG_C) { 1 } else { 0 };
        if self.flag(FLAG_D) {
            let mut lo = (a & 0x0F) - (m & 0x0F) - (1 - c);
            if lo < 0 {
                lo -= 0x06;
            }
            let mut hi = (a >> 4) - (m >> 4) - if lo < 0 { 1 } else { 0 };
            if hi < 0 {
                hi -= 0x06;
            }
            let result = ((hi & 0x0F) << 4) | (lo & 0x0F);
            self.set_flag(FLAG_C, hi >= 0);
            self.regs.a = result as u8;
            self.set_nz(self.regs.a);
        } else {
            let diff = a - m - (1 - c);
            let result = (diff & 0xFF) as u8;
            self.regs.a = result;
            self.set_flag(FLAG_C, diff >= 0);
            let av = a as u8;
            let mv = m as u8;
            self.set_flag(FLAG_V, ((av ^ mv) & (av ^ result) & 0x80) != 0);
            self.set_nz(self.regs.a);
        }
    }
    fn cmp_op(&mut self, lhs: u8, v: u8) {
        let result = (lhs as i32) - (v as i32);
        self.set_flag(FLAG_C, lhs >= v);
        self.set_flag(FLAG_Z, lhs == v);
        self.set_flag(FLAG_N, ((result as u32) & 0x80) != 0);
    }

    fn branch(&mut self, mem: &C64Mem, cond: bool) {
        let off = self.fetch_byte(mem) as i8;
        if cond {
            self.regs.pc = self.regs.pc.wrapping_add(off as i16 as u16);
        }
    }

    // --------------------------------------------------- step

    /// Execute one instruction. Returns `false` once the CPU has
    /// halted on BRK or JAM.
    pub fn step(&mut self, mem: &mut C64Mem) -> bool {
        if self.jammed || self.brk_hit {
            return false;
        }
        let op = self.fetch_byte(mem);
        self.execute(mem, op);
        self.cycles = self.cycles.wrapping_add(1);
        !self.jammed && !self.brk_hit
    }

    fn execute(&mut self, mem: &mut C64Mem, op: u8) {
        match op {
            // ---- ORA ----
            0x09 => {
                let v = self.fetch_byte(mem);
                self.regs.a |= v;
                self.set_nz(self.regs.a);
            }
            0x05 => {
                let a = self.am_zp(mem);
                let v = mem.read_byte(a);
                self.regs.a |= v;
                self.set_nz(self.regs.a);
            }
            0x15 => {
                let a = self.am_zpx(mem);
                let v = mem.read_byte(a);
                self.regs.a |= v;
                self.set_nz(self.regs.a);
            }
            0x0D => {
                let a = self.am_abs(mem);
                let v = mem.read_byte(a);
                self.regs.a |= v;
                self.set_nz(self.regs.a);
            }
            0x1D => {
                let a = self.am_abx(mem);
                let v = mem.read_byte(a);
                self.regs.a |= v;
                self.set_nz(self.regs.a);
            }
            0x19 => {
                let a = self.am_aby(mem);
                let v = mem.read_byte(a);
                self.regs.a |= v;
                self.set_nz(self.regs.a);
            }
            0x01 => {
                let a = self.am_indx(mem);
                let v = mem.read_byte(a);
                self.regs.a |= v;
                self.set_nz(self.regs.a);
            }
            0x11 => {
                let a = self.am_indy(mem);
                let v = mem.read_byte(a);
                self.regs.a |= v;
                self.set_nz(self.regs.a);
            }

            // ---- AND ----
            0x29 => {
                let v = self.fetch_byte(mem);
                self.regs.a &= v;
                self.set_nz(self.regs.a);
            }
            0x25 => {
                let a = self.am_zp(mem);
                let v = mem.read_byte(a);
                self.regs.a &= v;
                self.set_nz(self.regs.a);
            }
            0x35 => {
                let a = self.am_zpx(mem);
                let v = mem.read_byte(a);
                self.regs.a &= v;
                self.set_nz(self.regs.a);
            }
            0x2D => {
                let a = self.am_abs(mem);
                let v = mem.read_byte(a);
                self.regs.a &= v;
                self.set_nz(self.regs.a);
            }
            0x3D => {
                let a = self.am_abx(mem);
                let v = mem.read_byte(a);
                self.regs.a &= v;
                self.set_nz(self.regs.a);
            }
            0x39 => {
                let a = self.am_aby(mem);
                let v = mem.read_byte(a);
                self.regs.a &= v;
                self.set_nz(self.regs.a);
            }
            0x21 => {
                let a = self.am_indx(mem);
                let v = mem.read_byte(a);
                self.regs.a &= v;
                self.set_nz(self.regs.a);
            }
            0x31 => {
                let a = self.am_indy(mem);
                let v = mem.read_byte(a);
                self.regs.a &= v;
                self.set_nz(self.regs.a);
            }

            // ---- EOR ----
            0x49 => {
                let v = self.fetch_byte(mem);
                self.regs.a ^= v;
                self.set_nz(self.regs.a);
            }
            0x45 => {
                let a = self.am_zp(mem);
                let v = mem.read_byte(a);
                self.regs.a ^= v;
                self.set_nz(self.regs.a);
            }
            0x55 => {
                let a = self.am_zpx(mem);
                let v = mem.read_byte(a);
                self.regs.a ^= v;
                self.set_nz(self.regs.a);
            }
            0x4D => {
                let a = self.am_abs(mem);
                let v = mem.read_byte(a);
                self.regs.a ^= v;
                self.set_nz(self.regs.a);
            }
            0x5D => {
                let a = self.am_abx(mem);
                let v = mem.read_byte(a);
                self.regs.a ^= v;
                self.set_nz(self.regs.a);
            }
            0x59 => {
                let a = self.am_aby(mem);
                let v = mem.read_byte(a);
                self.regs.a ^= v;
                self.set_nz(self.regs.a);
            }
            0x41 => {
                let a = self.am_indx(mem);
                let v = mem.read_byte(a);
                self.regs.a ^= v;
                self.set_nz(self.regs.a);
            }
            0x51 => {
                let a = self.am_indy(mem);
                let v = mem.read_byte(a);
                self.regs.a ^= v;
                self.set_nz(self.regs.a);
            }

            // ---- ADC ----
            0x69 => {
                let v = self.fetch_byte(mem);
                self.adc(v);
            }
            0x65 => {
                let a = self.am_zp(mem);
                let v = mem.read_byte(a);
                self.adc(v);
            }
            0x75 => {
                let a = self.am_zpx(mem);
                let v = mem.read_byte(a);
                self.adc(v);
            }
            0x6D => {
                let a = self.am_abs(mem);
                let v = mem.read_byte(a);
                self.adc(v);
            }
            0x7D => {
                let a = self.am_abx(mem);
                let v = mem.read_byte(a);
                self.adc(v);
            }
            0x79 => {
                let a = self.am_aby(mem);
                let v = mem.read_byte(a);
                self.adc(v);
            }
            0x61 => {
                let a = self.am_indx(mem);
                let v = mem.read_byte(a);
                self.adc(v);
            }
            0x71 => {
                let a = self.am_indy(mem);
                let v = mem.read_byte(a);
                self.adc(v);
            }

            // ---- SBC ----
            0xE9 | 0xEB => {
                let v = self.fetch_byte(mem);
                self.sbc(v);
            }
            0xE5 => {
                let a = self.am_zp(mem);
                let v = mem.read_byte(a);
                self.sbc(v);
            }
            0xF5 => {
                let a = self.am_zpx(mem);
                let v = mem.read_byte(a);
                self.sbc(v);
            }
            0xED => {
                let a = self.am_abs(mem);
                let v = mem.read_byte(a);
                self.sbc(v);
            }
            0xFD => {
                let a = self.am_abx(mem);
                let v = mem.read_byte(a);
                self.sbc(v);
            }
            0xF9 => {
                let a = self.am_aby(mem);
                let v = mem.read_byte(a);
                self.sbc(v);
            }
            0xE1 => {
                let a = self.am_indx(mem);
                let v = mem.read_byte(a);
                self.sbc(v);
            }
            0xF1 => {
                let a = self.am_indy(mem);
                let v = mem.read_byte(a);
                self.sbc(v);
            }

            // ---- CMP ----
            0xC9 => {
                let v = self.fetch_byte(mem);
                self.cmp_op(self.regs.a, v);
            }
            0xC5 => {
                let a = self.am_zp(mem);
                let v = mem.read_byte(a);
                self.cmp_op(self.regs.a, v);
            }
            0xD5 => {
                let a = self.am_zpx(mem);
                let v = mem.read_byte(a);
                self.cmp_op(self.regs.a, v);
            }
            0xCD => {
                let a = self.am_abs(mem);
                let v = mem.read_byte(a);
                self.cmp_op(self.regs.a, v);
            }
            0xDD => {
                let a = self.am_abx(mem);
                let v = mem.read_byte(a);
                self.cmp_op(self.regs.a, v);
            }
            0xD9 => {
                let a = self.am_aby(mem);
                let v = mem.read_byte(a);
                self.cmp_op(self.regs.a, v);
            }
            0xC1 => {
                let a = self.am_indx(mem);
                let v = mem.read_byte(a);
                self.cmp_op(self.regs.a, v);
            }
            0xD1 => {
                let a = self.am_indy(mem);
                let v = mem.read_byte(a);
                self.cmp_op(self.regs.a, v);
            }

            // ---- CPX ----
            0xE0 => {
                let v = self.fetch_byte(mem);
                self.cmp_op(self.regs.x, v);
            }
            0xE4 => {
                let a = self.am_zp(mem);
                let v = mem.read_byte(a);
                self.cmp_op(self.regs.x, v);
            }
            0xEC => {
                let a = self.am_abs(mem);
                let v = mem.read_byte(a);
                self.cmp_op(self.regs.x, v);
            }

            // ---- CPY ----
            0xC0 => {
                let v = self.fetch_byte(mem);
                self.cmp_op(self.regs.y, v);
            }
            0xC4 => {
                let a = self.am_zp(mem);
                let v = mem.read_byte(a);
                self.cmp_op(self.regs.y, v);
            }
            0xCC => {
                let a = self.am_abs(mem);
                let v = mem.read_byte(a);
                self.cmp_op(self.regs.y, v);
            }

            // ---- BIT ----
            0x24 => {
                let a = self.am_zp(mem);
                let v = mem.read_byte(a);
                self.set_flag(FLAG_Z, (self.regs.a & v) == 0);
                self.set_flag(FLAG_V, (v & 0x40) != 0);
                self.set_flag(FLAG_N, (v & 0x80) != 0);
            }
            0x2C => {
                let a = self.am_abs(mem);
                let v = mem.read_byte(a);
                self.set_flag(FLAG_Z, (self.regs.a & v) == 0);
                self.set_flag(FLAG_V, (v & 0x40) != 0);
                self.set_flag(FLAG_N, (v & 0x80) != 0);
            }

            // ---- LDA ----
            0xA9 => {
                let v = self.fetch_byte(mem);
                self.regs.a = v;
                self.set_nz(v);
            }
            0xA5 => {
                let a = self.am_zp(mem);
                let v = mem.read_byte(a);
                self.regs.a = v;
                self.set_nz(v);
            }
            0xB5 => {
                let a = self.am_zpx(mem);
                let v = mem.read_byte(a);
                self.regs.a = v;
                self.set_nz(v);
            }
            0xAD => {
                let a = self.am_abs(mem);
                let v = mem.read_byte(a);
                self.regs.a = v;
                self.set_nz(v);
            }
            0xBD => {
                let a = self.am_abx(mem);
                let v = mem.read_byte(a);
                self.regs.a = v;
                self.set_nz(v);
            }
            0xB9 => {
                let a = self.am_aby(mem);
                let v = mem.read_byte(a);
                self.regs.a = v;
                self.set_nz(v);
            }
            0xA1 => {
                let a = self.am_indx(mem);
                let v = mem.read_byte(a);
                self.regs.a = v;
                self.set_nz(v);
            }
            0xB1 => {
                let a = self.am_indy(mem);
                let v = mem.read_byte(a);
                self.regs.a = v;
                self.set_nz(v);
            }

            // ---- LDX ----
            0xA2 => {
                let v = self.fetch_byte(mem);
                self.regs.x = v;
                self.set_nz(v);
            }
            0xA6 => {
                let a = self.am_zp(mem);
                let v = mem.read_byte(a);
                self.regs.x = v;
                self.set_nz(v);
            }
            0xB6 => {
                let a = self.am_zpy(mem);
                let v = mem.read_byte(a);
                self.regs.x = v;
                self.set_nz(v);
            }
            0xAE => {
                let a = self.am_abs(mem);
                let v = mem.read_byte(a);
                self.regs.x = v;
                self.set_nz(v);
            }
            0xBE => {
                let a = self.am_aby(mem);
                let v = mem.read_byte(a);
                self.regs.x = v;
                self.set_nz(v);
            }

            // ---- LDY ----
            0xA0 => {
                let v = self.fetch_byte(mem);
                self.regs.y = v;
                self.set_nz(v);
            }
            0xA4 => {
                let a = self.am_zp(mem);
                let v = mem.read_byte(a);
                self.regs.y = v;
                self.set_nz(v);
            }
            0xB4 => {
                let a = self.am_zpx(mem);
                let v = mem.read_byte(a);
                self.regs.y = v;
                self.set_nz(v);
            }
            0xAC => {
                let a = self.am_abs(mem);
                let v = mem.read_byte(a);
                self.regs.y = v;
                self.set_nz(v);
            }
            0xBC => {
                let a = self.am_abx(mem);
                let v = mem.read_byte(a);
                self.regs.y = v;
                self.set_nz(v);
            }

            // ---- STA ----
            0x85 => {
                let a = self.am_zp(mem);
                mem.write_byte(a, self.regs.a);
            }
            0x95 => {
                let a = self.am_zpx(mem);
                mem.write_byte(a, self.regs.a);
            }
            0x8D => {
                let a = self.am_abs(mem);
                mem.write_byte(a, self.regs.a);
            }
            0x9D => {
                let a = self.am_abx(mem);
                mem.write_byte(a, self.regs.a);
            }
            0x99 => {
                let a = self.am_aby(mem);
                mem.write_byte(a, self.regs.a);
            }
            0x81 => {
                let a = self.am_indx(mem);
                mem.write_byte(a, self.regs.a);
            }
            0x91 => {
                let a = self.am_indy(mem);
                mem.write_byte(a, self.regs.a);
            }

            // ---- STX ----
            0x86 => {
                let a = self.am_zp(mem);
                mem.write_byte(a, self.regs.x);
            }
            0x96 => {
                let a = self.am_zpy(mem);
                mem.write_byte(a, self.regs.x);
            }
            0x8E => {
                let a = self.am_abs(mem);
                mem.write_byte(a, self.regs.x);
            }

            // ---- STY ----
            0x84 => {
                let a = self.am_zp(mem);
                mem.write_byte(a, self.regs.y);
            }
            0x94 => {
                let a = self.am_zpx(mem);
                mem.write_byte(a, self.regs.y);
            }
            0x8C => {
                let a = self.am_abs(mem);
                mem.write_byte(a, self.regs.y);
            }

            // ---- Transfers ----
            0xAA => {
                self.regs.x = self.regs.a;
                self.set_nz(self.regs.x);
            }
            0xA8 => {
                self.regs.y = self.regs.a;
                self.set_nz(self.regs.y);
            }
            0x8A => {
                self.regs.a = self.regs.x;
                self.set_nz(self.regs.a);
            }
            0x98 => {
                self.regs.a = self.regs.y;
                self.set_nz(self.regs.a);
            }
            0xBA => {
                self.regs.x = self.regs.sp;
                self.set_nz(self.regs.x);
            }
            0x9A => {
                self.regs.sp = self.regs.x;
            }

            // ---- Shifts on A ----
            0x0A => {
                let v = self.regs.a;
                self.set_flag(FLAG_C, v & 0x80 != 0);
                self.regs.a = v << 1;
                self.set_nz(self.regs.a);
            }
            0x4A => {
                let v = self.regs.a;
                self.set_flag(FLAG_C, v & 0x01 != 0);
                self.regs.a = v >> 1;
                self.set_nz(self.regs.a);
            }
            0x2A => {
                let v = self.regs.a;
                let c = if self.flag(FLAG_C) { 1u8 } else { 0 };
                self.set_flag(FLAG_C, v & 0x80 != 0);
                self.regs.a = (v << 1) | c;
                self.set_nz(self.regs.a);
            }
            0x6A => {
                let v = self.regs.a;
                let c = if self.flag(FLAG_C) { 0x80u8 } else { 0 };
                self.set_flag(FLAG_C, v & 0x01 != 0);
                self.regs.a = (v >> 1) | c;
                self.set_nz(self.regs.a);
            }

            // ---- Shifts on memory ----
            0x06 => {
                let a = self.am_zp(mem);
                self.asl_mem(mem, a);
            }
            0x16 => {
                let a = self.am_zpx(mem);
                self.asl_mem(mem, a);
            }
            0x0E => {
                let a = self.am_abs(mem);
                self.asl_mem(mem, a);
            }
            0x1E => {
                let a = self.am_abx(mem);
                self.asl_mem(mem, a);
            }
            0x46 => {
                let a = self.am_zp(mem);
                self.lsr_mem(mem, a);
            }
            0x56 => {
                let a = self.am_zpx(mem);
                self.lsr_mem(mem, a);
            }
            0x4E => {
                let a = self.am_abs(mem);
                self.lsr_mem(mem, a);
            }
            0x5E => {
                let a = self.am_abx(mem);
                self.lsr_mem(mem, a);
            }
            0x26 => {
                let a = self.am_zp(mem);
                self.rol_mem(mem, a);
            }
            0x36 => {
                let a = self.am_zpx(mem);
                self.rol_mem(mem, a);
            }
            0x2E => {
                let a = self.am_abs(mem);
                self.rol_mem(mem, a);
            }
            0x3E => {
                let a = self.am_abx(mem);
                self.rol_mem(mem, a);
            }
            0x66 => {
                let a = self.am_zp(mem);
                self.ror_mem(mem, a);
            }
            0x76 => {
                let a = self.am_zpx(mem);
                self.ror_mem(mem, a);
            }
            0x6E => {
                let a = self.am_abs(mem);
                self.ror_mem(mem, a);
            }
            0x7E => {
                let a = self.am_abx(mem);
                self.ror_mem(mem, a);
            }

            // ---- Jumps & Branches ----
            0x4C => {
                self.regs.pc = self.fetch_word(mem);
            }
            0x6C => {
                let p = self.fetch_word(mem);
                // Indirect JMP page-wrap quirk.
                let lo = mem.read_byte(p) as u16;
                let hi_addr = if (p & 0x00FF) == 0x00FF {
                    p & 0xFF00
                } else {
                    p.wrapping_add(1)
                };
                let hi = mem.read_byte(hi_addr) as u16;
                self.regs.pc = (hi << 8) | lo;
            }
            0x20 => {
                let t = self.fetch_word(mem);
                let ret = self.regs.pc.wrapping_sub(1);
                self.push_word(mem, ret);
                self.regs.pc = t;
            }
            0x60 => {
                let r = self.pop_word(mem);
                self.regs.pc = r.wrapping_add(1);
            }
            0x90 => {
                let c = !self.flag(FLAG_C);
                self.branch(mem, c);
            }
            0xB0 => {
                let c = self.flag(FLAG_C);
                self.branch(mem, c);
            }
            0xF0 => {
                let c = self.flag(FLAG_Z);
                self.branch(mem, c);
            }
            0xD0 => {
                let c = !self.flag(FLAG_Z);
                self.branch(mem, c);
            }
            0x30 => {
                let c = self.flag(FLAG_N);
                self.branch(mem, c);
            }
            0x10 => {
                let c = !self.flag(FLAG_N);
                self.branch(mem, c);
            }
            0x50 => {
                let c = !self.flag(FLAG_V);
                self.branch(mem, c);
            }
            0x70 => {
                let c = self.flag(FLAG_V);
                self.branch(mem, c);
            }

            // ---- Stack & system ----
            0x48 => {
                let a = self.regs.a;
                self.push(mem, a);
            }
            0x68 => {
                let v = self.pop(mem);
                self.regs.a = v;
                self.set_nz(v);
            }
            0x08 => {
                let p = self.regs.p | FLAG_B | FLAG_U;
                self.push(mem, p);
            }
            0x28 => {
                let p = self.pop(mem);
                self.regs.p = (p | FLAG_U) & !FLAG_B;
            }
            0x00 => {
                self.brk_hit = true;
            }
            0x40 => {
                let p = self.pop(mem);
                self.regs.p = (p | FLAG_U) & !FLAG_B;
                self.regs.pc = self.pop_word(mem);
            }
            0xEA => { /* NOP */ }

            // ---- Flag flips ----
            0x18 => self.set_flag(FLAG_C, false),
            0x38 => self.set_flag(FLAG_C, true),
            0xD8 => self.set_flag(FLAG_D, false),
            0xF8 => self.set_flag(FLAG_D, true),
            0x58 => self.set_flag(FLAG_I, false),
            0x78 => self.set_flag(FLAG_I, true),
            0xB8 => self.set_flag(FLAG_V, false),

            // ---- INC/DEC memory ----
            0xE6 => {
                let a = self.am_zp(mem);
                self.inc_mem(mem, a);
            }
            0xF6 => {
                let a = self.am_zpx(mem);
                self.inc_mem(mem, a);
            }
            0xEE => {
                let a = self.am_abs(mem);
                self.inc_mem(mem, a);
            }
            0xFE => {
                let a = self.am_abx(mem);
                self.inc_mem(mem, a);
            }
            0xC6 => {
                let a = self.am_zp(mem);
                self.dec_mem(mem, a);
            }
            0xD6 => {
                let a = self.am_zpx(mem);
                self.dec_mem(mem, a);
            }
            0xCE => {
                let a = self.am_abs(mem);
                self.dec_mem(mem, a);
            }
            0xDE => {
                let a = self.am_abx(mem);
                self.dec_mem(mem, a);
            }
            0xE8 => {
                self.regs.x = self.regs.x.wrapping_add(1);
                self.set_nz(self.regs.x);
            }
            0xC8 => {
                self.regs.y = self.regs.y.wrapping_add(1);
                self.set_nz(self.regs.y);
            }
            0xCA => {
                self.regs.x = self.regs.x.wrapping_sub(1);
                self.set_nz(self.regs.x);
            }
            0x88 => {
                self.regs.y = self.regs.y.wrapping_sub(1);
                self.set_nz(self.regs.y);
            }

            // ---- NOP variants (eat operands but do nothing) ----
            0x1A | 0x3A | 0x5A | 0x7A | 0xDA | 0xFA => { /* implied NOP */ }
            0x80 | 0x82 | 0x89 | 0xC2 | 0xE2 => {
                let _ = self.fetch_byte(mem);
            }
            0x04 | 0x44 | 0x64 => {
                let _ = self.am_zp(mem);
            }
            0x14 | 0x34 | 0x54 | 0x74 | 0xD4 | 0xF4 => {
                let _ = self.am_zpx(mem);
            }
            0x0C => {
                let _ = self.am_abs(mem);
            }
            0x1C | 0x3C | 0x5C | 0x7C | 0xDC | 0xFC => {
                let _ = self.am_abx(mem);
            }

            // ---- LAX (illegal) ----
            0xA7 => {
                let a = self.am_zp(mem);
                let v = mem.read_byte(a);
                self.regs.a = v;
                self.regs.x = v;
                self.set_nz(v);
            }
            0xB7 => {
                let a = self.am_zpy(mem);
                let v = mem.read_byte(a);
                self.regs.a = v;
                self.regs.x = v;
                self.set_nz(v);
            }
            0xAF => {
                let a = self.am_abs(mem);
                let v = mem.read_byte(a);
                self.regs.a = v;
                self.regs.x = v;
                self.set_nz(v);
            }
            0xBF => {
                let a = self.am_aby(mem);
                let v = mem.read_byte(a);
                self.regs.a = v;
                self.regs.x = v;
                self.set_nz(v);
            }
            0xA3 => {
                let a = self.am_indx(mem);
                let v = mem.read_byte(a);
                self.regs.a = v;
                self.regs.x = v;
                self.set_nz(v);
            }
            0xB3 => {
                let a = self.am_indy(mem);
                let v = mem.read_byte(a);
                self.regs.a = v;
                self.regs.x = v;
                self.set_nz(v);
            }
            0xAB => {
                let v = self.fetch_byte(mem);
                self.regs.a = v;
                self.regs.x = v;
                self.set_nz(v);
            }

            // ---- SAX (illegal) ----
            0x87 => {
                let a = self.am_zp(mem);
                mem.write_byte(a, self.regs.a & self.regs.x);
            }
            0x97 => {
                let a = self.am_zpy(mem);
                mem.write_byte(a, self.regs.a & self.regs.x);
            }
            0x8F => {
                let a = self.am_abs(mem);
                mem.write_byte(a, self.regs.a & self.regs.x);
            }
            0x83 => {
                let a = self.am_indx(mem);
                mem.write_byte(a, self.regs.a & self.regs.x);
            }

            // ---- DCP / ISC / SLO / RLA / SRE / RRA (illegal RMWs) ----
            0xC7 | 0xD7 | 0xCF | 0xDF | 0xDB | 0xC3 | 0xD3 => {
                let a = self.illegal_rmw_addr(mem, op);
                let v = mem.read_byte(a).wrapping_sub(1);
                mem.write_byte(a, v);
                self.cmp_op(self.regs.a, v);
            }
            0xE7 | 0xF7 | 0xEF | 0xFF | 0xFB | 0xE3 | 0xF3 => {
                let a = self.illegal_rmw_addr(mem, op);
                let v = mem.read_byte(a).wrapping_add(1);
                mem.write_byte(a, v);
                self.sbc(v);
            }
            0x07 | 0x17 | 0x0F | 0x1F | 0x1B | 0x03 | 0x13 => {
                let a = self.illegal_rmw_addr(mem, op);
                let v = mem.read_byte(a);
                self.set_flag(FLAG_C, v & 0x80 != 0);
                let nv = v << 1;
                mem.write_byte(a, nv);
                self.regs.a |= nv;
                self.set_nz(self.regs.a);
            }
            0x27 | 0x37 | 0x2F | 0x3F | 0x3B | 0x23 | 0x33 => {
                let a = self.illegal_rmw_addr(mem, op);
                let v = mem.read_byte(a);
                let c = if self.flag(FLAG_C) { 1u8 } else { 0 };
                self.set_flag(FLAG_C, v & 0x80 != 0);
                let nv = (v << 1) | c;
                mem.write_byte(a, nv);
                self.regs.a &= nv;
                self.set_nz(self.regs.a);
            }
            0x47 | 0x57 | 0x4F | 0x5F | 0x5B | 0x43 | 0x53 => {
                let a = self.illegal_rmw_addr(mem, op);
                let v = mem.read_byte(a);
                self.set_flag(FLAG_C, v & 0x01 != 0);
                let nv = v >> 1;
                mem.write_byte(a, nv);
                self.regs.a ^= nv;
                self.set_nz(self.regs.a);
            }
            0x67 | 0x77 | 0x6F | 0x7F | 0x7B | 0x63 | 0x73 => {
                let a = self.illegal_rmw_addr(mem, op);
                let v = mem.read_byte(a);
                let cin = if self.flag(FLAG_C) { 0x80u8 } else { 0 };
                self.set_flag(FLAG_C, v & 0x01 != 0);
                let nv = (v >> 1) | cin;
                mem.write_byte(a, nv);
                self.adc(nv);
            }

            // ---- JAM ----
            0x02 | 0x12 | 0x22 | 0x32 | 0x42 | 0x52 | 0x62 | 0x72 | 0x92 | 0xB2 | 0xD2 | 0xF2 => {
                self.jammed = true;
            }

            // Unimplemented opcodes silently fall through as NOPs.
            _ => {}
        }
    }

    fn illegal_rmw_addr(&mut self, mem: &C64Mem, op: u8) -> u16 {
        match op & 0x1F {
            0x07 => self.am_zp(mem),
            0x17 => self.am_zpx(mem),
            0x0F => self.am_abs(mem),
            0x1F => self.am_abx(mem),
            0x1B => self.am_aby(mem),
            0x03 => self.am_indx(mem),
            0x13 => self.am_indy(mem),
            _ => self.am_abs(mem),
        }
    }

    fn asl_mem(&mut self, mem: &mut C64Mem, a: u16) {
        let v = mem.read_byte(a);
        self.set_flag(FLAG_C, v & 0x80 != 0);
        let r = v << 1;
        mem.write_byte(a, r);
        self.set_nz(r);
    }
    fn lsr_mem(&mut self, mem: &mut C64Mem, a: u16) {
        let v = mem.read_byte(a);
        self.set_flag(FLAG_C, v & 0x01 != 0);
        let r = v >> 1;
        mem.write_byte(a, r);
        self.set_nz(r);
    }
    fn rol_mem(&mut self, mem: &mut C64Mem, a: u16) {
        let v = mem.read_byte(a);
        let c = if self.flag(FLAG_C) { 1u8 } else { 0 };
        self.set_flag(FLAG_C, v & 0x80 != 0);
        let r = (v << 1) | c;
        mem.write_byte(a, r);
        self.set_nz(r);
    }
    fn ror_mem(&mut self, mem: &mut C64Mem, a: u16) {
        let v = mem.read_byte(a);
        let c = if self.flag(FLAG_C) { 0x80u8 } else { 0 };
        self.set_flag(FLAG_C, v & 0x01 != 0);
        let r = (v >> 1) | c;
        mem.write_byte(a, r);
        self.set_nz(r);
    }
    fn inc_mem(&mut self, mem: &mut C64Mem, a: u16) {
        let r = mem.read_byte(a).wrapping_add(1);
        mem.write_byte(a, r);
        self.set_nz(r);
    }
    fn dec_mem(&mut self, mem: &mut C64Mem, a: u16) {
        let r = mem.read_byte(a).wrapping_sub(1);
        mem.write_byte(a, r);
        self.set_nz(r);
    }
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}
