#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LegacyPrefixes {
    pub operand_16: bool,
    pub address_32: bool,
    pub lock: bool,
    pub rep: bool,
    pub repne: bool,
    pub segment: Option<Segment>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Segment {
    Fs,
    Gs,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Rex {
    pub w: bool,
    pub r: bool,
    pub x: bool,
    pub b: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Encoding {
    Legacy {
        rex: Option<Rex>,
        map: u8,
    },
    Vex {
        map: u8,
        pp: u8,
        length: u8,
        w: bool,
        source: u8,
        rex: Rex,
    },
    Evex {
        map: u8,
        pp: u8,
        length: u8,
        w: bool,
        source: u8,
        rex: Rex,
        mask: u8,
        zero: bool,
        broadcast: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ByteRegister {
    Low(u8),
    High(u8),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EffectiveAddress {
    pub base: Option<u8>,
    pub index: Option<u8>,
    pub scale: u8,
    pub displacement: i64,
    pub rip_relative: bool,
    pub address_32: bool,
    pub segment: Option<Segment>,
}

impl EffectiveAddress {
    pub fn resolve(&self, registers: &[u64; 16], next_instruction: u64, fs: u64, gs: u64) -> u64 {
        let mut value = if self.rip_relative {
            next_instruction
        } else {
            self.base.map_or(0, |r| registers[r as usize])
        };
        if let Some(index) = self.index {
            value = value.wrapping_add(registers[index as usize].wrapping_shl(u32::from(self.scale)));
        }
        value = value.wrapping_add(self.displacement as u64);
        if self.address_32 {
            value = u64::from(value as u32);
        }
        value.wrapping_add(match self.segment {
            Some(Segment::Fs) => fs,
            Some(Segment::Gs) => gs,
            None => 0,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedInstruction {
    pub length: u8,
    pub prefixes: LegacyPrefixes,
    pub encoding: Encoding,
    pub opcode: u8,
    pub modrm: Option<u8>,
    pub raw_mod: Option<u8>,
    pub raw_reg: Option<u8>,
    pub raw_rm: Option<u8>,
    pub register: Option<u8>,
    pub register_operand: Option<u8>,
    pub address: Option<EffectiveAddress>,
    pub immediate: Option<(i64, u8)>,
}
