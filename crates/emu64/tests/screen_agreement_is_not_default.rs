use emu64::{ScreenVerdict, SCREEN_UNWRITTEN, System};

#[test]
fn chrout_does_not_render_into_screen_ram() {

    let mut sys = System::new();
    let code: [u8; 11] = [
        0xA9, 0x48,
        0x20, 0xD2, 0xFF,
        0xA9, 0x49,
        0x20, 0xD2, 0xFF,
        0x60,
    ];
    let base = 0xC000usize;
    sys.mem.ram[base..base + code.len()].copy_from_slice(&code);
    sys.start_at(base as u16);
    for _ in 0..200 {
        if sys.cpu.regs.pc == 0xC000 + code.len() as u16 - 1 {
            break;
        }
        if !sys.step() {
            break;
        }
    }

    assert_eq!(sys.output, b"HI", "the trap should have captured the PETSCII");
    let screen = sys.screen_bytes();
    assert!(
        screen.iter().all(|&b| b == SCREEN_UNWRITTEN),
        "CHROUT rendered into screen RAM; screen_same may now be meaningful \
         on its own and this test's premise needs revisiting"
    );
}

#[test]
fn two_blank_screens_are_not_evidence_of_agreement() {
    let a = [SCREEN_UNWRITTEN; 1000];
    let b = [SCREEN_UNWRITTEN; 1000];
    let v = ScreenVerdict::of(&a, &b);

    assert!(v.same(), "the raw byte comparison is equal, which is the trap");
    assert_eq!(v.live, 0, "nothing wrote the screen");
    assert_eq!(
        v.evidence(),
        "none",
        "untouched screens must report no comparison evidence"
    );
}

/// A screen only one side wrote is a disagreement, not a blank.
#[test]
fn one_sided_write_differs() {
    let mut a = [SCREEN_UNWRITTEN; 1000];
    let b = [SCREEN_UNWRITTEN; 1000];
    a[500] = 0x01;
    let v = ScreenVerdict::of(&a, &b);
    assert_eq!(v.live, 1);
    assert_eq!(v.diff, 1);
    assert!(!v.same());
    assert_eq!(v.evidence(), "differ");
}

#[test]
fn matching_written_screens_agree() {
    let mut a = [SCREEN_UNWRITTEN; 1000];
    let mut b = [SCREEN_UNWRITTEN; 1000];
    for i in 0..40 {
        a[i] = b"HELLO"[i % 5];
        b[i] = b"HELLO"[i % 5];
    }
    let v = ScreenVerdict::of(&a, &b);
    assert_eq!(v.live, 40);
    assert_eq!(v.diff, 0);
    assert!(v.same());
    assert_eq!(v.evidence(), "agree");
}

/// Both sides wrote, and differed.
#[test]
fn differing_written_screens_differ() {
    let mut a = [SCREEN_UNWRITTEN; 1000];
    let mut b = [SCREEN_UNWRITTEN; 1000];
    a[0] = b'A';
    a[1] = b'B';
    b[1] = b'B';
    b[2] = b'C';
    let v = ScreenVerdict::of(&a, &b);
    assert_eq!(v.live, 3, "union of cells either side wrote");
    assert_eq!(v.diff, 2, "cell 0 and cell 2");
    assert_eq!(v.evidence(), "differ");
}
