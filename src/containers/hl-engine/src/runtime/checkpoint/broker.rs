use super::{
    CLAIM, COMMIT, CaptureFailure, CapturePhase, GROUP_BEGIN, GROUP_COMMIT, OBJECT_BEGIN, OBJECT_FINISH, OBJECT_TELL,
    OBJECT_WRITE, OBJECT_WRITE_AT, REGISTER_READY, RELEASE_EXIT, RELEASE_HOLD, RELEASE_RESUME, RELEASE_WAIT,
    REQUEST_BYTES, Reply, Request, Server,
    participants::{ExecutorId, ProcessIdentity},
};
use std::{
    collections::BTreeSet,
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
        let mut connection = Connection {
            server: self,
            descriptor,
            id,
            peer,
            registered: None,
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
            if read_authenticated(&mut channel, connection.peer.as_ref(), &mut header).is_err() {
                return;
            }
            let Some(request) = Request::decode(&header) else {
                self.fail("checkpoint channel framing is invalid".into());
                return;
            };
            let mut encoded_name = vec![0; request.name_size];
            if read_authenticated(&mut channel, connection.peer.as_ref(), &mut encoded_name).is_err() {
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
                if read_authenticated(&mut channel, connection.peer.as_ref(), &mut payload).is_err() {
                    return;
                }
            }
            if connection
                .peer
                .as_ref()
                .is_some_and(|authority| !matches!(authority.is_live(), Ok(true)))
            {
                return;
            }
            if request.op == RELEASE_WAIT {
                // Answered ahead of the membership and scope checks, and without
                // taking a mutation ticket: the sender is a member that is already
                // stopped with its whole thread registry held, and a park that
                // counted as a capture mutation would hold `publish_manifest`
                // behind the very processes it is waiting to release.
                let reply = Reply::value(self.release_disposition(request.generation));
                if reply.write(&mut channel).is_err() {
                    return;
                }
                continue;
            }
            if request.op == REGISTER_READY {
                let member = self.register_ready(&mut connection, &request, &encoded_name, &payload);
                let reply = member.map_or_else(|()| Reply::error(), Reply::value);
                let _ = reply.write(&mut channel);
                if member.is_err() {
                    return;
                }
                continue;
            }
            if let Some(active) = self.membership_scope()
                && publishes_capture_bytes(request.op)
                && connection.registered != Some(active)
            {
                // Naming the exact process is the whole point: an unregistered
                // publisher is otherwise indistinguishable from a stalled one,
                // and the capture would end at its deadline instead.
                self.fail(format!(
                    "checkpoint capture {active}: process {} published op {} without REGISTER_READY",
                    connection.id, request.op
                ));
                let _ = Reply::error().write(&mut channel);
                return;
            }
            let reply = self.dispatch(id, &request, &name, &payload);
            if reply.write(&mut channel).is_err() {
                return;
            }
        }
    }
}

/// Operations that publish or complete capture state. A process that has not
/// proven exact membership may still rendezvous (`GROUP_PRESENT`/`GROUP_COUNT`)
/// and may still abort, because the coordinator does both before its own dump.
fn publishes_capture_bytes(op: u32) -> bool {
    matches!(
        op,
        OBJECT_BEGIN
            | OBJECT_WRITE
            | OBJECT_WRITE_AT
            | OBJECT_TELL
            | OBJECT_FINISH
            | GROUP_BEGIN
            | GROUP_COMMIT
            | CLAIM
            | COMMIT
    )
}

impl Server {
    /// What a member parked inside its own freeze must do next.
    ///
    /// Release is tied to broker-observed capture state, never to a timer in the
    /// member: a coordinator that dies drops its channels and the parked member
    /// reads the transport failure as `RESUME`, so no crash can leave the tree
    /// frozen. `HOLD` is returned only while this member's own capture is still
    /// running and still inside its deadline.
    pub(super) fn release_disposition(&self, generation: u32) -> u64 {
        let Ok(capture) = self.capture_lock() else {
            return RELEASE_RESUME;
        };
        let generation = u64::from(generation);
        match capture.phase {
            CapturePhase::Active { id, deadline } if id == generation => {
                if std::time::Instant::now() < deadline {
                    RELEASE_HOLD
                } else {
                    RELEASE_RESUME
                }
            }
            CapturePhase::Publishing { id } if id == generation => RELEASE_HOLD,
            CapturePhase::Finished {
                id,
                result: Ok(()),
            } if id == generation => RELEASE_EXIT,
            CapturePhase::Complete if self.committed.load(Ordering::Acquire) => RELEASE_EXIT,
            _ => RELEASE_RESUME,
        }
    }

    /// The generation whose membership is currently sealed by a ledger.
    fn membership_scope(&self) -> Option<u64> {
        let capture = self.capture_lock().ok()?;
        match capture.phase {
            CapturePhase::Active { id, .. } => Some(id),
            _ => None,
        }
    }

    /// Installs this connection's process as an exact member of the running
    /// capture. The payload is the process's complete executor inventory,
    /// collected while every one of its threads is stopped.
    fn register_ready(
        &self,
        connection: &mut Connection<'_>,
        request: &Request,
        encoded_name: &[u8],
        payload: &[u8],
    ) -> Result<u64, ()> {
        match self.register_ready_reason(connection, request, encoded_name, payload) {
            Ok(member) => Ok(member),
            Err(reason) => {
                hl_log::hl_error!(
                    hl_log::tag::CHECKPOINT,
                    "checkpoint REGISTER_READY refused for process {}: {reason} (generation {})",
                    connection.id,
                    request.generation
                );
                Err(())
            }
        }
    }

    fn register_ready_reason(
        &self,
        connection: &mut Connection<'_>,
        request: &Request,
        encoded_name: &[u8],
        payload: &[u8],
    ) -> Result<u64, &'static str> {
        if !encoded_name.is_empty() || payload.len() < 8 {
            return Err("malformed registration frame");
        }
        let count = usize::try_from(u32::from_ne_bytes(
            payload[0..4].try_into().map_err(|_| "short inventory count")?,
        ))
        .map_err(|_| "inventory count out of range")?;
        if payload[4..8] != [0; 4] || count == 0 || count > EXECUTOR_INVENTORY_MAX || payload.len() != 8 + count * 4 {
            return Err("inventory count does not describe the payload");
        }
        let mut executors = BTreeSet::new();
        for bytes in payload[8..].chunks_exact(4) {
            let executor = u32::from_ne_bytes(bytes.try_into().map_err(|_| "short executor")?);
            if !executors.insert(ExecutorId(u64::from(executor))) {
                return Err("duplicate executor in the inventory");
            }
        }
        // Only the broker's authenticated capability may name a member; a
        // channel with no proven process identity is not a participant.
        let peer = connection.peer.as_ref().ok_or("channel has no authenticated peer")?;
        let identity = ProcessIdentity {
            pid: peer.host_pid,
            birth: peer.host_birth,
            generation: peer.host_generation,
        };
        let generation = u64::from(request.generation);
        if self.membership_scope() != Some(generation) {
            return Err("no capture is sealing membership at this generation");
        }
        let mut participants = self.participants.lock().map_err(|_| "participant ledger is poisoned")?;
        let ledger = participants.as_mut().ok_or("no participant ledger")?;
        let member = ledger
            .register(generation, identity, &executors)
            .map_err(|error| match error {
                super::participants::Error::Duplicate => "this process identity is already a member",
                super::participants::Error::Conflict => "registration conflicts with the sealed capture",
            })?;
        connection.registered = Some(generation);
        Ok(member.0)
    }
}

/// Bounds the guest-derived inventory before it is allocated against.
const EXECUTOR_INVENTORY_MAX: usize = 4096;

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
    peer: Option<hl_native::AuthenticatedCheckpointPeer>,
    /// The capture generation this connection proved membership of, if any.
    registered: Option<u64>,
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
