//! What an extension declares about itself, carried as an image label.

use hl_rpc::{Rejection, RelativePath};

use crate::capability::{Capability, Grant};

/// Identity of an extension. Also the key its grant and state are stored under.
pub type ExtensionName = hl_rpc::PeerName;

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

/// One named view an extension offers to a terminal pane chooser.
///
/// The identifier is stable program identity; the title and icon are only
/// presentation. A provider does not grant another interface capability: it
/// is discoverable only when the manifest already requests `interface`.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct PaneProvider {
    pub id: ExtensionName,
    pub title: String,
    #[serde(default)]
    pub icon: Option<String>,
}

/// Host event sent when a person chooses one of an extension's pane providers.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct PaneSelection {
    pub pane_provider: ExtensionName,
    /// Stable workspace slot mounting this provider.
    ///
    /// Provider selection is pane-addressed: without this identity an
    /// extension cannot distinguish two simultaneous mounts of the same
    /// provider, nor target their subsequent render streams independently.
    pub slot: String,
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
    /// Named views this extension makes available in terminal panes.
    #[serde(default)]
    pub pane_providers: Vec<PaneProvider>,
    #[serde(default)]
    pub resources: Resources,
    #[serde(default)]
    pub filesystem_roots: Vec<RelativePath>,
}

impl Manifest {
    /// Image label naming where the manifest lives inside the image.
    ///
    /// A path rather than the manifest itself: an extension author edits a
    /// file that lives beside their source and is reviewed with it, instead of
    /// a document folded into a build argument where nothing can read it.
    pub const LABEL: &'static str = "husklet.extension.manifest";
    /// Where an extension puts its manifest unless its label says otherwise.
    pub const DEFAULT_PATH: &'static str = "/etc/husklet/extension.toml";
    /// Image label carrying the protocol version alone, so an incompatible
    /// extension is refused without parsing the manifest at all.
    pub const PROTOCOL_LABEL: &'static str = "husklet.extension.protocol";
    /// Largest manifest document accepted.
    pub const LIMIT: usize = 64 * 1024;

    /// Reads a manifest from the document an image carries.
    ///
    /// # Errors
    /// Returns `Invalid` when the document is over-long, is not valid TOML,
    /// names an unknown field, or declares a protocol this host does not
    /// speak.
    pub fn parse(document: &str, protocol: u32) -> Result<Self, Invalid> {
        if document.len() > Self::LIMIT {
            return Err(Invalid::TooLong(document.len()));
        }
        let manifest: Self = toml::from_str(document).map_err(|error| Invalid::Malformed(error.to_string()))?;
        if manifest.protocol != protocol {
            return Err(Invalid::Protocol {
                declared: manifest.protocol,
                supported: protocol,
            });
        }
        if manifest.interface.is_some() && !manifest.capabilities.holds(Capability::Interface) {
            return Err(Invalid::Undeclared(Capability::Interface));
        }
        if !manifest.pane_providers.is_empty() && !manifest.capabilities.holds(Capability::Interface) {
            return Err(Invalid::Undeclared(Capability::Interface));
        }
        let mut providers = std::collections::BTreeSet::new();
        if manifest
            .pane_providers
            .iter()
            .any(|provider| provider.title.trim().is_empty() || !providers.insert(provider.id.clone()))
        {
            return Err(Invalid::PaneProviders);
        }
        if !manifest.filesystem_roots.is_empty()
            && !manifest.capabilities.holds(Capability::FilesystemRead)
            && !manifest.capabilities.holds(Capability::FilesystemWrite)
        {
            return Err(Invalid::Undeclared(Capability::FilesystemRead));
        }
        Ok(manifest)
    }

    /// Writes the manifest as the document an image should carry.
    ///
    /// # Errors
    /// Returns `Invalid::Malformed` when the manifest cannot be serialized.
    pub fn document(&self) -> Result<String, Invalid> {
        toml::to_string_pretty(self).map_err(|error| Invalid::Malformed(error.to_string()))
    }
}

impl From<Rejection> for Invalid {
    fn from(_rejected: Rejection) -> Self {
        Self::Name
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
    PaneProviders,
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
            Self::PaneProviders => formatter.write_str("pane provider ids must be unique and titles must not be empty"),
        }
    }
}

impl std::error::Error for Invalid {}
