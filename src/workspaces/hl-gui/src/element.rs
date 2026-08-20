//! Declarative description of an interface, and its reduction to patches.
//!
//! [`Surface`](crate::Surface) is imperative: a producer says what to change.
//! An [`Element`] says what the interface *is*, and [`Reconciliation`] works out
//! the difference from the previous description. The result is an ordinary
//! [`Frame`] of [`Patch`]es, so nothing downstream — the retained tree, the
//! adapter, the transport — learns that a description was involved.
//!
//! # Why creation is not batched
//!
//! A first render spends one patch per fact: a realistic panel — a card, a
//! heading, two rows, six styled and handled buttons, ten nodes in all — costs
//! 48 patches, of which 10 are creations, 10 are placements, 22 are properties
//! and 6 are handlers (`a_realistic_first_render_costs_one_patch_per_fact`
//! pins the number). A creation patch carrying its node's properties would
//! save those 22.
//!
//! It was not worth it. The saving lands once, on the first frame, and never
//! again: the frames that follow are already one patch per actual change, and
//! an unchanged description costs nothing at all. Against that, a batched
//! variant widens the wire contract permanently and forces every adapter to
//! implement a second creation path — for a fixed cost of a few dozen patches
//! at startup. If a description ever appears whose first frame is genuinely
//! large, the honest fix is a compression of the whole frame on the transport,
//! not a second shape for creation in the model.

use std::collections::{BTreeMap, BTreeSet};

use crate::identity::{Identities, NodeId};
use crate::node::{EventId, Frame, Handler, Patch, Prop, PropValue, Tag, Trigger};
use crate::style::{Length, Scale, Tone, Variant};

/// One described node: what it is, what it carries, and what it contains.
///
/// A description is a value, not a handle. It owns no identity, so the same
/// description reconciles against any previous one, and a producer is free to
/// rebuild the whole thing on every state change.
#[derive(Clone, Debug, PartialEq)]
pub struct Element {
    tag: Tag,
    key: Option<String>,
    props: BTreeMap<Prop, PropValue>,
    handlers: BTreeMap<Trigger, EventId>,
    children: Vec<Element>,
}

impl Element {
    /// An empty description of one node.
    #[must_use]
    pub fn new(tag: Tag) -> Self {
        Self {
            tag,
            key: None,
            props: BTreeMap::new(),
            handlers: BTreeMap::new(),
            children: Vec::new(),
        }
    }

    /// Names this child within its siblings.
    ///
    /// A keyed child keeps its retained node when the list is reordered, so
    /// widget state that lives in the adapter — scroll offset, caret, focus —
    /// travels with the item rather than with the position.
    #[must_use]
    pub fn key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }

    /// Declares one property.
    #[must_use]
    pub fn prop(mut self, prop: Prop, value: PropValue) -> Self {
        self.props.insert(prop, value);
        self
    }

    /// Declares the reaction this node offers for a trigger.
    #[must_use]
    pub fn on(mut self, trigger: Trigger, event: EventId) -> Self {
        self.handlers.insert(trigger, event);
        self
    }

    /// Appends one described child.
    #[must_use]
    pub fn child(mut self, child: Self) -> Self {
        self.children.push(child);
        self
    }

    /// Appends a described sequence, so a list renders from data in one call.
    #[must_use]
    pub fn children(mut self, children: impl IntoIterator<Item = Self>) -> Self {
        self.children.extend(children);
        self
    }

    /// Spacing between children.
    #[must_use]
    pub fn gap(self, gap: Length) -> Self {
        self.prop(Prop::Gap, PropValue::Length(gap))
    }

    /// The node's own text.
    #[must_use]
    pub fn label(self, label: impl Into<String>) -> Self {
        self.prop(Prop::Label, PropValue::text(label))
    }

    /// Width along the parent's main axis.
    #[must_use]
    pub fn width(self, width: Length) -> Self {
        self.prop(Prop::Width, PropValue::Length(width))
    }

    /// Semantic weight.
    #[must_use]
    pub fn tone(self, tone: Tone) -> Self {
        self.prop(Prop::Tone, PropValue::Tone(tone))
    }

    /// Emphasis level.
    #[must_use]
    pub fn variant(self, variant: Variant) -> Self {
        self.prop(Prop::Variant, PropValue::Variant(variant))
    }

    /// Text size role.
    #[must_use]
    pub fn scale(self, scale: Scale) -> Self {
        self.prop(Prop::Scale, PropValue::Scale(scale))
    }
}

/// Shorthands for the shapes a producer writes constantly. Each is exactly the
/// spelling above, so a description mixing them stays one vocabulary.
impl Element {
    /// A run of text.
    #[must_use]
    pub fn text(value: impl Into<String>) -> Self {
        Self::new(Tag::Text).label(value)
    }

    /// A section heading.
    #[must_use]
    pub fn heading(value: impl Into<String>) -> Self {
        Self::new(Tag::Heading).label(value)
    }

    /// A button bound to the identity it reports when invoked.
    #[must_use]
    pub fn button(label: impl Into<String>, event: EventId) -> Self {
        Self::new(Tag::Button).label(label).on(Trigger::Invoke, event)
    }

    /// A toned status marker.
    #[must_use]
    pub fn badge(label: impl Into<String>, tone: Tone) -> Self {
        Self::new(Tag::Badge).label(label).tone(tone)
    }

    /// A single-line text field reporting every change.
    #[must_use]
    pub fn entry(value: impl Into<String>, event: EventId) -> Self {
        Self::new(Tag::Entry)
            .prop(Prop::Value, PropValue::text(value))
            .on(Trigger::Change, event)
    }

    /// A horizontal container.
    #[must_use]
    pub fn row() -> Self {
        Self::new(Tag::Row)
    }

    /// A vertical container.
    #[must_use]
    pub fn column() -> Self {
        Self::new(Tag::Column)
    }

    /// A raised grouping surface.
    #[must_use]
    pub fn card() -> Self {
        Self::new(Tag::Card)
    }
}

/// One retained node behind a described one: the identity that was allocated
/// for it and the description it currently reflects.
///
/// The description is kept without its children, because the children live here
/// as instances; keeping both copies would let them disagree.
#[derive(Clone, Debug)]
struct Instance {
    id: NodeId,
    subject: Element,
    children: Vec<Instance>,
}

impl Instance {
    /// Whether a described node continues this one rather than replacing it.
    /// Tag and key are the identity of a position; everything else is a delta.
    fn continues(&self, element: &Element) -> bool {
        self.subject.tag == element.tag && self.subject.key == element.key
    }

    /// Emits the property delta and adopts the described properties.
    fn properties(&mut self, frame: &mut Frame, element: &Element) {
        let id = self.id;
        for (prop, value) in &element.props {
            if !self
                .subject
                .props
                .get(prop)
                .is_some_and(|current| current.unchanged(value))
            {
                frame.push(Patch::SetProp {
                    id,
                    prop: *prop,
                    value: value.clone(),
                });
            }
        }
        for prop in self.subject.props.keys() {
            if !element.props.contains_key(prop) {
                frame.push(Patch::ClearProp { id, prop: *prop });
            }
        }
        self.subject.props.clone_from(&element.props);
    }

    /// Emits the handler delta and adopts the described handlers.
    fn reactions(&mut self, frame: &mut Frame, element: &Element) {
        let id = self.id;
        for (trigger, event) in &element.handlers {
            if self.subject.handlers.get(trigger) != Some(event) {
                frame.push(Patch::SetHandler {
                    id,
                    handler: Handler::new(*trigger, event.clone()),
                });
            }
        }
        for trigger in self.subject.handlers.keys() {
            if !element.handlers.contains_key(trigger) {
                frame.push(Patch::ClearHandler { id, trigger: *trigger });
            }
        }
        self.subject.handlers.clone_from(&element.handlers);
    }
}

/// Turns successive descriptions into the frames that carry them.
///
/// One reconciliation owns one retained tree, so it holds the previous
/// description, the identities allocated for it, and the frame sequence. The
/// description it is given is always the single child of
/// [`NodeId::ROOT`](crate::NodeId::ROOT).
#[derive(Debug)]
pub struct Reconciliation {
    identities: Identities,
    sequence: u64,
    instance: Option<Instance>,
}

impl Default for Reconciliation {
    fn default() -> Self {
        Self::new()
    }
}

impl Reconciliation {
    /// A reconciliation with nothing rendered yet; the first frame creates the
    /// whole described tree.
    #[must_use]
    pub fn new() -> Self {
        Self {
            identities: Identities::new(),
            sequence: 0,
            instance: None,
        }
    }

    /// The last sequence handed out, matching the tree that accepted it.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Reduces a description to the frame that turns the previous one into it.
    ///
    /// The frame is minimal in the sense that matters to an adapter: a node
    /// that still exists is never destroyed and recreated, an unchanged
    /// property is never re-sent, and a reordered keyed child moves. Rendering
    /// the same description twice therefore yields an empty frame, which the
    /// tree still counts, so sequences stay contiguous.
    pub fn reconcile(&mut self, next: &Element) -> Frame {
        self.sequence = self.sequence.saturating_add(1);
        let mut frame = Frame::new(self.sequence);
        let previous = self.instance.take();
        self.instance = Some(self.root(&mut frame, previous, next));
        frame
    }

    /// Reconciles the top of the description against the root's single child.
    fn root(&mut self, frame: &mut Frame, previous: Option<Instance>, next: &Element) -> Instance {
        let Some(mut instance) = previous else {
            let fresh = self.build(frame, next);
            frame.push(Patch::Insert {
                parent: NodeId::ROOT,
                child: fresh.id,
                before: None,
            });
            return fresh;
        };
        if instance.continues(next) {
            self.update(frame, &mut instance, next);
            return instance;
        }
        // A different tag is a different widget: the replacement is attached
        // where the old node stood before the old node goes away, so the root
        // is never momentarily empty.
        let fresh = self.build(frame, next);
        frame.push(Patch::Insert {
            parent: NodeId::ROOT,
            child: fresh.id,
            before: Some(instance.id),
        });
        frame.push(Patch::Remove { id: instance.id });
        fresh
    }

    /// Creates a whole described subtree, detached from its parent.
    fn build(&mut self, frame: &mut Frame, element: &Element) -> Instance {
        let id = self.identities.allocate();
        frame.push(Patch::Create { id, tag: element.tag });
        for (prop, value) in &element.props {
            frame.push(Patch::SetProp {
                id,
                prop: *prop,
                value: value.clone(),
            });
        }
        for (trigger, event) in &element.handlers {
            frame.push(Patch::SetHandler {
                id,
                handler: Handler::new(*trigger, event.clone()),
            });
        }
        let mut children = Vec::with_capacity(element.children.len());
        for child in &element.children {
            let built = self.build(frame, child);
            frame.push(Patch::Insert {
                parent: id,
                child: built.id,
                before: None,
            });
            children.push(built);
        }
        Instance {
            id,
            subject: Element {
                tag: element.tag,
                key: element.key.clone(),
                props: element.props.clone(),
                handlers: element.handlers.clone(),
                children: Vec::new(),
            },
            children,
        }
    }

    /// Emits every delta between a retained node and the node now described.
    fn update(&mut self, frame: &mut Frame, instance: &mut Instance, element: &Element) {
        instance.properties(frame, element);
        instance.reactions(frame, element);
        self.offspring(frame, instance, element);
    }

    /// Continues an instance, or creates one where nothing was continued.
    fn resolve(&mut self, frame: &mut Frame, previous: Option<Instance>, element: &Element) -> Instance {
        let Some(mut instance) = previous else {
            return self.build(frame, element);
        };
        self.update(frame, &mut instance, element);
        instance
    }

    /// Reconciles one node's children: identities, then order, then removals.
    ///
    /// Order is settled right to left so every anchor a patch names is already
    /// in its final place, which is what lets a single pass be correct.
    fn offspring(&mut self, frame: &mut Frame, instance: &mut Instance, element: &Element) {
        let parent = instance.id;
        let retained = std::mem::take(&mut instance.children);
        let pairs = pairings(&retained, &element.children);
        let claimed = pairs.iter().flatten().copied().collect::<BTreeSet<_>>();
        let mut sources = retained.into_iter().map(Some).collect::<Vec<_>>();

        let mut resolved = Vec::with_capacity(element.children.len());
        for (index, described) in element.children.iter().enumerate() {
            let source = pairs[index].and_then(|origin| sources[origin].take());
            resolved.push(self.resolve(frame, source, described));
        }

        let settled = settlement(&pairs);
        for index in (0..resolved.len()).rev() {
            let anchor = resolved.get(index + 1).map(|sibling| sibling.id);
            let child = resolved[index].id;
            if pairs[index].is_none() {
                frame.push(Patch::Insert {
                    parent,
                    child,
                    before: anchor,
                });
            } else if !settled.contains(&index) {
                frame.push(Patch::Move {
                    parent,
                    child,
                    before: anchor,
                });
            }
        }

        // Departing nodes go last, so a replacement is already attached where
        // the node it replaces stood and the parent never renders short.
        for (index, source) in sources.iter().enumerate() {
            if !claimed.contains(&index) {
                frame.push(Patch::Remove {
                    id: source.as_ref().expect("an unclaimed node was never taken").id,
                });
            }
        }
        instance.children = resolved;
    }
}

/// For each described child, the retained child it continues, if any.
///
/// Keys win over position, so a keyed child that moved is still recognized;
/// an unkeyed child continues whatever unclaimed node shares its position.
fn pairings(retained: &[Instance], described: &[Element]) -> Vec<Option<usize>> {
    let mut keyed = BTreeMap::new();
    for (index, instance) in retained.iter().enumerate() {
        let Some(key) = instance.subject.key.as_deref() else {
            continue;
        };
        keyed.insert(key, index);
    }

    let mut pairs = described
        .iter()
        .map(|element| {
            element
                .key
                .as_deref()
                .and_then(|key| keyed.remove(key))
                .filter(|origin| retained[*origin].subject.tag == element.tag)
        })
        .collect::<Vec<_>>();

    let claimed = pairs.iter().flatten().copied().collect::<BTreeSet<_>>();
    for (index, element) in described.iter().enumerate() {
        pairs[index] = pairs[index].or_else(|| position(retained, &claimed, index, element));
    }
    pairs
}

/// The retained child at the same position, when neither side is keyed and the
/// tags agree. A keyed description never falls back to position: its key not
/// matching means the item itself is gone.
fn position(retained: &[Instance], claimed: &BTreeSet<usize>, index: usize, element: &Element) -> Option<usize> {
    if element.key.is_some() || claimed.contains(&index) {
        return None;
    }
    let candidate = retained.get(index)?;
    if candidate.subject.key.is_some() || candidate.subject.tag != element.tag {
        return None;
    }
    Some(index)
}

/// The described positions whose retained nodes are already in relative order,
/// and so need no `Move`.
///
/// This is the longest run of continued children whose retained positions
/// increase: keeping the longest such run still means moving every other
/// continued child, which is the fewest moves any correct frame can contain.
fn settlement(pairs: &[Option<usize>]) -> BTreeSet<usize> {
    let run = pairs
        .iter()
        .enumerate()
        .filter_map(|(index, origin)| origin.map(|origin| (index, origin)))
        .collect::<Vec<_>>();
    let mut length = vec![1usize; run.len()];
    let mut ancestry = vec![None; run.len()];
    for index in 0..run.len() {
        let Some(preceding) = predecessor(&run, &length, index) else {
            continue;
        };
        length[index] = length[preceding].saturating_add(1);
        ancestry[index] = Some(preceding);
    }

    let mut cursor = length
        .iter()
        .enumerate()
        .max_by_key(|(_, reach)| **reach)
        .map(|(index, _)| index);
    let mut settled = BTreeSet::new();
    while let Some(index) = cursor {
        settled.insert(run[index].0);
        cursor = ancestry[index];
    }
    settled
}

/// The earlier entry with the longest run that this one can extend.
fn predecessor(run: &[(usize, usize)], length: &[usize], index: usize) -> Option<usize> {
    (0..index)
        .filter(|earlier| run[*earlier].1 < run[index].1)
        .max_by_key(|earlier| length[*earlier])
}

#[cfg(test)]
mod tests {
    use super::{Element, Reconciliation};
    use crate::node::{Patch, Tag};

    #[test]
    fn an_unchanged_description_produces_no_work() {
        let mut reconciliation = Reconciliation::new();
        let described = Element::column().child(Element::text("one"));
        assert!(!reconciliation.reconcile(&described).is_empty());
        let second = reconciliation.reconcile(&described);
        assert!(second.is_empty());
        assert_eq!(second.sequence, 2);
        assert_eq!(reconciliation.sequence(), 2);
    }

    #[test]
    fn a_changed_tag_replaces_the_node_it_stands_in_for() {
        let mut reconciliation = Reconciliation::new();
        let _ = reconciliation.reconcile(&Element::column());
        let frame = reconciliation.reconcile(&Element::row());
        assert!(matches!(
            frame.patches.first(),
            Some(Patch::Create { tag: Tag::Row, .. })
        ));
        assert!(matches!(frame.patches.last(), Some(Patch::Remove { .. })));
    }
}
