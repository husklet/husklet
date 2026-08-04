use std::marker::PhantomData;
use std::sync::Arc;

use hl_descriptor::{ObjectError, OperationCancellation, PreparedSpliceRead};

use super::cursor::Cursor;
use super::description::{VfsFileDescription, VfsFileHost};

pub(super) struct VfsSpliceRead<H: VfsFileHost> {
    cursor: Arc<Cursor>,
    start: u64,
    bytes: Vec<u8>,
    reserved: bool,
    host_type: PhantomData<H>,
}

impl<H: VfsFileHost> VfsSpliceRead<H> {
    pub(super) fn prepare(
        description: &VfsFileDescription<H>,
        offset: Option<u64>,
        maximum: usize,
        nonblocking: bool,
        cancellation: Option<&dyn OperationCancellation>,
    ) -> Result<Self, ObjectError> {
        description.ensure_readable()?;
        let reserved = offset.is_none();
        let start = match offset {
            Some(offset) => offset,
            None => description
                .cursor
                .reserve(nonblocking, cancellation, || description.is_retired())?,
        };
        let mut bytes = vec![0_u8; maximum];
        let read = description
            .host
            .read_at(description.token, start, &mut bytes, nonblocking);
        let read = match read {
            Ok(read) => read.min(bytes.len()),
            Err(error) => {
                if reserved {
                    description.cursor.release();
                }
                return Err(error);
            }
        };
        bytes.truncate(read);
        Ok(Self {
            cursor: Arc::clone(&description.cursor),
            start,
            bytes,
            reserved,
            host_type: PhantomData,
        })
    }
}

impl<H: VfsFileHost> PreparedSpliceRead for VfsSpliceRead<H> {
    fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    fn commit(mut self: Box<Self>, count: usize) -> Result<(), ObjectError> {
        if count > self.bytes.len() {
            return Err(ObjectError::InvalidArgument);
        }
        if self.reserved {
            self.cursor.commit(self.start, count)?;
            self.reserved = false;
        }
        Ok(())
    }
}

impl<H: VfsFileHost> Drop for VfsSpliceRead<H> {
    fn drop(&mut self) {
        if self.reserved {
            self.cursor.release();
        }
    }
}
