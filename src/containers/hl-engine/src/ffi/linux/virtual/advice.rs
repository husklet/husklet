use hl_isa::{AddressRange, GuestAddress};

use super::arena::Operation;
use super::virtual_memory::Memory;
use super::virtual_memory::MemoryError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ForkAdvice {
    Omit,
    Wipe,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Mark {
    start: u64,
    end: u64,
    advice: ForkAdvice,
}

#[derive(Clone, Debug, Default)]
pub(super) struct Advice {
    marks: Vec<Mark>,
}

impl Advice {
    const LIMIT: usize = 1024;

    pub(super) fn update(&mut self, range: AddressRange, advice: Option<ForkAdvice>) -> Result<(), MemoryError> {
        let mut candidate = self
            .marks
            .iter()
            .flat_map(|mark| {
                let mut pieces = Vec::with_capacity(2);
                if mark.start < range.start().get() {
                    pieces.push(Mark {
                        end: range.start().get().min(mark.end),
                        ..*mark
                    });
                }
                if mark.end > range.end().get() {
                    pieces.push(Mark {
                        start: range.end().get().max(mark.start),
                        ..*mark
                    });
                }
                pieces
            })
            .collect::<Vec<_>>();
        if let Some(advice) = advice {
            candidate.push(Mark {
                start: range.start().get(),
                end: range.end().get(),
                advice,
            });
        }
        candidate.sort_by_key(|mark| mark.start);
        let mut normalized: Vec<Mark> = Vec::with_capacity(candidate.len());
        for mark in candidate {
            Self::merge(&mut normalized, mark);
        }
        if normalized.len() > Self::LIMIT {
            return Err(MemoryError::OutOfMemory);
        }
        self.marks = normalized;
        Ok(())
    }

    fn merge(normalized: &mut Vec<Mark>, mark: Mark) {
        if let Some(previous) = normalized.last_mut()
            && previous.end == mark.start
            && previous.advice == mark.advice
        {
            previous.end = mark.end;
            return;
        }
        normalized.push(mark);
    }

    pub(super) fn segments(&self, range: AddressRange) -> Result<Vec<(AddressRange, Option<ForkAdvice>)>, MemoryError> {
        let mut boundaries = vec![range.start().get(), range.end().get()];
        for mark in &self.marks {
            if mark.end > range.start().get() && mark.start < range.end().get() {
                boundaries.push(mark.start.max(range.start().get()));
                boundaries.push(mark.end.min(range.end().get()));
            }
        }
        boundaries.sort_unstable();
        boundaries.dedup();
        boundaries
            .windows(2)
            .map(|pair| {
                let start = pair[0];
                let advice = self
                    .marks
                    .iter()
                    .find(|mark| mark.start <= start && start < mark.end)
                    .map(|mark| mark.advice);
                AddressRange::nonempty(GuestAddress::new(start), pair[1] - start)
                    .map(|value| (value, advice))
                    .map_err(|_| MemoryError::InvalidRange)
            })
            .collect()
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
            Operation::Map(offset, request) => self.clear(offset, request.length),
            Operation::Unmap(offset, length) => self.clear(offset, length),
            Operation::Protect(_, _, _) => Ok(()),
            Operation::Remap(source, destination, request, keep) => {
                self.remap(source, destination, request.length, keep)
            }
        }
    }

    fn remap(&mut self, source: AddressRange, destination: u64, length: u64, keep: bool) -> Result<(), MemoryError> {
        let retained = source.length().min(length);
        let copied =
            self.segments(AddressRange::nonempty(source.start(), retained).map_err(|_| MemoryError::InvalidRange)?)?;
        let growth_advice = (length > source.length())
            .then(|| {
                self.marks
                    .iter()
                    .find(|mark| mark.start < source.end().get() && mark.end >= source.end().get())
                    .map(|mark| mark.advice)
            })
            .flatten();
        self.clear(destination, length)?;
        if !keep {
            self.update(source, None)?;
        }
        self.copy_marks(source.start(), destination, copied)?;
        if let Some(advice) = growth_advice {
            let start = destination
                .checked_add(source.length())
                .ok_or(MemoryError::InvalidRange)?;
            let extension = AddressRange::nonempty(GuestAddress::new(start), length - source.length())
                .map_err(|_| MemoryError::InvalidRange)?;
            self.update(extension, Some(advice))?;
        }
        Ok(())
    }

    fn copy_marks(
        &mut self,
        source: GuestAddress,
        destination: u64,
        segments: Vec<(AddressRange, Option<ForkAdvice>)>,
    ) -> Result<(), MemoryError> {
        for (segment, advice) in segments {
            let Some(advice) = advice else { continue };
            let offset = segment.start().get() - source.get();
            let start = destination.checked_add(offset).ok_or(MemoryError::InvalidRange)?;
            let moved = AddressRange::nonempty(GuestAddress::new(start), segment.length())
                .map_err(|_| MemoryError::InvalidRange)?;
            self.update(moved, Some(advice))?;
        }
        Ok(())
    }

    fn clear(&mut self, offset: u64, length: u64) -> Result<(), MemoryError> {
        let range = AddressRange::nonempty(GuestAddress::new(offset), length).map_err(|_| MemoryError::InvalidRange)?;
        self.update(range, None)
    }
}

impl Memory {
    pub(super) fn update_advice(&self, range: AddressRange, advice: Option<ForkAdvice>) -> Result<(), MemoryError> {
        self.advice
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .update(range, advice)
    }

    pub(super) fn advice_segments(
        &self,
        range: AddressRange,
    ) -> Result<Vec<(AddressRange, Option<ForkAdvice>)>, MemoryError> {
        self.advice
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .segments(range)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hl_memory::{Backing, MapRequest, Placement, Protection};

    fn range(start: u64, length: u64) -> AddressRange {
        AddressRange::nonempty(GuestAddress::new(start), length).unwrap()
    }

    fn request(length: u64) -> MapRequest {
        MapRequest {
            placement: Placement::Fixed(GuestAddress::new(0)),
            length,
            alignment: 4096,
            protection: Protection::READ,
            backing: Backing::Anonymous {
                identity: 1,
                shared: false,
            },
            backing_offset: 0,
        }
    }

    #[test]
    fn interval_normalization() {
        let mut advice = Advice::default();
        advice.update(range(0, 12_288), Some(ForkAdvice::Omit)).unwrap();
        advice.update(range(4096, 4096), Some(ForkAdvice::Wipe)).unwrap();
        advice.update(range(4096, 4096), None).unwrap();
        let segments = advice.segments(range(0, 12_288)).unwrap();
        assert_eq!(segments.len(), 3);
        assert_eq!(segments[0].1, Some(ForkAdvice::Omit));
        assert_eq!(segments[1].1, None);
        assert_eq!(segments[2].1, Some(ForkAdvice::Omit));
        advice.update(range(4096, 4096), Some(ForkAdvice::Omit)).unwrap();
        assert_eq!(advice.segments(range(0, 12_288)).unwrap().len(), 1);
    }

    #[test]
    fn remap_intervals() {
        let mut advice = Advice::default();
        advice.update(range(4096, 4096), Some(ForkAdvice::Omit)).unwrap();
        advice.update(range(8192, 4096), Some(ForkAdvice::Wipe)).unwrap();
        let moved = advice
            .apply(&[Operation::Remap(range(4096, 8192), 16_384, request(8192), false)])
            .unwrap();
        assert_eq!(
            moved.segments(range(4096, 8192)).unwrap(),
            vec![(range(4096, 8192), None)]
        );
        assert_eq!(
            moved.segments(range(16_384, 8192)).unwrap()[0].1,
            Some(ForkAdvice::Omit)
        );
        assert_eq!(
            moved.segments(range(16_384, 8192)).unwrap()[1].1,
            Some(ForkAdvice::Wipe)
        );

        let kept = advice
            .apply(&[Operation::Remap(range(4096, 8192), 24_576, request(8192), true)])
            .unwrap();
        assert_eq!(kept.segments(range(4096, 8192)).unwrap()[0].1, Some(ForkAdvice::Omit));
        assert_eq!(kept.segments(range(24_576, 8192)).unwrap()[0].1, Some(ForkAdvice::Omit));

        let grown = advice
            .apply(&[Operation::Remap(range(4096, 8192), 32_768, request(12_288), false)])
            .unwrap();
        let segments = grown.segments(range(32_768, 12_288)).unwrap();
        assert_eq!(segments[0].1, Some(ForkAdvice::Omit));
        assert_eq!(segments[1].1, Some(ForkAdvice::Wipe));
    }

    #[test]
    fn remap_limit_atomic() {
        let mut advice = Advice::default();
        for page in 0..Advice::LIMIT {
            let kind = if page % 2 == 0 {
                ForkAdvice::Omit
            } else {
                ForkAdvice::Wipe
            };
            advice
                .update(range((page as u64 + 1) * 4096, 4096), Some(kind))
                .unwrap();
        }
        let before = advice.clone();
        assert!(matches!(
            advice.apply(&[Operation::Remap(
                range(4096, Advice::LIMIT as u64 * 4096),
                (Advice::LIMIT as u64 + 2) * 4096,
                request(Advice::LIMIT as u64 * 4096),
                true,
            )]),
            Err(MemoryError::OutOfMemory),
        ));
        assert_eq!(advice.marks, before.marks);
    }
}
