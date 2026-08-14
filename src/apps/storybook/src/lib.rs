//! The component catalogue, described purely with `hl-gui`.
//!
//! Nothing here touches a toolkit. The catalogue is a producer like any
//! extension would be, which is what makes it a real test of the library
//! rather than a hand-built demo window.

mod story;

pub use story::{Catalogue, Story};

/// How many rows the catalogue's table claims to have.
pub const ROWS: u64 = story::ROWS;

/// Answers one window request, the way an out-of-process producer would.
#[must_use]
pub fn answer(request: &hl_gui::RowRequest) -> hl_gui::RowWindow {
    story::answer(request)
}
