#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObjectError {
    BadDescriptor,
    InvalidArgument,
    WouldBlock,
    Interrupted,
    Canceled,
    ResourceLimit,
    NoSpace,
    NoExtent,
    PermissionDenied,
    Busy,
    BrokenPipe,
    Retired,
    NoSuchProcess,
    NotSupported,
    Io,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObjectKind {
    File,
    Directory,
    Socket,
    Pipe,
    Event,
    EventCounter,
    Poll,
    Other,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(transparent)]
pub struct Readiness(u32);

impl Readiness {
    pub const READ: u32 = 1 << 0;
    pub const WRITE: u32 = 1 << 1;
    pub const PRIORITY: u32 = 1 << 2;
    pub const ERROR: u32 = 1 << 3;
    pub const HANGUP: u32 = 1 << 4;
    pub const READ_HANGUP: u32 = 1 << 5;
    #[must_use]
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }
    #[must_use]
    pub const fn contains(self, bit: u32) -> bool {
        self.0 & bit != 0
    }
}

pub trait PreparedAtomicRead: Send {
    fn bytes(&self) -> &[u8];
    ///
    /// # Errors
    /// Returns an error if the prepared operation cannot be completed.
    fn commit(self: Box<Self>) -> Result<(), ObjectError>;

    /// Commits an accessible prefix when this object has byte-stream partial
    /// read semantics. Record-oriented objects retain the default and leave
    /// the transaction untouched unless the complete record was copied.
    ///
    /// # Errors
    /// Returns an error if the prepared operation cannot be completed.
    fn commit_prefix(self: Box<Self>, count: usize) -> Result<bool, ObjectError> {
        if count != self.bytes().len() {
            return Ok(false);
        }
        self.commit()?;
        Ok(true)
    }
}

pub trait PreparedSpliceRead: Send {
    fn bytes(&self) -> &[u8];
    ///
    /// # Errors
    /// Returns an error if the prepared operation cannot be completed.
    fn commit(self: Box<Self>, count: usize) -> Result<(), ObjectError>;
}

/// Guest actor attached to one admitted descriptor operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OperationActor {
    pub process: u32,
    pub process_generation: u16,
    pub thread: u32,
    pub thread_generation: u16,
}

/// Caller identity and cancellation capabilities for one OFD operation.
#[derive(Clone, Copy)]
pub struct OperationContext<'a> {
    pub actor: Option<OperationActor>,
    pub cancellation: Option<&'a dyn OperationCancellation>,
}

/// Consumer-supplied cancellation state for a potentially blocking OFD operation.
pub trait OperationCancellation: Send + Sync {
    fn interrupted(&self) -> bool;
    fn subscribe(
        &self,
        notification: std::sync::Arc<dyn CancellationNotification>,
    ) -> Box<dyn CancellationSubscription>;
}

pub trait CancellationNotification: Send + Sync {
    fn notify(&self);
}

pub trait CancellationSubscription: Send {}

/// Type-erased pipe endpoint used only by cross-OFD transfer composition.
pub trait PipeTransferEndpoint: std::any::Any + Send + Sync {
    fn as_any(&self) -> &dyn std::any::Any;
}
