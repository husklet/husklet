//! What a peer is allowed to do, and the grant it was given.
//!
//! This crate owns the concept of a permission and the set of them a peer
//! holds; it deliberately owns no list of permissions. A library that declares
//! routes declares the permissions those routes need, as its own type, and
//! implements [`Capability`] for it. Two domains can therefore be mounted on
//! one socket without either knowing what the other permits.

use std::collections::BTreeSet;

/// One permission a domain declares.
///
/// Implemented by a domain's own enum. Only a value of such a type can produce
/// a [`CapabilityKey`], and only a key opens a check, so a route cannot ask for
/// a permission that was never declared and no permission can be conjured from
/// a string that arrived over the wire.
pub trait Capability: Copy + Ord + std::fmt::Debug + 'static {
    /// The library declaring these permissions. Part of the key, so two
    /// domains that both call something `read` remain distinct permissions.
    const DOMAIN: &'static str;

    /// Every permission this domain declares. A grant can be rebuilt from an
    /// erased [`Warrant`] only because this enumeration exists.
    const ALL: &'static [Self];

    /// This permission's name within its domain.
    fn name(&self) -> &'static str;

    /// Whether holding this amounts to running code on the host. Consent has to
    /// say so plainly rather than imply a sandbox; a permission that does not
    /// grant execution needs no opinion here.
    fn executes(&self) -> bool {
        false
    }

    /// The domain-qualified identity used wherever the permission is stored or
    /// checked without its type.
    fn key(&self) -> CapabilityKey {
        CapabilityKey {
            domain: Self::DOMAIN,
            name: self.name(),
        }
    }
}

/// A permission stripped of its type but not of its origin.
///
/// Constructible only through [`Capability::key`], which is what keeps the
/// erased side of the protocol from becoming a stringly-typed one: a key can be
/// compared, stored, and reported, but never invented.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CapabilityKey {
    domain: &'static str,
    name: &'static str,
}

impl CapabilityKey {
    /// The library that declared the permission.
    #[must_use]
    pub const fn domain(self) -> &'static str {
        self.domain
    }

    /// The permission's name within its domain.
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.name
    }
}

impl std::fmt::Display for CapabilityKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}.{}", self.domain, self.name)
    }
}

/// A granted set of one domain's permissions.
///
/// Typed, so a manifest declaring what it wants keeps the domain's own
/// serialized names, and so a domain's code never handles another's permissions
/// by accident.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(transparent)]
pub struct Grant<C: Capability> {
    held: BTreeSet<C>,
}

impl<C: Capability> Default for Grant<C> {
    fn default() -> Self {
        Self { held: BTreeSet::new() }
    }
}

impl<C: Capability> Grant<C> {
    /// A grant holding exactly the given permissions.
    #[must_use]
    pub fn new(capabilities: impl IntoIterator<Item = C>) -> Self {
        Self {
            held: capabilities.into_iter().collect(),
        }
    }

    /// Whether this permission is held.
    #[must_use]
    pub fn holds(&self, capability: C) -> bool {
        self.held.contains(&capability)
    }

    /// Whether nothing at all is granted.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    /// How many permissions are held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.held.len()
    }

    /// The permissions held, in a stable order.
    pub fn iter(&self) -> impl Iterator<Item = C> + '_ {
        self.held.iter().copied()
    }

    /// Whether every permission in `other` is already held. A re-consent prompt
    /// is required exactly when this is false.
    #[must_use]
    pub fn covers(&self, other: &Self) -> bool {
        other.held.is_subset(&self.held)
    }

    /// The permissions in `other` that this grant does not hold.
    #[must_use]
    pub fn missing(&self, other: &Self) -> Vec<C> {
        other.held.difference(&self.held).copied().collect()
    }

    /// Narrows to what both hold. An updated manifest asking for more must
    /// start from the recorded grant, never widen itself.
    #[must_use]
    pub fn intersect(&self, other: &Self) -> Self {
        Self {
            held: self.held.intersection(&other.held).copied().collect(),
        }
    }

    /// Whether this grant amounts to code execution on the host.
    #[must_use]
    pub fn executes(&self) -> bool {
        self.held.iter().any(Capability::executes)
    }

    /// This domain's share of what a peer was granted across every domain.
    #[must_use]
    pub fn within(warrant: &Warrant) -> Self {
        Self::new(C::ALL.iter().copied().filter(|entry| warrant.holds(entry.key())))
    }
}

/// Everything one peer holds, across every domain it can reach.
///
/// A connection carries one warrant, not one grant per library, because a
/// single socket may serve routes declared by several domains and the check has
/// to be uniform across them.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Warrant {
    held: BTreeSet<CapabilityKey>,
}

impl Warrant {
    /// An empty warrant, which reaches nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a permission is held.
    #[must_use]
    pub fn holds(&self, key: CapabilityKey) -> bool {
        self.held.contains(&key)
    }

    /// How many permissions are held, across all domains.
    #[must_use]
    pub fn len(&self) -> usize {
        self.held.len()
    }

    /// Whether nothing at all is granted.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    /// The permissions held, in a stable order.
    pub fn iter(&self) -> impl Iterator<Item = CapabilityKey> + '_ {
        self.held.iter().copied()
    }

    /// Adds another domain's grant. Composition is a union: mounting a second
    /// library widens what the peer may reach only by what that library was
    /// separately granted.
    #[must_use]
    pub fn with<C: Capability>(mut self, granted: &Grant<C>) -> Self {
        self.held.extend(granted.iter().map(|entry| entry.key()));
        self
    }

    /// Withdraws a permission.
    pub fn revoke(&mut self, key: CapabilityKey) {
        self.held.remove(&key);
    }
}

impl<C: Capability> From<&Grant<C>> for Warrant {
    fn from(granted: &Grant<C>) -> Self {
        Self::new().with(granted)
    }
}

impl<C: Capability> From<Grant<C>> for Warrant {
    fn from(granted: Grant<C>) -> Self {
        Self::from(&granted)
    }
}

#[cfg(test)]
mod tests {
    use super::{Capability, Grant, Warrant};

    #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize)]
    #[serde(rename_all = "kebab-case")]
    enum Reach {
        Read,
        Write,
    }

    impl Capability for Reach {
        const DOMAIN: &'static str = "sample";
        const ALL: &'static [Self] = &[Self::Read, Self::Write];

        fn name(&self) -> &'static str {
            match self {
                Self::Read => "read",
                Self::Write => "write",
            }
        }

        fn executes(&self) -> bool {
            matches!(self, Self::Write)
        }
    }

    #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
    enum Other {
        Read,
    }

    impl Capability for Other {
        const DOMAIN: &'static str = "other";
        const ALL: &'static [Self] = &[Self::Read];

        fn name(&self) -> &'static str {
            "read"
        }
    }

    #[test]
    fn a_grant_reports_exactly_what_it_holds() {
        let grant = Grant::new([Reach::Read]);
        assert!(grant.holds(Reach::Read));
        assert!(!grant.holds(Reach::Write));
        assert_eq!(grant.len(), 1);
    }

    #[test]
    fn two_domains_naming_a_permission_alike_stay_distinct() {
        let warrant = Warrant::new().with(&Grant::new([Reach::Read]));
        assert!(warrant.holds(Reach::Read.key()));
        assert!(
            !warrant.holds(Other::Read.key()),
            "a name is only a permission within its own domain"
        );
    }

    #[test]
    fn a_warrant_carries_several_domains_at_once() {
        let warrant = Warrant::new()
            .with(&Grant::new([Reach::Read]))
            .with(&Grant::new([Other::Read]));

        assert_eq!(warrant.len(), 2);
        assert_eq!(Grant::<Reach>::within(&warrant), Grant::new([Reach::Read]));
        assert_eq!(
            Grant::<Other>::within(&warrant),
            Grant::new([Other::Read]),
            "each domain reads back only its own"
        );
    }

    #[test]
    fn a_wider_request_is_narrowed_to_the_recorded_grant() {
        let recorded = Grant::new([Reach::Read]);
        let requested = Grant::new([Reach::Read, Reach::Write]);

        assert!(!recorded.covers(&requested));
        assert_eq!(recorded.missing(&requested), vec![Reach::Write]);
        assert_eq!(recorded.intersect(&requested), recorded);
    }

    #[test]
    fn execution_grants_are_identified_for_the_consent_prompt() {
        assert!(Grant::new([Reach::Write]).executes());
        assert!(!Grant::new([Reach::Read]).executes());
    }

    #[test]
    fn a_grant_keeps_its_own_serialized_names() {
        let encoded = serde_json::to_string(&Grant::new([Reach::Read, Reach::Write])).expect("encoded");
        assert_eq!(encoded, "[\"read\",\"write\"]");
    }
}
