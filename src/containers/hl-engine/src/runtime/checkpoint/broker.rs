use super::{
    CAPTURE_REFUSED, CLAIM, COMMIT, CaptureFailure, CapturePhase, GROUP_BEGIN, GROUP_COMMIT, MEMBER_EXITED,
    MEMBER_RESTORED, MEMBER_STDIO, OBJECT_BEGIN, OBJECT_FINISH, OBJECT_TELL, OBJECT_WRITE, OBJECT_WRITE_AT,
    REGISTER_READY, RELEASE_EXIT, RELEASE_HOLD, RELEASE_RESUME, RELEASE_WAIT, REQUEST_BYTES, Reply, Request,
    SEAL_MEMBERSHIP, Server,
    participants::{ExecutorId, ProcessIdentity},
};
use std::{
    collections::BTreeSet,
    os::{fd::AsRawFd, unix::net::UnixStream},
    sync::{Arc, atomic::Ordering},
};

#[path = "broker_connection.rs"]
mod connection;
use connection::{Connection, read_authenticated};
pub(super) use connection::AcceptedChannel;

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
            .spawn(move || accept_channels(&server, &broker))
            .expect("checkpoint broker thread construction")
    }

    pub(super) fn fail(&self, message: String) {
        self.fail_as(message, CaptureFailure::Failed);
    }

    /// Fail whatever capture or recovery is running, under a named failure.
    ///
    /// Separated from `fail` so a DECIDED refusal is not reported as a breakage: the two reach the
    /// caller as different errors, and only the refusal has a reason to quote.
    pub(super) fn fail_as(&self, message: String, failure: CaptureFailure) {
        hl_log::hl_error!(hl_log::tag::CHECKPOINT, "{message}");
        let phase = self.capture_lock().ok().map(|capture| capture.phase);
        match phase {
            Some(CapturePhase::Active { id, .. }) => {
                if self.finish_failed(id, failure).is_err() {
                    self.interrupt_channels();
                }
            }
            Some(CapturePhase::Recovery { id, .. }) => {
                if self.fail_recovery(id, failure).is_err() {
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
            if request.op == CAPTURE_REFUSED {
                // A DECIDED failure, not a stalled one. The coordinator has already made up its mind and
                // is about to exit; without this the broker learned nothing, the peers parked out their
                // hold and resumed holding their channels open, and the client waited its whole deadline
                // over a decision taken seconds earlier -- then reported an error naming none of it.
                //
                // Answered before the membership and scope checks for the same reason RELEASE_WAIT is:
                // a process that refuses publishes nothing, so requiring it to have proven membership
                // first would drop exactly the refusals taken before registration.
                let reason = if name.is_empty() {
                    "the engine refused the capture without naming a reason".to_owned()
                } else {
                    name.clone()
                };
                self.record_refusal(reason.clone());
                let _ = Reply::ok().write(&mut channel);
                self.fail_as(
                    format!("checkpoint capture refused by the engine: {reason}"),
                    CaptureFailure::Refused,
                );
                return;
            }
            if request.op == MEMBER_RESTORED {
                // Scope-checked like every other publication: only a restore may name a restored
                // member, so an announcement outside a running recovery is refused rather than
                // installing a capability nothing produced.
                let announced = if self.request_in_scope(connection.id, &request, &name) {
                    self.member_restored(&mut connection, &payload)
                } else {
                    hl_log::hl_error!(
                        hl_log::tag::CHECKPOINT,
                        "restored member announcement refused for process {}: no restore is in scope",
                        connection.id
                    );
                    Err(())
                };
                let reply = announced.map_or_else(|()| Reply::error(), |()| Reply::value(1));
                let _ = reply.write(&mut channel);
                continue;
            }
            if request.op == MEMBER_STDIO {
                // The member is asking, from inside its own descriptor restore, for the terminal the host
                // created for it before starting this restore. Scope-checked exactly like the announcement
                // that follows it: only a running restore may claim a member's terminal.
                //
                // A member with no registration is answered OK and no descriptor. That is not a failure --
                // it is every member the host did not seal an interactive session for -- and it leaves the
                // restore with the descriptors it would have had anyway.
                let terminal = self
                    .request_in_scope(connection.id, &request, &name)
                    .then(|| self.member_terminal(&payload))
                    .flatten();
                let header = Reply::ok().header();
                let sent = match terminal {
                    Some(terminal) => super::member_stdio::send_with_descriptor(channel, &header, terminal.as_raw_fd()),
                    None => Reply::ok().write(&mut channel),
                };
                if sent.is_err() {
                    return;
                }
                continue;
            }
            if request.op == MEMBER_EXITED {
                // Answered without a membership or scope check for the same reason RELEASE_WAIT is:
                // the sender proved which member it is when it announced itself, and it is reporting
                // its own exit on its way out of a tree no capture is sealing.
                let reported = self.members.report_exit(connection.id, &payload);
                if let Err(reason) = reported {
                    hl_log::hl_error!(
                        hl_log::tag::CHECKPOINT,
                        "restored member exit report refused for process {}: {reason}",
                        connection.id
                    );
                }
                let reply = if reported.is_ok() { Reply::ok() } else { Reply::error() };
                let _ = reply.write(&mut channel);
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
/// Serves every channel the broker accepts on its own thread, until the server stops running.
///
/// Joins the channel threads before returning, so the broker thread outlives every worker it made.
fn accept_channels(server: &Arc<Server>, broker: &hl_native::CheckpointBroker) {
    let mut workers = Vec::new();
    while server.running.load(Ordering::Acquire) {
        let Some((channel, peer)) = broker.accept(std::time::Duration::from_millis(50)) else {
            continue;
        };
        let accepted = AcceptedChannel::new(Arc::clone(server));
        let worker = Arc::clone(server);
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
}

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
            | SEAL_MEMBERSHIP
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
            CapturePhase::Finished { id, result: Ok(()) } if id == generation => RELEASE_EXIT,
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
            Ok(member) => {
                hl_log::hl_debug!(
                    hl_log::tag::CHECKPOINT,
                    "checkpoint REGISTER_READY admitted process {} as member {} (generation {})",
                    connection.id,
                    member,
                    request.generation
                );
                Ok(member)
            }
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

    /// Installs the connection's process as one addressable member of the restored tree.
    ///
    /// The guest pid comes from the member because only the member knows which captured image it was
    /// re-forked from; everything else comes from the authenticated peer, because a self-reported host
    /// identity would let any connection claim any member.
    fn member_restored(&self, connection: &mut Connection<'_>, payload: &[u8]) -> Result<(), ()> {
        let announced = self.member_restored_reason(connection, payload);
        if let Err(reason) = announced {
            hl_log::hl_error!(
                hl_log::tag::CHECKPOINT,
                "restored member announcement refused for process {}: {reason}",
                connection.id
            );
            return Err(());
        }
        Ok(())
    }

    fn member_restored_reason(&self, connection: &mut Connection<'_>, payload: &[u8]) -> Result<(), &'static str> {
        if payload.len() != 8 || payload[4..8] != [0; 4] {
            return Err("malformed member announcement frame");
        }
        let guest_pid = i32::from_ne_bytes(payload[0..4].try_into().map_err(|_| "short guest pid")?);
        let guest_pid = std::num::NonZeroI32::new(guest_pid).ok_or("member announced no guest pid")?;
        if guest_pid.get() < 0 {
            return Err("member announced a guest pid that is not a process identity");
        }
        let peer = connection.peer.as_ref().ok_or("channel has no authenticated peer")?;
        // A duplicate of the authenticated capability, so the registry outlives the connection that
        // carried it: the member stays reachable after the restore scope closes, which is the whole
        // point of holding it.
        let process = peer
            .process_capability()
            .try_clone()
            .map_err(|_| "cannot retain the authenticated peer capability")?;
        self.members.announce(connection.id, guest_pid, peer.host_pid, process)
    }

    /// The terminal registered for the member this request names, if the host registered one.
    ///
    /// The guest pid is read from the request rather than from the connection because this arrives BEFORE
    /// `MEMBER_RESTORED`: the descriptor restore runs before the identity is hydrated, so the connection is
    /// authenticated but not yet attributed to a member. The registry is what bounds the answer -- a pid the
    /// host never registered a terminal for reads `None`, so a member cannot name its way to another
    /// session's terminal.
    fn member_terminal(&self, payload: &[u8]) -> Option<std::os::fd::OwnedFd> {
        if payload.len() != 8 || payload[4..8] != [0; 4] {
            return None;
        }
        let guest_pid = i32::from_ne_bytes(payload[0..4].try_into().ok()?);
        self.member_terminals.take(std::num::NonZeroI32::new(guest_pid)?)
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
                super::participants::Error::Sealed => {
                    "membership was already sealed: this process was not frozen by the capture"
                }
            })?;
        connection.registered = Some(generation);
        Ok(member.0)
    }
}

/// Bounds the guest-derived inventory before it is allocated against.
const EXECUTOR_INVENTORY_MAX: usize = 4096;
