use std::fmt;
use std::num::NonZeroU64;

/// Stable identity assigned by the remote provider.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RemoteId(NonZeroU64);

impl RemoteId {
    pub fn new(value: u64) -> Option<Self> {
        NonZeroU64::new(value).map(Self)
    }

    pub fn get(self) -> u64 {
        self.0.get()
    }
}

/// Domain of a provider-owned resource.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum HandleKind {
    File,
    Directory,
    Mapping,
    Process,
    Event,
    Counter,
    Subscription,
    Transfer,
}

/// Opaque, generation-qualified identifier in a [`super::HandleNamespace`].
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct Handle(pub(super) u64);

impl fmt::Debug for Handle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Handle(opaque)")
    }
}
