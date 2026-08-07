use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

use hl_runtime::{GuestPath, GuestPathBytes};

/// One active host-root to guest-root identity mapping.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Projection {
    host: PathBuf,
    guest: GuestPath,
    active: bool,
}

impl Projection {
    pub(super) fn new(host: PathBuf, guest: GuestPath, active: bool) -> Result<Self, ProjectionError> {
        if !host.is_absolute() || !guest.as_str().starts_with('/') {
            return Err(ProjectionError::Relative);
        }
        Ok(Self { host, guest, active })
    }
}

/// Reverse projection for cwd, fd links, and checkpoint-visible paths.
pub(super) struct Projector {
    projections: Vec<Projection>,
}

impl Projector {
    pub(super) fn new(projections: Vec<Projection>) -> Self {
        Self { projections }
    }

    pub(super) fn guest(&self, host: &Path) -> Result<Option<GuestPathBytes>, ProjectionError> {
        if !host.is_absolute() {
            return Err(ProjectionError::Relative);
        }
        let selected = self
            .projections
            .iter()
            .filter(|projection| projection.active && host.strip_prefix(&projection.host).is_ok())
            .max_by_key(|projection| projection.host.as_os_str().as_bytes().len());
        let Some(selected) = selected else {
            return Ok(None);
        };
        let suffix = host
            .strip_prefix(&selected.host)
            .map_err(|_| ProjectionError::Outside)?;
        let mut guest = selected.guest.as_str().as_bytes().to_vec();
        if guest != b"/" {
            while guest.last() == Some(&b'/') {
                guest.pop();
            }
        }
        for component in suffix.components() {
            let Component::Normal(component) = component else {
                return Err(ProjectionError::InvalidHost);
            };
            if guest != b"/" {
                guest.push(b'/');
            }
            guest.extend_from_slice(component.as_bytes());
        }
        GuestPathBytes::new(&guest)
            .map(Some)
            .map_err(|_| ProjectionError::GuestPath)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ProjectionError {
    Relative,
    Outside,
    InvalidHost,
    GuestPath,
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    use std::path::PathBuf;

    use hl_runtime::GuestPath;

    use super::{Projection, Projector};

    fn projection(host: &str, guest: &[u8], active: bool) -> Projection {
        Projection::new(
            PathBuf::from(host),
            GuestPath::new(std::str::from_utf8(guest).unwrap()).unwrap(),
            active,
        )
        .unwrap()
    }

    #[test]
    fn longest_component_prefix_selects_nested_mount() {
        let projector = Projector::new(vec![
            projection("/layers/lower", b"/", true),
            projection("/layers/lower/scratch", b"/tmp", true),
        ]);

        assert_eq!(
            projector
                .guest(PathBuf::from("/layers/lower/scratch/run/a").as_path())
                .unwrap()
                .unwrap()
                .as_bytes(),
            b"/tmp/run/a",
        );
        assert_eq!(
            projector
                .guest(PathBuf::from("/layers/lower/usr/bin").as_path())
                .unwrap()
                .unwrap()
                .as_bytes(),
            b"/usr/bin",
        );
    }

    #[test]
    fn component_boundary_and_active_lifetime_prevent_prefix_leaks() {
        let projector = Projector::new(vec![
            projection("/images/root", b"/", true),
            projection("/images/root/data", b"/secret", false),
        ]);

        assert!(
            projector
                .guest(PathBuf::from("/images/rooted/etc").as_path())
                .unwrap()
                .is_none()
        );
        assert_eq!(
            projector
                .guest(PathBuf::from("/images/root/data/item").as_path())
                .unwrap()
                .unwrap()
                .as_bytes(),
            b"/data/item",
        );
    }

    #[test]
    fn projection_preserves_non_utf8_guest_suffix() {
        let projector = Projector::new(vec![projection("/images/root", b"/", true)]);
        let mut host = PathBuf::from("/images/root");
        host.push(OsString::from_vec(vec![0xff, b'x']));

        assert_eq!(projector.guest(&host).unwrap().unwrap().as_bytes(), b"/\xffx");
    }
}
