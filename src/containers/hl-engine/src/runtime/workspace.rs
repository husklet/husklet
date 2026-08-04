use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::engine::{EngineError, Workspace, WorkspaceId};

static NEXT: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
pub enum Input {
    File {
        source: PathBuf,
        relative: PathBuf,
        executable: bool,
    },
    Directory {
        source: Option<PathBuf>,
        relative: PathBuf,
    },
    Symlink {
        relative: PathBuf,
        target: PathBuf,
    },
}

impl Input {
    fn prefixed(self, prefix: &Path) -> Self {
        match self {
            Self::File {
                source,
                relative,
                executable,
            } => Self::File {
                source,
                relative: prefix.join(relative),
                executable,
            },
            Self::Directory { source, relative } => Self::Directory {
                source,
                relative: prefix.join(relative),
            },
            Self::Symlink { relative, target } => Self::Symlink {
                relative: prefix.join(relative),
                target,
            },
        }
    }
}

#[derive(Clone, Debug)]
pub struct Rootfs {
    entry: PathBuf,
    inputs: Vec<Input>,
}

#[derive(Clone, Debug)]
pub struct BaseSystem {
    files: Vec<(PathBuf, Box<[u8]>)>,
}

impl BaseSystem {
    #[must_use]
    pub fn empty() -> Self {
        Self { files: Vec::new() }
    }

    #[must_use]
    pub fn linux() -> Self {
        Self::empty()
            .with_file("etc/passwd", b"root:x:0:0:root:/root:/bin/sh\n".to_vec())
            .with_file("etc/group", b"root:x:0:\n".to_vec())
    }

    #[must_use]
    pub fn with_file(mut self, relative: impl Into<PathBuf>, contents: impl Into<Vec<u8>>) -> Self {
        self.files.push((relative.into(), contents.into().into_boxed_slice()));
        self
    }
}

impl Rootfs {
    #[must_use]
    pub fn scratch(entry: impl Into<PathBuf>) -> Self {
        Self {
            entry: entry.into(),
            inputs: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_input(mut self, input: Input) -> Self {
        self.inputs.push(input);
        self
    }
}

#[derive(Clone)]
pub(super) struct OwnedWorkspace {
    pub(super) root: Arc<PathBuf>,
    copied: Arc<Mutex<u64>>,
}

pub(super) struct PreparedRoot {
    pub(super) executable: PathBuf,
    pub(super) guest_entry: PathBuf,
    pub(super) rootfs: Option<PathBuf>,
}

impl OwnedWorkspace {
    const COPY_LIMIT: u64 = 64 * 1024 * 1024;

    pub(super) fn fail<T>(&self, error: EngineError) -> Result<T, EngineError> {
        fs::remove_dir_all(self.root.as_ref()).map_err(|_| EngineError::WorkspaceFailed)?;
        Err(error)
    }

    pub(super) fn prepare_rootfs(
        &self,
        source: &Path,
        rootfs: Option<Rootfs>,
        guest_executable: Option<&Path>,
    ) -> Result<PreparedRoot, EngineError> {
        let (relative, guest_entry, root) = if let Some(rootfs) = rootfs {
            let entry = self.confined(&rootfs.entry)?;
            for input in rootfs.inputs {
                self.stage_input(input.prefixed(Path::new("rootfs")))?;
            }
            (
                Path::new("rootfs").join(&entry),
                Path::new("/").join(entry),
                Some(self.root.join("rootfs")),
            )
        } else {
            let guest = guest_executable.unwrap_or_else(|| Path::new("/guest"));
            let alias = self.confined(guest.strip_prefix("/").map_err(|_| EngineError::WorkspaceFailed)?)?;
            let executable = self.stage(source, Path::new("guest"))?;
            if alias != Path::new("guest") {
                let destination = self.destination(&alias)?;
                if let Some(parent) = destination.parent() {
                    fs::create_dir_all(parent).map_err(|_| EngineError::WorkspaceFailed)?;
                }
                fs::hard_link(&executable, &destination).map_err(|_| EngineError::WorkspaceFailed)?;
            }
            return Ok(PreparedRoot {
                executable,
                guest_entry: guest.to_owned(),
                rootfs: None,
            });
        };
        let executable = self.stage(source, &relative)?;
        Ok(PreparedRoot {
            executable,
            guest_entry,
            rootfs: root,
        })
    }

    pub(super) fn stage_working_directory(
        &self,
        filesystem_root: &Path,
        guest: &Path,
    ) -> Result<(), EngineError> {
        let relative = guest
            .strip_prefix(Path::new("/"))
            .map_err(|_| EngineError::WorkspaceFailed)?;
        if relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(EngineError::WorkspaceFailed);
        }
        fs::create_dir_all(filesystem_root.join(relative)).map_err(|_| EngineError::WorkspaceFailed)
    }

    pub(super) fn stage_base_system(&self, system: BaseSystem) -> Result<(), EngineError> {
        for (relative, contents) in system.files {
            self.reserve(u64::try_from(contents.len()).map_err(|_| EngineError::WorkspaceFailed)?)?;
            let destination = self.destination(&relative)?;
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).map_err(|_| EngineError::WorkspaceFailed)?;
            }
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&destination)
                .map_err(|_| EngineError::WorkspaceFailed)?;
            file.write_all(&contents).map_err(|_| EngineError::WorkspaceFailed)?;
            file.set_permissions(fs::Permissions::from_mode(0o644))
                .map_err(|_| EngineError::WorkspaceFailed)?;
        }
        Ok(())
    }

    pub(super) fn create() -> Result<Self, EngineError> {
        for attempt in 0..16 {
            let root = std::env::temp_dir().join(format!(
                "hl-runtime-{}-{}-{attempt}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            match fs::create_dir(&root) {
                Ok(()) => {
                    return Ok(Self {
                        root: Arc::new(root),
                        copied: Arc::new(Mutex::new(0)),
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(EngineError::WorkspaceFailed),
            }
        }
        Err(EngineError::WorkspaceFailed)
    }

    fn stage(&self, source: &Path, relative: &Path) -> Result<PathBuf, EngineError> {
        let destination = self.destination(relative)?;
        if fs::symlink_metadata(&destination).is_ok() {
            return Err(EngineError::WorkspaceFailed);
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|_| EngineError::WorkspaceFailed)?;
        }
        fs::copy(source, &destination).map_err(|_| EngineError::WorkspaceFailed)?;
        Ok(destination)
    }

    pub(super) fn stage_input(&self, input: Input) -> Result<(), EngineError> {
        match input {
            Input::File {
                source,
                relative,
                executable,
            } => self.stage_file(&source, &relative, executable)?,
            Input::Directory { source, relative } => self.stage_directory(source.as_deref(), &relative)?,
            Input::Symlink { relative, target } => self.stage_symlink(&relative, &target)?,
        }
        Ok(())
    }

    fn stage_file(&self, source: &Path, relative: &Path, executable: bool) -> Result<(), EngineError> {
        let metadata = fs::symlink_metadata(source).map_err(|_| EngineError::WorkspaceFailed)?;
        if !metadata.file_type().is_file() {
            return Err(EngineError::WorkspaceFailed);
        }
        self.reserve(metadata.len())?;
        let destination = self.stage(source, relative)?;
        if executable {
            Self::executable(&destination)?;
        }
        Ok(())
    }

    fn stage_directory(&self, source: Option<&Path>, relative: &Path) -> Result<(), EngineError> {
        let destination = self.destination(relative)?;
        if fs::symlink_metadata(&destination).is_ok() {
            return Err(EngineError::WorkspaceFailed);
        }
        fs::create_dir_all(&destination).map_err(|_| EngineError::WorkspaceFailed)?;
        if let Some(source) = source {
            self.copy_tree(source, &destination)?;
        }
        Ok(())
    }

    fn copy_tree(&self, source: &Path, destination: &Path) -> Result<(), EngineError> {
        for entry in fs::read_dir(source).map_err(|_| EngineError::WorkspaceFailed)? {
            self.copy_entry(&entry.map_err(|_| EngineError::WorkspaceFailed)?, destination)?;
        }
        Ok(())
    }

    fn copy_entry(&self, entry: &fs::DirEntry, destination: &Path) -> Result<(), EngineError> {
        let kind = entry.file_type().map_err(|_| EngineError::WorkspaceFailed)?;
        let target = destination.join(entry.file_name());
        if kind.is_dir() {
            fs::create_dir(&target).map_err(|_| EngineError::WorkspaceFailed)?;
            return self.copy_tree(&entry.path(), &target);
        }
        if !kind.is_file() {
            return Err(EngineError::WorkspaceFailed);
        }
        self.reserve(entry.metadata().map_err(|_| EngineError::WorkspaceFailed)?.len())?;
        fs::copy(entry.path(), target).map_err(|_| EngineError::WorkspaceFailed)?;
        Ok(())
    }

    fn reserve(&self, bytes: u64) -> Result<(), EngineError> {
        let mut copied = self.copied.lock().map_err(|_| EngineError::WorkspaceFailed)?;
        *copied = copied.checked_add(bytes).ok_or(EngineError::WorkspaceFailed)?;
        if *copied > Self::COPY_LIMIT {
            return Err(EngineError::WorkspaceFailed);
        }
        Ok(())
    }

    fn destination(&self, relative: &Path) -> Result<PathBuf, EngineError> {
        if relative.components().any(|part| !matches!(part, Component::Normal(_))) {
            return Err(EngineError::WorkspaceFailed);
        }
        Ok(self.root.join(relative))
    }

    fn confined(&self, relative: &Path) -> Result<PathBuf, EngineError> {
        if relative.components().all(|part| matches!(part, Component::Normal(_))) {
            Ok(relative.to_owned())
        } else {
            Err(EngineError::WorkspaceFailed)
        }
    }

    fn stage_symlink(&self, relative: &Path, target: &Path) -> Result<(), EngineError> {
        let destination = self.destination(relative)?;
        if target.is_absolute() || target.components().any(|part| !matches!(part, Component::Normal(_))) {
            return Err(EngineError::WorkspaceFailed);
        }
        let parent = destination.parent().ok_or(EngineError::WorkspaceFailed)?;
        fs::create_dir_all(parent).map_err(|_| EngineError::WorkspaceFailed)?;
        let resolved = parent.join(target);
        if !resolved.starts_with(self.root.as_ref()) || !resolved.exists() {
            return Err(EngineError::WorkspaceFailed);
        }
        std::os::unix::fs::symlink(target, destination).map_err(|_| EngineError::WorkspaceFailed)
    }

    fn executable(path: &Path) -> Result<(), EngineError> {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)
            .map_err(|_| EngineError::WorkspaceFailed)?
            .permissions();
        permissions.set_mode(permissions.mode() | 0o111);
        fs::set_permissions(path, permissions).map_err(|_| EngineError::WorkspaceFailed)
    }
}

impl Workspace for OwnedWorkspace {
    fn prepare(&self) -> Result<WorkspaceId, EngineError> {
        Ok(WorkspaceId(1))
    }

    fn cleanup(&self, _: WorkspaceId) -> Result<(), EngineError> {
        if self.root.exists() {
            fs::remove_dir_all(self.root.as_ref()).map_err(|_| EngineError::WorkspaceFailed)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{BaseSystem, OwnedWorkspace};
    use crate::engine::{EngineError, Workspace, WorkspaceId};
    use std::fs::OpenOptions;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    #[test]
    fn base_files_are_owned() {
        let workspace = OwnedWorkspace::create().unwrap();
        workspace.stage_base_system(BaseSystem::linux()).unwrap();
        assert_eq!(
            std::fs::read(workspace.root.join("etc/passwd")).unwrap(),
            b"root:x:0:0:root:/root:/bin/sh\n"
        );
        assert_eq!(
            std::fs::metadata(workspace.root.join("etc/group"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o644
        );
        workspace.cleanup(WorkspaceId(1)).unwrap();
    }

    #[test]
    fn base_rejects_escape_collision() {
        let workspace = OwnedWorkspace::create().unwrap();
        assert_eq!(
            workspace.stage_base_system(BaseSystem::empty().with_file("../escape", b"bad".to_vec())),
            Err(EngineError::WorkspaceFailed)
        );
        workspace.stage_base_system(BaseSystem::linux()).unwrap();
        assert_eq!(
            workspace.stage_base_system(BaseSystem::empty().with_file("etc/passwd", b"replace".to_vec())),
            Err(EngineError::WorkspaceFailed)
        );
        workspace.cleanup(WorkspaceId(1)).unwrap();
    }

    #[test]
    fn copy_limit() {
        let workspace = OwnedWorkspace::create().unwrap();
        let source = workspace.root.join("large");
        OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&source)
            .unwrap()
            .set_len(OwnedWorkspace::COPY_LIMIT + 1)
            .unwrap();
        assert_eq!(
            workspace.stage_file(&source, Path::new("copy"), false),
            Err(EngineError::WorkspaceFailed)
        );
        workspace.cleanup(WorkspaceId(1)).unwrap();
    }

    #[test]
    fn file_mode() {
        let workspace = OwnedWorkspace::create().unwrap();
        let source = workspace.root.join("mode-source");
        std::fs::write(&source, b"mode").unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o640)).unwrap();
        workspace.stage_file(&source, Path::new("mode-copy"), false).unwrap();
        assert_eq!(
            std::fs::metadata(workspace.root.join("mode-copy"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
        workspace.cleanup(WorkspaceId(1)).unwrap();
    }

    #[test]
    fn source_symlink() {
        let workspace = OwnedWorkspace::create().unwrap();
        let source = workspace.root.join("source");
        let link = workspace.root.join("source-link");
        std::fs::write(&source, b"source").unwrap();
        std::os::unix::fs::symlink(&source, &link).unwrap();
        assert_eq!(
            workspace.stage_file(&link, Path::new("copy"), false),
            Err(EngineError::WorkspaceFailed)
        );
        workspace.cleanup(WorkspaceId(1)).unwrap();
    }

    #[test]
    fn guest_working_directory_mirrors_absolute_host_path() {
        let workspace = OwnedWorkspace::create().unwrap();
        workspace
            .stage_working_directory(&workspace.root, Path::new("/Users/example/project"))
            .unwrap();
        assert!(workspace.root.join("Users/example/project").is_dir());
        assert_eq!(
            workspace.stage_working_directory(&workspace.root, Path::new("relative")),
            Err(EngineError::WorkspaceFailed)
        );
        workspace.cleanup(WorkspaceId(1)).unwrap();
    }
}
