//! C64 BASIC ROM and KERNAL entry points used by the generated code.
//!
//! Each address has been verified against the C64 BASIC ROM behavior.
//!
//! Calling conventions (BASIC v2 ABI):
//!
//! * "FAC" = FAC1 = Floating Accumulator #1 at $61–$66 (unpacked).
//! * "ARG" = FAC2 = Argument register at $69–$6E (unpacked).
//! * Most "memory operand" routines take the pointer in `.A` (lo) `.Y` (hi).
//! * GIVAYF is the documented exception: integer in `.A` (HIGH byte) and
//!   `.Y` (LOW byte).
//! * MOVMF is also unique — destination pointer in `.X` (lo) `.Y` (hi).
//!
//! The FAC layout in zero page:
//!   $61 FACEXP   exponent (biased; $00 = value is 0.0)
//!   $62 FACHO    mantissa, high byte (implicit leading 1 in bit 7)
//!   $63 FACMOH   mantissa
//!   $64 FACMO    mantissa
//!   $65 FACLO    mantissa, low byte
//!   $66 FACSGN   sign byte ($00 positive, $FF negative)

// ---------- KERNAL ----------
/// Print one PETSCII char (in `.A`).
pub const CHROUT: u16 = 0xFFD2;
/// Read one character from keyboard buffer (non-blocking). Returns the
/// PETSCII code in `.A`, or 0 if no key is queued.
pub const GETIN: u16 = 0xFFE4;
/// KERNAL STOP — sets Z=1 if RUN/STOP key is currently pressed.
/// Used to escape long-running busy loops (e.g. the `PLAY 1`
/// blocking music wait).
pub const STOP: u16 = 0xFFE1;
/// PLOT — with carry set, returns cursor row in .X and column in .Y.
/// With carry clear, positions the cursor from .X/.Y.
pub const PLOT: u16 = 0xFFF0;
/// Read one character from the current input device, blocking until
/// one is available. Used by `INPUT` to gather a line from keyboard
/// (returns chars then $0D for RETURN).
pub const CHRIN: u16 = 0xFFCF;
/// Keyboard buffer index. Setting it to 0 drops any queued keystrokes
/// — important before `INPUT` so a leftover RETURN from `RUN` doesn't
/// short-circuit the read.
pub const KBD_BUF_NDX: u8 = 0xC6;

/// Atomic read of the 3-byte jiffy counter (TI variable). Returns:
///   `.Y` = high byte ($A0)
///   `.X` = mid byte  ($A1)
///   `.A` = low byte  ($A2)
/// Atomic in the sense that the IRQ that updates TI is suppressed for
/// the read, so we don't catch a half-updated value across a 60Hz tick.
pub const RDTIM: u16 = 0xFFDE;

// ---------- KERNAL: file/device I/O ----------
// Standard sequence to OPEN a logical file:
//   1. SETNAM with .A=name length, .X=name lo, .Y=name hi (use 0,0,0 if no name)
//   2. SETLFS with .A=file num, .X=device, .Y=secondary
//   3. OPEN
//
// To redirect output: SETLFS-equivalent already done; CHKOUT with .X=file
// num enables the channel. CLRCHN restores defaults. CHKIN sets input.

/// SETLFS — set logical file parameters (.A=file num, .X=device, .Y=secondary).
pub const SETLFS: u16 = 0xFFBA;
/// SETNAM — set filename (.A=length, .X=lo, .Y=hi).
pub const SETNAM: u16 = 0xFFBD;
/// OPEN — open a logical file using the parameters set by SETLFS+SETNAM.
pub const KERNAL_OPEN: u16 = 0xFFC0;
/// CLOSE — close a logical file (.A=file num).
pub const KERNAL_CLOSE: u16 = 0xFFC3;
/// CHKIN — set the current input channel to the named logical file (.X=file num).
pub const CHKIN: u16 = 0xFFC6;
/// CHKOUT — set the current output channel to the named logical file (.X=file num).
pub const CHKOUT: u16 = 0xFFC9;
/// CLRCHN — restore the default input/output channels (keyboard/screen).
pub const CLRCHN: u16 = 0xFFCC;

/// KERNAL LOAD ($FFD5). On entry: .A = 0 to load, 1 to verify;
/// .X/.Y = load address (only used when secondary != 1). On exit:
/// .X/.Y = end address + 1 if successful, status flags in the C bit.
/// Filename and file/device/secondary preset by SETNAM and SETLFS.
pub const KERNAL_LOAD: u16 = 0xFFD5;

/// KERNAL SAVE ($FFD8). On entry: .A = ZP byte holding start address
/// (LO/HI), .X/.Y = end address + 1. Filename and file/device preset
/// by SETNAM and SETLFS.
pub const KERNAL_SAVE: u16 = 0xFFD8;

/// KERNAL status byte — set by file-I/O routines to signal EOF, error,
/// timeout, etc. Read via the BASIC `ST` system variable.
pub const ST: u8 = 0x90;

// ---------- BASIC: console / strings ----------
/// Print null-terminated PETSCII string at `.A` (lo) `.Y` (hi).
pub const STROUT: u16 = 0xAB1E;

// ---------- BASIC: float ↔ integer ----------
/// Convert signed 16-bit (`.A` = HIGH byte, `.Y` = LOW byte) to FAC.
/// Note: opposite register order from the FADD-style routines below.
pub const GIVAYF: u16 = 0xB391;

// ---------- BASIC: FAC ↔ memory ----------
/// Memory→FAC. Source pointer in `.A` (lo) `.Y` (hi).
pub const MOVFM: u16 = 0xBBA2;
/// FAC→memory. Destination pointer in `.X` (lo) `.Y` (hi). $BBD7 is a
/// slightly cheaper internal entry but $BBD4 is the documented user
/// entry and works identically for our purposes.
pub const MOVMF: u16 = 0xBBD4;

// ---------- BASIC: float arithmetic taking memory operand at .A,.Y ----------
//
// All four take the operand pointer in `.A` (lo) `.Y` (hi). Operand
// Operand semantics:
//   FADD:  FAC = FAC + mem    (commutative)
//   FSUB:  FAC = mem - FAC    (codegen evaluates lhs to mem, rhs to FAC)
//   FMULT: FAC = FAC * mem    (commutative)
//   FDIV:  FAC = mem / FAC    (codegen evaluates lhs to mem, rhs to FAC)
//
// Each routine has multiple ROM entry points; the ones below load the
// operand from `.A,.Y` themselves. The companion entries at $B86A,
// $B853, $BA30 expect ARG to be pre-loaded and are not used here.
pub const FADD: u16 = 0xB867;
pub const FSUB: u16 = 0xB850;
pub const FMULT: u16 = 0xBA28;
pub const FDIV: u16 = 0xBB0F;

/// Power. Convention is opposite of the four basic ops — there's no
/// memory-operand entry in the ROM, so codegen explicitly loads
/// ARG = base via `FACARG`, then FAC = exponent via `MOVFM`, then
/// JSRs here. Result FAC = ARG^FAC.
pub const FPWRT: u16 = 0xBF7B;
/// Copy FAC1 to FAC2 (ARG). Used by exponentiation to set up the
/// base operand without going through memory.
pub const FACARG: u16 = 0xBC0C;

/// Compare FAC with memory at `.A` (lo) `.Y` (hi). Returns:
///   `.A` = $00 if FAC == mem
///   `.A` = $01 if FAC > mem
///   `.A` = $FF if FAC < mem
///
/// Currently unused — codegen does FSUB + sign-check on $61/$66 instead,
/// which is unambiguous and matches our existing infrastructure. Kept
/// here so a future "use FCOMP" optimisation pass has the address ready.
#[allow(dead_code)]
pub const FCOMP: u16 = 0xBC5B;

// ---------- BASIC: float printing ----------
/// Convert FAC to a null-terminated PETSCII string at $0100. The
/// caller is responsible for printing it (we wrap with STROUT in
/// `__PRINT_FAC`) or copying it elsewhere (`STR$` does this).
pub const FOUT: u16 = 0xBDDD;

/// Backwards-compatible alias for the convert routine. Older codegen
/// paths used the name `PRINT_FAC` even though the routine itself
/// only converts; new code should reference `FOUT`.
#[allow(dead_code)]
pub const PRINT_FAC: u16 = FOUT;

// ---------- Zero-page FAC fields ----------
/// FAC1 exponent. `LDA $61; BEQ ...` branches when FAC value is 0.0.
pub const FAC_EXP: u8 = 0x61;
/// FAC1 sign byte. Bit 7 = 1 means negative; toggle with `EOR #$FF`.
pub const FAC_SIGN: u8 = 0x66;

// ---------- Numeric one-arg functions (FAC → FAC) ----------
//
// Each takes the argument in FAC1 and leaves the result there. Codegen
// emits "eval arg → FAC; JSR <addr>".

pub const FN_ABS: u16 = 0xBC58;
pub const FN_INT: u16 = 0xBCCC; // BASIC INT() — truncate, value stays float
pub const FN_SGN: u16 = 0xBC39; // returns -1.0 / 0.0 / +1.0 in FAC
pub const FN_SQR: u16 = 0xBF71;
pub const FN_SIN: u16 = 0xE26B;
pub const FN_COS: u16 = 0xE264;
pub const FN_TAN: u16 = 0xE2B4;
pub const FN_ATN: u16 = 0xE30E;
pub const FN_LOG: u16 = 0xB9EA;
pub const FN_EXP: u16 = 0xBFED;
pub const FN_RND: u16 = 0xE097;

// ---------- FAC ↔ 16-bit integer for POKE/PEEK/SYS ----------
//
// GETADR (a.k.a. XFACWORD / FACWORD) converts FAC to an unsigned 16-bit
// address. The canonical place for the result is **LINNUM at $14–$15**
// — `$14` low, `$15` high. Some references claim it also leaves the
// value in `.A`/`.Y`, but that's unreliable for values above $7FFF;
// reading LINNUM works for the full 0..65535 range that POKE/PEEK
// addresses need.
pub const FACWORD: u16 = 0xB7F7;
/// Convert unsigned byte in `.Y` (not `.A`) to FAC.
pub const BYTEFAC: u16 = 0xB3A2;

/// Parse the string at `INDEX1` of length `.A`/`.Y` into FAC. Used to
/// implement `VAL(s$)` and the numeric variant of `INPUT`. Caller is
/// responsible for setting `VALTYPE` ($0D) to 0 (numeric mode).
pub const VAL_PARSE: u16 = 0xB7B5;
/// Zero-page pointer used by VAL and other parsing routines as the
/// source-of-text address.
pub const INDEX1_LO: u8 = 0x22;
pub const INDEX1_HI: u8 = 0x23;
/// VALTYPE selects parsing mode for several input routines: $00 means
/// "expect number", $80 means "expect string". We only ever set it to 0.
pub const VALTYPE: u8 = 0x0D;
/// LINNUM zero-page low byte. After GETADR, address low is here.
pub const LINNUM_LO: u8 = 0x14;
/// LINNUM zero-page high byte. After GETADR, address high is here.
pub const LINNUM_HI: u8 = 0x15;

/// Cursor column on the current screen line, maintained by CHROUT
/// (a.k.a. PNTR). Used by `TAB(n)` to compute how many spaces to emit,
/// and by `POS()` to report the current column.
pub const PNTR: u8 = 0xD3;

/// KERNAL's top-of-RAM pointer (MEMSIZ). Set at boot and respected by
/// BASIC and our heap allocator. `FRE()` returns MEMSIZ minus the
/// current heap top.
pub const MEMSIZ_LO: u8 = 0x37;
pub const MEMSIZ_HI: u8 = 0x38;

// ---------- BASIC error vectors ----------
//
// JMP here to raise a standard BASIC error. The ROM prints
// `?<NAME> ERROR IN <line>` and returns to READY. Useful when a
// language construct (e.g. `GET` with a non-digit) hits an input the
// BASIC interpreter would also reject.
pub const ERRSYN: u16 = 0xAF08;

/// BASIC warm-restart: prints READY and returns control to the
/// interpreter. Kept around as an emergency abort target; production
/// runtime errors should JMP to `BASIC_ERROR` instead so users get a
/// proper "?<MSG> ERROR IN <line>" message via the ROM's machinery.
pub const BASIC_WARM_START: u16 = 0xE37B;

/// BASIC ROM ERROR entry. JMP here with `.X` = error number (1-indexed
/// per the table at $A328) to print `?<MSG> ERROR` followed by
/// ` IN <line>` (when CURLIN is non-FFFF) and return to READY. The
/// routine takes care of stack cleanup, so callers don't need to
/// unwind nested JSRs themselves.
pub const BASIC_ERROR: u16 = 0xA437;
/// BASIC IERROR vector and its standard ROM target. `BASIC_ERROR`
/// eventually jumps through this vector; extension-style `ON ERROR` can
/// install a compiled dispatcher here to catch ROM/FAC-raised errors.
pub const IERROR_VEC: u16 = 0x0300;
pub const IERROR_ROM_HANDLER: u16 = 0xE38B;
/// BAD SUBSCRIPT — raised when an array index is out of bounds.
pub const ERR_BAD_SUBSCRIPT: u8 = 18;
/// ILLEGAL QUANTITY — raised when a value is outside its expected
/// range (e.g., bad TI$ format, negative SQR, log of zero).
pub const ERR_ILLEGAL_QUANTITY: u8 = 14;
/// STRING TOO LONG — raised when a string concat or operation
/// produces a result longer than 255 chars (BASIC's max).
pub const ERR_STRING_TOO_LONG: u8 = 23;
/// OUT OF MEMORY — raised when a runtime DIM can't allocate without
/// running into the live string heap.
pub const ERR_OUT_OF_MEMORY: u8 = 16;
/// UNDEF'D STATEMENT — used by computed GOTO when no compiled line
/// matches the runtime-selected target.
pub const ERR_UNDEF_STATEMENT: u8 = 17;
/// DIVISION BY ZERO — raised by `__DIV16` when the divisor is zero.
pub const ERR_DIVISION_BY_ZERO: u8 = 20;

/// QINT — convert FAC to signed 32-bit integer, deposited big-endian
/// in the FAC mantissa bytes ($62 MSB through $65 LSB). For values
/// fitting 24 bits, $62 is zero and $63/$64/$65 carry the result.
pub const QINT: u16 = 0xBC9B;

/// Current line number, used by BASIC's error printer to show
/// `IN <line>`. Set to $FFFF in direct mode (suppresses "IN ..."),
/// otherwise holds the active BASIC line. Compiled code writes this
/// at every line entry so any ROM-raised error (division by zero,
/// illegal quantity, overflow, etc.) reports the correct line — not
/// just our own bounds-check trap.
pub const CURLIN_LO: u8 = 0x39;
pub const CURLIN_HI: u8 = 0x3A;

// ---------- Compiler-owned zero page ----------
//
// $FB–$FE are documented as "free for any purpose" in the C64 memory map
// (no KERNAL or BASIC routine touches them). We split them between the
// DATA pointer and a scratch slot used by array indexing.
pub const DATA_PTR_LO: u8 = 0xFB;
pub const DATA_PTR_HI: u8 = 0xFC;
/// Scratch low byte for the runtime address of `A(i)`.
pub const ARRAY_ADDR_LO: u8 = 0xFD;
/// Scratch high byte for the runtime address of `A(i)`.
pub const ARRAY_ADDR_HI: u8 = 0xFE;

/// Two ZP pointer pairs reserved for string operations (compare and
/// concat src1/src2). $03–$06 are the canonical addresses of BASIC's
/// int↔float JMP vectors, but our compiled code never invokes those
/// vectors (we JSR the routines directly), so we can safely repurpose
/// them.
pub const STR_OP_LHS_LO: u8 = 0x03;
pub const STR_OP_LHS_HI: u8 = 0x04;
pub const STR_OP_RHS_LO: u8 = 0x05;
pub const STR_OP_RHS_HI: u8 = 0x06;

// String heap layout: the `__HEAP_BOTTOM` label is placed at the very
// end of the program's data section, and the heap grows upward from
// there to MEMSIZ ($37/$38) — the top-of-RAM pointer the KERNAL
// initialises at boot. Heap strings are length-prefixed; runtime
// chunks reserve two trailing owner bytes so the compacting GC can
// update the single persistent slot that owns a chunk.

// ---------- Reserved for future expansion ----------
//
// ROM entry points that codegen will likely need once it grows past
// the current subset. Parked here so we don't re-research them.
//
// Float math:
//   pub const MEMARG: u16   = 0xBA8C;  // load memory at .A,.Y → ARG (CONUPK)
//   pub const ARGFAC: u16   = 0xBBFC;  // load ARG from memory at .A,.Y
//   pub const FACARG: u16   = 0xBC0C;  // move FAC → ARG
//   pub const FACABS: u16   = 0xBC58;  // FAC = |FAC|
//   pub const SGNFAC: u16   = 0xBC2B;  // FAC = sign(FAC), returns ±1/0 in .A
//   pub const FACINT: u16   = 0xB1AA;  // FAC → signed 16-bit integer
//   pub const BYTEFAC: u16  = 0xB3A2;  // .A (unsigned byte) → FAC
//   pub const RNDFAC: u16   = 0xBC1B;  // round FAC
//
// Transcendentals (slow, called via JSR):
//   pub const FACSIN: u16   = 0xE26B;
//   pub const FACCOS: u16   = 0xE264;
//   pub const FACTAN: u16   = 0xE2B4;
//   pub const FACATN: u16   = 0xE30E;
//   pub const FACLOG: u16   = 0xB9EA;
//   pub const FACSQR: u16   = 0xBF71;
//   pub const FACEXPCALL: u16 = 0xBFED;
//   pub const FACPOW: u16   = 0xBF7B;
//   pub const FACRND: u16   = 0xE097;
//
// String + I/O:
//   pub const PRINTSTRS: u16 = 0xAB25;  // STROUT variant
//   pub const FACSTR: u16    = 0xBDDF;  // FAC → string buffer
//   pub const VALS: u16      = 0xB7B5;  // string → FAC
//   pub const INPUT: u16     = 0xA560;  // INPUT line
//
// Time / system:
//   pub const GETTI: u16   = 0xBE68;
//   pub const GETTIME: u16 = 0xAF7E;
//   pub const TI2FAC: u16  = 0xAF84;
//
// Errors (JMP here from generated code to raise standard BASIC errors):
//   pub const ERRALL: u16 = 0xA437;  // generic error
//   pub const ERRIQ: u16  = 0xB248;  // ?ILLEGAL QUANTITY
//   pub const ERREI: u16  = 0xACF4;  // ?EXTRA IGNORED
//   pub const ERRSYN: u16 = 0xAF08;  // ?SYNTAX ERROR
