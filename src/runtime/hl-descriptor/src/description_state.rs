use std::sync::atomic::Ordering;

use crate::{DescriptorError, DescriptorSnapshot, DescriptorTable, LeaseKind, SignalOwner, StatusFlags};

impl DescriptorTable {
    /// Returns a pointer-free snapshot of descriptor and shared OFD state.
    pub fn snapshot(&self, number: i32) -> Result<DescriptorSnapshot, DescriptorError> {
        let state = self.state.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        let descriptor = state.entries.get(&number).ok_or(DescriptorError::BadDescriptor)?;
        let description_state = descriptor
            .description
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(DescriptorSnapshot {
            number,
            description_identity: descriptor.description.identity,
            offset: description_state.offset,
            status: description_state.status,
            flags: descriptor.flags,
            descriptor_generation: descriptor.generation,
            description_generation: descriptor.description.generation,
            descriptor_references: descriptor.description.descriptor_references.load(Ordering::Acquire),
            kind: descriptor.description.object.kind(),
            flock_token: descriptor.description.identity,
        })
    }

    /// Updates OFD-local status flags shared by all aliases.
    pub fn set_status(&self, number: i32, status: StatusFlags) -> Result<(), DescriptorError> {
        let _checkpoint = self.checkpoint.operation()?;
        let descriptor = self.lookup(number)?;
        descriptor
            .description
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .status = status;
        Ok(())
    }

    /// Updates the shared OFD offset.
    pub fn set_offset(&self, number: i32, offset: u64) -> Result<(), DescriptorError> {
        let _checkpoint = self.checkpoint.operation()?;
        let descriptor = self.lookup(number)?;
        descriptor
            .description
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .offset = offset;
        Ok(())
    }
}

impl crate::OperationLease {
    /// Returns the asynchronous-I/O owner shared by every alias of this OFD.
    #[must_use]
    pub fn signal_owner(&self) -> SignalOwner {
        self.description
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .owner
    }

    /// Changes the asynchronous-I/O owner for this OFD.
    pub fn set_signal_owner(&self, owner: SignalOwner) {
        self.description
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .owner = owner;
    }

    /// Returns the configured signal, where zero selects Linux's SIGIO default.
    #[must_use]
    pub fn delivery_signal(&self) -> u8 {
        self.description
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .signal
    }

    /// Changes the signal used for asynchronous-I/O notification.
    pub fn set_delivery_signal(&self, signal: u8) {
        self.description
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .signal = signal;
    }

    /// Returns the lease currently held through this OFD.
    #[must_use]
    pub fn lease_kind(&self) -> Option<LeaseKind> {
        self.description
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .lease
    }

    /// Replaces the lease held through this OFD.
    pub fn set_lease_kind(&self, lease: Option<LeaseKind>) {
        self.description
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .lease = lease;
    }

    /// Returns the write-lifetime hint associated with this OFD.
    #[must_use]
    pub fn write_life_hint(&self) -> u64 {
        self.description
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .write_life_hint
    }

    /// Changes the write-lifetime hint associated with this OFD.
    pub fn set_write_life_hint(&self, hint: u64) {
        self.description
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .write_life_hint = hint;
    }
}
