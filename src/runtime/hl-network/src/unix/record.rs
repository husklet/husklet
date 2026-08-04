use super::transport::EndpointState;
use crate::{SocketHostError, UnixSocketEndpoint, UnixSocketHost};
use hl_sync::WaitQueue;

impl UnixSocketEndpoint {
    #[must_use]
    pub fn message_wait_queue(&self) -> &WaitQueue {
        self.ancillary[self.token].wait_queue()
    }

    #[must_use]
    pub fn has_message(&self) -> bool {
        self.ancillary[self.token].has_message()
    }

    #[must_use]
    pub fn message_closed(&self) -> bool {
        let state = self.host.state.lock().unwrap_or_else(|error| error.into_inner());
        let endpoint = &state.endpoints[self.token];
        endpoint.read_shutdown || endpoint.peer_write_shutdown
    }

    #[must_use]
    pub fn message_ready(&self) -> bool {
        self.has_message() || self.message_closed()
    }
}

impl EndpointState {
    fn copy_record(&mut self, output: &mut [u8], peek: bool) -> Option<(usize, usize)> {
        let record = self.incoming.front()?;
        let full_length = record.len();
        let count = output.len().min(full_length);
        output[..count].copy_from_slice(&record[..count]);
        if !peek {
            self.incoming.pop_front();
            self.bytes -= full_length;
        }
        Some((count, full_length))
    }
}

impl UnixSocketHost {
    pub(super) fn receive_record(
        &self,
        token: usize,
        output: &mut [u8],
        nonblocking: bool,
        peek: bool,
    ) -> Result<(usize, usize), SocketHostError> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        loop {
            let endpoint = &mut state.endpoints[token];
            if endpoint.canceled {
                return Err(SocketHostError::Canceled);
            }
            if endpoint.read_shutdown {
                return Ok((0, 0));
            }
            if let Some(result) = endpoint.copy_record(output, peek) {
                drop(state);
                self.notify();
                return Ok(result);
            }
            if endpoint.peer_write_shutdown {
                return Ok((0, 0));
            }
            if nonblocking {
                return Err(SocketHostError::WouldBlock);
            }
            state = self.wait(state);
        }
    }
}
