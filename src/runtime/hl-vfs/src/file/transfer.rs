use std::sync::Arc;

use hl_descriptor::{ObjectError, OperationCancellation};

use super::cursor::Cursor;
use super::description::{VfsFileDescription, VfsFileHost};

impl<H: VfsFileHost> VfsFileDescription<H> {
    pub(super) fn copy_to(
        &self,
        target: &dyn hl_descriptor::OpenFileDescription,
        input_offset: Option<u64>,
        output_offset: Option<u64>,
        maximum: usize,
        nonblocking: bool,
        cancellation: Option<&dyn OperationCancellation>,
    ) -> Result<(usize, u64, u64), ObjectError> {
        let target = target
            .domain_extension()
            .and_then(|extension| extension.downcast_ref::<VfsFileDescription<H>>())
            .ok_or(ObjectError::NotSupported)?;
        let transfer = FileTransfer::prepare(
            self,
            input_offset,
            target,
            output_offset,
            maximum,
            nonblocking,
            cancellation,
        )?;
        let input_start = transfer.input_start;
        let output_start = transfer.output_start;
        let mut buffer = vec![0_u8; maximum.min(65_536)];
        let mut done = 0_usize;
        while done < maximum {
            let chunk = (maximum - done).min(buffer.len());
            let read = match self.pread(input_start + done as u64, &mut buffer[..chunk]) {
                Ok(0) => break,
                Ok(read) => read,
                Err(error) if done == 0 => return Err(error),
                Err(_) => break,
            };
            let written = match target.pwrite(output_start + done as u64, &buffer[..read]) {
                Ok(written) => written.min(read),
                Err(error) if done == 0 => return Err(error),
                Err(_) => break,
            };
            done += written;
            if written < read {
                break;
            }
        }
        transfer.commit(done)?;
        Ok((done, input_start, output_start))
    }
}

/// Reserved regular-file cursor state for one two-description transfer.
///
/// Implicit cursors remain unchanged until `commit`. Dropping this value rolls
/// every reservation back. Aliased descriptions use one reservation and are
/// rejected when their effective ranges overlap.
pub struct FileTransfer {
    input: Option<(Arc<Cursor>, u64)>,
    output: Option<(Arc<Cursor>, u64)>,
    maximum: usize,
    input_start: u64,
    output_start: u64,
}

impl FileTransfer {
    /// Reserves every implicit cursor in stable address order.
    ///
    /// # Errors
    ///
    /// Returns the description access/lifecycle error, `WouldBlock` for an
    /// occupied nonblocking cursor, `Interrupted` when cancellation wins, or
    /// `InvalidArgument` for an overflowing or same-file overlapping range.
    pub fn prepare<H: VfsFileHost>(
        input: &VfsFileDescription<H>,
        input_offset: Option<u64>,
        output: &VfsFileDescription<H>,
        output_offset: Option<u64>,
        maximum: usize,
        nonblocking: bool,
        cancellation: Option<&dyn OperationCancellation>,
    ) -> Result<Self, ObjectError> {
        input.ensure_readable()?;
        output.ensure_writable()?;

        let input_cursor = input_offset.is_none().then(|| Arc::clone(&input.cursor));
        let output_cursor = output_offset.is_none().then(|| Arc::clone(&output.cursor));
        let alias = input_cursor
            .as_ref()
            .zip(output_cursor.as_ref())
            .is_some_and(|(left, right)| Arc::ptr_eq(left, right));

        let mut acquired = Vec::with_capacity(2);
        if let Some(cursor) = input_cursor.as_ref() {
            acquired.push((Cursor::address(cursor), Arc::clone(cursor), input));
        }
        if let Some(cursor) = output_cursor.as_ref()
            && !alias
        {
            acquired.push((Cursor::address(cursor), Arc::clone(cursor), output));
        }
        acquired.sort_unstable_by_key(|entry| entry.0);

        let mut reservations: Vec<(Arc<Cursor>, u64)> = Vec::with_capacity(acquired.len());
        for (_, cursor, description) in acquired {
            let start = match cursor.reserve(nonblocking, cancellation, || description.is_retired()) {
                Ok(start) => start,
                Err(error) => {
                    for (held, _) in &reservations {
                        held.release();
                    }
                    return Err(error);
                }
            };
            reservations.push((cursor, start));
        }

        let reserved_start = |cursor: &Arc<Cursor>| {
            reservations
                .iter()
                .find(|(held, _)| Arc::ptr_eq(held, cursor))
                .map(|(_, start)| *start)
                .ok_or(ObjectError::WouldBlock)
        };
        let input_start = match input_offset {
            Some(offset) => offset,
            None => reserved_start(input_cursor.as_ref().ok_or(ObjectError::WouldBlock)?)?,
        };
        let output_start = match output_offset {
            Some(offset) => offset,
            None => reserved_start(output_cursor.as_ref().ok_or(ObjectError::WouldBlock)?)?,
        };

        let input_end = input_start.checked_add(maximum as u64);
        let output_end = output_start.checked_add(maximum as u64);
        let overlaps = maximum != 0
            && input.identity() == output.identity()
            && input_end
                .zip(output_end)
                .is_some_and(|(input_end, output_end)| input_start < output_end && output_start < input_end);
        if input_end.is_none() || output_end.is_none() || overlaps {
            for (cursor, _) in reservations {
                cursor.release();
            }
            return Err(ObjectError::InvalidArgument);
        }

        Ok(Self {
            input: input_cursor.map(|cursor| (cursor, input_start)),
            output: output_cursor.map(|cursor| (cursor, output_start)),
            maximum,
            input_start,
            output_start,
        })
    }

    #[must_use]
    pub fn input_offset(&self) -> Option<u64> {
        self.input.as_ref().map(|(_, offset)| *offset)
    }

    #[must_use]
    pub fn output_offset(&self) -> Option<u64> {
        self.output.as_ref().map(|(_, offset)| *offset)
    }

    /// Advances both implicit cursors by the accepted transfer count.
    ///
    /// A short count is committed exactly, preserving partial-transfer
    /// behavior. All validation precedes either offset mutation.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` if `count` exceeds the prepared maximum or an
    /// offset would overflow. An invalidated reservation returns `WouldBlock`.
    pub fn commit(mut self, count: usize) -> Result<(), ObjectError> {
        if count > self.maximum {
            return Err(ObjectError::InvalidArgument);
        }
        for (_, start) in self.input.iter().chain(self.output.iter()) {
            start.checked_add(count as u64).ok_or(ObjectError::InvalidArgument)?;
        }
        if let (Some((input, start)), Some((output, _))) = (&self.input, &self.output)
            && Arc::ptr_eq(input, output)
        {
            input.commit(*start, count)?;
            self.input = None;
            self.output = None;
            return Ok(());
        }
        if let (Some((input, input_start)), Some((output, output_start))) = (&self.input, &self.output) {
            Cursor::commit_pair((input, *input_start), (output, *output_start), count)?;
            self.input = None;
            self.output = None;
            return Ok(());
        }
        if let Some((cursor, start)) = self.input.take() {
            cursor.commit(start, count)?;
        }
        if let Some((cursor, start)) = self.output.take() {
            cursor.commit(start, count)?;
        }
        Ok(())
    }
}

impl Drop for FileTransfer {
    fn drop(&mut self) {
        let aliased = self
            .input
            .as_ref()
            .zip(self.output.as_ref())
            .is_some_and(|((input, _), (output, _))| Arc::ptr_eq(input, output));
        if let Some((cursor, _)) = self.input.take() {
            cursor.release();
        }
        if let Some((cursor, _)) = self.output.take()
            && !aliased
        {
            cursor.release();
        }
    }
}
