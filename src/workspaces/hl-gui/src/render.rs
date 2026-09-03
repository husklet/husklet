//! The toolkit port. Implementors live in the embedding application, never here.

use crate::data::{RowRequest, RowWindow, SourceId, Version};
use crate::node::{EventId, NodeId, Patch, Tree};
use crate::style::Theme;

/// Applies validated mutations to retained widgets.
///
/// Deliberately narrow, and deliberately free of any handle-returning method:
/// returning a widget would drag a toolkit type across this boundary and make
/// the library non-portable.
pub trait Renderer {
    type Error;

    /// Apply one mutation. The tree is already updated for reads of context
    /// the adapter needs, such as sibling order or companion properties.
    ///
    /// # Errors
    /// Returns a toolkit failure; the tree treats it as fatal for the frame.
    fn patch(&mut self, patch: &Patch, tree: &Tree) -> Result<(), Self::Error>;

    /// Present everything applied since the previous commit, in one pass.
    ///
    /// # Errors
    /// Returns a toolkit failure raised while presenting.
    fn commit(&mut self, sequence: u64) -> Result<(), Self::Error>;

    /// Deliver rows answering an earlier request.
    ///
    /// # Errors
    /// Returns a toolkit failure raised while binding rows.
    fn rows(&mut self, window: &RowWindow) -> Result<(), Self::Error>;

    /// Replace the active appearance.
    ///
    /// # Errors
    /// Returns a toolkit failure raised while restyling.
    fn theme(&mut self, theme: &Theme) -> Result<(), Self::Error>;
}

/// What a user did, reported back to the producer.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Event {
    Invoke {
        node: NodeId,
        id: EventId,
    },
    Change {
        node: NodeId,
        id: EventId,
        value: crate::node::PropValue,
    },
    Submit {
        node: NodeId,
        id: EventId,
    },
    Select {
        node: NodeId,
        id: EventId,
        rows: Vec<u64>,
        collection: Option<CollectionSelection>,
    },
    Scroll {
        node: NodeId,
        id: EventId,
        dx: f64,
        dy: f64,
    },
    Close {
        node: NodeId,
        id: EventId,
    },
    Context {
        node: NodeId,
        id: EventId,
        x: f64,
        y: f64,
    },
    Key {
        node: NodeId,
        id: EventId,
        key: String,
        keycode: u32,
        modifiers: u32,
        pressed: bool,
    },
    Focus {
        node: NodeId,
        id: EventId,
        focused: bool,
    },
    Pointer {
        node: NodeId,
        id: EventId,
        phase: PointerPhase,
        x: Option<f64>,
        y: Option<f64>,
        button: u32,
        modifiers: u32,
    },
    /// The host needs a window of rows it does not have cached.
    Rows(RowRequest),
}

/// Immutable authority for rows selected from one version of a windowed source.
#[derive(Clone, Debug, PartialEq)]
pub struct CollectionSelection {
    pub source: SourceId,
    pub version: Version,
    pub rows: Vec<SelectedRow>,
}

/// A visible position paired with the producer-owned identity delivered there.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SelectedRow {
    pub index: u64,
    pub id: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PointerPhase {
    Enter,
    Motion,
    Leave,
    Press,
    Release,
}

/// Sink for interaction reported by a renderer.
pub trait Events {
    fn emit(&mut self, event: Event);
}

impl Events for Vec<Event> {
    fn emit(&mut self, event: Event) {
        self.push(event);
    }
}
