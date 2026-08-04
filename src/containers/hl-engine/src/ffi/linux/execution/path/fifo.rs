use std::sync::Arc;

use hl_descriptor::{
    DescriptionIdentity, ObjectError, ObjectKind, OfdMetadata, OpenFileDescription, OperationCancellation,
    OperationContext, Readiness, StatusFlags,
};
use hl_ipc::{NamedFifoCatalog, NamedFifoKey, NamedFifoOpen, NamedFifoOpenError, PipeEndpoint};
use hl_linux::OpenAbiPlan;
use hl_runtime::{OpenIntent, PreparedPathOpen, RuntimePathError};

#[derive(Default)]
pub(super) struct Registry {
    catalog: NamedFifoCatalog,
}

impl Registry {
    pub(super) fn prepare(
        &self,
        key: NamedFifoKey,
        plan: &OpenAbiPlan,
    ) -> Result<Box<dyn PreparedPathOpen>, RuntimePathError> {
        let reading = plan.intent.bits() & OpenIntent::READ != 0;
        let writing = plan.intent.bits() & OpenIntent::WRITE != 0;
        let fifo = self.catalog.open(key);
        if reading && writing {
            let (reader, writer) = fifo.open_readwrite(plan.nonblocking);
            return Ok(Box::new(Pending::new(Arc::new(Duplex { reader, writer }))));
        }
        let opened = if writing {
            fifo.open_writer(plan.nonblocking).map_err(|error| match error {
                NamedFifoOpenError::NoReader => RuntimePathError::NoDevice,
            })?
        } else {
            fifo.open_reader(plan.nonblocking)
        };
        match opened {
            NamedFifoOpen::Ready(endpoint) => Ok(Box::new(Pending::new(endpoint))),
            NamedFifoOpen::Waiting(wait) => Ok(Box::new(Pending::new(wait.wait()))),
        }
    }
}

#[derive(Debug)]
struct Duplex {
    reader: Arc<PipeEndpoint>,
    writer: Arc<PipeEndpoint>,
}

impl OpenFileDescription for Duplex {
    fn kind(&self) -> ObjectKind {
        ObjectKind::Pipe
    }

    fn metadata(&self) -> Result<OfdMetadata, ObjectError> {
        self.reader.metadata()
    }

    fn read(&self, output: &mut [u8]) -> Result<usize, ObjectError> {
        self.reader.read(output)
    }

    fn read_with_cancellation(
        &self,
        output: &mut [u8],
        cancellation: &dyn OperationCancellation,
    ) -> Result<usize, ObjectError> {
        self.reader.read_with_cancellation(output, cancellation)
    }

    fn write(&self, input: &[u8]) -> Result<usize, ObjectError> {
        self.writer.write(input)
    }

    fn write_with_cancellation(
        &self,
        input: &[u8],
        cancellation: &dyn OperationCancellation,
    ) -> Result<usize, ObjectError> {
        self.writer.write_with_cancellation(input, cancellation)
    }

    fn read_vector_context(
        &self,
        output: &mut [std::io::IoSliceMut<'_>],
        context: OperationContext<'_>,
    ) -> Result<usize, ObjectError> {
        self.reader.read_vector_context(output, context)
    }

    fn write_vector_context(
        &self,
        input: &[std::io::IoSlice<'_>],
        context: OperationContext<'_>,
    ) -> Result<usize, ObjectError> {
        self.writer.write_vector_context(input, context)
    }

    fn set_status_flags(&self, flags: StatusFlags) -> Result<(), ObjectError> {
        self.reader.set_status_flags(flags)?;
        self.writer.set_status_flags(flags)
    }

    fn pipe_capacity(&self) -> Result<usize, ObjectError> {
        self.reader.pipe_capacity()
    }

    fn set_pipe_capacity(&self, requested: usize) -> Result<usize, ObjectError> {
        self.reader.set_pipe_capacity(requested)
    }

    fn readiness(&self, interests: Readiness) -> Readiness {
        Readiness::from_bits(self.reader.readiness(interests).bits() | self.writer.readiness(interests).bits())
    }

    fn retire(&self) {
        self.reader.retire();
        self.writer.retire();
    }

    fn close(&self) {
        self.reader.close();
        self.writer.close();
    }
}

struct Pending {
    endpoint: Arc<dyn OpenFileDescription>,
    published: bool,
}

impl Pending {
    fn new(endpoint: Arc<dyn OpenFileDescription>) -> Self {
        Self {
            endpoint,
            published: false,
        }
    }
}

impl std::fmt::Debug for Pending {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PendingNamedFifoOpen")
    }
}

impl PreparedPathOpen for Pending {
    fn object(&self) -> Arc<dyn OpenFileDescription> {
        self.endpoint.clone()
    }

    fn bind(&mut self, _identity: DescriptionIdentity) -> Result<(), RuntimePathError> {
        Ok(())
    }

    fn commit(&mut self) -> Result<(), RuntimePathError> {
        self.published = true;
        Ok(())
    }

    fn rollback(self: Box<Self>) {
        if !self.published {
            OpenFileDescription::close(self.endpoint.as_ref());
        }
    }
}
