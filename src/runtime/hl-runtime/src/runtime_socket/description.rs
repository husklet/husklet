use std::io::{IoSlice, IoSliceMut};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use hl_descriptor::{
    ObjectError, ObjectKind, OpenFileDescription, Readiness, ReadinessObserver, ReadinessSubscription, StatusFlags,
};
use hl_network::SocketHostIo;

use super::{RuntimeSocket, RuntimeSocketKind};

impl<H: SocketHostIo> OpenFileDescription for RuntimeSocket<H> {
    fn kind(&self) -> ObjectKind {
        ObjectKind::Socket
    }
    fn read(&self, output: &mut [u8]) -> Result<usize, ObjectError> {
        if let Some(netlink) = &self.netlink {
            return netlink.receive(output, false).map(|(count, _)| count);
        }
        match &self.kind {
            RuntimeSocketKind::Host { description, .. } => description.read(output),
            RuntimeSocketKind::Unix { pair, endpoint } => pair.endpoints[*endpoint].description.read(output),
            RuntimeSocketKind::UnixStandalone { .. } => {
                let (pair, endpoint) = self.standalone_connection().ok_or(ObjectError::NotSupported)?;
                pair.endpoints[endpoint].description.read(output)
            }
        }
    }
    fn probe_read(&self, maximum: usize) -> Result<Option<usize>, ObjectError> {
        if let Some(netlink) = &self.netlink {
            return Ok(netlink.ready().then_some(maximum));
        }
        match &self.kind {
            RuntimeSocketKind::Host { description, .. } => description.probe_read(maximum),
            RuntimeSocketKind::Unix { pair, endpoint } => pair.endpoints[*endpoint].description.probe_read(maximum),
            RuntimeSocketKind::UnixStandalone { .. } => {
                let (pair, endpoint) = self.standalone_connection().ok_or(ObjectError::NotSupported)?;
                pair.endpoints[endpoint].description.probe_read(maximum)
            }
        }
    }
    fn read_with_cancellation(
        &self,
        output: &mut [u8],
        cancellation: &dyn hl_descriptor::OperationCancellation,
    ) -> Result<usize, ObjectError> {
        if let Some(netlink) = &self.netlink {
            return netlink.receive(output, false).map(|(count, _)| count);
        }
        match &self.kind {
            RuntimeSocketKind::Host { description, .. } => description.read_with_cancellation(output, cancellation),
            RuntimeSocketKind::Unix { pair, endpoint } => pair.endpoints[*endpoint]
                .description
                .read_with_cancellation(output, cancellation),
            RuntimeSocketKind::UnixStandalone { .. } => {
                let (pair, endpoint) = self.standalone_connection().ok_or(ObjectError::NotSupported)?;
                pair.endpoints[endpoint]
                    .description
                    .read_with_cancellation(output, cancellation)
            }
        }
    }
    fn write(&self, input: &[u8]) -> Result<usize, ObjectError> {
        if let Some(netlink) = &self.netlink {
            return netlink.send(input);
        }
        match &self.kind {
            RuntimeSocketKind::Host { description, .. } => description.write(input),
            RuntimeSocketKind::Unix { pair, endpoint } => pair.endpoints[*endpoint].description.write(input),
            RuntimeSocketKind::UnixStandalone { .. } => {
                let (pair, endpoint) = self.standalone_connection().ok_or(ObjectError::NotSupported)?;
                pair.endpoints[endpoint].description.write(input)
            }
        }
    }
    fn write_with_cancellation(
        &self,
        input: &[u8],
        cancellation: &dyn hl_descriptor::OperationCancellation,
    ) -> Result<usize, ObjectError> {
        if let Some(netlink) = &self.netlink {
            return netlink.send(input);
        }
        match &self.kind {
            RuntimeSocketKind::Host { description, .. } => description.write_with_cancellation(input, cancellation),
            RuntimeSocketKind::Unix { pair, endpoint } => pair.endpoints[*endpoint]
                .description
                .write_with_cancellation(input, cancellation),
            RuntimeSocketKind::UnixStandalone { .. } => {
                let (pair, endpoint) = self.standalone_connection().ok_or(ObjectError::NotSupported)?;
                pair.endpoints[endpoint]
                    .description
                    .write_with_cancellation(input, cancellation)
            }
        }
    }
    fn read_vector_context(
        &self,
        output: &mut [IoSliceMut<'_>],
        context: hl_descriptor::OperationContext<'_>,
    ) -> Result<usize, ObjectError> {
        let capacity = output
            .iter()
            .try_fold(0_usize, |total, part| total.checked_add(part.len()))
            .ok_or(ObjectError::ResourceLimit)?;
        let mut buffer = vec![0_u8; capacity];
        let count = self.read_context(&mut buffer, context)?;
        let mut copied = 0;
        for part in output {
            let length = part.len().min(count.saturating_sub(copied));
            part[..length].copy_from_slice(&buffer[copied..copied + length]);
            copied += length;
            if copied == count {
                break;
            }
        }
        Ok(count)
    }
    fn write_vector_context(
        &self,
        input: &[IoSlice<'_>],
        context: hl_descriptor::OperationContext<'_>,
    ) -> Result<usize, ObjectError> {
        let length = input
            .iter()
            .try_fold(0_usize, |total, part| total.checked_add(part.len()))
            .ok_or(ObjectError::ResourceLimit)?;
        let mut buffer = Vec::with_capacity(length);
        for part in input {
            buffer.extend_from_slice(part);
        }
        self.write_context(&buffer, context)
    }
    fn set_status_flags(&self, flags: StatusFlags) -> Result<(), ObjectError> {
        if self.netlink.is_some() {
            self.snapshot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .nonblocking = flags.bits() & StatusFlags::NONBLOCKING != 0;
            return Ok(());
        }
        match &self.kind {
            RuntimeSocketKind::Host { description, .. } => description.set_status_flags(flags),
            RuntimeSocketKind::Unix { pair, endpoint } => pair.endpoints[*endpoint].description.set_status_flags(flags),
            RuntimeSocketKind::UnixStandalone { .. } => match self.standalone_connection() {
                Some((pair, endpoint)) => pair.endpoints[endpoint].description.set_status_flags(flags),
                None => Ok(()),
            },
        }?;
        let mut snapshot = self.snapshot.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        snapshot.nonblocking = flags.bits() & StatusFlags::NONBLOCKING != 0;
        self.catalog
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .catalog
            .replace_snapshot(self.id, snapshot.clone())
            .map_err(|_| ObjectError::Io)
    }
    fn readiness(&self, interests: Readiness) -> Readiness {
        if let Some(netlink) = &self.netlink {
            let bits = Readiness::WRITE | if netlink.ready() { Readiness::READ } else { 0 };
            return Readiness::from_bits(bits & interests.bits());
        }
        let readiness = match &self.kind {
            RuntimeSocketKind::Host { description, .. } => description.readiness(interests),
            RuntimeSocketKind::Unix { pair, endpoint } => pair.endpoints[*endpoint].description.readiness(interests),
            RuntimeSocketKind::UnixStandalone { named, .. } => match self.standalone_connection() {
                Some((pair, endpoint)) => pair.endpoints[endpoint].description.readiness(interests),
                None if named.as_ref().is_some_and(|socket| socket.readable()) => Readiness::from_bits(Readiness::READ),
                None => Readiness::default(),
            },
        };
        match self.connect_status() {
            Ok(_) => readiness,
            Err(_) => Readiness::from_bits(Readiness::ERROR),
        }
    }
    fn subscribe_readiness(
        &self,
        observer: Arc<dyn ReadinessObserver>,
    ) -> Result<Box<dyn ReadinessSubscription>, ObjectError> {
        if let Some(netlink) = &self.netlink {
            return netlink.observe(observer);
        }
        match &self.kind {
            RuntimeSocketKind::Host { description, .. } => description.subscribe_readiness(observer),
            RuntimeSocketKind::Unix { pair, endpoint } => {
                pair.endpoints[*endpoint].description.subscribe_readiness(observer)
            }
            RuntimeSocketKind::UnixStandalone { .. } => match self.standalone_connection() {
                Some((pair, endpoint)) => pair.endpoints[endpoint].description.subscribe_readiness(observer),
                None => Err(ObjectError::NotSupported),
            },
        }
    }
    fn retire(&self) {
        if self.netlink.is_some() {
            self.unregister();
            return;
        }
        match &self.kind {
            RuntimeSocketKind::Host { description, .. } => description.retire(),
            RuntimeSocketKind::Unix { pair, endpoint } => pair.endpoints[*endpoint].description.retire(),
            RuntimeSocketKind::UnixStandalone { .. } => {
                if let Some((pair, endpoint)) = self.standalone_connection() {
                    pair.endpoints[endpoint].description.retire();
                }
            }
        }
        self.unregister();
    }
    fn close(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        if self.netlink.is_some() {
            self.unregister();
            self.catalog
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .release();
            return;
        }
        match &self.kind {
            RuntimeSocketKind::Host { description, .. } => description.close(),
            RuntimeSocketKind::Unix { pair, endpoint } => pair.endpoints[*endpoint].description.close(),
            RuntimeSocketKind::UnixStandalone { .. } => {
                if let Some((pair, endpoint)) = self.standalone_connection() {
                    pair.endpoints[endpoint].description.close();
                }
            }
        }
        if let Some(datagram) = self.unix_datagram() {
            datagram.close();
        }
        self.unregister();
        if let Some((namespace, address, binding)) = self
            .unix_binding
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            namespace.release(&address, binding);
        }
        self.catalog
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .release();
    }
}
