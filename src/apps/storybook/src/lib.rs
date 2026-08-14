//! The component catalogue, described purely with `hl-gui`.
//!
//! Nothing here touches a toolkit. The catalogue is a producer like any
//! extension would be, which is what makes it a real test of the library
//! rather than a hand-built demo window.

mod live;
pub(crate) mod story;

pub use live::{catalogue, host, Fault};
pub use story::{Catalogue, Story};

/// How many rows the catalogue's tables claim to have.
pub const ROWS: u64 = story::ROWS;

/// Every windowed source the catalogue draws from.
#[must_use]
pub fn sources() -> Vec<hl_gui::SourceId> {
    vec![story::SOURCE, story::database::SOURCE]
}

/// Answers one window request, the way an out-of-process producer would.
#[must_use]
pub fn answer(request: &hl_gui::RowRequest) -> hl_gui::RowWindow {
    story::answer(request)
}
