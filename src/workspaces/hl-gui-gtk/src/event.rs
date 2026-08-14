//! Toolkit signals to typed interaction.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use hl_gui::{Event, EventId, Handler, NodeId, PropValue, Trigger};

/// Collects interaction for the producer to drain.
///
/// The renderer holds this so a signal closure can report without owning the
/// producer, which keeps the toolkit callback free of application concerns.
#[derive(Clone, Debug, Default)]
pub struct Reports {
    queue: Rc<RefCell<Vec<Event>>>,
}

impl Reports {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&self, event: Event) {
        self.queue.borrow_mut().push(event);
    }

    /// Takes everything reported since the previous drain.
    #[must_use]
    pub fn drain(&self) -> Vec<Event> {
        std::mem::take(&mut self.queue.borrow_mut())
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.queue.borrow().is_empty()
    }
}

/// Connects a declared handler to the matching toolkit signal.
pub(crate) fn bind(widget: &gtk::Widget, node: NodeId, handler: &Handler, reports: &Reports) {
    match handler.trigger {
        Trigger::Invoke | Trigger::Activate => invoke(widget, node, &handler.id, reports),
        Trigger::Change | Trigger::Submit => change(widget, node, &handler.id, reports),
        Trigger::Toggle => toggle(widget, node, &handler.id, reports),
        Trigger::Select | Trigger::Expand | Trigger::Scroll | Trigger::Close | Trigger::Context => {}
    }
}

fn invoke(widget: &gtk::Widget, node: NodeId, id: &EventId, reports: &Reports) {
    let Some(button) = widget.downcast_ref::<gtk::Button>() else {
        return;
    };
    let reports = reports.clone();
    let id = id.clone();
    button.connect_clicked(move |_| {
        reports.push(Event::Invoke { node, id: id.clone() });
    });
}

fn change(widget: &gtk::Widget, node: NodeId, id: &EventId, reports: &Reports) {
    if let Some(entry) = widget.downcast_ref::<gtk::Entry>() {
        let reports = reports.clone();
        let id = id.clone();
        entry.connect_changed(move |entry| {
            reports.push(Event::Change {
                node,
                id: id.clone(),
                value: PropValue::text(entry.text().as_str()),
            });
        });
        return;
    }
    if let Some(scale) = widget.downcast_ref::<gtk::Scale>() {
        let reports = reports.clone();
        let id = id.clone();
        scale.connect_value_changed(move |scale| {
            reports.push(Event::Change {
                node,
                id: id.clone(),
                value: PropValue::Number(scale.value()),
            });
        });
    }
}

fn toggle(widget: &gtk::Widget, node: NodeId, id: &EventId, reports: &Reports) {
    if let Some(check) = widget.downcast_ref::<gtk::CheckButton>() {
        let reports = reports.clone();
        let id = id.clone();
        check.connect_toggled(move |check| {
            reports.push(Event::Change {
                node,
                id: id.clone(),
                value: PropValue::Flag(check.is_active()),
            });
        });
        return;
    }
    if let Some(switch) = widget.downcast_ref::<gtk::Switch>() {
        let reports = reports.clone();
        let id = id.clone();
        switch.connect_state_set(move |_, state| {
            reports.push(Event::Change {
                node,
                id: id.clone(),
                value: PropValue::Flag(state),
            });
            gtk::glib::Propagation::Proceed
        });
    }
}
