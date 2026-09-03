//! The retained node tree and its mutation semantics.

mod patch;
mod prop;
mod tag;

pub use patch::{Frame, Patch};
pub use prop::{Choice, EventId, Handler, Orientation, Prop, PropValue, Trigger};
pub use tag::Tag;

use std::collections::BTreeMap;

pub use crate::identity::{Identities, NodeId};

/// One retained node. Toolkit-free: an adapter keeps its widget handle beside
/// this by identity, never inside it.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub id: NodeId,
    pub tag: Tag,
    pub props: BTreeMap<Prop, PropValue>,
    pub handlers: BTreeMap<Trigger, EventId>,
    pub children: Vec<NodeId>,
}

impl Node {
    #[must_use]
    pub fn new(id: NodeId, tag: Tag) -> Self {
        Self {
            id,
            tag,
            props: BTreeMap::new(),
            handlers: BTreeMap::new(),
            children: Vec::new(),
        }
    }

    #[must_use]
    pub fn prop(&self, prop: Prop) -> Option<&PropValue> {
        self.props.get(&prop)
    }

    #[must_use]
    pub fn text(&self, prop: Prop) -> Option<&str> {
        self.props.get(&prop).and_then(PropValue::as_text)
    }

    /// Boolean property with a caller-chosen default, so adapters do not repeat
    /// the same unwrap at every call site.
    #[must_use]
    pub fn flag(&self, prop: Prop, fallback: bool) -> bool {
        self.props.get(&prop).and_then(PropValue::as_flag).unwrap_or(fallback)
    }

    #[must_use]
    pub fn handler(&self, trigger: Trigger) -> Option<&EventId> {
        self.handlers.get(&trigger)
    }

    /// Whether this node is currently presented to a user.
    #[must_use]
    pub fn is_visible(&self) -> bool {
        self.flag(Prop::Visible, true)
    }

    /// Whether this node can currently accept user input.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.flag(Prop::Enabled, true)
    }

    /// A declared handler is actionable only while its visible control is enabled.
    #[must_use]
    pub fn action(&self, trigger: Trigger) -> Option<&EventId> {
        (self.is_visible() && self.is_enabled())
            .then(|| self.handler(trigger))
            .flatten()
    }
}

/// Why a patch was rejected. Rejection happens before the adapter sees the
/// patch, so an adapter never has to defend against a malformed tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TreeError {
    UnknownNode(NodeId),
    DuplicateNode(NodeId),
    AlreadyAttached(NodeId),
    NotAttached(NodeId),
    LeafParent { parent: NodeId, tag: Tag },
    DetachedChild { child: NodeId, tag: Tag },
    SiblingMissing { parent: NodeId, before: NodeId },
    Cycle { parent: NodeId, child: NodeId },
    RemoveRoot,
    StaleSequence { expected: u64, received: u64 },
    NodeLimit { limit: usize, received: usize },
    InvalidSchema(&'static str),
}

impl std::fmt::Display for TreeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownNode(id) => write!(formatter, "unknown node {}", id.raw()),
            Self::DuplicateNode(id) => write!(formatter, "node {} already exists", id.raw()),
            Self::AlreadyAttached(id) => write!(formatter, "node {} is already attached", id.raw()),
            Self::NotAttached(id) => write!(formatter, "node {} is not attached", id.raw()),
            Self::LeafParent { parent, tag } => {
                write!(formatter, "node {} is a leaf {}", parent.raw(), tag.as_str())
            }
            Self::DetachedChild { child, tag } => write!(
                formatter,
                "{} node {} must attach to the root",
                tag.as_str(),
                child.raw()
            ),
            Self::SiblingMissing { parent, before } => {
                write!(formatter, "node {} is not a child of {}", before.raw(), parent.raw())
            }
            Self::Cycle { parent, child } => write!(
                formatter,
                "inserting {} into {} would form a cycle",
                child.raw(),
                parent.raw()
            ),
            Self::RemoveRoot => write!(formatter, "the root cannot be removed"),
            Self::StaleSequence { expected, received } => {
                write!(formatter, "expected frame {expected}, received {received}")
            }
            Self::NodeLimit { limit, received } => {
                write!(
                    formatter,
                    "interface tree has {received} nodes, above the limit of {limit}"
                )
            }
            Self::InvalidSchema(reason) => write!(formatter, "invalid table schema: {reason}"),
        }
    }
}

impl std::error::Error for TreeError {}

/// The authoritative retained tree. Validates every patch, then forwards it to
/// a renderer that can assume the mutation is already legal.
#[derive(Clone, Debug)]
pub struct Tree {
    nodes: BTreeMap<NodeId, Node>,
    parents: BTreeMap<NodeId, NodeId>,
    sequence: u64,
}

impl Default for Tree {
    fn default() -> Self {
        Self::new()
    }
}

impl Tree {
    #[must_use]
    pub fn new() -> Self {
        let root = Node::new(NodeId::ROOT, Tag::Column);
        Self {
            nodes: [(NodeId::ROOT, root)].into_iter().collect(),
            parents: BTreeMap::new(),
            sequence: 0,
        }
    }

    #[must_use]
    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(&id)
    }

    #[must_use]
    pub fn root(&self) -> &Node {
        &self.nodes[&NodeId::ROOT]
    }

    #[must_use]
    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.parents.get(&id).copied()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.root().children.is_empty()
    }

    /// The last applied frame sequence; the next frame must be its successor.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Validates a complete frame and its resulting retained size without
    /// mutating this tree or calling a renderer.
    ///
    /// This is the atomic admission pass for resource bounds: removals and
    /// creations are interpreted in their real order on a private snapshot,
    /// so a refused frame leaves the last valid tree and UI untouched.
    pub fn preflight(&self, frame: &Frame, node_limit: usize) -> Result<(), TreeError> {
        let expected = self.sequence.saturating_add(1);
        if frame.sequence != expected {
            return Err(TreeError::StaleSequence {
                expected,
                received: frame.sequence,
            });
        }
        let mut candidate = self.clone();
        for patch in &frame.patches {
            candidate.validate(patch)?;
            candidate.commit(patch);
            if candidate.len() > node_limit {
                return Err(TreeError::NodeLimit {
                    limit: node_limit,
                    received: candidate.len(),
                });
            }
        }
        Ok(())
    }

    /// Resolves the handler identity a triggered node declared, if any.
    #[must_use]
    pub fn handler(&self, id: NodeId, trigger: Trigger) -> Option<&EventId> {
        self.nodes.get(&id).and_then(|node| node.handler(trigger))
    }

    /// Applies one validated patch and forwards it to the renderer.
    ///
    /// The tree is updated first, so the renderer always reads the state the
    /// patch produces: a created node resolves, and sibling order is final.
    ///
    /// # Errors
    /// Returns the validation failure without mutating the tree or calling the
    /// renderer, so a rejected patch leaves both sides consistent.
    pub fn patch<R: crate::Renderer>(&mut self, patch: &Patch, renderer: &mut R) -> Result<(), Fault<R::Error>> {
        self.validate(patch).map_err(Fault::Tree)?;
        let removed = self.retain(patch);
        self.commit(patch);
        let view = removed.as_ref().unwrap_or(self);
        renderer.patch(patch, view).map_err(Fault::Render)
    }

    /// A removal destroys the node the renderer still has to look up, so that
    /// one case renders against a snapshot taken before the mutation.
    fn retain(&self, patch: &Patch) -> Option<Self> {
        let Patch::Remove { id } = patch else {
            return None;
        };
        let node = self.nodes.get(id)?;
        Some(Self {
            nodes: [(node.id, node.clone())].into_iter().collect(),
            parents: BTreeMap::new(),
            sequence: self.sequence,
        })
    }

    /// Applies a whole frame, then asks the renderer to present it once.
    ///
    /// # Errors
    /// Returns the first failure. Patches already applied stay applied; the
    /// producer recovers by rebuilding, which is why frames carry a sequence.
    pub fn apply<R: crate::Renderer>(&mut self, frame: &Frame, renderer: &mut R) -> Result<(), Fault<R::Error>> {
        let expected = self.sequence.saturating_add(1);
        if frame.sequence != expected {
            return Err(Fault::Tree(TreeError::StaleSequence {
                expected,
                received: frame.sequence,
            }));
        }
        for patch in &frame.patches {
            self.patch(patch, renderer)?;
        }
        self.sequence = frame.sequence;
        renderer.commit(frame.sequence).map_err(Fault::Render)
    }
}

/// A patch failure: either the tree rejected it or the adapter could not apply it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Fault<E> {
    Tree(TreeError),
    Render(E),
}

impl<E: std::fmt::Display> std::fmt::Display for Fault<E> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tree(error) => write!(formatter, "{error}"),
            Self::Render(error) => write!(formatter, "{error}"),
        }
    }
}

mod mutation;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_visible_enabled_handlers_are_actions() {
        let mut node = Node::new(NodeId::new(4), Tag::Button);
        node.handlers.insert(Trigger::Invoke, EventId::new("run"));
        assert!(node.action(Trigger::Invoke).is_some());

        node.props.insert(Prop::Enabled, PropValue::Flag(false));
        assert!(node.action(Trigger::Invoke).is_none());
        node.props.insert(Prop::Enabled, PropValue::Flag(true));
        node.props.insert(Prop::Visible, PropValue::Flag(false));
        assert!(node.action(Trigger::Invoke).is_none());
    }
}
