use std::{
    fs::{self, File},
    io::Write,
    path::Path,
};

use super::{Names, Ownerships, names::Name};
use crate::{Error, Result};

struct Entry<'a> {
    root: &'a Path,
    relative: &'a Path,
    ownerships: &'a Ownerships,
    names: Option<&'a Names>,
}

pub(super) fn write(root: &Path, ownerships: &Ownerships, writer: impl Write) -> Result<()> {
    write_names(root, ownerships, None, writer)
}

pub(super) fn write_names(
    root: &Path,
    ownerships: &Ownerships,
    names: Option<&Names>,
    writer: impl Write,
) -> Result<()> {
    let mut archive = tar::Builder::new(writer);
    archive.follow_symlinks(false);
    append_children(&mut archive, root, Path::new(""), ownerships, names)?;
    archive.finish()?;
    Ok(())
}

fn append_children<W: Write>(
    archive: &mut tar::Builder<W>,
    root: &Path,
    relative: &Path,
    ownerships: &Ownerships,
    names: Option<&Names>,
) -> Result<()> {
    let directory = root.join(relative);
    let original = fs::symlink_metadata(&directory)?.permissions();
    let mut accessible = original.clone();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        accessible.set_mode(accessible.mode() | 0o500);
    }
    #[cfg(not(unix))]
    accessible.set_readonly(false);
    fs::set_permissions(&directory, accessible)?;
    let appended = (|| -> Result<()> {
        let mut children = fs::read_dir(&directory)?.collect::<std::io::Result<Vec<_>>>()?;
        children.sort_by_key(std::fs::DirEntry::file_name);
        for child in children {
            let path = relative.join(child.file_name());
            Entry {
                root,
                relative: &path,
                ownerships,
                names,
            }
            .append(archive)?;
        }
        Ok(())
    })();
    let restored = fs::set_permissions(&directory, original);
    appended?;
    restored?;
    Ok(())
}

impl Entry<'_> {
    fn append<W: Write>(&self, archive: &mut tar::Builder<W>) -> Result<()> {
        let source = self.root.join(self.relative);
        let guest = self.names.map_or(self.relative, |names| names.guest(self.relative));
        let metadata = fs::symlink_metadata(&source)?;
        let mut header = tar::Header::new_gnu();
        header.set_metadata_in_mode(&metadata, tar::HeaderMode::Complete);
        header.set_mtime(0);
        let ownership = self
            .ownerships
            .get(guest)
            .unwrap_or(super::Ownership { uid: 0, gid: 0 });
        header.set_uid(u64::from(ownership.uid));
        header.set_gid(u64::from(ownership.gid));

        if metadata.is_dir() {
            header.set_entry_type(tar::EntryType::Directory);
            header.set_size(0);
            header.set_cksum();
            archive.append_data(&mut header, guest, &[][..])?;
            append_children(archive, self.root, self.relative, self.ownerships, self.names)?;
        } else if metadata.file_type().is_symlink() {
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            let target = fs::read_link(&source)?;
            let target = self.names.map_or(target.clone(), |names| {
                if target.is_absolute() {
                    return target.clone();
                }
                let joined = self.relative.parent().unwrap_or(Path::new("")).join(&target);
                let physical = Name::normalize(&joined);
                let guest_target = names.guest(&physical);
                Name::relative(guest.parent().unwrap_or(Path::new("")), guest_target)
            });
            header.set_link_name(target)?;
            header.set_cksum();
            archive.append_data(&mut header, guest, &[][..])?;
        } else if metadata.is_file() {
            header.set_entry_type(tar::EntryType::Regular);
            header.set_size(metadata.len());
            header.set_cksum();
            archive.append_data(&mut header, guest, File::open(&source)?)?;
        } else {
            return Err(Error::UnsafeArchive {
                path: self.relative.to_owned(),
                reason: "special filesystem node is forbidden",
            });
        }
        Ok(())
    }
}
