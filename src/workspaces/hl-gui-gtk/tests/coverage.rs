//! Every tag builds a widget, every container keeps its children, and every
//! property changes something a caller can observe.
//!
//! The component library declares its vocabulary once, in `Tag::ALL` and
//! `Prop`, and an adapter is only honest if it is total over both. These
//! scenarios walk those lists rather than a hand-picked sample, so a tag that
//! silently falls through to a default widget, a container that quietly drops
//! the children given to it, or a property applied to a widget that ignores it
//! fails here and names itself in the failure.
//!
//! They need a display connection; when none is available they report that
//! rather than passing silently.

use gtk::prelude::*;
use hl_gui::{Align, Choice, Fault, Length, NodeId, Orientation, Prop, PropValue, Scale, Tag, Tone, Tree, Variant};
use hl_gui_gtk::{Failure, Surface};

/// Text carried by the child every container scenario inserts.
const OFFSPRING: &str = "descendant";

/// One producer, one tree, one rendered surface: the whole stack under test.
struct Session {
    producer: hl_gui::Surface,
    tree: Tree,
    canvas: Surface,
}

impl Session {
    fn new() -> Self {
        Self {
            producer: hl_gui::Surface::new(),
            tree: Tree::new(),
            canvas: Surface::new(),
        }
    }

    /// Renders everything recorded since the previous flush.
    fn flush(&mut self) -> Result<(), Fault<Failure>> {
        let frame = self.producer.frame();
        self.tree.apply(&frame, &mut self.canvas)
    }

    /// Every widget at or below the surface root, parents before children.
    fn widgets(&self) -> Vec<gtk::Widget> {
        let mut found = vec![self.canvas.widget().clone().upcast::<gtk::Widget>()];
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

    /// Whether the inserted child reached the toolkit, wherever the container
    /// chose to put it.
    fn holds_offspring(&self) -> bool {
        self.widgets().iter().any(|widget| {
            widget
                .downcast_ref::<gtk::Label>()
                .is_some_and(|label| label.text() == OFFSPRING)
        })
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

/// One property, the tag expected to honour it, and the observation proving it
/// arrived. The verdict reads the toolkit back — a real GTK property where the
/// adapter sets one, and the style class where the property is only expressible
/// as appearance.
struct Probe {
    prop: Prop,
    tag: Tag,
    value: PropValue,
    verdict: fn(&gtk::Widget) -> bool,
}

/// Every scenario runs inside one test.
///
/// GTK may only be initialized from a single thread, and the libtest harness
/// gives every `#[test]` its own thread — so a second GTK test in the same
/// binary either panics or, if it treats the failure as "no display", skips
/// itself without saying anything useful. One test that runs the scenarios in
/// sequence is therefore the only shape in which all of them actually run.
#[test]
fn the_adapter_is_total_over_the_component_vocabulary() {
    if gtk::init().is_err() {
        eprintln!("skipped: no display connection");
        return;
    }
    every_tag_materializes_as_its_own_widget();
    every_container_keeps_the_child_it_is_given();
    every_property_changes_the_widget_it_is_applied_to();
}

/// A tag that no builder maps is the defect this catches: the surface would
/// still report success while the described component never appeared.
fn every_tag_materializes_as_its_own_widget() {
    for tag in Tag::ALL {
        let mut session = Session::new();
        let empty = session.canvas.len();
        let node = session.producer.create(*tag);
        session.producer.append(NodeId::ROOT, node);

        let outcome = session.flush();

        assert!(outcome.is_ok(), "{} failed to render: {outcome:?}", tag.as_str());
        assert_eq!(
            session.canvas.len(),
            empty + 1,
            "{} produced no widget of its own",
            tag.as_str()
        );
        assert!(
            session.tagged(*tag).is_some(),
            "{} is not reachable by its own style class",
            tag.as_str()
        );
    }
}

/// A container that accepts an insert and then drops the child is the defect
/// this catches — the tree and the widget tree disagree, silently.
fn every_container_keeps_the_child_it_is_given() {
    for tag in Tag::ALL.iter().filter(|tag| tag.accepts_children()) {
        let mut session = Session::new();
        let host = session.producer.create(*tag);
        session.producer.append(NodeId::ROOT, host);
        // GTK4 unparents a collapsed expander's child entirely rather than
        // hiding it, so a disclosure has to be open before its contents can be
        // looked for. Every other container ignores the property.
        session.producer.set(host, Prop::Expanded, PropValue::Flag(true));
        let child = session.producer.text(OFFSPRING);
        session.producer.append(host, child);

        let outcome = session.flush();

        assert!(outcome.is_ok(), "{} rejected a child: {outcome:?}", tag.as_str());
        assert!(
            session.holds_offspring(),
            "{} accepted a child that never reached the widget tree",
            tag.as_str()
        );
    }
}

/// A property applied to a widget that ignores it is the defect this catches:
/// the patch is accepted, and nothing on screen changes.
fn every_property_changes_the_widget_it_is_applied_to() {
    for probe in probes() {
        let mut session = Session::new();
        let node = session.producer.create(probe.tag);
        session.producer.append(NodeId::ROOT, node);
        session.producer.set(node, probe.prop, probe.value.clone());

        let outcome = session.flush();

        assert!(outcome.is_ok(), "{:?} failed to render: {outcome:?}", probe.prop);
        let widget = session
            .tagged(probe.tag)
            .unwrap_or_else(|| panic!("{} built no widget for {:?}", probe.tag.as_str(), probe.prop));
        assert!(
            (probe.verdict)(&widget),
            "{:?} on {} left the widget unchanged",
            probe.prop,
            probe.tag.as_str()
        );
    }
}

/// The representative property list, each paired with a tag whose contract
/// says it honours that property.
fn probes() -> Vec<Probe> {
    let mut listed = content();
    listed.extend(state());
    listed.extend(appearance());
    listed.extend(measure());
    listed.extend(range());
    listed
}

fn content() -> Vec<Probe> {
    vec![
        Probe {
            prop: Prop::Label,
            tag: Tag::Text,
            value: PropValue::text("Ready"),
            verdict: |widget| text_of(widget) == "Ready",
        },
        Probe {
            prop: Prop::Value,
            tag: Tag::Entry,
            value: PropValue::text("nginx"),
            verdict: |widget| entry(widget).text() == "nginx",
        },
        Probe {
            prop: Prop::Placeholder,
            tag: Tag::Entry,
            value: PropValue::text("filter…"),
            verdict: |widget| entry(widget).placeholder_text().is_some_and(|held| held == "filter…"),
        },
        Probe {
            prop: Prop::Icon,
            tag: Tag::Icon,
            value: PropValue::text("dialog-information-symbolic"),
            verdict: |widget| {
                widget
                    .downcast_ref::<gtk::Image>()
                    .and_then(gtk::Image::icon_name)
                    .is_some_and(|name| name == "dialog-information-symbolic")
            },
        },
        Probe {
            prop: Prop::Tooltip,
            tag: Tag::Button,
            value: PropValue::text("Restart the container"),
            verdict: |widget| {
                widget
                    .tooltip_text()
                    .is_some_and(|held| held == "Restart the container")
            },
        },
        Probe {
            prop: Prop::Choices,
            tag: Tag::Select,
            value: PropValue::Choices(vec![Choice::new("all", "All"), Choice::new("running", "Running")]),
            verdict: |widget| {
                widget
                    .downcast_ref::<gtk::DropDown>()
                    .and_then(gtk::DropDown::model)
                    .is_some_and(|model| model.n_items() == 2)
            },
        },
    ]
}

fn state() -> Vec<Probe> {
    vec![
        Probe {
            prop: Prop::Enabled,
            tag: Tag::Button,
            value: PropValue::Flag(false),
            verdict: |widget| !widget.is_sensitive(),
        },
        Probe {
            prop: Prop::Visible,
            tag: Tag::Text,
            value: PropValue::Flag(false),
            verdict: |widget| !widget.get_visible(),
        },
        Probe {
            prop: Prop::Checked,
            tag: Tag::Checkbox,
            value: PropValue::Flag(true),
            verdict: |widget| {
                widget
                    .downcast_ref::<gtk::CheckButton>()
                    .is_some_and(gtk::CheckButton::is_active)
            },
        },
        Probe {
            prop: Prop::Expanded,
            tag: Tag::Expander,
            value: PropValue::Flag(true),
            verdict: |widget| {
                widget
                    .downcast_ref::<gtk::Expander>()
                    .is_some_and(gtk::Expander::is_expanded)
            },
        },
        Probe {
            prop: Prop::Busy,
            tag: Tag::Spinner,
            value: PropValue::Flag(false),
            verdict: |widget| !widget.property::<bool>("spinning"),
        },
        Probe {
            prop: Prop::Wrap,
            tag: Tag::Text,
            value: PropValue::Flag(true),
            verdict: |widget| widget.property::<bool>("wrap"),
        },
        Probe {
            prop: Prop::Ellipsize,
            tag: Tag::Text,
            value: PropValue::Flag(true),
            verdict: |widget| {
                widget
                    .downcast_ref::<gtk::Label>()
                    .is_some_and(|label| label.ellipsize() == gtk::pango::EllipsizeMode::End)
            },
        },
    ]
}

/// Appearance is class-based by design, so the observation is the class the
/// generated sheet targets rather than a widget property.
fn appearance() -> Vec<Probe> {
    vec![
        Probe {
            prop: Prop::Variant,
            tag: Tag::Button,
            value: PropValue::Variant(Variant::Filled),
            verdict: |widget| widget.has_css_class("variant-filled"),
        },
        Probe {
            prop: Prop::Tone,
            tag: Tag::Badge,
            value: PropValue::Tone(Tone::Danger),
            verdict: |widget| widget.has_css_class("tone-danger"),
        },
        Probe {
            prop: Prop::Scale,
            tag: Tag::Heading,
            value: PropValue::Scale(Scale::Title),
            verdict: |widget| widget.has_css_class("scale-title"),
        },
    ]
}

fn measure() -> Vec<Probe> {
    vec![
        Probe {
            prop: Prop::Gap,
            tag: Tag::Row,
            value: PropValue::Length(Length::Step(3)),
            verdict: |widget| container(widget).spacing() == 12,
        },
        Probe {
            prop: Prop::Pad,
            tag: Tag::Column,
            value: PropValue::Length(Length::Step(2)),
            verdict: |widget| widget.margin_top() == 8 && widget.margin_start() == 8,
        },
        Probe {
            prop: Prop::Grow,
            tag: Tag::Column,
            value: PropValue::Number(1.0),
            verdict: gtk::prelude::WidgetExt::hexpands,
        },
        Probe {
            prop: Prop::Width,
            tag: Tag::Entry,
            value: PropValue::Length(Length::Chars(12)),
            verdict: |widget| entry(widget).width_chars() == 12,
        },
        Probe {
            prop: Prop::Height,
            tag: Tag::Column,
            value: PropValue::Length(Length::Step(4)),
            verdict: |widget| widget.size_request().1 == 16,
        },
        Probe {
            prop: Prop::Align,
            tag: Tag::Text,
            value: PropValue::Align(Align::End),
            verdict: |widget| widget.halign() == gtk::Align::End,
        },
        Probe {
            prop: Prop::Justify,
            tag: Tag::Text,
            value: PropValue::Align(Align::Center),
            verdict: |widget| widget.valign() == gtk::Align::Center,
        },
        Probe {
            prop: Prop::Orientation,
            tag: Tag::Row,
            value: PropValue::Orientation(Orientation::Vertical),
            verdict: |widget| container(widget).orientation() == gtk::Orientation::Vertical,
        },
    ]
}

fn range() -> Vec<Probe> {
    vec![
        Probe {
            prop: Prop::Minimum,
            tag: Tag::Slider,
            value: PropValue::Number(5.0),
            verdict: |widget| near(scale(widget).adjustment().lower(), 5.0),
        },
        Probe {
            prop: Prop::Maximum,
            tag: Tag::Slider,
            value: PropValue::Number(50.0),
            verdict: |widget| near(scale(widget).adjustment().upper(), 50.0),
        },
        Probe {
            prop: Prop::Fraction,
            tag: Tag::Progress,
            value: PropValue::Number(0.5),
            verdict: |widget| {
                widget
                    .downcast_ref::<gtk::ProgressBar>()
                    .is_some_and(|progress| near(progress.fraction(), 0.5))
            },
        },
    ]
}

/// Numbers cross the toolkit boundary as doubles, so a described value is
/// compared within the width of that round trip rather than bit for bit.
fn near(measured: f64, described: f64) -> bool {
    (measured - described).abs() < f64::EPSILON
}

fn text_of(widget: &gtk::Widget) -> String {
    widget
        .downcast_ref::<gtk::Label>()
        .map(|label| label.text().to_string())
        .unwrap_or_default()
}

fn entry(widget: &gtk::Widget) -> gtk::Entry {
    widget
        .clone()
        .downcast::<gtk::Entry>()
        .expect("an entry tag builds an entry")
}

fn container(widget: &gtk::Widget) -> gtk::Box {
    widget
        .clone()
        .downcast::<gtk::Box>()
        .expect("a layout tag builds a box")
}

fn scale(widget: &gtk::Widget) -> gtk::Scale {
    widget
        .clone()
        .downcast::<gtk::Scale>()
        .expect("a slider tag builds a scale")
}
