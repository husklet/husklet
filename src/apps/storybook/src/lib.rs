//! The component catalogue, described purely with `hl-gui`.
//!
//! Nothing here touches a toolkit. The catalogue is a producer like any
//! extension would be, which is what makes it a real test of the library
//! rather than a hand-built demo window.

mod story;

pub use story::{Catalogue, Story};

/// The sample rows the catalogue's table is populated with.
#[must_use]
pub fn rows() -> hl_gui::RowWindow {
    story::window()
}
