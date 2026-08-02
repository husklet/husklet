//! The values + invariants the runtime owns: per-connection [`session::Session`] state, the id→native
//! [`resources::SessionResources`] + residency accounting state, and the [`timeline::FenceTimeline`].
//! Pure data — the workflows that mutate it live in [`super::service`].

pub mod resources;
pub mod session;
pub mod sharing;
pub mod timeline;
