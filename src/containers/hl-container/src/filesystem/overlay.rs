use super::Path;
use crate::{Access, Error, Result};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Component, Path as FsPath, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
pub(super) struct Overlay {
    pub(super) lower: PathBuf,
    pub(super) upper: PathBuf,
    pub(super) lower_ownership: Arc<Mutex<hl_images::snapshot::Ownerships>>,
    pub(super) upper_ownership: Arc<Mutex<hl_images::snapshot::Ownerships>>,
}

impl Overlay {
    pub(super) fn archive(&self, relative: &FsPath, writer: impl Write) -> Result<()> {
        let resolved = self.resolve(relative, false)?;
        let metadata = fs::symlink_metadata(&resolved.path)?;
        let mut archive = tar::Builder::new(writer);
        archive.follow_symlinks(false);
        let name = relative.file_name().map_or_else(|| PathBuf::from("."), PathBuf::from);
        if metadata.is_dir() {
            if !relative.as_os_str().is_empty() {
                self.append_owned(&mut archive, relative, &name, &resolved.path)?;
            }
            self.archive_directory(&mut archive, relative, &name)?;
        } else {
            self.append_owned(&mut archive, relative, &name, &resolved.path)?;
        }
        archive.finish()?;
        Ok(())
    }

    pub(super) fn archive_directory<W: Write>(
        &self,
        archive: &mut tar::Builder<W>,
        relative: &FsPath,
        output: &FsPath,
    ) -> Result<()> {
        let lower = self.lower.join(relative);
        let upper = self.upper.join(relative);
        let opaque = upper.join(".wh..wh..opq").exists();
        let mut names = BTreeMap::<OsString, PathBuf>::new();
        if lower.is_dir() && !opaque {
            for entry in fs::read_dir(&lower)? {
                let entry = entry?;
                names.insert(entry.file_name(), entry.path());
            }
        }
        if upper.is_dir() {
            for entry in fs::read_dir(&upper)? {
                let entry = entry?;
                let name = entry.file_name();
                let text = name.to_string_lossy();
                if text == ".wh..wh..opq" {
                    continue;
                }
                if let Some(victim) = text.strip_prefix(".wh.") {
                    names.remove(&OsString::from(victim));
                    continue;
                }
                names.insert(name, entry.path());
            }
        }
        for (name, source) in names {
            let destination = if output == FsPath::new(".") {
                PathBuf::from(&name)
            } else {
                output.join(&name)
            };
            let metadata = fs::symlink_metadata(&source)?;
            if metadata.is_dir() {
                self.append_owned(archive, &relative.join(&name), &destination, &source)?;
                self.archive_directory(archive, &relative.join(&name), &destination)?;
            } else {
                self.append_owned(archive, &relative.join(&name), &destination, &source)?;
            }
        }
        Ok(())
    }

    pub(super) fn append_owned<W: Write>(
        &self,
        archive: &mut tar::Builder<W>,
        guest: &FsPath,
        destination: &FsPath,
        source: &FsPath,
    ) -> Result<()> {
        let metadata = fs::symlink_metadata(source)?;
        let mut header = tar::Header::new_gnu();
        header.set_metadata(&metadata);
        let ownership = self.ownership(guest, source.starts_with(&self.upper))?;
        if let Some(owner) = ownership {
            header.set_uid(u64::from(owner.uid));
            header.set_gid(u64::from(owner.gid));
        }
        if metadata.file_type().is_symlink() {
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_link_name(fs::read_link(source)?)?;
            header.set_cksum();
            archive.append_data(&mut header, destination, std::io::empty())?;
        } else if metadata.is_dir() {
            header.set_entry_type(tar::EntryType::Directory);
            header.set_size(0);
            header.set_cksum();
            archive.append_data(&mut header, destination, std::io::empty())?;
        } else {
            header.set_entry_type(tar::EntryType::Regular);
            header.set_size(metadata.len());
            header.set_cksum();
            archive.append_data(&mut header, destination, fs::File::open(source)?)?;
        }
        Ok(())
    }

    pub(super) fn ownership(&self, guest: &FsPath, upper: bool) -> Result<Option<hl_images::snapshot::Ownership>> {
        if upper {
            let ownership = self
                .upper_ownership
                .lock()
                .map_err(|_| Error::Corrupt("rootfs ownership lock is poisoned".into()))?
                .get(guest);
            if ownership.is_some() {
                return Ok(ownership);
            }
        }
        Ok(self
            .lower_ownership
            .lock()
            .map_err(|_| Error::Corrupt("rootfs ownership lock is poisoned".into()))?
            .get(guest))
    }

    pub(super) fn resolve(&self, relative: &FsPath, write: bool) -> Result<Resolution> {
        let lower = fs::canonicalize(&self.lower)?;
        let upper = fs::canonicalize(&self.upper)?;
        if write {
            self.copy_up_directory(relative)?;
            return Ok(Resolution {
                path: upper.join(relative),
                access: Access::ReadWrite,
            });
        }
        if self.masked(relative) {
            return Ok(Resolution {
                path: upper.join(relative),
                access: Access::ReadWrite,
            });
        }
        let candidate = if upper.join(relative).exists() {
            upper.join(relative)
        } else {
            lower.join(relative)
        };
        let base = if candidate.starts_with(&upper) { &upper } else { &lower };
        // The overlay root is itself a valid archive/stat target. Its parent
        // necessarily lies outside the overlay, so validate the root rather
        // than its parent in that case. Non-root targets still use the nearest
        // existing parent to avoid following an escaping final symlink.
        let parent = if candidate == *base {
            candidate.as_path()
        } else {
            candidate.parent().unwrap_or(&candidate)
        };
        let canonical = fs::canonicalize(Path::nearest(parent)?)?;
        if !canonical.starts_with(base) {
            return Err(Error::InvalidSpec("container path escapes its overlay root".into()));
        }
        Ok(Resolution {
            path: candidate,
            access: Access::ReadWrite,
        })
    }

    pub(super) fn masked(&self, relative: &FsPath) -> bool {
        let mut parent = PathBuf::new();
        for component in relative.components() {
            let Component::Normal(name) = component else {
                continue;
            };
            if self.upper.join(&parent).join(".wh..wh..opq").exists() && !self.upper.join(&parent).join(name).exists() {
                return true;
            }
            let marker = self.upper.join(&parent).join(format!(".wh.{}", name.to_string_lossy()));
            if marker.exists() {
                return true;
            }
            parent.push(name);
        }
        false
    }

    pub(super) fn copy_up_directory(&self, relative: &FsPath) -> Result<()> {
        let lower = fs::canonicalize(&self.lower)?;
        let upper = fs::canonicalize(&self.upper)?;
        let destination = upper.join(relative);
        if destination.is_dir() {
            let canonical = fs::canonicalize(&destination)?;
            if canonical.starts_with(&upper) {
                return Ok(());
            }
            return Err(Error::InvalidSpec("container path escapes its overlay root".into()));
        }
        let source = lower.join(relative);
        let metadata = fs::symlink_metadata(&source)?;
        if metadata.file_type().is_symlink() {
            return Err(Error::InvalidSpec(
                "writes through lower overlay symlinks are not supported".into(),
            ));
        }
        if !metadata.is_dir() {
            return Err(Error::InvalidSpec("archive destination must be a directory".into()));
        }
        let canonical = fs::canonicalize(&source)?;
        if !canonical.starts_with(&lower) {
            return Err(Error::InvalidSpec("container path escapes its overlay root".into()));
        }
        fs::create_dir_all(&destination)?;
        fs::set_permissions(&destination, metadata.permissions())?;
        if let Some(name) = relative.file_name() {
            let marker = destination
                .parent()
                .unwrap_or(&upper)
                .join(format!(".wh.{}", name.to_string_lossy()));
            match fs::remove_file(marker) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
}

pub(super) struct Resolution {
    pub(super) path: PathBuf,
    pub(super) access: Access,
}
