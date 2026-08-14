//! Public-contract coverage for declarative descriptions and their reduction
//! to patches. Every case applies the produced frame to a real [`Tree`], so the
//! description and the retained tree are proved to agree.

use hl_gui::{
    Element, EventId, Frame, Length, NodeId, Patch, Prop, PropValue, Reconciliation, Renderer, RowWindow, Tag, Theme,
    Tone, Tree, Trigger,
};

/// Records what an adapter would have been asked to do, so reconciliation is
/// provable without any toolkit.
#[derive(Debug, Default)]
struct Trace {
    applied: Vec<Patch>,
    commits: Vec<u64>,
}

impl Renderer for Trace {
    type Error = std::convert::Infallible;

    fn patch(&mut self, patch: &Patch, _tree: &Tree) -> Result<(), Self::Error> {
        self.applied.push(patch.clone());
        Ok(())
    }

    fn commit(&mut self, sequence: u64) -> Result<(), Self::Error> {
        self.commits.push(sequence);
        Ok(())
    }

    fn rows(&mut self, _window: &RowWindow) -> Result<(), Self::Error> {
        Ok(())
    }

    fn theme(&mut self, _theme: &Theme) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// One producer and the tree it drives, so a test reads as a sequence of
/// descriptions rather than as plumbing.
struct Session {
    reconciliation: Reconciliation,
    tree: Tree,
    trace: Trace,
}

impl Session {
    fn new() -> Self {
        Self {
            reconciliation: Reconciliation::new(),
            tree: Tree::new(),
            trace: Trace::default(),
        }
    }

    /// Reconciles a description, applies the frame, and hands the frame back.
    fn render(&mut self, described: &Element) -> Frame {
        let frame = self.reconciliation.reconcile(described);
        self.tree.apply(&frame, &mut self.trace).expect("frame applies");
        frame
    }

    /// The node reached by walking child positions down from the tree root.
    fn identity(&self, path: &[usize]) -> NodeId {
        let mut current = NodeId::ROOT;
        for step in path {
            current = self.tree.node(current).expect("live node").children[*step];
        }
        current
    }

    fn label(&self, path: &[usize]) -> String {
        self.tree
            .node(self.identity(path))
            .expect("live node")
            .text(Prop::Label)
            .expect("a labelled node")
            .to_owned()
    }

    fn tag(&self, path: &[usize]) -> Tag {
        self.tree.node(self.identity(path)).expect("live node").tag
    }
}

fn creations(frame: &Frame) -> usize {
    frame
        .patches
        .iter()
        .filter(|patch| matches!(patch, Patch::Create { .. }))
        .count()
}

fn kind_count(frame: &Frame, kind: fn(&Patch) -> bool) -> usize {
    frame.patches.iter().filter(|patch| kind(patch)).count()
}

fn row(key: &str, label: &str) -> Element {
    Element::text(label).key(key)
}

#[test]
fn a_first_description_creates_the_tree_it_describes() {
    let mut session = Session::new();
    let described = Element::column()
        .gap(Length::Step(2))
        .child(Element::heading("Containers"))
        .child(
            Element::row()
                .child(Element::text("one"))
                .child(Element::badge("2", Tone::Accent)),
        );

    let frame = session.render(&described);

    assert_eq!(frame.sequence, 1);
    assert_eq!(session.tree.root().children.len(), 1);
    assert_eq!(session.tag(&[0]), Tag::Column);
    assert_eq!(session.tag(&[0, 0]), Tag::Heading);
    assert_eq!(session.label(&[0, 0]), "Containers");
    assert_eq!(session.tag(&[0, 1]), Tag::Row);
    assert_eq!(session.label(&[0, 1, 0]), "one");
    assert_eq!(session.tag(&[0, 1, 1]), Tag::Badge);
    assert_eq!(
        session
            .tree
            .node(session.identity(&[0]))
            .expect("column")
            .prop(Prop::Gap),
        Some(&PropValue::Length(Length::Step(2)))
    );
    assert_eq!(session.trace.commits, vec![1]);
    assert_eq!(session.trace.applied.len(), frame.patches.len());
}

#[test]
fn an_identical_description_produces_no_patches_at_all() {
    let mut session = Session::new();
    let described = Element::card()
        .child(Element::heading("Idle"))
        .child(Element::button("Start", EventId::new("start")));

    session.render(&described);
    let second = session.render(&described);

    assert_eq!(second.patches, Vec::new(), "an unchanged description must not churn");
    assert_eq!(second.sequence, 2);
    assert_eq!(session.trace.commits, vec![1, 2], "the frame is still counted");
}

#[test]
fn a_changed_text_property_emits_exactly_one_patch() {
    let mut session = Session::new();
    session.render(&Element::column().child(Element::text("before")));
    let identity = session.identity(&[0, 0]);

    let frame = session.render(&Element::column().child(Element::text("after")));

    assert_eq!(
        frame.patches,
        vec![Patch::SetProp {
            id: identity,
            prop: Prop::Label,
            value: PropValue::text("after"),
        }]
    );
    assert_eq!(session.label(&[0, 0]), "after");
    assert_eq!(session.identity(&[0, 0]), identity, "the node survived the change");
}

#[test]
fn a_dropped_property_is_cleared_rather_than_left_behind() {
    let mut session = Session::new();
    session.render(&Element::column().child(Element::text("row").tone(Tone::Danger)));
    let identity = session.identity(&[0, 0]);

    let frame = session.render(&Element::column().child(Element::text("row")));

    assert_eq!(
        frame.patches,
        vec![Patch::ClearProp {
            id: identity,
            prop: Prop::Tone,
        }]
    );
    assert_eq!(
        session.tree.node(identity).expect("row").prop(Prop::Tone),
        None,
        "the property must be gone from the tree too"
    );
}

#[test]
fn reordering_keyed_children_moves_them_instead_of_recreating_them() {
    let mut session = Session::new();
    let first = Element::column()
        .child(row("a", "alpha"))
        .child(row("b", "beta"))
        .child(row("c", "gamma"));
    session.render(&first);
    let alpha = session.identity(&[0, 0]);
    let beta = session.identity(&[0, 1]);
    let gamma = session.identity(&[0, 2]);

    let reversed = Element::column()
        .child(row("c", "gamma"))
        .child(row("b", "beta"))
        .child(row("a", "alpha"));
    let frame = session.render(&reversed);

    assert_eq!(creations(&frame), 0, "reordering must not recreate anything");
    assert_eq!(kind_count(&frame, |patch| matches!(patch, Patch::Remove { .. })), 0);
    assert!(kind_count(&frame, |patch| matches!(patch, Patch::Move { .. })) > 0);
    assert_eq!(session.identity(&[0, 0]), gamma);
    assert_eq!(session.identity(&[0, 1]), beta);
    assert_eq!(session.identity(&[0, 2]), alpha);
    assert_eq!(session.label(&[0, 0]), "gamma");
}

#[test]
fn inserting_a_keyed_child_in_the_middle_keeps_its_siblings() {
    let mut session = Session::new();
    session.render(&Element::column().child(row("a", "alpha")).child(row("c", "gamma")));
    let alpha = session.identity(&[0, 0]);
    let gamma = session.identity(&[0, 1]);

    let frame = session.render(
        &Element::column()
            .child(row("a", "alpha"))
            .child(row("b", "beta"))
            .child(row("c", "gamma")),
    );

    assert_eq!(creations(&frame), 1, "only the new child is created");
    assert_eq!(kind_count(&frame, |patch| matches!(patch, Patch::Move { .. })), 0);
    assert_eq!(session.identity(&[0, 0]), alpha);
    assert_eq!(session.identity(&[0, 2]), gamma);
    assert_eq!(session.label(&[0, 1]), "beta");
}

#[test]
fn a_child_that_disappears_is_removed_with_its_subtree() {
    let mut session = Session::new();
    session.render(
        &Element::column()
            .child(Element::text("kept"))
            .child(Element::card().child(Element::text("doomed"))),
    );
    let card = session.identity(&[0, 1]);
    let inner = session.identity(&[0, 1, 0]);

    let frame = session.render(&Element::column().child(Element::text("kept")));

    assert_eq!(frame.patches, vec![Patch::Remove { id: card }]);
    assert!(session.tree.node(card).is_none());
    assert!(session.tree.node(inner).is_none(), "the subtree went with it");
    assert_eq!(
        session
            .tree
            .node(session.identity(&[0]))
            .expect("column")
            .children
            .len(),
        1
    );
}

#[test]
fn a_changed_tag_at_the_same_position_replaces_the_node() {
    let mut session = Session::new();
    session.render(&Element::column().child(Element::text("state")));
    let replaced = session.identity(&[0, 0]);

    let frame = session.render(&Element::column().child(Element::badge("state", Tone::Warning)));

    assert_eq!(creations(&frame), 1);
    assert_eq!(kind_count(&frame, |patch| matches!(patch, Patch::Insert { .. })), 1);
    assert_eq!(frame.patches.last(), Some(&Patch::Remove { id: replaced }));
    assert!(session.tree.node(replaced).is_none());
    assert_eq!(session.tag(&[0, 0]), Tag::Badge);
}

#[test]
fn a_deep_description_round_trips_through_the_tree() {
    let mut session = Session::new();
    let described = |detail: &str| {
        Element::column().child(
            Element::card().child(
                Element::row()
                    .child(
                        Element::column()
                            .child(Element::text("leaf"))
                            .child(Element::text(detail)),
                    )
                    .child(Element::badge("running", Tone::Positive)),
            ),
        )
    };

    session.render(&described("first"));
    let leaf = session.identity(&[0, 0, 0, 0, 0]);
    let detail = session.identity(&[0, 0, 0, 0, 1]);

    let frame = session.render(&described("second"));

    assert_eq!(
        frame.patches,
        vec![Patch::SetProp {
            id: detail,
            prop: Prop::Label,
            value: PropValue::text("second"),
        }],
        "only the deep leaf changed"
    );
    assert_eq!(session.identity(&[0, 0, 0, 0, 0]), leaf);
    assert_eq!(session.label(&[0, 0, 0, 0, 1]), "second");
    assert_eq!(session.tag(&[0, 0, 0, 1]), Tag::Badge);
}

#[test]
fn handler_changes_bind_and_unbind_without_touching_the_node() {
    let mut session = Session::new();
    session.render(&Element::column().child(Element::button("Go", EventId::new("go"))));
    let button = session.identity(&[0, 0]);

    let rebound = session.render(&Element::column().child(Element::button("Go", EventId::new("halt"))));
    assert_eq!(creations(&rebound), 0);
    assert_eq!(
        kind_count(&rebound, |patch| matches!(patch, Patch::SetHandler { .. })),
        1
    );
    assert_eq!(
        session.tree.handler(button, Trigger::Invoke).map(EventId::as_str),
        Some("halt")
    );

    let bare = session.render(&Element::column().child(Element::new(Tag::Button).label("Go")));
    assert_eq!(
        bare.patches,
        vec![Patch::ClearHandler {
            id: button,
            trigger: Trigger::Invoke,
        }]
    );
    assert!(session.tree.handler(button, Trigger::Invoke).is_none());
    assert_eq!(session.identity(&[0, 0]), button, "the button itself survived");
}
