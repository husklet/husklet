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
use hl_gui::{Choice, Event, EventId, NodeId, Prop, PropValue, Tag, Tree, Trigger};
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
        let frame = self.producer.frame();
        self.tree
            .apply(&frame, &mut self.canvas)
            .expect("a bound component renders");
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
        Event::Invoke { id, .. } | Event::Change { id, .. } => id.as_str() == "reported",
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
        let mut session = Session::new();
        let widget = session.bound(tag, *trigger);
        worked(&widget);
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
fn worked(widget: &gtk::Widget) {
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
    valued(widget);
}

/// The components whose interaction is a value changing.
fn valued(widget: &gtk::Widget) {
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
