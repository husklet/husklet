use std::io::{IoSlice, IoSliceMut};

use hl_descriptor::{ObjectError, OperationCancellation, StatusFlags};

use super::description::{VfsFileDescription, VfsFileHost};

impl<H: VfsFileHost> VfsFileDescription<H> {
    pub(super) fn read_vector(
        &self,
        output: &mut [IoSliceMut<'_>],
        cancellation: Option<&dyn OperationCancellation>,
    ) -> Result<usize, ObjectError> {
        self.ensure_readable()?;
        let mut state = self.lock_cursor(cancellation)?;
        let read = self.host.read_vector_at(
            self.token,
            state.offset,
            output,
            state.status.bits() & StatusFlags::NONBLOCKING != 0,
        )?;
        state.offset = state
            .offset
            .checked_add(read as u64)
            .ok_or(ObjectError::InvalidArgument)?;
        Ok(read)
    }

    pub(super) fn write_vector(
        &self,
        input: &[IoSlice<'_>],
        cancellation: Option<&dyn OperationCancellation>,
    ) -> Result<usize, ObjectError> {
        self.ensure_writable()?;
        let mut state = self.lock_cursor(cancellation)?;
        let nonblocking = state.status.bits() & StatusFlags::NONBLOCKING != 0;
        if state.status.bits() & StatusFlags::APPEND != 0 {
            let (written, end) = self.host.append_vector(self.token, input, nonblocking)?;
            state.offset = end;
            return Ok(written);
        }
        let written = self
            .host
            .write_vector_at(self.token, state.offset, input, nonblocking)?;
        state.offset = state
            .offset
            .checked_add(written as u64)
            .ok_or(ObjectError::InvalidArgument)?;
        Ok(written)
    }
}
