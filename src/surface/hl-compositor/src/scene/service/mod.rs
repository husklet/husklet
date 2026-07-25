//! The compositor's use-cases — one operation per file, each a pure(ish) function over the [`Scene`]
//! model reached through the [`crate::scene::port`] traits.
//!
//! - [`commit`] — apply a surface commit to the scene + mark damage.
//! - [`popup`] — resolve an `xdg_popup` positioner to an on-screen placement.
//! - [`compose`] — walk the tree; compute the ordered layers + damage to present.
//! - [`schedule`] — frame pacing (callbacks/feedback) + the vsync throttle.
//! - [`focus`] — keyboard/pointer focus + window activation.
//!
//! [`Scene`]: crate::scene::model::Scene

pub mod commit;
pub mod compose;
pub mod focus;
pub mod schedule;

pub use commit::{commit_surface, BufferChange, Commit};
pub use compose::{Frame, PresentItem};
pub use focus::{surface_at, update_pointer, FocusChange};
pub use schedule::{should_present, FramePacing, PacingPolicy};
