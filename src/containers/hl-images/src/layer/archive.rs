use std::{
    fs,
    io::Read,
    path::{Path as FsPath, PathBuf},
};

use super::Path;
use crate::{
    Error, Result,
    error::At as _,
    snapshot::{Names, Ownership, Ownerships},
};

#[cfg(test)]
thread_local! {
    static REUSE_PARENT_VALIDATION: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

#[cfg(test)]
fn reuse_parent_validation() -> bool {
    REUSE_PARENT_VALIDATION.with(std::cell::Cell::get)
}

#[cfg(not(test))]
const fn reuse_parent_validation() -> bool {
    true
}

fn prepare_entry_parent(
    path: &Path,
    root: &FsPath,
    parent: &FsPath,
    reusable: bool,
    cached: &mut Option<(PathBuf, super::path::Parents)>,
) -> Result<Option<super::path::Parents>> {
    if reusable && cached.as_ref().is_some_and(|(path, _)| path == parent) {
        return Ok(None);
    }
    *cached = None;
    let parents = path.prepare(root)?;
    if reusable {
        *cached = Some((parent.to_owned(), parents));
        return Ok(None);
    }
    Ok(Some(parents))
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct DiffSize(u64);

impl DiffSize {
    #[cfg(test)]
    pub(crate) fn new(bytes: u64) -> Self {
        Self(bytes)
    }
    #[must_use]
    pub fn bytes(self) -> u64 {
        self.0
    }

    fn add(&mut self, bytes: u64) -> Result<()> {
        self.0 = self
            .0
            .checked_add(bytes)
            .ok_or_else(|| Error::MalformedOci("layer diff size overflow".into()))?;
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Report {
    pub entries: u64,
    pub whiteouts: u64,
    /// Sum of tar header content sizes, matching Moby `UnpackLayer`/graphdriver `ApplyDiff`.
    pub diff_size: DiffSize,
}

struct Directory {
    path: Path,
    destination: PathBuf,
    mode: u32,
}

struct HardLink {
    path: Path,
    target: PathBuf,
    destination: PathBuf,
}

#[derive(Default)]
struct HardLinks(Vec<HardLink>);

impl HardLinks {
    fn supersede(&mut self, destination: &FsPath) {
        self.0.retain(|link| link.destination != destination);
    }

    fn finish(mut self) -> Result<()> {
        if self.0.is_empty() {
            return Ok(());
        }
        for _ in 0..self.0.len() {
            let (remaining, progress) = Self::attempt(self.0)?;
            self.0 = remaining;
            if self.0.is_empty() {
                return Ok(());
            }
            if !progress {
                break;
            }
        }
        let link = self.0.first().expect("unresolved hard-link set is non-empty");
        Err(link
            .path
            .error("hardlink target was not present after applying the complete layer"))
    }

    /// Links every pending entry once, reporting those still missing a target and whether any linked.
    fn attempt(links: Vec<HardLink>) -> Result<(Vec<HardLink>, bool)> {
        let mut remaining = Vec::new();
        let mut progress = false;
        for link in links {
            match fs::hard_link(&link.target, &link.destination) {
                Ok(()) => progress = true,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => remaining.push(link),
                Err(error) => return Err(link.path.io("create deferred hard link", error)),
            }
        }
        Ok((remaining, progress))
    }
}

#[derive(Default)]
struct Directories(Vec<Directory>);

#[derive(Default)]
/// Work that only the complete layer can settle: directory modes a later entry may supersede, and hard
/// links whose target has not been unpacked yet.
struct Backlog {
    directories: Directories,
    hard_links: HardLinks,
}

impl Directories {
    fn push(&mut self, path: Path, destination: PathBuf, mode: u32) {
        self.0.push(Directory {
            path,
            destination,
            mode,
        });
    }

    fn finish(mut self, ownerships: Option<&mut Ownerships>) -> Result<()> {
        self.0
            .sort_by_key(|directory| std::cmp::Reverse(directory.destination.components().count()));
        for directory in self.0 {
            // Directory modes are deferred so a restrictive parent cannot prevent later entries
            // from being unpacked. If a later entry replaces the path, the deferred metadata
            // belongs to the superseded directory rather than the final node.
            let Ok(metadata) = fs::symlink_metadata(&directory.destination) else {
                continue;
            };
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                directory.path.set_mode(&directory.destination, directory.mode)?;
            }
        }
        if let Some(ownerships) = ownerships {
            ownerships.flush()?;
        }
        Ok(())
    }
}

/// A streaming, uncompressed OCI filesystem layer.
pub struct Layer<R> {
    reader: R,
}

impl<R: Read> Layer<R> {
    /// Construct a layer from its uncompressed tar stream.
    pub fn new(reader: R) -> Self {
        Self { reader }
    }

    /// Return the underlying stream after applying the layer.
    pub fn into_inner(self) -> R {
        self.reader
    }

    /// Apply this layer without invoking an external archiver.
    ///
    /// # Errors
    /// Returns an error for malformed archives, unsafe paths or nodes, or filesystem failures.
    pub fn apply(&mut self, root: impl AsRef<FsPath>) -> Result<Report> {
        self.apply_to(root.as_ref(), None, None)
    }

    /// Apply this layer and preserve its guest uid/gid metadata separately
    /// from host filesystem ownership.
    ///
    /// # Errors
    /// Returns an error for malformed archives, unsafe paths or nodes,
    /// ownership persistence failures, or filesystem failures.
    pub fn apply_with_ownership(&mut self, root: impl AsRef<FsPath>, ownerships: &mut Ownerships) -> Result<Report> {
        self.apply_to(root.as_ref(), Some(ownerships), None)
    }

    /// Apply a layer while preserving guest ownership and case-distinct names.
    ///
    /// # Errors
    /// Returns an error for malformed archives, unsafe paths or nodes, metadata persistence
    /// failures, or filesystem failures.
    pub fn apply_with_metadata(
        &mut self,
        root: impl AsRef<FsPath>,
        ownerships: &mut Ownerships,
        names: &mut Names,
    ) -> Result<Report> {
        self.apply_to(root.as_ref(), Some(ownerships), Some(names))
    }

    fn apply_to(
        &mut self,
        root: &FsPath,
        mut ownerships: Option<&mut Ownerships>,
        mut names: Option<&mut Names>,
    ) -> Result<Report> {
        fs::create_dir_all(root).at(root)?;
        let root = root.canonicalize().at(root)?;
        let mut archive = tar::Archive::new(&mut self.reader);
        let mut report = Report::default();
        let mut backlog = Backlog::default();
        let mut prepared_parent: Option<(PathBuf, super::path::Parents)> = None;
        for item in archive.entries()? {
            let mut entry = item?;
            // Moby accounts every header before interpreting its kind. Directories,
            // symlinks, hardlinks, and whiteouts conventionally contribute zero;
            // sparse regular files contribute their logical header size. Tar block,
            // PAX, compression, and other container overhead is excluded.
            report.diff_size.add(entry.header().size()?)?;
            let raw = entry.path()?;
            if entry.header().entry_type().is_dir()
                && raw
                    .components()
                    .all(|part| matches!(part, std::path::Component::CurDir))
            {
                continue;
            }
            let path = Path::new(&raw)?;
            if path.name().starts_with(".wh.") {
                prepared_parent = None;
            }
            if path.apply_whiteout(&root, ownerships.as_deref_mut(), names.as_deref_mut())? {
                report.whiteouts += 1;
                continue;
            }
            let physical = names.as_deref_mut().map_or_else(
                || Ok(path.as_path().to_owned()),
                |names| names.resolve(&root, path.as_path()),
            )?;
            let physical_path = Path::new(&physical)?;
            let destination = physical_path.destination(&root);
            backlog.hard_links.supersede(&destination);
            let kind = entry.header().entry_type();
            let parent = physical_path.as_path().parent().unwrap_or(FsPath::new(""));
            let can_reuse_parent =
                reuse_parent_validation() && matches!(kind, tar::EntryType::Regular | tar::EntryType::GNUSparse);
            let entry_parents =
                prepare_entry_parent(&physical_path, &root, parent, can_reuse_parent, &mut prepared_parent)?;
            let ownership = Ownership::from_header(entry.header(), &path)?;
            if path.is_device()
                && matches!(
                    kind,
                    tar::EntryType::Char | tar::EntryType::Block | tar::EntryType::Fifo
                )
            {
                continue;
            }
            path.apply_entry(
                &mut entry,
                &physical,
                &destination,
                &root,
                names.as_deref_mut(),
                &mut backlog,
            )?;
            if !can_reuse_parent {
                prepared_parent = None;
            }
            drop(entry_parents);
            if let Some(ownerships) = ownerships.as_deref_mut() {
                ownerships.record(path.as_path(), ownership)?;
            }
            report.entries += 1;
        }
        backlog.hard_links.finish()?;
        backlog.directories.finish(ownerships)?;
        Ok(report)
    }
}

impl Path {
    fn apply_entry<R: Read>(
        &self,
        entry: &mut tar::Entry<'_, R>,
        physical: &FsPath,
        destination: &FsPath,
        root: &FsPath,
        names: Option<&mut Names>,
        backlog: &mut Backlog,
    ) -> Result<()> {
        let path = self;
        match entry.header().entry_type() {
            tar::EntryType::Directory => {
                if let Ok(metadata) = fs::symlink_metadata(destination)
                    && (!metadata.is_dir() || metadata.file_type().is_symlink())
                {
                    path.remove(destination)?;
                }
                fs::create_dir_all(destination).map_err(|source| path.io("create directory", source))?;
                backlog
                    .directories
                    .push(path.clone(), destination.to_owned(), entry.header().mode()?);
            }
            tar::EntryType::Regular | tar::EntryType::GNUSparse => {
                fs::create_dir_all(destination.parent().unwrap_or(root))
                    .map_err(|source| path.io("create parent directory", source))?;
                if fs::symlink_metadata(destination).is_ok() {
                    path.remove(destination)?;
                }
                let mut output = fs::OpenOptions::new()
                    .create(true)
                    .truncate(true)
                    .write(true)
                    .open(destination)
                    .map_err(|source| path.io("create regular file", source))?;
                std::io::copy(entry, &mut output)?;
                path.set_mode(destination, entry.header().mode()?)?;
            }
            tar::EntryType::Symlink => {
                let target = entry.link_name()?.ok_or_else(|| path.error("missing symlink target"))?;
                path.validate_link(&target)?;
                let target = if target.is_absolute() {
                    target.into_owned()
                } else if let Some(names) = names {
                    path.link_target(physical, &target, root, names)?
                } else {
                    target.into_owned()
                };
                fs::create_dir_all(destination.parent().unwrap_or(root))
                    .map_err(|source| path.io("create parent directory", source))?;
                if fs::symlink_metadata(destination).is_ok() {
                    path.remove(destination)?;
                }
                #[cfg(unix)]
                std::os::unix::fs::symlink(target, destination)
                    .map_err(|source| path.io("create symbolic link", source))?;
                #[cfg(not(unix))]
                return Err(path.error("symlinks are unsupported on this host"));
            }
            tar::EntryType::Link => {
                let target = entry
                    .link_name()?
                    .ok_or_else(|| path.error("missing hardlink target"))?;
                let target = Path::new(&target)?;
                let physical_target = names.map_or_else(
                    || Ok(target.as_path().to_owned()),
                    |names| names.resolve(root, target.as_path()),
                )?;
                let physical_target = Path::new(&physical_target)?;
                let _target_parents = physical_target.prepare(root)?;
                fs::create_dir_all(destination.parent().unwrap_or(root))
                    .map_err(|source| path.io("create parent directory", source))?;
                if fs::symlink_metadata(destination).is_ok() {
                    path.remove(destination)?;
                }
                let target = physical_target.destination(root);
                match fs::hard_link(&target, destination) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        backlog.hard_links.0.push(HardLink {
                            path: path.clone(),
                            target,
                            destination: destination.to_owned(),
                        });
                    }
                    Err(error) => return Err(path.io("create hard link", error)),
                }
            }
            _ => return Err(path.error("special filesystem node is forbidden")),
        }
        Ok(())
    }

    fn link_target(
        &self,
        physical_link: &FsPath,
        target: &FsPath,
        root: &FsPath,
        names: &mut Names,
    ) -> Result<PathBuf> {
        let joined = self.as_path().parent().unwrap_or(FsPath::new("")).join(target);
        let guest_target = Self::normalize(&joined)?;
        let physical_target = names.resolve(root, guest_target.as_path())?;
        let physical_target = Self::new(&physical_target)?;
        Ok(physical_target.relative_to(physical_link.parent().unwrap_or(FsPath::new(""))))
    }

    fn apply_opaque(
        &self,
        root: &FsPath,
        ownerships: Option<&mut Ownerships>,
        names: Option<&mut Names>,
    ) -> Result<()> {
        let parent = self
            .as_path()
            .parent()
            .ok_or_else(|| self.error("opaque whiteout has no parent"))?;
        if let Some(ownerships) = ownerships {
            ownerships.discard_tree(parent, false)?;
        }
        let physical = names.map_or_else(|| Ok(parent.to_owned()), |names| names.resolve(root, parent))?;
        let directory = Self::new(&physical)?.destination(root);
        if !directory.exists() {
            return Ok(());
        }
        for child in fs::read_dir(directory)? {
            self.remove(&child?.path())?;
        }
        Ok(())
    }

    fn apply_whiteout(
        &self,
        root: &FsPath,
        ownerships: Option<&mut Ownerships>,
        mut names: Option<&mut Names>,
    ) -> Result<bool> {
        let path = self;
        let name = path.name();
        if name == ".wh..wh..opq" {
            self.apply_opaque(root, ownerships, names.as_deref_mut())?;
            return Ok(true);
        }
        let Some(target) = name.strip_prefix(".wh.") else {
            return Ok(false);
        };
        if target.is_empty() {
            // Some Docker-produced layers contain the bare marker. It has no victim;
            // consume it without allowing an empty target to resolve to its parent.
            return Ok(true);
        }
        let guest_victim = path.as_path().parent().unwrap_or(FsPath::new("")).join(target);
        let physical_victim =
            names.map_or_else(|| Ok(guest_victim.clone()), |names| names.resolve(root, &guest_victim))?;
        let victim = root.join(physical_victim);
        if let Some(ownerships) = ownerships {
            let victim = path.as_path().parent().unwrap_or(FsPath::new("")).join(target);
            ownerships.discard_tree(&victim, true)?;
        }
        if victim.exists() || victim.symlink_metadata().is_ok() {
            path.remove(&victim)?;
        }
        Ok(true)
    }
}

impl Ownership {
    fn from_header(header: &tar::Header, path: &Path) -> Result<Self> {
        let uid = if header.as_old().uid.iter().all(|byte| matches!(byte, 0 | b' ')) {
            0
        } else {
            header.uid()?
        };
        let gid = if header.as_old().gid.iter().all(|byte| matches!(byte, 0 | b' ')) {
            0
        } else {
            header.gid()?
        };
        Ok(Self {
            uid: uid.try_into().map_err(|_| {
                Error::InvalidMetadata(format!("layer uid exceeds u32 for {}", path.as_path().display()))
            })?,
            gid: gid.try_into().map_err(|_| {
                Error::InvalidMetadata(format!("layer gid exceeds u32 for {}", path.as_path().display()))
            })?,
        })
    }
}

#[cfg(test)]
mod diff_size_tests {
    use super::{DiffSize, Layer};
    use std::io::Cursor;

    fn regular_entries(paths: impl IntoIterator<Item = String>) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut archive = tar::Builder::new(&mut bytes);
            for path in paths {
                let mut header = tar::Header::new_gnu();
                header.set_size(0);
                header.set_mode(0o644);
                header.set_cksum();
                archive.append_data(&mut header, path, &[][..]).unwrap();
            }
            archive.finish().unwrap();
        }
        bytes
    }

    #[test]
    fn rejects_overflow_instead_of_wrapping_layer_accounting() {
        let mut size = DiffSize(u64::MAX);

        assert!(size.add(1).is_err());
        assert_eq!(size.bytes(), u64::MAX);
    }

    #[test]
    fn consecutive_siblings_validate_their_shared_parent_once() {
        let paths = (0..64).map(|index| format!("one/two/three/four/file-{index}"));
        let bytes = regular_entries(paths);
        let root = tempfile::tempdir().unwrap();
        super::super::path::take_prepared_components();

        let report = Layer::new(Cursor::new(bytes)).apply(root.path()).unwrap();

        assert_eq!(report.entries, 64);
        assert_eq!(
            super::super::path::take_prepared_components(),
            4,
            "the four-component parent was revalidated for a sibling"
        );
    }

    #[cfg(unix)]
    #[test]
    fn replacing_a_cached_parent_with_a_symlink_forces_revalidation() {
        let outside = tempfile::tempdir().unwrap();
        let mut bytes = regular_entries(["safe/seed".to_owned()]);
        bytes.truncate(bytes.len() - 1024);
        {
            let mut archive = tar::Builder::new(&mut bytes);
            let mut link = tar::Header::new_gnu();
            link.set_entry_type(tar::EntryType::Symlink);
            link.set_size(0);
            link.set_mode(0o777);
            link.set_link_name(outside.path()).unwrap();
            link.set_cksum();
            archive.append_data(&mut link, "safe", &[][..]).unwrap();

            let mut file = tar::Header::new_gnu();
            file.set_size(0);
            file.set_mode(0o644);
            file.set_cksum();
            archive.append_data(&mut file, "safe/escaped", &[][..]).unwrap();
            archive.finish().unwrap();
        }
        let root = tempfile::tempdir().unwrap();

        assert!(Layer::new(Cursor::new(bytes)).apply(root.path()).is_err());
        assert!(!outside.path().join("escaped").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_reused_readonly_parent_is_restored_after_its_last_sibling() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = tempfile::tempdir().unwrap();
        let locked = root.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
        let bytes = regular_entries(["locked/one".to_owned(), "locked/two".to_owned()]);

        Layer::new(Cursor::new(bytes)).apply(root.path()).unwrap();

        assert!(locked.join("one").is_file());
        assert!(locked.join("two").is_file());
        assert_eq!(
            std::fs::symlink_metadata(locked).unwrap().permissions().mode() & 0o777,
            0o555
        );
    }

    #[test]
    #[ignore = "manual OCI extraction census used by /var/tmp/layer-parent-abba.sh"]
    fn layer_apply_census() {
        let archive = std::env::var_os("HL_LAYER_ARCHIVE").unwrap();
        let root = std::env::var_os("HL_LAYER_ROOT").unwrap();
        let mode = std::env::var("HL_LAYER_MODE").unwrap();
        let started = std::time::Instant::now();
        let entries = if matches!(mode.as_str(), "candidate" | "baseline") {
            super::REUSE_PARENT_VALIDATION.with(|reuse| reuse.set(mode == "candidate"));
            let file = std::fs::File::open(archive).unwrap();
            Layer::new(std::io::BufReader::new(file))
                .apply(std::path::Path::new(&root))
                .unwrap()
                .entries
        } else {
            assert_eq!(mode, "null");
            0
        };
        println!(
            "layer-apply mode={mode} entries={entries} elapsed_ns={}",
            started.elapsed().as_nanos()
        );
    }
}
