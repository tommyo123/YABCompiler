use emu64::System;
use yabcompiler_core::romfloat::{Fac, Machine, fin};

const STUB: usize = 0xC000;
const TEXT: usize = 0xC100;
const MEM: usize = 0xC200;

fn booted() -> System {
    let mut sys = System::new();
    sys.mem.ram[0x0281..0x0285].copy_from_slice(&[0x00, 0x08, 0x00, 0xA0]);
    run(&mut sys, &[0x20, 0xBF, 0xE3, 0x00]);
    sys
}

fn run(sys: &mut System, code: &[u8]) {
    sys.mem.ram[STUB..STUB + code.len()].copy_from_slice(code);
    sys.start_at(STUB as u16);
    for _ in 0..5_000_000 {
        if !sys.step() {
            break;
        }
    }
}

fn rom_fac(sys: &System) -> Fac {
    let z = &sys.mem.ram;
    Fac {
        exp: z[0x61],
        mant: [z[0x62], z[0x63], z[0x64], z[0x65]],
        sign: z[0x66],
        round: z[0x70],
    }
}

fn rom_fin(sys: &mut System, text: &[u8]) -> (Fac, Option<u8>) {
    sys.error_code = None;
    sys.mem.ram[TEXT..TEXT + text.len()].copy_from_slice(text);
    sys.mem.ram[TEXT + text.len()] = 0;
    run(
        sys,
        &[
            0xA9, TEXT as u8, 0x85, 0x7A, 0xA9, (TEXT >> 8) as u8, 0x85, 0x7B, // TXTPTR
            0x20, 0x79, 0x00, // JSR CHRGOT
            0x20, 0xF3, 0xBC, // JSR FIN
            0x00,
        ],
    );
    (rom_fac(sys), sys.error_code)
}

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) as u32
    }
    fn below(&mut self, n: u32) -> u32 {
        self.next() % n
    }
}

fn check_fin(sys: &mut System, text: &str) {
    let (want, err) = rom_fin(sys, text.as_bytes());
    let got = fin(text.as_bytes());
    match (err, got) {
        (Some(_), None) => {}
        (None, Some(got)) => assert_eq!(got, want, "FIN {text:?}"),
        (e, g) => panic!("FIN {text:?}: ROM error {e:?}, model {g:?}"),
    }
}

#[test]
fn fin_matches_the_rom_on_hand_picked_literals() {
    let mut sys = booted();
    for text in [
        "0", "1", "2", "9", "10", ".1", "0.1", ".10", "1.5", ".5", ".25", "3.14159", "3.14159265",
        "3.141592653589793", "2.718281828", ".3", ".7", "1.1", "1.3", "40", "255", "65535",
        "65536", "1E9", "1E10", "1E-5", "1.5E3", "1E38", "1E-38", "1.70141183E38", "123.456",
        "99999999", "999999999", "4294967295", "4294967296", "12345678901234567890", ".001",
        "1E+5", "2.5E-3", "7.77", "0.000001", "1.0000001", "100000", ".9999999999",
        "-1", "-.1", "1.2.3", "12E", "5E0",
    ] {
        check_fin(&mut sys, text);
    }
}

#[test]
fn fin_matches_the_rom_on_random_literals() {
    let mut sys = booted();
    let mut r = Lcg(12345);
    for _ in 0..4000 {
        let digits = 1 + r.below(14) as usize;
        let mut s = String::new();
        let dot = r.below(digits as u32 + 2) as usize;
        for i in 0..digits {
            if i == dot {
                s.push('.');
            }
            s.push(char::from(b'0' + r.below(10) as u8));
        }
        if r.below(4) == 0 {
            s.push('E');
            match r.below(3) {
                0 => s.push('-'),
                1 => s.push('+'),
                _ => {}
            }
            s.push_str(&r.below(40).to_string());
        }
        check_fin(&mut sys, &s);
    }
}

fn random_fac(r: &mut Lcg) -> Fac {
    Fac {
        exp: 0x60 + r.below(0x40) as u8,
        mant: [
            0x80 | r.below(0x80) as u8,
            r.below(256) as u8,
            r.below(256) as u8,
            if r.below(4) == 0 { 0 } else { r.below(256) as u8 },
        ],
        sign: if r.below(2) == 0 { 0 } else { 0xFF },
        round: match r.below(4) {
            0 => 0,
            1 => 0x80,
            _ => r.below(256) as u8,
        },
    }
}

fn random_packed(r: &mut Lcg) -> [u8; 5] {
    let mut m = Fac {
        round: 0,
        ..random_fac(r)
    }
    .packed_truncated();
    if r.below(3) == 0 {
        m[3] = 0;
        m[4] = 0;
    }
    m
}

fn rom_op(sys: &mut System, entry: u16, fac: Fac, mem: [u8; 5]) -> (Fac, Option<u8>) {
    sys.error_code = None;
    let z = &mut sys.mem.ram;
    z[0x61] = fac.exp;
    z[0x62..0x66].copy_from_slice(&fac.mant);
    z[0x66] = fac.sign;
    z[0x70] = fac.round;
    z[0x68] = 0;
    z[MEM..MEM + 5].copy_from_slice(&mem);
    run(
        sys,
        &[
            0xA9, MEM as u8, 0xA0, (MEM >> 8) as u8, // pointer in .A/.Y
            0x20, entry as u8, (entry >> 8) as u8, 0x00,
        ],
    );
    (rom_fac(sys), sys.error_code)
}

fn check_ops(op: &str, entry: u16, apply: fn(&mut Machine, [u8; 5]) -> Result<(), yabcompiler_core::romfloat::RomFloatError>) {
    let mut sys = booted();
    let mut r = Lcg(0xF00D + u64::from(entry));
    for i in 0..3000 {
        let fac = random_fac(&mut r);
        let mem = random_packed(&mut r);
        let (want, err) = rom_op(&mut sys, entry, fac, mem);
        let mut m = Machine::new();
        m.set_fac(fac);
        let got = apply(&mut m, mem);
        match (err, got) {
            (Some(_), Err(_)) => {}
            (None, Ok(())) => assert_eq!(
                m.fac(),
                want,
                "{op} #{i}: mem {mem:02X?} FAC {fac:02X?}"
            ),
            (e, g) => panic!("{op} #{i}: mem {mem:02X?} FAC {fac:02X?}: ROM error {e:?}, model {g:?}"),
        }
    }
}

#[test]
fn fadd_matches_the_rom() {
    check_ops("FADD", 0xB867, |m, x| m.fadd(x));
}

#[test]
fn fsub_matches_the_rom() {
    check_ops("FSUB", 0xB850, |m, x| m.fsub(x));
}

#[test]
fn fmult_matches_the_rom() {
    check_ops("FMULT", 0xBA28, |m, x| m.fmult(x));
}

#[test]
fn fdiv_matches_the_rom() {
    check_ops("FDIV", 0xBB0F, |m, x| m.fdiv(x));
}

#[test]
fn movmf_rounds_as_the_rom_does() {
    let mut sys = booted();
    let mut r = Lcg(77);
    for _ in 0..2000 {
        let fac = random_fac(&mut r);
        let z = &mut sys.mem.ram;
        z[0x61] = fac.exp;
        z[0x62..0x66].copy_from_slice(&fac.mant);
        z[0x66] = fac.sign;
        z[0x70] = fac.round;
        run(&mut sys, &[0xA2, MEM as u8, 0xA0, (MEM >> 8) as u8, 0x20, 0xD4, 0xBB, 0x00]);
        let want: [u8; 5] = sys.mem.ram[MEM..MEM + 5].try_into().unwrap();
        assert_eq!(fac.packed_rounded(), want, "MOVMF {fac:02X?}");
    }
}

#[test]
fn a_fac_state_survives_the_trip_through_f64() {
    let mut r = Lcg(5);
    for _ in 0..2000 {
        let f = random_fac(&mut r);
        let f = Fac { sign: f.sign & 0x80, ..f };
        assert_eq!(Fac::from_f64(f.to_f64()), Some(f));
    }
}


#[test]
fn fcomp_matches_the_rom() {
    let mut sys = booted();
    let mut r = Lcg(31337);
    for i in 0..4000 {
        let fac = random_fac(&mut r);
        let mem = if r.below(2) == 0 {
            let mut m = fac.packed_truncated();
            m[4] = m[4].wrapping_add(r.below(3) as u8).wrapping_sub(1);
            m
        } else {
            random_packed(&mut r)
        };
        let z = &mut sys.mem.ram;
        z[0x61] = fac.exp;
        z[0x62..0x66].copy_from_slice(&fac.mant);
        z[0x66] = fac.sign;
        z[0x70] = fac.round;
        z[MEM..MEM + 5].copy_from_slice(&mem);
        run(
            &mut sys,
            &[0xA9, MEM as u8, 0xA0, (MEM >> 8) as u8, 0x20, 0x5B, 0xBC, 0x8D, 0x00, 0xC3, 0x00],
        );
        let want = sys.mem.ram[0xC300] as i8;
        let mut m = Machine::new();
        m.set_fac(fac);
        assert_eq!(m.fcomp(mem), want, "FCOMP #{i}: mem {mem:02X?} FAC {fac:02X?}");
    }
}
