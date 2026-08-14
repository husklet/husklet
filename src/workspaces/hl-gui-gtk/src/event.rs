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
        Trigger::Expand => expand(widget, node, slot, reports),
        Trigger::Select | Trigger::Scroll | Trigger::Close | Trigger::Context => {}
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

/// Connects whichever way this widget holds a value.
///
/// The measured ones first, and only one connection is made: a counter is an
/// editable too, and connecting both would report the same keystroke twice —
/// once as the number it now stands at and once as the text showing it.
fn change(widget: &gtk::Widget, node: NodeId, slot: &Slot, reports: &Reports) {
    if counter(widget, node, slot, reports) || chosen(widget, node, slot, reports) {
        return;
    }
    entry(widget, node, slot, reports);
    scale(widget, node, slot, reports);
}

/// Text entry of every shape.
///
/// The editable interface rather than `gtk::Entry`: a search field, a password
/// field and a spin button are all edited the same way and none of them is an
/// entry in GTK4, so asking for the class would report from one field in four.
fn entry(widget: &gtk::Widget, node: NodeId, slot: &Slot, reports: &Reports) {
    let Some(editable) = widget.dynamic_cast_ref::<gtk::Editable>() else {
        return;
    };
    let reports = reports.clone();
    let slot = slot.clone();
    editable.connect_changed(move |editable| {
        let Some(id) = slot.id() else {
            return;
        };
        reports.push(Event::Change {
            node,
            id,
            value: PropValue::text(editable.text().as_str()),
        });
    });
}

/// A counter reports the number it stands at, not the text showing it.
fn counter(widget: &gtk::Widget, node: NodeId, slot: &Slot, reports: &Reports) -> bool {
    let Some(spin) = widget.downcast_ref::<gtk::SpinButton>() else {
        return false;
    };
    let reports = reports.clone();
    let slot = slot.clone();
    spin.connect_value_changed(move |spin| {
        let Some(id) = slot.id() else {
            return;
        };
        reports.push(Event::Change {
            node,
            id,
            value: PropValue::Number(spin.value()),
        });
    });
    true
}

/// A drop-down reports which option was picked, by its position among them.
fn chosen(widget: &gtk::Widget, node: NodeId, slot: &Slot, reports: &Reports) -> bool {
    let Some(drop) = widget.downcast_ref::<gtk::DropDown>() else {
        return false;
    };
    let reports = reports.clone();
    let slot = slot.clone();
    drop.connect_selected_notify(move |drop| {
        let Some(id) = slot.id() else {
            return;
        };
        reports.push(Event::Change {
            node,
            id,
            value: PropValue::Integer(i64::from(drop.selected())),
        });
    });
    true
}

/// A disclosure reports whether it is now open.
fn expand(widget: &gtk::Widget, node: NodeId, slot: &Slot, reports: &Reports) {
    let Some(expander) = widget.downcast_ref::<gtk::Expander>() else {
        return;
    };
    let reports = reports.clone();
    let slot = slot.clone();
    expander.connect_expanded_notify(move |expander| {
        let Some(id) = slot.id() else {
            return;
        };
        reports.push(Event::Change {
            node,
            id,
            value: PropValue::Flag(expander.is_expanded()),
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
    pressed(widget, node, slot, reports);
    switch(widget, node, slot, reports);
}

/// A toggle button is a button that stays down, and GTK4 keeps it in the button
/// hierarchy rather than the check one, so it needs its own connection.
fn pressed(widget: &gtk::Widget, node: NodeId, slot: &Slot, reports: &Reports) {
    let Some(toggle) = widget.downcast_ref::<gtk::ToggleButton>() else {
        return;
    };
    let reports = reports.clone();
    let slot = slot.clone();
    toggle.connect_toggled(move |toggle| {
        let Some(id) = slot.id() else {
            return;
        };
        reports.push(Event::Change {
            node,
            id,
            value: PropValue::Flag(toggle.is_active()),
        });
    });
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
