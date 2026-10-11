mod common;

use emu64::System;
use yabcompiler_core::{CompileOptions, CrunchError, compile_with_options, self_extracting_prg, tokenize_program};

const FIRST_RUN: [u8; 9] = [0x20, 0xBF, 0xE3, 0x20, 0x59, 0xA6, 0x4C, 0xAE, 0xA7];
const RUN_AGAIN: [u8; 6] = [0x20, 0x59, 0xA6, 0x4C, 0xAE, 0xA7];
const STUB: usize = 0x0334;
const BUDGET: u64 = 60_000_000;

struct Run {
    output: Vec<u8>,
    error: Option<u8>,
}

fn run_from(sys: &mut System, stub: &[u8]) -> Run {
    sys.mem.ram[STUB..STUB + stub.len()].copy_from_slice(stub);
    sys.output.clear();
    sys.ready_reached = false;
    sys.stop_key_down = false;
    sys.start_at(STUB as u16);
    for _ in 0..BUDGET {
        if !sys.step() {
            break;
        }
    }
    assert!(sys.ready_reached, "the program ends at READY");
    Run { output: sys.output.clone(), error: sys.error_code }
}

fn load_and_run_twice(prg: &[u8]) -> (System, Run, Run) {
    let mut sys = System::new();
    let (start, end) = sys.mem.load_prg(prg).expect("loads");
    assert_eq!(start, 0x0801);
    let vartab = end + 1;
    for zp in [0x2Dusize, 0x2F, 0x31] {
        sys.mem.ram[zp] = vartab as u8;
        sys.mem.ram[zp + 1] = (vartab >> 8) as u8;
    }
    sys.mem.ram[0x0281..0x0285].copy_from_slice(&[0x00, 0x08, 0x00, 0xA0]);
    let first = run_from(&mut sys, &FIRST_RUN);
    let second = run_from(&mut sys, &RUN_AGAIN);
    (sys, first, second)
}

fn unpack(prg: &[u8], entry: u16) -> System {
    let mut sys = System::new();
    let (_, end) = sys.mem.load_prg(prg).expect("loads");
    let vartab = end + 1;
    for zp in [0x2Dusize, 0x2F, 0x31] {
        sys.mem.ram[zp] = vartab as u8;
        sys.mem.ram[zp + 1] = (vartab >> 8) as u8;
    }
    sys.mem.ram[0x0281..0x0285].copy_from_slice(&[0x00, 0x08, 0x00, 0xA0]);
    sys.mem.ram[STUB..STUB + FIRST_RUN.len()].copy_from_slice(&FIRST_RUN);
    sys.stop_key_down = false;
    sys.start_at(STUB as u16);
    let mut unpacked = false;
    for _ in 0..BUDGET {
        unpacked |= sys.mem.ram[0x01] & 0x03 == 0;
        if unpacked && sys.cpu.regs.pc == entry {
            return sys;
        }
        assert!(sys.step(), "the program starts");
    }
    panic!("the unpacked program never reached ${entry:04X}");
}

fn sys_target(prg: &[u8]) -> u16 {
    let at = prg.iter().position(|&b| b == 0x9E).expect("a SYS stub") + 1;
    let digits: String = prg[at..].iter().take_while(|b| b.is_ascii_digit()).map(|&b| b as char).collect();
    digits.parse().expect("a decimal SYS target")
}

fn vartab(sys: &System) -> u32 {
    u32::from(sys.mem.ram[0x2D]) | u32::from(sys.mem.ram[0x2E]) << 8
}

fn same_as_plain(src: &str) -> (usize, usize) {
    let tokenized = tokenize_program(src).expect("tokenizes");
    let compiled = compile_with_options(&tokenized, CompileOptions::default()).expect("compiles");
    let plain = compiled.prg_bytes;
    let sfx = self_extracting_prg(&plain).expect("packs");
    assert_eq!(sfx.unpacked_end, 0x0801 + plain.len() as u32 - 2);

    let (_, want_first, want_second) = load_and_run_twice(&plain);
    let (sys, first, second) = load_and_run_twice(&sfx.prg);
    assert!(!want_first.output.is_empty());
    assert_eq!(common::petscii_to_string(&first.output), common::petscii_to_string(&want_first.output));
    assert_eq!(first.error, want_first.error);
    assert_eq!(common::petscii_to_string(&second.output), common::petscii_to_string(&want_second.output));
    assert_eq!(second.error, want_second.error);
    assert_eq!(vartab(&sys), sfx.unpacked_end, "VARTAB is still where LOAD puts it");

    let started = unpack(&sfx.prg, sys_target(&plain));
    assert_eq!(started.mem.ram[0x01] & 0x07, 0x07, "BASIC, KERNAL and I/O are banked in");
    assert_eq!(vartab(&started), sfx.unpacked_end, "VARTAB sits where LOAD puts it");
    assert_eq!(&started.mem.ram[0x0801..sfx.unpacked_end as usize], &plain[2..], "the image is intact");
    (plain.len(), sfx.prg.len())
}

#[test]
fn a_small_program() {
    same_as_plain(
        "10 FOR I=1 TO 10:PRINT I;:NEXT:PRINT\n\
         20 A$=\"HELLO\":B$=A$+\" WORLD\":PRINT B$;LEN(B$)\n\
         30 DIM A(20):FOR I=0 TO 20:A(I)=I*I:NEXT:PRINT A(20)/7\n",
    );
}

#[test]
fn a_program_with_text_and_data() {
    let mut src = String::from("5 S=0\n");
    for n in 0..60 {
        src.push_str(&format!(
            "{} PRINT \"ROOM {n}: A DARK PASSAGE LEADS NORTH AND EAST\";{n}*3\n",
            10 + n * 10
        ));
    }
    src.push_str("700 FOR I=1 TO 40:READ A,B$:S=S+A:NEXT:PRINT S;B$\n");
    for n in 0..20 {
        src.push_str(&format!("{} DATA {},ITEM{n},{},THING{n}\n", 800 + n, n * 7, n * 11));
    }
    same_as_plain(&src);
}

#[test]
fn a_tiny_program_runs_though_it_grows() {
    let (plain, packed) = same_as_plain("10 PRINT \"HI\"\n");
    assert!(packed > plain, "{plain} -> {packed}");
}

#[test]
fn a_program_that_reaches_past_a000() {
    let mut src = String::from("1 X=1:Y=2\n");
    for n in 0..550 {
        src.push_str(&format!("{} X=X+{n}:Y=(Y*3+{})AND1023:IF Y>{} THEN X=X-{}\n", n + 2, n % 7, n + 50, n % 5));
    }
    src.push_str("555 PRINT X;Y\n");
    let (plain, packed) = same_as_plain(&src);
    assert!(plain > 0xA000 - 0x0801, "the plain file runs past $A000 ({plain} bytes)");
    assert!(packed < plain / 2, "{plain} -> {packed}");
}

#[test]
fn an_image_past_cfff_unpacks_into_ram_under_io_and_kernal() {
    let text = tokenize_program("10 PRINT \"OK\"\n").expect("tokenizes");
    let mut prg = text.clone();
    let mut x: u32 = 1;
    while prg.len() - 2 < 0xF000 - 0x0801 {
        x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        let b = (x >> 24) as u8;
        for _ in 0..(1 + (x >> 20) % 4) {
            prg.push(b);
        }
    }
    prg.truncate(2 + 0xF000 - 0x0801);
    let sfx = self_extracting_prg(&prg).expect("packs");
    assert_eq!(sfx.unpacked_end, 0xF000);
    assert!(sfx.prg.len() + 0x0801 - 2 < 0xD000, "the packed file loads below $D000");

    let (sys, first, second) = load_and_run_twice(&sfx.prg);
    assert_eq!(common::petscii_to_string(&first.output), "OK\n");
    assert_eq!(common::petscii_to_string(&second.output), "OK\n");
    assert_eq!(&sys.mem.ram[0x0801..0xF000], &prg[2..], "RAM under BASIC, I/O and KERNAL holds the image");
    assert_eq!(vartab(&sys), 0xF000);
}

#[test]
fn a_program_that_does_not_start_at_0801_is_refused() {
    assert_eq!(
        self_extracting_prg(&[0x00, 0xC0, 0x60]).unwrap_err(),
        CrunchError::NotAtBasicStart(0xC000)
    );
}
