//! Reach for one member of a restored process tree.
//!
//! A whole-image restore re-forks every captured process out of one launch, so the runtime holds a
//! single handle for a tree of many. The two things a host needs to address one of them individually
//! are a durable NAME and a live CAPABILITY, and each arrives from a different side:
//!
//! * the name is the member's guest pid. `checkpoint/capture.c` writes each captured member's object
//!   group as `proc.<guest pid>` and `ckpt_fork_children` re-forks it under exactly that number, so it
//!   is the one identity that survives the capture. The member announces it with `MEMBER_RESTORED`.
//! * the capability is the authenticated peer of the channel that announcement arrives on. Every
//!   re-forked process creates its own channel (`hl_ckpt_channel_acquire`), and the broker
//!   authenticates each one into a handle on that exact process incarnation -- a pidfd on Linux, a
//!   `NOTE_EXIT` watch on macOS. It is immune to pid reuse, which a remembered pid is not.
//!
//! The registry joins the two. It deliberately outlives the recovery scope that admitted the
//! announcement: recovery ends when the tree is running, which is when a host starts needing to reach
//! into it.

use std::{
    collections::BTreeMap,
    num::NonZeroI32,
    os::fd::{AsFd, AsRawFd, OwnedFd},
    sync::{Arc, Mutex},
};

/// How a restored member's guest process ended, as the member itself reported it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemberExit {
    Code(i32),
    Signal(i32),
    /// The process is gone and reported nothing. A member killed outright never runs its report, so
    /// this is a real outcome and not a gap to paper over with an invented status.
    Unreported,
}

const EXIT_KIND_CODE: u32 = 1;
const EXIT_KIND_SIGNAL: u32 = 2;

/// One addressable member of a restored tree.
pub(crate) struct RestoredMember {
    guest_pid: NonZeroI32,
    host_pid: u64,
    /// Authenticated capability on exactly this process incarnation.
    process: OwnedFd,
    exit: Mutex<Option<MemberExit>>,
}

impl std::fmt::Debug for RestoredMember {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RestoredMember")
            .field("guest_pid", &self.guest_pid)
            .field("host_pid", &self.host_pid)
            .finish_non_exhaustive()
    }
}

impl RestoredMember {
    #[must_use]
    pub(crate) const fn guest_pid(&self) -> NonZeroI32 {
        self.guest_pid
    }

    /// False once this exact incarnation has exited. Never true of a different process that inherited
    /// the pid, because the capability names the incarnation rather than the number.
    #[must_use]
    pub(crate) fn is_live(&self) -> bool {
        hl_native::process_identity_live(self.process.as_fd())
    }

    /// The status this member reported on its way out, or `Unreported` once it is gone without one.
    /// `None` while it is still running.
    #[must_use]
    pub(crate) fn exit(&self) -> Option<MemberExit> {
        if let Ok(exit) = self.exit.lock()
            && let Some(exit) = *exit
        {
            return Some(exit);
        }
        (!self.is_live()).then_some(MemberExit::Unreported)
    }

    /// Delivers one signal to this member alone. Refused, never retargeted, once it has exited.
    pub(crate) fn signal(&self, signal: i32) -> Result<(), ()> {
        hl_native::process_identity_signal(self.process.as_raw_fd(), self.host_pid, signal)
    }

    fn publish_exit(&self, exit: MemberExit) {
        if let Ok(mut slot) = self.exit.lock() {
            slot.get_or_insert(exit);
        }
    }
}

/// Every member a restore has announced, keyed by the guest pid its image names it by.
#[derive(Default)]
pub(crate) struct RestoredMembers {
    /// Keyed on the durable name; the connection index resolves a later report back to the member.
    by_guest_pid: Mutex<BTreeMap<i32, Arc<RestoredMember>>>,
    by_connection: Mutex<BTreeMap<u64, i32>>,
}

impl RestoredMembers {
    /// Installs one announced member. A repeat announcement of the same guest pid replaces the
    /// registration, because a second restore of the same image is a new tree and the old capability
    /// names a process that is gone.
    pub(crate) fn announce(
        &self,
        connection: u64,
        guest_pid: NonZeroI32,
        host_pid: u64,
        process: OwnedFd,
    ) -> Result<(), &'static str> {
        if host_pid == 0 {
            return Err("announcement carries no authenticated host identity");
        }
        let member = Arc::new(RestoredMember {
            guest_pid,
            host_pid,
            process,
            exit: Mutex::new(None),
        });
        let mut members = self.by_guest_pid.lock().map_err(|_| "member registry is poisoned")?;
        let mut connections = self.by_connection.lock().map_err(|_| "member index is poisoned")?;
        members.insert(guest_pid.get(), member);
        connections.insert(connection, guest_pid.get());
        Ok(())
    }

    /// Records the exit one member reported for itself.
    pub(crate) fn report_exit(&self, connection: u64, payload: &[u8]) -> Result<(), &'static str> {
        if payload.len() != 8 {
            return Err("malformed member exit frame");
        }
        let status = i32::from_ne_bytes(payload[0..4].try_into().map_err(|_| "short member status")?);
        let kind = u32::from_ne_bytes(payload[4..8].try_into().map_err(|_| "short member exit kind")?);
        let exit = match kind {
            EXIT_KIND_CODE => MemberExit::Code(status),
            EXIT_KIND_SIGNAL => MemberExit::Signal(status),
            _ => return Err("member exit kind is not a known outcome"),
        };
        let guest_pid = self
            .by_connection
            .lock()
            .map_err(|_| "member index is poisoned")?
            .get(&connection)
            .copied()
            .ok_or("this connection never announced a restored member")?;
        let members = self.by_guest_pid.lock().map_err(|_| "member registry is poisoned")?;
        members
            .get(&guest_pid)
            .ok_or("the announced member is gone")?
            .publish_exit(exit);
        Ok(())
    }

    #[must_use]
    pub(crate) fn get(&self, guest_pid: NonZeroI32) -> Option<Arc<RestoredMember>> {
        self.by_guest_pid.lock().ok()?.get(&guest_pid.get()).map(Arc::clone)
    }

    /// Drops every registration. A fresh capture of this domain invalidates the whole set: the
    /// processes it named are the ones being captured, and the next restore announces new ones.
    pub(crate) fn clear(&self) {
        if let Ok(mut members) = self.by_guest_pid.lock() {
            members.clear();
        }
        if let Ok(mut connections) = self.by_connection.lock() {
            connections.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A capture replaces every process the restore announced, so the registry it hands out
    /// capabilities from must be emptied rather than carried across one.
    #[test]
    fn retiring_the_registry_leaves_no_member_reachable() {
        let members = RestoredMembers::default();
        let (announcer, _peer) = std::os::unix::net::UnixStream::pair().expect("capability stand-in");
        let guest_pid = NonZeroI32::new(77).expect("guest pid");
        members
            .announce(1, guest_pid, 4242, OwnedFd::from(announcer))
            .expect("announcement");
        assert!(members.get(guest_pid).is_some());

        members.clear();

        assert!(
            members.get(guest_pid).is_none(),
            "a retired registry still named a member"
        );
    }

    /// An exit report has to name a member the reporting connection announced. Any other connection
    /// reporting for it would let one process publish another's outcome.
    #[test]
    fn an_exit_report_from_an_unannounced_connection_is_refused() {
        let members = RestoredMembers::default();
        let mut payload = [0_u8; 8];
        payload[4..8].copy_from_slice(&EXIT_KIND_CODE.to_ne_bytes());
        assert_eq!(
            members.report_exit(9, &payload).err(),
            Some("this connection never announced a restored member")
        );
    }
}
