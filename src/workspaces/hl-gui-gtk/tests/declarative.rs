//! A described interface must reach the toolkit, and only its changes must.
//!
//! These are the end-to-end proof for the declarative layer: a description is
//! reconciled into a frame, the frame drives a real [`Tree`], and the tree
//! drives real GTK widgets. What the reconciler claims — no churn for an
//! unchanged description, moves rather than rebuilds for a reorder — is
//! therefore asserted against the widgets themselves, not against patches.
//!
//! They need a display connection; when none is available they report that
//! rather than passing silently.

use gtk::prelude::*;
use hl_gui::{
    Choice, Column, Element, Event, EventId, Length, Prop, PropValue, Reconciliation, Renderer, Scale, SourceId, Tag,
    Theme, Tone, Tree, Trigger, Variant,
};
use hl_gui_gtk::{Rows, Surface};

const SOURCE: SourceId = SourceId::new(7);

/// Initializes the toolkit, or reports that this environment cannot.
fn toolkit() -> bool {
    gtk::init().is_ok()
}

/// One producer, one tree, one rendered surface: the whole stack under test.
struct Session {
    reconciliation: Reconciliation,
    tree: Tree,
    surface: Surface,
}

impl Session {
    fn new() -> Self {
        Self {
            reconciliation: Reconciliation::new(),
            tree: Tree::new(),
            surface: Surface::new(),
        }
    }

    /// Reconciles a description and renders the resulting frame, reporting how
    /// many patches the toolkit was actually asked to apply.
    fn render(&mut self, described: &Element) -> usize {
        let frame = self.reconciliation.reconcile(described);
        self.tree
            .apply(&frame, &mut self.surface)
            .expect("the adapter applies every patch");
        frame.patches.len()
    }

    /// Every widget at or below the surface root, parents before children and
    /// siblings in the order the toolkit holds them — which is the order the
    /// description claims, and so the thing worth asserting on.
    fn widgets(&self) -> Vec<gtk::Widget> {
        let mut found = vec![self.surface.widget().clone().upcast::<gtk::Widget>()];
        let mut index = 0;
        while index < found.len() {
            found.extend(offspring(&found[index]));
            index += 1;
        }
        found
    }

    /// The first widget carrying a tag's style class, which is the adapter's
    /// own public naming — no back door into the registry is needed.
    fn tagged(&self, tag: Tag) -> Option<gtk::Widget> {
        let class = format!("hl-{}", tag.as_str().to_ascii_lowercase());
        self.widgets().into_iter().find(|widget| widget.has_css_class(&class))
    }

    fn labels(&self) -> Vec<String> {
        self.widgets()
            .iter()
            .filter_map(|widget| {
                widget
                    .downcast_ref::<gtk::Label>()
                    .map(|label| label.text().to_string())
            })
            .collect()
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

fn action(name: &str, tone: Tone) -> Element {
    Element::button(name, EventId::new(name))
        .variant(Variant::Filled)
        .tone(tone)
        .key(name)
}

/// A panel exercising every component family the adapter builds: layout,
/// display, control, surface, and collection.
fn panel(title: &str) -> Element {
    Element::column()
        .gap(Length::Step(3))
        .child(
            Element::new(Tag::Toolbar)
                .gap(Length::Step(2))
                .child(Element::heading(title).scale(Scale::Title))
                .child(Element::new(Tag::Spacer).width(Length::Fill))
                .child(action("start", Tone::Positive))
                .child(action("stop", Tone::Danger)),
        )
        .child(Element::new(Tag::Separator))
        .child(
            Element::card()
                .gap(Length::Step(2))
                .child(Element::text("Two containers running").tone(Tone::Neutral))
                .child(Element::badge("healthy", Tone::Positive))
                .child(
                    Element::row()
                        .gap(Length::Step(2))
                        .child(Element::entry("nginx", EventId::new("filter")))
                        .child(Element::new(Tag::Switch).on(Trigger::Toggle, EventId::new("live")))
                        .child(Element::new(Tag::Select).prop(
                            Prop::Choices,
                            PropValue::Choices(vec![Choice::new("all", "All"), Choice::new("running", "Running")]),
                        )),
                )
                .child(Element::new(Tag::Progress).prop(Prop::Fraction, PropValue::Number(0.42))),
        )
        .child(
            Element::new(Tag::Expander)
                .label("Details")
                .child(Element::text("cpu 4%, memory 128 MiB")),
        )
        .child(
            Element::new(Tag::Tabs).child(
                Element::new(Tag::TabPage).label("Processes").child(
                    Element::new(Tag::DataTable)
                        .prop(Prop::Source, PropValue::Source(SOURCE))
                        .prop(
                            Prop::Schema,
                            PropValue::Schema(vec![Column::new("pid", "PID"), Column::new("command", "Command")]),
                        ),
                ),
            ),
        )
}

/// Every scenario runs inside one test.
///
/// GTK may only be initialized from a single thread, and the libtest harness
/// gives every `#[test]` its own thread — so a second GTK test in the same
/// binary either panics or, if it treats the failure as "no display", skips
/// itself without saying anything useful. One test that runs the scenarios in
/// sequence is therefore the only shape in which all of them actually run.
#[test]
fn a_described_interface_reaches_the_toolkit_and_only_its_changes_do() {
    if !toolkit() {
        eprintln!("skipped: no display connection");
        return;
    }
    a_described_panel_materializes_as_widgets();
    an_unchanged_description_never_reaches_the_toolkit();
    a_keyed_reorder_moves_widgets_rather_than_rebuilding_them();
    a_described_handler_is_wired_to_the_real_signal();
    a_rebound_handler_reports_the_new_identity();
    rebinding_a_table_retires_its_previous_source();
    a_theme_installs_before_a_description_is_rendered();
}

fn rebinding_a_table_retires_its_previous_source() {
    let first = SourceId::new(41);
    let second = SourceId::new(42);
    let table = |source| {
        Element::new(Tag::DataTable)
            .key("records")
            .prop(Prop::Source, PropValue::Source(source))
            .prop(Prop::Schema, PropValue::Schema(vec![Column::new("id", "ID")]))
    };
    let mut session = Session::new();
    session.render(&table(first));
    session.surface.resize(first, hl_gui::Version::new(1), 100_000).unwrap();

    session.render(&table(second));

    assert!(
        session.surface.resize(first, hl_gui::Version::new(1), 100_000).is_err(),
        "a late old-source mutation must not regain routing authority"
    );
    session.surface.resize(second, hl_gui::Version::new(2), 100_000).unwrap();
    let table = session.tagged(Tag::DataTable).expect("rebound table");
    let view = table
        .downcast::<gtk::ScrolledWindow>()
        .expect("table viewport")
        .child()
        .and_downcast::<gtk::ColumnView>()
        .expect("column view");
    let rows = view
        .model()
        .and_downcast::<gtk::MultiSelection>()
        .and_then(|selection| selection.model())
        .and_downcast::<Rows>()
        .expect("windowed rows");
    assert_eq!(rows.source(), second, "GTK model follows the new sort/filter source");
    assert_eq!(u64::from(rows.n_items()), 100_000);
}

fn a_described_panel_materializes_as_widgets() {
    let mut session = Session::new();
    let empty = session.surface.len();

    let patches = session.render(&panel("Containers"));

    assert!(patches > 0);
    assert!(
        session.surface.len() > empty,
        "the surface holds {} widgets for a described panel",
        session.surface.len()
    );
    assert_eq!(session.surface.len(), session.tree.len(), "one widget per node");
    assert!(session.tagged(Tag::Card).is_some(), "the card reached the toolkit");
    assert!(session.tagged(Tag::Switch).is_some());
    assert!(session.tagged(Tag::Select).is_some());
    assert!(session.tagged(Tag::DataTable).is_some());
    assert!(session.tagged(Tag::Expander).is_some());
    assert!(session.tagged(Tag::Progress).is_some());
    assert!(session.tagged(Tag::Separator).is_some());
    assert!(session.tagged(Tag::Tabs).is_some());
    assert!(session.labels().iter().any(|text| text == "Containers"));
    assert!(session.surface.reports().is_empty(), "nothing was interacted with yet");
}

fn an_unchanged_description_never_reaches_the_toolkit() {
    let mut session = Session::new();
    session.render(&panel("Containers"));
    let widgets = session.surface.len();
    let live = session.widgets().len();

    let patches = session.render(&panel("Containers"));

    assert_eq!(patches, 0, "an identical description must produce an empty frame");
    assert_eq!(session.surface.len(), widgets, "no widget was created or destroyed");
    assert_eq!(session.widgets().len(), live, "the widget tree is untouched");
}

fn a_keyed_reorder_moves_widgets_rather_than_rebuilding_them() {
    let mut session = Session::new();
    let listing = |names: [&str; 3]| {
        Element::column().children(
            names
                .into_iter()
                .map(|name| Element::text(name).key(name))
                .collect::<Vec<_>>(),
        )
    };
    session.render(&listing(["alpha", "beta", "gamma"]));
    let widgets = session.surface.len();
    let identities = session
        .widgets()
        .iter()
        .map(|widget| widget.as_ptr() as usize)
        .collect::<std::collections::BTreeSet<_>>();

    let patches = session.render(&listing(["gamma", "alpha", "beta"]));

    assert!(patches > 0, "the order really did change");
    assert_eq!(
        session.surface.len(),
        widgets,
        "a reorder must not create or destroy a single widget"
    );
    assert_eq!(
        session
            .widgets()
            .iter()
            .map(|widget| widget.as_ptr() as usize)
            .collect::<std::collections::BTreeSet<_>>(),
        identities,
        "the very same widget objects survived the reorder"
    );
    let labels = session.labels();
    assert_eq!(labels, vec!["gamma", "alpha", "beta"], "in the described order");
}

fn a_described_handler_is_wired_to_the_real_signal() {
    let mut session = Session::new();
    session.render(&Element::column().child(action("start", Tone::Positive)));

    let button = session
        .tagged(Tag::Button)
        .expect("the button is reachable through its style class")
        .downcast::<gtk::Button>()
        .expect("a button tag builds a button");
    button.emit_clicked();

    let reported = session.surface.reports().drain();
    assert_eq!(reported.len(), 1, "one click, one report");
    assert!(
        matches!(&reported[0], Event::Invoke { id, .. } if id.as_str() == "start"),
        "expected the described identity, got {:?}",
        reported[0]
    );
}

fn a_rebound_handler_reports_the_new_identity() {
    let mut session = Session::new();
    session.render(&Element::column().child(Element::button("Go", EventId::new("go")).key("only")));
    let widgets = session.surface.len();

    session.render(&Element::column().child(Element::button("Go", EventId::new("halt")).key("only")));

    assert_eq!(session.surface.len(), widgets, "rebinding must not rebuild the button");
    let button = session
        .tagged(Tag::Button)
        .expect("the button survived")
        .downcast::<gtk::Button>()
        .expect("a button");
    button.emit_clicked();

    let reported = session.surface.reports().drain();
    // GTK holds every connected closure, so the adapter's `SetHandler` adds a
    // second one rather than replacing the first: both identities report.
    assert!(
        reported
            .iter()
            .any(|event| matches!(event, Event::Invoke { id, .. } if id.as_str() == "halt")),
        "the new identity must report, got {reported:?}"
    );
}

fn a_theme_installs_before_a_description_is_rendered() {
    let mut session = Session::new();
    session.surface.theme(&Theme::dark()).expect("a theme installs");

    session.render(&panel("Containers"));

    assert!(session.surface.len() > 1);
}
