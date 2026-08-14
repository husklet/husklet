use std::{
    fs::File,
    io::Read,
    os::unix::ffi::OsStrExt as _,
    path::{Component, Path},
};

use super::{EngineConfig, EntryKind, entry_is_opaque, launch_roots, layered_entry, open_components};

pub(super) fn pin_guest_image(config: &EngineConfig<'_>, guest: &[u8]) -> Result<Vec<u8>, i32> {
    let guest = Path::new(std::ffi::OsStr::from_bytes(guest));
    let roots = launch_roots(config)?;
    let mut file = if roots.is_empty() {
        File::open(guest).map_err(|_| 1)?
    } else {
        resolve_layered_guest(guest, &roots).map_err(|_| 1)?.ok_or(1)?
    };
    let size = usize::try_from(file.metadata().map_err(|_| 1)?.len()).map_err(|_| 1)?;
    if size == 0 || size > 64 * 1024 * 1024 {
        return Err(1);
    }
    let mut image = Vec::with_capacity(size);
    file.read_to_end(&mut image).map_err(|_| 1)?;
    (image.len() == size).then_some(image).ok_or(1)
}

pub(super) fn resolve_layered_guest(guest: &Path, roots: &[File]) -> std::io::Result<Option<File>> {
    let mut pending = guest
        .components()
        .filter_map(|component| match component {
            Component::RootDir | Component::CurDir => None,
            Component::Normal(value) => Some(value.to_owned()),
            Component::ParentDir | Component::Prefix(_) => Some(std::ffi::OsString::new()),
        })
        .collect::<Vec<_>>();
    if pending
        .iter()
        .any(|part| part.is_empty() || part.as_encoded_bytes().starts_with(b".wh."))
    {
        return Ok(None);
    }
    for _ in 0..40 {
        match resolve_pass(&pending, roots)? {
            Resolution::Restart(replacement) => pending = replacement,
            Resolution::File(file) => return Ok(Some(file)),
            Resolution::Missing => return Ok(None),
        }
    }
    Ok(None)
}

enum Resolution {
    Restart(Vec<std::ffi::OsString>),
    File(File),
    Missing,
}

fn resolve_pass(pending: &[std::ffi::OsString], roots: &[File]) -> std::io::Result<Resolution> {
    let mut prefix = Vec::new();
    let mut layer_limit = roots.len();
    for index in 0..pending.len() {
        prefix.push(pending[index].clone());
        let Some((root, kind)) = layered_entry(&prefix, &roots[..layer_limit])? else {
            return Ok(Resolution::Missing);
        };
        if let EntryKind::Symlink(target) = kind {
            return Ok(symlink_target(&target, &prefix, &pending[index + 1..])
                .map_or(Resolution::Missing, Resolution::Restart));
        }
        if index + 1 < pending.len() && !matches!(kind, EntryKind::Directory) {
            return Err(std::io::Error::from_raw_os_error(libc::ENOTDIR));
        }
        if matches!(kind, EntryKind::Directory) && entry_is_opaque(&roots[root], &prefix) {
            layer_limit = layer_limit.min(root + 1);
        }
    }
    let Some((root, EntryKind::Regular)) = layered_entry(pending, &roots[..layer_limit])? else {
        return Ok(Resolution::Missing);
    };
    open_components(&roots[root], pending, false).map(Resolution::File)
}

fn symlink_target(
    target: &Path,
    prefix: &[std::ffi::OsString],
    remainder: &[std::ffi::OsString],
) -> Option<Vec<std::ffi::OsString>> {
    let mut replacement = if target.is_absolute() {
        Vec::new()
    } else {
        prefix[..prefix.len() - 1].to_vec()
    };
    for component in target.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(value) => replacement.push(value.to_owned()),
            Component::ParentDir => replacement.pop().map(|_| ())?,
            Component::Prefix(_) => return None,
        }
    }
    replacement.extend_from_slice(remainder);
    Some(replacement)
}
