//! MOS 6510 / 6502 instruction emulator.
//! Execution limits count instructions, not hardware cycles.

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

/// Floating-bus value used by unstable immediate instructions.
const UNSTABLE_MAGIC: u8 = 0xEE;

/// Address and stored byte for unstable indexed stores.
fn unstable_store(base: u16, index: u8, src: u8) -> (u16, u8) {
    let lo = base as u8;
    let hi = (base >> 8) as u8;
    let crossed = lo as u16 + index as u16 > 0xFF;
    let value = src & hi.wrapping_add(1);
    let out_hi = if crossed { src & hi } else { hi };
    (((out_hi as u16) << 8) | lo.wrapping_add(index) as u16, value)
}

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
    /// Rotate A AND the operand through carry with ARR flag and decimal rules.
    fn arr(&mut self, m: u8) {
        let t = self.regs.a & m;
        let carry_in = if self.flag(FLAG_C) { 0x80u8 } else { 0 };
        let r = (t >> 1) | carry_in;
        if !self.flag(FLAG_D) {
            self.regs.a = r;
            self.set_nz(r);
            self.set_flag(FLAG_C, r & 0x40 != 0);
            self.set_flag(FLAG_V, ((r >> 6) ^ (r >> 5)) & 0x01 != 0);
            return;
        }

        self.set_flag(FLAG_N, carry_in != 0);
        self.set_flag(FLAG_Z, r == 0);
        self.set_flag(FLAG_V, (t ^ r) & 0x40 != 0);
        let mut a = r;
        if (t & 0x0F) + (t & 0x01) > 0x05 {
            a = (a & 0xF0) | (a.wrapping_add(0x06) & 0x0F);
        }
        if (t as u16 & 0xF0) + (t as u16 & 0x10) > 0x50 {
            a = a.wrapping_add(0x60);
            self.set_flag(FLAG_C, true);
        } else {
            self.set_flag(FLAG_C, false);
        }
        self.regs.a = a;
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

            0x0B | 0x2B => {
                let v = self.fetch_byte(mem);
                self.regs.a &= v;
                self.set_nz(self.regs.a);
                self.set_flag(FLAG_C, self.flag(FLAG_N));
            }

            0x4B => {
                let v = self.fetch_byte(mem);
                let t = self.regs.a & v;
                self.set_flag(FLAG_C, t & 0x01 != 0);
                self.regs.a = t >> 1;
                self.set_nz(self.regs.a);
            }

            0x6B => {
                let v = self.fetch_byte(mem);
                self.arr(v);
            }

            0xCB => {
                let v = self.fetch_byte(mem);
                let t = self.regs.a & self.regs.x;
                self.set_flag(FLAG_C, t >= v);
                self.regs.x = t.wrapping_sub(v);
                self.set_nz(self.regs.x);
            }

            0xBB => {
                let a = self.am_aby(mem);
                let r = mem.read_byte(a) & self.regs.sp;
                self.regs.sp = r;
                self.regs.x = r;
                self.regs.a = r;
                self.set_nz(r);
            }

            0x8B => {
                let v = self.fetch_byte(mem);
                self.regs.a = (self.regs.a | UNSTABLE_MAGIC) & self.regs.x & v;
                self.set_nz(self.regs.a);
            }

            0x9F => {
                let base = self.fetch_word(mem);
                let src = self.regs.a & self.regs.x;
                let (addr, v) = unstable_store(base, self.regs.y, src);
                mem.write_byte(addr, v);
            }
            0x93 => {
                let zp = self.fetch_byte(mem) as u16;
                let lo = mem.read_byte(zp) as u16;
                let hi = mem.read_byte(zp.wrapping_add(1) & 0xFF) as u16;
                let base = (hi << 8) | lo;
                let src = self.regs.a & self.regs.x;
                let (addr, v) = unstable_store(base, self.regs.y, src);
                mem.write_byte(addr, v);
            }
            0x9E => {
                let base = self.fetch_word(mem);
                let (addr, v) = unstable_store(base, self.regs.y, self.regs.x);
                mem.write_byte(addr, v);
            }
            0x9C => {
                let base = self.fetch_word(mem);
                let (addr, v) = unstable_store(base, self.regs.x, self.regs.y);
                mem.write_byte(addr, v);
            }
            0x9B => {
                let base = self.fetch_word(mem);
                let src = self.regs.a & self.regs.x;
                // TAS sets SP unconditionally, before the store.
                self.regs.sp = src;
                let (addr, v) = unstable_store(base, self.regs.y, src);
                mem.write_byte(addr, v);
            }

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Where every probe plants its one instruction.
    const CODE: u16 = 0xC000;

    /// Instruction length for each opcode.
    #[rustfmt::skip]
    const LEN: [u8; 256] = [
        1, 2, 1, 2, 2, 2, 2, 2, 1, 2, 1, 2, 3, 3, 3, 3,
        2, 2, 1, 2, 2, 2, 2, 2, 1, 3, 1, 3, 3, 3, 3, 3,
        3, 2, 1, 2, 2, 2, 2, 2, 1, 2, 1, 2, 3, 3, 3, 3,
        2, 2, 1, 2, 2, 2, 2, 2, 1, 3, 1, 3, 3, 3, 3, 3,
        1, 2, 1, 2, 2, 2, 2, 2, 1, 2, 1, 2, 3, 3, 3, 3,
        2, 2, 1, 2, 2, 2, 2, 2, 1, 3, 1, 3, 3, 3, 3, 3,
        1, 2, 1, 2, 2, 2, 2, 2, 1, 2, 1, 2, 3, 3, 3, 3,
        2, 2, 1, 2, 2, 2, 2, 2, 1, 3, 1, 3, 3, 3, 3, 3,
        2, 2, 2, 2, 2, 2, 2, 2, 1, 2, 1, 2, 3, 3, 3, 3,
        2, 2, 1, 2, 2, 2, 2, 2, 1, 3, 1, 3, 3, 3, 3, 3,
        2, 2, 2, 2, 2, 2, 2, 2, 1, 2, 1, 2, 3, 3, 3, 3,
        2, 2, 1, 2, 2, 2, 2, 2, 1, 3, 1, 3, 3, 3, 3, 3,
        2, 2, 2, 2, 2, 2, 2, 2, 1, 2, 1, 2, 3, 3, 3, 3,
        2, 2, 1, 2, 2, 2, 2, 2, 1, 3, 1, 3, 3, 3, 3, 3,
        2, 2, 2, 2, 2, 2, 2, 2, 1, 2, 1, 2, 3, 3, 3, 3,
        2, 2, 1, 2, 2, 2, 2, 2, 1, 3, 1, 3, 3, 3, 3, 3,
    ];

    const SWALLOWED: [u8; 12] = [
        0x0B, 0x2B, 0x4B, 0x6B, 0x8B, 0x93, 0x9B, 0x9C, 0x9E, 0x9F, 0xBB, 0xCB,
    ];

    /// The twelve `JAM`s, which halt the CPU where it stands.
    const JAMS: [u8; 12] = [
        0x02, 0x12, 0x22, 0x32, 0x42, 0x52, 0x62, 0x72, 0x92, 0xB2, 0xD2, 0xF2,
    ];

    fn sets_pc(op: u8) -> bool {
        matches!(op, 0x00 | 0x20 | 0x4C | 0x6C | 0x40 | 0x60)
    }
    fn is_branch(op: u8) -> bool {
        (op & 0x1F) == 0x10
    }

    /// All-RAM memory with a distinct value at every address a `$82 $12` operand could name under any addressing mode.
    fn fresh() -> C64Mem {
        let mut mem = C64Mem::new();
        mem.ram[0x0001] = 0x00;
        mem.ram[0x0082] = 0x20; // the zp target, and the ($82),Y pointer low
        mem.ram[0x0083] = 0x50;
        mem.ram[0x0093] = 0x40;
        mem
    }

    /// Plant `op o1 o2` at `CODE`, single-step it from the given registers, and hand back the CPU.
    #[allow(clippy::too_many_arguments)]
    fn exec(mem: &mut C64Mem, op: u8, o1: u8, o2: u8, a: u8, x: u8, y: u8, p: u8) -> Cpu {
        mem.ram[CODE as usize] = op;
        mem.ram[CODE as usize + 1] = o1;
        mem.ram[CODE as usize + 2] = o2;
        mem.ram[CODE as usize + 3] = 0xEA;
        mem.ram[CODE as usize + 4] = 0xEA;
        let mut cpu = Cpu::new();
        cpu.run_setup(CODE);
        cpu.regs.a = a;
        cpu.regs.x = x;
        cpu.regs.y = y;
        cpu.regs.p = p | FLAG_U;
        cpu.step(mem);
        cpu
    }

    /// `exec` with the standard `$82 $12` operand.
    fn run_op(mem: &mut C64Mem, op: u8, a: u8, x: u8, y: u8, p: u8) -> Cpu {
        exec(mem, op, 0x82, 0x12, a, x, y, p)
    }

    /// An immediate-mode instruction on a throwaway machine.
    fn imm(op: u8, i: u8, a: u8, x: u8, p: u8) -> Cpu {
        let mut mem = fresh();
        exec(&mut mem, op, i, 0x12, a, x, 0, p)
    }

    fn pc_advance(op: u8, p: u8) -> u16 {
        let mut mem = fresh();
        run_op(&mut mem, op, 0x5A, 0x10, 0x20, p)
            .regs
            .pc
            .wrapping_sub(CODE)
    }

    #[test]
    fn every_opcode_advances_pc_by_its_true_length() {
        let mut checked = 0usize;
        for op in 0u16..=0xFF {
            let op = op as u8;
            if sets_pc(op) || is_branch(op) {
                continue;
            }
            assert_eq!(
                pc_advance(op, 0) as u8,
                LEN[op as usize],
                "${op:02X} advanced the wrong number of bytes"
            );
            checked += 1;
        }

        for (p, ops) in [
            (0u8, [0x30u8, 0x70, 0xB0, 0xF0]),
            (FLAG_N | FLAG_V | FLAG_Z | FLAG_C, [0x10u8, 0x50, 0x90, 0xD0]),
        ] {
            for op in ops {
                assert_eq!(
                    pc_advance(op, p) as u8,
                    LEN[op as usize],
                    "${op:02X} not-taken branch length"
                );
                checked += 1;
            }
        }

        assert_eq!(checked, 250, "opcodes measured");

        let n = |l: u8| LEN.iter().filter(|&&x| x == l).count();
        assert_eq!((n(1), n(2), n(3)), (47, 128, 81), "the length oracle moved");
        assert_eq!(n(1) + n(2) + n(3), 256, "every opcode has a length");
    }

    #[test]
    fn none_of_the_twelve_is_still_a_nop() {
        // A setup in which every one of the twelve has a visible effect.
        let snapshot = |op: u8| {
            let mut mem = fresh();
            mem.ram[0x0082] = 0x00;
            mem.ram[0x0083] = 0x50;
            mem.ram[0x1286] = 0xFF; // abs,Y with Y = $04
            mem.ram[0x12B5] = 0xFF; // abs,X with X = $33
            mem.ram[0x5004] = 0xFF;
            let cpu = run_op(&mut mem, op, 0x5A, 0x33, 0x04, 0);
            let r = cpu.regs;
            let mut digest: u64 = 0;
            for (i, b) in mem.ram.iter().enumerate() {

                if (CODE as usize..CODE as usize + 5).contains(&i) {
                    continue;
                }
                digest = digest.wrapping_mul(31).wrapping_add((*b as u64) ^ (i as u64));
            }
            (r.a, r.x, r.y, r.sp, r.p, digest)
        };
        let nop = snapshot(0xEA);
        for op in SWALLOWED {
            assert_ne!(snapshot(op), nop, "${op:02X} still behaves as a NOP");
        }

        assert_eq!(snapshot(0x1A), nop, "$1A really is an undocumented NOP");
        assert_eq!(snapshot(0xEA), nop);
    }

    #[test]
    fn anc_copies_n_into_c() {
        for op in [0x0Bu8, 0x2B] {
            let cpu = imm(op, 0x8F, 0xF0, 0, 0);
            assert_eq!(cpu.regs.a, 0x80, "${op:02X}: $F0 & $8F");
            assert!(cpu.flag(FLAG_N), "${op:02X} N");
            assert!(cpu.flag(FLAG_C), "${op:02X} C := N");
            assert!(!cpu.flag(FLAG_Z));

            let cpu = imm(op, 0xF0, 0x0F, 0, FLAG_C);
            assert_eq!(cpu.regs.a, 0x00, "${op:02X}: $0F & $F0");
            assert!(cpu.flag(FLAG_Z));
            assert!(!cpu.flag(FLAG_N));
            assert!(!cpu.flag(FLAG_C), "${op:02X} C cleared with N");
        }
    }

    /// ALR / ASR ($4B): `A = (A & imm) >> 1`, C from the bit shifted out.
    #[test]
    fn alr_ands_then_shifts_right() {
        // $FF & $03 = $03 -> odd, so C comes out set; A = $01.
        let cpu = imm(0x4B, 0x03, 0xFF, 0, 0);
        assert_eq!(cpu.regs.a, 0x01);
        assert!(cpu.flag(FLAG_C), "bit 0 of $03 is set");
        assert!(!cpu.flag(FLAG_N), "a logical shift right never sets N");
        assert!(!cpu.flag(FLAG_Z));
        // $FF & $02 = $02 -> even, C clear, same A.
        let cpu = imm(0x4B, 0x02, 0xFF, 0, FLAG_C);
        assert_eq!(cpu.regs.a, 0x01);
        assert!(!cpu.flag(FLAG_C));

        let cpu = imm(0x4B, 0xFF, 0x80, 0, 0);
        assert_eq!(cpu.regs.a, 0x40);
        assert!(!cpu.flag(FLAG_Z));
        // Zero result sets Z.
        let cpu = imm(0x4B, 0x01, 0x01, 0, 0);
        assert_eq!(cpu.regs.a, 0x00);
        assert!(cpu.flag(FLAG_Z));
        assert!(cpu.flag(FLAG_C));
    }

    /// ARR ($6B) in binary mode: AND, ROR through C, then the opcode's own C (bit 6 of the result) and V (bit 6 XOR bit 5).
    #[test]
    fn arr_binary_mode_flags() {
        // $FF & $FF = $FF; carry in 0 -> $7F. bit6 = 1 -> C; bit6 ^ bit5 = 1 ^ 1 = 0 -> V clear.
        let cpu = imm(0x6B, 0xFF, 0xFF, 0, 0);
        assert_eq!(cpu.regs.a, 0x7F);
        assert!(cpu.flag(FLAG_C), "C = bit 6 of the result");
        assert!(!cpu.flag(FLAG_V), "bit6 == bit5");
        assert!(!cpu.flag(FLAG_N));
        // Carry IN rotates into bit 7: $FF -> $FF, and N follows.
        let cpu = imm(0x6B, 0xFF, 0xFF, 0, FLAG_C);
        assert_eq!(cpu.regs.a, 0xFF);
        assert!(cpu.flag(FLAG_N));
        assert!(cpu.flag(FLAG_C));
        assert!(!cpu.flag(FLAG_V));
        // $40 & $40 = $40 -> $20. bit6 = 0 -> C clear; bit6 ^ bit5 = 0 ^ 1 = 1 -> V set.
        let cpu = imm(0x6B, 0x40, 0x40, 0, 0);
        assert_eq!(cpu.regs.a, 0x20);
        assert!(!cpu.flag(FLAG_C));
        assert!(cpu.flag(FLAG_V), "V = bit6 XOR bit5");
        // Both clear.
        let cpu = imm(0x6B, 0x02, 0x02, 0, 0);
        assert_eq!(cpu.regs.a, 0x01);
        assert!(!cpu.flag(FLAG_C));
        assert!(!cpu.flag(FLAG_V));
    }

    #[test]
    fn arr_decimal_mode_fixes_up_the_result_after_setting_the_flags() {
        // t = $7F & $FF = $7F, carry in 0 -> r = $3F.
        let cpu = imm(0x6B, 0xFF, 0x7F, 0, FLAG_D);
        assert_eq!(cpu.regs.a, 0x95);
        assert!(cpu.flag(FLAG_C));
        assert!(cpu.flag(FLAG_V));
        assert!(
            !cpu.flag(FLAG_N),
            "N is decided before the fix-up, so $95 does not set it"
        );
        assert!(!cpu.flag(FLAG_Z));

        let cpu = imm(0x6B, 0xFF, 0x3F, 0, FLAG_D);
        assert_eq!(cpu.regs.a, 0x15);
        assert!(!cpu.flag(FLAG_C));
        assert!(!cpu.flag(FLAG_V));

        let cpu = imm(0x6B, 0xFF, 0xFF, 0, FLAG_D);
        assert_eq!(cpu.regs.a, 0xD5, "$7F -> $75 -> +$60");
        assert!(cpu.flag(FLAG_C));
    }

    #[test]
    fn sbx_subtracts_without_a_borrow_in() {
        for p in [0u8, FLAG_C] {
            let cpu = imm(0xCB, 0x82, 0xFF, 0x8F, p);
            assert_eq!(cpu.regs.x, 0x0D, "carry-in must not change the result");
            assert_eq!(cpu.regs.a, 0xFF, "A is untouched");
            assert!(cpu.flag(FLAG_C), "$8F >= $82, so no borrow");
            assert!(!cpu.flag(FLAG_Z));
            assert!(!cpu.flag(FLAG_N));
        }
        // A borrow wraps and clears C.
        let cpu = imm(0xCB, 0x82, 0xFF, 0x10, FLAG_C);
        assert_eq!(cpu.regs.x, 0x8E);
        assert!(!cpu.flag(FLAG_C));
        assert!(cpu.flag(FLAG_N));
        // Equal: zero, carry set.
        let cpu = imm(0xCB, 0x82, 0xFF, 0x82, 0);
        assert_eq!(cpu.regs.x, 0x00);
        assert!(cpu.flag(FLAG_Z));
        assert!(cpu.flag(FLAG_C));
        // It is (A AND X) that is reduced, not X alone.
        let cpu = imm(0xCB, 0x00, 0x0F, 0xFF, 0);
        assert_eq!(cpu.regs.x, 0x0F, "$0F & $FF, minus nothing");
    }

    /// LAS ($BB): `A = X = SP = M & SP`, addressed `abs,Y`.
    #[test]
    fn las_ands_memory_with_the_stack_pointer() {
        let mut mem = fresh();
        mem.ram[0x12A2] = 0xF0;
        mem.ram[CODE as usize] = 0xBB;
        mem.ram[CODE as usize + 1] = 0x82;
        mem.ram[CODE as usize + 2] = 0x12;
        let mut cpu = Cpu::new();
        cpu.run_setup(CODE);
        cpu.regs.y = 0x20;
        cpu.regs.sp = 0x3C;
        cpu.step(&mut mem);
        assert_eq!(cpu.regs.pc.wrapping_sub(CODE), 3, "abs,Y is three bytes");
        assert_eq!(cpu.regs.a, 0x30, "$F0 & $3C");
        assert_eq!(cpu.regs.x, 0x30);
        assert_eq!(cpu.regs.sp, 0x30, "LAS writes the stack pointer too");
        assert!(!cpu.flag(FLAG_Z));
        assert!(!cpu.flag(FLAG_N));
    }

    /// ANE / XAA ($8B): `A = (A | magic) & X & imm`.
    #[test]
    fn ane_folds_in_the_unstable_magic_constant() {

        assert_eq!(imm(0x8B, 0xFF, 0x00, 0x0F, 0).regs.a, 0x0E);
        // A zero accumulator exposes the floating-bus mask in the result.
        let cpu = imm(0x8B, 0xFF, 0x00, 0xFF, 0);
        assert_eq!(cpu.regs.a, 0xEE);
        assert!(cpu.flag(FLAG_N));
        // A's own bits fill the magic's gaps.
        assert_eq!(imm(0x8B, 0xFF, 0x11, 0xFF, 0).regs.a, 0xFF);

        let cpu = imm(0x8B, 0xFF, 0xFF, 0x00, 0);
        assert_eq!(cpu.regs.a, 0x00);
        assert!(cpu.flag(FLAG_Z));
    }

    fn run_store(op: u8, base: u16, a: u8, x: u8, y: u8) -> (C64Mem, Cpu) {
        let mut mem = fresh();
        let cpu = exec(&mut mem, op, base as u8, (base >> 8) as u8, a, x, y, 0);
        (mem, cpu)
    }

    /// With no page cross the address is the ordinary indexed one, and the value stored is `src & (high + 1)`.
    #[test]
    fn the_unstable_stores_fold_the_address_high_byte_into_the_value() {
        // SHA abs,Y: src = A & X = $FF. $1200 + $34 = $1234, high $12.
        let (mem, _) = run_store(0x9F, 0x1200, 0xFF, 0xFF, 0x34);
        assert_eq!(mem.ram[0x1234], 0x13, "SHA: (A & X) & ($12 + 1)");
        // SHX abs,Y: src = X.
        let (mem, _) = run_store(0x9E, 0x1200, 0x00, 0xAA, 0x34);
        assert_eq!(mem.ram[0x1234], 0x02, "SHX: X & ($12 + 1)");
        // SHY abs,X: src = Y, and the index is X.
        let (mem, _) = run_store(0x9C, 0x1200, 0x00, 0x34, 0xAA);
        assert_eq!(mem.ram[0x1234], 0x02, "SHY: Y & ($12 + 1)");

        let (mem, cpu) = run_store(0x9B, 0x1200, 0xF0, 0x3F, 0x34);
        assert_eq!(cpu.regs.sp, 0x30, "TAS sets SP to A & X");
        assert_eq!(mem.ram[0x1234], 0x10, "TAS: (A & X) & ($12 + 1)");
        // None of the four touches a flag.
        assert_eq!(cpu.regs.p, FLAG_U, "the unstable stores set no flags");

        let (mem, _) = run_store(0x9E, 0x2000, 0x00, 0xAA, 0x34);
        assert_eq!(mem.ram[0x2034], 0x20, "$AA & ($20 + 1) = $20");
    }

    // Distinct A and X bits verify that both registers affect the stored value.
    #[test]
    fn the_unstable_store_source_is_a_and_x_not_either_alone() {
        for (a, x) in [(0xFFu8, 0x0Fu8), (0x0F, 0xFF)] {
            let (mem, _) = run_store(0x9F, 0x1E00, a, x, 0x10);
            assert_eq!(mem.ram[0x1E10], 0x0F, "SHA abs,Y with A={a:02X} X={x:02X}");
            let (mem, cpu) = run_store(0x9B, 0x1E00, a, x, 0x10);
            assert_eq!(mem.ram[0x1E10], 0x0F, "TAS with A={a:02X} X={x:02X}");
            assert_eq!(cpu.regs.sp, 0x0F, "TAS SP with A={a:02X} X={x:02X}");
        }

        let (mem, _) = run_store(0x9F, 0x0C00, 0xFF, 0x0F, 0x10);
        assert_eq!(mem.ram[0x0C10], 0x0D, "$0F & ($0C + 1)");
    }

    #[test]
    fn sha_indirect_indexed_wraps_its_pointer_inside_the_zero_page() {
        let mut mem = fresh();
        mem.ram[0x00FF] = 0x00;
        mem.ram[0x0000] = 0x34; // the wrapped high byte -> $3400
        mem.ram[0x0100] = 0x99;
        let cpu = exec(&mut mem, 0x93, 0xFF, 0xEA, 0xFF, 0x0F, 0x12, 0);
        assert_eq!(cpu.regs.pc.wrapping_sub(CODE), 2);
        assert_eq!(mem.ram[0x3412], 0x05, "($FF),Y -> $3400 + $12; $0F & $35");

        assert_eq!(mem.ram[0x9912], 0x00, "nothing was written via $0100");
    }

    /// SHA ($93) takes its base from a zero-page pointer, and is two bytes long, not three.
    #[test]
    fn sha_indirect_indexed_reads_its_base_from_zero_page() {
        let mut mem = fresh();
        mem.ram[0x0080] = 0x00;
        mem.ram[0x0081] = 0x34;
        let cpu = exec(&mut mem, 0x93, 0x80, 0xEA, 0xFF, 0xFF, 0x12, 0);
        assert_eq!(cpu.regs.pc.wrapping_sub(CODE), 2, "($nn),Y is two bytes");
        assert_eq!(mem.ram[0x3412], 0x35, "$FF & ($34 + 1)");
    }

    #[test]
    fn a_page_cross_corrupts_the_target_address() {

        let (mem, _) = run_store(0x9E, 0x12F0, 0x00, 0xAA, 0x20);
        assert_eq!(mem.ram[0x0210], 0x02, "the corrupted target");
        assert_eq!(mem.ram[0x1310], 0x00, "the un-corrupted target is spared");

        let (mem, _) = run_store(0x9E, 0x12D0, 0x00, 0xAA, 0x20);
        assert_eq!(mem.ram[0x12F0], 0x02);
        assert_eq!(mem.ram[0x0210], 0x00, "no corruption without a cross");
    }

    #[test]
    fn the_page_cross_boundary_is_exact() {

        let (mem, _) = run_store(0x9E, 0x12EF, 0x00, 0xAA, 0x10);
        assert_eq!(mem.ram[0x12FF], 0x02, "$FF is still inside the page");
        assert_eq!(mem.ram[0x02FF], 0x00, "and nothing was corrupted");
        // One more, and it carries: the byte lands at ($AA & $12) << 8.
        let (mem, _) = run_store(0x9E, 0x12EF, 0x00, 0xAA, 0x11);
        assert_eq!(mem.ram[0x0200], 0x02, "the corrupted target");
        assert_eq!(mem.ram[0x1300], 0x00, "the named target is spared");
    }

    #[test]
    fn the_neighbours_of_the_twelve_still_work() {
        let one = |op: u8, a: u8, x: u8, p: u8| {
            let mut mem = fresh();
            run_op(&mut mem, op, a, x, 0, p)
        };
        assert_eq!(one(0x0A, 0x81, 0, 0).regs.a, 0x02, "ASL A");
        assert!(one(0x0A, 0x81, 0, 0).flag(FLAG_C), "ASL A carry");
        assert_eq!(one(0x2A, 0x81, 0, FLAG_C).regs.a, 0x03, "ROL A");
        assert_eq!(one(0x4A, 0x81, 0, 0).regs.a, 0x40, "LSR A");
        assert_eq!(one(0x6A, 0x81, 0, FLAG_C).regs.a, 0xC0, "ROR A");
        assert_eq!(one(0x8A, 0, 0x77, 0).regs.a, 0x77, "TXA");
        assert_eq!(one(0xAA, 0x77, 0, 0).regs.x, 0x77, "TAX");
        assert_eq!(one(0xCA, 0, 0x01, 0).regs.x, 0x00, "DEX");
        assert!(one(0xCA, 0, 0x01, 0).flag(FLAG_Z), "DEX sets Z");
        assert_eq!(one(0x9A, 0, 0x40, 0).regs.sp, 0x40, "TXS");
        assert_eq!(one(0xBA, 0, 0, 0).regs.x, 0xFF, "TSX");
        assert_eq!(one(0xEA, 0x5A, 0, 0).regs.a, 0x5A, "NOP");

        assert_eq!(one(0xA7, 0, 0, 0).regs.a, 0x20, "LAX $82");
        assert_eq!(one(0xA7, 0, 0, 0).regs.x, 0x20);
        let mut mem = fresh();
        run_op(&mut mem, 0x87, 0x3C, 0x0F, 0, 0);
        assert_eq!(mem.ram[0x0082], 0x0C, "SAX $82 = A & X");
    }

    /// Only the twelve JAM opcodes halt execution.
    #[test]
    fn exactly_twelve_opcodes_jam() {
        let halts = |op: u8| {
            let mut mem = fresh();
            let cpu = run_op(&mut mem, op, 0x5A, 0x10, 0x20, 0);
            (cpu.jammed, cpu.brk_hit)
        };
        for op in JAMS {
            assert_eq!(halts(op), (true, false), "${op:02X} must jam");
        }
        assert_eq!(halts(0x00), (false, true), "BRK halts, but as a BRK");
        let jammed = (0u16..=0xFF).filter(|op| halts(*op as u8).0).count();
        assert_eq!(jammed, JAMS.len(), "exactly twelve opcodes jam");

        for op in SWALLOWED {
            assert_eq!(halts(op), (false, false), "${op:02X} must not halt");
        }
    }

    // LXA loads the immediate value into both A and X.
    #[test]
    fn lxa_loads_the_immediate_into_both_registers() {
        let cpu = imm(0xAB, 0x0F, 0x00, 0x00, 0);
        assert_eq!(cpu.regs.a, 0x0F, "this emulator: A = imm");
        assert_eq!(cpu.regs.x, 0x0F);
        assert_eq!(
            (0x00u8 | UNSTABLE_MAGIC) & 0x0F,
            0x0E,
            "LXA loads all set immediate bits"
        );
    }
}
