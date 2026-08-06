use std::sync::Arc;

use hl_memory::MappingCoordinator;
use hl_runtime::{RuntimeMemoryError, RuntimeMemoryHost};

use super::{MappingHostAdapter, VirtualMemory, abi, virtual_advice::ForkAdvice};

#[derive(Debug)]
pub(super) struct Control {
    arena: Arc<VirtualMemory>,
    mappings: Arc<MappingCoordinator<MappingHostAdapter>>,
    limit: Arc<dyn LimitSource>,
}

pub(super) trait LimitSource: std::fmt::Debug + Send + Sync {
    fn soft(&self) -> u64;
}

impl Control {
    pub(super) fn new(
        arena: Arc<VirtualMemory>,
        mappings: Arc<MappingCoordinator<MappingHostAdapter>>,
        limit: Arc<dyn LimitSource>,
    ) -> Self {
        Self { arena, mappings, limit }
    }

    fn mapped(&self, range: hl_isa::AddressRange) -> Result<(), RuntimeMemoryError> {
        self.mappings
            .contains(range)
            .then_some(())
            .ok_or(RuntimeMemoryError::NoMemory)
    }

    fn private_anonymous(&self, range: hl_isa::AddressRange) -> bool {
        let snapshot = self.mappings.snapshot();
        let mut covered = 0_u64;
        for region in snapshot.regions {
            let start = region.range().start().get().max(range.start().get());
            let end = region.range().end().get().min(range.end().get());
            if start >= end {
                continue;
            }
            if !matches!(region.backing(), hl_memory::Backing::Anonymous { shared: false, .. }) {
                return false;
            }
            covered += end - start;
        }
        covered == range.length()
    }

    fn failure() -> RuntimeMemoryError {
        match std::io::Error::last_os_error().raw_os_error() {
            Some(abi::EINVAL) => RuntimeMemoryError::Invalid,
            Some(abi::ENOMEM) => RuntimeMemoryError::NoMemory,
            Some(abi::EPERM | abi::EACCES) => RuntimeMemoryError::Permission,
            _ => RuntimeMemoryError::Failed,
        }
    }
}

impl RuntimeMemoryHost for Control {
    fn advise(&self, plan: hl_linux::AdvicePlan) -> Result<(), RuntimeMemoryError> {
        let hl_linux::AdvicePlan::Apply { range, advice } = plan else {
            return Ok(());
        };
        self.mapped(range)?;
        let fork = match advice {
            hl_linux::Advice::DontFork if self.private_anonymous(range) => Some(Some(ForkAdvice::Omit)),
            hl_linux::Advice::DoFork if self.private_anonymous(range) => Some(None),
            hl_linux::Advice::DontFork | hl_linux::Advice::DoFork => return Ok(()),
            hl_linux::Advice::WipeOnFork | hl_linux::Advice::KeepOnFork if !self.private_anonymous(range) => {
                return Err(RuntimeMemoryError::Invalid);
            }
            hl_linux::Advice::WipeOnFork => Some(Some(ForkAdvice::Wipe)),
            hl_linux::Advice::KeepOnFork => Some(None),
            _ => None,
        };
        if let Some(advice) = fork {
            return self
                .arena
                .update_advice(range, advice)
                .map_err(|_| RuntimeMemoryError::NoMemory);
        }
        let value = match advice {
            hl_linux::Advice::Normal => 0,
            hl_linux::Advice::Random => 1,
            hl_linux::Advice::Sequential => 2,
            hl_linux::Advice::WillNeed => 3,
            hl_linux::Advice::DontNeed => 4,
            hl_linux::Advice::Free => 8,
            hl_linux::Advice::Remove => 9,
            hl_linux::Advice::Noop => return Ok(()),
            hl_linux::Advice::DontFork
            | hl_linux::Advice::DoFork
            | hl_linux::Advice::WipeOnFork
            | hl_linux::Advice::KeepOnFork => unreachable!("fork advice handled above"),
        };
        let (address, length) = self
            .arena
            .host_range(range.start().get(), range.length())
            .map_err(|_| RuntimeMemoryError::Invalid)?;
        // SAFETY: the checked address range is retained by `arena`; madvise is
        // advisory, retains no pointer, and cannot unwind.
        if advice == hl_linux::Advice::DontNeed && self.private_anonymous(range) {
            // Darwin's DONTNEED can preserve anonymous contents, so clear the
            // exact guest range before asking the host to discard its pages.
            // Clearing after madvise would fault every discarded page back in
            // and make mincore contradict Linux immediately after the call.
            let _ = self.arena.clear(range);
        }
        // SAFETY: `host_range` checked the range against the arena, which keeps the
        // mapping alive; madvise is advisory, retains no pointer, and cannot unwind.
        let result = unsafe { madvise(address, length, value) };
        if result != 0 && advice == hl_linux::Advice::Remove {
            return Err(Self::failure());
        }
        Ok(())
    }

    fn residency(&self, plan: hl_linux::MemoryRangePlan) -> Result<Vec<bool>, RuntimeMemoryError> {
        self.mapped(plan.range)?;
        let (address, length) = self
            .arena
            .host_range(plan.range.start().get(), plan.range.length())
            .map_err(|_| RuntimeMemoryError::Invalid)?;
        let pages = usize::try_from(plan.range.length() / 4096).map_err(|_| RuntimeMemoryError::NoMemory)?;
        let mut vector = vec![0_u8; pages];
        // SAFETY: the checked address range is retained by `arena`, the output
        // vector has one byte per Linux host page, no alias escapes, and the C
        // call retains no pointers and cannot unwind.
        if unsafe { mincore(address, length, vector.as_mut_ptr()) } != 0 {
            return Err(Self::failure());
        }
        Ok(vector.into_iter().map(|value| value & 1 != 0).collect())
    }

    fn lock(&self, plan: Option<hl_linux::MemoryRangePlan>, on_fault: bool) -> Result<(), RuntimeMemoryError> {
        let Some(plan) = plan else { return Ok(()) };
        let limit = self.limit.soft();
        if limit == 0 {
            return Err(RuntimeMemoryError::Permission);
        }
        self.mapped(plan.range)?;
        let candidate = self
            .arena
            .lock_candidate(plan.range, limit)
            .map_err(|_| RuntimeMemoryError::NoMemory)?;
        let (address, length) = self
            .arena
            .host_range(plan.range.start().get(), plan.range.length())
            .map_err(|_| RuntimeMemoryError::Invalid)?;
        let result = if on_fault {
            // SAFETY: the complete range is mapped and retained by the arena;
            // the guest limit bounds pinning, mapping transitions serialize,
            // and the kernel retains no pointer and cannot unwind.
            unsafe { libc::syscall(libc::SYS_mlock2, address, length, libc::MLOCK_ONFAULT) as i32 }
        } else {
            // SAFETY: the complete range is mapped and retained by the arena;
            // the guest limit bounds pinning, mapping transitions serialize,
            // and libc retains no pointer and cannot unwind.
            unsafe { abi::mlock(address, length) }
        };
        if result != 0 {
            return Err(Self::failure());
        }
        self.arena.publish_locks(candidate);
        Ok(())
    }

    fn unlock(&self, plan: Option<hl_linux::MemoryRangePlan>) -> Result<(), RuntimeMemoryError> {
        let Some(plan) = plan else { return Ok(()) };
        self.mapped(plan.range)?;
        let candidate = self.arena.unlock_candidate(plan.range);
        let (address, length) = self
            .arena
            .host_range(plan.range.start().get(), plan.range.length())
            .map_err(|_| RuntimeMemoryError::Invalid)?;
        // SAFETY: the complete range is mapped and retained by the arena; libc
        // retains no pointer and cannot unwind.
        if unsafe { abi::munlock(address, length) } != 0 {
            return Err(Self::failure());
        }
        self.arena.publish_locks(candidate);
        Ok(())
    }

    fn lock_all(&self, plan: hl_linux::LockAllPlan) -> Result<(), RuntimeMemoryError> {
        let limit = self.limit.soft();
        if limit == 0 {
            return Err(RuntimeMemoryError::Permission);
        }
        let ranges = if plan.current {
            self.mappings
                .snapshot()
                .regions
                .into_iter()
                .map(hl_memory::Region::range)
                .collect()
        } else {
            Vec::new()
        };
        let candidate = self
            .arena
            .lock_all_candidate(&ranges, limit, plan.future, plan.on_fault)
            .map_err(|_| RuntimeMemoryError::NoMemory)?;
        self.arena.install_all_locks(candidate);
        Ok(())
    }

    fn unlock_all(&self) -> Result<(), RuntimeMemoryError> {
        self.arena.clear_all_locks();
        Ok(())
    }

    fn sync(&self, plan: hl_linux::MsyncPlan) -> Result<(), RuntimeMemoryError> {
        let Some(range) = plan.range else { return Ok(()) };
        self.mapped(range)?;
        let (address, length) = self
            .arena
            .host_range(range.start().get(), range.length())
            .map_err(|_| RuntimeMemoryError::Invalid)?;
        let flags = if plan.asynchronous { 1 } else { 4 } | if plan.invalidate { 2 } else { 0 };
        // SAFETY: the checked address range is retained by `arena`, concurrent
        // mappings remain kernel-owned, and msync retains no pointer and cannot
        // unwind.
        if unsafe { msync(address, length, flags) } != 0 {
            return Err(Self::failure());
        }
        Ok(())
    }
}

unsafe extern "C" {
    fn madvise(address: *mut core::ffi::c_void, length: usize, advice: i32) -> i32;
    fn msync(address: *mut core::ffi::c_void, length: usize, flags: i32) -> i32;
    fn mincore(address: *mut core::ffi::c_void, length: usize, vector: *mut u8) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;
    use hl_isa::{AddressRange, GuestAddress};
    use hl_memory::{Backing, MapRequest, Placement, Protection};

    #[derive(Debug)]
    struct Limit;

    impl LimitSource for Limit {
        fn soft(&self) -> u64 {
            u64::MAX
        }
    }

    fn range(start: u64) -> AddressRange {
        AddressRange::nonempty(GuestAddress::new(start), 4096).unwrap()
    }

    fn fixture(shared: bool) -> (Control, Arc<VirtualMemory>) {
        let arena = Arc::new(VirtualMemory::reserve(16_384).unwrap());
        let mappings = Arc::new(MappingCoordinator::new(MappingHostAdapter::new(Arc::clone(&arena))));
        mappings
            .map(MapRequest {
                placement: Placement::Fixed(GuestAddress::new(4096)),
                length: 4096,
                alignment: 4096,
                protection: Protection::READ.union(Protection::WRITE),
                backing: Backing::Anonymous { identity: 1, shared },
                backing_offset: 0,
            })
            .unwrap();
        (Control::new(arena.clone(), mappings, Arc::new(Limit)), arena)
    }

    #[test]
    fn fork_advice_requires_private_anonymous() {
        let (private, arena) = fixture(false);
        RuntimeMemoryHost::advise(
            &private,
            hl_linux::AdvicePlan::Apply {
                range: range(4096),
                advice: hl_linux::Advice::WipeOnFork,
            },
        )
        .unwrap();
        assert_eq!(arena.advice_segments(range(4096)).unwrap()[0].1, Some(ForkAdvice::Wipe));
        RuntimeMemoryHost::advise(
            &private,
            hl_linux::AdvicePlan::Apply {
                range: range(4096),
                advice: hl_linux::Advice::KeepOnFork,
            },
        )
        .unwrap();
        assert_eq!(arena.advice_segments(range(4096)).unwrap()[0].1, None);

        let (shared, shared_arena) = fixture(true);
        RuntimeMemoryHost::advise(
            &shared,
            hl_linux::AdvicePlan::Apply {
                range: range(4096),
                advice: hl_linux::Advice::DontFork,
            },
        )
        .unwrap();
        assert_eq!(shared_arena.advice_segments(range(4096)).unwrap()[0].1, None);
        assert_eq!(
            RuntimeMemoryHost::advise(
                &shared,
                hl_linux::AdvicePlan::Apply {
                    range: range(4096),
                    advice: hl_linux::Advice::WipeOnFork,
                },
            ),
            Err(RuntimeMemoryError::Invalid),
        );
        assert_eq!(
            RuntimeMemoryHost::advise(
                &shared,
                hl_linux::AdvicePlan::Apply {
                    range: range(4096),
                    advice: hl_linux::Advice::KeepOnFork,
                },
            ),
            Err(RuntimeMemoryError::Invalid),
        );
    }

    #[test]
    fn dontneed_discards_private_pages() {
        let (control, arena) = fixture(false);
        arena.write(4096, b"stale").unwrap();
        RuntimeMemoryHost::advise(
            &control,
            hl_linux::AdvicePlan::Apply {
                range: range(4096),
                advice: hl_linux::Advice::DontNeed,
            },
        )
        .unwrap();
        let mut bytes = [1_u8; 5];
        arena.read(4096, &mut bytes).unwrap();
        assert_eq!(bytes, [0; 5]);
    }
}
