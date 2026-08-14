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
mod data;
mod dialog;
mod identity;
mod node;
mod render;
mod size;
mod style;

pub use builder::Surface;
pub use data::{Cell, Column, DataOp, RequestId, Row, RowRange, RowRequest, RowWindow, Sort, SourceId, Version};
pub use dialog::{Action, Dialog, Role};
pub use identity::{Identities, NodeId};
pub use node::{
    Choice, EventId, Fault, Frame, Handler, Node, Orientation, Patch, Prop, PropValue, Tag, Tree, TreeError, Trigger,
};
pub use render::{Event, Events, Renderer};
pub use size::ByteSize;
pub use style::{Align, Density, Length, Rgb, Scale, Theme, Token, Tone, Variant};
