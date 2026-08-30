//! Resolution of a guest entry path to the host path that backs it.

use std::ffi::OsString;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt as _;
use std::path::{Component, Path, PathBuf};

/// A guest path, resolved against the layered rootfs that backs it rather than against the host
/// root.
///
/// Every launch that carries a rootfs needs this: `RuntimePlan::executable_host` names a path the
/// *host* opens, while the command line names a path the *guest* sees, and the two are only the
/// same when no component of the guest path is a symbolic link. A stock distribution image makes
/// them differ immediately -- Alpine ships `/bin/sh -> /bin/busybox`, an **absolute** target -- so
/// a plain `rootfs.join(entry)` hands the host a link whose target it resolves from `/`, which is
/// the wrong root. It is not an escape (the host path it names is refused as well) but the guest
/// cannot start its own shell.
///
/// Resolution therefore walks the guest path one component at a time, reads each symbolic link
/// itself, and re-anchors an absolute target at the rootfs. `..` pops the accumulated guest
/// components and cannot climb past the rootfs, so a link pointing outside the image resolves to
/// nothing rather than to a host path.
pub struct GuestPath;

impl GuestPath {
    /// How many links a single resolution may follow before it is treated as a loop, matching the
    /// kernel's `SYMLOOP_MAX`-style bound. Each pass follows at most one link.
    const LINK_LIMIT: usize = 40;

    /// Resolves `program` -- a guest-absolute path -- to the host path that backs it, searching
    /// `roots` in order and returning `None` when no root supplies an executable file.
    #[must_use]
    pub fn host_executable(program: &Path, roots: &[PathBuf]) -> Option<PathBuf> {
        let mut pending = Self::guest_components(program)?;
        for _ in 0..Self::LINK_LIMIT {
            let mut prefix = Vec::new();
            let mut followed = false;
            for index in 0..pending.len() {
                prefix.push(pending[index].clone());
                let relative = prefix.iter().collect::<PathBuf>();
                let (root, metadata) = roots.iter().find_map(|root| {
                    std::fs::symlink_metadata(root.join(&relative))
                        .ok()
                        .map(|metadata| (root, metadata))
                })?;
                if metadata.file_type().is_symlink() {
                    let target = std::fs::read_link(root.join(relative)).ok()?;
                    let mut replacement = if target.is_absolute() {
                        Vec::new()
                    } else {
                        prefix[..prefix.len() - 1].to_vec()
                    };
                    Self::append_guest_components(&mut replacement, &target)?;
                    replacement.extend_from_slice(&pending[index + 1..]);
                    pending = replacement;
                    followed = true;
                    break;
                }
            }
            if followed {
                continue;
            }
            let relative = pending.iter().collect::<PathBuf>();
            return roots
                .iter()
                .map(|root| root.join(&relative))
                .find(|candidate| Self::executable_here(candidate));
        }
        None
    }

    /// Return the normalized guest interpreter named by a validated ELF executable.
    #[cfg(unix)]
    pub fn interpreter(
        executable: &Path,
        isa: crate::activation::GuestIsa,
    ) -> Result<Option<PathBuf>, crate::engine::EngineError> {
        let isa = match isa {
            crate::activation::GuestIsa::Aarch64 => 1,
            crate::activation::GuestIsa::X86_64 => 2,
        };
        hl_native::executable_interpreter(executable, isa)
            .map(|path| path.map(|bytes| PathBuf::from(std::ffi::OsString::from_vec(bytes))))
            .map_err(|_| crate::engine::EngineError::LaunchFailed)
    }

    /// Whether a host path is a file the host would agree to execute.
    #[cfg(unix)]
    #[must_use]
    pub fn executable_here(path: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    }

    /// Whether a host path is a file the host would agree to execute.
    ///
    /// A non-Unix host carries no execute bit, so "is a regular file" is the strongest answer
    /// available there; the guest image's own mode is not visible through this API.
    #[cfg(not(unix))]
    #[must_use]
    pub fn executable_here(path: &Path) -> bool {
        std::fs::metadata(path).is_ok_and(|meta| meta.is_file())
    }

    fn guest_components(path: &Path) -> Option<Vec<OsString>> {
        let mut output = Vec::new();
        Self::append_guest_components(&mut output, path)?;
        Some(output)
    }

    fn append_guest_components(output: &mut Vec<OsString>, path: &Path) -> Option<()> {
        for component in path.components() {
            match component {
                Component::RootDir | Component::CurDir => {}
                Component::Normal(value) => output.push(value.to_owned()),
                Component::ParentDir => {
                    output.pop()?;
                }
                Component::Prefix(_) => return None,
            }
        }
        Some(())
    }
}

#[cfg(test)]
#[path = "entry_test.rs"]
mod tests;
