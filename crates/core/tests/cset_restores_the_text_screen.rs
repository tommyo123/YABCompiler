//! Check that CSET 0 and 1 restore text mode and select the character set.

use yabcompiler_core::{CompileOptions, Dialect, compile_with_options, tokenize_program_with};

const MAX_INSNS: u64 = 20_000_000;

/// The VIC / CIA registers CSET writes, read back after the program ends.
#[derive(Debug, PartialEq)]
struct Video {
    dd00: u8,
    d011: u8,
    d016: u8,
    d018: u8,
}

/// Text screen at $0400, VIC bank 0, with the given $D018.
fn text(d018: u8) -> Video {
    Video {
        dd00: 0xC7,
        d011: 0x1B,
        d016: 0xC8,
        d018,
    }
}

/// The text screen in MEM mode: VIC bank 3, screen $CC00, chars $E000/$E800.
fn mem_text(d018: u8) -> Video {
    Video {
        dd00: 0xC4,
        d011: 0x1B,
        d016: 0xC8,
        d018,
    }
}

fn run(src: &str) -> Video {
    let prg = tokenize_program_with(src, Dialect::Tsb).expect("tokenizes");
    let image = compile_with_options(
        &prg,
        CompileOptions {
            dialect: Dialect::Tsb,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compiles: {e}\n{src}"))
    .prg_bytes;

    const SENTINEL: u16 = 0xFFFE;
    let mut sys = emu64::System::new();
    let entry = sys.load_prg(&image).expect("loads");
    // The KERNAL screen page a real boot leaves: text screen at $0400.
    sys.mem.ram[0x0288] = 0x04;
    sys.start_at(entry);
    let ret = SENTINEL.wrapping_sub(1);
    let sp = sys.cpu.regs.sp;
    sys.mem.ram[0x0100 | sp as usize] = (ret >> 8) as u8;
    sys.mem.ram[0x0100 | sp.wrapping_sub(1) as usize] = ret as u8;
    sys.cpu.regs.sp = sp.wrapping_sub(2);
    let mut done = false;
    for _ in 0..MAX_INSNS {
        if sys.cpu.regs.pc == SENTINEL {
            done = true;
            break;
        }
        if !sys.step() {
            break;
        }
    }
    assert!(done, "the program runs to its end:\n{src}");
    assert_eq!(sys.error_code, None, "no BASIC error:\n{src}");

    let io = &sys.mem.io;
    Video {
        dd00: io[0xD00],
        d011: io[0x011],
        d016: io[0x016],
        d018: io[0x018],
    }
}

#[test]
fn cset_1_after_the_graphics_screen_shows_the_text_screen() {
    // CSET 2 is the HIRES screen (bank 3, bitmap on); 216 turns MULTI on.
    assert_eq!(
        run("10 CSET 2:POKE 53270,216:CSET 1\n"),
        text(0x17),
        "CSET 1 is lower case on the text screen"
    );
    assert_eq!(
        run("10 CSET 2:POKE 53270,216:CSET 0\n"),
        text(0x15),
        "CSET 0 is upper case on the text screen"
    );
}

#[test]
fn cset_1_after_hires_shows_the_text_screen() {
    // Restore text mode after drawing.
    assert_eq!(
        run("10 HIRES 1,0:PLOT 10,10,1:CSET 1\n"),
        text(0x17)
    );
}

#[test]
fn a_computed_cset_mode_does_the_same() {
    // PEEK keeps the mode out of constant folding.
    assert_eq!(
        run("10 POKE 820,1:CSET 2:CSET PEEK(820)\n"),
        text(0x17)
    );
    assert_eq!(
        run("10 POKE 820,0:CSET 2:CSET PEEK(820)\n"),
        text(0x15)
    );
    // Anything from 2 up is the graphics screen.
    assert_eq!(
        run("10 POKE 820,7:CSET 1:CSET PEEK(820)\n"),
        Video {
            dd00: 0x94,
            d011: 0x3B,
            d016: 0xC8,
            d018: 0x0B,
        }
    );
}

#[test]
fn in_mem_mode_cset_uses_the_relocated_screen() {
    assert_eq!(
        run("10 MEM:CSET 2:CSET 1\n"),
        mem_text(0x17 ^ 0x2C),
        "MEM mode: bank 3 and the lower-case chars at $E800"
    );
    assert_eq!(
        run("10 MEM:CSET 2:CSET 0\n"),
        mem_text(0x15 ^ 0x2C),
        "MEM mode: bank 3 and the upper-case chars at $E000"
    );
}

#[test]
fn mem_mode_is_decided_when_cset_runs_not_when_it_compiles() {
    // Read $0288 at runtime: NRM has already left MEM mode.
    assert_eq!(
        run("10 MEM:NRM:CSET 2:CSET 1\n"),
        text(0x17),
        "after NRM, CSET 1 is the normal text screen"
    );
    // CSET before MEM must use the current screen layout.
    assert_eq!(
        run("10 CSET 2:CSET 0:IF PEEK(820)=99 THEN MEM\n"),
        text(0x15),
        "before MEM runs, CSET 0 is the normal text screen"
    );
}
