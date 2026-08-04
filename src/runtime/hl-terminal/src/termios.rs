/// Input processing flags represented with Linux-visible bit positions.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(transparent)]
pub struct Input(u32);

impl Input {
    pub const CR_TO_NL: u32 = 0o000_0400;
    pub const FLOW: u32 = 0o000_2000;

    #[must_use]
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    #[must_use]
    pub const fn contains(self, flag: u32) -> bool {
        self.0 & flag != 0
    }
}

/// Output processing flags represented with Linux-visible bit positions.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(transparent)]
pub struct Output(u32);

impl Output {
    pub const PROCESS: u32 = 0o000_0001;
    pub const NL_TO_CRNL: u32 = 0o000_0004;

    #[must_use]
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    #[must_use]
    pub const fn contains(self, flag: u32) -> bool {
        self.0 & flag != 0
    }
}

/// Control flags and Linux baud-code fields.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(transparent)]
pub struct Control(u32);

impl Control {
    #[must_use]
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }
}

/// Local line-discipline flags represented with Linux-visible bit positions.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(transparent)]
pub struct Local(u32);

impl Local {
    pub const SIGNALS: u32 = 0o000_0001;
    pub const CANONICAL: u32 = 0o000_0002;
    pub const ECHO: u32 = 0o000_0010;
    pub const TO_STOP: u32 = 0o000_0400;

    #[must_use]
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    #[must_use]
    pub const fn contains(self, flag: u32) -> bool {
        self.0 & flag != 0
    }
}

/// Linux terminal state independent of any host `termios` layout.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Settings {
    pub input: Input,
    pub output: Output,
    pub control: Control,
    pub local: Local,
    pub line: u8,
    pub characters: [u8; 19],
    pub input_speed: u32,
    pub output_speed: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WireError {
    Size,
}

impl Settings {
    pub const INTERRUPT: usize = 0;
    pub const QUIT: usize = 1;
    pub const ERASE: usize = 2;
    pub const EOF: usize = 4;
    pub const TIME: usize = 5;
    pub const MINIMUM: usize = 6;
    pub const SUSPEND: usize = 10;

    #[must_use]
    pub fn linux_default() -> Self {
        let mut characters = [0_u8; 19];
        characters[Self::INTERRUPT] = 3;
        characters[Self::QUIT] = 28;
        characters[Self::ERASE] = 127;
        characters[Self::EOF] = 4;
        characters[Self::SUSPEND] = 26;
        Self {
            input: Input::from_bits(Input::CR_TO_NL | Input::FLOW),
            output: Output::from_bits(Output::PROCESS | Output::NL_TO_CRNL),
            control: Control::from_bits(0o000_0277),
            local: Local::from_bits(Local::SIGNALS | Local::CANONICAL | Local::ECHO),
            line: 0,
            characters,
            input_speed: 38_400,
            output_speed: 38_400,
        }
    }

    #[must_use]
    pub const fn canonical(&self) -> bool {
        self.local.contains(Local::CANONICAL)
    }

    #[must_use]
    pub const fn signals(&self) -> bool {
        self.local.contains(Local::SIGNALS)
    }

    pub fn decode(input: &[u8]) -> Result<Self, WireError> {
        if !matches!(input.len(), 36 | 44) {
            return Err(WireError::Size);
        }
        let word = |offset: usize| {
            u32::from_le_bytes([input[offset], input[offset + 1], input[offset + 2], input[offset + 3]])
        };
        let mut characters = [0_u8; 19];
        characters.copy_from_slice(&input[17..36]);
        Ok(Self {
            input: Input::from_bits(word(0)),
            output: Output::from_bits(word(4)),
            control: Control::from_bits(word(8)),
            local: Local::from_bits(word(12)),
            line: input[16],
            characters,
            input_speed: if input.len() == 44 { word(36) } else { 0 },
            output_speed: if input.len() == 44 { word(40) } else { 0 },
        })
    }

    #[must_use]
    pub fn encode(&self, extended: bool) -> Vec<u8> {
        let mut output = vec![0_u8; if extended { 44 } else { 36 }];
        output[0..4].copy_from_slice(&self.input.bits().to_le_bytes());
        output[4..8].copy_from_slice(&self.output.bits().to_le_bytes());
        output[8..12].copy_from_slice(&self.control.bits().to_le_bytes());
        output[12..16].copy_from_slice(&self.local.bits().to_le_bytes());
        output[16] = self.line;
        output[17..36].copy_from_slice(&self.characters);
        if extended {
            output[36..40].copy_from_slice(&self.input_speed.to_le_bytes());
            output[40..44].copy_from_slice(&self.output_speed.to_le_bytes());
        }
        output
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn linux_defaults() {
        let settings = Settings::linux_default();
        assert!(settings.canonical());
        assert!(settings.signals());
        assert_eq!(settings.characters[Settings::ERASE], 127);
        assert_eq!(settings.output_speed, 38_400);
    }

    #[test]
    fn wire_roundtrip() {
        let settings = Settings::linux_default();
        let bytes = settings.encode(true);
        assert_eq!(Settings::decode(&bytes), Ok(settings));
        assert_eq!(Settings::decode(&bytes[..35]), Err(WireError::Size));
    }
}
