use super::prop::{Handler, Prop, PropValue, Trigger};
use super::tag::Tag;
use crate::identity::NodeId;

/// One structural mutation. This is the only shape structure takes on the wire;
/// there is no whole-tree replacement.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "wire", derive(serde::Deserialize, serde::Serialize))]
pub enum Patch {
    /// Allocate a detached node. Never attaches it, so a subtree is built
    /// bottom-up and inserted once.
    Create {
        id: NodeId,
        tag: Tag,
    },
    /// Attach a node before `before`, or append when it is absent.
    Insert {
        parent: NodeId,
        child: NodeId,
        before: Option<NodeId>,
    },
    /// Reorder an already attached node within the same parent.
    Move {
        parent: NodeId,
        child: NodeId,
        before: Option<NodeId>,
    },
    SetProp {
        id: NodeId,
        prop: Prop,
        value: PropValue,
    },
    ClearProp {
        id: NodeId,
        prop: Prop,
    },
    SetHandler {
        id: NodeId,
        handler: Handler,
    },
    ClearHandler {
        id: NodeId,
        trigger: Trigger,
    },
    /// Detach and destroy a node together with its whole subtree.
    Remove {
        id: NodeId,
    },
}

impl Patch {
    /// The node this patch acts on, for logging and adapter dispatch.
    #[must_use]
    pub const fn subject(&self) -> NodeId {
        match self {
            Self::Create { id, .. }
            | Self::SetProp { id, .. }
            | Self::ClearProp { id, .. }
            | Self::SetHandler { id, .. }
            | Self::ClearHandler { id, .. }
            | Self::Remove { id } => *id,
            Self::Insert { child, .. } | Self::Move { child, .. } => *child,
        }
    }
}

/// An ordered batch applied atomically between paints. A frame is never
/// partially visible: the adapter commits once, after the last patch.
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "wire", derive(serde::Deserialize, serde::Serialize))]
pub struct Frame {
    pub sequence: u64,
    pub patches: Vec<Patch>,
}

impl Frame {
    #[must_use]
    pub fn new(sequence: u64) -> Self {
        Self {
            sequence,
            patches: Vec::new(),
        }
    }

    #[must_use]
    pub fn with(mut self, patch: Patch) -> Self {
        self.patches.push(patch);
        self
    }

    pub fn push(&mut self, patch: Patch) {
        self.patches.push(patch);
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.patches.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::{Frame, NodeId, Patch, Tag};

    #[test]
    fn insert_reports_the_child_as_its_subject() {
        let patch = Patch::Insert {
            parent: NodeId::ROOT,
            child: NodeId::new(4),
            before: None,
        };
        assert_eq!(patch.subject(), NodeId::new(4));
    }

    #[test]
    fn frames_preserve_patch_order() {
        let frame = Frame::new(1)
            .with(Patch::Create {
                id: NodeId::new(1),
                tag: Tag::Column,
            })
            .with(Patch::Remove { id: NodeId::new(1) });
        assert_eq!(frame.patches.len(), 2);
        assert_eq!(frame.patches[0].subject(), NodeId::new(1));
    }
}
