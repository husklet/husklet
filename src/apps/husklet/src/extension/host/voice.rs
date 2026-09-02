//! The host's half of the conversation: what it says, and what it says it on.
//!
//! Native callbacks must not wait for an extension socket. They enqueue into a
//! bounded mailbox here; the conversation thread drains it through its sole
//! framed writer.
//!
//! The protocol models no interaction message of its own yet, so what a person
//! did is encoded here: a row request as itself, because that is the shape both
//! ends already agree on, and everything else in an envelope naming it.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use hl_extension::{Frame, Kind};

use super::EVENTS;

/// Tells the extension what a person did.
///
/// A row request travels as itself, because that is the shape both ends of the
/// protocol already agree on; every other interaction travels in an envelope
/// naming it, since the protocol models no interaction message of its own yet.
pub(super) fn speak(voice: &Voice, event: &hl_gui::Event) {
    let Some(payload) = carriage(event, None) else {
        return;
    };
    voice.say(&Frame::new(EVENTS, Kind::Event, payload));
}

/// Tells the extension what happened and which owned surface produced it.
pub(crate) fn speak_at(voice: &Voice, event: &hl_extension::SurfaceEvent) {
    let Some(payload) = carriage(&event.event, Some(&event.slot)) else {
        return;
    };
    voice.say(&Frame::new(EVENTS, Kind::Event, payload));
}

/// Tells an extension which of its manifest-declared pane views was selected.
pub(super) fn speak_provider(voice: &Voice, selection: &hl_extension::PaneSelection) {
    let Ok(payload) = serde_json::to_vec(selection) else {
        return;
    };
    voice.say(&Frame::new(EVENTS, Kind::Event, payload));
}

/// Encodes one interaction. `None` when it cannot be encoded, which is not
/// worth ending a conversation over.
fn carriage(event: &hl_gui::Event, slot: Option<&str>) -> Option<Vec<u8>> {
    if matches!(
        event,
        hl_gui::Event::Invoke { .. }
            | hl_gui::Event::Submit { .. }
            | hl_gui::Event::Change { .. }
            | hl_gui::Event::Select { .. }
            | hl_gui::Event::Focus { .. }
    ) {
        return hl_extension::codec::interaction(event, slot);
    }
    let mut value = match event {
        hl_gui::Event::Rows(request) => serde_json::to_value(request).ok()?,
        hl_gui::Event::Invoke { .. }
        | hl_gui::Event::Submit { .. }
        | hl_gui::Event::Change { .. }
        | hl_gui::Event::Select { .. }
        | hl_gui::Event::Focus { .. } => unreachable!("shared interaction encoder handled this event"),
        hl_gui::Event::Scroll { node, id, dx, dy } => {
            details("scroll", *node, id, serde_json::json!({ "dx": dx, "dy": dy }))
        }
        hl_gui::Event::Close { node, id } => envelope("close", *node, id),
        hl_gui::Event::Context { node, id, x, y } => {
            details("context", *node, id, serde_json::json!({ "x": x, "y": y }))
        }
        hl_gui::Event::Key {
            node,
            id,
            key,
            keycode,
            modifiers,
            pressed,
        } => details(
            "key",
            *node,
            id,
            serde_json::json!({ "key": key, "keycode": keycode, "modifiers": modifiers, "pressed": pressed }),
        ),
        hl_gui::Event::Pointer {
            node,
            id,
            phase,
            x,
            y,
            button,
            modifiers,
        } => details(
            "pointer",
            *node,
            id,
            serde_json::json!({
                "phase": format!("{phase:?}").to_ascii_lowercase(), "x": x, "y": y,
                "button": button, "modifiers": modifiers,
            }),
        ),
        _ => return None,
    };
    if let (Some(slot), Some(object)) = (slot, value.as_object_mut()) {
        object.insert("slot".into(), serde_json::Value::String(slot.to_owned()));
    }
    serde_json::to_vec(&value)
        .ok()
        .filter(|payload| payload.len() <= Frame::PAYLOAD_LIMIT)
}

/// The shape every interaction is sent in.
fn envelope(interaction: &str, node: hl_gui::NodeId, id: &hl_gui::EventId) -> serde_json::Value {
    let mut trigger = interaction.to_owned();
    if let Some(initial) = trigger.get_mut(0..1) {
        initial.make_ascii_uppercase();
    }
    serde_json::json!({ "interaction": interaction, "trigger": trigger, "node": node, "id": id })
}

fn details(
    interaction: &str,
    node: hl_gui::NodeId,
    id: &hl_gui::EventId,
    details: serde_json::Value,
) -> serde_json::Value {
    let mut carried = envelope(interaction, node, id);
    if let (Some(carried), Some(details)) = (carried.as_object_mut(), details.as_object()) {
        carried.extend(details.clone());
    }
    carried
}


#[derive(Clone, Default)]
pub(crate) struct Voice {
    held: Arc<Mutex<Pending>>,
    contended: Arc<AtomicU64>,
}

#[derive(Default)]
struct Pending {
    active: bool,
    frames: VecDeque<Frame>,
    dropped: u64,
}

impl Voice {
    const LIMIT: usize = 256;

    /// Starts accepting events for a connected conversation.
    pub(crate) fn hold(&self) {
        let mut pending = self.pending();
        pending.active = true;
        pending.frames.clear();
        pending.dropped = 0;
        self.contended.store(0, Ordering::Relaxed);
    }

    /// Gives it up when the conversation ends.
    pub(super) fn release(&self) {
        let mut pending = self.pending();
        pending.active = false;
        pending.frames.clear();
    }

    /// Queues without ever waiting for the socket or the conversation thread.
    pub(super) fn say(&self, frame: &Frame) {
        let Ok(mut pending) = self.held.try_lock() else {
            self.contended.fetch_add(1, Ordering::Relaxed);
            return;
        };
        if !pending.active {
            return;
        }
        if pending.frames.len() == Self::LIMIT {
            pending.frames.pop_front();
            pending.dropped = pending.dropped.saturating_add(1);
        }
        pending.frames.push_back(frame.clone());
    }

    pub(crate) fn drain(&self) -> Vec<Frame> {
        let mut pending = self.pending();
        let dropped = pending
            .dropped
            .saturating_add(self.contended.swap(0, Ordering::Relaxed));
        pending.dropped = 0;
        let mut frames: Vec<_> = pending.frames.drain(..).collect();
        if dropped > 0 {
            if let Some(first) = frames.first_mut() {
                if let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&first.payload) {
                    if let Some(object) = value.as_object_mut() {
                        object.insert("dropped".into(), dropped.into());
                        if let Ok(payload) = serde_json::to_vec(&value) {
                            first.payload = payload;
                        }
                    }
                }
            }
        }
        frames
    }

    fn pending(&self) -> std::sync::MutexGuard<'_, Pending> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::{carriage, Frame, Kind, Voice};

    #[test]
    fn ui_event_flood_is_bounded_without_writing_a_socket() {
        let voice = Voice::default();
        voice.hold();
        for index in 0..Voice::LIMIT + 7 {
            voice.say(&Frame::new(super::EVENTS, Kind::Event, serde_json::to_vec(&serde_json::json!({
                "interaction": "pointer", "id": "motion", "node": 1, "x": index,
            })).unwrap()));
        }
        let drained = voice.drain();
        assert_eq!(drained.len(), Voice::LIMIT);
        let first: serde_json::Value = serde_json::from_slice(&drained[0].payload).unwrap();
        assert_eq!(first["dropped"], 7);
    }

    #[test]
    fn a_contended_callback_drops_instead_of_waiting() {
        let voice = Voice::default();
        voice.hold();
        let held = voice.held.lock().unwrap();
        voice.say(&Frame::new(
            super::EVENTS,
            Kind::Event,
            br#"{"interaction":"focus","id":"editor","node":1}"#.to_vec(),
        ));
        drop(held);
        voice.say(&Frame::new(
            super::EVENTS,
            Kind::Event,
            br#"{"interaction":"focus","id":"editor","node":1}"#.to_vec(),
        ));

        let drained = voice.drain();
        assert_eq!(drained.len(), 1);
        let event: serde_json::Value = serde_json::from_slice(&drained[0].payload).unwrap();
        assert_eq!(event["dropped"], 1);
    }

    #[test]
    fn an_addressed_interaction_carries_its_surface_slot() {
        let payload = carriage(
            &hl_gui::Event::Invoke {
                node: hl_gui::NodeId::new(7),
                id: hl_gui::EventId::new("save"),
            },
            Some("surface-2"),
        )
        .expect("encoded");
        let value: serde_json::Value = serde_json::from_slice(&payload).expect("json");
        assert_eq!(value["slot"], "surface-2");
        assert_eq!(value["node"], 7);
        assert_eq!(value["id"], "save");
    }

    #[test]
    fn a_legacy_interaction_remains_unaddressed() {
        let payload = carriage(
            &hl_gui::Event::Invoke {
                node: hl_gui::NodeId::ROOT,
                id: hl_gui::EventId::new("save"),
            },
            None,
        )
        .expect("encoded");
        let value: serde_json::Value = serde_json::from_slice(&payload).expect("json");
        assert!(value.get("slot").is_none());
    }

    #[test]
    fn keyboard_focus_and_pointer_keep_bounded_slot_and_node_identity() {
        let events = [
            hl_gui::Event::Key {
                node: hl_gui::NodeId::new(4),
                id: hl_gui::EventId::new("editor-key"),
                key: "Enter".into(),
                keycode: 36,
                modifiers: 1,
                pressed: true,
            },
            hl_gui::Event::Focus {
                node: hl_gui::NodeId::new(5),
                id: hl_gui::EventId::new("editor-focus"),
                focused: true,
            },
            hl_gui::Event::Pointer {
                node: hl_gui::NodeId::new(6),
                id: hl_gui::EventId::new("editor-pointer"),
                phase: hl_gui::PointerPhase::Press,
                x: Some(12.0),
                y: Some(8.0),
                button: 1,
                modifiers: 0,
            },
        ];
        for (event, interaction, node, id) in [
            (&events[0], "key", 4, "editor-key"),
            (&events[1], "focus", 5, "editor-focus"),
            (&events[2], "pointer", 6, "editor-pointer"),
        ] {
            let payload = carriage(event, Some("pane-stable")).expect("bounded event");
            assert!(payload.len() <= Frame::PAYLOAD_LIMIT);
            let value: serde_json::Value = serde_json::from_slice(&payload).expect("json");
            assert_eq!(value["interaction"], interaction);
            assert_eq!(value["slot"], "pane-stable");
            assert_eq!(value["node"], node);
            assert_eq!(value["id"], id);
        }

        let oversized = hl_gui::Event::Key {
            node: hl_gui::NodeId::ROOT,
            id: hl_gui::EventId::new("oversized"),
            key: "x".repeat(Frame::PAYLOAD_LIMIT + 1),
            keycode: 0,
            modifiers: 0,
            pressed: true,
        };
        assert!(carriage(&oversized, Some("pane-stable")).is_none());
    }
}
