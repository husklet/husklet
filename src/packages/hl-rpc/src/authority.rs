//! The single place a capability is checked.
//!
//! Ports are reachable only through this type, so a route cannot obtain a
//! handle to a host service without naming the capability it needs. A missing
//! check is therefore a compile error rather than something review has to catch.

use crate::capability::{Capability, CapabilityKey, Grant, Warrant};
use crate::name::PeerName;
use crate::path::RelativePath;

/// A refused operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Denial {
    pub capability: CapabilityKey,
    pub detail: Reason,
}

/// Why an operation was refused.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Reason {
    /// The peer does not hold the capability.
    Ungranted,
    /// The path lies outside every declared root.
    Unrooted(RelativePath),
}

impl std::fmt::Display for Denial {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.detail {
            Reason::Ungranted => write!(formatter, "{} is not granted", self.capability.name()),
            Reason::Unrooted(path) => write!(
                formatter,
                "{path} is outside the roots {} was granted",
                self.capability.name()
            ),
        }
    }
}

impl std::error::Error for Denial {}

/// A permission-bearing handle to one host service.
///
/// Holding this is proof a capability was checked. It is only ever produced by
/// [`Authority`], and it borrows rather than owns, so it cannot outlive the
/// check that made it.
#[derive(Debug)]
pub struct Permit<'a, P: ?Sized> {
    port: &'a P,
}

impl<P: ?Sized> Permit<'_, P> {
    /// The service this permit grants access to.
    #[must_use]
    pub const fn port(&self) -> &P {
        self.port
    }
}

impl<P: ?Sized> std::ops::Deref for Permit<'_, P> {
    type Target = P;

    fn deref(&self) -> &Self::Target {
        self.port
    }
}

/// What one connected peer is allowed to do.
///
/// The held permissions are kept as a [`Warrant`] rather than one domain's
/// [`Grant`], because a single socket may serve routes declared by several
/// libraries and one check has to answer for all of them. What may be asked is
/// still typed: a check takes a value of a domain's own capability type.
#[derive(Debug)]
pub struct Authority {
    peer: PeerName,
    held: Warrant,
    roots: Vec<RelativePath>,
}

impl Authority {
    /// The authority of one peer, over what it was granted and where it may reach.
    #[must_use]
    pub fn new(peer: PeerName, granted: impl Into<Warrant>, roots: Vec<RelativePath>) -> Self {
        Self {
            peer,
            held: granted.into(),
            roots,
        }
    }

    /// Who is connected.
    #[must_use]
    pub const fn peer(&self) -> &PeerName {
        &self.peer
    }

    /// Everything held, across every domain.
    #[must_use]
    pub const fn warrant(&self) -> &Warrant {
        &self.held
    }

    /// One domain's share of what is held, in that domain's own type.
    #[must_use]
    pub fn granted<C: Capability>(&self) -> Grant<C> {
        Grant::within(&self.held)
    }

    /// The roots a path-bearing call is confined to.
    #[must_use]
    pub fn roots(&self) -> &[RelativePath] {
        &self.roots
    }

    /// Narrows the grant. A revoked capability takes effect on the next check,
    /// including for a subscription established while it was still held.
    pub fn revoke<C: Capability>(&mut self, capability: C) {
        self.held.revoke(capability.key());
    }

    /// # Errors
    /// Returns `Denial` when the capability is not held.
    pub fn permit<C: Capability>(&self, capability: C) -> Result<(), Denial> {
        self.permit_key(capability.key())
    }

    /// Checks a capability whose type was erased when a route declared it.
    ///
    /// # Errors
    /// Returns `Denial` when the capability is not held.
    pub fn permit_key(&self, capability: CapabilityKey) -> Result<(), Denial> {
        if self.held.holds(capability) {
            return Ok(());
        }
        Err(Denial {
            capability,
            detail: Reason::Ungranted,
        })
    }

    /// Grants access to a port, which is the only way to obtain one.
    ///
    /// # Errors
    /// Returns `Denial` when the capability is not held.
    pub fn port<'a, C: Capability, P: ?Sized>(&self, capability: C, port: &'a P) -> Result<Permit<'a, P>, Denial> {
        self.permit(capability)?;
        Ok(Permit { port })
    }

    /// Admits a caller to this authority itself, as proof that a route's
    /// declared capability was checked before its handler ran.
    ///
    /// # Errors
    /// Returns `Denial` when the capability is not held.
    pub fn admit(&self, capability: CapabilityKey) -> Result<Permit<'_, Self>, Denial> {
        self.permit_key(capability)?;
        Ok(Permit { port: self })
    }

    /// Checks a capability and confines a path to the declared roots.
    ///
    /// This is the only path check in the protocol. A peer declaring no roots
    /// reaches nothing, rather than reaching everything.
    ///
    /// # Errors
    /// Returns `Denial` when the capability is not held or the path lies
    /// outside every declared root.
    pub fn permit_path<C: Capability>(&self, capability: C, path: &RelativePath) -> Result<(), Denial> {
        self.permit(capability)?;
        if self.roots.iter().any(|root| path.within(root)) {
            return Ok(());
        }
        Err(Denial {
            capability: capability.key(),
            detail: Reason::Unrooted(path.clone()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Authority, Reason};
    use crate::capability::{Capability, Grant};
    use crate::name::PeerName;
    use crate::path::RelativePath;

    #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
    enum Reach {
        Read,
        Write,
        Files,
    }

    impl Capability for Reach {
        const DOMAIN: &'static str = "sample";
        const ALL: &'static [Self] = &[Self::Read, Self::Write, Self::Files];

        fn name(&self) -> &'static str {
            match self {
                Self::Read => "read",
                Self::Write => "write",
                Self::Files => "files",
            }
        }
    }

    fn authority(capabilities: &[Reach], roots: &[&str]) -> Authority {
        Authority::new(
            PeerName::new("sample").expect("name"),
            Grant::new(capabilities.iter().copied()),
            roots
                .iter()
                .map(|root| RelativePath::new(*root).expect("root"))
                .collect(),
        )
    }

    #[test]
    fn an_ungranted_capability_is_refused_rather_than_emptied() {
        let authority = authority(&[Reach::Read], &[]);
        let denied = authority.permit(Reach::Write).expect_err("refused");
        assert_eq!(denied.capability, Reach::Write.key());
        assert_eq!(denied.detail, Reason::Ungranted);
    }

    #[test]
    fn a_port_is_unobtainable_without_its_capability() {
        let authority = authority(&[Reach::Read], &[]);
        let port: &str = "a host service";
        assert!(authority.port(Reach::Read, port).is_ok());
        assert!(authority.port(Reach::Write, port).is_err());
    }

    #[test]
    fn a_path_outside_every_root_is_refused() {
        let authority = authority(&[Reach::Files], &["logs", "state"]);
        let inside = RelativePath::new("logs/app.log").expect("path");
        let outside = RelativePath::new("etc/shadow").expect("path");

        assert!(authority.permit_path(Reach::Files, &inside).is_ok());
        let denied = authority.permit_path(Reach::Files, &outside).expect_err("refused");
        assert_eq!(denied.detail, Reason::Unrooted(outside));
    }

    #[test]
    fn declaring_no_roots_reaches_nothing() {
        let authority = authority(&[Reach::Files], &[]);
        let path = RelativePath::new("logs/app.log").expect("path");
        assert!(authority.permit_path(Reach::Files, &path).is_err());
    }

    #[test]
    fn the_capability_is_checked_before_the_path() {
        let authority = authority(&[Reach::Files], &["logs"]);
        let path = RelativePath::new("logs/app.log").expect("path");
        let denied = authority.permit_path(Reach::Write, &path).expect_err("refused");
        assert_eq!(denied.capability, Reach::Write.key());
        assert_eq!(
            denied.detail,
            Reason::Ungranted,
            "a rooted path does not imply the grant"
        );
    }

    #[test]
    fn revocation_takes_effect_immediately() {
        let mut authority = authority(&[Reach::Read, Reach::Write], &[]);
        assert!(authority.permit(Reach::Write).is_ok());

        authority.revoke(Reach::Write);

        assert!(authority.permit(Reach::Write).is_err());
        assert!(authority.permit(Reach::Read).is_ok(), "only the named one");
    }

    #[test]
    fn a_domain_reads_its_own_grant_back_out() {
        let authority = authority(&[Reach::Read], &[]);
        assert_eq!(authority.granted::<Reach>(), Grant::new([Reach::Read]));
    }
}
