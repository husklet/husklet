//! What an extension declares about itself, carried as an image label.

use hl_rpc::{Rejection, RelativePath};

use crate::capability::{Capability, Grant};

/// Workspace paths an extension may read or change.
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct FilesystemGrant {
    #[serde(default)]
    pub read: Vec<FilesystemSelector>,
    #[serde(default)]
    pub write: Vec<FilesystemSelector>,
    #[serde(default)]
    pub create: Vec<FilesystemSelector>,
    #[serde(default)]
    pub delete: Vec<FilesystemSelector>,
    #[serde(default)]
    pub rename: Vec<FilesystemSelector>,
}

/// One exact workspace path or an explicitly selected directory subtree.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum FilesystemSelector {
    Exact { exact: RelativePath },
    Subtree { subtree: RelativePath },
}

impl FilesystemSelector {
    #[must_use]
    pub fn permits(&self, path: &RelativePath) -> bool {
        match self {
            Self::Exact { exact } => path == exact,
            Self::Subtree { subtree } => path.within(subtree),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceEnvironmentGrant {
    #[serde(default)]
    pub read: Vec<WorkspaceEnvironmentSelector>,
    #[serde(default)]
    pub write: Vec<WorkspaceEnvironmentSelector>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize)]
#[serde(untagged)]
pub enum WorkspaceEnvironmentSelector {
    Exact { workspace: String, name: String },
    All { all: bool },
}

impl WorkspaceEnvironmentGrant {
    pub const LIMIT: usize = 128;
    pub const BYTES: usize = 16 * 1024;

    #[must_use]
    pub fn permits_read(&self, workspace: &str, name: &str) -> bool {
        permits(&self.read, workspace, name)
    }

    #[must_use]
    pub fn permits_write(&self, workspace: &str, name: &str) -> bool {
        permits(&self.write, workspace, name)
    }

    #[must_use]
    pub fn intersect(&self, consented: &Self) -> Self {
        Self {
            read: environment_intersection(&self.read, &consented.read),
            write: environment_intersection(&self.write, &consented.write),
        }
    }

    fn validate(&self, built_in_top: bool) -> Result<(), Invalid> {
        let bytes = validate_environment_selectors(&self.read, built_in_top)?
            .checked_add(validate_environment_selectors(&self.write, built_in_top)?)
            .ok_or(Invalid::WorkspaceEnvironment)?;
        if self.read.len() + self.write.len() > Self::LIMIT || bytes > Self::BYTES {
            return Err(Invalid::WorkspaceEnvironment);
        }
        Ok(())
    }
}

fn permits(selectors: &[WorkspaceEnvironmentSelector], workspace: &str, name: &str) -> bool {
    selectors.iter().any(|selector| match selector {
        WorkspaceEnvironmentSelector::Exact {
            workspace: selected,
            name: selected_name,
        } => selected == workspace && selected_name == name,
        WorkspaceEnvironmentSelector::All { all } => *all,
    })
}

fn environment_intersection(
    requested: &[WorkspaceEnvironmentSelector],
    consented: &[WorkspaceEnvironmentSelector],
) -> Vec<WorkspaceEnvironmentSelector> {
    let all = consented.contains(&WorkspaceEnvironmentSelector::All { all: true });
    requested
        .iter()
        .filter(|item| all || consented.contains(item))
        .cloned()
        .collect()
}

fn validate_environment_selectors(
    selectors: &[WorkspaceEnvironmentSelector],
    built_in_top: bool,
) -> Result<usize, Invalid> {
    let mut bytes = 0usize;
    for selector in selectors {
        match selector {
            WorkspaceEnvironmentSelector::Exact { workspace, name } => {
                let valid_name = name.bytes().enumerate().all(|(index, byte)| {
                    byte == b'_' || byte.is_ascii_alphabetic() || index > 0 && byte.is_ascii_digit()
                });
                if workspace.is_empty() || workspace.chars().any(char::is_control) || name.is_empty() || !valid_name {
                    return Err(Invalid::WorkspaceEnvironment);
                }
                bytes = bytes
                    .checked_add(workspace.len() + name.len())
                    .ok_or(Invalid::WorkspaceEnvironment)?;
            }
            WorkspaceEnvironmentSelector::All { all: true } if built_in_top => {}
            WorkspaceEnvironmentSelector::All { .. } => return Err(Invalid::WorkspaceEnvironment),
        }
    }
    if selectors.iter().collect::<std::collections::BTreeSet<_>>().len() != selectors.len() {
        return Err(Invalid::WorkspaceEnvironment);
    }
    Ok(bytes)
}

impl FilesystemGrant {
    pub const ROOT_LIMIT: usize = 128;

    #[must_use]
    pub fn intersect(&self, consented: &Self) -> Self {
        Self {
            read: self
                .read
                .iter()
                .filter(|root| consented.read.contains(root))
                .cloned()
                .collect(),
            write: self
                .write
                .iter()
                .filter(|root| consented.write.contains(root))
                .cloned()
                .collect(),
            create: intersection(&self.create, &consented.create),
            delete: intersection(&self.delete, &consented.delete),
            rename: intersection(&self.rename, &consented.rename),
        }
    }

    fn validate(&self) -> Result<(), Invalid> {
        if self.read.len() + self.write.len() + self.create.len() + self.delete.len() + self.rename.len()
            > Self::ROOT_LIMIT
        {
            return Err(Invalid::FilesystemRoots);
        }
        if [&self.read, &self.write, &self.create, &self.delete, &self.rename]
            .iter()
            .any(|roots| roots.iter().collect::<std::collections::BTreeSet<_>>().len() != roots.len())
        {
            return Err(Invalid::FilesystemRoots);
        }
        Ok(())
    }
}

fn intersection(requested: &[FilesystemSelector], consented: &[FilesystemSelector]) -> Vec<FilesystemSelector> {
    requested
        .iter()
        .filter(|root| consented.contains(root))
        .cloned()
        .collect()
}

/// One exact container an extension asks to see, or an explicit workspace-wide selector.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Serialize)]
#[serde(untagged)]
pub enum ContainerSelector {
    Id { id: String },
    Name { name: String },
    All { all: bool },
}

impl<'de> serde::Deserialize<'de> for ContainerSelector {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            id: Option<String>,
            name: Option<String>,
            all: Option<bool>,
        }
        let raw = <Raw as serde::Deserialize>::deserialize(deserializer)?;
        match (raw.id, raw.name, raw.all) {
            (Some(id), None, None) => Ok(Self::Id { id }),
            (None, Some(name), None) => Ok(Self::Name { name }),
            (None, None, Some(all)) => Ok(Self::All { all }),
            _ => Err(serde::de::Error::custom(
                "a container selector must contain exactly one of id, name, or all",
            )),
        }
    }
}

/// Container resource authority, independent from the verb capabilities.
///
/// An omitted value is empty. Workspace-wide authority therefore requires an
/// explicit `[{ all = true }]`, and creation is separately consented.
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContainerGrant {
    #[serde(default)]
    pub selectors: Vec<ContainerSelector>,
    #[serde(default)]
    pub create: bool,
}

impl ContainerGrant {
    pub const SELECTOR_LIMIT: usize = 128;

    #[must_use]
    pub fn intersect(&self, consented: &Self) -> Self {
        let selectors = self
            .selectors
            .iter()
            .filter(|selector| consented.selectors.contains(selector))
            .cloned()
            .collect();
        Self {
            selectors,
            create: self.create && consented.create,
        }
    }

    #[must_use]
    pub fn all(&self) -> bool {
        self.selectors
            .iter()
            .any(|selector| matches!(selector, ContainerSelector::All { all: true }))
    }

    fn validate(&self) -> Result<(), Invalid> {
        if self.selectors.len() > Self::SELECTOR_LIMIT {
            return Err(Invalid::ContainerSelectors);
        }
        let mut unique = std::collections::BTreeSet::new();
        for selector in &self.selectors {
            let valid = match selector {
                ContainerSelector::Id { id } => {
                    matches!(id.len(), 32 | 64) && id.bytes().all(|byte| byte.is_ascii_hexdigit())
                }
                ContainerSelector::Name { name } => {
                    !name.is_empty()
                        && name.len() <= 128
                        && name
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
                }
                ContainerSelector::All { all } => *all,
            };
            if !valid || !unique.insert(selector) {
                return Err(Invalid::ContainerSelectors);
            }
        }
        Ok(())
    }
}

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
    /// Exact container resources requested. Omission intentionally means none.
    #[serde(default)]
    pub containers: ContainerGrant,
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
    /// Path authority is separate by verb; writable paths are not implicitly readable.
    #[serde(default)]
    pub filesystem: FilesystemGrant,
    #[serde(default)]
    pub workspace_environment: WorkspaceEnvironmentGrant,
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
        if !manifest.filesystem.read.is_empty() && !manifest.capabilities.holds(Capability::FilesystemRead) {
            return Err(Invalid::Undeclared(Capability::FilesystemRead));
        }
        if [
            &manifest.filesystem.write,
            &manifest.filesystem.create,
            &manifest.filesystem.delete,
            &manifest.filesystem.rename,
        ]
        .iter()
        .any(|roots| !roots.is_empty())
            && !manifest.capabilities.holds(Capability::FilesystemWrite)
        {
            return Err(Invalid::Undeclared(Capability::FilesystemWrite));
        }
        manifest.filesystem.validate()?;
        if !manifest.workspace_environment.read.is_empty()
            && !manifest.capabilities.holds(Capability::WorkspaceEnvironmentRead)
        {
            return Err(Invalid::Undeclared(Capability::WorkspaceEnvironmentRead));
        }
        if !manifest.workspace_environment.write.is_empty()
            && !manifest.capabilities.holds(Capability::WorkspaceEnvironmentWrite)
        {
            return Err(Invalid::Undeclared(Capability::WorkspaceEnvironmentWrite));
        }
        manifest
            .workspace_environment
            .validate(manifest.name.to_string() == "top")?;
        manifest.containers.validate()?;
        if (!manifest.containers.selectors.is_empty() || manifest.containers.create)
            && !manifest.capabilities.holds(Capability::ContainerRead)
            && !manifest.capabilities.holds(Capability::ContainerControl)
            && !manifest.capabilities.holds(Capability::ContainerAttach)
        {
            return Err(Invalid::Undeclared(Capability::ContainerRead));
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
    ContainerSelectors,
    FilesystemRoots,
    WorkspaceEnvironment,
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
            Self::ContainerSelectors => formatter.write_str("container selectors must contain at most 128 unique exact ids, names, or one explicit `{ all = true }`"),
            Self::FilesystemRoots => formatter.write_str("filesystem scopes must contain at most 128 unique read or write roots"),
            Self::WorkspaceEnvironment => formatter.write_str("workspace environment scopes must be bounded unique exact pairs; all is reserved for top"),
        }
    }
}

impl std::error::Error for Invalid {}

#[cfg(test)]
mod tests {
    use super::{ContainerGrant, ContainerSelector, FilesystemGrant, FilesystemSelector, Manifest};
    use crate::{RelativePath, PROTOCOL};

    fn document(extra: &str) -> String {
        format!("name = \"sample\"\ndisplay_name = \"Sample\"\nversion = \"1\"\nprotocol = {PROTOCOL}\ncapabilities = [\"containers:read\"]\n{extra}")
    }

    #[test]
    fn omitted_container_scope_is_empty_not_wildcard() {
        let manifest = Manifest::parse(&document(""), PROTOCOL).unwrap();
        assert_eq!(manifest.containers, ContainerGrant::default());
        assert!(!manifest.containers.all());
    }

    #[test]
    fn wildcard_and_creation_are_both_explicit() {
        let manifest = Manifest::parse(
            &document("[containers]\nselectors = [{ all = true }]\ncreate = true\n"),
            PROTOCOL,
        )
        .unwrap();
        assert_eq!(
            manifest.containers.selectors,
            vec![ContainerSelector::All { all: true }]
        );
        assert!(manifest.containers.create);
    }

    #[test]
    fn false_wildcards_duplicates_and_malformed_identities_fail_closed() {
        for scope in [
            "[containers]\nselectors = [{ all = false }]\n",
            "[containers]\nselectors = [{ name = \"api\" }, { name = \"api\" }]\n",
            "[containers]\nselectors = [{ id = \"short\" }]\n",
            "[containers]\nselectors = [{ id = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\", name = \"api\" }]\n",
        ] {
            assert!(Manifest::parse(&document(scope), PROTOCOL).is_err(), "accepted {scope}");
        }
    }

    #[test]
    fn filesystem_consent_intersects_read_and_write_roots_independently() {
        let exact = |path| FilesystemSelector::Exact {
            exact: RelativePath::new(path).unwrap(),
        };
        let subtree = |path| FilesystemSelector::Subtree {
            subtree: RelativePath::new(path).unwrap(),
        };
        let requested = FilesystemGrant {
            read: vec![subtree("src"), exact("README.md")],
            write: vec![exact("workspace.toml")],
            ..FilesystemGrant::default()
        };
        let consented = FilesystemGrant {
            read: vec![subtree("src")],
            write: vec![exact("README.md"), exact("workspace.toml")],
            ..FilesystemGrant::default()
        };
        assert_eq!(
            requested.intersect(&consented),
            FilesystemGrant {
                read: vec![subtree("src")],
                write: vec![exact("workspace.toml")],
                ..FilesystemGrant::default()
            }
        );
    }
}
