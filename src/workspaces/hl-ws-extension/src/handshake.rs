//! Version negotiation, mirroring the workspace domain's published protocol.
//!
//! The host speaks first and states the grant, so an extension knows what it
//! actually holds before it asks for anything and can degrade rather than
//! collect refusals.

use crate::capability::Grant;
use crate::manifest::ExtensionName;

/// The protocol this host speaks.
pub const PROTOCOL: u32 = 1;

/// Bounds the host advertises so an extension can size its own work.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub payload_limit: usize,
    pub channel_limit: usize,
    pub credit: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            payload_limit: crate::frame::Frame::PAYLOAD_LIMIT,
            channel_limit: crate::channel::Channels::LIMIT,
            credit: crate::channel::Channels::CREDIT,
        }
    }
}

/// The host's opening frame.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct Welcome {
    pub protocol: u32,
    pub host: String,
    pub workspace: String,
    pub extension: ExtensionName,
    pub granted: Grant,
    pub limits: Limits,
}

/// The extension's reply.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct Hello {
    pub protocol: u32,
    /// Logged for diagnosis only. The socket is the credential; this is never
    /// trusted to name which extension is connected.
    pub name: ExtensionName,
    #[serde(default)]
    pub features: Vec<String>,
}

/// The outcome of comparing two protocol versions.
///
/// `Unknown` exists because a connection that has not spoken yet is not the
/// same as one that spoke a wrong version, and must not be reported as one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Compatibility {
    Compatible,
    Mismatched { declared: u32, supported: u32 },
    Unknown,
}

impl Compatibility {
    /// Compares a declared version against this host's.
    #[must_use]
    pub const fn of(declared: u32) -> Self {
        if declared == PROTOCOL {
            Self::Compatible
        } else {
            Self::Mismatched {
                declared,
                supported: PROTOCOL,
            }
        }
    }

    #[must_use]
    pub const fn is_compatible(self) -> bool {
        matches!(self, Self::Compatible)
    }
}

impl std::fmt::Display for Compatibility {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Compatible => write!(formatter, "protocol {PROTOCOL}"),
            Self::Mismatched { declared, supported } => {
                write!(
                    formatter,
                    "extension speaks protocol {declared}, this host speaks {supported}"
                )
            }
            Self::Unknown => write!(formatter, "protocol not yet declared"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Compatibility, PROTOCOL};

    #[test]
    fn a_matching_version_is_compatible() {
        assert!(Compatibility::of(PROTOCOL).is_compatible());
    }

    #[test]
    fn a_mismatch_names_both_versions() {
        let outcome = Compatibility::of(PROTOCOL + 1);
        assert_eq!(
            outcome,
            Compatibility::Mismatched {
                declared: PROTOCOL + 1,
                supported: PROTOCOL
            }
        );
        assert!(outcome.to_string().contains("this host speaks"));
    }

    #[test]
    fn silence_is_distinct_from_a_wrong_version() {
        assert_ne!(Compatibility::Unknown, Compatibility::of(PROTOCOL + 1));
        assert!(!Compatibility::Unknown.is_compatible());
    }
}
