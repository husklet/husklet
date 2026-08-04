use super::abi::{Abi, AbiError, RangePlan};
use crate::{GuestAccess, GuestFault, GuestMemory, MarshalError, StagedMemoryCopyout};

impl<M: GuestMemory> Abi<'_, M> {
    pub fn mincore(&self, address: u64, length: u64, output: u64) -> Result<(Option<RangePlan>, u64), AbiError> {
        let plan = Self::mincore_plan(address, length)?;
        let Some(plan) = plan else {
            return Ok((None, output));
        };
        self.probe_mincore_output(output, plan.range.length())?;
        Ok((Some(plan), output))
    }

    pub fn mincore_plan(address: u64, length: u64) -> Result<Option<RangePlan>, AbiError> {
        if address & (super::abi::PAGE - 1) != 0 {
            return Err(AbiError::Invalid);
        }
        if length == 0 {
            return Ok(None);
        }
        let range = Self::range(address, length, true)?;
        Ok(Some(RangePlan {
            range,
            protection: None,
        }))
    }

    pub fn probe_mincore_output(&self, output: u64, length: u64) -> Result<(), AbiError> {
        let pages = length / super::abi::PAGE;
        let size = usize::try_from(pages).map_err(|_| AbiError::Overflow)?;
        let available = self.marshaller.probe(output, size, GuestAccess::Write)?;
        if available != size {
            return Err(Self::output_fault(output, available));
        }
        Ok(())
    }

    pub fn stage_mincore(&self, output: u64, residency: &[bool]) -> Result<StagedMemoryCopyout, AbiError> {
        let bytes = residency.iter().map(|resident| u8::from(*resident)).collect::<Vec<_>>();
        let available = self.marshaller.probe(output, bytes.len(), GuestAccess::Write)?;
        if available != bytes.len() {
            return Err(Self::output_fault(output, available));
        }
        Ok(StagedMemoryCopyout::single(output, bytes))
    }

    fn output_fault(output: u64, available: usize) -> AbiError {
        AbiError::Marshal(MarshalError::Fault(GuestFault {
            address: output.saturating_add(available as u64),
            access: GuestAccess::Write,
        }))
    }
}
