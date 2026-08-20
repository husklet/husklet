//! One addressable member of a restored process tree, as the product sees it.
//!
//! A whole-image restore yields one engine handle for a tree of many processes. This is the handle for
//! one of them, named by the guest pid its image was captured under. It can be asked whether it is
//! still the same live process, signalled, and read for the exit it reported.
//!
//! It cannot be started. That is the point: a member either IS the restored process or it is nothing,
//! and a caller that cannot find one must say so rather than run the command again.

use std::{num::NonZeroI32, sync::Arc};

pub use super::checkpoint::members::MemberExit;

/// A live handle on one process a restore re-forked.
#[derive(Clone, Debug)]
pub struct RestoredMember(Arc<super::checkpoint::members::RestoredMember>);

impl RestoredMember {
    pub(crate) const fn new(member: Arc<super::checkpoint::members::RestoredMember>) -> Self {
        Self(member)
    }

    /// The guest pid this member was captured and restored under.
    #[must_use]
    pub fn guest_pid(&self) -> NonZeroI32 {
        self.0.guest_pid()
    }

    /// Whether this exact process incarnation is still running. Never true of a later process that
    /// inherited its pid: the handle names the incarnation, not the number.
    #[must_use]
    pub fn is_live(&self) -> bool {
        self.0.is_live()
    }

    /// The exit this member reported for itself, or [`MemberExit::Unreported`] once it is gone without
    /// one. `None` while it is still running.
    #[must_use]
    pub fn exit(&self) -> Option<MemberExit> {
        self.0.exit()
    }

    /// Delivers one signal to this member alone, never to its container's tree.
    ///
    /// # Errors
    /// Returns `Err(())` once the member has exited, and when the host refuses delivery.
    pub fn signal(&self, signal: i32) -> Result<(), ()> {
        self.0.signal(signal)
    }
}
