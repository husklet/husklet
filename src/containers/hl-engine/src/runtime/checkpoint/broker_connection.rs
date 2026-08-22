use super::Server;
use std::{
    io::Read,
    os::unix::net::UnixStream,
    sync::{Arc, atomic::Ordering},
};

pub(super) fn read_authenticated(
    channel: &mut &UnixStream,
    peer: Option<&hl_native::AuthenticatedCheckpointPeer>,
    output: &mut [u8],
) -> std::io::Result<()> {
    match peer {
        Some(authority) => authority.read_exact(channel, output),
        None => channel.read_exact(output),
    }
}

pub(super) struct Connection<'a> {
    pub(super) server: &'a Server,
    pub(super) descriptor: i32,
    pub(super) id: u64,
    pub(super) peer: Option<hl_native::AuthenticatedCheckpointPeer>,
    /// The capture generation this connection proved membership of, if any.
    pub(super) registered: Option<u64>,
    pub(super) _accepted: AcceptedChannel,
}

impl Drop for Connection<'_> {
    fn drop(&mut self) {
        if let Ok(mut channels) = self.server.channels.lock() {
            channels.remove(&self.descriptor);
        }
        if let Ok(mut connections) = self.server.recovery_connections.lock() {
            connections.remove(&self.id);
        }
    }
}

pub(crate) struct AcceptedChannel {
    server: Arc<Server>,
}

impl AcceptedChannel {
    pub(crate) fn new(server: Arc<Server>) -> Self {
        server.connections.fetch_add(1, Ordering::AcqRel);
        #[cfg(test)]
        server.accepts.fetch_add(1, Ordering::AcqRel);
        Self { server }
    }
}

impl Drop for AcceptedChannel {
    fn drop(&mut self) {
        let previous = self.server.connections.fetch_sub(1, Ordering::AcqRel);
        debug_assert_ne!(previous, 0, "checkpoint connection count underflow");
        if previous == 1 && self.server.running.load(Ordering::Acquire) {
            self.server
                .fail("every native checkpoint channel closed before capture completion".into());
        }
    }
}
