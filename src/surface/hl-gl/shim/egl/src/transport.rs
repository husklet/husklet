//! Ordered ownership of the display's eagerly connected GPU transport.

mod actor;
mod init;

pub(crate) use actor::Plan;
#[cfg(test)]
pub(crate) use actor::Sequencer;
pub(crate) use init::{DisplayTransport, Ready};
