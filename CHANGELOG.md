# Changelog

All notable changes to YABCompiler are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.9.9] - 2026-07-26

### Fixed

- `END` reached while a `GOSUB` is still open ends the program instead
  of returning to the caller and running on.
- `A(i) = FN f(...)` stores to `A(i)`. The function body's own array
  indexing no longer overwrites the destination address.
- A `DEF FN` body is emitted without inheriting the caller's
  ARRAY_ADDR / FAC / runtime-`DIM` state, so a repeated array
  reference such as `DEF FN F(X) = A(X)*2+A(X)` reads the right
  element.
- `PRINT` with three or more items ending on a non-literal
  (`PRINT "a";"b";STR$(N)`) emits its trailing newline again.
- A float comparison no longer reuses a stale FAC value, so chains
  like `IF X>PX OR Y>PY` with equal bounds evaluate both sides.
- Every `GOSUB` leaves a marker on the runtime FOR-stack when that
  stack is in use, matching what `RETURN` unwinds. A `GOSUB` to a
  subroutine without a `FOR` no longer drops the caller's loop frames.
- A `NEXT` inside `IF ... THEN` no longer closes the loop for the
  fall-through path, so a later bare `NEXT` still binds to that loop
  variable.

## [0.9.8] - 2026-06-07

### Fixed

- PETSCII string escapes accept the hex form (`{$A6}`) the detokenizer
  emits, so a `list` listing tokenizes back to the same bytes.

## [0.9.7] - 2026-06-07

### Fixed

- `GOTO` and `GOSUB` ignore a label appended to the line-number target
  (`GOSUB 500NAME`), matching BASIC v2.

## [0.9.6] - 2026-06-02

### Fixed

- `INPUT#` now reads all comma-separated values on a line, not just the first.

## [0.9.5] - 2026-05-25

### Fixed

- Float vars with a shadow-int slot also mirror FAC to the float slot
  on every write, so FAC ops that take the var as a memory operand
  always read a valid MFLPT.
- A FOR whose static NEXT lives inside an `IF` / `IFELSE` / `RCOMP`
  body joins the runtime FOR-stack when a later orphan NEXT can
  reach the same loop. The static exit also pops the runtime frame.
- Early helper-factoring of repeated 3-7 instruction windows is
  disabled. The late factoring pass keeps running with its tighter
  JSR-budget gate.

## [0.9.4] - 2026-05-24

### Fixed

- CR-only line endings (C64 native) no longer collapse the source to
  one line (issue #1).
- `NEXT` that can't be statically paired with a `FOR` now compiles
  through a runtime FOR-stack. The inline fast path is kept for
  programs that don't need it (issue #2).
- `TI$ = "..."` is no longer constant-folded, so the assignment
  reaches `__SET_TI` and `PRINT TI$` reads the live jiffy clock
  (issue #3).
- u8-FOR exit syncs the counter slot to `END+STEP`, so post-loop
  byte reads (`POKE S,N`) see the right value (issue #3).
- `FOR X=… TO … STEP 0` exits when the counter equals `END`,
  matching v2 (issue #4).
- Bare `NEXT` is resolved to its loop variable before liveness
  analysis, so `LET I=…` doesn't get dropped as dead (issue #4).

## [0.9.3] - 2026-05-24

### Added

- `--rem-hints=<dialect>` (CLI flag and matching GUI dropdown).
  Honours REM-based optimisation pragmas from two third-party
  compilers:
  - `basic64` (Data Becker): `REM@i=A,B,C` for integer scalars,
    `REM@b=...` for bytes, `REM@r=...` for reals (no-op).
  - `basic-boss`: `REM@ \BYTE A,B,C` and `REM@ \WORD A,B,C`, with
    an optional `=FAST` suffix for zero-page placement.

  Hinted vars skip the range proof that normally guards byte and
  integer promotion, so the user takes responsibility for keeping
  values in range.

### Performance

- Multi-dim integer-array indexing handles strides 11, 13, 14, 19,
  21 and 22 natively. Typical `DIM A(N, M)` shapes no longer fall
  back to the ROM float multiplier on each access.
- Numeric DATA pools get a binary layout (2 bytes per integer,
  5 per float). READ stores directly instead of going through the
  ROM VAL parser.
- `--profile=speed` keeps shared peephole helpers inlined instead
  of factoring them into `__JSR2_<n>` stubs. The default profile
  still factors, preferring compact code.
- Shadow-int promotion handles u16 alongside i16, so address vars
  like `UC=55296` get a 2-byte slot. Variables used as
  `POKE`/`PEEK`/`DPOKE` addresses are promoted regardless of
  read-count.
- Shadow-int promotion no longer double-counts writes inside `IF`,
  `IFELSE` and `RCOMP` bodies, which previously disqualified almost
  every conditionally assigned scalar.

### Changed

- REM hints are now honoured strictly. A hinted-int var stays
  integer even when the RHS is a float expression; the store
  converts via `FAC_TO_INT16`, matching Basic 64 and Basic-Boss.
- New `SplitMultiTypeVars` pass: a Float scalar with disjoint
  float-only and int-only lifetimes is split into independent vars
  so each cluster can be typed on its own. FOR-counter vars are
  skipped.

## [0.9.2] - 2026-05-23

### Added

- `SYS` with parameters in BASIC v2 mode (`SYS49152"text",8`). The
  trailing tokens are stashed inline and TXTPTR is pointed at them
  before the call, so ML routines can parse the arguments with the
  usual ROM helpers (CHRGOT, FRMEVL, FRESTR, CHKCOM, FRMNUM, GETADR).
- `--safe-sys-calls` (CLI flag and matching GUI checkbox, off by
  default). When on, every `SYS` saves and restores `$FB-$FE` plus
  the zero-page cells the codegen allocated, so ML targets that
  clobber zero page no longer corrupt program state.
- Dynamic start and step values in integer `FOR` loops. Loops like
  `FOR I=A TO B STEP S` with non-literal `A` or `S` now use the
  integer path instead of falling back to the floating-point loop.

### Changed

- Under `--extraram`, every `SYS` call is bracketed with
  `INC $01` / `DEC $01` so the target sees BASIC ROM banked in. SYS
  targets that call ROM helpers (FRMEVL, CHROUT, etc.) no longer
  crash under extraram.

### Fixed

- `INPUT A$` followed by RETURN now returns an empty string instead
  of reprompting with `??`, matching the interpreter.

## [0.9.1] - 2026-05-22

### Fixed

- `LOAD`, `SAVE` and `VERIFY` now accept a string expression as the
  filename, a string variable (`LOAD A$,8,1`), a parenthesised
  expression (`LOAD (F$),8`) or a concatenation (`LOAD N$+".C",8,1`),
  instead of only a literal quoted string. Programs that build the
  filename at runtime now compile and run correctly.

## [0.9.0] - 2026-05-21

First public release.

### Added

- Compiler from Commodore BASIC V2 to native 6502 machine code. Input
  is plain `.bas` source or a tokenized `.prg`; output is a runnable
  `.prg`.
- Support for a useful subset of TSB Neo, based on TSB by godot64.
  Some runtime routines were ported from TSB. See the README for
  details.
- Type inference that promotes integer-only variables to byte or word
  storage where safe.
- Extra RAM mode that banks BASIC ROM out to use `$A000-$BFFF` for
  code or data, decided automatically or forced.
- Auto-reserve that scans POKE/PEEK targets and keeps the assembler
  off those addresses.
- Custom start address for cartridge and expansion builds.
- Lenient parser mode that tolerates runtime-only BASIC typos in dead
  code.
- Command line front-end (`yabcompiler`) with `compile`, `list`,
  `dump` and `tokenize`.
- egui GUI (`yabcompiler-gui`) with a menu bar, theme selection that
  persists across runs, and an about dialog.
- `emu64`, a small in-process C64 emulator that runs compiled programs
  against the real BASIC and KERNAL ROM, used by the test suite.
- GitHub Actions release workflow that builds a Windows MSI installer,
  a portable ZIP, and Linux and macOS tarballs.

[0.9.9]: https://github.com/tommyo123/YABCompiler/releases/tag/v0.9.9
[0.9.8]: https://github.com/tommyo123/YABCompiler/releases/tag/v0.9.8
[0.9.7]: https://github.com/tommyo123/YABCompiler/releases/tag/v0.9.7
[0.9.6]: https://github.com/tommyo123/YABCompiler/releases/tag/v0.9.6
[0.9.5]: https://github.com/tommyo123/YABCompiler/releases/tag/v0.9.5
[0.9.4]: https://github.com/tommyo123/YABCompiler/releases/tag/v0.9.4
[0.9.3]: https://github.com/tommyo123/YABCompiler/releases/tag/v0.9.3
[0.9.2]: https://github.com/tommyo123/YABCompiler/releases/tag/v0.9.2
[0.9.1]: https://github.com/tommyo123/YABCompiler/releases/tag/v0.9.1
[0.9.0]: https://github.com/tommyo123/YABCompiler/releases/tag/v0.9.0
