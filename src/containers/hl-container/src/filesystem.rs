mod extraction;
mod inventory;
mod overlay;
mod path;

pub use extraction::{Extraction, Limits};
pub use inventory::{Change, ChangeKind, Changes};
pub use path::Path;

use self::overlay::{Overlay, Resolution};
use crate::{Access, Error, Result, model::ResolvedMount};
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

/// Filesystem metadata independent from Docker's wire encoding.
#[derive(Clone, Debug)]
pub struct Stat {
    pub name: String,
    pub size: u64,
    pub mode: u32,
    pub modified: SystemTime,
    pub link: Option<PathBuf>,
}

/// Mount-aware filesystem surface for one container.
#[derive(Clone, Debug)]
pub struct Filesystem {
    root: PathBuf,
    overlay: Option<Overlay>,
    mounts: Vec<ResolvedMount>,
    generation: Option<crate::generation::Generation>,
}

impl Filesystem {
    pub(crate) fn new(root: PathBuf, mounts: Vec<ResolvedMount>) -> Self {
        Self {
            root,
            overlay: None,
            mounts,
            generation: None,
        }
    }

    pub(crate) fn overlay(
        lower: PathBuf,
        upper: PathBuf,
        lower_ownership: hl_images::snapshot::Ownerships,
        upper_ownership: hl_images::snapshot::Ownerships,
        mounts: Vec<ResolvedMount>,
    ) -> Self {
        Self {
            root: lower.clone(),
            overlay: Some(Overlay {
                lower,
                upper,
                lower_ownership: Arc::new(Mutex::new(lower_ownership)),
                upper_ownership: Arc::new(Mutex::new(upper_ownership)),
            }),
            mounts,
            generation: None,
        }
    }

    pub(crate) fn with_generation(mut self, value: crate::generation::Generation) -> Self {
        self.generation = Some(value);
        self
    }

    /// Reads metadata without following the final symlink.
    ///
    /// # Errors
    /// Returns an error for unsafe guest paths, symlink escapes, or filesystem failures.
    pub fn stat(&self, path: impl AsRef<FsPath>) -> Result<Stat> {
        let _span = hl_log::hl_span!(hl_log::tag::CONTAINER, "filesystem.stat");
        let resolved = self.resolve(path.as_ref(), false)?;
        let metadata = fs::symlink_metadata(&resolved.path)?;
        #[cfg(unix)]
        let mode = metadata.permissions().mode();
        #[cfg(not(unix))]
        let mode = u32::from(metadata.is_dir()) << 31;
        Ok(Stat {
            name: resolved
                .path
                .file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned()),
            size: metadata.len(),
            mode,
            modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            link: metadata
                .file_type()
                .is_symlink()
                .then(|| fs::read_link(&resolved.path))
                .transpose()?,
        })
    }

    /// Produces a tar archive containing the requested path and its basename.
    ///
    /// # Errors
    /// Returns an error for unsafe paths, missing files, or archive/filesystem failures.
    pub fn archive(&self, path: impl AsRef<FsPath>, writer: impl Write) -> Result<()> {
        let _span = hl_log::hl_span!(hl_log::tag::CONTAINER, "filesystem.archive");
        let guest = Path::guest(path.as_ref())?;
        let mounted = self
            .mounts
            .iter()
            .any(|mount| guest.as_path() == mount.target || guest.as_path().starts_with(&mount.target));
        if !mounted
            && let Some(overlay) = &self.overlay {
                let relative = guest.as_path().strip_prefix("/").unwrap_or(guest.as_path());
                return overlay.archive(relative, writer);
            }
        let resolved = self.resolve(path.as_ref(), false)?;
        fs::symlink_metadata(&resolved.path)?;
        let name = resolved.path.file_name().unwrap_or_else(|| std::ffi::OsStr::new("."));
        let mut archive = tar::Builder::new(writer);
        archive.follow_symlinks(false);
        if resolved.path == fs::canonicalize(&self.root)? {
            for entry in fs::read_dir(&resolved.path)? {
                let entry = entry?;
                let path = entry.path();
                let name = entry.file_name();
                Self::append_archive_entry(&mut archive, &name, &path)?;
            }
        } else if resolved.path.is_dir() {
            archive.append_dir_all(name, &resolved.path)?;
        } else {
            archive.append_path_with_name(&resolved.path, name)?;
        }
        archive.finish()?;
        Ok(())
    }

    fn append_archive_entry(
        archive: &mut tar::Builder<impl Write>,
        name: &std::ffi::OsStr,
        path: &FsPath,
    ) -> Result<()> {
        if path.is_dir() {
            archive.append_dir_all(name, path)?;
        } else {
            archive.append_path_with_name(path, name)?;
        }
        Ok(())
    }

    /// Extracts a tar archive into an existing container directory.
    ///
    /// # Errors
    /// Returns an error for read-only mounts, unsafe/special entries, exceeded limits, or I/O failures.
    pub fn extract(&self, path: impl AsRef<FsPath>, reader: impl Read, limits: Limits) -> Result<()> {
        self.extract_with(path, reader, limits, Extraction::default())
    }

    /// Extract a tar archive, optionally preserving its guest uid/gid metadata.
    ///
    /// # Errors
    /// Returns the same validation and filesystem failures as [`Self::extract`].
    pub fn extract_owned(
        &self,
        path: impl AsRef<FsPath>,
        reader: impl Read,
        limits: Limits,
        copy_uid_gid: bool,
    ) -> Result<()> {
        self.extract_with(
            path,
            reader,
            limits,
            Extraction {
                copy_uid_gid,
                ..Extraction::default()
            },
        )
    }

    /// Extracts a tar archive under the supplied ownership and replacement policy.
    ///
    /// # Errors
    /// Returns the same validation and filesystem failures as [`Self::extract`].
    pub fn extract_with(
        &self,
        path: impl AsRef<FsPath>,
        mut reader: impl Read,
        limits: Limits,
        extraction: Extraction,
    ) -> Result<()> {
        let _span = hl_log::hl_span!(hl_log::tag::CONTAINER, "filesystem.extract");
        let destination = self.resolve(path.as_ref(), true)?;
        if destination.access == Access::ReadOnly {
            return Err(Error::ReadOnly(path.as_ref().to_owned()));
        }
        let root = fs::canonicalize(&destination.path)?;
        if !root.is_dir() {
            return Err(Error::InvalidSpec("archive destination must be a directory".into()));
        }
        let raw_limit = limits
            .bytes
            .saturating_add(limits.entries.saturating_mul(1024))
            .saturating_add(1024);
        let mut staged = tempfile::tempfile()?;
        let received = std::io::copy(&mut reader.by_ref().take(raw_limit.saturating_add(1)), &mut staged)?;
        if received > raw_limit {
            return Err(Error::InvalidSpec("archive input exceeds extraction limit".into()));
        }
        hl_log::hl_debug!(
            hl_log::tag::CONTAINER,
            "filesystem archive received bytes={} preserve_owner={}",
            received,
            extraction.copy_uid_gid
        );
        staged.seek(SeekFrom::Start(0))?;
        Self::preflight(&mut staged, limits)?;
        staged.seek(SeekFrom::Start(0))?;
        let mut archive = tar::Archive::new(staged);
        let destination_relative = self
            .overlay
            .as_ref()
            .map(|overlay| fs::canonicalize(&overlay.upper))
            .transpose()?
            .and_then(|upper| destination.path.strip_prefix(upper).ok().map(PathBuf::from));
        let ownership = self
            .overlay
            .as_ref()
            .map(|overlay| Arc::clone(&overlay.upper_ownership));
        let extracted = archive.entries()?.try_for_each(|item| {
            let mut entry = item?;
            Self::extract_owned_entry(
                &root,
                destination_relative.as_deref(),
                ownership.as_deref(),
                &mut entry,
                extraction,
            )
        });
        extracted?;
        if let Some(generation) = &self.generation {
            generation.bump()?;
        }
        Ok(())
    }

    fn extract_owned_entry<R: Read>(
        root: &FsPath,
        destination_relative: Option<&FsPath>,
        ownership: Option<&Mutex<hl_images::snapshot::Ownerships>>,
        entry: &mut tar::Entry<'_, R>,
        extraction: Extraction,
    ) -> Result<()> {
        let path = Path::entry(&entry.path()?)?;
        let relative = path.as_path().to_owned();
        let output = path.output(root);
        path.prepare(root)?;
        let kind = entry.header().entry_type();
        Self::validate_replacement(&output, kind, extraction)?;
        let owner = if extraction.copy_uid_gid {
            hl_images::snapshot::Ownership {
                uid: u32::try_from(entry.header().uid()?)
                    .map_err(|_| Error::InvalidSpec("archive uid exceeds u32".into()))?,
                gid: u32::try_from(entry.header().gid()?)
                    .map_err(|_| Error::InvalidSpec("archive gid exceeds u32".into()))?,
            }
        } else {
            hl_images::snapshot::Ownership { uid: 0, gid: 0 }
        };
        Self::extract_entry(root, &path, &output, kind, entry)?;
        if let (Some(base), Some(ownership)) = (destination_relative, ownership) {
            ownership
                .lock()
                .map_err(|_| Error::Corrupt("rootfs ownership lock is poisoned".into()))?
                .set(base.join(relative), owner)?;
        }
        Ok(())
    }

    fn extract_entry<R: Read>(
        root: &FsPath,
        path: &Path,
        output: &FsPath,
        kind: tar::EntryType,
        entry: &mut tar::Entry<'_, R>,
    ) -> Result<()> {
        if kind.is_dir() {
            path.ensure_dir(root)?;
        } else if kind.is_file() {
            Self::extract_file(root, path, output, entry)?;
        } else if kind.is_symlink() {
            Self::extract_symlink(root, path, output, entry)?;
        } else if kind.is_hard_link() {
            Self::extract_hard_link(root, path, output, entry)?;
        } else {
            return Err(Error::InvalidSpec("special archive entries are unsupported".into()));
        }
        #[cfg(unix)]
        if !kind.is_symlink()
            && let Ok(mode) = entry.header().mode() {
                fs::set_permissions(output, fs::Permissions::from_mode(mode & 0o7777))?;
            }
        Ok(())
    }

    fn validate_replacement(output: &FsPath, kind: tar::EntryType, extraction: Extraction) -> Result<()> {
        if !extraction.no_overwrite_dir_non_dir {
            return Ok(());
        }
        let existing = match fs::symlink_metadata(output) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        match (existing.is_dir(), kind.is_dir()) {
            (true, false) => Err(Error::InvalidSpec(format!(
                "cannot overwrite directory {:?} with a non-directory",
                output.file_name().unwrap_or(output.as_os_str())
            ))),
            (false, true) => Err(Error::InvalidSpec(format!(
                "cannot overwrite non-directory {:?} with a directory",
                output.file_name().unwrap_or(output.as_os_str())
            ))),
            _ => Ok(()),
        }
    }

    fn extract_file<R: Read>(root: &FsPath, path: &Path, output: &FsPath, entry: &mut tar::Entry<'_, R>) -> Result<()> {
        path.replace(root)?;
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(output)?;
        std::io::copy(entry, &mut file)?;
        file.flush()?;
        Ok(())
    }

    fn extract_symlink<R: Read>(root: &FsPath, path: &Path, output: &FsPath, entry: &tar::Entry<'_, R>) -> Result<()> {
        let target = entry
            .link_name()?
            .ok_or_else(|| Error::InvalidSpec("symlink entry has no target".into()))?;
        path.validate_link(&target)?;
        path.replace(root)?;
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, output)?;
            Ok(())
        }
        #[cfg(not(unix))]
        {
            Err(Error::InvalidSpec("symlink archives are unsupported".into()))
        }
    }

    fn extract_hard_link<R: Read>(
        root: &FsPath,
        path: &Path,
        output: &FsPath,
        entry: &tar::Entry<'_, R>,
    ) -> Result<()> {
        let target = entry
            .link_name()?
            .ok_or_else(|| Error::InvalidSpec("hard-link entry has no target".into()))?;
        let canonical = fs::canonicalize(Path::entry(&target)?.output(root))?;
        if !canonical.starts_with(root) || !canonical.is_file() {
            return Err(Error::InvalidSpec("archive hard-link target is unsafe".into()));
        }
        path.replace(root)?;
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::hard_link(canonical, output)?;
        Ok(())
    }

    fn preflight(reader: impl Read, limits: Limits) -> Result<()> {
        let mut archive = tar::Archive::new(reader);
        let mut entries = 0_u64;
        let mut bytes = 0_u64;
        for item in archive.entries()? {
            let entry = item?;
            entries = entries.saturating_add(1);
            bytes = bytes.saturating_add(entry.size());
            if entries > limits.entries || bytes > limits.bytes {
                return Err(Error::InvalidSpec("archive extraction limit exceeded".into()));
            }
            Self::validate_archive_entry(&entry)?;
        }
        Ok(())
    }

    fn validate_archive_entry<R: Read>(entry: &tar::Entry<'_, R>) -> Result<()> {
        let path = Path::entry(&entry.path()?)?;
        let kind = entry.header().entry_type();
        if kind.is_symlink() {
            let target = entry
                .link_name()?
                .ok_or_else(|| Error::InvalidSpec("symlink entry has no target".into()))?;
            path.validate_link(&target)?;
        } else if kind.is_hard_link() {
            let target = entry
                .link_name()?
                .ok_or_else(|| Error::InvalidSpec("hard-link entry has no target".into()))?;
            Path::entry(&target)?;
        } else if !kind.is_dir() && !kind.is_file() {
            return Err(Error::InvalidSpec("special archive entries are unsupported".into()));
        }
        Ok(())
    }

    fn resolve(&self, path: &FsPath, write: bool) -> Result<Resolution> {
        let guest = Path::guest(path)?;
        let selected = self
            .mounts
            .iter()
            .filter(|mount| guest.as_path() == mount.target || guest.as_path().starts_with(&mount.target))
            .max_by_key(|mount| mount.target.components().count());
        if selected.is_none()
            && let Some(overlay) = &self.overlay {
                let relative = guest.as_path().strip_prefix("/").unwrap_or(guest.as_path());
                return overlay.resolve(relative, write);
            }
        let (base, relative, access) = selected.map_or_else(
            || {
                (
                    self.root.as_path(),
                    guest.as_path().strip_prefix("/").unwrap_or(guest.as_path()),
                    Access::ReadWrite,
                )
            },
            |mount| {
                (
                    mount.source.as_path(),
                    guest.as_path().strip_prefix(&mount.target).unwrap_or(FsPath::new("")),
                    mount.access,
                )
            },
        );
        let base = fs::canonicalize(base)?;
        let path = base.join(relative);
        let parent = if write || path == base {
            path.as_path()
        } else {
            path.parent().unwrap_or(&path)
        };
        let canonical = fs::canonicalize(Path::nearest(parent)?)?;
        if !canonical.starts_with(&base) {
            return Err(Error::InvalidSpec("container path escapes its filesystem root".into()));
        }
        Ok(Resolution { path, access })
    }
}

#[cfg(test)]
mod test;
