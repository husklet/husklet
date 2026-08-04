use std::collections::BTreeMap;
use std::sync::{Arc, Weak};

use crate::model::OpenDescription;
use crate::table::FIRST_DESCRIPTOR;
use crate::{Descriptor, DescriptorError, DescriptorTable};

#[derive(Debug)]
pub(crate) struct TableState {
    pub(crate) entries: BTreeMap<i32, Descriptor>,
    pub(crate) reservations: BTreeMap<i32, u32>,
    pub(crate) generations: BTreeMap<i32, u32>,
    pub(crate) transfers: BTreeMap<u64, Weak<OpenDescription>>,
    pub(crate) checkpoint_roots: BTreeMap<u64, Arc<OpenDescription>>,
    pub(crate) limit: i32,
}

impl TableState {
    pub(crate) fn validate_number(&self, number: i32) -> Result<(), DescriptorError> {
        if number < FIRST_DESCRIPTOR || number >= self.limit {
            return Err(DescriptorError::BadDescriptor);
        }
        Ok(())
    }

    pub(crate) fn lowest_free(&self, minimum: i32) -> Result<i32, DescriptorError> {
        self.lowest_free_below(minimum, self.limit)
    }

    pub(crate) fn lowest_free_below(&self, minimum: i32, limit: i32) -> Result<i32, DescriptorError> {
        if minimum < FIRST_DESCRIPTOR {
            return Err(DescriptorError::InvalidArgument);
        }
        for candidate in minimum..limit.min(self.limit) {
            if !self.entries.contains_key(&candidate) && !self.reservations.contains_key(&candidate) {
                return Ok(candidate);
            }
        }
        Err(DescriptorError::TooManyOpenFiles)
    }

    pub(crate) fn advance_generation(&mut self, number: i32) -> u32 {
        let generation = self
            .generations
            .get(&number)
            .copied()
            .unwrap_or_default()
            .wrapping_add(1);
        let generation = generation.max(1);
        self.generations.insert(number, generation);
        generation
    }
}

impl Drop for DescriptorTable {
    fn drop(&mut self) {
        let state = self.state.get_mut().unwrap_or_else(|error| error.into_inner());
        let entries = std::mem::take(&mut state.entries);
        for descriptor in entries.into_values() {
            descriptor.description.release_descriptor();
        }
    }
}
