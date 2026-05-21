//! C64 memory map with ROM/RAM banking controlled by the 6510 processor
//! port at $0001 (LORAM/HIRAM/CHAREN bits).

pub struct C64Mem {
    pub ram: [u8; 0x10000],
    pub basic_rom: [u8; 0x2000],   // $A000-$BFFF
    pub kernal_rom: [u8; 0x2000],  // $E000-$FFFF
    pub chargen_rom: [u8; 0x1000], // $D000-$DFFF when CHAREN=0
    pub io: [u8; 0x1000],          // $D000-$DFFF when CHAREN=1
    pub start_address: u16,
    pub end_address: u16,
}

impl C64Mem {
    pub fn new() -> Self {
        Self {
            ram: [0; 0x10000],
            basic_rom: [0; 0x2000],
            kernal_rom: [0; 0x2000],
            chargen_rom: [0; 0x1000],
            io: [0; 0x1000],
            start_address: 0,
            end_address: 0,
        }
    }

    fn port01(&self) -> u8 {
        self.ram[0x0001]
    }

    fn basic_visible(ctrl: u8) -> bool {
        (ctrl & 0x01) != 0 && (ctrl & 0x02) != 0
    }
    fn kernal_visible(ctrl: u8) -> bool {
        (ctrl & 0x02) != 0 && ((ctrl & 0x01) != 0 || (ctrl & 0x04) != 0)
    }
    fn io_visible(ctrl: u8) -> bool {
        (ctrl & 0x04) != 0 && ((ctrl & 0x01) != 0 || (ctrl & 0x02) != 0)
    }
    fn char_visible(ctrl: u8) -> bool {
        (ctrl & 0x04) == 0 && (ctrl & 0x02) != 0
    }

    pub fn read_byte(&self, addr: u16) -> u8 {
        let a = addr as usize;
        let ctrl = self.port01();

        if a < 0xA000 || (0xC000..0xD000).contains(&a) {
            return self.ram[a];
        }

        match a {
            0xA000..=0xBFFF => {
                if Self::basic_visible(ctrl) {
                    self.basic_rom[a - 0xA000]
                } else {
                    self.ram[a]
                }
            }
            0xD000..=0xDFFF => {
                if Self::io_visible(ctrl) {
                    self.io[a - 0xD000]
                } else if Self::char_visible(ctrl) {
                    self.chargen_rom[a - 0xD000]
                } else {
                    self.ram[a]
                }
            }
            0xE000..=0xFFFF => {
                if Self::kernal_visible(ctrl) {
                    self.kernal_rom[a - 0xE000]
                } else {
                    self.ram[a]
                }
            }
            _ => self.ram[a],
        }
    }

    pub fn read_word(&self, addr: u16) -> u16 {
        let lo = self.read_byte(addr) as u16;
        let hi = self.read_byte(addr.wrapping_add(1)) as u16;
        (hi << 8) | lo
    }

    pub fn write_byte(&mut self, addr: u16, value: u8) {
        let a = addr as usize;
        let ctrl = self.port01();

        // Writes hit the I/O area when it is mapped, otherwise the
        // underlying RAM cell.
        if (0xD000..=0xDFFF).contains(&a) && Self::io_visible(ctrl) {
            self.io[a - 0xD000] = value;
            return;
        }
        self.ram[a] = value;
    }

    pub fn write_word(&mut self, addr: u16, value: u16) {
        self.write_byte(addr, value as u8);
        self.write_byte(addr.wrapping_add(1), (value >> 8) as u8);
    }

    /// Load a PRG image. The first two bytes are the little-endian
    /// load address; the remaining bytes are placed contiguously from
    /// there. Returns `(start, end)`.
    pub fn load_prg(&mut self, bytes: &[u8]) -> Result<(u16, u16), String> {
        if bytes.len() < 3 {
            return Err("PRG too small (need >= 3 bytes)".into());
        }
        let start = u16::from_le_bytes([bytes[0], bytes[1]]);
        let data = &bytes[2..];
        let end = start
            .checked_add((data.len() - 1) as u16)
            .ok_or_else(|| "PRG overflows 64K".to_string())?;
        for (i, &b) in data.iter().enumerate() {
            self.ram[(start as usize) + i] = b;
        }
        self.start_address = start;
        self.end_address = end;
        Ok((start, end))
    }
}

impl Default for C64Mem {
    fn default() -> Self {
        Self::new()
    }
}
