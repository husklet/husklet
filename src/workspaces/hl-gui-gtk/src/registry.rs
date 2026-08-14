use std::collections::HashMap;

use hl_gui::NodeId;

/// Widget handles owned by the adapter, keyed by node identity.
///
/// The tree never holds a toolkit handle; this is the only place the two sides
/// are associated, which is what keeps the component library portable.
#[derive(Debug, Default)]
pub(crate) struct Registry {
    widgets: HashMap<NodeId, gtk::Widget>,
}

impl Registry {
    pub(crate) fn insert(&mut self, id: NodeId, widget: gtk::Widget) {
        self.widgets.insert(id, widget);
    }

    pub(crate) fn get(&self, id: NodeId) -> Option<&gtk::Widget> {
        self.widgets.get(&id)
    }

    /// Drops a handle. The caller has already unparented it, so the toolkit
    /// releases the widget when this reference goes away.
    pub(crate) fn remove(&mut self, id: NodeId) -> Option<gtk::Widget> {
        self.widgets.remove(&id)
    }

    pub(crate) fn len(&self) -> usize {
        self.widgets.len()
    }
}

/// Why a mutation could not be applied to the widget tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Failure {
    /// The tree validated a node the adapter has no widget for. Indicates the
    /// adapter and the tree disagree, which is always a defect here.
    Unmapped(NodeId),
    /// A container tag was mapped to a widget that cannot hold children.
    NotAContainer(NodeId),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unmapped(id) => write!(formatter, "no widget for node {}", id.raw()),
            Self::NotAContainer(id) => {
                write!(formatter, "node {} cannot hold children", id.raw())
            }
        }
    }
}

impl std::error::Error for Failure {}
