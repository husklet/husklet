use super::{CaptureFailure, CapturePhase, REQUEST_BYTES, Request, Server};
use std::{
    io::Read,
    os::{fd::AsRawFd, unix::net::UnixStream},
    sync::{Arc, atomic::Ordering},
};

impl Server {
    pub(crate) fn stop(&self) {
        self.running.store(false, Ordering::Release);
        if let Ok(mut channels) = self.channels.lock() {
            for (_, channel) in channels.drain() {
                let _ = channel.shutdown(std::net::Shutdown::Both);
            }
        }
    }

    pub(crate) fn start(server: &Arc<Self>, broker: hl_native::CheckpointBroker) -> std::thread::JoinHandle<()> {
        let server = Arc::clone(server);
        std::thread::Builder::new()
            .name("hl-checkpoint-broker".into())
            .spawn(move || {
                let mut workers = Vec::new();
                while server.running.load(Ordering::Acquire) {
                    let Some((channel, peer)) = broker.accept(std::time::Duration::from_millis(50)) else {
                        continue;
                    };
                    let accepted = AcceptedChannel::new(Arc::clone(&server));
                    let worker = Arc::clone(&server);
                    match std::thread::Builder::new()
                        .name("hl-checkpoint-channel".into())
                        .spawn(move || worker.serve_accepted(channel, Some(peer), accepted))
                    {
                        Ok(worker) => workers.push(worker),
                        Err(error) => server.fail(format!("checkpoint channel worker construction failed: {error}")),
                    }
                }
                for worker in workers {
                    let _ = worker.join();
                }
            })
            .expect("checkpoint broker thread construction")
    }

    pub(super) fn fail(&self, message: String) {
        hl_log::hl_error!(hl_log::tag::CHECKPOINT, "{message}");
        let phase = self.capture_lock().ok().map(|capture| capture.phase);
        match phase {
            Some(CapturePhase::Active { id, .. }) => {
                if self.finish_failed(id, CaptureFailure::Failed).is_err() {
                    self.interrupt_channels();
                }
            }
            Some(CapturePhase::Recovery { id, .. }) => {
                if self.fail_recovery(id, CaptureFailure::Failed).is_err() {
                    self.interrupt_channels();
                }
            }
            _ => {}
        }
    }

    #[cfg(test)]
    pub(super) fn serve(self: &Arc<Self>, channel: UnixStream, id: u64) {
        let accepted = AcceptedChannel::new(Arc::clone(self));
        self.serve_accepted_for_test(channel, id, accepted);
    }

    #[cfg(test)]
    pub(super) fn serve_authenticated_for_test(
        self: &Arc<Self>,
        channel: UnixStream,
        peer: hl_native::AuthenticatedCheckpointPeer,
    ) {
        let accepted = AcceptedChannel::new(Arc::clone(self));
        self.serve_accepted(channel, Some(peer), accepted);
    }

    #[cfg(test)]
    fn serve_accepted_for_test(self: &Arc<Self>, channel: UnixStream, id: u64, accepted: AcceptedChannel) {
        self.serve_channel(channel, id, None, accepted);
    }

    fn serve_accepted(
        self: &Arc<Self>,
        channel: UnixStream,
        peer: Option<hl_native::AuthenticatedCheckpointPeer>,
        accepted: AcceptedChannel,
    ) {
        let id = peer.as_ref().expect("production peer authority").host_pid;
        self.serve_channel(channel, id, peer, accepted);
    }

    fn serve_channel(
        self: &Arc<Self>,
        channel: UnixStream,
        id: u64,
        peer: Option<hl_native::AuthenticatedCheckpointPeer>,
        accepted: AcceptedChannel,
    ) {
        let channel = Arc::new(channel);
        let descriptor = channel.as_raw_fd();
        let _connection = Connection {
            server: self,
            descriptor,
            id,
            _peer: peer,
            _accepted: accepted,
        };
        let Ok(mut channels) = self.channels.lock() else {
            return;
        };
        channels.insert(descriptor, Arc::clone(&channel));
        drop(channels);
        let mut channel = channel.as_ref();
        if let Ok(capture) = self.capture_lock()
            && let CapturePhase::Recovery { id: recovery, .. } = capture.phase
            && let Ok(mut connections) = self.recovery_connections.lock()
        {
            connections.insert(id, recovery);
        }
        if !self.running.load(Ordering::Acquire) {
            return;
        }
        loop {
            let mut header = [0_u8; REQUEST_BYTES];
            if read_authenticated(&mut channel, _connection._peer.as_ref(), &mut header).is_err() {
                return;
            }
            let Some(request) = Request::decode(&header) else {
                self.fail("checkpoint channel framing is invalid".into());
                return;
            };
            let mut encoded_name = vec![0; request.name_size];
            if read_authenticated(&mut channel, _connection._peer.as_ref(), &mut encoded_name).is_err() {
                return;
            }
            let name = match encoded_name.split_last() {
                Some((0, bytes)) => match std::str::from_utf8(bytes) {
                    Ok(name) => name.to_owned(),
                    Err(_) => return,
                },
                None if request.name_size == 0 => String::new(),
                _ => return,
            };
            let mut payload = Vec::new();
            if request.carries_payload() {
                payload.resize(request.length as usize, 0);
                if read_authenticated(&mut channel, _connection._peer.as_ref(), &mut payload).is_err() {
                    return;
                }
            }
            if _connection
                ._peer
                .as_ref()
                .is_some_and(|authority| !matches!(authority.is_live(), Ok(true)))
            {
                return;
            }
            let reply = self.dispatch(id, &request, &name, &payload);
            if reply.write(&mut channel).is_err() {
                return;
            }
        }
    }
}

fn read_authenticated(
    channel: &mut &UnixStream,
    peer: Option<&hl_native::AuthenticatedCheckpointPeer>,
    output: &mut [u8],
) -> std::io::Result<()> {
    match peer {
        Some(authority) => authority.read_exact(channel, output),
        None => channel.read_exact(output),
    }
}

struct Connection<'a> {
    server: &'a Server,
    descriptor: i32,
    id: u64,
    _peer: Option<hl_native::AuthenticatedCheckpointPeer>,
    _accepted: AcceptedChannel,
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

pub(super) struct AcceptedChannel {
    server: Arc<Server>,
}

impl AcceptedChannel {
    pub(super) fn new(server: Arc<Server>) -> Self {
        server.connections.fetch_add(1, Ordering::AcqRel);
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
