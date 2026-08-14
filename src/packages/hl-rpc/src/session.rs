//! One connected peer: what it may reach, and what it follows.
//!
//! Only the bookkeeping lives here. What a call means, and which service
//! answers it, belongs to the library that declared the route.

use std::collections::BTreeSet;

use crate::authority::Authority;
use crate::capability::{Capability, CapabilityKey};
use crate::channel::Channels;
use crate::outbox::Outbox;
use crate::subscription::Subscriptions;

/// A stream of host state a peer can follow.
///
/// Declared by the domain that produces the state, like a capability, so this
/// crate routes topics it knows nothing about.
pub trait Topic: Copy + Ord + std::fmt::Debug + 'static {
    /// The capability required to follow this topic. Checked when subscribing
    /// and again on every emission, so a revoked grant stops the stream.
    fn requirement(&self) -> CapabilityKey;
}

/// One connected peer.
#[derive(Debug)]
pub struct Session<T: Topic> {
    authority: Authority,
    topics: BTreeSet<T>,
}

impl<T: Topic> Session<T> {
    /// A session for a peer with the given authority, following nothing.
    #[must_use]
    pub fn new(authority: Authority) -> Self {
        Self {
            authority,
            topics: BTreeSet::new(),
        }
    }

    /// What this peer may reach.
    #[must_use]
    pub const fn authority(&self) -> &Authority {
        &self.authority
    }

    /// What this peer may reach, for revocation.
    #[must_use]
    pub const fn authority_mut(&mut self) -> &mut Authority {
        &mut self.authority
    }

    /// Topics this session currently follows.
    #[must_use]
    pub fn topics(&self) -> Vec<T> {
        self.topics.iter().copied().collect()
    }

    /// Starts following a topic.
    pub fn follow(&mut self, topic: T) {
        self.topics.insert(topic);
    }

    /// Stops following a topic.
    pub fn unfollow(&mut self, topic: T) {
        self.topics.remove(&topic);
    }

    /// Withdraws a capability and drops everything it entitled.
    ///
    /// Re-checking at emission is not enough on its own: a snapshot queued
    /// while the grant was held would still be drained afterwards, so the
    /// subscriber would receive data it is no longer entitled to. Revocation
    /// therefore closes every topic the capability covered, which discards
    /// what those channels were holding.
    pub fn revoke<C: Capability>(
        &mut self,
        capability: C,
        subscriptions: &mut Subscriptions<T>,
        channels: &mut Channels,
        outbox: &mut Outbox<T>,
    ) {
        self.authority.revoke(capability);
        for topic in self.topics() {
            if topic.requirement() != capability.key() {
                continue;
            }
            self.topics.remove(&topic);
            subscriptions.close(topic, channels, outbox);
        }
    }

    /// Whether an emission on `topic` may still be delivered.
    ///
    /// Checked on every emission rather than only at subscribe time, so
    /// revoking a capability stops an established stream.
    #[must_use]
    pub fn may_emit(&self, topic: T) -> bool {
        self.topics.contains(&topic) && self.authority.permit_key(topic.requirement()).is_ok()
    }
}
