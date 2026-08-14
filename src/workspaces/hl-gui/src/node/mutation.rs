use super::{Node, Patch, Tree, TreeError};
use crate::identity::NodeId;

impl Tree {
    /// Rejects a patch that would leave the tree inconsistent. Runs before the
    /// renderer sees anything, so validation and application never disagree.
    pub(super) fn validate(&self, patch: &Patch) -> Result<(), TreeError> {
        match patch {
            Patch::Create { id, .. } => self.validate_create(*id),
            Patch::Insert { parent, child, before } => self.validate_insert(*parent, *child, *before),
            Patch::Move { parent, child, before } => self.validate_move(*parent, *child, *before),
            Patch::SetProp { id, .. }
            | Patch::ClearProp { id, .. }
            | Patch::SetHandler { id, .. }
            | Patch::ClearHandler { id, .. } => self.require(*id).map(|_| ()),
            Patch::Remove { id } => self.validate_remove(*id),
        }
    }

    /// Applies an already validated patch. Infallible by construction.
    pub(super) fn commit(&mut self, patch: &Patch) {
        match patch {
            Patch::Create { id, tag } => {
                self.nodes.insert(*id, Node::new(*id, *tag));
            }
            Patch::Insert { parent, child, before } => self.attach(*parent, *child, *before),
            Patch::Move { parent, child, before } => {
                self.detach(*child);
                self.attach(*parent, *child, *before);
            }
            Patch::SetProp { id, prop, value } => {
                self.entry(*id).props.insert(*prop, value.clone());
            }
            Patch::ClearProp { id, prop } => {
                self.entry(*id).props.remove(prop);
            }
            Patch::SetHandler { id, handler } => {
                self.entry(*id).handlers.insert(handler.trigger, handler.id.clone());
            }
            Patch::ClearHandler { id, trigger } => {
                self.entry(*id).handlers.remove(trigger);
            }
            Patch::Remove { id } => {
                self.detach(*id);
                self.discard(*id);
            }
        }
    }

    fn require(&self, id: NodeId) -> Result<&Node, TreeError> {
        self.nodes.get(&id).ok_or(TreeError::UnknownNode(id))
    }

    fn entry(&mut self, id: NodeId) -> &mut Node {
        self.nodes.get_mut(&id).expect("validated patch names a live node")
    }

    fn validate_create(&self, id: NodeId) -> Result<(), TreeError> {
        if self.nodes.contains_key(&id) {
            return Err(TreeError::DuplicateNode(id));
        }
        Ok(())
    }

    fn validate_insert(&self, parent: NodeId, child: NodeId, before: Option<NodeId>) -> Result<(), TreeError> {
        if self.parents.contains_key(&child) {
            return Err(TreeError::AlreadyAttached(child));
        }
        self.validate_placement(parent, child, before)
    }

    fn validate_move(&self, parent: NodeId, child: NodeId, before: Option<NodeId>) -> Result<(), TreeError> {
        if !self.parents.contains_key(&child) {
            return Err(TreeError::NotAttached(child));
        }
        self.validate_placement(parent, child, before)
    }

    fn validate_placement(&self, parent: NodeId, child: NodeId, before: Option<NodeId>) -> Result<(), TreeError> {
        let host = self.require(parent)?;
        let node = self.require(child)?;
        if !host.tag.accepts_children() {
            return Err(TreeError::LeafParent { parent, tag: host.tag });
        }
        if node.tag.is_detached() && !parent.is_root() {
            return Err(TreeError::DetachedChild { child, tag: node.tag });
        }
        if let Some(sibling) = before {
            if !host.children.contains(&sibling) {
                return Err(TreeError::SiblingMissing {
                    parent,
                    before: sibling,
                });
            }
        }
        self.reject_cycle(parent, child)
    }

    fn reject_cycle(&self, parent: NodeId, child: NodeId) -> Result<(), TreeError> {
        let mut walk = Some(parent);
        while let Some(current) = walk {
            if current == child {
                return Err(TreeError::Cycle { parent, child });
            }
            walk = self.parents.get(&current).copied();
        }
        Ok(())
    }

    fn validate_remove(&self, id: NodeId) -> Result<(), TreeError> {
        if id.is_root() {
            return Err(TreeError::RemoveRoot);
        }
        self.require(id).map(|_| ())
    }

    fn attach(&mut self, parent: NodeId, child: NodeId, before: Option<NodeId>) {
        let host = self.entry(parent);
        let at = before
            .and_then(|sibling| host.children.iter().position(|entry| *entry == sibling))
            .unwrap_or(host.children.len());
        host.children.insert(at, child);
        self.parents.insert(child, parent);
    }

    fn detach(&mut self, child: NodeId) {
        let Some(parent) = self.parents.remove(&child) else {
            return;
        };
        if let Some(host) = self.nodes.get_mut(&parent) {
            host.children.retain(|entry| *entry != child);
        }
    }

    /// Drops a node and everything beneath it, so identifiers of removed
    /// descendants stop resolving and a late patch naming one is rejected.
    fn discard(&mut self, id: NodeId) {
        let mut pending = vec![id];
        while let Some(current) = pending.pop() {
            let Some(node) = self.nodes.remove(&current) else {
                continue;
            };
            self.parents.remove(&current);
            pending.extend(node.children);
        }
    }
}
