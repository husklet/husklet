use hl_isa::GuestArchitecture;
use std::io::{IoSlice, IoSliceMut};

use crate::{CopyProgress, Errno, GuestAccess, GuestFault, GuestMemory};

pub const IOV_MAXIMUM: usize = 1024;
pub const MAX_RW_COUNT: u64 = 0x7fff_f000;
pub const USER_ADDRESS_LIMIT: u64 = 0x0001_0000_0000_0000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarshalError {
    Fault(GuestFault),
    Invalid,
    TooBig,
    Overflow,
}

impl MarshalError {
    #[must_use]
    pub const fn errno(self) -> Errno {
        match self {
            Self::Fault(_) => Errno::EFAULT,
            Self::Invalid => Errno::EINVAL,
            Self::TooBig => Errno::E2BIG,
            Self::Overflow => Errno::EOVERFLOW,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GuestIovec {
    pub base: u64,
    pub length: u64,
}

impl GuestIovec {
    fn validate(&self, access: GuestAccess) -> Result<(), MarshalError> {
        if self.length > i64::MAX as u64 {
            return Err(MarshalError::Invalid);
        }
        if self.length == 0 {
            return Ok(());
        }
        let end = self
            .base
            .checked_add(self.length)
            .ok_or(MarshalError::Fault(GuestFault {
                address: self.base,
                access,
            }))?;
        if end > USER_ADDRESS_LIMIT {
            return Err(MarshalError::Fault(GuestFault {
                address: self.base,
                access,
            }));
        }
        Ok(())
    }

    fn truncate(&mut self, remaining: &mut u64) {
        self.length = self.length.min(*remaining);
        *remaining -= self.length;
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IovecPlan {
    pub vectors: Vec<GuestIovec>,
    pub total_length: u64,
}

impl IovecPlan {
    fn bounded(mut vectors: Vec<GuestIovec>) -> Result<Self, MarshalError> {
        if vectors.iter().any(|vector| vector.length > i64::MAX as u64) {
            return Err(MarshalError::Invalid);
        }
        let mut remaining = MAX_RW_COUNT;
        for vector in &mut vectors {
            vector.truncate(&mut remaining);
        }
        Ok(Self {
            vectors,
            total_length: MAX_RW_COUNT - remaining,
        })
    }

    pub fn validate_io(self, access: GuestAccess) -> Result<Self, MarshalError> {
        for vector in &self.vectors {
            vector.validate(access)?;
        }
        Ok(self)
    }
}

pub struct VectorTransfer {
    vectors: Vec<GuestIovec>,
    buffers: Vec<Vec<u8>>,
}

impl VectorTransfer {
    pub fn capture<M: GuestMemory + ?Sized>(
        marshaller: &GuestMarshaller<'_, M>,
        plan: IovecPlan,
    ) -> Result<Self, MarshalError> {
        Self::capture_mode(marshaller, plan, false)
    }

    /// Captures every admitted byte or returns the first source fault without
    /// exposing a partial transfer to the descriptor operation.
    pub fn capture_all<M: GuestMemory + ?Sized>(
        marshaller: &GuestMarshaller<'_, M>,
        plan: IovecPlan,
    ) -> Result<Self, MarshalError> {
        Self::capture_mode(marshaller, plan, true)
    }

    fn capture_mode<M: GuestMemory + ?Sized>(
        marshaller: &GuestMarshaller<'_, M>,
        plan: IovecPlan,
        require_all: bool,
    ) -> Result<Self, MarshalError> {
        let mut buffers = Vec::with_capacity(plan.vectors.len());
        let mut copied = 0;
        for vector in &plan.vectors {
            let length = usize::try_from(vector.length).map_err(|_| MarshalError::Invalid)?;
            let mut bytes = vec![0; length];
            let progress = marshaller.copy_from(vector.base, &mut bytes);
            if progress.fault.is_some() && (require_all || progress.copied == 0 && copied == 0) {
                return Err(MarshalError::Fault(progress.fault.expect("checked fault")));
            }
            bytes.truncate(progress.copied);
            copied += progress.copied;
            buffers.push(bytes);
            if progress.fault.is_some() || progress.copied != length {
                break;
            }
        }
        Ok(Self {
            vectors: plan.vectors,
            buffers,
        })
    }

    #[must_use]
    pub fn vacant(plan: IovecPlan) -> Self {
        let buffers = plan
            .vectors
            .iter()
            .map(|vector| vec![0; vector.length as usize])
            .collect();
        Self {
            vectors: plan.vectors,
            buffers,
        }
    }

    pub fn input(&self) -> Vec<IoSlice<'_>> {
        self.buffers.iter().map(|bytes| IoSlice::new(bytes)).collect()
    }

    pub fn output(&mut self) -> Vec<IoSliceMut<'_>> {
        self.buffers.iter_mut().map(|bytes| IoSliceMut::new(bytes)).collect()
    }

    pub fn publish<M: GuestMemory + ?Sized>(&self, marshaller: &GuestMarshaller<'_, M>, count: usize) -> CopyProgress {
        let mut copied = 0;
        for (vector, bytes) in self.vectors.iter().zip(&self.buffers) {
            let length = bytes.len().min(count.saturating_sub(copied));
            if length == 0 {
                break;
            }
            let progress = marshaller.copy_to(vector.base, &bytes[..length]);
            copied += progress.copied;
            if progress.fault.is_some() || progress.copied != length {
                return CopyProgress {
                    copied,
                    fault: progress.fault,
                };
            }
        }
        CopyProgress::complete(copied)
    }
}

pub struct GuestMarshaller<'a, M: GuestMemory + ?Sized> {
    memory: &'a M,
    architecture: GuestArchitecture,
}

impl<'a, M: GuestMemory + ?Sized> GuestMarshaller<'a, M> {
    #[must_use]
    pub const fn new(memory: &'a M, architecture: GuestArchitecture) -> Self {
        Self { memory, architecture }
    }

    #[must_use]
    pub const fn architecture(&self) -> GuestArchitecture {
        self.architecture
    }

    pub fn copy_from(&self, source: u64, destination: &mut [u8]) -> CopyProgress {
        let Some(_) = source.checked_add(destination.len() as u64) else {
            return CopyProgress::fault(0, Self::fault(source, GuestAccess::Read));
        };
        let mut copied = 0;
        while copied < destination.len() {
            match self.memory.read(source + copied as u64, &mut destination[copied..]) {
                Ok(0) => {
                    return CopyProgress::fault(copied, Self::fault(source + copied as u64, GuestAccess::Read));
                }
                Ok(count) => copied += count.min(destination.len() - copied),
                Err(fault) => return CopyProgress::fault(copied, fault),
            }
        }
        CopyProgress::complete(copied)
    }

    pub fn copy_to(&self, destination: u64, source: &[u8]) -> CopyProgress {
        let Some(_) = destination.checked_add(source.len() as u64) else {
            return CopyProgress::fault(0, Self::fault(destination, GuestAccess::Write));
        };
        let mut copied = 0;
        while copied < source.len() {
            match self.memory.write(destination + copied as u64, &source[copied..]) {
                Ok(0) => {
                    return CopyProgress::fault(copied, Self::fault(destination + copied as u64, GuestAccess::Write));
                }
                Ok(count) => copied += count.min(source.len() - copied),
                Err(fault) => return CopyProgress::fault(copied, fault),
            }
        }
        CopyProgress::complete(copied)
    }

    pub fn probe(&self, address: u64, length: usize, access: GuestAccess) -> Result<usize, MarshalError> {
        if length == 0 {
            return Ok(0);
        }
        address
            .checked_add(length as u64)
            .ok_or(MarshalError::Fault(Self::fault(address, access)))?;
        self.memory
            .probe(address, length, access)
            .map(|available| available.min(length))
            .map_err(MarshalError::Fault)
    }

    pub fn c_string(&self, source: u64, capacity: usize) -> Result<Vec<u8>, MarshalError> {
        if source == 0 {
            return Err(MarshalError::Fault(Self::fault(0, GuestAccess::Read)));
        }
        if capacity == 0 {
            return Err(MarshalError::TooBig);
        }
        let mut result = Vec::with_capacity(capacity.min(256));
        for index in 0..capacity {
            let address = source
                .checked_add(index as u64)
                .ok_or(MarshalError::Fault(Self::fault(source, GuestAccess::Read)))?;
            let mut byte = [0];
            let progress = self.copy_from(address, &mut byte);
            if let Some(fault) = progress.fault {
                return Err(MarshalError::Fault(fault));
            }
            if byte[0] == 0 {
                return Ok(result);
            }
            result.push(byte[0]);
        }
        Err(MarshalError::TooBig)
    }

    pub fn pointer_vector(&self, source: u64, maximum: usize) -> Result<Vec<u64>, MarshalError> {
        if maximum == 0 {
            return Err(MarshalError::TooBig);
        }
        let mut pointers = Vec::new();
        for index in 0..maximum {
            let offset = index.checked_mul(8).ok_or(MarshalError::Overflow)?;
            let address = source
                .checked_add(offset as u64)
                .ok_or(MarshalError::Fault(Self::fault(source, GuestAccess::Read)))?;
            let pointer = self.read_word(address)?;
            if pointer == 0 {
                return Ok(pointers);
            }
            pointers.push(pointer);
        }
        Err(MarshalError::TooBig)
    }

    pub fn iovecs(&self, source: u64, count: usize) -> Result<IovecPlan, MarshalError> {
        let vectors = self.iovec_records(source, count)?;
        let total_length = vectors.iter().try_fold(0_u64, |total, vector| {
            total.checked_add(vector.length).ok_or(MarshalError::Overflow)
        })?;
        Ok(IovecPlan { vectors, total_length })
    }

    pub fn io_vectors(&self, source: u64, count: usize, access: GuestAccess) -> Result<IovecPlan, MarshalError> {
        self.io_vector_records(source, count, access)?.validate_io(access)
    }

    /// Imports the complete descriptor array without touching payload ranges.
    /// Native vector terminals need this Linux ordering so a later payload
    /// fault can expose an earlier accessible prefix through one host call.
    pub fn io_vector_records(
        &self,
        source: u64,
        count: usize,
        access: GuestAccess,
    ) -> Result<IovecPlan, MarshalError> {
        let vectors = self.iovec_records(source, count)?;
        for vector in &vectors {
            vector.validate(access)?;
        }
        IovecPlan::bounded(vectors)
    }

    fn iovec_records(&self, source: u64, count: usize) -> Result<Vec<GuestIovec>, MarshalError> {
        if count > IOV_MAXIMUM {
            return Err(MarshalError::Invalid);
        }
        let byte_count = count.checked_mul(16).ok_or(MarshalError::Overflow)?;
        source
            .checked_add(byte_count as u64)
            .ok_or(MarshalError::Fault(Self::fault(source, GuestAccess::Read)))?;
        let mut bytes = vec![0; byte_count];
        let progress = self.copy_from(source, &mut bytes);
        if let Some(fault) = progress.fault {
            return Err(MarshalError::Fault(fault));
        }
        let mut vectors = Vec::with_capacity(count);
        for chunk in bytes.chunks_exact(16) {
            let base = Self::u64(chunk, 0);
            let length = Self::u64(chunk, 8);
            vectors.push(GuestIovec { base, length });
        }
        Ok(vectors)
    }

    pub fn socklen(&self, source: u64, maximum: u32) -> Result<u32, MarshalError> {
        let mut bytes = [0; 4];
        let progress = self.copy_from(source, &mut bytes);
        if let Some(fault) = progress.fault {
            return Err(MarshalError::Fault(fault));
        }
        let length = u32::from_le_bytes(bytes);
        if length > maximum {
            Err(MarshalError::Invalid)
        } else {
            Ok(length)
        }
    }

    pub fn write_socklen(&self, destination: u64, length: u32) -> Result<(), MarshalError> {
        let progress = self.copy_to(destination, &length.to_le_bytes());
        progress.fault.map_or(Ok(()), |fault| Err(MarshalError::Fault(fault)))
    }

    pub fn copy_struct_from<const SIZE: usize>(&self, source: u64) -> Result<[u8; SIZE], MarshalError> {
        let mut bytes = [0; SIZE];
        let progress = self.copy_from(source, &mut bytes);
        progress
            .fault
            .map_or(Ok(bytes), |fault| Err(MarshalError::Fault(fault)))
    }

    pub fn copy_struct_to<const SIZE: usize>(&self, destination: u64, bytes: &[u8; SIZE]) -> Result<(), MarshalError> {
        let accessible = self.probe(destination, SIZE, GuestAccess::Write)?;
        if accessible != SIZE {
            let address = destination
                .checked_add(accessible as u64)
                .ok_or(MarshalError::Fault(Self::fault(destination, GuestAccess::Write)))?;
            return Err(MarshalError::Fault(Self::fault(address, GuestAccess::Write)));
        }
        let progress = self.copy_to(destination, bytes);
        progress.fault.map_or(Ok(()), |fault| Err(MarshalError::Fault(fault)))
    }

    fn read_word(&self, address: u64) -> Result<u64, MarshalError> {
        let mut bytes = [0; 8];
        let progress = self.copy_from(address, &mut bytes);
        if let Some(fault) = progress.fault {
            return Err(MarshalError::Fault(fault));
        }
        match self.architecture {
            GuestArchitecture::Aarch64 | GuestArchitecture::X86_64 => Ok(u64::from_le_bytes(bytes)),
        }
    }

    fn u64(bytes: &[u8], offset: usize) -> u64 {
        u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("word"))
    }

    const fn fault(address: u64, access: GuestAccess) -> GuestFault {
        GuestFault { address, access }
    }
}
