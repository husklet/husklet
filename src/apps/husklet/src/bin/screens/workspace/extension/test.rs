//! What the extension page promises: queued work reaches the widgets under a
//! per-tick cap, nothing is dropped, a stopped extension keeps its last
//! interface on screen, and interaction reaches the caller's sink.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use hl_gui::{Element, Event, EventId, Reconciliation, Tag};

use super::{channel, Delivery, Interface, Post, Signal, DRAIN};

/// Everything the sink was handed, in order.
type Record = Rc<RefCell<Vec<Signal>>>;

/// One page, its queue, and what its sink recorded.
struct Fixture {
    widget: gtk::Box,
    page: Interface,
    post: Post,
    recorded: Record,
    reconciliation: Reconciliation,
}

impl Fixture {
    fn new() -> Self {
        let (post, deliveries) = channel();
        let recorded: Record = Rc::new(RefCell::new(Vec::new()));
        let sink = recorded.clone();
        let (widget, page) = Interface::new(deliveries, Rc::new(move |signal| sink.borrow_mut().push(signal)));
        Self {
            widget,
            page,
            post,
            recorded,
            reconciliation: Reconciliation::new(),
        }
    }

    /// Queues the frame that turns the current description into `described`.
    fn describe(&mut self, described: &Element) {
        let frame = self.reconciliation.reconcile(described);
        self.post.send(Delivery::Frame(frame)).expect("the page is listening");
    }

    /// Every widget on the page, parents before children.
    fn widgets(&self) -> Vec<gtk::Widget> {
        let mut found = vec![self.widget.clone().upcast::<gtk::Widget>()];
        let mut index = 0;
        while index < found.len() {
            found.extend(offspring(&found[index]));
            index += 1;
        }
        found
    }

    /// The first widget carrying a tag's style class, which is the adapter's
    /// own public naming.
    fn tagged(&self, tag: Tag) -> Option<gtk::Widget> {
        let class = format!("hl-{}", tag.as_str().to_ascii_lowercase());
        self.widgets().into_iter().find(|widget| widget.has_css_class(&class))
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

/// A small interface with one button, keyed so a redescription is a no-op.
fn panel(title: &str) -> Element {
    Element::column()
        .child(Element::heading(title).key("title"))
        .child(Element::button("Restart", EventId::new("restart")).key("restart"))
}

/// Every scenario runs inside one test, on the binary's toolkit thread.
///
/// GTK belongs to whichever thread entered it and libtest gives every `#[test]`
/// its own, so the scenarios are handed to `test_support`, which owns the one
/// thread in this process that entered GTK. This test and the extension-shelf
/// test both need that thread, and entering GTK in their own is what used to
/// leave whichever ran second either panicking or skipped.
#[test]
fn an_extension_page_renders_what_is_queued_and_survives_the_extension() {
    let ran = crate::test_support::on_the_toolkit_thread(|| {
        a_queued_frame_puts_widgets_on_the_page();
        an_identical_frame_changes_nothing();
        a_burst_beyond_the_tick_bound_stays_queued();
        a_stopped_extension_keeps_its_widgets_and_says_so();
        a_structured_fault_reaches_lifecycle_on_the_toolkit_tick();
        a_rendered_button_reaches_the_sink();
        retained_pane_actions_keep_their_slot();
        retiring_a_pane_discards_its_queued_interaction();
        semantics_are_redacted_and_actions_reject_stale_revisions();
        semantic_actions_are_safe_by_default_and_preserve_authored_danger();
    });
    if !ran {
        eprintln!("skipped: no display connection, so the extension page cannot be rendered");
    }
}

fn retiring_a_pane_discards_its_queued_interaction() {
    let mut fixture = Fixture::new();
    let pane = fixture.page.pane("pane-gone");
    let frame = Reconciliation::new().reconcile(&panel("Gone"));
    fixture
        .post
        .send(Delivery::FrameAt {
            slot: "pane-gone".into(),
            frame,
        })
        .expect("the page is listening");
    fixture.page.tick();
    let button = descendants(&pane)
        .into_iter()
        .find_map(|widget| widget.downcast::<gtk::Button>().ok())
        .expect("retained pane button");
    button.emit_clicked();
    fixture.page.retire("pane-gone");
    fixture.page.tick();
    assert!(fixture.recorded.borrow().is_empty(), "a retired pane cannot leak its queued event");
}

fn descendants(widget: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut found = vec![widget.clone()];
    let mut index = 0;
    while index < found.len() {
        found.extend(offspring(&found[index]));
        index += 1;
    }
    found
}

fn retained_pane_actions_keep_their_slot() {
    let mut fixture = Fixture::new();
    let frame = Reconciliation::new().reconcile(&panel("Pane two"));
    fixture
        .post
        .send(Delivery::FrameAt {
            slot: "pane-2".into(),
            frame,
        })
        .expect("the page is listening");
    fixture.page.tick();
    let tree = fixture.page.semantics("pane-2").expect("pane semantics");
    let button = &tree.root.children[0].children[1];
    fixture
        .page
        .semantic_action_at(
            "pane-2",
            &hl_extension::PaneSemanticAction {
                revision: tree.revision,
                node: button.id,
                action: hl_extension::SemanticActionKind::Invoke,
                value: None,
            },
        )
        .expect("declared pane action");
    assert!(matches!(
        fixture.recorded.borrow().last(),
        Some(Signal::InteractionAt { slot, event: Event::Invoke { .. } }) if slot == "pane-2"
    ));
}

fn semantics_are_redacted_and_actions_reject_stale_revisions() {
    let mut fixture = Fixture::new();
    let described = Element::column().child(
        Element::password_entry(EventId::new("secret"))
            .prop(hl_gui::Prop::Label, hl_gui::PropValue::text("Password"))
            .prop(hl_gui::Prop::Value, hl_gui::PropValue::text("hunter2"))
            .prop(hl_gui::Prop::Secret, hl_gui::PropValue::Flag(true)),
    );
    fixture.describe(&described);
    fixture.page.tick();
    let tree = fixture.page.semantics("pane-1").expect("semantic snapshot");
    let field = &tree.root.children[0].children[0];
    assert_eq!(field.label.as_deref(), Some("Password"));
    assert_eq!(field.value.as_deref(), Some("[redacted]"));
    assert_eq!(field.actions, vec![hl_extension::SemanticActionKind::Change]);

    fixture
        .page
        .semantic_action(&hl_extension::PaneSemanticAction {
            revision: tree.revision,
            node: field.id,
            action: hl_extension::SemanticActionKind::Change,
            value: Some("replacement".into()),
        })
        .expect("declared action");
    assert!(matches!(
        fixture.recorded.borrow().last(),
        Some(Signal::Interaction(Event::Change { .. }))
    ));
    let stale = fixture.page.semantic_action(&hl_extension::PaneSemanticAction {
        revision: tree.revision.saturating_sub(1),
        node: field.id,
        action: hl_extension::SemanticActionKind::Change,
        value: None,
    });
    assert!(matches!(stale, Err(hl_extension::HostError::Conflict(_))));
}

fn semantic_actions_are_safe_by_default_and_preserve_authored_danger() {
    let mut fixture = Fixture::new();
    let described = Element::column()
        .child(Element::button("Keep", EventId::new("keep")))
        .child(
            Element::button("Delete", EventId::new("delete"))
                .prop(hl_gui::Prop::Destructive, hl_gui::PropValue::Flag(true)),
        );
    fixture.describe(&described);
    fixture.page.tick();

    let tree = fixture.page.semantics("pane-1").expect("semantic snapshot");
    let actions = &tree.root.children[0].children;
    assert!(!actions[0].destructive, "ordinary actions remain safe by default");
    assert!(actions[1].destructive, "authored danger reaches semantic clients");
}

fn a_structured_fault_reaches_lifecycle_on_the_toolkit_tick() {
    let (post, deliveries) = channel();
    let seen = Rc::new(RefCell::new(Vec::new()));
    let carried = Rc::clone(&seen);
    let (_widget, mut page) = Interface::with_faults(
        deliveries,
        Rc::new(|_| {}),
        Rc::new(move |count| {
            carried.borrow_mut().push(count);
        }),
    );

    post.send(Delivery::Fault { restarts: 5 }).expect("page listening");
    assert!(
        seen.borrow().is_empty(),
        "the host thread never calls GTK lifecycle directly"
    );
    page.tick();
    assert_eq!(&*seen.borrow(), &[5]);
}

fn a_queued_frame_puts_widgets_on_the_page() {
    let mut fixture = Fixture::new();
    let empty = fixture.page.surface().len();

    fixture.describe(&panel("Containers"));
    let applied = fixture.page.tick();

    assert_eq!(applied, 1, "one queued frame, one applied delivery");
    assert!(
        fixture.page.surface().len() > empty,
        "the surface holds {} widgets",
        fixture.page.surface().len()
    );
    assert!(fixture.tagged(Tag::Button).is_some(), "the button reached the page");
    assert!(!fixture.page.banner().is_visible(), "a live extension shows no banner");
}

fn an_identical_frame_changes_nothing() {
    let mut fixture = Fixture::new();
    fixture.describe(&panel("Containers"));
    fixture.page.tick();
    let widgets = fixture.page.surface().len();
    let live = fixture.widgets().len();

    fixture.describe(&panel("Containers"));
    fixture.page.tick();

    assert_eq!(
        fixture.page.surface().len(),
        widgets,
        "no widget was created or destroyed"
    );
    assert_eq!(fixture.widgets().len(), live, "the widget tree is untouched");
}

fn a_burst_beyond_the_tick_bound_stays_queued() {
    let mut fixture = Fixture::new();
    let burst = DRAIN + 3;
    for index in 0..burst {
        fixture.describe(&panel(&format!("Containers {index}")));
    }

    let first = fixture.page.tick();
    let second = fixture.page.tick();

    assert_eq!(first, DRAIN, "a tick applies at most the bound");
    assert_eq!(second, burst - DRAIN, "the remainder was queued, not dropped");
    assert!(
        !fixture.page.banner().is_visible(),
        "every frame applied in order: {}",
        fixture.page.banner().text()
    );
}

fn a_stopped_extension_keeps_its_widgets_and_says_so() {
    let mut fixture = Fixture::new();
    fixture.describe(&panel("Containers"));
    fixture.page.tick();
    let widgets = fixture.page.surface().len();
    let live = fixture.widgets().len();

    fixture
        .post
        .send(Delivery::Loss("the socket closed".into()))
        .expect("the page is listening");
    fixture.page.tick();

    assert_eq!(
        fixture.page.surface().len(),
        widgets,
        "the surface is retained, not blanked"
    );
    assert!(fixture.widgets().len() >= live, "the last interface is still on screen");
    assert!(fixture.tagged(Tag::Button).is_some(), "including its widgets");
    assert!(fixture.page.banner().is_visible(), "the banner explains the stop");
    assert!(
        fixture.page.banner().text().contains("the socket closed"),
        "the banner says why, got {:?}",
        fixture.page.banner().text()
    );
    let retry = fixture
        .widgets()
        .into_iter()
        .find(|widget| widget.has_css_class("hl-extension-retry"))
        .expect("the banner offers a retry action")
        .downcast::<gtk::Button>()
        .expect("a button");
    retry.emit_clicked();
    assert_eq!(
        fixture.recorded.borrow().as_slice(),
        [Signal::Retry],
        "retry reaches the sink"
    );
}

fn a_rendered_button_reaches_the_sink() {
    let mut fixture = Fixture::new();
    fixture.describe(&panel("Containers"));
    fixture.page.tick();

    let button = fixture
        .tagged(Tag::Button)
        .expect("the button is reachable through its style class")
        .downcast::<gtk::Button>()
        .expect("a button tag builds a button");
    button.emit_clicked();
    fixture.page.tick();

    let recorded = fixture.recorded.borrow();
    assert!(
        recorded.iter().any(|signal| matches!(
            signal,
            Signal::Interaction(Event::Invoke { id, .. }) if id.as_str() == "restart"
        )),
        "the click reached the sink, got {recorded:?}"
    );
}
