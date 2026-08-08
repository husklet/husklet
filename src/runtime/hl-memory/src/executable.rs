use std::collections::BTreeMap;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use hl_isa::AddressRange;

const EXECUTABLE_PAGE_BYTES: u64 = 4_096;

/// Stable evidence for the executable bytes in one guest source range.
///
/// `incarnation` is supplied by the address-space owner and rotates when the
/// complete image is replaced. `version` is the later of the era and the page
/// versions covered by the range: a committed write versions the pages it
/// touches, while a published mapping transition advances the era and so
/// supersedes every range at once.
///
/// The era exists because reuse leaves both other components fixed. Unmapping a
/// range and mapping different bytes over the same addresses, replacing a
/// mapping in place, or turning a written data range into an executable one all
/// keep the incarnation and leave the page versions untouched, so a token built
/// only from those two would certify bytes that no longer exist.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ExecutableToken {
    pub incarnation: u64,
    pub version: u64,
}

/// Address-space-owned versions for guest pages which have contained code.
///
/// `sequence` numbers every publication, `era` is the sequence of the most
/// recent whole-space publication, and `pages` holds per-page sequences.
///
/// `pages` stays small in practice: instrumenting `tests/bench/fs-churn/spawn`
/// (500 fork/exec) measured a high-water mark of 256 entries, so neither the
/// `range` scan in `token` nor the sweep in `publish_all` is a fork/exec cost.
#[derive(Debug, Default)]
pub(crate) struct ExecutableVersions {
    sequence: AtomicU64,
    era: AtomicU64,
    pages: RwLock<BTreeMap<u64, u64>>,
}

impl ExecutableVersions {
    pub(crate) fn token(&self, range: AddressRange, incarnation: u64) -> ExecutableToken {
        let era = self.era.load(Ordering::Acquire);
        let first = Self::page(range.start().get());
        let last = Self::page(range.end().get().saturating_sub(1));
        let pages = self.pages.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        let version = pages
            .range(first..=last)
            .map(|(_, version)| *version)
            .max()
            .unwrap_or(0)
            .max(era);
        ExecutableToken { incarnation, version }
    }

    /// Supersedes every range in the space. Mapping transitions publish here
    /// because they change which bytes an address denotes without writing any.
    pub(crate) fn rotate(&self) {
        let version = self.sequence.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
        self.era.fetch_max(version, Ordering::AcqRel);
    }

    pub(crate) fn publish(&self, ranges: impl IntoIterator<Item = AddressRange>) {
        let ranges: Vec<_> = ranges.into_iter().collect();
        if ranges.is_empty() {
            return;
        }
        let mut pages = self.pages.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        let version = self.sequence.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
        for range in ranges {
            let first = Self::page(range.start().get());
            let last = Self::page(range.end().get().saturating_sub(1));
            for page in first..=last {
                pages.insert(page, version);
            }
        }
    }

    pub(crate) fn publish_all(&self) {
        let mut pages = self.pages.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        let version = self.sequence.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
        for observed in pages.values_mut() {
            *observed = version;
        }
        // Pages which never took a committed write carry no entry, so the era
        // is what supersedes them.
        self.era.fetch_max(version, Ordering::AcqRel);
    }

    pub(crate) fn generation(&self) -> u64 {
        self.sequence.load(Ordering::Acquire)
    }

    const fn page(address: u64) -> u64 {
        address / EXECUTABLE_PAGE_BYTES
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;

    use hl_isa::{AddressRange, GuestAddress};

    use super::ExecutableVersions;

    fn range(address: u64, length: u64) -> AddressRange {
        AddressRange::nonempty(GuestAddress::new(address), length).unwrap()
    }

    #[test]
    fn overlapping_publish_changes_only_covered_pages() {
        let versions = ExecutableVersions::default();
        let first = range(0x1_000, 16);
        let second = range(0x3_000, 16);
        let first_before = versions.token(first, 7);
        let second_before = versions.token(second, 7);

        versions.publish([first]);

        assert_ne!(versions.token(first, 7), first_before);
        assert_eq!(versions.token(second, 7), second_before);
        assert_ne!(versions.token(first, 8), versions.token(first, 7));
    }

    #[test]
    fn concurrent_readers_observe_complete_publications() {
        let versions = Arc::new(ExecutableVersions::default());
        let source = range(0x5_000, 16);
        let reader = Arc::clone(&versions);
        let handle = thread::spawn(move || {
            let mut previous = 0;
            for _ in 0..10_000 {
                let current = reader.token(source, 1).version;
                assert!(current >= previous);
                previous = current;
            }
        });
        for _ in 0..1_000 {
            versions.publish([source]);
        }
        handle.join().unwrap();
        assert_eq!(versions.token(source, 1).version, 1_000);
    }
}
