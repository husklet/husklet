//! Content identity of a benchmark artifact: the hash a campaign pins a file or rootfs to.
//!
//! A campaign records a sha256 beside every artifact it measures, and a run is only evidence if
//! the bytes, mode, ownership, extended attributes and hard-link topology are still the ones the
//! receipt names. Identity is therefore a property of the artifact rather than of the campaign
//! that quotes it, and it is computed and verified here.

use super::definition::Artifact;
use crate::{record::FramedIdentity, suite::Error};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

pub(super) fn verify_artifact(label: &str, artifact: &Artifact, directory: bool) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(&artifact.path)?;
    let expected_type = if directory {
        metadata.is_dir()
    } else {
        metadata.is_file()
    };
    if !expected_type {
        return Err(format!("{label} has the wrong file type").into());
    }
    let observed = if directory {
        tree_hash(&artifact.path)?
    } else {
        file_hash(&artifact.path)?
    };
    if observed != artifact.sha256 {
        return Err(format!(
            "{label} sha256 changed: expected {}, observed {observed}",
            artifact.sha256
        )
        .into());
    }
    Ok(())
}

pub(super) fn tree_hash(root: &Path) -> Result<String, Error> {
    fn permissions(metadata: &fs::Metadata, identity: &mut FramedIdentity) -> Result<(), Error> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
            unix_attributes(metadata.permissions().mode(), metadata.uid(), metadata.gid(), identity)?;
        }
        #[cfg(not(unix))]
        identity.field(&[u8::from(metadata.permissions().readonly())])?;
        Ok(())
    }

    fn walk(
        root: &Path,
        directory: &Path,
        identity: &mut FramedIdentity,
        links: &mut BTreeMap<(u64, u64), PathBuf>,
    ) -> Result<(), Error> {
        let mut entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let relative = path.strip_prefix(root)?;
            identity.field(relative.as_os_str().as_encoded_bytes())?;
            let metadata = fs::symlink_metadata(&path)?;
            permissions(&metadata, identity)?;
            if !metadata.file_type().is_symlink() {
                attributes(&path, identity)?;
            }
            if metadata.file_type().is_symlink() {
                identity.field(b"L")?;
                identity.field(fs::read_link(path)?.as_os_str().as_encoded_bytes())?;
            } else if metadata.is_dir() {
                identity.field(b"D")?;
                walk(root, &path, identity, links)?;
            } else if metadata.is_file() {
                identity.field(b"F")?;
                hardlink(relative, &metadata, identity, links)?;
                identity.field(&fs::read(path)?)?;
            } else {
                return Err("rootfs contains an unsupported entry type".into());
            }
        }
        Ok(())
    }
    let mut identity = FramedIdentity::new(b"husklet-rootfs-tree-v4")?;
    permissions(&fs::symlink_metadata(root)?, &mut identity)?;
    attributes(root, &mut identity)?;
    walk(root, root, &mut identity, &mut BTreeMap::new())?;
    Ok(identity.finish())
}

fn attributes(path: &Path, identity: &mut FramedIdentity) -> Result<(), Error> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        let mut names = xattr::list(path)?.collect::<Vec<_>>();
        names.sort();
        identity.field(&(names.len() as u64).to_le_bytes())?;
        for name in names {
            identity.field(name.as_bytes())?;
            let value = xattr::get(path, &name)?.ok_or("rootfs xattr disappeared while hashing")?;
            identity.field(&value)?;
        }
    }
    #[cfg(not(unix))]
    identity.field(&0_u64.to_le_bytes())?;
    Ok(())
}

fn hardlink(
    relative: &Path,
    metadata: &fs::Metadata,
    identity: &mut FramedIdentity,
    links: &mut BTreeMap<(u64, u64), PathBuf>,
) -> Result<(), Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.nlink() > 1 {
            let first = links
                .entry((metadata.dev(), metadata.ino()))
                .or_insert_with(|| relative.to_owned());
            identity.field(b"H")?;
            identity.field(first.as_os_str().as_encoded_bytes())?;
            return Ok(());
        }
    }
    let _ = (relative, metadata, links);
    identity.field(b"U")?;
    Ok(())
}

#[cfg(unix)]
pub(super) fn unix_attributes(mode: u32, uid: u32, gid: u32, identity: &mut FramedIdentity) -> Result<(), Error> {
    identity.field(&(mode & 0o7777).to_le_bytes())?;
    identity.field(&uid.to_le_bytes())?;
    identity.field(&gid.to_le_bytes())?;
    Ok(())
}

pub(super) fn artifact_identity(path: &Path) -> Result<String, Error> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() {
        tree_hash(path)
    } else if metadata.is_file() {
        file_hash(path)
    } else {
        Err("benchmark artifact is neither a regular file nor a directory".into())
    }
}

pub(super) fn file_hash(path: &Path) -> Result<String, Error> {
    let metadata = fs::symlink_metadata(path)?;
    let mut identity = FramedIdentity::new(b"husklet-benchmark-file-v3")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        unix_attributes(
            metadata.permissions().mode(),
            metadata.uid(),
            metadata.gid(),
            &mut identity,
        )?;
        identity.field(&metadata.nlink().to_le_bytes())?;
    }
    #[cfg(not(unix))]
    identity.field(&[u8::from(metadata.permissions().readonly())])?;
    attributes(path, &mut identity)?;
    identity.field(&fs::read(path)?)?;
    Ok(identity.finish())
}

#[cfg(test)]
mod tests {
    use super::{Artifact, file_hash, tree_hash, unix_attributes, verify_artifact};
    use crate::record::FramedIdentity;
    use std::fs;

    #[test]
    fn regular_file_artifact_is_accepted() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("engine");
        fs::write(&path, b"engine").unwrap();
        let artifact = Artifact {
            sha256: file_hash(&path).unwrap(),
            path,
        };
        verify_artifact("engine", &artifact, false).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn executable_artifact_identity_includes_mode() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("engine");
        fs::write(&path, b"same engine bytes").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let before = file_hash(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        assert_ne!(before, file_hash(&path).unwrap());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn executable_artifact_identity_includes_extended_attributes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("engine");
        fs::write(&path, b"same engine bytes").unwrap();
        let before = file_hash(&path).unwrap();
        xattr::set(&path, "user.husklet-benchmark", b"changed capability").unwrap();
        assert_ne!(before, file_hash(&path).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn executable_artifact_identity_includes_hardlink_aliases() {
        let temporary = tempfile::tempdir().unwrap();
        let executable = temporary.path().join("engine");
        fs::write(&executable, b"engine").unwrap();
        let before = file_hash(&executable).unwrap();
        fs::hard_link(&executable, temporary.path().join("engine-alias")).unwrap();
        let after = file_hash(&executable).unwrap();
        assert_ne!(before, after, "a hard-link alias must change executable identity");
    }

    #[cfg(unix)]
    #[test]
    fn executable_artifact_cannot_be_a_symlink() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("engine-real");
        let path = directory.path().join("engine");
        fs::write(&target, b"engine").unwrap();
        symlink(&target, &path).unwrap();
        let artifact = Artifact {
            path,
            sha256: file_hash(&target).unwrap(),
        };
        assert!(verify_artifact("engine", &artifact, false).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rootfs_identity_includes_executable_permissions() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().unwrap();
        let guest = directory.path().join("guest");
        fs::write(&guest, b"same bytes").unwrap();
        fs::set_permissions(&guest, fs::Permissions::from_mode(0o644)).unwrap();
        let before = tree_hash(directory.path()).unwrap();
        fs::set_permissions(&guest, fs::Permissions::from_mode(0o755)).unwrap();
        let after = tree_hash(directory.path()).unwrap();
        assert_ne!(before, after, "chmod must change the rootfs artifact identity");
    }

    #[cfg(unix)]
    #[test]
    fn rootfs_identity_includes_ownership() {
        let attributes = |uid, gid| {
            let mut identity = FramedIdentity::new(b"ownership-test").unwrap();
            unix_attributes(0o755, uid, gid, &mut identity).unwrap();
            identity.finish()
        };
        assert_ne!(attributes(1000, 1000), attributes(1001, 1000));
        assert_ne!(attributes(1000, 1000), attributes(1000, 1001));
    }

    #[cfg(unix)]
    #[test]
    fn rootfs_identity_includes_hardlink_topology() {
        use std::os::unix::fs::PermissionsExt as _;

        let linked = tempfile::tempdir().unwrap();
        fs::write(linked.path().join("a"), b"same bytes").unwrap();
        fs::hard_link(linked.path().join("a"), linked.path().join("b")).unwrap();

        let copied = tempfile::tempdir().unwrap();
        fs::write(copied.path().join("a"), b"same bytes").unwrap();
        fs::write(copied.path().join("b"), b"same bytes").unwrap();
        for root in [linked.path(), copied.path()] {
            fs::set_permissions(root.join("a"), fs::Permissions::from_mode(0o644)).unwrap();
            fs::set_permissions(root.join("b"), fs::Permissions::from_mode(0o644)).unwrap();
        }
        assert_ne!(tree_hash(linked.path()).unwrap(), tree_hash(copied.path()).unwrap());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn rootfs_identity_includes_extended_attributes() {
        let directory = tempfile::tempdir().unwrap();
        let guest = directory.path().join("guest");
        fs::write(&guest, b"same bytes").unwrap();
        let before = tree_hash(directory.path()).unwrap();
        xattr::set(&guest, "user.husklet-benchmark", b"changed behavior").unwrap();
        assert_ne!(before, tree_hash(directory.path()).unwrap());
    }
}
