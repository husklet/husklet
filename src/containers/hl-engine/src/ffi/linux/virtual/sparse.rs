use std::collections::BTreeMap;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::sync::{Arc, Mutex};

use hl_isa::AddressRange;

use super::abi;
use super::virtual_memory::MemoryError;

#[derive(Debug)]
struct Backing {
    address: usize,
    length: usize,
    _file: Option<File>,
}

unsafe impl Send for Backing {}
unsafe impl Sync for Backing {}

impl Backing {
    #[cfg(test)]
    fn anonymous(length: u64) -> Result<Arc<Self>, MemoryError> {
        Self::map(length, None, 0)
    }

    fn file(file: &File, length: u64, offset: u64) -> Result<(Arc<Self>, u64), MemoryError> {
        // SAFETY: sysconf receives no pointer and retains no state.
        let page = u64::try_from(unsafe { abi::sysconf(abi::_SC_PAGESIZE) }).map_err(|_| MemoryError::Host)?;
        if page == 0 || !page.is_power_of_two() {
            return Err(MemoryError::Host);
        }
        let first = offset / page * page;
        let displacement = offset - first;
        let needed = displacement.checked_add(length).ok_or(MemoryError::InvalidRange)?;
        let mapped = needed.checked_add(page - 1).ok_or(MemoryError::InvalidRange)? / page * page;
        Ok((
            Self::map(mapped, Some(file.try_clone().map_err(|_| MemoryError::Host)?), first)?,
            displacement,
        ))
    }

    fn map(length: u64, file: Option<File>, offset: u64) -> Result<Arc<Self>, MemoryError> {
        let length = usize::try_from(length).map_err(|_| MemoryError::InvalidRange)?;
        let offset = i64::try_from(offset).map_err(|_| MemoryError::InvalidRange)?;
        if length == 0 {
            return Err(MemoryError::InvalidRange);
        }
        let (flags, descriptor) = file
            .as_ref()
            .map_or((abi::MAP_PRIVATE | abi::MAP_ANONYMOUS, -1), |file| {
                (abi::MAP_SHARED, file.as_raw_fd())
            });
        // SAFETY: the host chooses an unrelated interval, retains no pointer,
        // and Backing owns the complete result until its final Arc is dropped.
        let address = unsafe { abi::mmap(std::ptr::null_mut(), length, 1 | 2, flags, descriptor, offset) };
        if address == abi::MAP_FAILED {
            return Err(MemoryError::Host);
        }
        Ok(Arc::new(Self {
            address: address as usize,
            length,
            _file: file,
        }))
    }
}

impl Drop for Backing {
    fn drop(&mut self) {
        // SAFETY: this is the complete interval owned by Backing. The final Arc
        // proves that neither a guest view nor a BackingLease can still use it.
        let _ = unsafe { abi::munmap(self.address as *mut core::ffi::c_void, self.length) };
    }
}

#[derive(Clone, Debug)]
struct View {
    guest: u64,
    length: u64,
    offset: u64,
    backing: Arc<Backing>,
}

impl View {
    fn end(&self) -> Result<u64, MemoryError> {
        self.guest.checked_add(self.length).ok_or(MemoryError::InvalidRange)
    }
}

#[derive(Debug)]
pub struct BackingLease {
    _backing: Arc<Backing>,
    address: u64,
}

impl BackingLease {
    pub(super) const fn address(&self) -> u64 {
        self.address
    }

    #[cfg(test)]
    fn read(&self, output: &mut [u8]) {
        assert!(output.len() <= self._backing.length);
        // SAFETY: the lease retains the backing and the test slice is unique.
        unsafe { std::ptr::copy_nonoverlapping(self.address as *const u8, output.as_mut_ptr(), output.len()) };
    }

    #[cfg(test)]
    fn write(&self, input: &[u8]) {
        assert!(input.len() <= self._backing.length);
        // SAFETY: the lease retains writable canonical storage and the ranges
        // do not overlap.
        unsafe { std::ptr::copy_nonoverlapping(input.as_ptr(), self.address as *mut u8, input.len()) };
    }
}

#[derive(Debug, Default)]
pub(super) struct SparseMappings {
    views: Mutex<BTreeMap<u64, View>>,
}

#[derive(Clone, Debug)]
pub(super) struct Prepared {
    views: BTreeMap<u64, View>,
}

impl SparseMappings {
    pub(super) fn is_empty(&self) -> bool {
        self.views.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_empty()
    }

    pub(super) fn prepare_same(&self, prior: Option<&Prepared>) -> Prepared {
        Prepared {
            views: prior.map_or_else(
                || self.views.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone(),
                |prepared| prepared.views.clone(),
            ),
        }
    }
    pub(super) fn prepare_map(
        &self,
        prior: Option<&Prepared>,
        guest: u64,
        length: u64,
        file: Option<(&File, u64)>,
    ) -> Result<Prepared, MemoryError> {
        let mut views = prior.map_or_else(
            || self.views.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone(),
            |prepared| prepared.views.clone(),
        );
        Self::remove(&mut views, guest, length)?;
        if let Some((file, offset)) = file {
            let (backing, displacement) = Backing::file(file, length, offset)?;
            views.insert(
                guest,
                View {
                    guest,
                    length,
                    offset: displacement,
                    backing,
                },
            );
        }
        Ok(Prepared { views })
    }

    pub(super) fn prepare_unmap(
        &self,
        prior: Option<&Prepared>,
        guest: u64,
        length: u64,
    ) -> Result<Prepared, MemoryError> {
        let mut views = prior.map_or_else(
            || self.views.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone(),
            |prepared| prepared.views.clone(),
        );
        Self::remove(&mut views, guest, length)?;
        Ok(Prepared { views })
    }

    pub(super) fn prepare_remap(
        &self,
        prior: Option<&Prepared>,
        source: AddressRange,
        destination: u64,
        length: u64,
        keep_source: bool,
        file: Option<(&File, u64)>,
    ) -> Result<Prepared, MemoryError> {
        let mut views = prior.map_or_else(
            || self.views.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone(),
            |prepared| prepared.views.clone(),
        );
        let source_view = Self::find_in(&views, source.start().get(), source.length())?;
        Self::remove(&mut views, destination, length)?;
        if !keep_source {
            Self::remove(&mut views, source.start().get(), source.length())?;
        }
        let Some(source_view) = source_view else {
            return Ok(Prepared { views });
        };
        let source_offset = source_view
            .offset
            .checked_add(source.start().get() - source_view.guest)
            .ok_or(MemoryError::InvalidRange)?;
        let available = (source_view.backing.length as u64).saturating_sub(source_offset);
        let (backing, offset) = if length <= available {
            (source_view.backing, source_offset)
        } else if let Some((file, file_offset)) = file {
            Backing::file(file, length, file_offset)?
        } else {
            return Err(MemoryError::InvalidRange);
        };
        views.insert(
            destination,
            View {
                guest: destination,
                length,
                offset,
                backing,
            },
        );
        Ok(Prepared { views })
    }

    pub(super) fn publish(&self, prepared: Prepared) {
        *self.views.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = prepared.views;
    }

    #[cfg(test)]
    pub(super) fn map_anonymous(&self, guest: u64, length: u64) -> Result<(), MemoryError> {
        self.replace(guest, length, Backing::anonymous(length)?, 0)
    }

    #[cfg(test)]
    pub(super) fn alias(&self, source_guest: u64, guest: u64, length: u64) -> Result<(), MemoryError> {
        let source = self.find(source_guest, length)?.ok_or(MemoryError::InvalidRange)?;
        let offset = source
            .offset
            .checked_add(source_guest - source.guest)
            .ok_or(MemoryError::InvalidRange)?;
        self.replace(guest, length, source.backing, offset)
    }

    pub(super) fn pin(&self, range: AddressRange) -> Result<Option<BackingLease>, MemoryError> {
        let Some(view) = self.find(range.start().get(), range.length())? else {
            return Ok(None);
        };
        let displacement = range.start().get() - view.guest;
        let address = (view.backing.address as u64)
            .checked_add(view.offset)
            .and_then(|value| value.checked_add(displacement))
            .ok_or(MemoryError::InvalidRange)?;
        Ok(Some(BackingLease {
            _backing: view.backing,
            address,
        }))
    }

    fn find(&self, guest: u64, length: u64) -> Result<Option<View>, MemoryError> {
        let views = self.views.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::find_in(&views, guest, length)
    }

    fn find_in(views: &BTreeMap<u64, View>, guest: u64, length: u64) -> Result<Option<View>, MemoryError> {
        let end = guest.checked_add(length).ok_or(MemoryError::InvalidRange)?;
        let Some((_, view)) = views.range(..=guest).next_back() else {
            return Ok(None);
        };
        Ok((guest >= view.guest && end <= view.end()?).then(|| view.clone()))
    }

    #[cfg(test)]
    fn replace(&self, guest: u64, length: u64, backing: Arc<Backing>, offset: u64) -> Result<(), MemoryError> {
        let mut views = self.views.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::remove(&mut views, guest, length)?;
        views.insert(
            guest,
            View {
                guest,
                length,
                offset,
                backing,
            },
        );
        Ok(())
    }

    fn remove(views: &mut BTreeMap<u64, View>, guest: u64, length: u64) -> Result<(), MemoryError> {
        let end = guest.checked_add(length).ok_or(MemoryError::InvalidRange)?;
        if length == 0 {
            return Err(MemoryError::InvalidRange);
        }
        let affected = views
            .range(..end)
            .filter_map(|(start, view)| (view.end().ok()? > guest).then_some((*start, view.clone())))
            .collect::<Vec<_>>();
        for (start, view) in affected {
            views.remove(&start);
            if start < guest {
                views.insert(
                    start,
                    View {
                        length: guest - start,
                        ..view.clone()
                    },
                );
            }
            let old_end = view.end()?;
            if old_end > end {
                views.insert(
                    end,
                    View {
                        guest: end,
                        length: old_end - end,
                        offset: view.offset.checked_add(end - start).ok_or(MemoryError::InvalidRange)?,
                        backing: view.backing,
                    },
                );
            }
        }
        Ok(())
    }
}

impl hl_memory::HostProjection for BackingLease {
    fn storage_address(&self) -> u64 {
        self.address
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use hl_isa::{AddressRange, GuestAddress};

    use super::SparseMappings;

    fn range(start: u64, length: u64) -> AddressRange {
        AddressRange::nonempty(GuestAddress::new(start), length).unwrap()
    }

    #[test]
    fn old_pin_survives_guest_replacement() {
        let mappings = SparseMappings::default();
        mappings.map_anonymous(0x1000, 4096).unwrap();
        let old = mappings.pin(range(0x1000, 4)).unwrap().unwrap();
        old.write(b"old!");
        mappings.map_anonymous(0x1000, 4096).unwrap();
        let current = mappings.pin(range(0x1000, 4)).unwrap().unwrap();
        current.write(b"new!");
        let mut bytes = [0; 4];
        old.read(&mut bytes);
        assert_eq!(&bytes, b"old!");
        current.read(&mut bytes);
        assert_eq!(&bytes, b"new!");
    }

    #[test]
    fn aliases_share_storage_until_last_lease_drops() {
        let mappings = SparseMappings::default();
        mappings.map_anonymous(0x1000, 4096).unwrap();
        mappings.alias(0x1008, 0x8000, 8).unwrap();
        let first = mappings.pin(range(0x1008, 4)).unwrap().unwrap();
        let alias = mappings.pin(range(0x8000, 4)).unwrap().unwrap();
        first.write(b"same");
        let mut bytes = [0; 4];
        alias.read(&mut bytes);
        assert_eq!(&bytes, b"same");
        mappings.map_anonymous(0x1000, 4096).unwrap();
        mappings.map_anonymous(0x8000, 8).unwrap();
        alias.read(&mut bytes);
        assert_eq!(&bytes, b"same");
    }

    #[test]
    fn remap_candidate_is_atomic_and_retains_old_pin() {
        let mappings = SparseMappings::default();
        mappings.map_anonymous(0x1000, 4096).unwrap();
        let old = mappings.pin(range(0x1008, 4)).unwrap().unwrap();
        old.write(b"move");
        let prepared = mappings
            .prepare_remap(None, range(0x1000, 4096), 0x8000, 2048, true, None)
            .unwrap();
        assert!(mappings.pin(range(0x8008, 4)).unwrap().is_none());
        mappings.publish(prepared);
        let alias = mappings.pin(range(0x8008, 4)).unwrap().unwrap();
        let mut bytes = [0; 4];
        alias.read(&mut bytes);
        assert_eq!(&bytes, b"move");

        let rolled_back = mappings
            .prepare_remap(None, range(0x8000, 2048), 0xc000, 1024, false, None)
            .unwrap();
        drop(rolled_back);
        assert!(mappings.pin(range(0xc000, 4)).unwrap().is_none());
        old.read(&mut bytes);
        assert_eq!(&bytes, b"move");
    }

    #[test]
    fn file_remap_grows_with_retained_capability() {
        let path = std::path::PathBuf::from(format!("/tmp/hl-sparse-grow-{}", std::process::id()));
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        file.set_len(8192).unwrap();
        let mappings = SparseMappings::default();
        let initial = mappings.prepare_map(None, 0x1000, 4096, Some((&file, 0))).unwrap();
        mappings.publish(initial);
        let old = mappings.pin(range(0x1000, 4)).unwrap().unwrap();
        old.write(b"grow");
        let grown = mappings
            .prepare_remap(None, range(0x1000, 4096), 0x9000, 8192, false, Some((&file, 0)))
            .unwrap();
        mappings.publish(grown);
        assert!(mappings.pin(range(0x1000, 4)).unwrap().is_none());
        let moved = mappings.pin(range(0x9000, 4)).unwrap().unwrap();
        let mut bytes = [0; 4];
        moved.read(&mut bytes);
        assert_eq!(&bytes, b"grow");
        old.read(&mut bytes);
        assert_eq!(&bytes, b"grow");
        fs::remove_file(path).unwrap();
    }
}
