/// Host-stable identity for one retained node, allocated by the producer.
///
/// Identifiers are monotonic within a session and are never reused, so a late
/// patch naming a removed node is always detectable rather than ambiguous.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[cfg_attr(feature = "wire", derive(serde::Deserialize, serde::Serialize))]
pub struct NodeId(u64);

impl NodeId {
    /// The implicit container every top-level node is inserted into.
    pub const ROOT: Self = Self(0);

    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }

    #[must_use]
    pub const fn is_root(self) -> bool {
        self.0 == Self::ROOT.0
    }
}

/// Allocates node identifiers for one producer session.
#[derive(Debug, Default)]
pub struct Identities {
    next: u64,
}

impl Identities {
    #[must_use]
    pub const fn new() -> Self {
        Self { next: 1 }
    }

    /// Returns an identifier that this allocator has never returned before.
    pub fn allocate(&mut self) -> NodeId {
        let raw = self.next.max(1);
        self.next = raw.saturating_add(1);
        NodeId::new(raw)
    }
}

#[cfg(test)]
mod tests {
    use super::{Identities, NodeId};

    #[test]
    fn root_is_distinct_from_every_allocated_identity() {
        let mut identities = Identities::new();
        let first = identities.allocate();
        let second = identities.allocate();
        assert!(NodeId::ROOT.is_root());
        assert!(!first.is_root());
        assert_ne!(first, second);
    }
}
