//! Toolkit signals to typed interaction.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use gtk::prelude::*;
use hl_gui::{Event, EventId, Handler, NodeId, PointerPhase, PropValue, Trigger};

const KEY_LIMIT: usize = 64;
const SELECTION_LIMIT: u32 = 4096;
/// One toolkit turn cannot enqueue interaction without bound. Pointer motion
/// and scrolling are replaceable observations; actions remain FIFO.
const REPORT_LIMIT: usize = 1024;

/// Collects interaction for the producer to drain.
///
/// The renderer holds this so a signal closure can report without owning the
/// producer, which keeps the toolkit callback free of application concerns.
#[derive(Clone, Debug, Default)]
pub struct Reports {
    queue: Rc<RefCell<VecDeque<Event>>>,
}

impl Reports {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&self, event: Event) {
        let mut queue = self.queue.borrow_mut();
        if replaceable(&event) {
            if let Some(index) = queue.iter().rposition(|held| same_observation(held, &event)) {
                queue[index] = event;
                return;
            }
        }
        if queue.len() == REPORT_LIMIT {
            if let Some(index) = queue.iter().position(replaceable) {
                queue.remove(index);
            } else {
                queue.pop_front();
            }
        }
        queue.push_back(event);
    }

    /// Drops interaction whose producer authority was withdrawn before the
    /// host had a chance to drain it.
    pub(crate) fn withdraw(&self, node: NodeId, trigger: Option<Trigger>) {
        self.queue.borrow_mut().retain(|event| {
            event_authority(event)
                .is_none_or(|(held, kind)| held != node || trigger.is_some_and(|wanted| wanted != kind))
        });
    }

    /// Takes everything reported since the previous drain.
    #[must_use]
    pub fn drain(&self) -> Vec<Event> {
        std::mem::take(&mut *self.queue.borrow_mut()).into_iter().collect()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.queue.borrow().is_empty()
    }
}

fn event_authority(event: &Event) -> Option<(NodeId, Trigger)> {
    Some(match event {
        Event::Invoke { node, .. } => (*node, Trigger::Invoke),
        Event::Activate { node, .. } => (*node, Trigger::Activate),
        Event::Change { node, .. } => (*node, Trigger::Change),
        Event::Toggle { node, .. } => (*node, Trigger::Toggle),
        Event::Expand { node, .. } => (*node, Trigger::Expand),
        Event::Submit { node, .. } => (*node, Trigger::Submit),
        Event::Select { node, .. } => (*node, Trigger::Select),
        Event::Scroll { node, .. } => (*node, Trigger::Scroll),
        Event::Close { node, .. } => (*node, Trigger::Close),
        Event::Context { node, .. } => (*node, Trigger::Context),
        Event::Key { node, .. } => (*node, Trigger::Key),
        Event::Focus { node, .. } => (*node, Trigger::Focus),
        Event::Pointer { node, .. } => (*node, Trigger::Pointer),
        Event::Drag { node, .. } => (*node, Trigger::Drag),
        Event::Drop { node, .. } => (*node, Trigger::Drop),
        _ => return None,
    })
}

fn replaceable(event: &Event) -> bool {
    matches!(
        event,
        Event::Scroll { .. }
            | Event::Pointer {
                phase: PointerPhase::Motion,
                ..
            }
    )
}

fn same_observation(left: &Event, right: &Event) -> bool {
    match (left, right) {
        (Event::Scroll { node: a, id: ai, .. }, Event::Scroll { node: b, id: bi, .. }) => a == b && ai == bi,
        (
            Event::Pointer {
                node: a,
                id: ai,
                phase: PointerPhase::Motion,
                ..
            },
            Event::Pointer {
                node: b,
                id: bi,
                phase: PointerPhase::Motion,
                ..
            },
        ) => a == b && ai == bi,
        _ => false,
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
    let target = crate::component::slot::editable(widget).unwrap_or_else(|| widget.clone());
    match trigger {
        Trigger::Invoke => invoke(widget, node, slot, reports),
        Trigger::Activate => activate(widget, node, slot, reports),
        Trigger::Change => change(&target, node, slot, reports),
        Trigger::Submit => submit(&target, node, slot, reports),
        Trigger::Toggle => toggle(widget, node, slot, reports),
        Trigger::Expand => expand(widget, node, slot, reports),
        Trigger::Select => select(widget, node, slot, reports),
        Trigger::Scroll => scroll(widget, node, slot, reports),
        Trigger::Close => close(widget, node, slot, reports),
        Trigger::Context => context(&target, node, slot, reports),
        Trigger::Key => key(&target, node, slot, reports),
        Trigger::Focus => focus(&target, node, slot, reports),
        Trigger::Pointer => pointer(widget, node, slot, reports),
        Trigger::Drag => drag(widget, node, slot, reports),
        Trigger::Drop => drop_target(widget, node, slot, reports),
    }
}

fn drag(widget: &gtk::Widget, node: NodeId, slot: &Slot, reports: &Reports) {
    let source = gtk::DragSource::new();
    source.set_actions(gtk::gdk::DragAction::MOVE);
    let slot = slot.clone();
    let reports = reports.clone();
    source.connect_prepare(move |_, _, _| {
        let id = slot.id()?;
        reports.push(Event::Drag { node, id });
        let marker = format!("husklet-node:{}", node.raw());
        Some(gtk::gdk::ContentProvider::for_value(&marker.to_value()))
    });
    widget.add_controller(source);
}

fn drop_target(widget: &gtk::Widget, node: NodeId, slot: &Slot, reports: &Reports) {
    let target = gtk::DropTarget::new(String::static_type(), gtk::gdk::DragAction::MOVE);
    let slot = slot.clone();
    let reports = reports.clone();
    target.connect_drop(move |_, value, x, y| {
        if !x.is_finite() || !y.is_finite() {
            return false;
        }
        let Ok(marker) = value.get::<String>() else {
            return false;
        };
        let Some(raw) = marker
            .strip_prefix("husklet-node:")
            .and_then(|raw| raw.parse::<u64>().ok())
        else {
            return false;
        };
        let Some(id) = slot.id() else {
            return false;
        };
        reports.push(Event::Drop {
            node,
            id,
            source: NodeId::new(raw),
            x,
            y,
        });
        true
    });
    widget.add_controller(target);
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

fn activate(widget: &gtk::Widget, node: NodeId, slot: &Slot, reports: &Reports) {
    let Some(button) = widget.downcast_ref::<gtk::Button>() else { return };
    let reports = reports.clone();
    let slot = slot.clone();
    button.connect_clicked(move |_| identified(&reports, &slot, |id| Event::Activate { node, id }));
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
    text_view(widget, node, slot, reports);
    scale(widget, node, slot, reports);
}

fn text_view(widget: &gtk::Widget, node: NodeId, slot: &Slot, reports: &Reports) {
    let Some(view) = crate::component::field::view(widget) else {
        return;
    };
    let buffer = view.buffer();
    let reports = reports.clone();
    let slot = slot.clone();
    buffer.connect_changed(move |buffer| {
        let Some(id) = slot.id() else { return };
        reports.push(Event::Change {
            node,
            id,
            value: PropValue::text(buffer.text(&buffer.start_iter(), &buffer.end_iter(), false)),
        });
    });
}

fn submit(widget: &gtk::Widget, node: NodeId, slot: &Slot, reports: &Reports) {
    let reports = reports.clone();
    let slot = slot.clone();
    if let Some(entry) = widget.downcast_ref::<gtk::Entry>() {
        entry.connect_activate(move |_| identified(&reports, &slot, |id| Event::Submit { node, id }));
    } else if let Some(search) = widget.downcast_ref::<gtk::SearchEntry>() {
        search.connect_activate(move |_| identified(&reports, &slot, |id| Event::Submit { node, id }));
    }
}

fn select(widget: &gtk::Widget, node: NodeId, slot: &Slot, reports: &Reports) {
    if let Some(drop) = widget.downcast_ref::<gtk::DropDown>() {
        let reports = reports.clone();
        let slot = slot.clone();
        drop.connect_selected_notify(move |drop| {
            identified(&reports, &slot, |id| Event::Select {
                node,
                id,
                rows: vec![u64::from(drop.selected())],
                collection: None,
            });
        });
        return;
    }
    let Some(view) = crate::component::table::columns(widget) else {
        return;
    };
    connect_selection(&view, node, slot, reports);
    let reports = reports.clone();
    let slot = slot.clone();
    view.connect_model_notify(move |view| connect_selection(view, node, &slot, &reports));
}

fn connect_selection(view: &gtk::ColumnView, node: NodeId, slot: &Slot, reports: &Reports) {
    let Some(selection) = view
        .model()
        .and_then(|model| model.downcast::<gtk::MultiSelection>().ok())
    else {
        return;
    };
    let reports = reports.clone();
    let slot = slot.clone();
    selection.connect_selection_changed(move |selection, _, _| {
        let bitset = selection.selection();
        let rows: Vec<u64> = gtk::BitsetIter::init_first(&bitset)
            .into_iter()
            .flat_map(|(iter, first)| std::iter::once(first).chain(iter))
            .take(SELECTION_LIMIT as usize)
            .map(u64::from)
            .collect();
        let Some(model) = selection.model().and_then(|model| model.downcast::<crate::Rows>().ok()) else {
            return;
        };
        let Some(collection) = model.selection(&rows) else {
            return;
        };
        identified(&reports, &slot, |id| Event::Select {
            node,
            id,
            rows,
            collection: Some(collection),
        });
    });
}

fn scroll(widget: &gtk::Widget, node: NodeId, slot: &Slot, reports: &Reports) {
    let controller = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
    let reports = reports.clone();
    let slot = slot.clone();
    controller.connect_scroll(move |_, dx, dy| {
        if dx.is_finite() && dy.is_finite() {
            identified(&reports, &slot, |id| Event::Scroll { node, id, dx, dy });
        }
        gtk::glib::Propagation::Proceed
    });
    widget.add_controller(controller);
}

fn close(widget: &gtk::Widget, node: NodeId, slot: &Slot, reports: &Reports) {
    let Some(popover) = widget.downcast_ref::<gtk::Popover>() else {
        return;
    };
    let reports = reports.clone();
    let slot = slot.clone();
    popover.connect_closed(move |_| identified(&reports, &slot, |id| Event::Close { node, id }));
}

fn context(widget: &gtk::Widget, node: NodeId, slot: &Slot, reports: &Reports) {
    let gesture = gtk::GestureClick::new();
    gesture.set_button(3);
    let reports = reports.clone();
    let slot = slot.clone();
    gesture.connect_pressed(move |_, _, x, y| {
        if x.is_finite() && y.is_finite() {
            identified(&reports, &slot, |id| Event::Context { node, id, x, y });
        }
    });
    widget.add_controller(gesture);
}

fn key(widget: &gtk::Widget, node: NodeId, slot: &Slot, reports: &Reports) {
    let controller = gtk::EventControllerKey::new();
    let down_reports = reports.clone();
    let down_slot = slot.clone();
    controller.connect_key_pressed(move |_, key, keycode, modifiers| {
        key_event(&down_reports, &down_slot, node, key, keycode, modifiers, true);
        gtk::glib::Propagation::Proceed
    });
    let up_reports = reports.clone();
    let up_slot = slot.clone();
    controller.connect_key_released(move |_, key, keycode, modifiers| {
        key_event(&up_reports, &up_slot, node, key, keycode, modifiers, false);
    });
    widget.add_controller(controller);
}

fn key_event(
    reports: &Reports,
    slot: &Slot,
    node: NodeId,
    key: gtk::gdk::Key,
    keycode: u32,
    modifiers: gtk::gdk::ModifierType,
    pressed: bool,
) {
    let Some(mut name) = key.name().map(|name| name.to_string()) else {
        return;
    };
    if name.len() > KEY_LIMIT {
        name.truncate(KEY_LIMIT);
    }
    identified(reports, slot, |id| Event::Key {
        node,
        id,
        key: name,
        keycode,
        modifiers: modifiers.bits(),
        pressed,
    });
}

fn focus(widget: &gtk::Widget, node: NodeId, slot: &Slot, reports: &Reports) {
    let controller = gtk::EventControllerFocus::new();
    let entered = reports.clone();
    let entered_slot = slot.clone();
    controller.connect_enter(move |_| {
        identified(&entered, &entered_slot, |id| Event::Focus {
            node,
            id,
            focused: true,
        })
    });
    let left = reports.clone();
    let left_slot = slot.clone();
    controller.connect_leave(move |_| {
        identified(&left, &left_slot, |id| Event::Focus {
            node,
            id,
            focused: false,
        })
    });
    widget.add_controller(controller);
}

fn pointer(widget: &gtk::Widget, node: NodeId, slot: &Slot, reports: &Reports) {
    let motion = gtk::EventControllerMotion::new();
    let entered = reports.clone();
    let entered_slot = slot.clone();
    motion.connect_enter(move |controller, x, y| {
        pointer_event(
            &entered,
            &entered_slot,
            node,
            PointerPhase::Enter,
            Some((x, y)),
            0,
            controller.current_event_state(),
        )
    });
    let moved = reports.clone();
    let moved_slot = slot.clone();
    motion.connect_motion(move |controller, x, y| {
        pointer_event(
            &moved,
            &moved_slot,
            node,
            PointerPhase::Motion,
            Some((x, y)),
            0,
            controller.current_event_state(),
        )
    });
    let left = reports.clone();
    let left_slot = slot.clone();
    motion.connect_leave(move |controller| {
        pointer_event(
            &left,
            &left_slot,
            node,
            PointerPhase::Leave,
            None,
            0,
            controller.current_event_state(),
        )
    });
    widget.add_controller(motion);
    let click = gtk::GestureClick::new();
    click.set_button(0);
    let pressed = reports.clone();
    let pressed_slot = slot.clone();
    click.connect_pressed(move |gesture, _, x, y| {
        pointer_event(
            &pressed,
            &pressed_slot,
            node,
            PointerPhase::Press,
            Some((x, y)),
            gesture.current_button(),
            gesture.current_event_state(),
        )
    });
    let released = reports.clone();
    let released_slot = slot.clone();
    click.connect_released(move |gesture, _, x, y| {
        pointer_event(
            &released,
            &released_slot,
            node,
            PointerPhase::Release,
            Some((x, y)),
            gesture.current_button(),
            gesture.current_event_state(),
        )
    });
    widget.add_controller(click);
}

fn pointer_event(
    reports: &Reports,
    slot: &Slot,
    node: NodeId,
    phase: PointerPhase,
    position: Option<(f64, f64)>,
    button: u32,
    modifiers: gtk::gdk::ModifierType,
) {
    if position.is_some_and(|(x, y)| !x.is_finite() || !y.is_finite()) {
        return;
    }
    let (x, y) = position.map_or((None, None), |(x, y)| (Some(x), Some(y)));
    identified(reports, slot, |id| Event::Pointer {
        node,
        id,
        phase,
        x,
        y,
        button,
        modifiers: modifiers.bits(),
    });
}

fn identified(reports: &Reports, slot: &Slot, event: impl FnOnce(EventId) -> Event) {
    if let Some(id) = slot.id() {
        reports.push(event(id));
    }
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
        reports.push(Event::Expand {
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
        reports.push(Event::Toggle {
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
        reports.push(Event::Toggle {
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
            reports.push(Event::Toggle {
                node,
                id,
                value: PropValue::Flag(state),
            });
        }
        gtk::glib::Propagation::Proceed
    });
}
