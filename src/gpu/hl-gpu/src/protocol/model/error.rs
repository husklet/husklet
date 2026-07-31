//! The protocol's single error type. Every fallible boundary — wire decode, handle-table lifecycle,
//! capability negotiation — returns this, so a malformed guest stream is a typed error, never UB.

/// The protocol's typed error. Ported byte-for-byte in meaning from `hl-gpu`'s `GpuError`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GpuError {
    /// Ran off the end of the input while decoding.
    ShortBuffer,
    /// Unknown top-level command / encoder tag byte.
    BadTag(u32),
    /// An enum field held a value outside its defined set.
    BadEnum { what: &'static str, val: u32 },
    /// A string field was not valid UTF-8.
    Utf8,
    /// A length-prefixed frame held extra bytes after its message decoded (trailing garbage).
    TrailingBytes,
    /// A boolean wire field held a byte other than the canonical `0`/`1`.
    NonCanonicalBool(u8),
    /// Created an id that is already live.
    DuplicateId { kind: &'static str, id: u32 },
    /// Referenced an id that was never created or was already freed.
    UnknownId { kind: &'static str, id: u32 },
    /// The backend does not implement this operation.
    Unsupported(&'static str),
    /// A copy/write/read fell outside the resource's bounds.
    OutOfBounds,
    /// A render-state float (viewport/scissor coordinate, clear color, depth) decoded as non-finite
    /// (NaN or ±∞) at the wire boundary — a malformed/hostile payload rejected before it can poison a
    /// backend's viewport/clear/transform. The `&'static str` names the offending field.
    NonFinite(&'static str),
    /// A descriptor/command field was structurally invalid (zero size, contradictory range,
    /// unsupported dimensionality, missing required usage bit, etc.) — a validation rejection
    /// distinct from a plain bounds violation.
    Invalid(&'static str),
    /// A negotiated per-object or aggregate executor resource limit was exceeded.
    ResourceLimit(&'static str),
    /// Malformed or unsupported kernel descriptor while decoding a neutral kernel-IR payload.
    Kernel(String),
    /// The executor ABORTED ABNORMALLY — it panicked partway through a batch instead of returning. The
    /// frame is refused and the session rolled back exactly as for any other rejection, so this is not a
    /// different OUTCOME; it is a different CAUSE, and it stays distinguishable because the two are worth
    /// telling apart. A refusal is the backend doing its job; a panic is a backend defect, and one that
    /// reached this variant is a bug to fix rather than a guest to blame. Carries the panic's own message.
    Panicked(String),
    /// Higher-level decode context wrapped around a low-level wire error.
    Decode(String),
    /// A typed failure at the remote command transport boundary.
    Transport(crate::transport::model::error::TransportError),
}

impl GpuError {
    /// Construct an interpreter/kernel diagnostic while keeping its owned context.
    pub(crate) fn kernel(message: impl Into<String>) -> Self {
        Self::Kernel(message.into())
    }
}

impl std::fmt::Display for GpuError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GpuError::ShortBuffer => write!(f, "short buffer while decoding"),
            GpuError::BadTag(t) => write!(f, "bad command/encoder tag {t}"),
            GpuError::BadEnum { what, val } => write!(f, "bad {what} enum value {val}"),
            GpuError::Utf8 => write!(f, "invalid utf-8 in string field"),
            GpuError::TrailingBytes => write!(f, "trailing bytes after framed message"),
            GpuError::NonCanonicalBool(b) => write!(f, "non-canonical boolean wire byte {b}"),
            GpuError::DuplicateId { kind, id } => write!(f, "duplicate {kind} id {id}"),
            GpuError::UnknownId { kind, id } => write!(f, "unknown/freed {kind} id {id}"),
            GpuError::Unsupported(op) => write!(f, "backend does not support {op}"),
            GpuError::OutOfBounds => write!(f, "access out of bounds"),
            GpuError::NonFinite(field) => write!(f, "non-finite float in render state: {field}"),
            GpuError::Invalid(m) => write!(f, "invalid argument: {m}"),
            GpuError::ResourceLimit(m) => write!(f, "executor resource limit: {m}"),
            GpuError::Kernel(m) => write!(f, "kernel: {m}"),
            GpuError::Panicked(m) => write!(f, "executor panicked (backend defect): {m}"),
            GpuError::Decode(m) => write!(f, "decode: {m}"),
            GpuError::Transport(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for GpuError {}

/// The protocol's `Result` alias.
pub type Result<T> = std::result::Result<T, GpuError>;
