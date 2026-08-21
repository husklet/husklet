use std::fs;
use std::io::Write;
#[cfg(unix)]
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
    pub(super) root: Arc<WorkspaceRoot>,
    copied: Arc<Mutex<u64>>,
}

pub(super) struct WorkspaceRoot(PathBuf);

impl std::ops::Deref for WorkspaceRoot {
    type Target = PathBuf;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<Path> for WorkspaceRoot {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for WorkspaceRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
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

    pub(super) fn stage_working_directory(&self, filesystem_root: &Path, guest: &Path) -> Result<(), EngineError> {
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

    pub(super) fn stage_base_system(&self) -> Result<(), EngineError> {
        for (relative, contents) in [
            (Path::new("etc/passwd"), b"root:x:0:0:root:/root:/bin/sh\n".as_slice()),
            (Path::new("etc/group"), b"root:x:0:\n".as_slice()),
        ] {
            self.reserve(u64::try_from(contents.len()).map_err(|_| EngineError::WorkspaceFailed)?)?;
            let destination = self.destination(relative)?;
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).map_err(|_| EngineError::WorkspaceFailed)?;
            }
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&destination)
                .map_err(|_| EngineError::WorkspaceFailed)?;
            file.write_all(contents).map_err(|_| EngineError::WorkspaceFailed)?;
            Self::regular_permissions(&file)?;
        }
        Ok(())
    }

    pub(super) fn create() -> Result<Self, EngineError> {
        for attempt in 0..16 {
            let root = std::env::temp_dir().join(format!(
                "husklet-engine-{}-{}-{attempt}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            match fs::create_dir(&root) {
                Ok(()) => {
                    return Ok(Self {
                        root: Arc::new(WorkspaceRoot(root)),
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
        Self::symlink(target, &destination, &resolved)
    }

    #[cfg(unix)]
    fn regular_permissions(file: &fs::File) -> Result<(), EngineError> {
        file.set_permissions(fs::Permissions::from_mode(0o644))
            .map_err(|_| EngineError::WorkspaceFailed)
    }

    #[cfg(windows)]
    fn regular_permissions(_: &fs::File) -> Result<(), EngineError> {
        Ok(())
    }

    #[cfg(unix)]
    fn symlink(target: &Path, destination: &Path, _: &Path) -> Result<(), EngineError> {
        std::os::unix::fs::symlink(target, destination).map_err(|_| EngineError::WorkspaceFailed)
    }

    #[cfg(windows)]
    fn symlink(target: &Path, destination: &Path, resolved: &Path) -> Result<(), EngineError> {
        let result = if resolved.is_dir() {
            std::os::windows::fs::symlink_dir(target, destination)
        } else {
            std::os::windows::fs::symlink_file(target, destination)
        };
        result.map_err(|_| EngineError::WorkspaceFailed)
    }

    #[cfg(unix)]
    fn executable(path: &Path) -> Result<(), EngineError> {
        let mut permissions = fs::metadata(path)
            .map_err(|_| EngineError::WorkspaceFailed)?
            .permissions();
        permissions.set_mode(permissions.mode() | 0o111);
        fs::set_permissions(path, permissions).map_err(|_| EngineError::WorkspaceFailed)
    }

    #[cfg(windows)]
    fn executable(_: &Path) -> Result<(), EngineError> {
        Ok(())
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
    use super::OwnedWorkspace;
    use crate::engine::{EngineError, Workspace, WorkspaceId};
    use std::fs::OpenOptions;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    #[test]
    fn base_files_are_owned() {
        let workspace = OwnedWorkspace::create().unwrap();
        workspace.stage_base_system().unwrap();
        assert_eq!(
            std::fs::read(workspace.root.join("etc/passwd")).unwrap(),
            b"root:x:0:0:root:/root:/bin/sh\n"
        );
        // The mode half only: `OwnedWorkspace::regular_permissions` is a Unix mode write, and
        // its Windows arm is deliberately `Ok(())` because NTFS carries no `0o644`. The staged
        // content above is a contract on every host, so it stays outside this gate.
        #[cfg(unix)]
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

    /// `#[cfg(unix)]` because the mode a staged copy inherits is a Unix mode: the Windows
    /// arm of `OwnedWorkspace::regular_permissions` writes none, so there is nothing here to
    /// assert on that host rather than something that would fail.
    #[cfg(unix)]
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

    /// `#[cfg(unix)]` for the fixture, not for the contract. Refusing a symlinked source is a
    /// contract on every host -- `stage_file` reads `Path::is_symlink`, which Windows answers --
    /// but *creating* the link the fixture needs requires `SeCreateSymbolicLinkPrivilege` there,
    /// so a Windows arm of this test would fail on host policy rather than on the code it covers.
    #[cfg(unix)]
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

    #[test]
    fn final_owner_removes_workspace_after_early_return() {
        let workspace = OwnedWorkspace::create().unwrap();
        let root = workspace.root.0.clone();
        let retained = workspace.clone();

        drop(workspace);
        assert!(root.is_dir(), "a live workspace owner lost its directory");
        drop(retained);
        assert!(!root.exists(), "the final workspace owner leaked its directory");
    }

    #[test]
    fn concurrent_final_owners_remove_workspace_exactly_once() {
        let workspace = OwnedWorkspace::create().unwrap();
        let root = workspace.root.0.clone();
        let first = workspace.clone();
        let second = workspace.clone();
        drop(workspace);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let dropper = |workspace, barrier: std::sync::Arc<std::sync::Barrier>| {
            std::thread::spawn(move || {
                barrier.wait();
                drop(workspace);
            })
        };
        let left = dropper(first, barrier.clone());
        let right = dropper(second, barrier.clone());
        barrier.wait();
        left.join().unwrap();
        right.join().unwrap();
        assert!(!root.exists(), "concurrent final owners leaked their workspace");
    }
}
