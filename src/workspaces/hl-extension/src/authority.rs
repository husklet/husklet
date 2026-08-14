//! The single place a capability is checked.
//!
//! Ports are reachable only through this type, so a dispatcher cannot obtain a
//! handle to a host service without naming the capability it needs. A missing
//! check is therefore a compile error rather than something review has to catch.

use crate::capability::{Capability, Grant};
use crate::manifest::ExtensionName;
use crate::path::RelativePath;

/// A refused operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Denial {
    pub capability: Capability,
    pub detail: Reason,
}

/// Why an operation was refused.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Reason {
    /// The extension does not hold the capability.
    Ungranted,
    /// The path lies outside every declared root.
    Unrooted(RelativePath),
}

impl std::fmt::Display for Denial {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.detail {
            Reason::Ungranted => write!(formatter, "{} is not granted", self.capability.as_str()),
            Reason::Unrooted(path) => write!(
                formatter,
                "{path} is outside the roots {} was granted",
                self.capability.as_str()
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

/// What one connected extension is allowed to do.
#[derive(Debug)]
pub struct Authority {
    extension: ExtensionName,
    granted: Grant,
    roots: Vec<RelativePath>,
}

impl Authority {
    #[must_use]
    pub fn new(extension: ExtensionName, granted: Grant, roots: Vec<RelativePath>) -> Self {
        Self {
            extension,
            granted,
            roots,
        }
    }

    #[must_use]
    pub const fn extension(&self) -> &ExtensionName {
        &self.extension
    }

    #[must_use]
    pub const fn granted(&self) -> &Grant {
        &self.granted
    }

    #[must_use]
    pub fn roots(&self) -> &[RelativePath] {
        &self.roots
    }

    /// Narrows the grant. A revoked capability takes effect on the next check,
    /// including for a subscription established while it was still held.
    pub fn revoke(&mut self, capability: Capability) {
        self.granted = Grant::new(self.granted.iter().filter(|held| *held != capability));
    }

    /// # Errors
    /// Returns `Denial` when the capability is not held.
    pub fn permit(&self, capability: Capability) -> Result<(), Denial> {
        if self.granted.holds(capability) {
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
    pub fn port<'a, P: ?Sized>(&self, capability: Capability, port: &'a P) -> Result<Permit<'a, P>, Denial> {
        self.permit(capability)?;
        Ok(Permit { port })
    }

    /// Checks a capability and confines a path to the declared roots.
    ///
    /// This is the only path check in the subsystem. An extension declaring no
    /// roots reaches nothing, rather than reaching everything.
    ///
    /// # Errors
    /// Returns `Denial` when the capability is not held or the path lies
    /// outside every declared root.
    pub fn permit_path(&self, capability: Capability, path: &RelativePath) -> Result<(), Denial> {
        self.permit(capability)?;
        if self.roots.iter().any(|root| path.within(root)) {
            return Ok(());
        }
        Err(Denial {
            capability,
            detail: Reason::Unrooted(path.clone()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Authority, Capability, Grant, Reason};
    use crate::manifest::ExtensionName;
    use crate::path::RelativePath;

    fn authority(capabilities: &[Capability], roots: &[&str]) -> Authority {
        Authority::new(
            ExtensionName::new("sample").expect("name"),
            Grant::new(capabilities.iter().copied()),
            roots
                .iter()
                .map(|root| RelativePath::new(*root).expect("root"))
                .collect(),
        )
    }

    #[test]
    fn an_ungranted_capability_is_refused_rather_than_emptied() {
        let authority = authority(&[Capability::ContainerRead], &[]);
        let denied = authority.permit(Capability::ContainerControl).expect_err("refused");
        assert_eq!(denied.capability, Capability::ContainerControl);
        assert_eq!(denied.detail, Reason::Ungranted);
    }

    #[test]
    fn a_port_is_unobtainable_without_its_capability() {
        let authority = authority(&[Capability::ContainerRead], &[]);
        let port: &str = "container service";
        assert!(authority.port(Capability::ContainerRead, port).is_ok());
        assert!(authority.port(Capability::ContainerControl, port).is_err());
    }

    #[test]
    fn a_path_outside_every_root_is_refused() {
        let authority = authority(&[Capability::FilesystemRead], &["logs", "state"]);
        let inside = RelativePath::new("logs/app.log").expect("path");
        let outside = RelativePath::new("etc/shadow").expect("path");

        assert!(authority.permit_path(Capability::FilesystemRead, &inside).is_ok());
        let denied = authority
            .permit_path(Capability::FilesystemRead, &outside)
            .expect_err("refused");
        assert_eq!(denied.detail, Reason::Unrooted(outside));
    }

    #[test]
    fn declaring_no_roots_reaches_nothing() {
        let authority = authority(&[Capability::FilesystemRead], &[]);
        let path = RelativePath::new("logs/app.log").expect("path");
        assert!(authority.permit_path(Capability::FilesystemRead, &path).is_err());
    }

    #[test]
    fn the_capability_is_checked_before_the_path() {
        let authority = authority(&[Capability::FilesystemRead], &["logs"]);
        let path = RelativePath::new("logs/app.log").expect("path");
        let denied = authority
            .permit_path(Capability::FilesystemWrite, &path)
            .expect_err("refused");
        assert_eq!(denied.capability, Capability::FilesystemWrite);
        assert_eq!(
            denied.detail,
            Reason::Ungranted,
            "a rooted path does not imply the grant"
        );
    }

    #[test]
    fn revocation_takes_effect_immediately() {
        let mut authority = authority(&[Capability::ContainerRead, Capability::ContainerControl], &[]);
        assert!(authority.permit(Capability::ContainerControl).is_ok());

        authority.revoke(Capability::ContainerControl);

        assert!(authority.permit(Capability::ContainerControl).is_err());
        assert!(
            authority.permit(Capability::ContainerRead).is_ok(),
            "only the named one"
        );
    }
}
