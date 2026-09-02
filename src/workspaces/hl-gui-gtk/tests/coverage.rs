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
    markdown_is_safe_selectable_and_structured();
    json_is_selectable_string_safe_and_depth_bounded();
    every_tag_materializes_as_its_own_widget();
    every_container_keeps_the_child_it_is_given();
    every_declared_property_changes_the_component_that_declares_it();
    every_tag_honours_the_property_it_is_for();
    every_part_lands_in_the_slot_its_parent_keeps();
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

/// A component that declares a property the adapter ignores is the defect this
/// catches, and it is checked for every component and every property it
/// declares rather than for a chosen sample.
///
/// The observation is the whole rendered widget tree, read back from the
/// toolkit: every readable GTK property of every widget the component built,
/// its style classes, the text its buffers hold, the adjustment behind a
/// control and the grid cell it occupies. A described component is rendered
/// twice — once plain, once carrying the property — and the property is only
/// honoured if the toolkit reports something different. Nothing is asserted
/// about *which* difference, because that is what the scenario below this one
/// is for; what is asserted here is that accepting the property was not a
/// silent no-op.
fn every_declared_property_changes_the_component_that_declares_it() {
    let ignored: Vec<String> = Tag::ALL.iter().flat_map(|tag| unhonoured(*tag)).collect();
    assert!(
        ignored.is_empty(),
        "the adapter accepts declared properties and changes nothing:\n{}",
        ignored.join("\n")
    );
}

/// Everything one component declares and the adapter does not do.
///
/// The whole list rather than the first: a contract is fixed by reading every
/// gap at once, not by discovering them one test run at a time.
fn unhonoured(tag: Tag) -> Vec<String> {
    let mut ignored = Vec::new();
    assert_eq!(
        portrait(tag, Prop::Label, None),
        portrait(tag, Prop::Label, None),
        "{} renders differently from one description, so nothing read back from it means anything",
        tag.as_str()
    );
    for prop in tag.props() {
        if unobservable(tag, *prop) {
            continue;
        }
        let plain = portrait(tag, *prop, None);
        let honoured = offers(*prop)
            .iter()
            .any(|value| portrait(tag, *prop, Some(value)) != plain);
        if !honoured {
            ignored.push(format!("  {} declares {prop:?}", tag.as_str()));
        }
    }
    ignored
}

/// The one pairing the toolkit will not let this scenario observe.
///
/// A popover is hidden until it is popped up, so being told to hide leaves it
/// exactly where it was, and telling it to show pops a surface up against a
/// window that does not exist in a test — which GTK does not survive. The
/// adapter does apply the property; there is no reading it back here, and
/// pretending otherwise by weakening the observation would hide real gaps in
/// every other component.
fn unobservable(tag: Tag, prop: Prop) -> bool {
    prop == Prop::Destructive || (prop == Prop::Visible && (tag == Tag::Popover || tag == Tag::ContextMenu))
}

/// One component, rendered with the property and without it, described down to
/// the last toolkit property.
fn portrait(tag: Tag, prop: Prop, value: Option<&PropValue>) -> String {
    let mut session = Session::new();
    let host = seat(&mut session, prop);
    let node = session.producer.create(tag);
    session.producer.append(host, node);
    fill(&mut session, node, tag);
    if let Some(value) = value {
        session.producer.set(node, prop, value.clone());
    }
    session.flush().expect("a declared property must render");
    session
        .widgets()
        .iter()
        .flat_map(subtree)
        .map(|widget| traced(&widget))
        .collect::<Vec<String>>()
        .join("\n")
}

/// Where the probed component is placed.
///
/// A cell is decided by the grid holding the child, so the two placement
/// properties are the only ones that need a grid to mean anything at all.
fn seat(session: &mut Session, prop: Prop) -> NodeId {
    if prop != Prop::Span && prop != Prop::RowSpan {
        return NodeId::ROOT;
    }
    let grid = session.producer.create(Tag::Grid);
    session.producer.append(NodeId::ROOT, grid);
    // A grid one column wide has no room to span: a child asking for two
    // columns is given the one that exists, whatever it described.
    session.producer.set(grid, Prop::Columns, PropValue::Integer(3));
    grid
}

/// Gives a container something to arrange, since a gap, a column count and a
/// wrapping axis are all invisible in an empty one.
fn fill(session: &mut Session, node: NodeId, tag: Tag) {
    if !tag.accepts_children() {
        return;
    }
    for _ in 0..2 {
        let child = session.producer.text(OFFSPRING);
        session.producer.append(node, child);
    }
}

/// One widget as the toolkit reports it.
fn traced(widget: &gtk::Widget) -> String {
    let mut written = vec![widget.type_().to_string(), widget.css_classes().join(".")];
    for spec in widget.list_properties() {
        if !spec.flags().contains(gtk::glib::ParamFlags::READABLE) || delegated(widget, spec.name()) {
            continue;
        }
        written.push(format!(
            "{}={}",
            spec.name(),
            shown(&widget.property_value(spec.name()))
        ));
    }
    written.push(held(widget));
    written.push(celled(widget));
    written.join(" ")
}

/// Whether a property is one a box only answers through the box layout it
/// happens to be running.
///
/// A wrapping container runs a layout of its own, and GTK answers these from
/// the box layout it no longer has — loudly, and with nothing. The layout
/// manager itself is in the description, so the swap is still visible.
fn delegated(widget: &gtk::Widget, name: &str) -> bool {
    const DEFERRED: [&str; 5] = [
        "orientation",
        "spacing",
        "homogeneous",
        "baseline-child",
        "baseline-position",
    ];
    let Some(manager) = widget.layout_manager() else {
        return false;
    };
    widget.is::<gtk::Box>() && !manager.is::<gtk::BoxLayout>() && DEFERRED.contains(&name)
}

/// One property value as text.
///
/// An object is described by what it is and how much it holds, never by where
/// it lives: two renderings of the same description allocate different objects,
/// so an address would report every component as changed by everything.
fn shown(value: &gtk::glib::Value) -> String {
    let Ok(object) = value.get::<Option<gtk::glib::Object>>() else {
        let written = format!("{value:?}");
        // A boxed value — a rectangle, a colour — is printed by address too,
        // and there is no reading it back generically, so it is described by
        // its type and left out of the comparison.
        if written.contains("0x") {
            return value.type_().to_string();
        }
        return written;
    };
    let Some(object) = object else {
        return "none".to_owned();
    };
    match object.dynamic_cast_ref::<gtk::gio::ListModel>() {
        Some(model) => format!("{} of {}", object.type_(), model.n_items()),
        None => object.type_().to_string(),
    }
}

/// The state a widget keeps in an object beside itself rather than in a
/// property of its own: what a view's buffer holds, and where a control's
/// adjustment stands.
fn held(widget: &gtk::Widget) -> String {
    if let Some(view) = widget.downcast_ref::<gtk::TextView>() {
        let buffer = view.buffer();
        return format!(
            "buffer {}",
            buffer.text(&buffer.start_iter(), &buffer.end_iter(), false)
        );
    }
    let Some(adjustment) = adjusted(widget) else {
        return String::new();
    };
    format!(
        "range {} {} {} {}",
        adjustment.lower(),
        adjustment.upper(),
        adjustment.value(),
        adjustment.step_increment()
    )
}

fn adjusted(widget: &gtk::Widget) -> Option<gtk::Adjustment> {
    if let Some(range) = widget.downcast_ref::<gtk::Range>() {
        return Some(range.adjustment());
    }
    widget
        .downcast_ref::<gtk::SpinButton>()
        .map(gtk::SpinButton::adjustment)
}

/// The cell a grid gives a child, which is kept by the grid rather than by the
/// child and so appears in no property of it.
fn celled(widget: &gtk::Widget) -> String {
    let Some(grid) = widget.parent().and_then(|parent| parent.downcast::<gtk::Grid>().ok()) else {
        return String::new();
    };
    let (column, row, width, height) = grid.query_child(widget);
    format!("cell {column},{row},{width},{height}")
}

/// The values one property is probed with.
///
/// More than one where a component may already be in the state the first would
/// put it in: a spinner is spinning before anything is described, and a notice
/// is revealed. Honouring the property is changing the widget for at least one
/// of them, never for a value the widget already stands at.
fn offers(prop: Prop) -> Vec<PropValue> {
    match prop {
        Prop::Label | Prop::Detail | Prop::Placeholder | Prop::Help | Prop::Tooltip => {
            vec![PropValue::text(TITLE)]
        }
        // A value is whatever the component holds, and a date and a time are
        // spelled the one way that is not this library's invention.
        Prop::Value => vec![
            PropValue::text(CONTENT),
            PropValue::Number(3.0),
            PropValue::text("2024-03-05"),
            PropValue::text("14:30"),
        ],
        Prop::Icon => vec![PropValue::text(EMBLEM)],
        Prop::Uri => vec![PropValue::text(REFERENCE)],
        Prop::Enabled | Prop::Visible => vec![PropValue::Flag(false)],
        Prop::Selected | Prop::Checked => vec![PropValue::Flag(true)],
        Prop::Expanded
        | Prop::Busy
        | Prop::Secret
        | Prop::Destructive
        | Prop::Monospace
        | Prop::Wrap
        | Prop::Ellipsize => {
            vec![PropValue::Flag(true), PropValue::Flag(false)]
        }
        _ => shaped(prop),
    }
}

/// The values of the properties whose shape is not text, a flag or a name.
fn shaped(prop: Prop) -> Vec<PropValue> {
    match prop {
        Prop::Variant => vec![PropValue::Variant(Variant::Filled)],
        Prop::Tone => vec![PropValue::Tone(Tone::Danger)],
        Prop::Scale => vec![PropValue::Scale(Scale::Title)],
        Prop::Color => vec![PropValue::Token(hl_gui::Token::Accent)],
        Prop::Gap | Prop::Pad => vec![PropValue::Length(Length::Step(3)), PropValue::Length(Length::Step(5))],
        Prop::Grow => vec![PropValue::Number(1.0), PropValue::Number(0.0)],
        Prop::Width | Prop::Height => vec![
            PropValue::Length(Length::Step(4)),
            PropValue::Length(Length::Step(7)),
            PropValue::Length(Length::Chars(12)),
        ],
        Prop::Align | Prop::Justify => vec![
            PropValue::Align(Align::End),
            PropValue::Align(Align::Center),
            PropValue::Align(Align::Start),
        ],
        Prop::Orientation => vec![
            PropValue::Orientation(Orientation::Vertical),
            PropValue::Orientation(Orientation::Horizontal),
        ],
        _ => numbered(prop),
    }
}

/// The values of the properties measured in numbers, rows or columns.
fn numbered(prop: Prop) -> Vec<PropValue> {
    match prop {
        Prop::Columns | Prop::Span | Prop::RowSpan => vec![PropValue::Integer(2), PropValue::Integer(3)],
        Prop::Position => vec![PropValue::Number(120.0)],
        Prop::Minimum => vec![PropValue::Number(5.0)],
        Prop::Maximum => vec![PropValue::Number(50.0)],
        Prop::Step => vec![PropValue::Number(2.0)],
        Prop::Fraction => vec![PropValue::Number(0.5)],
        Prop::Choices => vec![PropValue::Choices(vec![
            Choice::new("all", "All"),
            Choice::new("running", "Running"),
        ])],
        Prop::Schema => vec![PropValue::Schema(vec![
            hl_gui::Column::new("name", "Name"),
            hl_gui::Column::new("state", "State"),
        ])],
        Prop::Source => vec![PropValue::Source(hl_gui::SourceId::new(1))],
        // RowHeight is the one property no component declares, so no value is
        // ever asked for here.
        _ => vec![PropValue::Integer(2)],
    }
}

/// Numbers cross the toolkit boundary as doubles, so a described value is
/// compared within the width of that round trip rather than bit for bit.
fn near(measured: f64, described: f64) -> bool {
    (measured - described).abs() < f64::EPSILON
}

/// What a component is *for*: the one property it would be dishonest to accept
/// and ignore. Every tag names one, so a component that materializes an inert
/// widget is caught here rather than in an application.
#[derive(Clone, Copy, Debug)]
enum Aspect {
    Label,
    Value,
    Number,
    Icon,
    Uri,
    Fraction,
    Choices,
    Checked,
    /// Whether a surface has slid into view.
    Revealed,
    /// A count of whole stars.
    Stars,
    Busy,
    Gap,
    Grow,
    Orientation,
    Measure,
    Date,
    Time,
}

/// The text every label-shaped scenario writes, and reads back.
const TITLE: &str = "Ready";
/// The value every value-shaped scenario writes, and reads back.
const CONTENT: &str = "nginx";
/// The icon every icon-shaped scenario names.
const EMBLEM: &str = "dialog-information-symbolic";
/// The file every reference-shaped scenario names.
const REFERENCE: &str = "/var/lib/hl/sample.png";

/// A tag that accepts its own principal property and changes nothing is the
/// defect this catches: the component library would claim a component it does
/// not actually have.
fn every_tag_honours_the_property_it_is_for() {
    for tag in Tag::ALL {
        let aspect = principal(*tag);
        let mut session = Session::new();
        let node = session.producer.create(*tag);
        session.producer.append(NodeId::ROOT, node);
        session.producer.set(node, asked(aspect), written(aspect));

        let outcome = session.flush();

        assert!(outcome.is_ok(), "{} failed to render: {outcome:?}", tag.as_str());
        let widget = session
            .tagged(*tag)
            .unwrap_or_else(|| panic!("{} built no widget", tag.as_str()));
        assert!(
            honoured(&widget, aspect),
            "{} ignores {:?}, the property it exists for",
            tag.as_str(),
            asked(aspect)
        );
    }
}

fn asked(aspect: Aspect) -> Prop {
    match aspect {
        Aspect::Label => Prop::Label,
        Aspect::Value | Aspect::Number | Aspect::Stars | Aspect::Date | Aspect::Time => Prop::Value,
        Aspect::Revealed => Prop::Expanded,
        Aspect::Icon => Prop::Icon,
        Aspect::Uri => Prop::Uri,
        Aspect::Fraction => Prop::Fraction,
        Aspect::Choices => Prop::Choices,
        Aspect::Checked => Prop::Checked,
        Aspect::Busy => Prop::Busy,
        Aspect::Gap => Prop::Gap,
        Aspect::Grow => Prop::Grow,
        Aspect::Orientation => Prop::Orientation,
        Aspect::Measure => Prop::Width,
    }
}

fn written(aspect: Aspect) -> PropValue {
    match aspect {
        Aspect::Label => PropValue::text(TITLE),
        Aspect::Value => PropValue::text(CONTENT),
        Aspect::Number => PropValue::Number(42.0),
        Aspect::Icon => PropValue::text(EMBLEM),
        Aspect::Uri => PropValue::text(REFERENCE),
        Aspect::Fraction => PropValue::Number(0.5),
        Aspect::Choices => PropValue::Choices(vec![Choice::new("all", "All"), Choice::new("some", "Some")]),
        Aspect::Checked | Aspect::Revealed => PropValue::Flag(true),
        // Three of five stars: a rating is bounded, so the figure every other
        // number-shaped component is probed with would only prove clamping.
        Aspect::Stars => PropValue::Number(3.0),
        Aspect::Busy => PropValue::Flag(false),
        Aspect::Gap => PropValue::Length(Length::Step(3)),
        Aspect::Grow => PropValue::Number(1.0),
        Aspect::Orientation => PropValue::Orientation(Orientation::Vertical),
        Aspect::Measure => PropValue::Length(Length::Step(4)),
        Aspect::Date => PropValue::text("2024-03-05"),
        Aspect::Time => PropValue::text("14:30"),
    }
}

fn honoured(widget: &gtk::Widget, aspect: Aspect) -> bool {
    match aspect {
        Aspect::Label => shows(widget, TITLE),
        Aspect::Value => holds(widget, CONTENT),
        Aspect::Number => measured(widget, 42.0),
        Aspect::Icon => named(widget, EMBLEM),
        Aspect::Uri => referenced(widget),
        Aspect::Fraction => filled(widget),
        Aspect::Choices => offered(widget),
        Aspect::Checked => active(widget),
        Aspect::Revealed => revealed(widget),
        Aspect::Stars => measured(widget, 3.0),
        Aspect::Busy => !widget.property::<bool>("spinning"),
        Aspect::Gap => spaced(widget),
        Aspect::Grow => widget.hexpands(),
        Aspect::Orientation => upright(widget),
        Aspect::Measure => widget.size_request().0 == 16,
        Aspect::Date => dated(widget),
        Aspect::Time => timed(widget),
    }
}

/// Every widget at or below one widget, parents before children.
fn subtree(widget: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut found = vec![widget.clone()];
    let mut index = 0;
    while index < found.len() {
        found.extend(offspring(&found[index]));
        index += 1;
    }
    found
}

/// Whether the text became something a person can read, wherever the component
/// keeps its caption.
fn shows(widget: &gtk::Widget, text: &str) -> bool {
    subtree(widget).iter().any(|held| captioned(held, text))
}

fn captioned(widget: &gtk::Widget, text: &str) -> bool {
    if let Some(label) = widget.downcast_ref::<gtk::Label>() {
        return label.text() == text;
    }
    if let Some(button) = widget.downcast_ref::<gtk::Button>() {
        return button.label().is_some_and(|held| held == text);
    }
    if let Some(check) = widget.downcast_ref::<gtk::CheckButton>() {
        return check.label().is_some_and(|held| held == text);
    }
    if let Some(menu) = widget.downcast_ref::<gtk::MenuButton>() {
        return menu.label().is_some_and(|held| held == text);
    }
    chromed(widget, text)
}

fn chromed(widget: &gtk::Widget, text: &str) -> bool {
    if let Some(frame) = widget.downcast_ref::<gtk::Frame>() {
        return frame.label().is_some_and(|held| held == text);
    }
    if let Some(expander) = widget.downcast_ref::<gtk::Expander>() {
        return expander.label().is_some_and(|held| held == text);
    }
    widget.tooltip_text().is_some_and(|held| held == text)
}

/// Whether the value reached whatever the component holds a value in.
fn holds(widget: &gtk::Widget, text: &str) -> bool {
    subtree(widget).iter().any(|held| kept(held, text))
}

fn kept(widget: &gtk::Widget, text: &str) -> bool {
    if let Some(label) = widget.downcast_ref::<gtk::Label>() {
        return label.text() == text;
    }
    if let Some(entry) = widget.downcast_ref::<gtk::Entry>() {
        return entry.text() == text;
    }
    if let Some(entry) = widget.downcast_ref::<gtk::SearchEntry>() {
        return entry.text() == text;
    }
    if let Some(entry) = widget.downcast_ref::<gtk::PasswordEntry>() {
        return entry.text() == text;
    }
    if let Some(view) = widget.downcast_ref::<gtk::TextView>() {
        return written_text(view).contains(text);
    }
    captioned(widget, text)
}

fn written_text(view: &gtk::TextView) -> String {
    let buffer = view.buffer();
    buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string()
}

fn measured(widget: &gtk::Widget, number: f64) -> bool {
    subtree(widget).iter().any(|held| counted(held, number))
}

fn counted(widget: &gtk::Widget, number: f64) -> bool {
    if let Some(spin) = widget.downcast_ref::<gtk::SpinButton>() {
        return near(spin.value(), number);
    }
    let Some(scale) = widget.downcast_ref::<gtk::Scale>() else {
        return false;
    };
    near(scale.value(), number)
}

fn named(widget: &gtk::Widget, icon: &str) -> bool {
    subtree(widget).iter().any(|held| marked(held, icon))
}

fn marked(widget: &gtk::Widget, icon: &str) -> bool {
    if let Some(image) = widget.downcast_ref::<gtk::Image>() {
        return image.icon_name().is_some_and(|held| held == icon);
    }
    if let Some(button) = widget.downcast_ref::<gtk::Button>() {
        return button.icon_name().is_some_and(|held| held == icon);
    }
    let Some(menu) = widget.downcast_ref::<gtk::MenuButton>() else {
        return false;
    };
    menu.icon_name().is_some_and(|held| held == icon)
}

fn referenced(widget: &gtk::Widget) -> bool {
    if let Some(picture) = widget.downcast_ref::<gtk::Picture>() {
        return picture.file().is_some();
    }
    let Some(video) = widget.downcast_ref::<gtk::Video>() else {
        return false;
    };
    video.file().is_some()
}

fn filled(widget: &gtk::Widget) -> bool {
    if let Some(progress) = widget.downcast_ref::<gtk::ProgressBar>() {
        return near(progress.fraction(), 0.5);
    }
    let Some(level) = widget.downcast_ref::<gtk::LevelBar>() else {
        return false;
    };
    near(level.value(), 0.5)
}

fn offered(widget: &gtk::Widget) -> bool {
    if let Some(drop) = widget.downcast_ref::<gtk::DropDown>() {
        return drop.model().is_some_and(|model| model.n_items() == 2);
    }
    subtree(widget)
        .iter()
        .filter(|held| held.is::<gtk::CheckButton>())
        .count()
        == 2
}

fn active(widget: &gtk::Widget) -> bool {
    if let Some(switch) = widget.downcast_ref::<gtk::Switch>() {
        return switch.is_active();
    }
    let Some(check) = widget.downcast_ref::<gtk::CheckButton>() else {
        return false;
    };
    check.is_active()
}

fn revealed(widget: &gtk::Widget) -> bool {
    widget
        .downcast_ref::<gtk::Revealer>()
        .is_some_and(gtk::Revealer::reveals_child)
}

fn spaced(widget: &gtk::Widget) -> bool {
    if let Some(container) = widget.downcast_ref::<gtk::Box>() {
        return container.spacing() == 12;
    }
    let Some(grid) = widget.downcast_ref::<gtk::Grid>() else {
        return false;
    };
    grid.row_spacing() == 12
}

fn upright(widget: &gtk::Widget) -> bool {
    let Some(separator) = widget.downcast_ref::<gtk::Separator>() else {
        return false;
    };
    separator.orientation() == gtk::Orientation::Vertical
}

fn dated(widget: &gtk::Widget) -> bool {
    let Some(calendar) = widget.downcast_ref::<gtk::Calendar>() else {
        return false;
    };
    calendar.day() == 5 && calendar.month() == 2 && calendar.year() == 2024
}

fn timed(widget: &gtk::Widget) -> bool {
    let counters = subtree(widget)
        .into_iter()
        .filter_map(|held| held.downcast::<gtk::SpinButton>().ok())
        .collect::<Vec<_>>();
    let (Some(hours), Some(minutes)) = (counters.first(), counters.last()) else {
        return false;
    };
    near(hours.value(), 14.0) && near(minutes.value(), 30.0)
}

/// The property each component exists to honour.
///
/// Total over the catalogue on purpose: a new tag cannot be added without
/// saying what it is for, and saying it here is what makes the scenario above
/// run for it.
fn principal(tag: Tag) -> Aspect {
    match tag {
        Tag::Column | Tag::Row | Tag::Grid | Tag::Container => Aspect::Gap,
        Tag::Scroll | Tag::Splitter | Tag::Stack | Tag::Overlay | Tag::Spacer => Aspect::Grow,
        Tag::Separator | Tag::StepConnector => Aspect::Orientation,
        Tag::Card | Tag::Paper | Tag::CardHeader | Tag::CardActionArea => Aspect::Label,
        Tag::CardContent | Tag::CardActions | Tag::Section | Tag::Toolbar | Tag::Sidebar => Aspect::Gap,
        Tag::CardMedia | Tag::Image | Tag::ImageListItem | Tag::Video => Aspect::Uri,
        Tag::HeaderBar
        | Tag::ImageList
        | Tag::Tabs
        | Tag::List
        | Tag::DataTable
        | Tag::KeyValueTable
        | Tag::TreeTable
        | Tag::EventStream => Aspect::Grow,
        Tag::Tree | Tag::Drawer => Aspect::Grow,
        Tag::DrawerPanel => Aspect::Revealed,
        Tag::Rating => Aspect::Stars,
        Tag::TablePagination => Aspect::Value,
        Tag::Popover | Tag::ContextMenu | Tag::ColorPicker => Aspect::Grow,
        Tag::AvatarGroup => Aspect::Gap,
        Tag::Icon | Tag::IconButton | Tag::Fab | Tag::SpeedDial | Tag::Overflow => Aspect::Icon,
        Tag::ListItemIcon | Tag::StepIcon => Aspect::Icon,
        Tag::Progress | Tag::Meter => Aspect::Fraction,
        Tag::Spinner => Aspect::Busy,
        Tag::Skeleton => Aspect::Measure,
        Tag::Stat => Aspect::Value,
        Tag::Switch => Aspect::Checked,
        Tag::Select | Tag::Autocomplete | Tag::RadioGroup => Aspect::Choices,
        Tag::NumberEntry | Tag::Slider => Aspect::Number,
        Tag::DatePicker => Aspect::Date,
        Tag::TimePicker => Aspect::Time,
        Tag::Entry
        | Tag::Search
        | Tag::CommandPalette
        | Tag::TagInput
        | Tag::TextArea
        | Tag::PasswordEntry
        | Tag::TextField
        | Tag::CodeView
        | Tag::MarkdownView
        | Tag::JsonView
        | Tag::LogView => Aspect::Value,
        _ => structural(tag),
    }
}

fn markdown_is_safe_selectable_and_structured() {
    let mut session = Session::new();
    let document = session.producer.create(Tag::MarkdownView);
    session.producer.append(NodeId::ROOT, document);
    session.producer.set(
        document,
        Prop::Value,
        PropValue::text("# Release <unsafe>\n- bounded\n```\nlet x = 1;\n```"),
    );
    session.flush().expect("markdown renders");
    let scroller = session.tagged(Tag::MarkdownView).expect("markdown widget");
    let label = subtree(&scroller)
        .into_iter()
        .find_map(|child| child.downcast::<gtk::Label>().ok())
        .expect("markdown owns a text label");
    assert!(label.is_selectable(), "document text must be copyable");
    assert_eq!(label.text(), "Release <unsafe>\n• bounded\nlet x = 1;");
    assert!(!label.text().contains("```"), "fence syntax is presentation, not content");
}

fn json_is_selectable_string_safe_and_depth_bounded() {
    let mut session = Session::new();
    let document = session.producer.create(Tag::JsonView);
    session.producer.append(NodeId::ROOT, document);
    session.producer.set(
        document,
        Prop::Value,
        PropValue::text(r#"{"message":"{literal},:[]","items":[1,2]}"#),
    );
    session.flush().expect("json renders");
    let widget = session.tagged(Tag::JsonView).expect("json widget");
    let view = subtree(&widget)
        .into_iter()
        .find_map(|child| child.downcast::<gtk::TextView>().ok())
        .expect("json owns a text view");
    assert!(!view.is_editable());
    assert!(view.is_monospace());
    let rendered = written_text(&view);
    assert!(rendered.contains("\"{literal},:[]\""), "punctuation inside strings is untouched");
    assert!(rendered.contains("\n  \"items\": ["), "objects and arrays are structured");
}

/// The families whose principal property is how they arrange what they hold.
fn structural(tag: Tag) -> Aspect {
    match tag {
        Tag::ButtonGroup
        | Tag::ToggleButtonGroup
        | Tag::FormControl
        | Tag::FormGroup
        | Tag::ListRow
        | Tag::ListItemAction
        | Tag::ListItemSecondaryAction
        | Tag::Table
        | Tag::TableHead
        | Tag::TableBody
        | Tag::TableFooter
        | Tag::TableRow => Aspect::Gap,
        Tag::TabPage
        | Tag::Breadcrumb
        | Tag::Pagination
        | Tag::Stepper
        | Tag::Step
        | Tag::StepContent
        | Tag::NavigationRail
        | Tag::BottomNavigation
        | Tag::AccordionDetails
        | Tag::AccordionActions => Aspect::Gap,
        Tag::Dialog | Tag::DialogContent | Tag::DialogActions | Tag::Menu => Aspect::Gap,
        Tag::DiffViewer => Aspect::Gap,
        Tag::DiffLine => Aspect::Value,
        // Everything else names itself: a caption is what it carries.
        _ => Aspect::Label,
    }
}

/// A part accepted by its parent and then merely appended is the defect this
/// catches: the description says "this is the header" and the surface renders
/// one more anonymous row.
fn every_part_lands_in_the_slot_its_parent_keeps() {
    a_card_header_lands_in_the_cards_header();
    a_disclosure_keeps_its_summary_and_its_details_apart();
    a_dialog_orders_its_title_and_its_actions();
    a_table_puts_its_heading_above_its_body();
    a_row_leads_with_its_mark_and_trails_with_its_controls();
    a_revealed_action_lands_inside_the_menu_it_belongs_to();
    an_adornment_lands_beside_the_value_it_decorates();
    a_trailing_action_is_the_last_thing_in_its_row();
    a_tree_nests_an_item_inside_the_item_that_holds_it();
    a_drawer_panel_covers_the_content_instead_of_joining_it();
    a_tag_input_keeps_retained_tags_before_its_editor();
    a_validation_summary_keeps_actions_below_its_message();
    diff_lines_are_selectable_and_keep_status_beside_content();
}

fn diff_lines_are_selectable_and_keep_status_beside_content() {
    let session = placed(Tag::DiffViewer, &[Tag::DiffLine]);
    let line = session.tagged(Tag::DiffLine).expect("a diff line renders");
    let parts = offspring(&line);
    let status = parts.first().and_then(|part| part.downcast_ref::<gtk::Label>()).expect("status");
    let content = parts.last().and_then(|part| part.downcast_ref::<gtk::Label>()).expect("content");
    assert!(!status.is_selectable());
    assert!(content.is_selectable(), "diff text cannot be selected and copied");
    assert!(content.has_css_class("monospace"));
}

fn a_validation_summary_keeps_actions_below_its_message() {
    let session = placed(Tag::ValidationSummary, &[Tag::Button]);
    let summary = session.tagged(Tag::ValidationSummary).expect("a validation summary renders");
    let body = offspring(&summary)
        .into_iter()
        .find(|part| part.has_css_class("hl-validation-body"))
        .expect("validation summary message body");
    assert!(
        offspring(&body).last().is_some_and(|part| part.has_css_class("hl-button")),
        "the corrective action is not grouped below the validation message"
    );
}

fn a_tag_input_keeps_retained_tags_before_its_editor() {
    let session = placed(Tag::TagInput, &[Tag::Chip, Tag::ToggleButton]);
    let input = session.tagged(Tag::TagInput).expect("a tag input renders");
    let parts = offspring(&input);
    assert!(parts.first().is_some_and(|part| part.has_css_class("hl-chip")));
    assert!(parts.get(1).is_some_and(|part| part.has_css_class("hl-togglebutton")));
    assert!(parts.last().is_some_and(|part| part.has_css_class("hl-field")));
}

/// One parent, one part, rendered: the shape every slot scenario needs.
fn placed(parent: Tag, parts: &[Tag]) -> Session {
    let mut session = Session::new();
    let host = session.producer.create(parent);
    session.producer.append(NodeId::ROOT, host);
    session.producer.set(host, Prop::Expanded, PropValue::Flag(true));
    for part in parts {
        let child = session.producer.create(*part);
        session.producer.append(host, child);
    }
    session.flush().expect("a part its parent declares must render");
    session
}

fn a_card_header_lands_in_the_cards_header() {
    let session = placed(Tag::Card, &[Tag::CardContent, Tag::CardHeader]);
    let card = session.tagged(Tag::Card).expect("a card renders");
    let header = card
        .downcast_ref::<gtk::Frame>()
        .and_then(gtk::Frame::label_widget)
        .expect("a card keeps a header slot");
    assert!(
        header.has_css_class("hl-cardheader"),
        "the card header was appended as content instead of filling the header slot"
    );
}

fn a_disclosure_keeps_its_summary_and_its_details_apart() {
    let session = placed(Tag::Accordion, &[Tag::AccordionDetails, Tag::AccordionSummary]);
    let accordion = session.tagged(Tag::Accordion).expect("an accordion renders");
    let disclosure = accordion
        .downcast_ref::<gtk::Expander>()
        .expect("an accordion is a disclosure");
    let summary = disclosure.label_widget().expect("a summary slot is kept");
    assert!(
        summary.has_css_class("hl-accordionsummary"),
        "the summary is not the label"
    );
    let details = session.tagged(Tag::AccordionDetails).expect("details render");
    assert!(
        subtree(&disclosure.child().expect("a body is revealed")).contains(&details),
        "the details are not inside the body the disclosure reveals"
    );
}

fn a_dialog_orders_its_title_and_its_actions() {
    let session = placed(Tag::Dialog, &[Tag::DialogActions, Tag::DialogContent, Tag::DialogTitle]);
    let dialog = session.tagged(Tag::Dialog).expect("a dialog renders");
    let parts = offspring(&dialog);
    assert!(
        parts.first().is_some_and(|part| part.has_css_class("hl-dialogtitle")),
        "the title is not at the top of the dialog"
    );
    assert!(
        parts.last().is_some_and(|part| part.has_css_class("hl-dialogactions")),
        "the actions are not at the foot of the dialog"
    );
}

fn a_table_puts_its_heading_above_its_body() {
    let session = placed(Tag::Table, &[Tag::TableBody, Tag::TableHead]);
    let table = session.tagged(Tag::Table).expect("a table renders");
    let sections = offspring(&table);
    assert!(
        sections.first().is_some_and(|part| part.has_css_class("hl-tablehead")),
        "the heading was placed under the body it heads"
    );
}

fn a_row_leads_with_its_mark_and_trails_with_its_controls() {
    let session = placed(
        Tag::ListRow,
        &[Tag::ListItemAction, Tag::ListItemText, Tag::ListItemIcon],
    );
    let row = session.tagged(Tag::ListRow).expect("a row renders");
    let parts = offspring(&row);
    assert!(
        parts.first().is_some_and(|part| part.has_css_class("hl-listitemicon")),
        "the mark does not lead the row"
    );
    assert!(
        parts.last().is_some_and(|part| part.has_css_class("hl-listitemaction")),
        "the controls do not trail the row"
    );
}

fn a_revealed_action_lands_inside_the_menu_it_belongs_to() {
    let session = placed(Tag::SpeedDial, &[Tag::SpeedDialAction]);
    let dial = session.tagged(Tag::SpeedDial).expect("a speed dial renders");
    let popover = dial
        .downcast_ref::<gtk::MenuButton>()
        .and_then(gtk::MenuButton::popover)
        .expect("a speed dial reveals a popover");
    let action = session.tagged(Tag::SpeedDialAction).expect("an action renders");
    assert!(
        subtree(&popover.child().expect("the popover holds a column")).contains(&action),
        "the action was placed beside the dial instead of inside what it reveals"
    );
}

/// The trailing slot of a row is the one part that must come after the ordinary
/// controls, whatever order the producer described the two in.
fn a_trailing_action_is_the_last_thing_in_its_row() {
    let session = placed(
        Tag::ListRow,
        &[Tag::ListItemSecondaryAction, Tag::ListItemAction, Tag::ListItemText],
    );
    let row = session.tagged(Tag::ListRow).expect("a row renders");
    let parts = offspring(&row);
    assert!(
        parts
            .last()
            .is_some_and(|part| part.has_css_class("hl-listitemsecondaryaction")),
        "the trailing action is not at the end of the row"
    );
    assert!(
        parts.first().is_some_and(|part| part.has_css_class("hl-listitemtext")),
        "the text was pushed behind the controls it names"
    );
}

/// A tree is only a tree if depth survives: an item described inside another is
/// disclosed by it and indented under it, at any depth.
fn a_tree_nests_an_item_inside_the_item_that_holds_it() {
    let mut session = Session::new();
    let tree = session.producer.create(Tag::Tree);
    session.producer.append(NodeId::ROOT, tree);
    let branch = session.producer.create(Tag::TreeItem);
    session.producer.append(tree, branch);
    // A collapsed disclosure unparents what it holds, so the level below is
    // only reachable once the level above it is open.
    session.producer.set(branch, Prop::Expanded, PropValue::Flag(true));
    let leaf = session.producer.create(Tag::TreeItem);
    session.producer.append(branch, leaf);
    session.flush().expect("a described hierarchy must render");

    let trunk = session.tagged(Tag::Tree).expect("a tree renders");
    let items: Vec<gtk::Widget> = subtree(&trunk)
        .into_iter()
        .filter(|held| held.has_css_class("hl-treeitem"))
        .collect();
    assert_eq!(items.len(), 2, "the tree lost a level of its hierarchy");
    let body = items[0]
        .clone()
        .downcast::<gtk::Expander>()
        .expect("an item is a disclosure")
        .child()
        .expect("an item with children discloses a body");
    assert!(
        subtree(&body).contains(&items[1]),
        "the nested item sits beside its parent instead of inside it"
    );
    assert!(
        items[1].parent().is_some_and(|level| level.margin_start() > 0),
        "a nested level is not indented, so its depth cannot be read"
    );
}

/// A drawer that merely appends its panel is a second column, not a drawer.
fn a_drawer_panel_covers_the_content_instead_of_joining_it() {
    let session = placed(Tag::Drawer, &[Tag::DrawerPanel, Tag::Text]);
    let drawer = session.tagged(Tag::Drawer).expect("a drawer renders");
    let overlay = drawer
        .downcast_ref::<gtk::Overlay>()
        .expect("a drawer overlays what it covers");
    let content = overlay.child().expect("a drawer keeps a slot for its content");
    let panel = session.tagged(Tag::DrawerPanel).expect("a panel renders");
    assert!(
        !subtree(&content).contains(&panel),
        "the panel was placed in the content it is supposed to cover"
    );
    assert!(
        panel.parent().is_some_and(|held| held.eq(&drawer)),
        "the panel is not an overlay child, so it would never draw above the content"
    );
}

fn an_adornment_lands_beside_the_value_it_decorates() {
    let session = placed(Tag::TextField, &[Tag::InputAdornment]);
    let adornment = session.tagged(Tag::InputAdornment).expect("an adornment renders");
    let line = adornment.parent().expect("an adornment is placed somewhere");
    assert!(
        offspring(&line).iter().any(gtk::prelude::ObjectExt::is::<gtk::Entry>),
        "the adornment does not sit beside the field's own value"
    );
}
