use hl_descriptor::{DescriptionIdentity, DescriptorFlags};

use crate::{Control, ControlError, RuntimeDescriptorTable};

impl Control {
    /// Creates a caller-private descriptor table and applies one range
    /// operation to the unpublished copy.
    ///
    /// Open-file descriptions and event objects remain shared with the source
    /// table. Descriptor flags and membership become private to the returned
    /// table. The source is not mutated when validation or candidate mutation
    /// fails.
    pub fn unshare_range(
        &self,
        table: &RuntimeDescriptorTable,
        first: u32,
        last: u32,
        close_on_exec: bool,
    ) -> Result<RuntimeDescriptorTable, ControlError> {
        if first > last {
            return Err(ControlError::Descriptor(
                hl_descriptor::DescriptorError::InvalidArgument,
            ));
        }

        let candidate = self.fork(table);
        self.close_range(&candidate, first, last, close_on_exec)?;
        Ok(candidate)
    }

    /// Applies a descriptor-range operation while event ownership is
    /// serialized with ordinary close and duplication.
    pub fn close_range(
        &self,
        table: &RuntimeDescriptorTable,
        first: u32,
        last: u32,
        close_on_exec: bool,
    ) -> Result<(), ControlError> {
        let _mutation = self.mutation.lock().unwrap_or_else(|error| error.into_inner());
        let descriptors = table.descriptor_table();
        let selected = descriptors
            .active_snapshots()
            .into_iter()
            .filter(|snapshot| {
                let number = snapshot.number as u32;
                number >= first && number <= last
            })
            .collect::<Vec<_>>();
        if close_on_exec {
            for snapshot in selected {
                let flags = DescriptorFlags::from_bits(snapshot.flags.bits() | DescriptorFlags::CLOSE_ON_EXEC);
                descriptors.set_flags(snapshot.number, flags)?;
            }
            return Ok(());
        }
        for snapshot in selected {
            descriptors.close(snapshot.number)?;
            if snapshot.descriptor_references == 1 {
                self.retire_identity(DescriptionIdentity {
                    identity: snapshot.description_identity,
                    generation: snapshot.description_generation,
                });
            }
        }
        Ok(())
    }
}
