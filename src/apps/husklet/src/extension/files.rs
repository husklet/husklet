//! Files beneath one workspace's storage directory.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use hl_extension::RelativePath;
use hl_extension::port::{Entry, HostError, WorkspaceFiles};

/// The workspace file port, rooted at one directory.
///
/// [`RelativePath`] already refuses traversal and absolute paths when it is
/// constructed, and `Authority::permit_path` confines a call to the declared
/// roots. Neither check can see a symbolic link, so every path this adapter is
/// about to touch is resolved on the real filesystem and re-checked against the
/// canonical root. Syntax is not containment.
pub struct WorkspaceDirectory {
    root: PathBuf,
}

impl WorkspaceDirectory {
    /// Roots the port at `root`, creating it if it does not exist.
    ///
    /// The root is canonicalized once here so that later containment checks
    /// compare two resolved paths; a root reached through a symbolic link would
    /// otherwise make every legitimate path look like an escape.
    ///
    /// # Errors
    /// Returns the failure to create or resolve the root directory.
    pub fn new(root: impl AsRef<Path>) -> io::Result<Self> {
        let root = root.as_ref();
        std::fs::create_dir_all(root)?;
        Ok(Self {
            root: root.canonicalize()?,
        })
    }

    /// The directory this port is confined to.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolves a path that must already exist.
    fn existing(&self, path: &RelativePath) -> Result<PathBuf, HostError> {
        let joined = join(&self.root, path.parts());
        let resolved = joined.canonicalize().map_err(|error| absence(path, &error))?;
        confine(&self.root, resolved)
    }

    /// Resolves a path that may not exist yet, by resolving its parent.
    ///
    /// A file that is already there is resolved too, because an existing entry
    /// can itself be a symbolic link pointing out of the root.
    fn destination(&self, path: &RelativePath) -> Result<PathBuf, HostError> {
        let parts = path.parts();
        let Some((name, parents)) = parts.split_last() else {
            return Err(HostError::Conflict("the workspace root is a directory".to_owned()));
        };
        let parent = join(&self.root, parents.to_vec());
        let parent = parent.canonicalize().map_err(|error| absence(path, &error))?;
        let parent = confine(&self.root, parent)?;
        let target = parent.join(name);
        let Ok(resolved) = target.canonicalize() else {
            return Ok(target);
        };
        confine(&self.root, resolved)
    }

    /// Resolves the parent while leaving the final entry itself untouched.
    fn entry(&self, path: &RelativePath) -> Result<PathBuf, HostError> {
        let parts = path.parts();
        let Some((name, parents)) = parts.split_last() else {
            return Err(HostError::Conflict("the workspace root cannot be mutated".to_owned()));
        };
        let parent = join(&self.root, parents.to_vec());
        let parent = parent.canonicalize().map_err(|error| absence(path, &error))?;
        Ok(confine(&self.root, parent)?.join(name))
    }

    fn publication(&self, path: &RelativePath) -> Result<PathBuf, HostError> {
        let target = self.entry(path)?;
        if std::fs::symlink_metadata(&target).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return Err(HostError::Conflict(format!("{path} is a symbolic link")));
        }
        Ok(target)
    }
}

impl WorkspaceFiles for WorkspaceDirectory {
    /// # Errors
    /// Returns `HostError::Absent` for a missing directory, `HostError::Conflict`
    /// for a path that resolves outside the root, and a failure otherwise.
    fn list(&self, path: &RelativePath) -> Result<Vec<Entry>, HostError> {
        let directory = self.existing(path)?;
        let reading = std::fs::read_dir(&directory).map_err(|error| absence(path, &error))?;
        let mut entries = Vec::new();
        for entry in reading {
            entries.push(described(path, &entry.map_err(|error| absence(path, &error))?)?);
        }
        entries.sort_by(|first, second| first.path.cmp(&second.path));
        Ok(entries)
    }

    /// # Errors
    /// Returns `HostError::Absent` for a missing file, `HostError::Conflict` for
    /// a path that resolves outside the root, and a failure otherwise.
    fn read(&self, path: &RelativePath) -> Result<Vec<u8>, HostError> {
        let file = self.existing(path)?;
        std::fs::read(file).map_err(|error| absence(path, &error))
    }

    fn stat(&self, path: &RelativePath) -> Result<Entry, HostError> {
        let resolved = self.existing(path)?;
        let metadata = std::fs::metadata(resolved).map_err(|error| absence(path, &error))?;
        Ok(Entry {
            path: path.clone(),
            directory: metadata.is_dir(),
            size: metadata.len(),
        })
    }

    /// # Errors
    /// Returns `HostError::Absent` when the containing directory does not exist,
    /// `HostError::Conflict` for a path that resolves outside the root, and a
    /// failure otherwise.
    fn write(&self, path: &RelativePath, contents: &[u8]) -> Result<(), HostError> {
        let target = self.publication(path)?;
        atomic_write(&target, contents).map_err(|error| absence(path, &error))
    }

    fn mkdir(&self, path: &RelativePath) -> Result<(), HostError> {
        let directory = self.destination(path)?;
        std::fs::create_dir(directory).map_err(|error| absence(path, &error))
    }

    fn rename(&self, from: &RelativePath, to: &RelativePath) -> Result<(), HostError> {
        let source = self.entry(from)?;
        std::fs::symlink_metadata(&source).map_err(|error| absence(from, &error))?;
        let destination = self.entry(to)?;
        if std::fs::symlink_metadata(&destination).is_ok() {
            return Err(HostError::Conflict(format!("{to} already exists")));
        }
        std::fs::rename(source, destination).map_err(|error| absence(from, &error))
    }

    fn remove(&self, path: &RelativePath) -> Result<(), HostError> {
        let target = self.entry(path)?;
        let metadata = std::fs::symlink_metadata(&target).map_err(|error| absence(path, &error))?;
        if metadata.is_dir() {
            std::fs::remove_dir(target).map_err(|error| absence(path, &error))
        } else {
            std::fs::remove_file(target).map_err(|error| absence(path, &error))
        }
    }
}

static TEMPORARY: AtomicU64 = AtomicU64::new(0);

struct Temporary(PathBuf);

impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn atomic_write(target: &Path, contents: &[u8]) -> io::Result<()> {
    atomic_write_before_publish(target, contents, |_| Ok(()))
}

fn atomic_write_before_publish(
    target: &Path,
    contents: &[u8],
    before_publish: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<()> {
    let parent = target
        .parent()
        .ok_or_else(|| io::Error::other("file has no parent directory"))?;
    let mut opened = None;
    for _ in 0..128 {
        let sequence = TEMPORARY.fetch_add(1, Ordering::Relaxed);
        let temporary = parent.join(format!(".husklet-write-{}-{sequence}.tmp", std::process::id()));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => {
                opened = Some((Temporary(temporary), file));
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    let (temporary, mut file) = opened
        .ok_or_else(|| io::Error::new(io::ErrorKind::AlreadyExists, "could not reserve atomic write temporary"))?;
    if let Ok(metadata) = std::fs::metadata(target) {
        file.set_permissions(metadata.permissions())?;
    }
    file.write_all(contents)?;
    file.sync_all()?;
    before_publish(&temporary.0)?;
    std::fs::rename(&temporary.0, target)?;
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

/// Appends already-validated components to the root.
fn join(root: &Path, parts: Vec<&str>) -> PathBuf {
    let mut full = root.to_path_buf();
    for part in parts {
        full.push(part);
    }
    full
}

/// Accepts a resolved path only while it is still under the resolved root.
///
/// This is the check a symbolic link would otherwise defeat.
fn confine(root: &Path, resolved: PathBuf) -> Result<PathBuf, HostError> {
    if resolved.starts_with(root) {
        return Ok(resolved);
    }
    Err(HostError::Conflict(format!(
        "{} resolves outside the workspace root",
        resolved.display()
    )))
}

/// A missing file is an absence; anything else is a host failure.
fn absence(path: &RelativePath, error: &io::Error) -> HostError {
    if error.kind() == io::ErrorKind::NotFound {
        return HostError::Absent(path.to_string());
    }
    HostError::Failed(format!("{path}: {error}"))
}

/// Describes one directory entry relative to the listed path.
fn described(parent: &RelativePath, entry: &std::fs::DirEntry) -> Result<Entry, HostError> {
    let name = entry.file_name().to_string_lossy().into_owned();
    let joined = if parent.parts().is_empty() {
        name
    } else {
        format!("{parent}/{name}")
    };
    let path = RelativePath::new(joined).map_err(|refusal| HostError::Failed(refusal.to_string()))?;
    // Report the link itself rather than following it: a listing must not be a
    // way to learn about anything outside the root.
    let metadata = entry
        .metadata()
        .map_err(|error| HostError::Failed(format!("{path}: {error}")))?;
    Ok(Entry {
        path,
        directory: metadata.is_dir(),
        size: metadata.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::WorkspaceDirectory;
    use hl_extension::RelativePath;
    use hl_extension::port::{HostError, WorkspaceFiles};

    fn path(value: &str) -> RelativePath {
        RelativePath::new(value).expect("path")
    }

    #[test]
    fn an_ordinary_nested_path_is_accepted() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        std::fs::create_dir_all(root.join("logs")).expect("directory");
        std::fs::write(root.join("logs/app.log"), b"hello").expect("file");
        let files = WorkspaceDirectory::new(&root).expect("root");

        assert_eq!(files.read(&path("logs/app.log")).expect("contents"), b"hello");
        let listed = files.list(&path("logs")).expect("listing");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].path.as_str(), "logs/app.log");
        assert_eq!(listed[0].size, 5);
        assert!(!listed[0].directory);
        assert_eq!(files.stat(&path("logs/app.log")).expect("metadata"), listed[0]);

        files.write(&path("logs/written.txt"), b"new").expect("write");
        assert_eq!(std::fs::read(root.join("logs/written.txt")).expect("file"), b"new");
        files.mkdir(&path("logs/nested")).expect("mkdir");
        files
            .rename(&path("logs/written.txt"), &path("logs/renamed.txt"))
            .expect("rename");
        files.remove(&path("logs/renamed.txt")).expect("remove file");
        files.remove(&path("logs/nested")).expect("remove empty directory");
        assert!(!root.join("logs/renamed.txt").exists());
    }

    #[test]
    fn a_symbolic_link_out_of_the_root_is_refused() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        std::fs::create_dir_all(&root).expect("root");
        let secret = temporary.path().join("secret.txt");
        std::fs::write(&secret, b"private").expect("secret");
        let local = root.join("local.txt");
        std::fs::write(&local, b"local").expect("local target");
        std::os::unix::fs::symlink(&secret, root.join("escape.txt")).expect("link");
        std::os::unix::fs::symlink("local.txt", root.join("local-link.txt")).expect("local link");
        std::os::unix::fs::symlink(temporary.path(), root.join("outside")).expect("link");
        let files = WorkspaceDirectory::new(&root).expect("root");

        // The name is syntactically valid and inside a declared root, so nothing
        // upstream of this adapter can refuse it.
        let escape = path("escape.txt");
        assert!(matches!(files.read(&escape), Err(HostError::Conflict(_))));
        assert!(matches!(files.stat(&escape), Err(HostError::Conflict(_))));
        assert!(matches!(files.write(&escape, b"owned"), Err(HostError::Conflict(_))));
        assert!(matches!(
            files.write(&path("local-link.txt"), b"owned"),
            Err(HostError::Conflict(_))
        ));
        assert!(matches!(files.list(&path("outside")), Err(HostError::Conflict(_))));
        assert!(
            matches!(files.read(&path("outside/secret.txt")), Err(HostError::Conflict(_))),
            "a linked directory does not become a root"
        );
        assert_eq!(
            std::fs::read(&secret).expect("secret"),
            b"private",
            "the refused write must not have reached the target"
        );
        files.remove(&path("escape.txt")).expect("remove link itself");
        assert_eq!(std::fs::read(&secret).expect("secret"), b"private");
        assert_eq!(std::fs::read(&local).expect("local target"), b"local");
    }

    #[test]
    fn concurrent_atomic_writes_publish_one_complete_value_and_leave_no_temporaries() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        let files = std::sync::Arc::new(WorkspaceDirectory::new(&root).expect("root"));
        let first = vec![b'a'; 64 * 1024];
        let second = vec![b'b'; 64 * 1024];
        let threads = [first.clone(), second.clone()].map(|contents| {
            let files = std::sync::Arc::clone(&files);
            std::thread::spawn(move || files.write(&path("state.bin"), &contents).expect("atomic write"))
        });
        for thread in threads {
            thread.join().expect("writer");
        }
        let published = std::fs::read(root.join("state.bin")).expect("published");
        assert!(
            published == first || published == second,
            "a reader sees one whole publication"
        );
        assert!(std::fs::read_dir(&root).expect("root listing").all(|entry| {
            !entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .starts_with(".husklet-write-")
        }));
    }

    #[test]
    fn failed_publication_preserves_the_old_entry_and_cleans_its_temporary() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        std::fs::create_dir_all(root.join("target")).expect("old directory");
        std::fs::write(root.join("target/held"), b"old").expect("old contents");
        let files = WorkspaceDirectory::new(&root).expect("root");
        assert!(files.write(&path("target"), b"replacement").is_err());
        assert_eq!(std::fs::read(root.join("target/held")).expect("old contents"), b"old");
        assert!(std::fs::read_dir(&root).expect("root listing").all(|entry| {
            !entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .starts_with(".husklet-write-")
        }));
    }

    #[test]
    fn failure_before_publication_preserves_prior_file_bytes_and_cleans_temporary() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let target = temporary.path().join("state");
        std::fs::write(&target, b"old bytes").expect("old contents");

        let result = super::atomic_write_before_publish(&target, b"new bytes", |_| {
            Err(std::io::Error::other("injected before publication"))
        });

        assert!(result.is_err());
        assert_eq!(std::fs::read(&target).expect("old contents"), b"old bytes");
        assert!(
            std::fs::read_dir(temporary.path())
                .expect("directory listing")
                .all(|entry| {
                    !entry
                        .expect("entry")
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".husklet-write-")
                })
        );
    }

    #[test]
    fn unrelated_torn_temporary_is_never_published_or_removed() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        std::fs::create_dir_all(&root).expect("root");
        let torn = root.join(".husklet-write-old.tmp");
        std::fs::write(&torn, b"torn").expect("torn temporary");
        let files = WorkspaceDirectory::new(&root).expect("root");
        files.write(&path("state"), b"new").expect("write");
        assert_eq!(std::fs::read(root.join("state")).expect("state"), b"new");
        assert_eq!(std::fs::read(torn).expect("unowned temporary"), b"torn");
    }

    #[test]
    fn a_missing_file_is_absent_rather_than_a_failure() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let files = WorkspaceDirectory::new(temporary.path().join("workspace")).expect("root");
        assert!(matches!(files.read(&path("nothing.txt")), Err(HostError::Absent(_))));
        assert!(matches!(files.list(&path("nowhere")), Err(HostError::Absent(_))));
    }

    #[test]
    fn a_root_reached_through_a_link_still_accepts_its_own_files() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let real = temporary.path().join("real");
        std::fs::create_dir_all(&real).expect("directory");
        std::fs::write(real.join("app.log"), b"hello").expect("file");
        let linked = temporary.path().join("linked");
        std::os::unix::fs::symlink(&real, &linked).expect("link");

        let files = WorkspaceDirectory::new(&linked).expect("root");
        assert_eq!(files.read(&path("app.log")).expect("contents"), b"hello");
    }
}
