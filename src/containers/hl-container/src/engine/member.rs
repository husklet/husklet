//! The live session of one member of a restored process tree.
//!
//! A whole-image restore re-forks every captured process out of a single launch, so the runtime holds one
//! handle for a tree of many and hl-container holds nothing that names an individual session. Two things
//! are needed before a pane can be seated on one of them again: the process itself, which the engine's
//! restored-member registry supplies, and I/O to reach it through, which nothing supplied until this.
//!
//! This is the second half. The terminal has to exist before the container starts -- a restoring member
//! asks for it from inside its own descriptor restore -- so it is created here, at launch, for every
//! sealed record the service named, and parked until a pane arrives.

use crate::{
    Error, ExitStatus, Result, Signal,
    service::{MemberTerminal, Running},
};
use async_trait::async_trait;
use std::sync::{Arc, Mutex as StdMutex};

/// How often a member's exit is looked for. A restored member is not a child of this process, so there
/// is nothing to `wait` on: its liveness is read through the authenticated capability the broker holds,
/// and its status through the report it sends on its way out.
const EXIT_POLL: std::time::Duration = std::time::Duration::from_millis(25);

pub(super) struct MemberSession {
    guest_pid: std::num::NonZeroI32,
    /// The host end of this member's pty, pumping to and from the log/input channels below. Held for the
    /// life of the container process: dropping it stops the pumps and closes the session's I/O.
    terminal: hl_engine::runtime::MemberTerminal,
    logs: StdMutex<Option<crate::service::LogReceiver>>,
}

impl MemberSession {
    /// Creates one member's terminal and registers it with the engine that is about to restore it.
    ///
    /// # Errors
    /// Returns a runtime failure when the pty cannot be created, and when the engine coordinates no
    /// checkpoint and therefore has no restore to register against -- which is a caller mistake, not a
    /// degraded mode, and is reported rather than silently producing an unreachable terminal.
    pub(super) fn open(engine: &hl_engine::runtime::Engine, member: MemberTerminal) -> Result<Self> {
        let MemberTerminal { guest_pid, size, input } = member;
        let (sender, receiver) = crate::service::log_channel();
        let port = Arc::new(super::TerminalChannel::new(input, sender));
        let terminal = hl_engine::composition::Terminal::new(port, size.rows(), size.columns())
            .map_err(|_| Error::Runtime(format!("member {guest_pid} terminal construction failed")))?;
        let (terminal, slave) = hl_engine::runtime::MemberTerminal::open(terminal)
            .map_err(|error| Error::Runtime(format!("member {guest_pid} terminal: {error:?}")))?;
        // By value: the slave descriptor is moved into the engine, so this call cannot be dropped without
        // leaving an unused binding behind.
        engine
            .provide_member_terminal(guest_pid, slave)
            .map_err(|error| Error::Runtime(format!("member {guest_pid} terminal registration: {error:?}")))?;
        Ok(Self {
            guest_pid,
            terminal,
            logs: StdMutex::new(Some(receiver)),
        })
    }

    pub(super) const fn guest_pid(&self) -> std::num::NonZeroI32 {
        self.guest_pid
    }

    /// The member's output stream, taken once by whoever owns the session's journal.
    pub(super) fn take_logs(&self) -> Option<crate::service::LogReceiver> {
        self.logs.lock().ok()?.take()
    }

    /// Resizes this member's terminal alone, never its container's.
    ///
    /// # Errors
    /// Returns a runtime failure when the pty refuses the change.
    pub(super) fn resize(&self, size: crate::Size) -> Result<()> {
        self.terminal
            .resize(size.rows(), size.columns())
            .map_err(|error| Error::Runtime(format!("member {} resize: {error:?}", self.guest_pid)))
    }

    /// Waits for output already produced to reach the journal, as the engine's own bridge does at exit.
    pub(super) fn flush(&self) {
        self.terminal.flush();
    }
}

/// One restored member, owned the way a started process is.
///
/// It is the process the user left running, not a replacement for it. Nothing here can start anything:
/// the member either exists in the restore that revived it or it does not, and a caller that cannot find
/// one must refuse rather than run the command again.
pub(super) struct MemberProcess {
    id: u64,
    domain: hl_engine::Domain,
    session: Arc<MemberSession>,
    member: hl_engine::runtime::RestoredMember,
}

impl MemberProcess {
    pub(super) fn new(
        domain: hl_engine::Domain,
        session: Arc<MemberSession>,
        member: hl_engine::runtime::RestoredMember,
    ) -> Self {
        Self {
            id: super::Process::next_id(),
            domain,
            session,
            member,
        }
    }

    fn status(exit: hl_engine::runtime::MemberExit) -> ExitStatus {
        match exit {
            hl_engine::runtime::MemberExit::Code(code) => ExitStatus::Code(code),
            hl_engine::runtime::MemberExit::Signal(signal) => ExitStatus::Signal(signal),
            // The member was killed outright and never ran its report. Saying so is the honest answer;
            // inventing a status would make a killed session indistinguishable from a clean one.
            hl_engine::runtime::MemberExit::Unreported => ExitStatus::Fault {
                status: -1,
                detail: 0,
                reason: crate::FaultCause::Unknown,
            },
        }
    }
}

#[async_trait]
impl Running for MemberProcess {
    fn id(&self) -> u64 {
        self.id
    }

    fn domain(&self) -> hl_engine::Domain {
        self.domain
    }

    fn guest_pid(&self) -> Option<std::num::NonZeroI32> {
        Some(self.member.guest_pid())
    }

    /// A member has no tree of its own: it IS one member of its container's.
    fn restored_member(&self, _guest_pid: std::num::NonZeroI32) -> Option<hl_engine::runtime::RestoredMember> {
        None
    }

    /// A member is captured as part of its container's freeze and can never be the subject of a capture
    /// of its own, exactly as a started exec session cannot.
    fn checkpointable(&self) -> bool {
        false
    }

    async fn wait(self: Arc<Self>) -> Result<ExitStatus> {
        loop {
            if let Some(exit) = self.member.exit() {
                self.session.flush();
                return Ok(Self::status(exit));
            }
            tokio::time::sleep(EXIT_POLL).await;
        }
    }

    async fn signal(&self, signal: Signal) -> Result<()> {
        // Refused, never retargeted: once the incarnation is gone the capability names nothing, and
        // delivering to the pid anyway could reach whatever inherited it.
        self.member
            .signal(i32::from(signal.get()))
            .map_err(|()| Error::Runtime(format!("restored member {} refused signal", self.member.guest_pid())))
    }

    async fn pause(&self) -> Result<()> {
        self.signal(Signal::new(19).expect("SIGSTOP")).await
    }

    async fn resume(&self) -> Result<()> {
        self.signal(Signal::new(18).expect("SIGCONT")).await
    }

    async fn checkpoint(&self, _timeout: std::time::Duration) -> Result<()> {
        Err(Error::Runtime(format!(
            "restored member {} is captured with its container and holds no image of its own",
            self.member.guest_pid()
        )))
    }

    async fn resize(&self, size: crate::Size) -> Result<()> {
        self.session.resize(size)
    }

    fn take_logs(&self) -> Option<crate::service::LogReceiver> {
        self.session.take_logs()
    }
}
