//! Local mutable storage for runtime-corpus execution.

use super::{Error, workspace};
use sha2::{Digest as _, Sha256};
use std::{
    env, fs,
    io::Write as _,
    path::{Path, PathBuf},
    sync::OnceLock,
};

const ENVIRONMENT: &str = "HL_RUNTIME_WORK_ROOT";
static CONFIGURED: OnceLock<PathBuf> = OnceLock::new();

/// One root for every disposable or reusable mutable corpus artifact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WorkRoot(PathBuf);

impl WorkRoot {
    /// Resolves the configured root, or a host-local default isolated by repository identity.
    pub(crate) fn open() -> Result<Self, Error> {
        if let Some(root) = CONFIGURED.get() {
            return Ok(Self(root.clone()));
        }
        let workspace = workspace()?;
        Self::resolve(
            env::var_os(ENVIRONMENT).map(PathBuf::from),
            &workspace,
            default_parent(),
        )
    }

    /// Installs the command's resolved root before any concurrent work is scheduled.
    pub(crate) fn configure(configured: Option<PathBuf>) -> Result<Self, Error> {
        let workspace = workspace()?;
        let root = Self::resolve(configured, &workspace, default_parent())?;
        if let Some(existing) = CONFIGURED.get() {
            if existing != &root.0 {
                return Err(format!(
                    "runtime work root was already configured as {}, cannot replace it with {}",
                    existing.display(),
                    root.0.display()
                )
                .into());
            }
        } else {
            CONFIGURED
                .set(root.0.clone())
                .map_err(|_| "runtime work root initialization raced")?;
        }
        Ok(root)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }

    fn resolve(configured: Option<PathBuf>, workspace: &Path, default_parent: PathBuf) -> Result<Self, Error> {
        let root = match configured {
            Some(path) if path.is_absolute() => path,
            Some(path) => {
                return Err(format!("{ENVIRONMENT} must be an absolute path, got {}", path.display()).into());
            }
            None => default_parent
                .join("husklet-runtime")
                .join(workspace_identity(workspace)),
        };
        Ok(Self(root))
    }

    pub(crate) fn images(&self, architecture: &str) -> PathBuf {
        self.0.join("images").join(architecture)
    }

    pub(crate) fn workers(&self) -> PathBuf {
        self.0.join("workers")
    }

    pub(crate) fn failures(&self) -> PathBuf {
        self.0.join("failures")
    }

    pub(crate) fn builds(&self, application: &str, target: &str) -> PathBuf {
        self.0.join("builds").join(application).join(target)
    }

    pub(crate) fn state(&self) -> PathBuf {
        self.0.join("state")
    }

    pub(crate) fn scratch_images(&self) -> PathBuf {
        self.0.join("scratch-images")
    }

    /// Proves the selected filesystem supports the publication operations the corpus requires.
    pub(crate) fn preflight(&self) -> Result<(), Error> {
        fs::create_dir_all(&self.0)
            .map_err(|error| format!("create runtime work root {}: {error}", self.0.display()))?;
        let probe = tempfile::Builder::new()
            .prefix(".preflight-")
            .tempdir_in(&self.0)
            .map_err(|error| format!("create probe in runtime work root {}: {error}", self.0.display()))?;
        let source = probe.path().join("source");
        let renamed = probe.path().join("renamed");
        let linked = probe.path().join("linked");
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&source)
            .map_err(|error| format!("create file in runtime work root {}: {error}", self.0.display()))?;
        file.write_all(b"husklet-runtime-work-root-v1")?;
        file.sync_all()?;
        fs::hard_link(&source, &linked)
            .map_err(|error| format!("hard-link in runtime work root {}: {error}", self.0.display()))?;
        fs::rename(&source, &renamed)
            .map_err(|error| format!("rename in runtime work root {}: {error}", self.0.display()))?;
        let contents = fs::read(&renamed)?;
        if contents != b"husklet-runtime-work-root-v1" {
            return Err(format!("runtime work root {} failed its data-integrity probe", self.0.display()).into());
        }
        Ok(())
    }
}

#[cfg(unix)]
fn default_parent() -> PathBuf {
    PathBuf::from("/var/tmp")
}

#[cfg(not(unix))]
fn default_parent() -> PathBuf {
    env::temp_dir()
}

fn workspace_identity(workspace: &Path) -> String {
    let mut digest = Sha256::new();
    digest.update(workspace.as_os_str().as_encoded_bytes());
    digest.finalize()[..12]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::WorkRoot;
    use std::path::{Path, PathBuf};

    #[test]
    fn default_separates_mutable_state_from_workspace() {
        let workspace = Path::new("/mounted/repository");
        let root = WorkRoot::resolve(None, workspace, PathBuf::from("/local")).unwrap();
        assert!(root.images("arm64").starts_with("/local/husklet-runtime"));
        assert!(!root.images("arm64").starts_with(workspace));
        assert_ne!(root.workers(), root.failures());
        assert_ne!(root.builds("python", "arm64"), root.state());
    }

    #[test]
    fn configured_root_is_the_single_namespace() {
        let root = WorkRoot::resolve(
            Some(PathBuf::from("/chosen/runtime")),
            Path::new("/mounted/repository"),
            PathBuf::from("/ignored"),
        )
        .unwrap();
        for path in [
            root.images("amd64"),
            root.workers(),
            root.failures(),
            root.builds("sqlite", "amd64"),
            root.state(),
            root.scratch_images(),
        ] {
            assert!(path.starts_with("/chosen/runtime"), "{}", path.display());
        }
    }

    #[test]
    fn relative_configuration_is_rejected() {
        let error = WorkRoot::resolve(
            Some(PathBuf::from("relative")),
            Path::new("/workspace"),
            PathBuf::from("/local"),
        )
        .unwrap_err();
        assert!(error.to_string().contains("must be an absolute path"));
    }

    #[test]
    fn preflight_exercises_required_filesystem_operations() {
        let parent = tempfile::tempdir().unwrap();
        let root = WorkRoot(parent.path().join("runtime"));
        root.preflight().unwrap();
        assert!(root.0.is_dir());
    }
}
