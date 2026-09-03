//! A rebound handler must replace the previous one, and a cleared one must go.
//!
//! GTK keeps every closure connected to a signal, so an adapter that connects
//! again on a rebind leaves the old identity reporting alongside the new one.
//! These assert on what the surface reports for a single real click, which is
//! the only place that mistake is visible.
//!
//! They need a display connection; when none is available they report that
//! rather than passing silently.

use gtk::prelude::*;
use hl_gui::{Event, EventId, Frame, NodeId, Patch, Tag, Tree, Trigger};

/// Initializes the toolkit, or reports that this environment cannot.
fn toolkit() -> bool {
    gtk::init().is_ok()
}

/// One tree and one rendered surface, with a button to click.
struct Session {
    tree: Tree,
    surface: hl_gui_gtk::Surface,
    button: NodeId,
    sequence: u64,
}

impl Session {
    fn new() -> Self {
        let mut session = Self {
            tree: Tree::new(),
            surface: hl_gui_gtk::Surface::new(),
            button: NodeId::ROOT,
            sequence: 0,
        };
        let mut producer = hl_gui::Surface::new();
        session.button = producer.create(Tag::Button);
        producer.append(NodeId::ROOT, session.button);
        let frame = producer.frame();
        session
            .tree
            .apply(&frame, &mut session.surface)
            .expect("the panel builds");
        session.sequence = frame.sequence;
        session
    }

    fn apply(&mut self, patch: Patch) {
        self.sequence += 1;
        let frame = Frame::new(self.sequence).with(patch);
        self.tree.apply(&frame, &mut self.surface).expect("the patch applies");
    }

    /// The single button, reached through the adapter's own style class.
    fn button(&self) -> gtk::Button {
        let mut found = vec![self.surface.widget().clone().upcast::<gtk::Widget>()];
        let mut index = 0;
        while index < found.len() {
            found.extend(offspring(&found[index]));
            index += 1;
        }
        found
            .into_iter()
            .find_map(|widget| widget.downcast::<gtk::Button>().ok())
            .expect("a button tag builds a button")
    }
}

fn offspring(widget: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut children = Vec::new();
    let mut cursor = widget.first_child();
    while let Some(child) = cursor {
        cursor = child.next_sibling();
        children.push(child);
    }
    children
}

fn handler(id: NodeId, name: &str) -> Patch {
    Patch::SetHandler {
        id,
        handler: hl_gui::Handler::new(Trigger::Invoke, EventId::new(name)),
    }
}

/// Every scenario runs inside one test: GTK initializes on one thread only, and
/// libtest gives every `#[test]` a thread of its own.
#[test]
fn a_handler_reports_exactly_the_identity_it_is_currently_bound_to() {
    if !toolkit() {
        eprintln!("skipped: no display connection");
        return;
    }
    a_rebound_handler_replaces_the_previous_one();
    a_cleared_handler_reports_nothing();
    queued_interaction_is_withdrawn_when_its_handler_is_cleared();
    queued_interaction_is_withdrawn_when_its_node_is_removed();
    a_handler_bound_again_after_clearing_reports_once_more();
}

fn queued_interaction_is_withdrawn_when_its_handler_is_cleared() {
    let mut session = Session::new();
    session.apply(handler(session.button, "stale"));
    session.button().emit_clicked();
    session.apply(Patch::ClearHandler {
        id: session.button,
        trigger: Trigger::Invoke,
    });

    assert!(session.surface.reports().is_empty(), "clearing authority also retires its queued action");
}

fn queued_interaction_is_withdrawn_when_its_node_is_removed() {
    let mut session = Session::new();
    session.apply(handler(session.button, "stale"));
    session.button().emit_clicked();
    session.apply(Patch::Remove { id: session.button });

    assert!(session.surface.reports().is_empty(), "removing a node retires its queued action");
}

fn a_rebound_handler_replaces_the_previous_one() {
    let mut session = Session::new();
    session.apply(handler(session.button, "first"));
    session.apply(handler(session.button, "second"));

    session.button().emit_clicked();

    let reported = session.surface.reports().drain();
    assert_eq!(reported.len(), 1, "one click reports once, got {reported:?}");
    assert!(
        matches!(&reported[0], Event::Invoke { id, .. } if id.as_str() == "second"),
        "the rebound identity is the only one that reports, got {:?}",
        reported[0]
    );
}

fn a_cleared_handler_reports_nothing() {
    let mut session = Session::new();
    session.apply(handler(session.button, "first"));
    session.apply(Patch::ClearHandler {
        id: session.button,
        trigger: Trigger::Invoke,
    });

    session.button().emit_clicked();

    assert!(
        session.surface.reports().is_empty(),
        "a cleared handler must not report, got {:?}",
        session.surface.reports().drain()
    );
}

fn a_handler_bound_again_after_clearing_reports_once_more() {
    let mut session = Session::new();
    session.apply(handler(session.button, "first"));
    session.apply(Patch::ClearHandler {
        id: session.button,
        trigger: Trigger::Invoke,
    });
    session.apply(handler(session.button, "third"));

    session.button().emit_clicked();

    let reported = session.surface.reports().drain();
    assert_eq!(reported.len(), 1, "one click reports once, got {reported:?}");
    assert!(
        matches!(&reported[0], Event::Invoke { id, .. } if id.as_str() == "third"),
        "clearing then binding again restores exactly one report, got {:?}",
        reported[0]
    );
}
