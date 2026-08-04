use hl_descriptor::{ObjectError, OperationLease};
use hl_linux::{Errno, GuestIovec};

/// Direction of one Linux vector transfer from the descriptor's perspective.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VectorDirection {
    Read,
    Write,
}

/// File-position selection for one vector transfer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VectorPosition {
    Shared,
    At(u64),
}

/// Pointer-free request passed to the selected native terminal.
#[derive(Clone, Copy, Debug)]
pub struct VectorRequest<'a> {
    pub vectors: &'a [GuestIovec],
    pub direction: VectorDirection,
    pub position: VectorPosition,
    /// `Some` distinguishes preadv2/pwritev2 from legacy vector calls even
    /// when the supplied flag word is zero.
    pub flags: Option<u32>,
}

/// Failure returned by a selected vector terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VectorError {
    /// This OFD belongs to another semantic owner; use its ordinary vector API.
    Unsupported,
    /// The first accessible payload byte is absent.
    Fault,
    /// A descriptor object rejected the operation before a native terminal.
    Object(ObjectError),
    /// Exact Linux errno returned by the native terminal.
    Errno(Errno),
}

/// Consumer-owned capability for one opaque native vector operation.
///
/// Implementations receive guest coordinates and an admitted OFD lease, never
/// host pointers or native descriptor integers. The application adapter is the
/// only layer allowed to join those resources at its reviewed unsafe boundary.
pub trait VectorTerminal: Send + Sync {
    fn execute(&self, descriptor: &OperationLease, request: VectorRequest<'_>) -> Result<usize, VectorError>;
}
