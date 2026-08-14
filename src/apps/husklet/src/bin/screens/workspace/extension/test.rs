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

/// Every scenario runs inside one test.
///
/// GTK may only be initialized from a single thread and the libtest harness
/// gives every `#[test]` its own, so a second GTK test in the same binary
/// either panics or silently skips. One test running the scenarios in sequence
/// is the only shape in which all of them actually run.
#[test]
fn an_extension_page_renders_what_is_queued_and_survives_the_extension() {
    if gtk::init().is_err() {
        eprintln!("skipped: no display connection, so the extension page cannot be rendered");
        return;
    }
    a_queued_frame_puts_widgets_on_the_page();
    an_identical_frame_changes_nothing();
    a_burst_beyond_the_tick_bound_stays_queued();
    a_stopped_extension_keeps_its_widgets_and_says_so();
    a_rendered_button_reaches_the_sink();
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
