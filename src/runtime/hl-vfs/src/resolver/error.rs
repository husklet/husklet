use crate::PathError;

/// Host-mechanism failures preserved without assigning Linux errno policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolveHostError {
    NotFound,
    NotDirectory,
    PermissionDenied,
    ResourceLimit,
    Io,
}

/// One host-neutral confined-walk failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolveError {
    Path(PathError),
    RelativeBase,
    PathTooLong,
    ComponentTooLong,
    TooManyComponents,
    SymlinkLoop,
    SymlinkForbidden,
    MagicLinkForbidden,
    CrossDevice,
    Escape,
    NotDirectory,
    UnsupportedMountKind,
    Host(ResolveHostError),
}
