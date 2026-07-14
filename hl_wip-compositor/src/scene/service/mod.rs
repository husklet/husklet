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
pub mod popup;
pub mod schedule;

pub use commit::{commit_surface, BufferChange, Commit};
pub use compose::{compose_frame, is_tree_dirty, Frame, PresentItem};
pub use focus::{activate, clear_focus, focus_surface, on_window_gone, surface_at, update_pointer, FocusChange};
pub use popup::{constrain_popup, place_popup, place_popup_in, popup_placement};
pub use schedule::{fallback_timing, from_outcome, should_present, FramePacing, PacingPolicy};
