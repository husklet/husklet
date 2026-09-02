//! Producer-side composition. Accumulates mutations into frames.
//!
//! This is the only writing surface a producer needs, whether it renders in the
//! same process or serializes the resulting frames onto a transport.

use crate::data::SourceId;
use crate::node::{EventId, Frame, Handler, Identities, NodeId, Patch, Prop, PropValue, Tag, Trigger};
use crate::style::{Align, Length, Tone, Variant};

/// Accumulates patches and hands them out as sequenced frames.
#[derive(Debug)]
pub struct Surface {
    identities: Identities,
    pending: Vec<Patch>,
    sequence: u64,
}

impl Default for Surface {
    fn default() -> Self {
        Self::new()
    }
}

impl Surface {
    #[must_use]
    pub fn new() -> Self {
        Self {
            identities: Identities::new(),
            pending: Vec::new(),
            sequence: 0,
        }
    }

    /// Allocates a node and records its creation.
    pub fn create(&mut self, tag: Tag) -> NodeId {
        let id = self.identities.allocate();
        self.pending.push(Patch::Create { id, tag });
        id
    }

    pub fn set(&mut self, id: NodeId, prop: Prop, value: PropValue) {
        self.pending.push(Patch::SetProp { id, prop, value });
    }

    pub fn clear(&mut self, id: NodeId, prop: Prop) {
        self.pending.push(Patch::ClearProp { id, prop });
    }

    pub fn on(&mut self, id: NodeId, trigger: Trigger, event: EventId) {
        self.pending.push(Patch::SetHandler {
            id,
            handler: Handler::new(trigger, event),
        });
    }

    pub fn append(&mut self, parent: NodeId, child: NodeId) {
        self.pending.push(Patch::Insert {
            parent,
            child,
            before: None,
        });
    }

    pub fn insert(&mut self, parent: NodeId, child: NodeId, before: NodeId) {
        self.pending.push(Patch::Insert {
            parent,
            child,
            before: Some(before),
        });
    }

    pub fn reorder(&mut self, parent: NodeId, child: NodeId, before: Option<NodeId>) {
        self.pending.push(Patch::Move { parent, child, before });
    }

    pub fn remove(&mut self, id: NodeId) {
        self.pending.push(Patch::Remove { id });
    }

    /// Takes everything recorded since the previous frame.
    pub fn frame(&mut self) -> Frame {
        self.sequence = self.sequence.saturating_add(1);
        Frame {
            sequence: self.sequence,
            patches: std::mem::take(&mut self.pending),
        }
    }

    #[must_use]
    pub fn is_pending(&self) -> bool {
        !self.pending.is_empty()
    }
}

/// Composition shorthands. Each is one node plus its common properties, so the
/// frequent cases do not spell out four `set` calls.
impl Surface {
    /// A container. `Row` and `Column` differ only in orientation.
    pub fn container(&mut self, tag: Tag, gap: Length) -> NodeId {
        let id = self.create(tag);
        self.set(id, Prop::Gap, PropValue::Length(gap));
        id
    }

    pub fn text(&mut self, value: impl Into<String>) -> NodeId {
        let id = self.create(Tag::Text);
        self.set(id, Prop::Label, PropValue::text(value));
        id
    }

    pub fn heading(&mut self, value: impl Into<String>) -> NodeId {
        let id = self.create(Tag::Heading);
        self.set(id, Prop::Label, PropValue::text(value));
        id
    }

    pub fn button(&mut self, label: impl Into<String>, event: EventId) -> NodeId {
        let id = self.create(Tag::Button);
        self.set(id, Prop::Label, PropValue::text(label));
        self.on(id, Trigger::Invoke, event);
        id
    }

    pub fn badge(&mut self, label: impl Into<String>, tone: Tone) -> NodeId {
        let id = self.create(Tag::Badge);
        self.set(id, Prop::Label, PropValue::text(label));
        self.set(id, Prop::Tone, PropValue::Tone(tone));
        id
    }

    pub fn entry(&mut self, value: impl Into<String>, event: EventId) -> NodeId {
        let id = self.create(Tag::Entry);
        self.set(id, Prop::Value, PropValue::text(value));
        self.on(id, Trigger::Change, event);
        id
    }

    /// A table bound to a windowed source; rows arrive separately.
    pub fn table(&mut self, source: SourceId) -> NodeId {
        let id = self.create(Tag::DataTable);
        self.set(id, Prop::Source, PropValue::Source(source));
        id
    }

    /// A chronological event history bound to a windowed source.
    pub fn event_stream(&mut self, source: SourceId) -> NodeId {
        let id = self.create(Tag::EventStream);
        self.set(id, Prop::Source, PropValue::Source(source));
        id
    }

    /// Marks a node as emphasized and toned in one call.
    pub fn style(&mut self, id: NodeId, variant: Variant, tone: Tone) {
        self.set(id, Prop::Variant, PropValue::Variant(variant));
        self.set(id, Prop::Tone, PropValue::Tone(tone));
    }

    /// Marks a node to consume the leftover space along its parent's axis.
    pub fn fill(&mut self, id: NodeId) {
        self.set(id, Prop::Width, PropValue::Length(Length::Fill));
        self.set(id, Prop::Align, PropValue::Align(Align::Stretch));
    }
}

#[cfg(test)]
mod tests {
    use super::Surface;
    use crate::node::{NodeId, Patch, Prop, PropValue, Tag};
    use crate::SourceId;

    #[test]
    fn frames_are_sequenced_from_one_and_drain_pending_work() {
        let mut surface = Surface::new();
        let column = surface.container(Tag::Column, crate::style::Length::Step(2));
        surface.append(NodeId::ROOT, column);
        let first = surface.frame();
        assert_eq!(first.sequence, 1);
        assert!(!surface.is_pending());

        surface.text("second");
        let second = surface.frame();
        assert_eq!(second.sequence, 2);
        assert!(matches!(second.patches[0], Patch::Create { .. }));
    }

    #[test]
    fn identities_are_unique_across_frames() {
        let mut surface = Surface::new();
        let first = surface.create(Tag::Button);
        let _ = surface.frame();
        let second = surface.create(Tag::Button);
        assert_ne!(first, second);
    }

    #[test]
    fn event_stream_binds_the_windowed_source_without_rows() {
        let mut surface = Surface::new();
        let source = SourceId::new(7);
        let node = surface.event_stream(source);
        let frame = surface.frame();
        assert!(frame.patches.contains(&Patch::Create {
            id: node,
            tag: Tag::EventStream,
        }));
        assert!(frame.patches.contains(&Patch::SetProp {
            id: node,
            prop: Prop::Source,
            value: PropValue::Source(source),
        }));
        assert_eq!(frame.patches.len(), 2, "logical events arrive only through row windows");
    }
}
