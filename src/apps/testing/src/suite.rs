use clap::ValueEnum;
use serde::Deserialize;
use std::path::{Path, PathBuf};

pub(crate) type Error = Box<dyn std::error::Error>;

const MAX_MANIFESTS: usize = 1024;

pub(crate) struct Manifest {
    pub(crate) directory: PathBuf,
    pub(crate) definition: PathBuf,
}

/// Discovers manifests owned by direct category directories beneath `root`.
///
/// Symlinked and nested directories are deliberately excluded: a category owns
/// its `test.yaml` and adjacent source, golden, and oracle evidence.
pub(crate) fn manifests(root: &Path, selected: Option<&str>) -> Result<Vec<Manifest>, Error> {
    let mut entries = std::fs::read_dir(root)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    let mut manifests = Vec::new();
    for entry in entries {
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| format!("category name beneath {} is not UTF-8", root.display()))?;
        if selected.is_some_and(|candidate| candidate != name) {
            continue;
        }
        let directory = entry.path();
        let definition = directory.join("test.yaml");
        if definition
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.file_type().is_file())
        {
            manifests.push(Manifest { directory, definition });
            if manifests.len() > MAX_MANIFESTS {
                return Err(format!("{} contains more than {MAX_MANIFESTS} test manifests", root.display()).into());
            }
        }
    }
    Ok(manifests)
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Target {
    Arm64,
    Amd64,
}

impl Target {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Arm64 => "arm64",
            Self::Amd64 => "amd64",
        }
    }

    pub(crate) const fn guest(self) -> hl_container::Guest {
        match self {
            Self::Arm64 => hl_container::Guest::Aarch64,
            Self::Amd64 => hl_container::Guest::X86_64,
        }
    }

    pub(crate) fn platform(self) -> hl_images::Platform {
        match self {
            Self::Arm64 => hl_images::Platform::linux_arm64(),
            Self::Amd64 => hl_images::Platform::linux_amd64(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Execution {
    #[serde(default)]
    native: bool,
    #[serde(default)]
    diagnostics: bool,
}

impl Execution {
    pub(crate) fn container(self) -> Result<hl_container::Execution, Error> {
        if self.diagnostics && !self.native {
            return Err("native diagnostics require native execution".into());
        }
        Ok(if self.native {
            hl_container::Execution::native(self.diagnostics)
        } else {
            hl_container::Execution::default()
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Commands {
    arm64: String,
    amd64: String,
}

impl Commands {
    pub(crate) fn for_target(&self, target: Target) -> &str {
        match target {
            Target::Arm64 => &self.arm64,
            Target::Amd64 => &self.amd64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Commands, Execution, Target, manifests};
    use clap::ValueEnum;
    use std::fs;

    #[test]
    fn targets_preserve_guest_and_platform_routing() {
        assert!(Target::from_str("x86", false).is_err());
        assert_eq!(Target::from_str("arm64", false), Ok(Target::Arm64));
        assert_eq!(Target::from_str("amd64", false), Ok(Target::Amd64));
        assert_eq!(Target::Arm64.guest(), hl_container::Guest::Aarch64);
        assert_eq!(Target::Amd64.guest(), hl_container::Guest::X86_64);
        assert_eq!(Target::Arm64.platform(), hl_images::Platform::linux_arm64());
        assert_eq!(Target::Amd64.platform(), hl_images::Platform::linux_amd64());
    }

    #[test]
    fn execution_preserves_native_diagnostic_validation() {
        let enabled: Execution = serde_yaml::from_str("native: true\ndiagnostics: true\n").unwrap();
        let enabled = enabled.container().unwrap();
        assert!(enabled.is_native());
        assert!(enabled.diagnostics());
        let invalid: Execution = serde_yaml::from_str("diagnostics: true\n").unwrap();
        assert!(invalid.container().is_err());
    }

    #[test]
    fn commands_select_the_existing_isa_spelling() {
        let commands: Commands = serde_yaml::from_str("arm64: arm-cc\namd64: amd-cc\n").unwrap();
        assert_eq!(commands.for_target(Target::Arm64), "arm-cc");
        assert_eq!(commands.for_target(Target::Amd64), "amd-cc");
    }

    #[test]
    fn manifest_discovery_is_sorted_and_owned_by_direct_categories() {
        let root = tempfile::tempdir().unwrap();
        for category in ["zeta", "alpha"] {
            let directory = root.path().join(category);
            fs::create_dir(&directory).unwrap();
            fs::write(directory.join("test.yaml"), "cases: []\n").unwrap();
        }
        fs::create_dir(root.path().join("wrapper")).unwrap();
        fs::create_dir_all(root.path().join("nested/category")).unwrap();
        fs::write(root.path().join("nested/category/test.yaml"), "cases: []\n").unwrap();

        let found = manifests(root.path(), None).unwrap();
        let names = found
            .iter()
            .map(|manifest| manifest.directory.file_name().unwrap().to_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(names, ["alpha", "zeta"]);
        assert!(
            found
                .iter()
                .all(|manifest| manifest.definition.parent() == Some(manifest.directory.as_path()))
        );
        assert_eq!(manifests(root.path(), Some("zeta")).unwrap().len(), 1);
        assert!(manifests(root.path(), Some("wrapper")).unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn manifest_discovery_does_not_follow_wrapper_symlinks() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        fs::write(external.path().join("test.yaml"), "cases: []\n").unwrap();
        symlink(external.path(), root.path().join("borrowed")).unwrap();

        assert!(manifests(root.path(), None).unwrap().is_empty());
    }
}
