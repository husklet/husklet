//! The boundary traits the protocol talks through. Currently the [`sink::CommandSink`] port — the
//! contract every driver submits its command batches through — plus a recording test double.

pub mod sink;
