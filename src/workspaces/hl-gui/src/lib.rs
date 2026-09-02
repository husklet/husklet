//! A portable, toolkit-neutral component library.
//!
//! The library owns three things and nothing else: a retained node tree with
//! incremental mutation, a closed styling vocabulary, and windowed data sources
//! for collection components. It has no dependencies, no toolkit imports, and
//! no knowledge of the application embedding it, so an interface described here
//! can be rendered by any adapter and driven over any transport.
//!
//! A producer emits [`Frame`]s of [`Patch`]es; [`Tree`] validates and retains
//! them and forwards them to a [`Renderer`]. Interaction returns as [`Event`]s.
//!
//! ```
//! use hl_gui::{Prop, PropValue, Surface, Tag};
//!
//! let mut surface = Surface::new();
//! let button = surface.create(Tag::Button);
//! surface.set(button, Prop::Label, PropValue::text("Restart"));
//! surface.append(hl_gui::NodeId::ROOT, button);
//! let frame = surface.frame();
//! assert_eq!(frame.sequence, 1);
//! ```

mod builder;
mod component;
mod data;
mod dialog;
mod element;
mod identity;
mod node;
mod render;
mod size;
mod style;

pub use builder::Surface;
pub use component::{HexSource, HexView};
pub use data::{
    Cell, Column, Lookup, RequestId, Row, RowCache, RowRange, RowRequest, RowWindow, Sort, SourceId, SourceMutation,
    Version,
};
pub use dialog::{Action, Dialog, Role};
pub use element::{Element, Reconciliation};
pub use identity::{Identities, NodeId};
pub use node::{
    Choice, EventId, Fault, Frame, Handler, Node, Orientation, Patch, Prop, PropValue, Tag, Tree, TreeError, Trigger,
};
pub use render::{Event, Events, PointerPhase, Renderer};
pub use size::ByteSize;
pub use style::{Align, Bounds, Density, Edges, Length, Rgb, Scale, Theme, Token, Tone, Variant};

/// Maximum Unicode characters a [`Tag::LogView`] retains.
///
/// A log's `Value` patches are append-only deltas. Renderers discard the oldest
/// characters beyond this bound so a long-running operational surface cannot
/// grow host memory without limit.
pub const LOG_VIEW_CHARACTER_LIMIT: i32 = 4_096;

/// Maximum number of source bytes a [`Tag::HexView`] renders.
pub const HEX_VIEW_BYTE_LIMIT: usize = 4_096;
