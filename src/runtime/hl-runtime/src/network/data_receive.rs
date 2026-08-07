//! Stream and datagram receive helpers for the network syscall surface.

use hl_linux::{Errno, GuestMemory};

use crate::{
    RuntimeNetworkHost, RuntimeNetworkSyscalls, filesystem::FileErrno, network::errno::SocketErrno,
    network::wait::SocketCancellation,
};

use super::ReadinessWake;

impl<H: RuntimeNetworkHost, M: GuestMemory> RuntimeNetworkSyscalls<H, M> {
    pub(super) fn host_receive(
        &self,
        socket: &crate::RuntimeSocket<H>,
        description: &std::sync::Arc<hl_network::SocketDescription<H>>,
        token: H::Token,
        output: &mut [u8],
        nonblocking: bool,
        source_requested: bool,
        peek: bool,
        waitall: bool,
        socket_type: hl_network::SocketType,
    ) -> Result<(usize, usize, Option<hl_network::SocketAddress>), Errno> {
        if socket_type == hl_network::SocketType::Stream && !source_requested && !peek {
            if let Some(deadline) = self.socket_deadline(socket)? {
                let count = self.read_stream_until(socket, description, output, waitall, deadline)?;
                return Ok((count, count, None));
            }
            let count = self.read_stream(socket, output, nonblocking, waitall)?;
            return Ok((count, count, None));
        }
        let host = self.host.as_ref().ok_or(Errno::ENOSYS)?;
        loop {
            match host.receive_from(token, output, true, peek) {
                Ok(received) => {
                    return Ok((
                        received.count,
                        received.full_length,
                        source_requested.then_some(received.source),
                    ));
                }
                Err(crate::RuntimeNetworkError::WouldBlock) if !nonblocking => {}
                Err(error) => return Err(SocketErrno::runtime(error)),
            }
            let wait = self.wait.as_ref().ok_or(Errno::EAGAIN)?;
            let cancellation = SocketCancellation::new(wait.interruption());
            description.wait_readable(&cancellation).map_err(FileErrno::object)?;
        }
    }

    fn read_stream_until(
        &self,
        socket: &crate::RuntimeSocket<H>,
        description: &std::sync::Arc<hl_network::SocketDescription<H>>,
        output: &mut [u8],
        waitall: bool,
        deadline: hl_time::Deadline,
    ) -> Result<usize, Errno> {
        let wait = self.wait.as_ref().ok_or(Errno::EAGAIN)?;
        let queue = std::sync::Arc::new(hl_sync::WaitQueue::new());
        let _subscription = description
            .observe_readiness(std::sync::Arc::new(ReadinessWake(queue.clone())))
            .map_err(FileErrno::object)?;
        let mut count = 0;
        loop {
            let observed = queue.observation();
            let progress = match socket.read_with(&mut output[count..], true) {
                Ok(0) => return Ok(count),
                Ok(read) => Some(read),
                Err(hl_descriptor::ObjectError::WouldBlock) => match wait.wait(&queue, observed, Some(deadline)) {
                    Ok(hl_sync::WaitOutcome::Notified) => None,
                    Ok(hl_sync::WaitOutcome::Interrupted) => return Err(Errno::EINTR),
                    Ok(hl_sync::WaitOutcome::TimedOut) if count == 0 => return Err(Errno::EAGAIN),
                    Ok(hl_sync::WaitOutcome::TimedOut) => return Ok(count),
                    Err(_) => return Err(Errno::EIO),
                },
                Err(error) if count == 0 => return Err(FileErrno::object(error)),
                Err(_) => return Ok(count),
            };
            let Some(read) = progress else {
                continue;
            };
            count += read;
            if !waitall || count == output.len() {
                return Ok(count);
            }
        }
    }

    pub(super) fn unix_receive(
        &self,
        socket: &crate::RuntimeSocket<H>,
        endpoint: &hl_network::UnixSocketEndpoint,
        output: &mut [u8],
        nonblocking: bool,
        source_requested: bool,
        requested_source: Option<hl_network::SocketAddress>,
        peek: bool,
        waitall: bool,
        socket_type: hl_network::SocketType,
    ) -> Result<(usize, usize, Option<hl_network::SocketAddress>), Errno> {
        if socket_type == hl_network::SocketType::Stream {
            let count = if peek {
                self.peek_stream(endpoint, output, nonblocking)?
            } else {
                self.read_stream(socket, output, nonblocking, waitall)?
            };
            return Ok((count, count, requested_source));
        }
        if matches!(&socket.kind, crate::RuntimeSocketKind::UnixStandalone { .. }) && !source_requested && !peek {
            let count = if nonblocking {
                socket.read_with(output, true).map_err(FileErrno::object)?
            } else if let Some(wait) = &self.wait {
                let cancellation = SocketCancellation::new(wait.interruption());
                socket.read_blocking(output, &cancellation).map_err(FileErrno::object)?
            } else {
                socket.read_with(output, true).map_err(FileErrno::object)?
            };
            return Ok((count, count, None));
        }
        let source = if source_requested {
            Some(requested_source.ok_or(Errno::ENOTCONN)?)
        } else {
            None
        };
        endpoint
            .receive_record(output, true, peek)
            .map(|(count, full_length)| (count, full_length, source))
            .map_err(SocketErrno::socket_host)
    }

    fn read_stream(
        &self,
        socket: &crate::RuntimeSocket<H>,
        output: &mut [u8],
        nonblocking: bool,
        waitall: bool,
    ) -> Result<usize, Errno> {
        let mut count = 0;
        loop {
            let result = if nonblocking {
                socket.read_with(&mut output[count..], true)
            } else if let Some(wait) = &self.wait {
                let cancellation = SocketCancellation::new(wait.interruption());
                socket.read_blocking(&mut output[count..], &cancellation)
            } else {
                socket.read_with(&mut output[count..], false)
            };
            match result {
                Ok(0) => return Ok(count),
                Ok(read) => count += read,
                Err(error) if count == 0 => return Err(FileErrno::object(error)),
                Err(_) => return Ok(count),
            }
            if !waitall || count == output.len() {
                return Ok(count);
            }
        }
    }

    fn peek_stream(
        &self,
        endpoint: &hl_network::UnixSocketEndpoint,
        output: &mut [u8],
        nonblocking: bool,
    ) -> Result<usize, Errno> {
        loop {
            match endpoint.peek(output, true) {
                Ok(count) => return Ok(count),
                Err(hl_network::SocketHostError::WouldBlock) if !nonblocking => {}
                Err(error) => return Err(SocketErrno::socket_host(error)),
            }
            let Some(wait) = &self.wait else {
                return Err(Errno::EAGAIN);
            };
            let cancellation = SocketCancellation::new(wait.interruption());
            endpoint
                .description
                .wait_readable(&cancellation)
                .map_err(FileErrno::object)?;
        }
    }
}
