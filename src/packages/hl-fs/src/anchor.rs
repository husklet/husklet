use std::ffi::{CStr, CString};
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

static RESERVATION_ID: AtomicU64 = AtomicU64::new(0);

/// A directory held by descriptor, so renaming or replacing its pathname cannot redirect
/// operations issued against it.
#[derive(Debug)]
pub struct Anchor(OwnedFd);

impl Anchor {
    /// Creates the directory when absent and holds the resulting directory itself.
    ///
    /// # Errors
    /// Returns the creation failure, or the open failure when the name is not a directory,
    /// is a symbolic link, or was replaced before it could be held.
    pub fn create(path: &Path, mode: u32) -> io::Result<Arc<Self>> {
        let name = Self::name(path.as_os_str().as_bytes())?;
        match rustix::fs::mkdirat(rustix::fs::CWD, &name, rustix::fs::Mode::from_raw_mode(mode)) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => {}
            Err(error) => return Err(error.into()),
        }
        let directory = rustix::fs::openat(
            rustix::fs::CWD,
            &name,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW,
            rustix::fs::Mode::empty(),
        )?;
        Ok(Arc::new(Self(directory)))
    }

    fn name(bytes: &[u8]) -> io::Result<CString> {
        CString::new(bytes).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "name contains NUL"))
    }

    fn remove(&self, name: &CStr) {
        let _ = rustix::fs::unlinkat(&self.0, name, rustix::fs::AtFlags::empty());
    }
}

struct Reservation {
    anchor: Arc<Anchor>,
    staged: Option<CString>,
    name: CString,
    path: Vec<u8>,
    committed: bool,
}

impl Reservation {
    fn occupied(&self) -> &CStr {
        match &self.staged {
            Some(staged) if !self.committed => staged,
            _ => &self.name,
        }
    }
}

fn identity(status: &rustix::fs::Stat) -> (u64, u64) {
    (status.st_dev as u64, status.st_ino as u64)
}

/// A set of names created under quarantine and moved into place as one transaction.
///
/// Each name is committed with a no-replace rename inside its held directory, so an existing
/// name is never clobbered and a parent rename cannot redirect the commit. Withdrawal removes
/// names in the reverse of the order they were reserved.
#[derive(Default)]
pub struct Publication {
    reservations: Vec<Reservation>,
}

impl std::fmt::Debug for Publication {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Publication").finish_non_exhaustive()
    }
}

impl Publication {
    /// Takes ownership of `name`, which the caller already created at `path`, after confirming that
    /// `path` still resolves to the very entry `anchor` holds.
    ///
    /// Use this for names a peer protocol requires the creating call to address by pathname, such as
    /// an `AF_UNIX` bind whose pathname `getsockname` reports. Ownership is anchored from here on, so
    /// withdrawal cannot be redirected onto another directory.
    ///
    /// # Errors
    /// Returns [`io::ErrorKind::NotFound`] when the pathname no longer names the held entry, and the
    /// name is left untouched because it is not this owner's.
    pub fn adopt(&mut self, anchor: &Arc<Anchor>, name: &[u8], path: Vec<u8>) -> io::Result<()> {
        let name = Anchor::name(name)?;
        let absolute = Anchor::name(&path)?;
        let held = rustix::fs::statat(&anchor.0, &name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)?;
        let resolved = rustix::fs::statat(rustix::fs::CWD, &absolute, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)?;
        if identity(&held) != identity(&resolved) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "the pathname no longer names the held entry",
            ));
        }
        self.reservations.push(Reservation {
            anchor: Arc::clone(anchor),
            staged: None,
            name,
            path,
            committed: true,
        });
        Ok(())
    }

    /// Reserves `name` under `anchor` as a symbolic link to `target`.
    ///
    /// # Errors
    /// Returns the failure to encode the name or target, or the link creation failure.
    pub fn reserve_link(&mut self, anchor: &Arc<Anchor>, name: &[u8], path: Vec<u8>, target: &[u8]) -> io::Result<()> {
        let target = Anchor::name(target)?;
        let reservation = self.stage(anchor, name, path)?;
        let staged = reservation
            .staged
            .as_ref()
            .expect("a staged reservation has a quarantine name");
        rustix::fs::symlinkat(&target, &anchor.0, staged)?;
        self.reservations.push(reservation);
        Ok(())
    }

    fn stage(&self, anchor: &Arc<Anchor>, name: &[u8], path: Vec<u8>) -> io::Result<Reservation> {
        let name = Anchor::name(name)?;
        let staged = Anchor::name(
            format!(
                ".hl-{:x}-{:x}",
                std::process::id(),
                RESERVATION_ID.fetch_add(1, Ordering::Relaxed)
            )
            .as_bytes(),
        )?;
        Ok(Reservation {
            anchor: Arc::clone(anchor),
            staged: Some(staged),
            name,
            path,
            committed: false,
        })
    }

    /// Moves every reserved name into place, in reservation order.
    ///
    /// # Errors
    /// Returns [`io::ErrorKind::AlreadyExists`] when a name is occupied, or the rename failure.
    /// Every name already committed by this call is withdrawn before the error is returned.
    pub fn commit(&mut self) -> io::Result<()> {
        for index in 0..self.reservations.len() {
            let reservation = &self.reservations[index];
            let Some(staged) = reservation.staged.as_ref().filter(|_| !reservation.committed) else {
                continue;
            };
            let result = rustix::fs::renameat_with(
                &reservation.anchor.0,
                staged,
                &reservation.anchor.0,
                &reservation.name,
                rustix::fs::RenameFlags::NOREPLACE,
            );
            match result {
                Ok(()) => self.reservations[index].committed = true,
                Err(error) => {
                    self.withdraw();
                    return Err(error.into());
                }
            }
        }
        Ok(())
    }

    /// Returns the full pathname of every reserved name, in reservation order.
    pub fn paths(&self) -> impl Iterator<Item = &[u8]> {
        self.reservations.iter().map(|reservation| reservation.path.as_slice())
    }

    fn withdraw(&mut self) {
        let reservations = std::mem::take(&mut self.reservations);
        withdraw(reservations, |reservation| {
            reservation.anchor.remove(reservation.occupied());
        });
    }
}

fn withdraw<T>(entries: Vec<T>, mut remove: impl FnMut(&T)) {
    for entry in entries.iter().rev() {
        remove(entry);
    }
}

impl Drop for Publication {
    fn drop(&mut self) {
        self.withdraw();
    }
}

#[cfg(test)]
mod tests {
    use super::{Anchor, Publication, withdraw};
    use std::io;

    fn names(path: &std::path::Path) -> Vec<String> {
        let mut entries = std::fs::read_dir(path)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        entries.sort();
        entries
    }

    fn adopt(publication: &mut Publication, anchor: &std::sync::Arc<Anchor>, directory: &std::path::Path, name: &str) {
        let path = directory.join(name);
        std::fs::write(&path, b"node").unwrap();
        publication
            .adopt(
                anchor,
                name.as_bytes(),
                path.to_string_lossy().into_owned().into_bytes(),
            )
            .unwrap();
    }

    #[test]
    fn a_linked_reservation_is_invisible_until_commit() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("switch");
        let anchor = Anchor::create(&directory, 0o700).unwrap();
        let mut publication = Publication::default();
        publication
            .reserve_link(&anchor, b"alias", b"/switch/alias".to_vec(), b"/switch/primary")
            .unwrap();

        assert_eq!(names(&directory).len(), 1);
        assert!(!directory.join("alias").exists());
        publication.commit().unwrap();
        assert_eq!(names(&directory), vec!["alias".to_owned()]);
        drop(publication);
        assert!(names(&directory).is_empty());
    }

    #[test]
    fn commit_reports_an_occupied_name_and_withdraws_everything() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("switch");
        let anchor = Anchor::create(&directory, 0o700).unwrap();
        std::fs::write(directory.join("alias"), b"foreign").unwrap();
        let mut publication = Publication::default();
        adopt(&mut publication, &anchor, &directory, "primary");
        publication
            .reserve_link(&anchor, b"alias", b"/switch/alias".to_vec(), b"/switch/primary")
            .unwrap();

        let error = publication.commit().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(names(&directory), vec!["alias".to_owned()]);
        assert_eq!(std::fs::read(directory.join("alias")).unwrap(), b"foreign");
    }

    #[test]
    fn a_renamed_parent_cannot_redirect_the_withdrawal() {
        let temporary = tempfile::tempdir().unwrap();
        let held = temporary.path().join("switch");
        let anchor = Anchor::create(&held, 0o700).unwrap();
        let mut publication = Publication::default();
        adopt(&mut publication, &anchor, &held, "10.0.0.2:80");

        let moved = temporary.path().join("switch.moved");
        std::fs::rename(&held, &moved).unwrap();
        std::fs::create_dir(&held).unwrap();
        std::fs::write(held.join("10.0.0.2:80"), b"foreign").unwrap();

        drop(publication);
        assert!(names(&moved).is_empty());
        assert_eq!(std::fs::read(held.join("10.0.0.2:80")).unwrap(), b"foreign");
    }

    #[test]
    fn a_renamed_parent_cannot_redirect_a_linked_commit() {
        let temporary = tempfile::tempdir().unwrap();
        let held = temporary.path().join("switch");
        let anchor = Anchor::create(&held, 0o700).unwrap();
        let mut publication = Publication::default();
        publication
            .reserve_link(&anchor, b"alias", b"/switch/alias".to_vec(), b"/switch/primary")
            .unwrap();

        let moved = temporary.path().join("switch.moved");
        std::fs::rename(&held, &moved).unwrap();
        std::fs::create_dir(&held).unwrap();
        std::fs::write(held.join("alias"), b"foreign").unwrap();

        publication.commit().unwrap();
        assert_eq!(names(&moved), vec!["alias".to_owned()]);
        assert_eq!(std::fs::read(held.join("alias")).unwrap(), b"foreign");
        drop(publication);
        assert!(names(&moved).is_empty());
        assert_eq!(std::fs::read(held.join("alias")).unwrap(), b"foreign");
    }

    #[test]
    fn adoption_refuses_a_name_the_held_directory_does_not_own() {
        let temporary = tempfile::tempdir().unwrap();
        let held = temporary.path().join("switch");
        let anchor = Anchor::create(&held, 0o700).unwrap();
        let moved = temporary.path().join("switch.moved");
        std::fs::rename(&held, &moved).unwrap();
        std::fs::create_dir(&held).unwrap();
        let redirected = held.join("10.0.0.2:80");
        std::fs::write(&redirected, b"redirected").unwrap();

        let mut publication = Publication::default();
        let error = publication
            .adopt(
                &anchor,
                b"10.0.0.2:80",
                redirected.to_string_lossy().into_owned().into_bytes(),
            )
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        drop(publication);
        assert_eq!(std::fs::read(&redirected).unwrap(), b"redirected");
    }

    #[test]
    fn a_replaced_directory_name_is_never_opened_through_a_link() {
        let temporary = tempfile::tempdir().unwrap();
        let elsewhere = temporary.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        let held = temporary.path().join("switch");
        std::os::unix::fs::symlink(&elsewhere, &held).unwrap();
        assert!(Anchor::create(&held, 0o700).is_err());
        assert!(names(&elsewhere).is_empty());
    }

    #[test]
    fn withdrawal_reverses_reservation_order() {
        let mut removed = Vec::new();
        withdraw(vec!["primary", "alias-one", "alias-two"], |entry| removed.push(*entry));
        assert_eq!(removed, vec!["alias-two", "alias-one", "primary"]);
    }
}
