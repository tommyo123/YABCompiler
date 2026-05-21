# emu64

Minimal in-process Commodore 64 emulator. Provides a 6510 CPU with
banked C64 memory, embedded BASIC / KERNAL / CHARGEN ROMs, and PRG
autostart by parsing the BASIC `SYS` stub.

## Layout

```
emu64
├── memory.rs   64 KB RAM with port-$01 banking (BASIC / KERNAL /
│               CHARGEN / I/O)
├── cpu.rs      6510 CPU: documented opcodes, the common illegal
│               opcodes (LAX, SAX, DCP, ISC, SLO, RLA, SRE, RRA),
│               and decimal-mode ADC / SBC
├── system.rs   Glue: ROM loading, PRG loading with `SYS` target
│               detection, KERNAL traps, screen-RAM access
├── lib.rs      `run_until_screen_pattern` + `run_prg_to_end`
└── main.rs     CLI: `emu64 run <prg> <expected-hex> [...]`
```

The ROM images live under `roms/` and are embedded at build time.

## Autostart model

The KERNAL / BASIC boot is skipped. Instead:

1. The PRG bytes are loaded at the address from the file header.
2. If the load address is `$0801`, the BASIC stub is scanned for the
   `SYS` token (`$9E`) and the decimal address that follows it is
   used as the entry point.
3. Conio zeropage state (`SCREEN_PTR`, `CRAM_PTR`, `CURS_X`,
   `CURS_Y`, `RVS`, `CHARCOLOR`) is pre-initialized to the values a
   real KERNAL boot would leave behind.
4. The BASIC zero page a cold start would set up is initialized so the
   ROM floating-point and string routines run correctly without a real
   boot: the program/variable/array pointers (`TXTTAB`, `VARTAB`,
   `ARYTAB`, `STREND`, `FRETOP`, `MEMSIZ`), the temp-descriptor stack
   pointer `TEMPPT` (`$16`), and the 24-byte `CHRGET` routine copied to
   `$0073` (needed by `VAL` and numeric `READ`/`DATA`).
5. A handful of KERNAL entry points are trapped:
   - `$FFD2` CHROUT — captures `A` into the output buffer, clears carry
     (success) and returns. Clearing carry matters: KERNAL callers
     `BCS` to an error handler otherwise.
   - `$FFF0` PLOT — sets or reads the cursor position and recomputes
     the screen and colour pointers.
   - `$FFE4` GETIN, `$FFCF` CHRIN, `$FFE1` STOP — return `A = 0`, carry
     clear.

`run_prg_to_end` runs a PRG from its `SYS` entry to its final `RTS`
(detected via a sentinel return frame), capturing everything it PRINTs.
It is the basis of the compiler's fast in-process corpus regression
test (`crates/core/tests/emu_corpus.rs`).

## CLI

```
emu64 run <file.prg> <expected-hex> [options]
```

- `<expected-hex>` is the byte sequence to find anywhere in the
  40×25 screen RAM at `$0400`, e.g. `313233340f0b21`.
- `--max-insns N` instruction budget (default 50 000 000).
- `--dump-screen` print the final 25-row screen on exit.
- `--dump-output` print captured CHROUT bytes as PETSCII hex.

Exit code is `0` on a screen-pattern match, `1` on timeout or error.

## Limitations

- One nominal cycle per instruction — not cycle-accurate.
- No VIC-II, SID, or CIA emulation. Screen RAM is just memory that
  programs write to and the harness reads.
- Programs that rely on full BASIC state (BASIC pointers, file I/O,
  KERNAL screen editor input) will not work.
