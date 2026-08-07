use std::collections::BTreeMap;

use hl_memory::{Backing, BackingChange, MapRequest, Protection, ReservationCoordinate};

use super::MemoryError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Mapping {
    pub(super) length: u64,
    pub(super) protection: Protection,
    pub(super) backing: Backing,
    pub(super) backing_offset: u64,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Operation {
    Backing(BackingChange),
    Map(u64, MapRequest),
    Remap(hl_isa::AddressRange, u64, MapRequest, bool),
    Unmap(u64, u64),
    Protect(u64, u64, Protection),
}

#[derive(Clone, Debug, Default)]
pub(super) struct Ledger(BTreeMap<u64, Mapping>);

impl Ledger {
    pub(super) fn access_prefix(&self, offset: u64, length: u64, required: Protection) -> Result<u64, MemoryError> {
        let end = offset.checked_add(length).ok_or(MemoryError::InvalidRange)?;
        if length == 0 {
            return Ok(0);
        }
        let mut cursor = offset;
        while cursor < end {
            let Some((start, mapping)) = self.0.range(..=cursor).next_back() else {
                break;
            };
            let mapping_end = start.checked_add(mapping.length).ok_or(MemoryError::InvalidRange)?;
            if cursor < *start || cursor >= mapping_end || !mapping.protection.contains(required) {
                break;
            }
            cursor = mapping_end.min(end);
        }
        Ok(cursor - offset)
    }

    pub(super) fn access(&self, offset: u64, length: u64, required: Protection) -> Result<(), MemoryError> {
        let end = offset.checked_add(length).ok_or(MemoryError::InvalidRange)?;
        if length == 0 {
            return Err(MemoryError::InvalidRange);
        }
        let mut cursor = offset;
        while cursor < end {
            let (start, mapping) = self.0.range(..=cursor).next_back().ok_or(MemoryError::InvalidRange)?;
            let mapping_end = start.checked_add(mapping.length).ok_or(MemoryError::InvalidRange)?;
            if cursor < *start || cursor >= mapping_end || !mapping.protection.contains(required) {
                return Err(MemoryError::InvalidRange);
            }
            cursor = mapping_end.min(end);
        }
        Ok(())
    }

    pub(super) fn apply(&mut self, operation: Operation) -> Result<(), MemoryError> {
        match operation {
            Operation::Backing(_) => {}
            Operation::Map(offset, request) => {
                if matches!(request.placement, hl_memory::Placement::Fixed(_)) {
                    self.overlay(offset, request.length)?;
                } else if self.overlaps(offset, request.length) {
                    return Err(MemoryError::InvalidRange);
                }
                self.0.insert(
                    offset,
                    Mapping {
                        length: request.length,
                        protection: request.protection,
                        backing: request.backing,
                        backing_offset: request.backing_offset,
                    },
                );
            }
            Operation::Remap(source, destination, mut request, keep) => {
                request.placement = hl_memory::Placement::Fixed(hl_isa::GuestAddress::new(destination));
                self.apply(Operation::Map(destination, request))?;
                if !keep {
                    self.apply(Operation::Unmap(source.start().get(), source.length()))?;
                }
            }
            Operation::Unmap(offset, length) => self.replace(offset, length, None, false)?,
            Operation::Protect(offset, length, protection) => {
                self.replace(offset, length, Some(protection), true)?;
            }
        }
        Ok(())
    }

    fn overlay(&mut self, offset: u64, length: u64) -> Result<(), MemoryError> {
        let end = offset.checked_add(length).ok_or(MemoryError::InvalidRange)?;
        let affected = self
            .0
            .range(..end)
            .filter_map(|(start, mapping)| {
                let mapping_end = start.checked_add(mapping.length)?;
                (mapping_end > offset).then_some((*start, *mapping))
            })
            .collect::<Vec<_>>();
        for (start, _) in &affected {
            self.0.remove(start);
        }
        for (start, mapping) in affected {
            let mapping_end = start.checked_add(mapping.length).ok_or(MemoryError::InvalidRange)?;
            if start < offset {
                self.0.insert(
                    start,
                    Mapping {
                        length: offset - start,
                        protection: mapping.protection,
                        backing: mapping.backing,
                        backing_offset: mapping.backing_offset,
                    },
                );
            }
            if mapping_end > end {
                self.0.insert(
                    end,
                    Mapping {
                        length: mapping_end - end,
                        protection: mapping.protection,
                        backing: mapping.backing,
                        backing_offset: mapping
                            .backing_offset
                            .checked_add(end - start)
                            .ok_or(MemoryError::InvalidRange)?,
                    },
                );
            }
        }
        Ok(())
    }

    pub(super) fn inverse(&self, operation: Operation) -> Result<Vec<Operation>, MemoryError> {
        match operation {
            Operation::Backing(_) => Ok(Vec::new()),
            Operation::Map(offset, request) => Ok(vec![Operation::Unmap(offset, request.length)]),
            Operation::Remap(_, _, _, _) => Ok(Vec::new()),
            Operation::Unmap(offset, length) => Ok(self
                .intersections(offset, length)?
                .into_iter()
                .map(|(start, mapping)| Operation::Protect(start, mapping.length, mapping.protection))
                .collect()),
            Operation::Protect(offset, length, _) => Ok(self
                .prior(offset, length)?
                .into_iter()
                .map(|(start, mapping)| Operation::Protect(start, mapping.length, mapping.protection))
                .collect()),
        }
    }

    fn replace(
        &mut self,
        offset: u64,
        length: u64,
        protection: Option<Protection>,
        require_coverage: bool,
    ) -> Result<(), MemoryError> {
        let end = offset.checked_add(length).ok_or(MemoryError::InvalidRange)?;
        if require_coverage {
            self.prior(offset, length)?;
        } else {
            self.intersections(offset, length)?;
        }
        let affected = self
            .0
            .range(..end)
            .filter_map(|(start, mapping)| {
                let mapping_end = start.checked_add(mapping.length)?;
                (mapping_end > offset).then_some((*start, *mapping))
            })
            .collect::<Vec<_>>();
        for (start, _) in &affected {
            self.0.remove(start);
        }
        for (start, mapping) in affected {
            let mapping_end = start.checked_add(mapping.length).ok_or(MemoryError::InvalidRange)?;
            if start < offset {
                self.0.insert(
                    start,
                    Mapping {
                        length: offset - start,
                        protection: mapping.protection,
                        backing: mapping.backing,
                        backing_offset: mapping.backing_offset,
                    },
                );
            }
            let middle_start = start.max(offset);
            let middle_end = mapping_end.min(end);
            if let Some(value) = protection {
                self.0.insert(
                    middle_start,
                    Mapping {
                        length: middle_end - middle_start,
                        protection: value,
                        backing: mapping.backing,
                        backing_offset: mapping
                            .backing_offset
                            .checked_add(middle_start - start)
                            .ok_or(MemoryError::InvalidRange)?,
                    },
                );
            }
            if mapping_end > end {
                self.0.insert(
                    end,
                    Mapping {
                        length: mapping_end - end,
                        protection: mapping.protection,
                        backing: mapping.backing,
                        backing_offset: mapping
                            .backing_offset
                            .checked_add(end - start)
                            .ok_or(MemoryError::InvalidRange)?,
                    },
                );
            }
        }
        self.coalesce();
        Ok(())
    }

    fn prior(&self, offset: u64, length: u64) -> Result<Vec<(u64, Mapping)>, MemoryError> {
        let end = offset.checked_add(length).ok_or(MemoryError::InvalidRange)?;
        if length == 0 {
            return Err(MemoryError::InvalidRange);
        }
        let mut cursor = offset;
        let mut ranges = Vec::new();
        while cursor < end {
            let (start, mapping) = self.0.range(..=cursor).next_back().ok_or(MemoryError::InvalidRange)?;
            let mapping_end = start.checked_add(mapping.length).ok_or(MemoryError::InvalidRange)?;
            if cursor < *start || cursor >= mapping_end {
                return Err(MemoryError::InvalidRange);
            }
            let range_end = mapping_end.min(end);
            ranges.push((
                cursor,
                Mapping {
                    length: range_end - cursor,
                    protection: mapping.protection,
                    backing: mapping.backing,
                    backing_offset: mapping
                        .backing_offset
                        .checked_add(cursor - start)
                        .ok_or(MemoryError::InvalidRange)?,
                },
            ));
            cursor = range_end;
        }
        Ok(ranges)
    }

    fn intersections(&self, offset: u64, length: u64) -> Result<Vec<(u64, Mapping)>, MemoryError> {
        let end = offset.checked_add(length).ok_or(MemoryError::InvalidRange)?;
        if length == 0 {
            return Err(MemoryError::InvalidRange);
        }
        self.0
            .range(..end)
            .filter_map(|(start, mapping)| {
                let Some(mapping_end) = start.checked_add(mapping.length) else {
                    return Some(Err(MemoryError::InvalidRange));
                };
                if mapping_end <= offset {
                    return None;
                }
                let intersection_start = (*start).max(offset);
                let intersection_end = mapping_end.min(end);
                let Some(backing_offset) = mapping.backing_offset.checked_add(intersection_start - start) else {
                    return Some(Err(MemoryError::InvalidRange));
                };
                Some(Ok((
                    intersection_start,
                    Mapping {
                        length: intersection_end - intersection_start,
                        protection: mapping.protection,
                        backing: mapping.backing,
                        backing_offset,
                    },
                )))
            })
            .collect()
    }

    fn overlaps(&self, offset: u64, length: u64) -> bool {
        self.0.iter().any(|(start, mapping)| {
            offset < start.saturating_add(mapping.length) && *start < offset.saturating_add(length)
        })
    }

    fn coalesce(&mut self) {
        let mut merged: BTreeMap<u64, Mapping> = BTreeMap::new();
        for (start, mapping) in std::mem::take(&mut self.0) {
            let adjacent = merged.iter().next_back().and_then(|(prior_start, prior)| {
                let end = start.checked_add(mapping.length)?;
                let adjacent_offset = prior.backing_offset.checked_add(prior.length);
                (prior_start.checked_add(prior.length) == Some(start)
                    && prior.protection == mapping.protection
                    && prior.backing == mapping.backing
                    && adjacent_offset == Some(mapping.backing_offset))
                .then_some((*prior_start, end, prior.backing_offset))
            });
            if let Some((prior_start, end, backing_offset)) = adjacent {
                merged.insert(
                    prior_start,
                    Mapping {
                        length: end - prior_start,
                        protection: mapping.protection,
                        backing: mapping.backing,
                        backing_offset,
                    },
                );
            } else {
                merged.insert(start, mapping);
            }
        }
        self.0 = merged;
    }

    pub(super) fn reservation(&self, address: u64) -> Result<(ReservationCoordinate, u64), MemoryError> {
        let (start, mapping) = self.0.range(..=address).next_back().ok_or(MemoryError::InvalidRange)?;
        let end = start.checked_add(mapping.length).ok_or(MemoryError::InvalidRange)?;
        if address < *start || address >= end {
            return Err(MemoryError::InvalidRange);
        }
        let backing_offset = mapping
            .backing_offset
            .checked_add(address - start)
            .ok_or(MemoryError::InvalidRange)?;
        let coordinate = ReservationCoordinate::from_mapping(mapping.backing, backing_offset, address)
            .map_err(|_| MemoryError::InvalidRange)?;
        Ok((coordinate, end))
    }
}
