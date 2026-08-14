//! Toolkit signals to typed interaction.

use std::cell::RefCell;
use std::collections::HashMap;
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

/// The identity one connected signal currently reports, or none while the
/// producer has cleared it.
///
/// A signal closure reads this on every emission instead of capturing an
/// identity, which is what lets a rebind stay a single connection.
#[derive(Clone, Debug, Default)]
struct Slot {
    id: Rc<RefCell<Option<EventId>>>,
}

impl Slot {
    fn set(&self, id: EventId) {
        *self.id.borrow_mut() = Some(id);
    }

    fn clear(&self) {
        *self.id.borrow_mut() = None;
    }

    fn id(&self) -> Option<EventId> {
        self.id.borrow().clone()
    }
}

/// The signal connections the adapter has made, keyed by node and trigger.
///
/// GTK keeps every connected closure alive, so connecting again on a rebind
/// would leave the previous identity reporting too. Each pair is therefore
/// connected once and afterwards only its [`Slot`] changes — cheaper than
/// tracking a `SignalHandlerId` per pair and disconnecting, and it needs no
/// widget handle to clear a handler for a node that is already gone.
///
/// Ownership: a closure captures a `Slot` and a [`Reports`], both of which are
/// plain shared data holding no widget, and reads its widget from the signal
/// argument. Nothing on the toolkit side therefore points back at the surface,
/// so the connections cannot form a reference cycle and need no `Weak`.
#[derive(Debug, Default)]
pub(crate) struct Bindings {
    slots: HashMap<(NodeId, Trigger), Slot>,
}

impl Bindings {
    /// Points a trigger at an identity, connecting the signal on first use.
    pub(crate) fn set(&mut self, widget: &gtk::Widget, node: NodeId, handler: &Handler, reports: &Reports) {
        let key = (node, handler.trigger);
        if let Some(slot) = self.slots.get(&key) {
            slot.set(handler.id.clone());
            return;
        }
        let slot = Slot::default();
        slot.set(handler.id.clone());
        connect(widget, node, handler.trigger, &slot, reports);
        self.slots.insert(key, slot);
    }

    /// Silences a trigger. The connection stays, reporting nothing.
    pub(crate) fn clear(&mut self, node: NodeId, trigger: Trigger) {
        if let Some(slot) = self.slots.get(&(node, trigger)) {
            slot.clear();
        }
    }

    /// Forgets a removed node, so a later node reusing its identity connects
    /// against its own widget rather than inheriting a dead slot.
    pub(crate) fn forget(&mut self, node: NodeId) {
        self.slots.retain(|(id, _), _| *id != node);
    }
}

fn connect(widget: &gtk::Widget, node: NodeId, trigger: Trigger, slot: &Slot, reports: &Reports) {
    match trigger {
        Trigger::Invoke | Trigger::Activate => invoke(widget, node, slot, reports),
        Trigger::Change | Trigger::Submit => change(widget, node, slot, reports),
        Trigger::Toggle => toggle(widget, node, slot, reports),
        Trigger::Select | Trigger::Expand | Trigger::Scroll | Trigger::Close | Trigger::Context => {}
    }
}

fn invoke(widget: &gtk::Widget, node: NodeId, slot: &Slot, reports: &Reports) {
    let Some(button) = widget.downcast_ref::<gtk::Button>() else {
        return;
    };
    let reports = reports.clone();
    let slot = slot.clone();
    button.connect_clicked(move |_| {
        if let Some(id) = slot.id() {
            reports.push(Event::Invoke { node, id });
        }
    });
}

fn change(widget: &gtk::Widget, node: NodeId, slot: &Slot, reports: &Reports) {
    entry(widget, node, slot, reports);
    scale(widget, node, slot, reports);
}

fn entry(widget: &gtk::Widget, node: NodeId, slot: &Slot, reports: &Reports) {
    let Some(entry) = widget.downcast_ref::<gtk::Entry>() else {
        return;
    };
    let reports = reports.clone();
    let slot = slot.clone();
    entry.connect_changed(move |entry| {
        let Some(id) = slot.id() else {
            return;
        };
        reports.push(Event::Change {
            node,
            id,
            value: PropValue::text(entry.text().as_str()),
        });
    });
}

fn scale(widget: &gtk::Widget, node: NodeId, slot: &Slot, reports: &Reports) {
    let Some(scale) = widget.downcast_ref::<gtk::Scale>() else {
        return;
    };
    let reports = reports.clone();
    let slot = slot.clone();
    scale.connect_value_changed(move |scale| {
        let Some(id) = slot.id() else {
            return;
        };
        reports.push(Event::Change {
            node,
            id,
            value: PropValue::Number(scale.value()),
        });
    });
}

fn toggle(widget: &gtk::Widget, node: NodeId, slot: &Slot, reports: &Reports) {
    check(widget, node, slot, reports);
    switch(widget, node, slot, reports);
}

fn check(widget: &gtk::Widget, node: NodeId, slot: &Slot, reports: &Reports) {
    let Some(check) = widget.downcast_ref::<gtk::CheckButton>() else {
        return;
    };
    let reports = reports.clone();
    let slot = slot.clone();
    check.connect_toggled(move |check| {
        let Some(id) = slot.id() else {
            return;
        };
        reports.push(Event::Change {
            node,
            id,
            value: PropValue::Flag(check.is_active()),
        });
    });
}

fn switch(widget: &gtk::Widget, node: NodeId, slot: &Slot, reports: &Reports) {
    let Some(switch) = widget.downcast_ref::<gtk::Switch>() else {
        return;
    };
    let reports = reports.clone();
    let slot = slot.clone();
    switch.connect_state_set(move |_, state| {
        if let Some(id) = slot.id() {
            reports.push(Event::Change {
                node,
                id,
                value: PropValue::Flag(state),
            });
        }
        gtk::glib::Propagation::Proceed
    });
}
