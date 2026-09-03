//! Public-contract coverage for the retained tree and its mutation rules.

use hl_gui::{
    Column, Fault, Frame, NodeId, Patch, Prop, PropValue, Renderer, RowWindow, Surface, TABLE_COLUMN_LIMIT, Tag, Theme,
    Tree, TreeError, Trigger,
};

/// Records what an adapter would have been asked to do, so tree semantics are
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

fn tree_with(patches: Vec<Patch>) -> (Tree, Trace) {
    let mut tree = Tree::new();
    let mut trace = Trace::default();
    let frame = Frame { sequence: 1, patches };
    tree.apply(&frame, &mut trace).expect("valid frame");
    (tree, trace)
}

#[test]
fn bounded_frame_preflight_preserves_the_last_valid_tree_and_sequence() {
    let first = NodeId::new(1);
    let (tree, trace) = tree_with(vec![
        Patch::Create {
            id: first,
            tag: Tag::Text,
        },
        Patch::Insert {
            parent: NodeId::ROOT,
            child: first,
            before: None,
        },
    ]);
    let second = NodeId::new(2);
    let growth = Frame {
        sequence: 2,
        patches: vec![
            Patch::Create {
                id: second,
                tag: Tag::Text,
            },
            Patch::Insert {
                parent: NodeId::ROOT,
                child: second,
                before: None,
            },
        ],
    };

    assert_eq!(
        tree.preflight(&growth, 2),
        Err(TreeError::NodeLimit { limit: 2, received: 3 })
    );
    assert_eq!(tree.len(), 2);
    assert_eq!(tree.sequence(), 1);
    assert_eq!(trace.applied.len(), 2, "preflight calls no renderer");
}

#[test]
fn table_schema_is_refused_before_tree_or_renderer_mutation() {
    let table = NodeId::new(1);
    let (tree, trace) = tree_with(vec![Patch::Create { id: table, tag: Tag::DataTable }]);
    let invalid = Frame {
        sequence: 2,
        patches: vec![Patch::SetProp {
            id: table,
            prop: Prop::Schema,
            value: PropValue::Schema(
                (0..=TABLE_COLUMN_LIMIT).map(|index| Column::new(format!("key-{index}"), "Title")).collect(),
            ),
        }],
    };
    assert!(matches!(tree.preflight(&invalid, 100), Err(TreeError::InvalidSchema(_))));
    assert_eq!(tree.sequence(), 1);
    assert_eq!(trace.applied.len(), 1, "invalid schema reached no renderer");
}

#[test]
fn a_composed_surface_applies_as_one_frame() {
    let mut surface = Surface::new();
    let card = surface.create(Tag::Card);
    let title = surface.heading("Containers");
    surface.append(NodeId::ROOT, card);
    surface.append(card, title);
    let frame = surface.frame();

    let mut tree = Tree::new();
    let mut trace = Trace::default();
    tree.apply(&frame, &mut trace).expect("valid frame");

    assert_eq!(tree.root().children, vec![card]);
    assert_eq!(tree.node(card).expect("card").children, vec![title]);
    assert_eq!(tree.node(title).expect("title").text(Prop::Label), Some("Containers"));
    assert_eq!(trace.commits, vec![1]);
    assert_eq!(trace.applied.len(), frame.patches.len());
}

#[test]
fn insert_before_places_a_node_ahead_of_its_sibling() {
    let first = NodeId::new(1);
    let second = NodeId::new(2);
    let (tree, _) = tree_with(vec![
        Patch::Create {
            id: first,
            tag: Tag::Text,
        },
        Patch::Create {
            id: second,
            tag: Tag::Text,
        },
        Patch::Insert {
            parent: NodeId::ROOT,
            child: first,
            before: None,
        },
        Patch::Insert {
            parent: NodeId::ROOT,
            child: second,
            before: Some(first),
        },
    ]);
    assert_eq!(tree.root().children, vec![second, first]);
}

#[test]
fn moving_a_node_reorders_without_recreating_it() {
    let first = NodeId::new(1);
    let second = NodeId::new(2);
    let (mut tree, mut trace) = tree_with(vec![
        Patch::Create {
            id: first,
            tag: Tag::Text,
        },
        Patch::Create {
            id: second,
            tag: Tag::Text,
        },
        Patch::Insert {
            parent: NodeId::ROOT,
            child: first,
            before: None,
        },
        Patch::Insert {
            parent: NodeId::ROOT,
            child: second,
            before: None,
        },
    ]);

    let frame = Frame {
        sequence: 2,
        patches: vec![Patch::Move {
            parent: NodeId::ROOT,
            child: second,
            before: Some(first),
        }],
    };
    tree.apply(&frame, &mut trace).expect("valid move");
    assert_eq!(tree.root().children, vec![second, first]);
    assert_eq!(tree.len(), 3);
}

#[test]
fn removing_a_node_drops_its_whole_subtree() {
    let card = NodeId::new(1);
    let label = NodeId::new(2);
    let (mut tree, mut trace) = tree_with(vec![
        Patch::Create {
            id: card,
            tag: Tag::Card,
        },
        Patch::Create {
            id: label,
            tag: Tag::Text,
        },
        Patch::Insert {
            parent: NodeId::ROOT,
            child: card,
            before: None,
        },
        Patch::Insert {
            parent: card,
            child: label,
            before: None,
        },
    ]);

    let frame = Frame {
        sequence: 2,
        patches: vec![Patch::Remove { id: card }],
    };
    tree.apply(&frame, &mut trace).expect("valid removal");

    assert!(tree.node(card).is_none());
    assert!(tree.node(label).is_none(), "descendant must not survive");
    assert!(tree.is_empty());
}

#[test]
fn a_leaf_tag_rejects_children() {
    let mut tree = Tree::new();
    let mut trace = Trace::default();
    let text = NodeId::new(1);
    let child = NodeId::new(2);
    let frame = Frame {
        sequence: 1,
        patches: vec![
            Patch::Create {
                id: text,
                tag: Tag::Text,
            },
            Patch::Create {
                id: child,
                tag: Tag::Text,
            },
            Patch::Insert {
                parent: NodeId::ROOT,
                child: text,
                before: None,
            },
            Patch::Insert {
                parent: text,
                child,
                before: None,
            },
        ],
    };
    let failure = tree.apply(&frame, &mut trace).expect_err("leaf parent");
    assert_eq!(
        failure,
        Fault::Tree(TreeError::LeafParent {
            parent: text,
            tag: Tag::Text
        })
    );
}

#[test]
fn a_detached_surface_must_attach_to_the_root() {
    let mut tree = Tree::new();
    let mut trace = Trace::default();
    let card = NodeId::new(1);
    let dialog = NodeId::new(2);
    let frame = Frame {
        sequence: 1,
        patches: vec![
            Patch::Create {
                id: card,
                tag: Tag::Card,
            },
            Patch::Create {
                id: dialog,
                tag: Tag::Dialog,
            },
            Patch::Insert {
                parent: NodeId::ROOT,
                child: card,
                before: None,
            },
            Patch::Insert {
                parent: card,
                child: dialog,
                before: None,
            },
        ],
    };
    let failure = tree.apply(&frame, &mut trace).expect_err("detached child");
    assert_eq!(
        failure,
        Fault::Tree(TreeError::DetachedChild {
            child: dialog,
            tag: Tag::Dialog
        })
    );
}

#[test]
fn a_cycle_is_rejected_before_the_adapter_sees_it() {
    let outer = NodeId::new(1);
    let inner = NodeId::new(2);
    let (mut tree, mut trace) = tree_with(vec![
        Patch::Create {
            id: outer,
            tag: Tag::Column,
        },
        Patch::Create {
            id: inner,
            tag: Tag::Column,
        },
        Patch::Insert {
            parent: NodeId::ROOT,
            child: outer,
            before: None,
        },
        Patch::Insert {
            parent: outer,
            child: inner,
            before: None,
        },
    ]);

    let applied = trace.applied.len();
    let frame = Frame {
        sequence: 2,
        patches: vec![Patch::Move {
            parent: inner,
            child: outer,
            before: None,
        }],
    };
    let failure = tree.apply(&frame, &mut trace).expect_err("cycle");
    assert_eq!(
        failure,
        Fault::Tree(TreeError::Cycle {
            parent: inner,
            child: outer
        })
    );
    assert_eq!(trace.applied.len(), applied, "adapter must not be called");
}

#[test]
fn an_unknown_node_is_rejected_rather_than_created_implicitly() {
    let mut tree = Tree::new();
    let mut trace = Trace::default();
    let ghost = NodeId::new(9);
    let frame = Frame {
        sequence: 1,
        patches: vec![Patch::SetProp {
            id: ghost,
            prop: Prop::Label,
            value: PropValue::text("x"),
        }],
    };
    assert_eq!(
        tree.apply(&frame, &mut trace).expect_err("unknown node"),
        Fault::Tree(TreeError::UnknownNode(ghost))
    );
}

#[test]
fn frames_must_arrive_in_order() {
    let mut tree = Tree::new();
    let mut trace = Trace::default();
    let frame = Frame {
        sequence: 7,
        patches: Vec::new(),
    };
    assert_eq!(
        tree.apply(&frame, &mut trace).expect_err("stale sequence"),
        Fault::Tree(TreeError::StaleSequence {
            expected: 1,
            received: 7
        })
    );
}

#[test]
fn the_root_cannot_be_removed() {
    let mut tree = Tree::new();
    let mut trace = Trace::default();
    let frame = Frame {
        sequence: 1,
        patches: vec![Patch::Remove { id: NodeId::ROOT }],
    };
    assert_eq!(
        tree.apply(&frame, &mut trace).expect_err("root removal"),
        Fault::Tree(TreeError::RemoveRoot)
    );
}

#[test]
fn handlers_resolve_by_node_and_trigger() {
    let mut surface = Surface::new();
    let button = surface.button("Stop", hl_gui::EventId::new("stop"));
    surface.append(NodeId::ROOT, button);
    let frame = surface.frame();

    let mut tree = Tree::new();
    let mut trace = Trace::default();
    tree.apply(&frame, &mut trace).expect("valid frame");

    assert_eq!(
        tree.handler(button, Trigger::Invoke).map(hl_gui::EventId::as_str),
        Some("stop")
    );
    assert!(tree.handler(button, Trigger::Submit).is_none());
}
