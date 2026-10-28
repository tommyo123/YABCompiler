//! BASIC floating point arithmetic for constant evaluation.

/// FAC exponent, mantissa, sign and rounding byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fac {
    pub exp: u8,
    pub mant: [u8; 4],
    pub sign: u8,
    pub round: u8,
}

impl Fac {
    pub const ZERO: Fac = Fac {
        exp: 0,
        mant: [0; 4],
        sign: 0,
        round: 0,
    };

    /// The exact value, all 40 bits of it.
    pub fn to_f64(self) -> f64 {
        if self.exp == 0 {
            return 0.0;
        }
        let m = u64::from_be_bytes([0, 0, 0, self.mant[0], self.mant[1], self.mant[2], self.mant[3], self.round]);
        let v = m as f64 * 2f64.powi(i32::from(self.exp) - 128 - 40);
        if self.sign & 0x80 != 0 { -v } else { v }
    }

    /// Exact FAC state for values within BASIC range and 40 significant bits.
    pub fn from_f64(v: f64) -> Option<Fac> {
        if v == 0.0 {
            return Some(Fac::ZERO);
        }
        if !v.is_finite() {
            return None;
        }
        let sign = if v < 0.0 { 0x80 } else { 0 };
        let a = v.abs();
        // a = m * 2^(e - 40) with 2^39 <= m < 2^40.
        let mut e = a.log2().floor() as i32 + 1;
        let mut scaled = a * 2f64.powi(40 - e);
        if scaled >= 2f64.powi(40) {
            e += 1;
            scaled = a * 2f64.powi(40 - e);
        } else if scaled < 2f64.powi(39) {
            e -= 1;
            scaled = a * 2f64.powi(40 - e);
        }
        if scaled.fract() != 0.0 {
            return None;
        }
        let exp = e + 128;
        if !(1..=255).contains(&exp) {
            return None;
        }
        let m = scaled as u64;
        let b = m.to_be_bytes();
        Some(Fac {
            exp: exp as u8,
            mant: [b[3], b[4], b[5], b[6]],
            sign,
            round: b[7],
        })
    }

    /// Round and pack FAC into five bytes.
    pub fn packed_rounded(self) -> [u8; 5] {
        let mut m = Machine::new();
        m.set_fac(self);
        m.movmf()
    }

    /// As `packed_rounded`, or None where the rounding raises ?OVERFLOW.
    pub fn packed_rounded_checked(self) -> Option<[u8; 5]> {
        let mut m = Machine::new();
        m.set_fac(self);
        m.round_checked().ok()?;
        Some(m.pack())
    }

    /// Pack FAC without its rounding byte.
    pub fn packed_truncated(self) -> [u8; 5] {
        [
            self.exp,
            (self.sign | 0x7F) & self.mant[0],
            self.mant[1],
            self.mant[2],
            self.mant[3],
        ]
    }
}

/// What can stop a ROM routine part way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RomFloatError {
    Overflow,
    DivisionByZero,
}

type R<T> = Result<T, RomFloatError>;

// Zero-page cells.
const RESHO: usize = 0x26; // Product or quotient in $26..$29.
const FADD_EXT: usize = 0x56; // the other operand's rounding byte in FADD
const FIN_DECCNT: usize = 0x5D; // digits after the point
const FIN_EXP: usize = 0x5E; // decimal exponent
const FIN_DOT: usize = 0x5F; // Bit 7 marks the first point and bit 6 the second.
const FIN_EXPSGN: usize = 0x60; // Bit 7 marks a negative exponent.
const FAC: usize = 0x61; // $61 exponent, $62..$65 mantissa, $66 sign
const FAC_SIGN: usize = 0x66;
const FIN_SIGN: usize = 0x67;
const BITS: usize = 0x68; // fill byte for byte-wise right shifts
const ARG: usize = 0x69; // $69 exponent, $6A..$6D mantissa, $6E sign
const ARG_SIGN: usize = 0x6E;
const ARISGN: usize = 0x6F;
const FACOV: usize = 0x70;

/// State used by the floating point routines.
pub struct Machine {
    z: [u8; 256],
    a: u8,
    x: u8,
    y: u8,
    c: bool,
    n: bool,
    zf: bool,
}

impl Default for Machine {
    fn default() -> Self {
        Self::new()
    }
}

impl Machine {
    pub fn new() -> Self {
        Machine {
            z: [0; 256],
            a: 0,
            x: 0,
            y: 0,
            c: false,
            n: false,
            zf: true,
        }
    }

    pub fn fac(&self) -> Fac {
        Fac {
            exp: self.z[FAC],
            mant: [self.z[FAC + 1], self.z[FAC + 2], self.z[FAC + 3], self.z[FAC + 4]],
            sign: self.z[FAC_SIGN],
            round: self.z[FACOV],
        }
    }

    pub fn set_fac(&mut self, f: Fac) {
        self.z[FAC] = f.exp;
        self.z[FAC + 1..FAC + 5].copy_from_slice(&f.mant);
        self.z[FAC_SIGN] = f.sign;
        self.z[FACOV] = f.round;
    }


    fn nz(&mut self, v: u8) -> u8 {
        self.n = v & 0x80 != 0;
        self.zf = v == 0;
        v
    }

    fn lda(&mut self, v: u8) {
        self.a = self.nz(v);
    }

    fn adc(&mut self, v: u8) {
        let r = u16::from(self.a) + u16::from(v) + u16::from(self.c);
        self.c = r > 0xFF;
        self.a = self.nz(r as u8);
    }

    fn sbc(&mut self, v: u8) {
        self.adc(!v);
    }

    /// Set carry when r is at least v.
    fn cmp(&mut self, r: u8, v: u8) {
        self.c = r >= v;
        self.nz(r.wrapping_sub(v));
    }

    fn asl_z(&mut self, addr: usize) {
        let v = self.z[addr];
        self.c = v & 0x80 != 0;
        self.z[addr] = self.nz(v << 1);
    }

    fn lsr_z(&mut self, addr: usize) {
        let v = self.z[addr];
        self.c = v & 1 != 0;
        self.z[addr] = self.nz(v >> 1);
    }

    fn rol_z(&mut self, addr: usize) {
        let v = self.z[addr];
        let r = (v << 1) | u8::from(self.c);
        self.c = v & 0x80 != 0;
        self.z[addr] = self.nz(r);
    }

    fn ror_z(&mut self, addr: usize) {
        let v = self.z[addr];
        let r = (v >> 1) | (u8::from(self.c) << 7);
        self.c = v & 1 != 0;
        self.z[addr] = self.nz(r);
    }

    fn ror_a(&mut self) {
        let v = self.a;
        let r = (v >> 1) | (u8::from(self.c) << 7);
        self.c = v & 1 != 0;
        self.a = self.nz(r);
    }

    fn lsr_a(&mut self) {
        let v = self.a;
        self.c = v & 1 != 0;
        self.a = self.nz(v >> 1);
    }

    fn inc_z(&mut self, addr: usize) {
        self.z[addr] = self.nz(self.z[addr].wrapping_add(1));
    }

    /// `zp,X` addressing.
    fn zx(&self, base: usize) -> usize {
        (base + usize::from(self.x)) & 0xFF
    }


    /// Unpack into ARG and set the arithmetic sign.
    pub fn conupk(&mut self, m: [u8; 5]) {
        self.z[ARG + 4] = m[4];
        self.z[ARG + 3] = m[3];
        self.z[ARG + 2] = m[2];
        self.z[ARG_SIGN] = m[1];
        self.z[ARISGN] = m[1] ^ self.z[FAC_SIGN];
        self.z[ARG + 1] = m[1] | 0x80;
        self.z[ARG] = m[0];
        self.lda(self.z[FAC]);
    }

    /// Unpack into FAC and clear the rounding byte.
    pub fn movfm(&mut self, m: [u8; 5]) {
        self.z[FAC + 4] = m[4];
        self.z[FAC + 3] = m[3];
        self.z[FAC + 2] = m[2];
        self.z[FAC_SIGN] = m[1];
        self.z[FAC + 1] = m[1] | 0x80;
        self.lda(m[0]);
        self.z[FAC] = m[0];
        self.z[FACOV] = 0;
    }

    /// Round FAC, pack it and clear the rounding byte.
    pub fn movmf(&mut self) -> [u8; 5] {
        self.round();
        self.pack()
    }

    /// Pack FAC as it stands and clear the rounding byte.
    fn pack(&mut self) -> [u8; 5] {
        let out = [
            self.z[FAC],
            (self.z[FAC_SIGN] | 0x7F) & self.z[FAC + 1],
            self.z[FAC + 2],
            self.z[FAC + 3],
            self.z[FAC + 4],
        ];
        self.z[FACOV] = 0;
        out
    }

    /// Copy ARG into FAC and clear the rounding byte.
    fn movfa(&mut self) {
        self.z[FAC_SIGN] = self.z[ARG_SIGN];
        for i in (1..=5).rev() {
            self.z[0x60 + i] = self.z[0x68 + i];
        }
        self.z[FACOV] = 0;
    }

    /// Round FAC, copy it into ARG and clear the rounding byte.
    fn movaf(&mut self) -> R<()> {
        self.round_checked()?;
        for i in (1..=6).rev() {
            self.z[0x68 + i] = self.z[0x60 + i];
        }
        self.a = self.z[FAC];
        self.x = 0;
        self.zf = true;
        self.n = false;
        self.z[FACOV] = 0;
        Ok(())
    }


    /// Shift the rounding byte and add its top bit to the mantissa.
    pub fn round(&mut self) {
        let _ = self.round_checked();
    }

    fn round_checked(&mut self) -> R<()> {
        if self.z[FAC] == 0 {
            return Ok(());
        }
        self.asl_z(FACOV);
        if !self.c {
            return Ok(());
        }
        self.inc_mantissa();
        if !self.zf {
            return Ok(());
        }
        self.renormalize_up()
    }

    /// Increment the mantissa and set Z when it wraps.
    fn inc_mantissa(&mut self) {
        for addr in [FAC + 4, FAC + 3, FAC + 2, FAC + 1] {
            self.inc_z(addr);
            if !self.zf {
                return;
            }
        }
    }

    /// Increment the exponent and shift the mantissa with carry.
    fn renormalize_up(&mut self) -> R<()> {
        self.inc_z(FAC);
        if self.zf {
            return Err(RomFloatError::Overflow);
        }
        for addr in [FAC + 1, FAC + 2, FAC + 3, FAC + 4, FACOV] {
            self.ror_z(addr);
        }
        Ok(())
    }

    /// Negate the mantissa and rounding byte, then flip the sign.
    fn negate_mantissa(&mut self) {
        self.z[FAC_SIGN] ^= 0xFF;
        for addr in [FAC + 1, FAC + 2, FAC + 3, FAC + 4, FACOV] {
            self.z[addr] ^= 0xFF;
        }
        self.inc_z(FACOV);
        if !self.zf {
            return;
        }
        self.inc_mantissa();
    }

    /// Negate on clear carry, then normalize.
    fn normalize_from_carry(&mut self) -> R<()> {
        if !self.c {
            self.negate_mantissa();
        }
        self.normalize()
    }

    /// Normalize the mantissa or set zero on underflow.
    fn normalize(&mut self) -> R<()> {
        self.y = 0;
        self.a = 0;
        self.c = false;
        loop {
            // B8DB
            let top = self.z[FAC + 1];
            self.x = top;
            self.nz(top);
            if top != 0 {
                break;
            }
            self.z[FAC + 1] = self.z[FAC + 2];
            self.z[FAC + 2] = self.z[FAC + 3];
            self.z[FAC + 3] = self.z[FAC + 4];
            self.z[FAC + 4] = self.z[FACOV];
            self.z[FACOV] = self.y;
            self.adc(8);
            self.cmp(self.a, 0x20);
            if self.zf {
                self.zero_fac();
                return Ok(());
            }
        }
        while !self.n {
            // B91D
            self.adc(1);
            self.asl_z(FACOV);
            self.rol_z(FAC + 4);
            self.rol_z(FAC + 3);
            self.rol_z(FAC + 2);
            self.rol_z(FAC + 1);
        }
        // B92B
        self.c = true;
        self.sbc(self.z[FAC]);
        if self.c {
            self.zero_fac();
            return Ok(());
        }
        self.a ^= 0xFF;
        self.adc(1);
        self.z[FAC] = self.a;
        // B936
        if self.c {
            return self.renormalize_up();
        }
        Ok(())
    }

    /// Set FAC to zero.
    fn zero_fac(&mut self) {
        self.a = 0;
        self.z[FAC] = 0;
        self.z[FAC_SIGN] = 0;
    }


    /// Shift the operand by whole bytes, then bits.
    fn shiftr(&mut self) {
        loop {
            // B999
            self.adc(8);
            if !(self.n || self.zf) {
                break;
            }
            self.shift_bytes();
        }
        // B99F
        self.sbc(8);
        self.y = self.a;
        self.a = self.z[FACOV];
        if !self.c {
            self.shift_bits_arith();
        }
        self.c = false;
    }

    /// Shift right by one byte.
    fn shift_bytes(&mut self) {
        let x = self.x as usize;
        self.z[FACOV] = self.z[(x + 4) & 0xFF];
        self.z[(x + 4) & 0xFF] = self.z[(x + 3) & 0xFF];
        self.z[(x + 3) & 0xFF] = self.z[(x + 2) & 0xFF];
        self.z[(x + 2) & 0xFF] = self.z[(x + 1) & 0xFF];
        self.z[(x + 1) & 0xFF] = self.z[BITS];
    }

    /// Shift right arithmetically until Y reaches zero.
    fn shift_bits_arith(&mut self) {
        loop {
            let top = self.zx(1);
            self.asl_z(top);
            if self.c {
                self.inc_z(top);
            }
            self.ror_z(top);
            self.ror_z(top);
            self.ror_lower();
            self.y = self.y.wrapping_add(1);
            if self.y == 0 {
                return;
            }
        }
    }

    /// Rotate the lower mantissa bytes and A right.
    fn ror_lower(&mut self) {
        let (b2, b3, b4) = (self.zx(2), self.zx(3), self.zx(4));
        self.ror_z(b2);
        self.ror_z(b3);
        self.ror_z(b4);
        self.ror_a();
    }


    /// Add memory to FAC.
    pub fn fadd(&mut self, m: [u8; 5]) -> R<()> {
        self.conupk(m);
        self.faddt()
    }

    /// Subtract FAC from memory.
    pub fn fsub(&mut self, m: [u8; 5]) -> R<()> {
        self.conupk(m);
        self.z[FAC_SIGN] ^= 0xFF;
        self.z[ARISGN] = self.z[FAC_SIGN] ^ self.z[ARG_SIGN];
        self.lda(self.z[FAC]);
        self.faddt()
    }

    /// Add ARG to FAC.
    fn faddt(&mut self) -> R<()> {
        if self.z[FAC] == 0 {
            self.movfa();
            return Ok(());
        }
        self.z[FADD_EXT] = self.z[FACOV];
        self.x = ARG as u8;
        self.lda(self.z[ARG]);
        self.faddt_from_arg_exp()
    }

    /// Add using the exponent in A.
    fn faddt_from_arg_exp(&mut self) -> R<()> {
        self.y = self.a;
        if self.a == 0 {
            return Ok(());
        }
        self.c = true;
        self.sbc(self.z[FAC]);
        if self.zf {
            return self.faddt_combine();
        }
        if self.c {
            // Use the larger ARG exponent and sign for FAC.
            self.z[FAC] = self.y;
            self.z[FAC_SIGN] = self.z[ARG_SIGN];
            self.a ^= 0xFF;
            self.adc(0);
            self.z[FADD_EXT] = 0;
            self.x = FAC as u8;
        } else {
            // Save the FAC rounding byte while shifting ARG.
            self.z[FACOV] = 0;
        }
        // B897
        self.cmp(self.a, 0xF9);
        if self.n {
            // B862
            self.shiftr();
            return self.faddt_combine();
        }
        self.y = self.a;
        self.a = self.z[FACOV];
        let top = self.zx(1);
        self.lsr_z(top);
        self.ror_lower();
        self.y = self.y.wrapping_add(1);
        if self.y != 0 {
            self.shift_bits_arith();
        }
        self.c = false;
        self.faddt_combine()
    }

    /// Add or subtract aligned mantissas according to their signs.
    fn faddt_combine(&mut self) -> R<()> {
        if self.z[ARISGN] & 0x80 == 0 {
            // B8FE
            self.adc(self.z[FADD_EXT]);
            self.z[FACOV] = self.a;
            for k in (1..=4).rev() {
                self.lda(self.z[FAC + k]);
                self.adc(self.z[ARG + k]);
                self.z[FAC + k] = self.a;
            }
            if self.c {
                return self.renormalize_up();
            }
            return Ok(());
        }
        let (keep, shifted) = if self.x as usize == ARG {
            (FAC, ARG)
        } else {
            (ARG, FAC)
        };
        self.c = true;
        self.a ^= 0xFF;
        self.adc(self.z[FADD_EXT]);
        self.z[FACOV] = self.a;
        for k in (1..=4).rev() {
            self.lda(self.z[keep + k]);
            self.sbc(self.z[shifted + k]);
            self.z[FAC + k] = self.a;
        }
        self.normalize_from_carry()
    }


    /// Multiply memory by FAC.
    pub fn fmult(&mut self, m: [u8; 5]) -> R<()> {
        self.conupk(m);
        self.fmultt()
    }

    /// Multiply ARG by FAC, including the rounding byte.
    fn fmultt(&mut self) -> R<()> {
        if self.z[FAC] == 0 {
            return Ok(());
        }
        if !self.muldiv()? {
            return Ok(());
        }
        for k in 0..4 {
            self.z[RESHO + k] = 0;
        }
        for addr in [FACOV, FAC + 4, FAC + 3, FAC + 2] {
            let b = self.z[addr];
            if b == 0 {
                // Shift the product by a byte before the remaining bits.
                self.lda(0);
                self.x = (RESHO - 1) as u8;
                self.shift_bytes();
                self.shiftr();
            } else {
                self.mltply(b);
            }
        }
        let top = self.z[FAC + 1];
        self.mltply(top);
        self.product_to_fac()
    }

    /// Add ARG for each set multiplier bit, shifting after each bit.
    fn mltply(&mut self, b: u8) {
        self.a = b;
        self.lsr_a();
        self.a |= 0x80;
        loop {
            self.y = self.a;
            if self.c {
                self.c = false;
                for k in (0..4).rev() {
                    self.lda(self.z[RESHO + k]);
                    self.adc(self.z[ARG + 1 + k]);
                    self.z[RESHO + k] = self.a;
                }
            }
            for addr in [RESHO, RESHO + 1, RESHO + 2, RESHO + 3, FACOV] {
                self.ror_z(addr);
            }
            self.a = self.y;
            self.lsr_a();
            if self.a == 0 {
                return;
            }
        }
    }

    /// Copy the product or quotient into FAC and normalize.
    fn product_to_fac(&mut self) -> R<()> {
        for k in 0..4 {
            self.z[FAC + 1 + k] = self.z[RESHO + k];
        }
        self.normalize()
    }

    /// Combine exponents and signs. Return false on zero or underflow.
    fn muldiv(&mut self) -> R<bool> {
        if self.z[ARG] == 0 {
            self.zero_fac();
            return Ok(false);
        }
        self.a = self.z[ARG];
        self.c = false;
        self.adc(self.z[FAC]);
        if self.c {
            if self.n {
                return Err(RomFloatError::Overflow);
            }
            self.c = false;
        } else if !self.n {
            self.zero_fac();
            return Ok(false);
        }
        self.adc(0x80);
        self.z[FAC] = self.a;
        if self.a == 0 {
            // A zero exponent clears the sign.
            self.z[FAC_SIGN] = 0;
            return Ok(true);
        }
        self.z[FAC_SIGN] = self.z[ARISGN];
        Ok(true)
    }


    /// Divide memory by FAC.
    pub fn fdiv(&mut self, m: [u8; 5]) -> R<()> {
        self.conupk(m);
        self.fdivt()
    }

    /// Divide ARG by rounded FAC.
    fn fdivt(&mut self) -> R<()> {
        if self.z[FAC] == 0 {
            return Err(RomFloatError::DivisionByZero);
        }
        self.round_checked()?;
        self.z[FAC] = 0u8.wrapping_sub(self.z[FAC]);
        if !self.muldiv()? {
            return Ok(());
        }
        self.inc_z(FAC);
        if self.zf {
            return Err(RomFloatError::Overflow);
        }
        // Divide mantissas into quotient bytes and two rounding bits.
        let mut out = 0usize; // quotient bytes stored
        let mut acc: u8 = 1;
        let mut extra = false;
        // Compare the mantissas.
        let mut cmp_ge = self.mantissa_ge();
        loop {
            // Shift the comparison carry into the quotient.
            let carry = cmp_ge;
            let bit7 = acc & 0x80 != 0;
            acc = (acc << 1) | u8::from(carry);
            if bit7 {
                // A full byte (or the two extra bits) is done.
                if !extra {
                    self.z[RESHO + out] = acc;
                    out += 1;
                    if out == 4 {
                        extra = true;
                        acc = 0x40;
                    } else {
                        acc = 1;
                    }
                } else {
                    self.z[FACOV] = acc << 6;
                    return self.product_to_fac();
                }
            }
            // Subtract FAC when ARG is at least FAC.
            if carry {
                let mut c = true;
                for k in (1..=4).rev() {
                    let r = i16::from(self.z[ARG + k]) - i16::from(self.z[FAC + k]) - i16::from(!c);
                    c = r >= 0;
                    self.z[ARG + k] = r as u8;
                }
            }
            // Shift the dividend left.
            let mut c = false;
            for k in (1..=4).rev() {
                let v = self.z[ARG + k];
                self.z[ARG + k] = (v << 1) | u8::from(c);
                c = v & 0x80 != 0;
            }
            if c {
                // A carry out makes the dividend larger than the divisor.
                cmp_ge = true;
            } else if self.z[ARG + 1] & 0x80 != 0 {
                cmp_ge = self.mantissa_ge();
            } else {
                cmp_ge = false;
            }
        }
    }

    /// Compare ARG and FAC mantissas from their highest bytes.
    fn mantissa_ge(&self) -> bool {
        for k in 1..=4 {
            let (a, f) = (self.z[ARG + k], self.z[FAC + k]);
            if a != f {
                return a > f;
            }
        }
        true
    }


    /// Compare FAC with memory, including the rounding bit without carrying.
    pub fn fcomp(&self, m: [u8; 5]) -> i8 {
        let z = &self.z;
        let fac_sign_result = |sign: u8| if sign & 0x80 != 0 { -1 } else { 1 };
        if m[0] == 0 {
            // SGN of FAC.
            return if z[FAC] == 0 { 0 } else { fac_sign_result(z[FAC_SIGN]) };
        }
        if (m[1] ^ z[FAC_SIGN]) & 0x80 != 0 {
            return fac_sign_result(z[FAC_SIGN]);
        }
        let carry;
        'differ: {
            if m[0] != z[FAC] {
                carry = m[0] >= z[FAC];
                break 'differ;
            }
            let mem = [m[1] | 0x80, m[2], m[3]];
            for k in 0..3 {
                if mem[k] != z[FAC + 1 + k] {
                    carry = mem[k] >= z[FAC + 1 + k];
                    break 'differ;
                }
            }
            // The last byte, less one more when the rounding byte's top bit is set.
            let borrow = i16::from(z[FACOV] > 0x7F);
            let r = i16::from(m[4]) - i16::from(z[FAC + 4]) - borrow;
            if r == 0 {
                return 0;
            }
            carry = r >= 0;
        }
        // Apply the sign to the comparison result.
        let a = if carry { z[FAC_SIGN] ^ 0xFF } else { z[FAC_SIGN] };
        if a & 0x80 != 0 { -1 } else { 1 }
    }


    /// Multiply FAC by ten through addition and shifts.
    fn mul10(&mut self) -> R<()> {
        self.movaf()?;
        self.x = self.a;
        if self.a == 0 {
            return Ok(());
        }
        self.c = false;
        self.adc(2);
        if self.c {
            return Err(RomFloatError::Overflow);
        }
        self.x = 0;
        self.z[ARISGN] = 0;
        self.faddt_from_arg_exp()?;
        self.inc_z(FAC);
        if self.zf {
            return Err(RomFloatError::Overflow);
        }
        Ok(())
    }

    /// Divide FAC by ten.
    fn div10(&mut self) -> R<()> {
        self.movaf()?;
        self.z[ARISGN] = 0;
        self.movfm([0x84, 0x20, 0x00, 0x00, 0x00]);
        self.fdivt()
    }

    /// Convert an unsigned byte to FAC.
    fn float_byte(&mut self, v: u8) -> R<()> {
        self.z[FAC + 1] = v;
        self.z[FAC + 2] = 0;
        // Carry is the inverse of the top bit before normalization.
        self.c = v & 0x80 == 0;
        self.z[FAC + 4] = 0;
        self.z[FAC + 3] = 0;
        self.z[FAC] = 0x88;
        self.z[FACOV] = 0;
        self.z[FAC_SIGN] = 0;
        self.normalize_from_carry()
    }

    /// Add a digit to FAC after rounding the saved operand.
    fn add_digit(&mut self, d: u8) -> R<()> {
        self.movaf()?;
        self.float_byte(d)?;
        self.z[ARISGN] = self.z[ARG_SIGN] ^ self.z[FAC_SIGN];
        self.faddt()
    }

    /// Negate FAC.
    pub fn negop(&mut self) {
        if self.z[FAC] != 0 {
            self.z[FAC_SIGN] ^= 0xFF;
        }
    }


    /// Parse literal text and return the number of bytes consumed.
    pub fn fin(&mut self, text: &[u8]) -> R<usize> {
        for k in FIN_DECCNT..=FIN_SIGN {
            self.z[k] = 0;
        }
        let mut i = 0usize;
        let at = |i: usize| text.get(i).copied().unwrap_or(0);
        // A leading sign.
        match at(i) {
            b'-' => {
                self.z[FIN_SIGN] = 0xFF;
                i += 1;
            }
            b'+' => i += 1,
            _ => {}
        }
        loop {
            let ch = at(i);
            if ch.is_ascii_digit() {
                if self.z[FIN_DOT] & 0x80 != 0 {
                    self.z[FIN_DECCNT] = self.z[FIN_DECCNT].wrapping_add(1);
                }
                self.mul10()?;
                self.add_digit(ch - b'0')?;
                i += 1;
                continue;
            }
            if ch == b'.' {
                let v = self.z[FIN_DOT];
                self.z[FIN_DOT] = (v >> 1) | 0x80;
                if self.z[FIN_DOT] & 0x40 != 0 {
                    break;
                }
                i += 1;
                continue;
            }
            if ch == b'E' {
                i += 1;
                match at(i) {
                    b'-' | 0xAB => {
                        self.z[FIN_EXPSGN] = 0x80;
                        i += 1;
                    }
                    b'+' | 0xAA => i += 1,
                    _ => {}
                }
                while at(i).is_ascii_digit() {
                    let d = at(i) - b'0';
                    let e = self.z[FIN_EXP];
                    if e >= 10 {
                        if self.z[FIN_EXPSGN] & 0x80 != 0 {
                            self.z[FIN_EXP] = 0x64;
                        } else {
                            return Err(RomFloatError::Overflow);
                        }
                    } else {
                        self.z[FIN_EXP] = e * 10 + d;
                    }
                    i += 1;
                }
                if self.z[FIN_EXPSGN] & 0x80 != 0 {
                    self.z[FIN_EXP] = 0u8.wrapping_sub(self.z[FIN_EXP]);
                }
            }
            break;
        }
        // Apply the decimal exponent after accounting for fractional digits.
        let scale = self.z[FIN_EXP].wrapping_sub(self.z[FIN_DECCNT]);
        self.z[FIN_EXP] = scale;
        if scale != 0 {
            if scale & 0x80 != 0 {
                let mut s = scale;
                while s != 0 {
                    self.div10()?;
                    s = s.wrapping_add(1);
                }
            } else {
                let mut s = scale;
                while s != 0 {
                    self.mul10()?;
                    s -= 1;
                }
            }
        }
        if self.z[FIN_SIGN] & 0x80 != 0 {
            self.negop();
        }
        Ok(i)
    }
}

/// Exact FAC state produced by parsing literal text.
pub fn fin(text: &[u8]) -> Option<Fac> {
    let mut m = Machine::new();
    m.fin(text).ok()?;
    Some(m.fac())
}

/// FIN of `text` as a value, when FIN consumes all of it.
pub fn fin_value(text: &[u8]) -> Option<f64> {
    let mut m = Machine::new();
    let used = m.fin(text).ok()?;
    (used == text.len()).then(|| m.fac().to_f64())
}

/// Recover exact constant state or load its rounded packed value.
pub fn fac_of(v: f64) -> Option<Fac> {
    Fac::from_f64(v).or_else(|| Fac::from_f64(crate::codegen::round_to_mflpt(v)))
}

/// Rounding byte after constant evaluation.
pub fn rounding_byte(v: f64) -> u8 {
    Fac::from_f64(v).map_or(0, |f| f.round)
}

/// A binary operator BASIC applies to two numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Add,
    Sub,
    Mul,
    Div,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// Evaluate with the left operand rounded and the right operand in FAC.
pub fn binop(op: Op, l: f64, r: f64) -> Option<f64> {
    let lf = fac_of(l)?;
    let rf = fac_of(r)?;
    let mem = lf.packed_rounded_checked()?;
    let mut m = Machine::new();
    m.set_fac(rf);
    let truth = |b: bool| if b { -1.0 } else { 0.0 };
    match op {
        Op::Add => m.fadd(mem).ok()?,
        Op::Sub => m.fsub(mem).ok()?,
        Op::Mul => m.fmult(mem).ok()?,
        Op::Div => m.fdiv(mem).ok()?,
        // FCOMP gives the sign of FAC - mem, that is of r - l.
        Op::Eq => return Some(truth(m.fcomp(mem) == 0)),
        Op::Ne => return Some(truth(m.fcomp(mem) != 0)),
        Op::Lt => return Some(truth(m.fcomp(mem) == 1)),
        Op::Le => return Some(truth(m.fcomp(mem) != -1)),
        Op::Gt => return Some(truth(m.fcomp(mem) == -1)),
        Op::Ge => return Some(truth(m.fcomp(mem) != 1)),
    }
    Some(m.fac().to_f64())
}

/// Round and convert to a signed integer, or return None on a range error.
pub fn int_store(v: f64) -> Option<f64> {
    let mut m = Machine::new();
    m.set_fac(fac_of(v)?);
    m.round_checked().ok()?;
    let f = m.fac().to_f64().floor();
    (-32768.0..=32767.0).contains(&f).then_some(f)
}

/// `v` rounded as FRMEVL rounds a pushed operand (ROUND, rounding byte gone).
pub fn rounded(v: f64) -> Option<f64> {
    let f = fac_of(v)?;
    let mut m = Machine::new();
    m.movfm(f.packed_rounded_checked()?);
    Some(m.fac().to_f64())
}
