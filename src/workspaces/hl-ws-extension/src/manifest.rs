//! What an extension declares about itself, carried as an image label.

use crate::capability::{Capability, Grant};
use crate::path::RelativePath;

/// Identity of an extension. Also the key its grant and state are stored under.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Serialize)]
#[serde(transparent)]
pub struct ExtensionName(String);

impl ExtensionName {
    pub const LIMIT: usize = 64;

    /// # Errors
    /// Returns `Invalid` when the name is empty, over-long, or contains
    /// anything outside the permitted alphabet, which keeps it usable as a
    /// directory component and a container name without further escaping.
    pub fn new(value: impl Into<String>) -> Result<Self, Invalid> {
        let value = value.into();
        if value.is_empty() || value.len() > Self::LIMIT {
            return Err(Invalid::Name);
        }
        let permitted = value
            .chars()
            .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit() || "._-".contains(character));
        if !permitted || value.starts_with(['.', '-']) {
            return Err(Invalid::Name);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ExtensionName {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de> serde::Deserialize<'de> for ExtensionName {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// When the sidecar is started.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Activation {
    /// Started with the workspace.
    Workspace,
    /// Started when the person opens it.
    #[default]
    Manual,
    /// Started when its tab is first shown.
    Tab,
}

/// What an extension asks to be given. Every value is clamped by host policy;
/// nothing here is trusted upward.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct Resources {
    pub memory_mb: u32,
    pub cpus: u32,
    pub process_count: u32,
}

impl Resources {
    pub const DEFAULT_MEMORY_MB: u32 = 256;
    pub const DEFAULT_CPUS: u32 = 1;
    pub const DEFAULT_PROCESS_COUNT: u32 = 128;
    pub const CEILING_MEMORY_MB: u32 = 1024;
    pub const CEILING_CPUS: u32 = 2;
    pub const CEILING_PROCESS_COUNT: u32 = 512;

    /// Narrows a request to what the host will actually grant.
    #[must_use]
    pub const fn clamp(self) -> Self {
        Self {
            memory_mb: Self::bound(self.memory_mb, Self::CEILING_MEMORY_MB),
            cpus: Self::bound(self.cpus, Self::CEILING_CPUS),
            process_count: Self::bound(self.process_count, Self::CEILING_PROCESS_COUNT),
        }
    }

    /// Whether a request exceeded the ceiling, so install can say so instead of
    /// silently giving less than was asked for.
    #[must_use]
    pub const fn exceeds_ceiling(self) -> bool {
        self.memory_mb > Self::CEILING_MEMORY_MB
            || self.cpus > Self::CEILING_CPUS
            || self.process_count > Self::CEILING_PROCESS_COUNT
    }

    const fn bound(value: u32, ceiling: u32) -> u32 {
        if value == 0 {
            return ceiling;
        }
        if value > ceiling {
            ceiling
        } else {
            value
        }
    }
}

impl Default for Resources {
    fn default() -> Self {
        Self {
            memory_mb: Self::DEFAULT_MEMORY_MB,
            cpus: Self::DEFAULT_CPUS,
            process_count: Self::DEFAULT_PROCESS_COUNT,
        }
    }
}

/// How an extension presents itself when it owns a tab.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct Presentation {
    pub tab_title: String,
    #[serde(default)]
    pub icon: Option<String>,
}

/// Everything an extension declares, parsed from its image label.
///
/// Unknown fields are refused rather than ignored: an extension asking for
/// something this host does not model must fail loudly, not silently receive
/// less than it expects.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub name: ExtensionName,
    pub display_name: String,
    pub version: String,
    pub protocol: u32,
    pub capabilities: Grant,
    #[serde(default)]
    pub entrypoint: Option<Vec<String>>,
    #[serde(default)]
    pub activation: Activation,
    #[serde(default)]
    pub interface: Option<Presentation>,
    #[serde(default)]
    pub resources: Resources,
    #[serde(default)]
    pub filesystem_roots: Vec<RelativePath>,
}

impl Manifest {
    /// Image label carrying the serialized manifest.
    pub const LABEL: &'static str = "husklet.extension.manifest";
    /// Image label carrying the protocol version alone, so an incompatible
    /// extension is refused without parsing the manifest at all.
    pub const PROTOCOL_LABEL: &'static str = "husklet.extension.protocol";
    /// Largest serialized manifest accepted.
    pub const LIMIT: usize = 64 * 1024;

    /// # Errors
    /// Returns `Invalid` when the label is over-long, is not valid JSON, names
    /// an unknown field, or declares a protocol this host does not speak.
    pub fn parse(label: &str, protocol: u32) -> Result<Self, Invalid> {
        if label.len() > Self::LIMIT {
            return Err(Invalid::TooLong(label.len()));
        }
        let manifest: Self = serde_json::from_str(label).map_err(|error| Invalid::Malformed(error.to_string()))?;
        if manifest.protocol != protocol {
            return Err(Invalid::Protocol {
                declared: manifest.protocol,
                supported: protocol,
            });
        }
        if manifest.interface.is_some() && !manifest.capabilities.holds(Capability::Interface) {
            return Err(Invalid::Undeclared(Capability::Interface));
        }
        if !manifest.filesystem_roots.is_empty()
            && !manifest.capabilities.holds(Capability::FilesystemRead)
            && !manifest.capabilities.holds(Capability::FilesystemWrite)
        {
            return Err(Invalid::Undeclared(Capability::FilesystemRead));
        }
        Ok(manifest)
    }

    /// # Errors
    /// Returns `Invalid::Malformed` when the manifest cannot be serialized.
    pub fn label(&self) -> Result<String, Invalid> {
        serde_json::to_string(self).map_err(|error| Invalid::Malformed(error.to_string()))
    }
}

/// Why a manifest was refused.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Invalid {
    Name,
    TooLong(usize),
    Malformed(String),
    Protocol { declared: u32, supported: u32 },
    Undeclared(Capability),
}

impl std::fmt::Display for Invalid {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Name => write!(
                formatter,
                "extension name must be 1 to {} characters of a-z, 0-9, dot, underscore, or hyphen",
                ExtensionName::LIMIT
            ),
            Self::TooLong(length) => {
                write!(
                    formatter,
                    "manifest is {length} bytes, above the {} limit",
                    Manifest::LIMIT
                )
            }
            Self::Malformed(detail) => write!(formatter, "malformed manifest: {detail}"),
            Self::Protocol { declared, supported } => {
                write!(
                    formatter,
                    "extension speaks protocol {declared}, this host speaks {supported}"
                )
            }
            Self::Undeclared(capability) => {
                write!(formatter, "manifest uses {} without declaring it", capability.as_str())
            }
        }
    }
}

impl std::error::Error for Invalid {}
