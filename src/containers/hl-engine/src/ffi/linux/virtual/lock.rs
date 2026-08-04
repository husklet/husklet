use hl_isa::AddressRange;

use super::abi;
use super::arena::Operation;
use super::virtual_memory::MemoryError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Span {
    start: u64,
    end: u64,
}

#[derive(Clone, Debug, Default)]
pub(super) struct Locks {
    spans: Vec<Span>,
    pub(super) future: bool,
    pub(super) on_fault: bool,
    future_limit: u64,
}

impl Locks {
    const LIMIT: usize = 1024;

    pub(super) fn bytes(&self) -> u64 {
        self.spans.iter().map(|span| span.end - span.start).sum()
    }

    fn added(&self, previous: &Self) -> Vec<AddressRange> {
        self.spans
            .iter()
            .flat_map(|span| Self::subtract(*span, &previous.spans))
            .collect()
    }

    fn subtract(span: Span, previous: &[Span]) -> Vec<AddressRange> {
        let mut ranges = Vec::new();
        let mut cursor = span.start;
        for prior in previous {
            if prior.end <= cursor {
                continue;
            }
            if prior.start >= span.end {
                break;
            }
            if prior.start > cursor {
                ranges.push(Self::range(cursor, prior.start.min(span.end)));
            }
            cursor = cursor.max(prior.end);
            if cursor >= span.end {
                break;
            }
        }
        if cursor < span.end {
            ranges.push(Self::range(cursor, span.end));
        }
        ranges
    }

    fn range(start: u64, end: u64) -> AddressRange {
        AddressRange::nonempty(hl_isa::GuestAddress::new(start), end - start).expect("normalized lock span")
    }

    pub(super) fn add(&mut self, range: AddressRange, limit: u64) -> Result<(), MemoryError> {
        let mut spans = self.spans.clone();
        spans.push(Span {
            start: range.start().get(),
            end: range.end().get(),
        });
        spans.sort_by_key(|span| span.start);
        let mut normalized: Vec<Span> = Vec::with_capacity(spans.len());
        for span in spans {
            if let Some(previous) = normalized.last_mut()
                && span.start <= previous.end
            {
                previous.end = previous.end.max(span.end);
            } else {
                normalized.push(span);
            }
        }
        if normalized.len() > Self::LIMIT {
            return Err(MemoryError::OutOfMemory);
        }
        let bytes = normalized
            .iter()
            .map(|span| span.end - span.start)
            .try_fold(0_u64, u64::checked_add)
            .ok_or(MemoryError::OutOfMemory)?;
        if bytes > limit {
            return Err(MemoryError::OutOfMemory);
        }
        self.spans = normalized;
        Ok(())
    }

    pub(super) fn remove(&mut self, range: AddressRange) {
        self.spans = self
            .spans
            .iter()
            .flat_map(|span| {
                let mut pieces = Vec::with_capacity(2);
                if span.start < range.start().get() {
                    pieces.push(Span {
                        start: span.start,
                        end: span.end.min(range.start().get()),
                    });
                }
                if span.end > range.end().get() {
                    pieces.push(Span {
                        start: span.start.max(range.end().get()),
                        end: span.end,
                    });
                }
                pieces
            })
            .collect();
    }

    pub(super) fn clear(&mut self) {
        self.spans.clear();
        self.future = false;
        self.on_fault = false;
        self.future_limit = 0;
    }

    pub(super) fn apply(&self, operations: &[Operation]) -> Result<Self, MemoryError> {
        let mut candidate = self.clone();
        for operation in operations {
            candidate.apply_operation(*operation)?;
        }
        Ok(candidate)
    }

    fn apply_operation(&mut self, operation: Operation) -> Result<(), MemoryError> {
        match operation {
            Operation::Backing(_) => Ok(()),
            Operation::Map(start, request) if self.future => {
                let range = AddressRange::nonempty(hl_isa::GuestAddress::new(start), request.length)
                    .map_err(|_| MemoryError::InvalidRange)?;
                self.add(range, self.future_limit)
            }
            Operation::Map(_, _) | Operation::Protect(_, _, _) => Ok(()),
            Operation::Unmap(start, length) => {
                let range = AddressRange::nonempty(hl_isa::GuestAddress::new(start), length)
                    .map_err(|_| MemoryError::InvalidRange)?;
                self.remove(range);
                Ok(())
            }
            Operation::Remap(source, destination, request, keep) => {
                self.remap(source, destination, request.length, keep)
            }
        }
    }

    fn remap(&mut self, source: AddressRange, destination: u64, length: u64, keep: bool) -> Result<(), MemoryError> {
        let retained = source.length().min(length);
        let pieces = self
            .spans
            .iter()
            .filter_map(|span| {
                let start = span.start.max(source.start().get());
                let end = span.end.min(source.start().get() + retained);
                (start < end).then_some((start - source.start().get(), end - start))
            })
            .collect::<Vec<_>>();
        let limit = self.future_limit.max(self.bytes());
        self.remove(
            AddressRange::nonempty(hl_isa::GuestAddress::new(destination), length)
                .map_err(|_| MemoryError::InvalidRange)?,
        );
        if !keep {
            self.remove(source);
        }
        for (offset, piece_length) in pieces {
            let start = destination.checked_add(offset).ok_or(MemoryError::InvalidRange)?;
            let range = AddressRange::nonempty(hl_isa::GuestAddress::new(start), piece_length)
                .map_err(|_| MemoryError::InvalidRange)?;
            self.add(range, limit)?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn contains(&self, range: AddressRange) -> bool {
        self.spans
            .iter()
            .any(|span| span.start <= range.start().get() && span.end >= range.end().get())
    }
}

impl super::virtual_memory::Memory {
    pub(super) fn wire(&self, range: AddressRange) -> Result<(), MemoryError> {
        let (address, length) = self.host_range(range.start().get(), range.length())?;
        abi::PageLock::lock(address, length)
            .then_some(())
            .ok_or(MemoryError::Host)
    }

    pub(super) fn unwire(&self, range: AddressRange) -> Result<(), MemoryError> {
        let (address, length) = self.host_range(range.start().get(), range.length())?;
        abi::PageLock::unlock(address, length)
            .then_some(())
            .ok_or(MemoryError::Host)
    }

    fn restore_wires(&self, wired: Vec<AddressRange>, unwired: Vec<AddressRange>) -> Result<(), MemoryError> {
        for range in wired.into_iter().rev() {
            self.unwire(range)?;
        }
        for range in unwired.into_iter().rev() {
            self.wire(range)?;
        }
        Ok(())
    }

    pub(super) fn transition_locks(&self, previous: &Locks, candidate: &Locks) -> Result<(), MemoryError> {
        let removed = previous.added(candidate);
        let added = candidate.added(previous);
        let mut unwired = Vec::new();
        for range in removed {
            if self.unwire(range).is_err() {
                self.restore_wires(Vec::new(), unwired)?;
                return Err(MemoryError::Host);
            }
            unwired.push(range);
        }
        let mut wired = Vec::new();
        for range in added {
            if self.wire(range).is_err() {
                return self.restore_wires(wired, unwired).and(Err(MemoryError::OutOfMemory));
            }
            wired.push(range);
        }
        Ok(())
    }

    pub(super) fn lock_candidate(&self, range: AddressRange, limit: u64) -> Result<Locks, MemoryError> {
        let locks = self.locks.lock().unwrap_or_else(|error| error.into_inner());
        let mut candidate = locks.clone();
        candidate.add(range, limit)?;
        Ok(candidate)
    }

    pub(super) fn unlock_candidate(&self, range: AddressRange) -> Locks {
        let locks = self.locks.lock().unwrap_or_else(|error| error.into_inner());
        let mut candidate = locks.clone();
        candidate.remove(range);
        candidate
    }

    pub(super) fn publish_locks(&self, candidate: Locks) {
        *self.locks.lock().unwrap_or_else(|error| error.into_inner()) = candidate;
    }

    pub(super) fn lock_all_candidate(
        &self,
        ranges: &[AddressRange],
        limit: u64,
        future: bool,
        on_fault: bool,
    ) -> Result<Locks, MemoryError> {
        let locks = self.locks.lock().unwrap_or_else(|error| error.into_inner());
        let mut candidate = locks.clone();
        for range in ranges {
            candidate.add(*range, limit)?;
        }
        candidate.future = future;
        candidate.on_fault = on_fault;
        candidate.future_limit = if future { limit } else { 0 };
        Ok(candidate)
    }

    pub(super) fn install_all_locks(&self, candidate: Locks) {
        let mut locks = self.locks.lock().unwrap_or_else(|error| error.into_inner());
        for range in locks.added(&candidate) {
            let _ = self.unwire(range);
        }
        for range in candidate.added(&locks) {
            let _ = self.wire(range);
        }
        *locks = candidate;
    }

    pub(super) fn clear_all_locks(&self) {
        let mut locks = self.locks.lock().unwrap_or_else(|error| error.into_inner());
        let empty = Locks::default();
        for range in locks.added(&empty) {
            let _ = self.unwire(range);
        }
        locks.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::super::virtual_memory::Memory;
    use super::*;
    use hl_isa::GuestAddress;
    use hl_memory::{Backing, MapRequest, Placement, Protection};

    fn range(start: u64, length: u64) -> AddressRange {
        AddressRange::nonempty(GuestAddress::new(start), length).unwrap()
    }

    #[test]
    fn overlap_accounting() {
        let mut locks = Locks::default();
        locks.add(range(4096, 8192), 12_288).unwrap();
        locks.add(range(8192, 8192), 12_288).unwrap();
        assert_eq!(locks.bytes(), 12_288);
        locks.remove(range(8192, 4096));
        assert_eq!(locks.bytes(), 8192);
        assert!(locks.contains(range(4096, 4096)));
        assert!(locks.contains(range(12_288, 4096)));
    }

    fn request(identity: u64, start: u64) -> MapRequest {
        MapRequest {
            placement: Placement::Fixed(GuestAddress::new(start)),
            length: 4096,
            alignment: 4096,
            protection: Protection::READ.union(Protection::WRITE),
            backing: Backing::Anonymous {
                identity,
                shared: false,
            },
            backing_offset: 0,
        }
    }

    #[test]
    fn future_mapping() {
        let memory = Memory::reserve(8192).unwrap();
        let candidate = memory.lock_all_candidate(&[], 4096, true, false).unwrap();
        memory.install_all_locks(candidate);
        let first = memory.stage(Operation::Map(0, request(1, 0))).unwrap();
        memory.commit(&[first]).unwrap();
        assert!(memory.locks.lock().unwrap().contains(range(0, 4096)));
        let second = memory.stage(Operation::Map(4096, request(2, 4096))).unwrap();
        assert_eq!(memory.commit(&[second]), Err(MemoryError::OutOfMemory));
        assert!(!memory.locks.lock().unwrap().contains(range(4096, 4096)));
    }

    #[test]
    fn current_unlock() {
        let memory = Memory::reserve(4096).unwrap();
        let token = memory.stage(Operation::Map(0, request(1, 0))).unwrap();
        memory.commit(&[token]).unwrap();
        let candidate = memory
            .lock_all_candidate(&[range(0, 4096)], 4096, false, false)
            .unwrap();
        memory.install_all_locks(candidate);
        assert_eq!(memory.locks.lock().unwrap().bytes(), 4096);
        memory.clear_all_locks();
        assert_eq!(memory.locks.lock().unwrap().bytes(), 0);
    }

    #[test]
    fn whole_space_lock_tracks_inaccessible_mapping() {
        let memory = Memory::reserve(4096).unwrap();
        let mut map = request(1, 0);
        map.protection = Protection::NONE;
        let token = memory.stage(Operation::Map(0, map)).unwrap();
        memory.commit(&[token]).unwrap();
        let candidate = memory
            .lock_all_candidate(&[range(0, 4096)], 4096, false, false)
            .unwrap();
        memory.install_all_locks(candidate);
        assert_eq!(memory.locks.lock().unwrap().bytes(), 4096);
        memory.clear_all_locks();
        assert_eq!(memory.locks.lock().unwrap().bytes(), 0);
    }
}
