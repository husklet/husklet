use hl_isa::{CoreRegister, GuestArchitecture};

use crate::Errno;

pub trait RegisterView {
    fn read(&self, register: CoreRegister) -> Option<u64>;
    fn write(&mut self, register: CoreRegister, value: u64) -> bool;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Frame {
    pub architecture: GuestArchitecture,
    pub raw_number: u64,
    pub arguments: [u64; 6],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameError {
    MissingRegister(CoreRegister),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RestartKind {
    NoInterrupt = 512,
    NoHandler = 513,
    RestartBlock = 516,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinuxResult {
    Value(u64),
    Error(Errno),
    Restart(RestartKind),
}

impl LinuxResult {
    #[must_use]
    pub const fn encode(self) -> u64 {
        match self {
            Self::Value(value) => value,
            Self::Error(errno) => errno.negative_i64() as u64,
            Self::Restart(kind) => -(kind as i64) as u64,
        }
    }
}

pub struct FrameDecoder;

impl FrameDecoder {
    pub fn decode(architecture: GuestArchitecture, registers: &dyn RegisterView) -> Result<Frame, FrameError> {
        let (number, arguments) = match architecture {
            GuestArchitecture::Aarch64 => (CoreRegister::GeneralPurpose(8), [0, 1, 2, 3, 4, 5]),
            GuestArchitecture::X86_64 => (CoreRegister::GeneralPurpose(0), [7, 6, 2, 10, 8, 9]),
        };
        let raw_number = registers.read(number).ok_or(FrameError::MissingRegister(number))?;
        let mut values = [0; 6];
        for (slot, index) in values.iter_mut().zip(arguments) {
            let register = CoreRegister::GeneralPurpose(index);
            *slot = registers.read(register).ok_or(FrameError::MissingRegister(register))?;
        }
        Ok(Frame {
            architecture,
            raw_number,
            arguments: values,
        })
    }

    pub fn write_result(
        frame: &Frame,
        registers: &mut dyn RegisterView,
        result: LinuxResult,
    ) -> Result<(), FrameError> {
        let register = match frame.architecture {
            GuestArchitecture::Aarch64 | GuestArchitecture::X86_64 => CoreRegister::GeneralPurpose(0),
        };
        if registers.write(register, result.encode()) {
            Ok(())
        } else {
            Err(FrameError::MissingRegister(register))
        }
    }
}
