//! The crossing from the extension's thread to the toolkit thread.
//!
//! GTK widgets may only be touched from the main loop, so whatever thread is
//! reading the extension's socket cannot render. It posts here instead, and the
//! page drains on its tick.

use std::sync::mpsc::{sync_channel, Receiver, SyncSender};

use hl_gui::{Frame, SourceMutation};

/// One item posted from the extension's thread to the page.
#[derive(Clone, Debug, PartialEq)]
pub enum Delivery {
    /// A batch of patches for the node tree.
    Frame(Frame),
    /// A source mutation: how long a table is, or a window of its rows.
    Source(SourceMutation),
    /// The extension is gone, and why. The page freezes rather than blanks.
    Loss(String),
    /// A structured crash-loop count for the central lifecycle card.
    Fault { restarts: u32 },
}

/// The end the page drains.
pub type Deliveries = Receiver<Delivery>;

/// The end the extension's thread posts to.
pub type Post = SyncSender<Delivery>;

/// How many deliveries may wait before the posting thread blocks.
///
/// Bounded on purpose: an unbounded queue turns a chatty extension into host
/// memory growth, and the page can never catch up with a producer that is
/// faster than the display anyway. At the page's 100ms tick this is roughly
/// eight ticks of slack, enough to absorb a burst without back-pressuring a
/// well-behaved extension.
pub const CAPACITY: usize = 64;

/// How many deliveries one tick applies before yielding to the main loop.
///
/// Applying the whole queue in one turn would let a chatty extension hold the
/// main loop and starve painting and input for the rest of the application.
/// Eight per 100ms tick is 80 frames a second, above what a display can show,
/// so the cap costs a well-behaved extension nothing and leaves the remainder
/// queued for the next tick rather than dropped.
pub const DRAIN: usize = 8;

/// Creates the posting end and the draining end of the page's queue.
#[must_use]
pub fn channel() -> (Post, Deliveries) {
    sync_channel(CAPACITY)
}
