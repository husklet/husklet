//! Every interaction a component declares reaches the producer.
//!
//! The component library declares, beside each component, which interactions
//! that component can report. A handler bound to an interaction the adapter
//! never connected is the defect these scenarios catch: the producer waits for
//! an event that cannot arrive, and nothing anywhere says so.
//!
//! The component is poked the way a person would work it — a button is clicked,
//! a field is typed into, a disclosure is opened — and the report is read back
//! out of the surface.
//!
//! They need a display connection; when none is available they report that
//! rather than passing silently.

use gtk::prelude::*;
use hl_gui::{Cell, Choice, Event, EventId, NodeId, Prop, PropValue, Renderer, Row, RowWindow, Tag, Tree, Trigger};
use hl_gui_gtk::Surface;

/// One producer, one tree, one rendered surface.
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

    /// One component, bound to one of its declared triggers.
    fn bound(&mut self, tag: Tag, trigger: Trigger) -> gtk::Widget {
        let node = self.producer.create(tag);
        self.producer.append(NodeId::ROOT, node);
        self.producer.on(node, trigger, EventId::new("reported"));
        // A drop-down cannot be moved off an option it does not have, so a
        // component offering choices is given some before it is worked.
        if tag.accepts(Prop::Choices) {
            let offered = vec![Choice::new("all", "All"), Choice::new("running", "Running")];
            self.producer.set(node, Prop::Choices, PropValue::Choices(offered));
        }
        if tag.accepts(Prop::Source) {
            self.producer
                .set(node, Prop::Source, PropValue::Source(hl_gui::SourceId::new(1)));
        }
        let frame = self.producer.frame();
        self.tree
            .apply(&frame, &mut self.canvas)
            .expect("a bound component renders");
        if tag.accepts(Prop::Source) {
            self.canvas
                .resize(hl_gui::SourceId::new(1), hl_gui::Version::new(1), 1)
                .expect("a selectable source has one row");
            let view = self
                .widgets()
                .into_iter()
                .find_map(|widget| widget.downcast::<gtk::ColumnView>().ok())
                .expect("a source component has a column view");
            let rows = view
                .model()
                .and_then(|model| model.downcast::<gtk::MultiSelection>().ok())
                .and_then(|selection| selection.model())
                .and_then(|model| model.downcast::<hl_gui_gtk::Rows>().ok())
                .expect("a source component has windowed rows");
            assert!(rows.item(0).is_some());
            let request = self.canvas.requests(0).pop().expect("realizing the row requests it");
            Renderer::rows(
                &mut self.canvas,
                &RowWindow {
                    source: request.source,
                    version: request.version,
                    request: request.id,
                    range: request.range,
                    rows: vec![Row::new(41, [Cell::text("ready")])],
                },
            )
            .expect("the selectable row arrives");
        }
        let class = format!("hl-{}", tag.as_str().to_ascii_lowercase());
        self.widgets()
            .into_iter()
            .find(|widget| widget.has_css_class(&class))
            .expect("a component is reachable by its own style class")
    }

    fn widgets(&self) -> Vec<gtk::Widget> {
        let mut found = vec![self.canvas.widget().clone().upcast::<gtk::Widget>()];
        let mut index = 0;
        while index < found.len() {
            let mut cursor = found[index].first_child();
            while let Some(child) = cursor {
                cursor = child.next_sibling();
                found.push(child);
            }
            index += 1;
        }
        found
    }

    /// Whether the surface reported anything against the bound identity.
    fn reported(&self) -> bool {
        self.canvas.reports().drain().iter().any(identified)
    }
}

fn identified(event: &Event) -> bool {
    match event {
        Event::Invoke { id, .. }
        | Event::Activate { id, .. }
        | Event::Change { id, .. }
        | Event::Toggle { id, .. }
        | Event::Expand { id, .. }
        | Event::Submit { id, .. }
        | Event::Select { id, .. }
        | Event::Edit { id, .. }
        | Event::Scroll { id, .. }
        | Event::Close { id, .. }
        | Event::Context { id, .. }
        | Event::Key { id, .. }
        | Event::Focus { id, .. }
        | Event::Pointer { id, .. } => id.as_str() == "reported",
        Event::Drag { id, .. } | Event::Drop { id, .. } => id.as_str() == "reported",
        _ => false,
    }
}

#[test]
fn every_declared_trigger_reports_when_the_component_is_worked() {
    if gtk::init().is_err() {
        eprintln!("skipped: no display connection");
        return;
    }
    let silent: Vec<String> = Tag::ALL.iter().flat_map(|tag| unreported(*tag)).collect();
    assert!(
        silent.is_empty(),
        "components declare interactions the adapter never connected:\n{}",
        silent.join("\n")
    );
}

/// Everything one component declares it can report and then does not.
fn unreported(tag: Tag) -> Vec<String> {
    let mut silent = Vec::new();
    for trigger in tag.triggers() {
        // Virtual editable cells exist only after a real rooted viewport asks
        // for and receives a row window. The Storybook GTK integration test
        // exercises that complete lifecycle rather than manufacturing one.
        if *trigger == Trigger::Edit {
            continue;
        }
        let mut session = Session::new();
        let widget = session.bound(tag, *trigger);
        worked(&widget, *trigger);
        if !session.reported() {
            silent.push(format!("  {} declares {trigger:?}", tag.as_str()));
        }
    }
    silent
}

/// Works the component the way a person would.
///
/// Through the toolkit rather than through a described property, because a
/// property is applied by the adapter and an interaction is not: a value the
/// producer sends is not news to it, and only what the widget itself emits
/// proves the connection exists.
fn worked(widget: &gtk::Widget, trigger: Trigger) {
    let widget = editable(widget).unwrap_or_else(|| widget.clone());
    if controlled(&widget, trigger) {
        return;
    }
    if trigger == Trigger::Close {
        widget.emit_by_name::<()>("closed", &[]);
        return;
    }
    if trigger == Trigger::Select {
        if let Some(drop) = widget.downcast_ref::<gtk::DropDown>() {
            drop.set_selected(1);
            return;
        }
        if let Some(view) = widget
            .downcast_ref::<gtk::ScrolledWindow>()
            .and_then(gtk::ScrolledWindow::child)
            .and_then(|child| child.downcast::<gtk::ColumnView>().ok())
        {
            if let Some(selection) = view
                .model()
                .and_then(|model| model.downcast::<gtk::MultiSelection>().ok())
            {
                selection.select_item(0, true);
                return;
            }
        }
    }
    if trigger == Trigger::Submit {
        widget.emit_by_name::<()>("activate", &[]);
        return;
    }
    if let Some(button) = widget.downcast_ref::<gtk::Button>() {
        button.emit_by_name::<()>("clicked", &[]);
        return;
    }
    if let Some(toggle) = widget.downcast_ref::<gtk::CheckButton>() {
        toggle.set_active(true);
        return;
    }
    if let Some(switch) = widget.downcast_ref::<gtk::Switch>() {
        switch.set_active(true);
        return;
    }
    if let Some(expander) = widget.downcast_ref::<gtk::Expander>() {
        expander.set_expanded(true);
        return;
    }
    valued(&widget);
}

fn editable(widget: &gtk::Widget) -> Option<gtk::Widget> {
    let mut pending = vec![widget.clone()];
    while let Some(parent) = pending.pop() {
        let mut child = parent.first_child();
        while let Some(current) = child {
            child = current.next_sibling();
            if current.has_css_class("hl-field") {
                return Some(current);
            }
            pending.push(current);
        }
    }
    None
}

fn controlled(widget: &gtk::Widget, trigger: Trigger) -> bool {
    let controllers = widget.observe_controllers();
    for index in 0..controllers.n_items() {
        let Some(controller) = controllers.item(index) else {
            continue;
        };
        match trigger {
            Trigger::Key if controller.is::<gtk::EventControllerKey>() => {
                controller.emit_by_name::<bool>(
                    "key-pressed",
                    &[&gtk::gdk::Key::a, &38_u32, &gtk::gdk::ModifierType::CONTROL_MASK],
                );
                return true;
            }
            Trigger::Focus if controller.is::<gtk::EventControllerFocus>() => {
                controller.emit_by_name::<()>("enter", &[]);
                return true;
            }
            Trigger::Scroll if controller.is::<gtk::EventControllerScroll>() => {
                controller.emit_by_name::<bool>("scroll", &[&1.0_f64, &2.0_f64]);
                return true;
            }
            Trigger::Pointer if controller.is::<gtk::EventControllerMotion>() => {
                controller.emit_by_name::<()>("motion", &[&2.0_f64, &3.0_f64]);
                return true;
            }
            Trigger::Context if controller.is::<gtk::GestureClick>() => {
                let gesture = controller.downcast_ref::<gtk::GestureClick>().expect("checked");
                if gesture.button() == 3 {
                    controller.emit_by_name::<()>("pressed", &[&1_i32, &2.0_f64, &3.0_f64]);
                    return true;
                }
            }
            Trigger::Drag if controller.is::<gtk::DragSource>() => {
                let provider =
                    controller.emit_by_name::<Option<gtk::gdk::ContentProvider>>("prepare", &[&2.0_f64, &3.0_f64]);
                assert!(provider.is_some(), "a drag publishes a bounded node marker");
                return true;
            }
            Trigger::Drop if controller.is::<gtk::DropTarget>() => {
                let marker = "husklet-node:1".to_value();
                let boxed = gtk::glib::BoxedValue(marker);
                let accepted = controller.emit_by_name::<bool>("drop", &[&boxed, &2.0_f64, &3.0_f64]);
                assert!(accepted, "an internal bounded node marker is accepted");
                return true;
            }
            _ => {}
        }
    }
    false
}

/// The components whose interaction is a value changing.
fn valued(widget: &gtk::Widget) {
    if let Some(view) = widget
        .downcast_ref::<gtk::ScrolledWindow>()
        .and_then(gtk::ScrolledWindow::child)
        .and_then(|child| child.downcast::<gtk::TextView>().ok())
    {
        view.buffer().set_text("typed");
        return;
    }
    if let Some(drop) = widget.downcast_ref::<gtk::DropDown>() {
        drop.set_selected(1);
        return;
    }
    if let Some(spin) = widget.downcast_ref::<gtk::SpinButton>() {
        spin.set_value(7.0);
        return;
    }
    if let Some(scale) = widget.downcast_ref::<gtk::Scale>() {
        scale.set_value(3.0);
        return;
    }
    if let Some(editable) = widget.dynamic_cast_ref::<gtk::Editable>() {
        editable.set_text("typed");
    }
}
