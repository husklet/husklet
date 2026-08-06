pub const MAX_INSTRUCTION_BYTES: usize = 15;

mod input;
mod model;

use input::Cursor;
pub use input::{Error as DecodeError, FetchError};
pub use model::*;

pub struct Decoder;
pub type X86Decoder = Decoder;

pub trait InstructionFetch {
    fn fetch(&self, address: u64, destination: &mut [u8]) -> Result<(), FetchError>;
}

impl X86Decoder {
    pub fn decode_at(fetch: &dyn InstructionFetch, address: u64) -> Result<DecodedInstruction, DecodeError> {
        let mut bytes = [0_u8; MAX_INSTRUCTION_BYTES];
        let available = (4096 - (address as usize & 4095)).min(MAX_INSTRUCTION_BYTES);
        fetch
            .fetch(address, &mut bytes[..available])
            .map_err(|_| DecodeError::Fetch)?;
        match Self::decode(&bytes[..available]) {
            Ok(decoded) => Ok(decoded),
            Err(DecodeError::Truncated) if available < MAX_INSTRUCTION_BYTES => {
                fetch.fetch(address, &mut bytes).map_err(|_| DecodeError::Fetch)?;
                Self::decode(&bytes)
            }
            result => result,
        }
    }

    pub fn decode(bytes: &[u8]) -> Result<DecodedInstruction, DecodeError> {
        if bytes.len() > MAX_INSTRUCTION_BYTES {
            return Err(DecodeError::TooLong);
        }
        let mut cursor = Cursor::new(bytes);
        let mut prefixes = LegacyPrefixes::default();
        loop {
            match cursor.peek()? {
                0x66 => prefixes.operand_16 = true,
                0x67 => prefixes.address_32 = true,
                0xf0 => prefixes.lock = true,
                0xf2 => {
                    prefixes.rep = false;
                    prefixes.repne = true;
                }
                0xf3 => {
                    prefixes.rep = true;
                    prefixes.repne = false;
                }
                0x64 => prefixes.segment = Some(Segment::Fs),
                0x65 => prefixes.segment = Some(Segment::Gs),
                0x2e | 0x36 | 0x3e | 0x26 => {}
                _ => break,
            }
            cursor.byte()?;
        }
        let lead = cursor.peek()?;
        let (encoding, opcode) = match lead {
            0xc5 => {
                cursor.byte()?;
                let p = cursor.byte()?;
                let rex = Rex {
                    r: p & 0x80 == 0,
                    ..Rex::default()
                };
                (
                    Encoding::Vex {
                        map: 1,
                        pp: p & 3,
                        length: (p >> 2) & 1,
                        w: false,
                        source: !(p >> 3) & 15,
                        rex,
                    },
                    cursor.byte()?,
                )
            }
            0xc4 => {
                cursor.byte()?;
                let a = cursor.byte()?;
                let b = cursor.byte()?;
                let rex = Rex {
                    r: a & 0x80 == 0,
                    x: a & 0x40 == 0,
                    b: a & 0x20 == 0,
                    w: false,
                };
                (
                    Encoding::Vex {
                        map: a & 31,
                        pp: b & 3,
                        length: (b >> 2) & 1,
                        w: b & 0x80 != 0,
                        source: !(b >> 3) & 15,
                        rex,
                    },
                    cursor.byte()?,
                )
            }
            0x62 => {
                cursor.byte()?;
                let a = cursor.byte()?;
                let b = cursor.byte()?;
                let c = cursor.byte()?;
                let rex = Rex {
                    r: a & 0x80 == 0,
                    x: a & 0x40 == 0,
                    b: a & 0x20 == 0,
                    w: false,
                };
                let source = (!(b >> 3) & 15) | if c & 8 == 0 { 16 } else { 0 };
                (
                    Encoding::Evex {
                        map: a & 3,
                        pp: b & 3,
                        length: (c >> 5) & 3,
                        w: b & 0x80 != 0,
                        source,
                        rex,
                        mask: c & 7,
                        zero: c & 0x80 != 0,
                        broadcast: c & 0x10 != 0,
                    },
                    cursor.byte()?,
                )
            }
            _ => Self::legacy_encoding(&mut cursor, lead)?,
        };
        let (rex, map, vector) = match encoding {
            Encoding::Legacy { rex, map } => (rex.unwrap_or_default(), map, false),
            Encoding::Vex { rex, map, .. } | Encoding::Evex { rex, map, .. } => (rex, map, true),
        };
        let has_modrm = if vector {
            opcode != 0x77
        } else {
            Self::has_modrm(map, opcode)
        };
        let mut modrm = None;
        let mut raw_mod = None;
        let mut raw_reg = None;
        let mut raw_rm = None;
        let mut register = None;
        let mut register_operand = None;
        let mut address = None;
        if has_modrm {
            let byte = cursor.byte()?;
            modrm = Some(byte);
            raw_mod = Some(byte >> 6);
            raw_reg = Some((byte >> 3) & 7);
            raw_rm = Some(byte & 7);
            register = Some(((byte >> 3) & 7) | (u8::from(rex.r) << 3));
            let mode = byte >> 6;
            let rm = byte & 7;
            if mode == 3 {
                register_operand = Some(rm | (u8::from(rex.b) << 3));
            } else {
                address = Some(Self::address(
                    &mut cursor,
                    prefixes,
                    rex,
                    map,
                    opcode,
                    mode,
                    rm,
                    vector,
                )?);
            }
        }
        let operand_bytes = if rex.w {
            8
        } else if prefixes.operand_16 {
            2
        } else {
            4
        };
        let immediate_bytes = Self::immediate_bytes(map, opcode, vector, modrm, operand_bytes, prefixes.address_32);
        let immediate = if immediate_bytes == 0 {
            None
        } else {
            Some((cursor.signed(immediate_bytes)?, immediate_bytes))
        };
        Ok(DecodedInstruction {
            length: cursor.position() as u8,
            prefixes,
            encoding,
            opcode,
            modrm,
            raw_mod,
            raw_reg,
            raw_rm,
            register,
            register_operand,
            address,
            immediate,
        })
    }

    fn legacy_encoding(cursor: &mut Cursor<'_>, lead: u8) -> Result<(Encoding, u8), DecodeError> {
        let rex = if lead & 0xf0 == 0x40 {
            let value = cursor.byte()?;
            Some(Rex {
                w: value & 8 != 0,
                r: value & 4 != 0,
                x: value & 2 != 0,
                b: value & 1 != 0,
            })
        } else {
            None
        };
        let mut opcode = cursor.byte()?;
        let mut map = 0;
        if opcode != 0x0f {
            return Ok((Encoding::Legacy { rex, map }, opcode));
        }
        map = 1;
        opcode = cursor.byte()?;
        if opcode == 0x38 || opcode == 0x3a {
            map = if opcode == 0x38 { 2 } else { 3 };
            opcode = cursor.byte()?;
        }
        Ok((Encoding::Legacy { rex, map }, opcode))
    }

    fn has_modrm(map: u8, op: u8) -> bool {
        if map >= 2 {
            return true;
        }
        if map == 1 {
            if matches!(op, 0x05 | 0x0b | 0xa2 | 0x31 | 0x77) || (0xc8..=0xcf).contains(&op) || op & 0xf0 == 0x80 {
                return false;
            }
            return true;
        }
        if (0x50..=0x5f).contains(&op)
            || (0x70..=0x7f).contains(&op)
            || matches!(
                op,
                0xe8 | 0xe9
                    | 0xeb
                    | 0xe3
                    | 0xe0
                    | 0xe1
                    | 0xe2
                    | 0xc3
                    | 0xc2
                    | 0xc9
                    | 0xcf
                    | 0x90
                    | 0xf4
                    | 0x99
                    | 0x98
                    | 0x9b
                    | 0x9c
                    | 0x9d
                    | 0x9e
                    | 0x9f
                    | 0xfc
                    | 0xfd
                    | 0xcc
                    | 0xf5
                    | 0xf8
                    | 0xf9
                    | 0x68
                    | 0x6a
                    | 0xf1
                    | 0xd7
            )
            || (0x91..=0x97).contains(&op)
            || (0xa0..=0xbf).contains(&op)
            || (op < 0x40 && matches!(op & 7, 4 | 5))
        {
            return false;
        }
        true
    }

    fn immediate_bytes(map: u8, op: u8, vector: bool, modrm: Option<u8>, os: u8, address_32: bool) -> u8 {
        if map == 3 {
            return 1;
        }
        if vector {
            return u8::from(map == 1 && matches!(op, 0x70..=0x73 | 0xc2 | 0xc4..=0xc6));
        }
        if map == 2 {
            return 0;
        }
        if map == 1 {
            return if op & 0xf0 == 0x80 {
                4
            } else { u8::from(matches!(op, 0xba | 0xa4 | 0xac | 0x70..=0x73 | 0xc2 | 0xc4..=0xc6)) };
        }
        if op == 0xc2 {
            return 2;
        }
        if matches!(op, 0x70..=0x7f | 0xeb | 0xe3 | 0xe0..=0xe2 | 0xb0..=0xb7 | 0x6a | 0x80 | 0x83 | 0xc6 | 0xc0 | 0xc1 | 0x6b | 0xa8)
        {
            return 1;
        }
        if matches!(op, 0xe8 | 0xe9) {
            return 4;
        }
        if (0xa0..=0xa3).contains(&op) {
            return if address_32 { 4 } else { 8 };
        }
        if (0xb8..=0xbf).contains(&op) {
            return if os == 8 { 8 } else { os };
        }
        if op < 0x40 && op & 7 == 4 {
            return 1;
        }
        if op < 0x40 && op & 7 == 5 || matches!(op, 0xa9 | 0x68 | 0x81 | 0xc7 | 0x69) {
            return if os == 2 { 2 } else { 4 };
        }
        if matches!(op, 0xf6 | 0xf7) && modrm.is_some_and(|m| ((m >> 3) & 7) <= 1) {
            return if op == 0xf6 {
                1
            } else if os == 2 {
                2
            } else {
                4
            };
        }
        0
    }

    fn address(
        cursor: &mut Cursor<'_>,
        prefixes: LegacyPrefixes,
        rex: Rex,
        map: u8,
        opcode: u8,
        mode: u8,
        rm: u8,
        vector: bool,
    ) -> Result<EffectiveAddress, DecodeError> {
        let mut value = EffectiveAddress {
            address_32: prefixes.address_32,
            segment: prefixes.segment,
            ..EffectiveAddress::default()
        };
        if rm == 4 {
            let sib = cursor.byte()?;
            value.scale = sib >> 6;
            let raw_index = (sib >> 3) & 7;
            let vsib = vector && map == 2 && matches!(opcode, 0x90..=0x93);
            if raw_index != 4 || rex.x || vsib {
                value.index = Some(raw_index | (u8::from(rex.x) << 3));
            }
            let raw_base = sib & 7;
            if raw_base != 5 || mode != 0 {
                value.base = Some(raw_base | (u8::from(rex.b) << 3));
            }
        } else if rm == 5 && mode == 0 {
            value.rip_relative = true;
        } else {
            value.base = Some(rm | (u8::from(rex.b) << 3));
        }
        value.displacement = if mode == 1 {
            cursor.signed(1)?
        } else if mode == 2 || value.rip_relative || (rm == 4 && value.base.is_none()) {
            cursor.signed(4)?
        } else {
            0
        };
        Ok(value)
    }
}

impl DecodedInstruction {
    #[must_use]
    pub const fn rex(&self) -> Option<Rex> {
        match self.encoding {
            Encoding::Legacy { rex, .. } => rex,
            _ => None,
        }
    }

    #[must_use]
    pub const fn byte_register(&self, raw: u8, extension: bool) -> Option<ByteRegister> {
        if raw > 7 {
            return None;
        }
        if extension {
            return Some(ByteRegister::Low(raw + 8));
        }
        if self.rex().is_some() || raw < 4 {
            return Some(ByteRegister::Low(raw));
        }
        Some(ByteRegister::High(raw - 4))
    }
}
