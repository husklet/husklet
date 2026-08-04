use hl_vfs::{Access, GuestPathBytes, OpenDirectory, OpenIntent};

use crate::{MarshalError, StatEncodingError, StatxExtensions};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AbiError {
    Marshal(MarshalError),
    NoEntry,
    Invalid,
    Range,
    TooBig,
    NameTooLong,
    Overflow,
    Encoding,
}

impl From<MarshalError> for AbiError {
    fn from(error: MarshalError) -> Self {
        Self::Marshal(error)
    }
}

impl From<StatEncodingError> for AbiError {
    fn from(_: StatEncodingError) -> Self {
        Self::Encoding
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ResolveFlags {
    pub no_cross_device: bool,
    pub no_magic_links: bool,
    pub no_symlinks: bool,
    pub beneath: bool,
    pub in_root: bool,
    pub cached: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PathOperand {
    pub directory: OpenDirectory,
    pub path: GuestPathBytes,
    pub allow_empty: bool,
    pub nofollow: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Target {
    Descriptor(i32),
    Path(PathOperand),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenAbiPlan {
    pub operand: PathOperand,
    pub intent: OpenIntent,
    pub mode: u32,
    pub close_on_exec: bool,
    pub nonblocking: bool,
    pub no_controlling_terminal: bool,
    pub resolve: ResolveFlags,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccessPlan {
    pub operand: PathOperand,
    pub access: Access,
    pub effective_ids: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatOutputKind {
    Stat,
    Statx { extensions: StatxExtensions },
}
