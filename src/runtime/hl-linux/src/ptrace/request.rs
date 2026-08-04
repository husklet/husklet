pub const NT_PRSTATUS: u64 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Options(u64);

impl Options {
    pub const TRACESYSGOOD: u64 = 1;
    pub const TRACEEXEC: u64 = 0x10;

    #[must_use]
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    #[must_use]
    pub const fn bits(self) -> u64 {
        self.0
    }

    #[must_use]
    pub const fn traces_syscalls(self) -> bool {
        self.0 & Self::TRACESYSGOOD != 0
    }

    #[must_use]
    pub const fn traces_exec(self) -> bool {
        self.0 & Self::TRACEEXEC != 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Resume {
    Continue,
    Syscall,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Plan {
    TraceMe,
    Attach {
        process: u32,
    },
    Seize {
        process: u32,
        options: Options,
    },
    Detach {
        process: u32,
        signal: u32,
    },
    Resume {
        process: u32,
        signal: u32,
        mode: Resume,
    },
    Kill {
        process: u32,
    },
    GetRegisters {
        process: u32,
        destination: u64,
    },
    SetRegisters {
        process: u32,
        source: u64,
    },
    GetRegisterSet {
        process: u32,
        note: u64,
        iovec: u64,
    },
    SetRegisterSet {
        process: u32,
        note: u64,
        iovec: u64,
    },
    PeekData {
        process: u32,
        address: u64,
        destination: u64,
    },
    PokeData {
        process: u32,
        address: u64,
        word: u64,
    },
    PeekUser {
        process: u32,
        offset: u64,
        destination: u64,
    },
    PokeUser {
        process: u32,
        offset: u64,
        word: u64,
    },
    SetOptions {
        process: u32,
        options: Options,
    },
    GetEventMessage {
        process: u32,
        destination: u64,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Request {
    Supported(Plan),
    Unsupported,
}

impl Request {
    #[must_use]
    pub const fn decode(arguments: [u64; 6]) -> Self {
        let request = arguments[0];
        let process = arguments[1] as u32;
        let address = arguments[2];
        let data = arguments[3];
        let plan = match request {
            0 => Plan::TraceMe,
            1 | 2 => Plan::PeekData {
                process,
                address,
                destination: data,
            },
            3 => Plan::PeekUser {
                process,
                offset: address,
                destination: data,
            },
            4 | 5 => Plan::PokeData {
                process,
                address,
                word: data,
            },
            6 => Plan::PokeUser {
                process,
                offset: address,
                word: data,
            },
            7 => Plan::Resume {
                process,
                signal: data as u32,
                mode: Resume::Continue,
            },
            8 => Plan::Kill { process },
            12 => Plan::GetRegisters {
                process,
                destination: data,
            },
            13 => Plan::SetRegisters { process, source: data },
            16 => Plan::Attach { process },
            17 => Plan::Detach {
                process,
                signal: data as u32,
            },
            24 => Plan::Resume {
                process,
                signal: data as u32,
                mode: Resume::Syscall,
            },
            0x4200 => Plan::SetOptions {
                process,
                options: Options::from_bits(data),
            },
            0x4201 => Plan::GetEventMessage {
                process,
                destination: data,
            },
            0x4204 => Plan::GetRegisterSet {
                process,
                note: address,
                iovec: data,
            },
            0x4205 => Plan::SetRegisterSet {
                process,
                note: address,
                iovec: data,
            },
            0x4206 => Plan::Seize {
                process,
                options: Options::from_bits(data),
            },
            _ => return Self::Unsupported,
        };
        Self::Supported(plan)
    }
}
